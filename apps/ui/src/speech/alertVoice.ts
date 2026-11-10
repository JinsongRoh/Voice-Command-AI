/**
 * The app's one speaker: spoken alerts (NTF-04) and every spoken prompt use
 * the source the user picked in Settings, the device voice (Web Speech, the
 * default) or a TTS engine on the PC, whose audio is played with an
 * `HTMLAudioElement`.
 *
 * - One run at a time. A new run, `stop()` or the run's own abort signal
 *   ends the run before it; every step checks the run again after each
 *   await, so audio that arrives for a stopped run is never played.
 * - The two players exclude each other: a PC clip silences the device voice
 *   before it starts, and the device voice pauses the PC clip.
 * - The device voice stands in for the PC only while none of the run's PC
 *   audio has played (old server, engine down, rate limit, refused
 *   playback). Once the PC voice has spoken, a later failure ends the run
 *   instead of switching voices in the middle of a message.
 * - Text longer than one PC request is split into sentences, played in
 *   order with the PC voice.
 * - Fixed prompts can be synthesized ahead (`warm`) and stay cached, so a
 *   short "네" plays at once.
 *
 * The diagnostic log gets the source and the reason only, never the text.
 */
import { diag } from '../diag/log';
import { TTS_MAX_TEXT_CHARS } from '../protocol/types';
import { speakableText } from './radioSpeech';
import { cancelSpeech, fullLangTag, speakUntilEnd, type TtsPrefs } from './tts';

/** Audio returned by the PC (`audio` reply). */
export interface AlertAudio {
  format: string;
  data: Uint8Array;
}

export interface AlertVoicePrefs extends Partial<TtsPrefs> {
  /** Engine name on the PC; `null` uses the device voice. */
  serverEngine: string | null;
  /** Speaker id of the PC engine; `null` or empty: the engine's configured voice. */
  serverVoice?: string | null;
}

/** Sends `synthesize` to the PC; rejects on any error reply. */
export type Synthesize = (
  text: string,
  language: string,
  engine: string | null,
  voice: string | null,
) => Promise<AlertAudio>;

/** Who spoke: the PC voice, the device voice, or nobody (the run was stopped). */
export type SpeakOutcome = 'server' | 'device' | 'stopped';

/** Why the PC voice did not (fully) speak. */
export type FallbackReason =
  'timeout' | 'rate_limited' | 'too_long' | 'play_failed' | 'no_engine' | 'failed';

export interface AlertVoiceDeps {
  /** Plays encoded audio to its end; rejects when it cannot start. Defaults to an `HTMLAudioElement`. */
  playToEnd?(audio: AlertAudio): Promise<void>;
  /** The device voice, resolving at the end of the utterance. Defaults to Web Speech. */
  speakDeviceToEnd?(text: string, lang: string, prefs: Partial<TtsPrefs>): Promise<void>;
  /** Silences both players. */
  stopAll?(): void;
  /** Longest wait for one PC clip before the device voice speaks instead. */
  timeoutMs?: number;
  /** Longest text of one PC request (after the spoken-form rewrite). */
  chunkChars?: number;
  sleep?(ms: number): Promise<void>;
}

export interface WarmOptions {
  /** Pause between two syntheses, so warming never uses up the PC's limit. */
  spacingMs?: number;
  signal?: AbortSignal;
}

export interface WarmResult {
  /** Already cached. */
  cached: number;
  /** Synthesized now. */
  synthesized: number;
  failed: number;
  /** The last failure. */
  reason?: FallbackReason;
}

export interface AlertVoice {
  /**
   * Speaks `text` and resolves when it has been said (or the run was
   * stopped). Ends the run before it. Never rejects.
   */
  speak(
    text: string,
    lang: string,
    prefs: AlertVoicePrefs,
    synthesize: Synthesize,
    signal?: AbortSignal,
  ): Promise<SpeakOutcome>;
  /** The same as `speak` (the voice queue's name for it). */
  play(
    text: string,
    lang: string,
    prefs: AlertVoicePrefs,
    synthesize: Synthesize,
    signal?: AbortSignal,
  ): Promise<SpeakOutcome>;
  /** Stops the current run and silences both players. */
  stop(): void;
  /** Forgets cached audio (another PC was selected). */
  clear(): void;
  /** Synthesizes fixed prompts ahead, one at a time, and keeps them cached. */
  warm(
    texts: readonly string[],
    lang: string,
    prefs: AlertVoicePrefs,
    synthesize: Synthesize,
    options?: WarmOptions,
  ): Promise<WarmResult>;
}

