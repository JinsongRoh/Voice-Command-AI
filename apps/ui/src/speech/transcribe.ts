import { diag, errorInfo } from '../diag/log';
import type { Locale } from '../i18n/i18n';
import type { Client, Transport } from '../protocol/transport';
import type { AudioEncoding, ServerMsg } from '../protocol/types';
import { RequestError } from '../state/core';
import { dropLoneHallucination, wavLevel } from './hallucinations';
import { collapseRepeats } from './repeats';

/**
 * Where a correction came from: the vocabulary layers of VOC-05 (`base`,
 * `project`, `personal`) or a contextual (LLM-based) correction pass
 * (`context`).
 */
export type CorrectionLayer = 'base' | 'project' | 'personal' | 'context';
export const CORRECTION_LAYERS: readonly CorrectionLayer[] = [
  'base',
  'project',
  'personal',
  'context',
];

/** A term replaced by a correction layer (VOC-05). */
export interface TranscriptCorrection {
  /** What the recognizer heard (appears in the raw text). */
  from: string;
  /** What it was replaced with (appears in `text`). */
  to: string;
  layer: CorrectionLayer;
}

export interface Transcript {
  /** Corrected text. */
  text: string;
  /**
   * Recognizer output before filler removal and corrections (the server's
   * `raw_text`); absent when the engine provides none.
   */
  rawText?: string;
  corrections: TranscriptCorrection[];
  /** Topic of the ongoing dictation detected by the server (absent when unknown). */
  topic?: string;
}

export interface TranscribeOptions {
  /** UI language (on-device engines and the mock); never sent as the speech language. */
  locale: Locale;
  /**
   * The speech language the user picked explicitly; `null`/absent lets the
   * computer's speech settings decide.
   */
  language?: string | null;
  sessionId: string | null;
  /** Preceding text for continuity (sent as `context`; omitted when empty). */
  context?: string | null;
  /**
   * A FINAL chunk (or a whole recording) rather than an interim preview; sent
   * as `final: true` so the server may run its contextual correction.
   */
  final?: boolean;
}

/**
 * Speech-to-text entry point injected into the UI. The real implementation
 * will send the audio to the server's speech pipeline (vca-speech) or run an
 * on-device engine; the mock returns canned transcripts.
 */
export type TranscribeFn = (audio: Blob, options: TranscribeOptions) => Promise<Transcript>;

const CANNED: Transcript[] = [
  {
    text: 'Run pnpm test and fix the useEffect warning in Settings.tsx',
    corrections: [
      { from: 'p n p m', to: 'pnpm', layer: 'base' },
      { from: 'use effect', to: 'useEffect', layer: 'base' },
      { from: 'settings dot tsx', to: 'Settings.tsx', layer: 'project' },
    ],
  },
  {
    text: 'Check the kubectl rollout status for payments-api and summarize it',
    corrections: [
      { from: 'kube control', to: 'kubectl', layer: 'base' },
      { from: 'payments API', to: 'payments-api', layer: 'project' },
    ],
  },
  {
    text: 'Commit the changes and open a PR against main',
    corrections: [{ from: 'P are', to: 'PR', layer: 'personal' }],
  },
];

export function createMockTranscriber(delayMs = 900): TranscribeFn {
  let i = 0;
  return (_audio, _options) =>
    new Promise((resolve) => {
      const result = CANNED[i % CANNED.length]!;
      i += 1;
      setTimeout(() => resolve(result), delayMs);
    });
}

/** Maps a Blob MIME type to the protocol's audio encoding (null when unsupported). */
export function audioEncodingOf(mime: string): AudioEncoding | null {
  const base = mime.split(';')[0]!.trim().toLowerCase();
  switch (base) {
    case 'audio/wav':
    case 'audio/wave':
    case 'audio/x-wav':
    case 'audio/vnd.wave':
      return 'wav';
    case 'audio/webm':
      return 'webm';
    case 'audio/ogg':
      return 'ogg';
    case 'audio/mp4':
    case 'audio/m4a':
    case 'audio/x-m4a':
      return 'm4a';
    default:
      return null;
  }
}

/**
 * Maps the server's `transcript` message to the UI type (unknown layers become
 * `base`). Repetition loops ("O.O.O.O.…") are collapsed here too, so a PC on
 * an older version cannot put one in the composer; a transcript that is one
 * loop becomes empty, like silence.
 */
export function toTranscript(msg: Extract<ServerMsg, { type: 'transcript' }>): Transcript {
  return {
    text: collapseRepeats(msg.text),
    ...(typeof msg.raw_text === 'string' ? { rawText: collapseRepeats(msg.raw_text) } : {}),
    ...(typeof msg.topic === 'string' && msg.topic.trim() ? { topic: msg.topic.trim() } : {}),
    corrections: (msg.corrections ?? []).map((c) => ({
      from: c.from,
      to: c.to,
      layer: CORRECTION_LAYERS.includes(c.layer) ? c.layer : 'base',
    })),
  };
}

