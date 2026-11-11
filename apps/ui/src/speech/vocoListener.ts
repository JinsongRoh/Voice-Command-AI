/**
 * Continuous listening of the Voco wake flow (hands-free v2): keeps the
 * microphone open and cuts the audio into utterances with a local energy VAD.
 * Nothing leaves the device here: the caller decides what to transcribe
 * (`vocoFlow.ts` only sends a short head of each utterance while it waits for
 * the wake word).
 *
 * It needs a streaming recorder (`peek` + `split`, the native shell): `peek`
 * returns the audio buffered since the last `split` without stopping, and
 * `split` drops the audio before a quiet point. The listener polls `peek`,
 * finds speech followed by `endSilenceMs` of silence, slices that utterance
 * out of the peeked WAV and then `split`s the buffer so it stays short.
 * Audio that was already handled but is still buffered after a split is
 * skipped (`skipFrames`).
 *
 * While another voice is playing (`busy`, a spoken alert) the audio is
 * dropped, so the app never transcribes itself. On iOS playback can freeze the
 * capture: one that stopped growing during the voice is replaced as soon as
 * the voice ends, not `stallMs` later. `pause` releases the
 * microphone (the app speaks, a decision is read out, the composer dictates)
 * and `resume` opens it again.
 */
import { diag, errorInfo } from '../diag/log';
import { frameLevels, level3, noiseFloor, SPEECH_RMS, speechThreshold } from './handsFreeIO';
import type { Recorder } from './recorder';

/** Silence that ends an utterance. */
export const END_SILENCE_MS = 800;
/**
 * …and a short one (a call such as "보코" is about 0.4–0.6 s): its STT
 * request starts sooner (see `VocoListenerOptions.endSilenceFor`).
 */
export const SHORT_END_SILENCE_MS = 450;
export const SHORT_UTTERANCE_MS = 1500;
/**
 * Once a dictation has gone on this long, a longer silence ends the piece:
 * a thinking pause in a long sentence keeps it whole for the recognizer.
 * (The message itself never ends on a pause; see `vocoFlow.ts`.)
 */
export const LONG_UTTERANCE_MS = 5000;
export const LONG_END_SILENCE_MS = 1200;

/**
 * The end silence of the Voco listener: short for a call while waiting for
 * the wake word, the normal one for dictation, longer in a long dictation.
 * The short value never applies once the flow listens to a message.
 */
export function vocoEndSilenceMs(waiting: boolean, speechMs: number): number {
  if (waiting && speechMs < SHORT_UTTERANCE_MS) return SHORT_END_SILENCE_MS;
  if (!waiting && speechMs >= LONG_UTTERANCE_MS) return LONG_END_SILENCE_MS;
  return END_SILENCE_MS;
}
/** A pause at least this long before the last words counts as a pause (send commands). */
export const PAUSE_BEFORE_MS = 600;
/** …when the words after it are at most this long. */
export const LAST_WORDS_MAX_MS = 1200;
/** Less speech than this is a noise, not an utterance. */
export const MIN_SPEECH_MS = 200;
/** An utterance this long is cut even without a pause. */
export const MAX_UTTERANCE_MS = 30_000;
/** Silence buffered this long (no speech) is dropped locally. */
const DROP_SILENCE_MS = 3000;
export const LISTEN_POLL_MS = 200;
/**
 * Audio kept before the first and after the last speech frame. The VAD starts
 * late on a soft first syllable in noise, so the lead-in is the longer one.
 */
export const LEAD_MS = 400;
const TAIL_MS = 200;
/** Gaps shorter than this inside a word run are not pauses. */
const BURST_GAP_MS = 200;

/** A capture whose buffer has not grown this long is dead (restarted). */
export const STALL_MS = 2000;
/** Waits before opening the microphone again after a failure. */
export const RETRY_MS = [1000, 2000, 5000, 10_000] as const;

const FRAME_MS = 20;
const SAMPLE_RATE = 16_000;
const WAV_HEADER_BYTES = 44;
const FRAME_BYTES = (SAMPLE_RATE * 2 * FRAME_MS) / 1000;

