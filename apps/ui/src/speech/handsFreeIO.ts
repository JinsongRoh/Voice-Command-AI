/**
 * Real speech I/O of hands-free mode (`VoiceIO` in `handsFree.ts`):
 *
 * - `speakAndWait` speaks through the spoken-alert voice (the device voice or
 *   the PC engine) and resolves once playback has ended, so the microphone
 *   does not hear the read-back.
 * - `captureAnswer` records a short answer with the dictation recorder and
 *   transcribes it once. With a streaming recorder (`peek`, the native shell)
 *   a simple energy VAD ends the capture after the answer (trailing silence)
 *   or gives up when nobody speaks; other recorders capture a fixed window.
 *
 * Neither picks an audio device: input and output follow the system route,
 * so a Bluetooth headset works exactly like the built-in microphone/speaker.
 */
import { alertAudioPlaying, stopAlertAudio } from './alertVoice';
import { isShortWav } from './dictation';
import type { Recorder } from './recorder';
import type { Transcript } from './transcribe';

/** Longest answer capture. */
export const ANSWER_MAX_MS = 7000;
/** Nothing above the speech level by then: a timeout (no STT request). */
export const ANSWER_NO_SPEECH_MS = 5000;
/** Silence after speech that ends the answer. */
export const ANSWER_SILENCE_MS = 1000;
/** Capture window of recorders without `peek` (no VAD). */
export const ANSWER_FIXED_MS = 3000;
export const VAD_POLL_MS = 200;
/**
 * RMS (0..1) of a 20 ms frame that counts as speech (about -34 dBFS) before
 * the noise floor is known.
 */
export const SPEECH_RMS = 0.02;
/** Adaptive VAD: speech is this many times the noise floor… */
export const FLOOR_FACTOR = 2.5;
/** …but never below this (a silent room) or above this (a loud one). */
export const MIN_SPEECH_RMS = 0.006;
export const MAX_SPEECH_RMS = 0.05;
/** Frames of the noise floor estimate (the last 5 s) and the least it needs (0.5 s). */
export const FLOOR_FRAMES = 250;
export const FLOOR_MIN_FRAMES = 25;

/**
 * The noise floor: the 20th percentile of the frame levels of the last
 * `FLOOR_FRAMES` frames, or `null` with fewer than `FLOOR_MIN_FRAMES`.
 */
export function noiseFloor(levels: readonly number[]): number | null {
  const recent = levels.slice(-FLOOR_FRAMES);
  if (recent.length < FLOOR_MIN_FRAMES) return null;
  const sorted = [...recent].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length * 0.2)]!;
}

/**
 * The speech level for a noise floor: `FLOOR_FACTOR` times the floor within
 * [`MIN_SPEECH_RMS`, `MAX_SPEECH_RMS`]; `SPEECH_RMS` while the floor is unknown.
 * A soft voice in a quiet room counts; steady noise in a loud one does not.
 */
export function speechThreshold(floor: number | null): number {
  if (floor === null) return SPEECH_RMS;
  return Math.min(MAX_SPEECH_RMS, Math.max(MIN_SPEECH_RMS, floor * FLOOR_FACTOR));
}

/** Numbers for the debug log (three decimals). */
export function level3(x: number | null): number | null {
  return x === null ? null : Math.round(x * 1000) / 1000;
}

const WAV_HEADER_BYTES = 44;
const SAMPLE_RATE = 16_000;
const FRAME_MS = 20;

const defaultSleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** RMS level (0..1) of each 20 ms frame of 16 kHz 16-bit mono PCM WAV bytes. */
export function frameLevels(wav: Uint8Array): number[] {
  const samples = Math.floor(Math.max(0, wav.length - WAV_HEADER_BYTES) / 2);
  const view = new DataView(wav.buffer, wav.byteOffset + WAV_HEADER_BYTES, samples * 2);
  const frame = (SAMPLE_RATE * FRAME_MS) / 1000;
  const levels: number[] = [];
  for (let start = 0; start + frame <= samples; start += frame) {
    let sum = 0;
    for (let i = start; i < start + frame; i += 1) {
      const s = view.getInt16(i * 2, true) / 32768;
      sum += s * s;
    }
    levels.push(Math.sqrt(sum / frame));
  }
  return levels;
}

/** Whether `levels` contain speech, and how long the silence at the end is. */
export function voiceActivity(
  levels: readonly number[],
  threshold = SPEECH_RMS,
): { speech: boolean; trailingSilenceMs: number } {
  let last = -1;
  levels.forEach((level, i) => {
    if (level >= threshold) last = i;
  });
  return {
    speech: last >= 0,
    trailingSilenceMs: (levels.length - 1 - last) * FRAME_MS,
  };
}

async function bytesOf(blob: Blob): Promise<Uint8Array> {
  if (typeof blob.arrayBuffer === 'function') return new Uint8Array(await blob.arrayBuffer());
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(new Uint8Array(reader.result as ArrayBuffer));
    reader.onerror = () => reject(reader.error ?? new Error('read failed'));
    reader.readAsArrayBuffer(blob);
  });
}

