import type { Locale } from '../i18n/i18n';
import type { Client } from '../protocol/transport';
import type { AudioEncoding, ServerMsg } from '../protocol/types';
import { RequestError } from '../state/core';
import type { SttEngine } from '../state/settings';

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
}

export interface TranscribeOptions {
  engine: SttEngine;
  locale: Locale;
  sessionId: string | null;
  /** Preceding text for continuity (sent as `context`; omitted when empty). */
  context?: string | null;
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

/** Maps the server's `transcript` message to the UI type (unknown layers become `base`). */
export function toTranscript(msg: Extract<ServerMsg, { type: 'transcript' }>): Transcript {
  return {
    text: msg.text,
    ...(typeof msg.raw_text === 'string' ? { rawText: msg.raw_text } : {}),
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

async function blobBytes(blob: Blob): Promise<Uint8Array> {
  if (typeof blob.arrayBuffer === 'function') return new Uint8Array(await blob.arrayBuffer());
  return new Uint8Array(await new Response(blob).arrayBuffer());
}

/**
 * Server-side speech recognition (VOC-01): sends `transcribe` through the
 * selected machine's client. Error responses (e.g. speech disabled on the
 * server) reject with a `RequestError` carrying the server's message.
 */
export function createServerTranscriber(getClient: () => Client | null): TranscribeFn {
  return async (audio, options) => {
    const client = getClient();
    if (!client) throw new RequestError('transport', 'no machine is connected');
    const format = audioEncodingOf(audio.type);
    if (!format) {
      throw new RequestError('unsupported', `unsupported audio format: ${audio.type || 'unknown'}`);
    }
    let response: ServerMsg;
    try {
      response = await client.request({
        type: 'transcribe',
        audio: await blobBytes(audio),
        format,
        sample_rate: null,
        language: options.locale,
        session_id: options.sessionId,
        // Only sent when present, so servers without the field are unaffected.
        ...(options.context ? { context: options.context } : {}),
      });
    } catch (err) {
      throw new RequestError('transport', (err as Error).message);
    }
    if (response.type === 'error') throw new RequestError(response.code, response.message);
    if (response.type !== 'transcript') {
      throw new RequestError('internal', `unexpected response: ${response.type}`);
    }
    return toTranscript(response);
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