/** Synthesized clips kept in memory, so repeated alerts are not re-synthesized. */
const CACHE_ENTRIES = 48;
/** Warmed prompts, kept apart so alerts never push them out. */
const PINNED_ENTRIES = 64;
const TIMEOUT_MS = 10_000;
/**
 * One PC request at most. Well under the PC's limit: the PC spells numbers
 * and Latin words out in Korean, which makes the text longer.
 */
export const CHUNK_CHARS = 200;
/** Pause between two warming syntheses (at most 15 a minute). */
export const WARM_SPACING_MS = 4_000;
/** Longest PC clip; a player that never reports its end is stopped then. */
const CLIP_MAX_MS = 120_000;

const MIME: Record<string, string> = {
  mp3: 'audio/mpeg',
  opus: 'audio/ogg',
  aac: 'audio/aac',
  flac: 'audio/flac',
  wav: 'audio/wav',
};

let playing: HTMLAudioElement | null = null;

/** True while a PC voice clip is playing (hands-free mode listens only after it). */
export function alertAudioPlaying(): boolean {
  return !!playing && !playing.paused && !playing.ended;
}

/** Stops the PC voice clip that is playing, if any (a tap stops a read-back). */
export function stopAlertAudio(): void {
  playing?.pause();
}

/**
 * Plays `audio` and resolves when it has ended, failed or was stopped
 * (`stopAlertAudio`). Rejects when playback cannot start.
 */
export async function playAudioUntilEnd(audio: AlertAudio): Promise<void> {
  await playAudio(audio);
  const element = playing;
  if (!element || element.ended || element.paused) return;
  await new Promise<void>((resolve) => {
    const timer = setTimeout(() => element.pause(), CLIP_MAX_MS);
    const done = () => {
      clearTimeout(timer);
      resolve();
    };
    element.addEventListener('ended', done, { once: true });
    element.addEventListener('error', done, { once: true });
    element.addEventListener('pause', done, { once: true });
  });
}

/**
 * Plays `audio` with an audio element. The device voice and the previous
 * clip are silenced first: two voices never speak at once.
 */
export async function playAudio(audio: AlertAudio): Promise<void> {
  if (typeof Audio === 'undefined' || typeof URL.createObjectURL !== 'function') {
    throw new Error('audio playback is not available');
  }
  const blob = new Blob([audio.data as BlobPart], { type: MIME[audio.format] ?? 'audio/mpeg' });
  const url = URL.createObjectURL(blob);
  const element = new Audio(url);
  const release = () => URL.revokeObjectURL(url);
  element.addEventListener('ended', release, { once: true });
  element.addEventListener('error', release, { once: true });
  cancelSpeech();
  playing?.pause();
  playing = element;
  try {
    await element.play();
  } catch (err) {
    release();
    throw err;
  }
}

/** The device voice, after silencing the PC clip and any earlier utterance. */
function deviceUntilEnd(text: string, lang: string, prefs: Partial<TtsPrefs>): Promise<void> {
  stopAlertAudio();
  cancelSpeech();
  return speakUntilEnd(text, lang, prefs);
}

function silenceBoth(): void {
  cancelSpeech();
  stopAlertAudio();
}

class TimeoutError extends Error {
  constructor() {
    super('timed out');
    this.name = 'TimeoutError';
  }
}

class StoppedError extends Error {
  constructor() {
    super('stopped');
    this.name = 'StoppedError';
  }
}

/** `promise`, rejected after `ms` or as soon as `signal` aborts. */
function settle<T>(promise: Promise<T>, ms: number, signal?: AbortSignal): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    if (signal?.aborted) {
      reject(new StoppedError());
      return;
    }
    let done = false;
    const finish = (fn: () => void) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      signal?.removeEventListener('abort', onAbort);
      fn();
    };
    const onAbort = () => finish(() => reject(new StoppedError()));
    const timer = setTimeout(() => finish(() => reject(new TimeoutError())), ms);
    signal?.addEventListener('abort', onAbort, { once: true });
    promise.then(
      (v) => finish(() => resolve(v)),
      (e: unknown) => finish(() => reject(e instanceof Error ? e : new Error(String(e)))),
    );
  });
}

/** The reason a PC request failed, from the error the request rejected with. */
export function fallbackReason(err: unknown): FallbackReason {
  if (err instanceof TimeoutError) return 'timeout';
  const e = err as { code?: unknown; message?: unknown } | null;
  const code = typeof e?.code === 'string' ? e.code : '';
  const message = typeof e?.message === 'string' ? e.message : '';
  // The PC's text for a device over its limit (`tts::RATE_LIMITED`).
  if (code === 'unavailable' && /too many spoken alerts/i.test(message)) return 'rate_limited';
  if (code === 'invalid_request' && /limit/i.test(message)) return 'too_long';
  if (code === 'not_found' || code === 'unsupported') return 'no_engine';
  return 'failed';
}