/** One utterance cut out of the stream. */
export interface HeardUtterance {
  /** The whole utterance (16 kHz PCM WAV). */
  audio: Blob;
  /** The first `ms` of speech (with its lead-in) for a wake check. */
  head(ms: number): Blob;
  /** From the first to the last speech frame. */
  speechMs: number;
  /** A pause of `PAUSE_BEFORE_MS` before short last words ("…, 전송해"). */
  pausedBeforeLast: boolean;
  /** `now()` when the speech ended. */
  endedAt: number;
}

export interface VocoListenerOptions {
  createRecorder(): Recorder;
  /** Speech started (after a silence). */
  onSpeechStart(): void;
  /** Speech ended: the utterance, or `null` for a noise too short to count. */
  onSpeechEnd(utterance: HeardUtterance | null): void;
  /** The microphone could not be opened or stopped answering (it is retried). */
  onError?(error: unknown): void;
  /** The microphone is open and delivering audio. */
  onListening?(): void;
  /** Another voice is playing: its audio is dropped. */
  busy?(): boolean;
  endSilenceMs?: number;
  /** The end silence for an utterance with `speechMs` of speech so far (overrides `endSilenceMs`). */
  endSilenceFor?(speechMs: number): number;
  pollMs?: number;
  /** See `STALL_MS`; `0` turns the check off. */
  stallMs?: number;
  /** See `RETRY_MS`; an empty list never retries. */
  retryMs?: readonly number[];
  threshold?: number;
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
}

const defaultSleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

async function bytesOf(blob: Blob): Promise<Uint8Array> {
  if (typeof blob.arrayBuffer === 'function') return new Uint8Array(await blob.arrayBuffer());
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(new Uint8Array(reader.result as ArrayBuffer));
    reader.onerror = () => reject(reader.error ?? new Error('read failed'));
    reader.readAsArrayBuffer(blob);
  });
}

/** A 16 kHz, 16-bit mono WAV file holding `pcm`. */
export function wavOf(pcm: Uint8Array): Uint8Array {
  const out = new Uint8Array(WAV_HEADER_BYTES + pcm.length);
  const v = new DataView(out.buffer);
  const ascii = (at: number, s: string) => {
    for (let i = 0; i < s.length; i += 1) out[at + i] = s.charCodeAt(i);
  };
  ascii(0, 'RIFF');
  v.setUint32(4, 36 + pcm.length, true);
  ascii(8, 'WAVE');
  ascii(12, 'fmt ');
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true);
  v.setUint16(22, 1, true);
  v.setUint32(24, SAMPLE_RATE, true);
  v.setUint32(28, SAMPLE_RATE * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  ascii(36, 'data');
  v.setUint32(40, pcm.length, true);
  out.set(pcm, WAV_HEADER_BYTES);
  return out;
}

/** Frames `from`..`to` (exclusive) of a WAV file, as a new WAV blob. */
export function sliceWav(wav: Uint8Array, from: number, to: number): Blob {
  const start = WAV_HEADER_BYTES + Math.max(0, from) * FRAME_BYTES;
  const end = Math.min(wav.length, WAV_HEADER_BYTES + Math.max(from, to) * FRAME_BYTES);
  const pcm = wav.slice(Math.min(start, end), end);
  return new Blob([wavOf(pcm) as BlobPart], { type: 'audio/wav' });
}

function framesOf(bytes: number): number {
  return Math.max(0, Math.floor((bytes - WAV_HEADER_BYTES) / FRAME_BYTES));
}

/** Runs of speech frames, short gaps merged: `[start, end)` frame pairs. */
export function speechBursts(
  levels: readonly number[],
  threshold = SPEECH_RMS,
): [number, number][] {
  const out: [number, number][] = [];
  const gap = BURST_GAP_MS / FRAME_MS;
  levels.forEach((level, i) => {
    if (level < threshold) return;
    const last = out[out.length - 1];
    if (last && i - last[1] <= gap) last[1] = i + 1;
    else out.push([i, i + 1]);
  });
  return out;
}

/** Whether the last words came after a pause (and are short, like "전송해"). */
export function pausedBeforeLast(bursts: readonly [number, number][]): boolean {
  if (bursts.length < 2) return false;
  const [s, e] = bursts[bursts.length - 1]!;
  const prevEnd = bursts[bursts.length - 2]![1];
  return (s - prevEnd) * FRAME_MS >= PAUSE_BEFORE_MS && (e - s) * FRAME_MS <= LAST_WORDS_MAX_MS;
}