/** A correction term located in a text. */
export interface CorrectionSpan {
  start: number;
  end: number;
  correction: TranscriptCorrection;
}

/**
 * Locates correction terms in `text`: the replacements (`to`) in corrected
 * text, or the originally heard words (`from`) in raw text. Overlaps are
 * dropped; matching is literal (case-sensitive).
 */
export function correctionSpans(
  text: string,
  corrections: TranscriptCorrection[],
  field: 'from' | 'to',
): CorrectionSpan[] {
  const spans: CorrectionSpan[] = [];
  for (const c of corrections) {
    const term = c[field];
    if (!term) continue;
    let from = 0;
    for (;;) {
      const idx = text.indexOf(term, from);
      if (idx < 0) break;
      spans.push({ start: idx, end: idx + term.length, correction: c });
      from = idx + term.length;
    }
  }
  spans.sort((a, b) => a.start - b.start);
  const merged: CorrectionSpan[] = [];
  for (const s of spans) {
    const last = merged[merged.length - 1];
    if (last && s.start < last.end) continue;
    merged.push(s);
  }
  return merged;
}

export async function blobBytes(blob: Blob): Promise<Uint8Array> {
  if (typeof blob.arrayBuffer === 'function') return new Uint8Array(await blob.arrayBuffer());
  return new Uint8Array(await new Response(blob).arrayBuffer());
}

/**
 * How long a FINAL chunk waits for the link to come back (a phone that just
 * woke up or changed networks reconnects within seconds) before it is sent,
 * or before its one retry.
 */
export const RECONNECT_WAIT_MS = 20_000;

/**
 * Resolves true once `transport` is connected (at once when it is), false
 * after `ms` or when it is disconnected for good (signed out, removed).
 */
export function waitConnected(transport: Transport, ms: number): Promise<boolean> {
  if (transport.state === 'connected') return Promise.resolve(true);
  if (transport.state === 'disconnected' || ms <= 0) return Promise.resolve(false);
  return new Promise((resolve) => {
    const done = (ok: boolean) => {
      clearTimeout(timer);
      off();
      resolve(ok);
    };
    const timer = setTimeout(() => done(false), ms);
    const off = transport.onStateChange((state) => {
      if (state === 'connected') done(true);
      else if (state === 'disconnected') done(false);
    });
  });
}

export interface ServerTranscriberOptions {
  /** See `RECONNECT_WAIT_MS`. */
  reconnectWaitMs?: number;
}

/**
 * Server-side speech recognition (VOC-01): sends `transcribe` through the
 * selected machine's client. Error responses (e.g. speech disabled on the
 * server) reject with a `RequestError` carrying the server's message.
 *
 * Mobile links drop (sleep, a network change). A FINAL chunk, which the user
 * cannot get back by waiting, rides that out: while the link is reconnecting
 * it waits for it (`RECONNECT_WAIT_MS`), and when the link dropped under the
 * request it is sent once more over the new link (recognition has no side
 * effect beyond the dictation topic, so a repeat is safe). A request that
 * timed out on a link that still looks alive is not repeated: the network is
 * too slow, and a second upload would make it worse. Interim previews and
 * wake checks never wait: a newer one follows anyway. A failure that never
 * reached the server is a `RequestError` with code `transport`
 * (`isNetworkError`).
 */