function codePoints(text: string): number {
  return Array.from(text).length;
}

/** Cuts one long sentence at commas, then at spaces, then anywhere. */
function splitLong(sentence: string, max: number): string[] {
  for (const separator of [/(?<=[,，、;；:：])\s*/u, /\s+/u]) {
    const parts = sentence.split(separator).filter(Boolean);
    if (parts.length > 1) return pack(parts, max);
  }
  const chars = Array.from(sentence);
  const out: string[] = [];
  for (let i = 0; i < chars.length; i += max) out.push(chars.slice(i, i + max).join(''));
  return out;
}

/** Joins pieces into chunks of at most `max` code points. */
function pack(pieces: readonly string[], max: number): string[] {
  const out: string[] = [];
  let cur = '';
  for (const piece of pieces.flatMap((p) => (codePoints(p) > max ? splitLong(p, max) : [p]))) {
    const joined = cur ? `${cur} ${piece}` : piece;
    if (codePoints(joined) <= max) {
      cur = joined;
    } else {
      if (cur) out.push(cur);
      cur = piece;
    }
  }
  if (cur) out.push(cur);
  return out;
}

/**
 * `text` in pieces of at most `max` code points, cut between sentences
 * where possible, for PC requests played one after another.
 */
export function splitSpeech(text: string, max: number = CHUNK_CHARS): string[] {
  const limit = Math.max(1, Math.min(max, TTS_MAX_TEXT_CHARS));
  const flat = text.trim();
  if (!flat) return [];
  if (codePoints(flat) <= limit) return [flat];
  const sentences = flat
    .split(/(?<=[.!?。！？…])\s+|\n+/u)
    .map((s) => s.trim())
    .filter(Boolean);
  return pack(sentences, limit);
}