/** See the module comment. */
export class VocoListener {
  private generation = 0;
  private recorder: Recorder | null = null;
  private loop: Promise<void> = Promise.resolve();
  private running = false;
  private failures = 0;
  /** Noise floor of the room (kept across captures); `null` until measured. */
  private floor: number | null = null;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  /** Ends the poll wait of the running capture. */
  private wakePoll: () => void = () => {};

  constructor(private readonly o: VocoListenerOptions) {}

  /** Whether the microphone is open (not paused or stopped). */
  get listening(): boolean {
    return this.running;
  }

  /** Opens the microphone and starts listening (no-op when already listening). */
  start(): void {
    if (this.running) return;
    this.clearRetry();
    this.running = true;
    const gen = ++this.generation;
    this.loop = this.run(gen).catch((err: unknown) => {
      diag('voco.listen.error', errorInfo(err));
      if (this.generation !== gen) return;
      this.running = false;
      const recorder = this.recorder;
      this.recorder = null;
      if (recorder) void this.close(recorder);
      this.o.onError?.(err);
      this.scheduleRetry(gen);
    });
  }

  /** Opens the microphone again after a failure, with a growing delay. */
  private scheduleRetry(gen: number) {
    const delays = this.o.retryMs ?? RETRY_MS;
    if (delays.length === 0) return;
    const ms = delays[Math.min(this.failures, delays.length - 1)]!;
    this.failures += 1;
    diag('voco.blocked', { reason: 'listen_failed', retryMs: ms, failures: this.failures });
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      // Paused or stopped meanwhile: whoever resumes it starts it again.
      if (this.generation === gen && !this.running) this.start();
    }, ms);
  }

  private clearRetry() {
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.retryTimer = null;
  }

  /** Stops `recorder`, awaited, so the next start cannot be cancelled by it. */
  private async close(recorder: Recorder) {
    try {
      await recorder.stop();
    } catch {
      recorder.cancel();
    }
  }

  /** Releases the microphone; resolves once it is closed. */
  async pause(): Promise<void> {
    this.clearRetry();
    if (!this.running && !this.recorder) return;
    this.running = false;
    this.generation += 1;
    // Someone is waiting for the microphone: the loop does not sit out its poll.
    this.wakePoll();
    const recorder = this.recorder;
    this.recorder = null;
    // `stop` (not `cancel`) so the release is awaited: whoever takes the
    // microphone next must not be cancelled by a late release.
    if (recorder) await this.close(recorder);
    await this.loop;
  }

  resume(): void {
    this.start();
  }

  stop(): Promise<void> {
    return this.pause();
  }

  private async run(gen: number): Promise<void> {
    const live = () => this.generation === gen;
    // A capture that stops delivering audio (iOS can end it without an
    // error, e.g. around the photo picker or the camera) is replaced.
    while (live()) {
      const outcome = await this.capture(gen);
      if (outcome !== 'stalled' || !live()) return;
      const recorder = this.recorder;
      this.recorder = null;
      if (recorder) await this.close(recorder);
    }
  }

  /** One capture; `stalled` when its buffer stopped growing. */
  private async capture(gen: number): Promise<'stopped' | 'stalled'> {
    const o = this.o;
    const sleep = o.sleep ?? defaultSleep;
    const now = o.now ?? Date.now;
    const pollMs = o.pollMs ?? LISTEN_POLL_MS;
    const fixedSilence = o.endSilenceMs ?? END_SILENCE_MS;
    const stallMs = o.stallMs ?? STALL_MS;
    const recorder = o.createRecorder();
    if (!recorder.peek || !recorder.split) throw new Error('the recorder cannot stream');
    await recorder.start();
    if (this.generation !== gen) {
      await this.close(recorder);
      return 'stopped';
    }
    this.recorder = recorder;
    diag('voco.listen.start');
    let skip = 0;
    /** Of the `skip` frames, how many are silence a lead-in may reach back over. */
    let quietSkip = 0;
    let inSpeech = false;
    /** Frames buffered at the last poll (-1 after a drop), and when the buffer last grew. */
    let lastFrames = -1;
    let grewAt = now();
    let healthy = false;
    let blockedBusy = false;
    /** The buffer did not grow between the last two polls of a voice block. */
    let frozen = false;
    const live = () => this.generation === gen;
    /** Drops the buffer up to a quiet point; `consumed` frames were handled. */
    const drop = async (consumed: number, quiet = false) => {
      const head = await recorder.split!(0);
      if (!live()) return;
      skip = Math.max(0, consumed - framesOf(head.size));
      quietSkip = quiet ? skip : 0;
      if (framesOf(head.size) > 0) lastFrames = -1;
    };
    while (live()) {
      await Promise.race([
        sleep(pollMs),
        new Promise<void>((resolve) => {
          this.wakePoll = resolve;
        }),
      ]);
      if (!live()) return 'stopped';
      const wav = await bytesOf(await recorder.peek());
      if (!live()) return 'stopped';
      const levels = frameLevels(wav);
      const busy = o.busy?.() ?? false;
      const grew = levels.length > lastFrames;
      if (grew) {
        grewAt = now();
        if (!healthy && levels.length > 0) {
          healthy = true;
          this.failures = 0;
          o.onListening?.();
        }
      } else if (!busy && stallMs > 0 && (frozen || now() - grewAt >= stallMs)) {
        diag('voco.blocked', {
          reason: 'mic_stalled',
          frames: levels.length,
          ms: now() - grewAt,
          afterVoice: frozen,
        });
        if (inSpeech) o.onSpeechEnd(null);
        return 'stalled';
      }
      lastFrames = levels.length;
      frozen = busy && !grew;
      if (busy) {
        // Another voice is playing: none of it is speech for us.
        if (!blockedBusy) diag('voco.blocked', { reason: 'voice_playing' });
        blockedBusy = true;
        if (inSpeech) o.onSpeechEnd(null);
        inSpeech = false;
        await drop(levels.length);
        continue;
      }
      blockedBusy = false;
      // Adaptive VAD: the noise floor follows the room while nobody speaks.
      if (!inSpeech) this.floor = noiseFloor(levels) ?? this.floor;
      const threshold = o.threshold ?? speechThreshold(this.floor);
      const fresh = levels.slice(skip);
      const bursts = speechBursts(fresh, threshold);
      if (bursts.length === 0) {
        if (inSpeech) o.onSpeechEnd(null);
        inSpeech = false;
        if (levels.length * FRAME_MS >= DROP_SILENCE_MS) await drop(levels.length, true);
        continue;
      }
      const first = bursts[0]![0];
      const last = bursts[bursts.length - 1]![1];
      const speechFrames = bursts.reduce((n, [s, e]) => n + e - s, 0);
      if (!inSpeech && speechFrames * FRAME_MS >= 100) {
        inSpeech = true;
        diag('voco.vad', {
          decision: 'speech',
          rms: level3(Math.max(...fresh.slice(first, last))),
          floor: level3(this.floor),
          threshold: level3(threshold),
        });
        o.onSpeechStart();
      }
      const trailing = fresh.length - last;
      const long = (fresh.length - first) * FRAME_MS >= MAX_UTTERANCE_MS;
      const endSilence = (o.endSilenceFor?.((last - first) * FRAME_MS) ?? fixedSilence) / FRAME_MS;
      if (trailing < endSilence && !long) continue;
      // The utterance ended (or is too long to wait for).
      const from = skip + Math.max(-quietSkip, first - LEAD_MS / FRAME_MS);
      const to = skip + Math.min(fresh.length, last + TAIL_MS / FRAME_MS);
      const tooShort = speechFrames * FRAME_MS < MIN_SPEECH_MS;
      diag('voco.vad', {
        decision: tooShort ? 'noise' : 'utterance',
        speechMs: speechFrames * FRAME_MS,
        rms: level3(Math.max(...fresh.slice(first, last))),
        floor: level3(this.floor),
        threshold: level3(threshold),
      });
      const utterance: HeardUtterance | null = tooShort
        ? null
        : {
            audio: sliceWav(wav, from, to),
            head: (ms: number) => sliceWav(wav, from, Math.min(to, skip + first + ms / FRAME_MS)),
            speechMs: (last - first) * FRAME_MS,
            pausedBeforeLast: pausedBeforeLast(bursts),
            endedAt: now() - trailing * FRAME_MS,
          };
      inSpeech = false;
      await drop(levels.length);
      if (!live()) return 'stopped';
      o.onSpeechEnd(utterance);
    }
    return 'stopped';
  }
}