export function createServerTranscriber(
  getClient: () => Client | null,
  opts: ServerTranscriberOptions = {},
): TranscribeFn {
  const waitMs = opts.reconnectWaitMs ?? RECONNECT_WAIT_MS;
  return async (audio, options) => {
    const client = getClient();
    if (!client) throw new RequestError('transport', 'no machine is connected');
    const format = audioEncodingOf(audio.type);
    const started = Date.now();
    const info = {
      bytes: audio.size,
      // 16 kHz mono 16-bit WAV (the native recorder): 32 bytes per ms.
      audioMs: format === 'wav' ? Math.max(0, Math.round((audio.size - 44) / 32)) : undefined,
      format: format ?? 'unknown',
      final: options.final === true,
      contextChars: options.context?.length ?? 0,
    };
    diag('stt.request', info);
    if (!format) {
      throw new RequestError('unsupported', `unsupported audio format: ${audio.type || 'unknown'}`);
    }
    const bytes = await blobBytes(audio);
    const send = () =>
      client.request({
        type: 'transcribe',
        audio: bytes,
        format,
        sample_rate: null,
        // Only an explicit choice: a forced UI language made Whisper
        // translate (e.g. Korean speech came out in English).
        language: options.language ?? null,
        session_id: options.sessionId,
        // Only sent when present, so servers without the field are unaffected.
        ...(options.context ? { context: options.context } : {}),
        // Only final chunks carry the flag; interim previews omit it.
        ...(options.final ? { final: true } : {}),
      });
    const patient = options.final === true;
    const { transport } = client;
    if (patient && transport.state !== 'connected') {
      const from = transport.state;
      const ok = await waitConnected(transport, waitMs);
      diag('stt.link.wait', { ...info, from, ok, ms: Date.now() - started });
    }
    let response: ServerMsg;
    try {
      response = await send();
    } catch (err) {
      const timedOut = /timed out/i.test((err as Error).message);
      diag('stt.transport.error', {
        ...info,
        ms: Date.now() - started,
        state: transport.state,
        ...errorInfo(err),
      });
      // A request that timed out on a link that still looks alive is not
      // repeated (see above); one the link lost is, once it is back.
      const retry = patient && !(timedOut && transport.state === 'connected');
      if (!retry || !(await waitConnected(transport, waitMs))) {
        throw new RequestError('transport', (err as Error).message);
      }
      diag('stt.retry', { ...info, ms: Date.now() - started });
      try {
        response = await send();
      } catch (again) {
        diag('stt.retry.error', { ...info, ms: Date.now() - started, ...errorInfo(again) });
        throw new RequestError('transport', (again as Error).message);
      }
    }
    if (response.type === 'error') {
      diag('stt.server.error', {
        ...info,
        ms: Date.now() - started,
        code: response.code,
        error: response.message,
      });
      throw new RequestError(response.code, response.message);
    }
    diag('stt.response', {
      ...info,
      ms: Date.now() - started,
      type: response.type,
      textChars: response.type === 'transcript' ? response.text.length : null,
    });
    if (response.type !== 'transcript') {
      throw new RequestError('internal', `unexpected response: ${response.type}`);
    }
    const transcript = toTranscript(response);
    // Plain-text path: a lone stock phrase on short or quiet audio is invented.
    if (format === 'wav' && transcript.text.trim() !== '') {
      const level = wavLevel(bytes);
      const text = dropLoneHallucination(transcript.text, level);
      if (text === '') {
        diag('stt.hallucination.dropped', { ...info, textChars: transcript.text.length });
        return { ...transcript, text: '', ...(transcript.rawText ? { rawText: '' } : {}) };
      }
    }
    return transcript;
  };
}

/**
 * Whether a transcription failure means server-side speech recognition is
 * turned off on that machine (vca-server answers `transcribe` with
 * `unsupported: server-side transcription is off (set [speech] engine in
 * config.toml)`). The audio reached the server, so this is not an error the
 * user caused; the UI shows a neutral notice instead.
 */
export function isSpeechDisabledError(err: unknown): boolean {
  if (!(err instanceof RequestError) || err.code !== 'unsupported') return false;
  return /transcription is off|\[speech\]/i.test(err.message);
}

/**
 * Whether a transcription failed because the speech server could not be
 * reached for a moment (vcad restarting, a network blip) rather than because
 * it rejected the audio. vca-server retries once, then answers `unavailable`
 * with a `speech_unreachable:` message; older servers send the raw
 * `http error: error sending request`. The UI shows a friendly "tap again".
 */
export function isSpeechUnreachableError(err: unknown): boolean {
  if (!(err instanceof RequestError) || err.code !== 'unavailable') return false;
  return /^speech_unreachable\b|^http error: error (sending request|trying to connect)/i.test(
    err.message,
  );
}

/**
 * Whether a transcription failed on the way (no link, the link dropped, no
 * answer in time) rather than being answered by the server. The recording is
 * intact and can be sent again.
 */
export function isNetworkError(err: unknown): boolean {
  return err instanceof RequestError && err.code === 'transport';
}

/** Character ranges of correction terms in `text`, for highlighting. */
export function correctionRanges(
  text: string,
  corrections: TranscriptCorrection[],
): { start: number; end: number }[] {
  const ranges: { start: number; end: number }[] = [];
  for (const c of corrections) {
    if (!c.to) continue;
    let from = 0;
    for (;;) {
      const idx = text.indexOf(c.to, from);
      if (idx < 0) break;
      ranges.push({ start: idx, end: idx + c.to.length });
      from = idx + c.to.length;
    }
  }
  ranges.sort((a, b) => a.start - b.start);
  // Drop overlaps.
  const merged: { start: number; end: number }[] = [];
  for (const r of ranges) {
    const last = merged[merged.length - 1];
    if (last && r.start < last.end) continue;
    merged.push(r);
  }
  return merged;
}