export function createAlertVoice(deps: AlertVoiceDeps = {}): AlertVoice {
  const playToEnd = deps.playToEnd ?? playAudioUntilEnd;
  const deviceToEnd = deps.speakDeviceToEnd ?? deviceUntilEnd;
  const halt = deps.stopAll ?? silenceBoth;
  const timeoutMs = deps.timeoutMs ?? TIMEOUT_MS;
  const chunkChars = deps.chunkChars ?? CHUNK_CHARS;
  const sleep = deps.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const cache = new Map<string, Promise<AlertAudio>>();
  const pinned = new Map<string, Promise<AlertAudio>>();

  const keyOf = (engine: string, voice: string, language: string, text: string) =>
    // The speaker is part of the key: another voice is another clip.
    JSON.stringify([engine, voice, language, text]);

  const audioFor = (
    synthesize: Synthesize,
    text: string,
    language: string,
    engine: string,
    voice: string,
    pin = false,
  ): Promise<AlertAudio> => {
    const key = keyOf(engine, voice, language, text);
    const kept = pinned.get(key);
    if (kept) return kept;
    const hit = cache.get(key);
    if (hit) {
      cache.delete(key);
      // Most recently used last, so the oldest is evicted first.
      if (pin) pinned.set(key, hit);
      else cache.set(key, hit);
      return hit;
    }
    const pending = synthesize(text, language, engine || null, voice || null);
    const target = pin ? pinned : cache;
    target.set(key, pending);
    // Failures are not cached: the next request asks again.
    pending.catch(() => {
      if (target.get(key) === pending) target.delete(key);
    });
    for (const [map, cap] of [
      [cache, CACHE_ENTRIES],
      [pinned, PINNED_ENTRIES],
    ] as const) {
      while (map.size > cap) {
        const oldest = map.keys().next().value;
        if (oldest === undefined) break;
        map.delete(oldest);
      }
    }
    return pending;
  };

  /** The run that may speak; a new run replaces it. */
  let current: AbortController | null = null;
  /** The run whose audio is on now. */
  let speaking: AbortController | null = null;

  const begin = (outer?: AbortSignal): { run: AbortController; end(): void } => {
    // One voice: the run before this one ends now, and its sound with it.
    current?.abort();
    const run = new AbortController();
    current = run;
    run.signal.addEventListener(
      'abort',
      () => {
        if (speaking === run) {
          speaking = null;
          halt();
        }
      },
      { once: true },
    );
    const forward = () => run.abort();
    if (outer?.aborted) run.abort();
    else outer?.addEventListener('abort', forward, { once: true });
    return {
      run,
      end: () => {
        outer?.removeEventListener('abort', forward);
        if (current === run) current = null;
        if (speaking === run) speaking = null;
      },
    };
  };

  /** Puts sound out for `run`; ends at once when the run is stopped. */
  const output = async (run: AbortController, sound: () => Promise<void>): Promise<void> => {
    if (run.signal.aborted) throw new StoppedError();
    speaking = run;
    let onAbort = () => {};
    const stopped = new Promise<void>((resolve) => {
      onAbort = () => resolve();
      run.signal.addEventListener('abort', onAbort, { once: true });
    });
    try {
      await Promise.race([sound(), stopped]);
    } finally {
      run.signal.removeEventListener('abort', onAbort);
      if (speaking === run) speaking = null;
    }
  };

  const report = (
    source: 'server' | 'device',
    reason: FallbackReason | null,
    played: number,
    chunks: number,
  ) => {
    diag('voice.out', { source, reason: reason ?? 'none', played, chunks });
  };

  const run = async (
    text: string,
    lang: string,
    prefs: AlertVoicePrefs,
    synthesize: Synthesize,
    signal?: AbortSignal,
  ): Promise<SpeakOutcome> => {
    const { run: me, end } = begin(signal);
    const stopped = () => me.signal.aborted;
    const devicePrefs: Partial<TtsPrefs> = { voice: prefs.voice, rate: prefs.rate };
    const viaDevice = async (reason: FallbackReason | null): Promise<SpeakOutcome> => {
      await output(me, () => deviceToEnd(text, lang, devicePrefs));
      if (stopped()) return 'stopped';
      report('device', reason, 0, 0);
      return 'device';
    };
    try {
      if (prefs.serverEngine === null) return await viaDevice(null);
      const chunks = splitSpeech(speakableText(text, lang), chunkChars);
      if (chunks.length === 0) return 'device';
      const language = fullLangTag(lang);
      const engine = prefs.serverEngine;
      const voice = prefs.serverVoice ?? '';
      const clip = (i: number) => audioFor(synthesize, chunks[i]!, language, engine, voice);
      let next = clip(0);
      let played = 0;
      for (let i = 0; i < chunks.length; i += 1) {
        let audio: AlertAudio;
        try {
          audio = await settle(next, timeoutMs, me.signal);
        } catch (err) {
          if (stopped()) return 'stopped';
          const reason = fallbackReason(err);
          // The device voice only while nothing was said in the PC voice.
          if (played === 0) return await viaDevice(reason);
          report('server', reason, played, chunks.length);
          return 'server';
        }
        if (stopped()) return 'stopped';
        if (i + 1 < chunks.length) {
          // The next piece is synthesized while this one plays.
          next = clip(i + 1);
          next.catch(() => {});
        }
        try {
          await output(me, () => playToEnd(audio));
        } catch {
          if (stopped()) return 'stopped';
          if (played === 0) return await viaDevice('play_failed');
          report('server', 'play_failed', played, chunks.length);
          return 'server';
        }
        if (stopped()) return 'stopped';
        played += 1;
      }
      report('server', null, played, chunks.length);
      return 'server';
    } catch {
      return stopped() ? 'stopped' : 'device';
    } finally {
      end();
    }
  };

  return {
    speak: run,
    play: run,
    stop() {
      current?.abort();
      halt();
    },
    clear() {
      cache.clear();
      pinned.clear();
    },
    async warm(texts, lang, prefs, synthesize, options = {}) {
      const result: WarmResult = { cached: 0, synthesized: 0, failed: 0 };
      const engine = prefs.serverEngine;
      if (engine === null) return result;
      const { signal } = options;
      const spacing = options.spacingMs ?? WARM_SPACING_MS;
      const language = fullLangTag(lang);
      const voice = prefs.serverVoice ?? '';
      let asked = 0;
      for (const text of texts) {
        if (signal?.aborted) break;
        const chunks = splitSpeech(speakableText(text, lang), chunkChars);
        // Fixed prompts are short; anything longer is not worth keeping.
        if (chunks.length !== 1) continue;
        const key = keyOf(engine, voice, language, chunks[0]!);
        if (pinned.has(key) || cache.has(key)) {
          audioFor(synthesize, chunks[0]!, language, engine, voice, true);
          result.cached += 1;
          continue;
        }
        if (asked > 0) await sleep(spacing);
        if (signal?.aborted) break;
        asked += 1;
        try {
          await settle(
            audioFor(synthesize, chunks[0]!, language, engine, voice, true),
            timeoutMs,
            signal,
          );
          result.synthesized += 1;
        } catch (err) {
          if (signal?.aborted) break;
          result.failed += 1;
          result.reason = fallbackReason(err);
          // No point in asking again now.
          if (result.reason === 'rate_limited' || result.reason === 'no_engine') break;
        }
      }
      diag('voice.warm', {
        cached: result.cached,
        synthesized: result.synthesized,
        failed: result.failed,
        reason: result.reason ?? 'none',
        stopped: !!signal?.aborted,
      });
      return result;
    },
  };
}