export interface CaptureOptions {
  recorder: Recorder;
  /** One transcription of the whole answer. */
  transcribe(audio: Blob): Promise<Transcript>;
  /** A fixed speech level instead of the adaptive one (tests). */
  threshold?: number;
  signal: AbortSignal;
  maxMs?: number;
  noSpeechMs?: number;
  silenceMs?: number;
  fixedMs?: number;
  pollMs?: number;
  /** Injectable for tests. */
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
}

/**
 * Records and transcribes one short answer. `null` on silence, timeout,
 * a microphone or STT failure, or when `signal` aborts (the microphone is
 * released in every case); `''` when there was speech but the recognizer
 * wrote nothing (the caller asks again with the two choices).
 */
export async function captureAnswer(o: CaptureOptions): Promise<string | null> {
  const sleep = o.sleep ?? defaultSleep;
  const now = o.now ?? Date.now;
  const maxMs = o.maxMs ?? ANSWER_MAX_MS;
  const noSpeechMs = o.noSpeechMs ?? ANSWER_NO_SPEECH_MS;
  const silenceMs = o.silenceMs ?? ANSWER_SILENCE_MS;
  const fixedMs = o.fixedMs ?? ANSWER_FIXED_MS;
  const pollMs = o.pollMs ?? VAD_POLL_MS;
  const { recorder, signal } = o;
  if (signal.aborted) return null;
  try {
    await recorder.start();
  } catch {
    return null;
  }
  const startedAt = now();
  let vad = !!recorder.peek;
  let speechSeen = false;
  for (;;) {
    await sleep(pollMs);
    if (signal.aborted) {
      recorder.cancel();
      return null;
    }
    const elapsed = now() - startedAt;
    if (elapsed >= (vad ? maxMs : fixedMs)) break;
    if (!vad || !recorder.peek) continue;
    let levels: number[];
    try {
      levels = frameLevels(await bytesOf(await recorder.peek()));
    } catch {
      // No live audio after all: fall back to the fixed window.
      vad = false;
      continue;
    }
    if (signal.aborted) continue;
    const activity = voiceActivity(levels, o.threshold ?? speechThreshold(noiseFloor(levels)));
    if (activity.speech) speechSeen = true;
    if (speechSeen && activity.trailingSilenceMs >= silenceMs) break;
    if (!speechSeen && elapsed >= noSpeechMs) {
      recorder.cancel();
      return null;
    }
  }
  let audio: Blob;
  try {
    audio = await recorder.stop();
  } catch {
    return null;
  }
  if (signal.aborted || audio.size === 0 || isShortWav(audio)) return null;
  let transcript: Transcript;
  try {
    transcript = await o.transcribe(audio);
  } catch {
    return null;
  }
  if (signal.aborted) return null;
  // The raw recognizer output: filler removal could drop a lone "응" or "네".
  const heard = (transcript.rawText ?? '').trim() || transcript.text.trim();
  return heard || (speechSeen ? '' : null);
}

export interface SpeakWaitDeps {
  /** Hands the text to the voice; resolves once playback started. */
  speak(text: string): Promise<unknown>;
  /** True while the voice is still speaking. */
  busy?(): boolean;
  /** Stops the voice now. */
  stop?(): void;
  /** Time for playback to report itself busy after `speak` resolved. */
  graceMs?: number;
  pollMs?: number;
  /** Upper bound of the wait (a stuck player never blocks the flow). */
  maxMs?: number;
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
}

function deviceSpeaking(): boolean {
  try {
    return typeof window !== 'undefined' && !!window.speechSynthesis?.speaking;
  } catch {
    return false;
  }
}

/** Device voice or PC clip still playing. */
export function voiceBusy(): boolean {
  return deviceSpeaking() || alertAudioPlaying();
}

/** Stops the device voice and the PC clip. */
export function stopVoice(): void {
  try {
    if (typeof window !== 'undefined') window.speechSynthesis?.cancel();
  } catch {
    // Nothing is speaking.
  }
  stopAlertAudio();
}

/** Speaks `text` and resolves when it has finished; `signal` stops it at once. */
export async function speakAndWait(
  text: string,
  signal: AbortSignal,
  deps: SpeakWaitDeps,
): Promise<void> {
  if (signal.aborted) return;
  const busy = deps.busy ?? voiceBusy;
  const stop = deps.stop ?? stopVoice;
  const sleep = deps.sleep ?? defaultSleep;
  const now = deps.now ?? Date.now;
  const pollMs = deps.pollMs ?? 150;
  const maxMs = deps.maxMs ?? 60_000;
  const onAbort = () => stop();
  signal.addEventListener('abort', onAbort, { once: true });
  try {
    try {
      await deps.speak(text);
    } catch {
      // Spoken prompts are best-effort; the screen shows the same state.
    }
    if (signal.aborted) {
      stop();
      return;
    }
    const startedAt = now();
    await sleep(deps.graceMs ?? 150);
    while (!signal.aborted && busy() && now() - startedAt < maxMs) await sleep(pollMs);
  } finally {
    signal.removeEventListener('abort', onAbort);
  }
}
