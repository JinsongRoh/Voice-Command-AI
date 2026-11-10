/**
 * Live dictation (VOC-01/VOC-04): turns a running recording into text that
 * grows in the composer while the user speaks.
 *
 * With a streaming recorder (`peek` + `split`, the native shell):
 * - long recordings are cut into FINAL chunks at quiet points (`split`), each
 *   transcribed once and committed in recording order (a sequence number
 *   places out-of-order completions correctly);
 * - between cuts, the audio since the last cut is transcribed as an INTERIM
 *   tail (`peek`) that is replaced on every update. Each preview re-sends
 *   that whole tail, so previews back off as it grows and when the server is
 *   slow (`previewGapMs`); the cut checks keep their own fast cadence;
 * - on stop, the remainder is transcribed as the last FINAL chunk.
 * Recorders without `peek`/`split` (the browser MediaRecorder) are
 * transcribed once on stop.
 *
 * iOS can take the input away without an error (audio playback changes the
 * route) and the capture does not come back by itself. With `captured`, a
 * capture that stopped growing for `MIC_STALL_MS` is stopped, its audio becomes
 * a FINAL chunk and a fresh capture continues the same dictation (opening one
 * reactivates the audio session).
 *
 * A FINAL chunk that only repeats the end of the context it was sent with, and
 * that the previews say was not spoken, is the engine's echo of that context:
 * it is dropped (`isContextEcho`).
 *
 * The state functions are pure; `Dictation` drives them against a recorder.
 */
import { diag, errorInfo } from '../diag/log';
import { isContextEcho, repeatsContextTail, wavLevel, type ChunkPreview } from './hallucinations';
import type { Recorder } from './recorder';
import {
  audioEncodingOf,
  blobBytes,
  type Transcript,
  type TranscriptCorrection,
} from './transcribe';

/** Recordings shorter than this are accidental taps and are discarded. */
export const MIN_RECORDING_MS = 300;
/** `split` is asked to cut once at least this many seconds are buffered. */
export const CHUNK_SECONDS = 15;
/** Minimum time between the starts of two live requests. */
export const MIN_REQUEST_GAP_MS = 700;
/** How much preceding text is sent as recognition context. */
export const CONTEXT_CHARS = 300;
/** A recorder that has delivered no audio at all this long after start is stuck. */
export const NO_AUDIO_MS = 8_000;
/** Longest wait for the native recorder to answer a stop/peek/split call. */
export const NATIVE_TIMEOUT_MS = 15_000;
/** A capture that has not grown this long lost its input (it is restarted). */
export const MIC_STALL_MS = 1_000;
/** Restarts in a row without new audio; each waits twice as long as the last. */
export const MAX_MIC_RESTARTS = 3;

/**
 * Minimum time between interim previews by the length of the audio they
 * re-send (a preview always covers the whole tail since the last cut, so a
 * long tail costs the server a lot every time). Short tails stay snappy.
 */
export const PREVIEW_BACKOFF: readonly { readonly fromMs: number; readonly gapMs: number }[] = [
  { fromMs: 0, gapMs: MIN_REQUEST_GAP_MS },
  { fromMs: 4_000, gapMs: 1_500 },
  { fromMs: 8_000, gapMs: 3_000 },
];
/** Previews leave the server idle at least this many times their own duration. */
export const PREVIEW_LATENCY_FACTOR = 2;
/** A preview slower than this pauses previews until the next cut (the server is busy). */
export const PREVIEW_SLOW_MS = 4_000;
/**
 * After a failed preview (a weak or lost link) the next waits this long,
 * doubling per failure in a row up to `PREVIEW_ERROR_MAX_MS`: previews must
 * not crowd the link the final chunks need.
 */
export const PREVIEW_ERROR_GAP_MS = 3_000;
export const PREVIEW_ERROR_MAX_MS = 24_000;

/** 16 kHz, 16-bit mono PCM WAV from the native recorder. */
const WAV_HEADER_BYTES = 44;
const WAV_BYTES_PER_SECOND = 32_000;

/** Duration of a native-recorder WAV in ms (null for other formats). */
export function wavDurationMs(audio: Blob): number | null {
  if (audioEncodingOf(audio.type) !== 'wav') return null;
  return (Math.max(0, audio.size - WAV_HEADER_BYTES) * 1000) / WAV_BYTES_PER_SECOND;
}

/**
 * How long after a preview's start the next preview may start: the backoff
 * step for the audio it sent (`null`: unknown length, the first step), but at
 * least `PREVIEW_LATENCY_FACTOR` times what the server took, and never less
 * than `minGapMs`. `null` when the server is too slow for previews at all.
 */
export function previewGapMs(
  audioMs: number | null,
  latencyMs: number,
  minGapMs = MIN_REQUEST_GAP_MS,
): number | null {
  if (latencyMs >= PREVIEW_SLOW_MS) return null;
  let step = PREVIEW_BACKOFF[0]!.gapMs;
  for (const s of PREVIEW_BACKOFF) if ((audioMs ?? 0) >= s.fromMs) step = s.gapMs;
  return Math.max(minGapMs, step, latencyMs * PREVIEW_LATENCY_FACTOR);
}

/** True for WAV clips holding less than `MIN_RECORDING_MS` of audio. */
export function isShortWav(audio: Blob): boolean {
  if (audioEncodingOf(audio.type) !== 'wav') return false;
  return audio.size < WAV_HEADER_BYTES + (WAV_BYTES_PER_SECOND * MIN_RECORDING_MS) / 1000;
}

/** True when a recording is too short to contain speech (by duration, or WAV size). */
export function isTooShort(elapsedMs: number, audio?: Blob): boolean {
  if (elapsedMs < MIN_RECORDING_MS) return true;
  return audio ? isShortWav(audio) : false;
}

/** Formats a duration as mm:ss. */
export function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const mm = String(Math.floor(total / 60)).padStart(2, '0');
  const ss = String(total % 60).padStart(2, '0');
  return `${mm}:${ss}`;
}

/** Appends `next` to `text`, adding a space unless one side already provides it. */
export function joinText(text: string, next: string): string {
  if (!next) return text;
  if (!text) return next;
  return /\s$/.test(text) || /^\s/.test(next) ? text + next : `${text} ${next}`;
}

/** The last `CONTEXT_CHARS` characters of `text` (null when blank). */
export function contextOf(text: string): string | null {
  const trimmed = text.trim();
  return trimmed ? trimmed.slice(-CONTEXT_CHARS) : null;
}

export interface Segment {
  seq: number;
  /** Corrected text (what goes into the composer). */
  text: string;
  /** Recognizer output before filler removal and vocabulary correction. */
  raw: string;
  corrections: TranscriptCorrection[];
  /** False while the chunk is being transcribed (`text` is then the last interim). */
  final: boolean;
}

export interface DictationState {
  /** Composer text from before dictation started (kept untouched). */
  base: string;
  /** Committed chunks, ordered by `seq`. */
  segments: Segment[];
  /** Interim transcript of the audio since the last cut. */
  tail: string;
  /** Incremented on every cut; interim results from an older epoch are stale. */
  epoch: number;
  nextSeq: number;
}

export function initialDictation(base: string): DictationState {
  return { base, segments: [], tail: '', epoch: 0, nextSeq: 0 };
}

/**
 * Starts a FINAL chunk for audio that was just cut off. The current interim
 * tail stands in for it until its transcript arrives (no flicker).
 */
export function cutChunk(state: DictationState): { state: DictationState; seq: number } {
  const seq = state.nextSeq;
  return {
    seq,
    state: {
      ...state,
      segments: [
        ...state.segments,
        { seq, text: state.tail, raw: state.tail, corrections: [], final: false },
      ],
      tail: '',
      epoch: state.epoch + 1,
      nextSeq: seq + 1,
    },
  };
}

/** Places a chunk's final transcript at its position, whatever order it arrives in. */
export function applyFinal(state: DictationState, seq: number, t: Transcript): DictationState {
  if (!state.segments.some((s) => s.seq === seq)) return state;
  return {
    ...state,
    segments: state.segments.map((s) =>
      s.seq === seq
        ? {
            seq,
            text: t.text.trim(),
            raw: (t.rawText ?? t.text).trim(),
            corrections: t.corrections,
            final: true,
          }
        : s,
    ),
  };
}

/** A chunk whose transcription failed keeps its interim stand-in text. */
export function failChunk(state: DictationState, seq: number): DictationState {
  return {
    ...state,
    segments: state.segments.map((s) => (s.seq === seq ? { ...s, final: true } : s)),
  };
}

/** Replaces the interim tail, unless the audio it covers has since been cut. */
export function applyInterim(state: DictationState, epoch: number, text: string): DictationState {
  if (epoch !== state.epoch) return state;
  return { ...state, tail: text.trim() };
}

export interface ComposedDictation {
  /** Full composer text: base + committed chunks + interim tail. */
  text: string;
  /** Characters at the end of `text` that are still interim (shown dimmed). */
  tailLength: number;
  /** Vocabulary corrections of the committed chunks. */
  corrections: TranscriptCorrection[];
  /** Corrected text of this run only (committed chunks, without the base). */
  dictated: string;
  /** Raw (uncorrected) text of this run's committed chunks. */
  raw: string;
}

/** Text before chunk `seq` (all chunks when omitted), including the base. */
export function committedText(state: DictationState, beforeSeq = Infinity): string {
  return [...state.segments]
    .filter((s) => s.seq < beforeSeq)
    .sort((a, b) => a.seq - b.seq)
    .reduce((acc, s) => joinText(acc, s.text), state.base);
}

export function composeDictation(state: DictationState): ComposedDictation {
  const committed = committedText(state);
  const text = joinText(committed, state.tail);
  const ordered = [...state.segments].sort((a, b) => a.seq - b.seq);
  return {
    text,
    tailLength: text.length - committed.length,
    corrections: ordered.flatMap((s) => s.corrections),
    dictated: ordered.reduce((acc, s) => joinText(acc, s.text), ''),
    raw: ordered.reduce((acc, s) => joinText(acc, s.raw), ''),
  };
}

/**
 * Transcribes one piece of a dictation. `final` is true for FINAL chunks
 * (split cuts and the remainder on stop) and false for interim previews.
 */
export type DictationTranscribe = (
  audio: Blob,
  context: string | null,
  final: boolean,
) => Promise<Transcript>;

export interface DictationOptions {
  recorder: Recorder;
  transcribe: DictationTranscribe;
  /** Composer text before dictation. */
  base: string;
  onUpdate: (state: DictationState) => void;
  chunkSeconds?: number;
  minGapMs?: number;
  /**
   * Called once when the recorder has delivered no audio within `noAudioMs`
   * of `run()` (a dead input: nothing will ever be transcribed). The caller
   * should drop the dictation and tell the user.
   */
  onStall?: () => void;
  noAudioMs?: number;
  /**
   * The capture stopped delivering audio and is being restarted: what was
   * said in the gap is gone, so the caller should tell the user.
   */
  onMicStalled?: () => void;
  /** See `MIC_STALL_MS`; `0` turns the check off. */
  stallMs?: number;
  /** Longest wait for a native stop/peek/split answer (default `NATIVE_TIMEOUT_MS`). */
  nativeTimeoutMs?: number;
  /** Injectable for tests. */
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
}

/** A FINAL chunk whose transcription failed, kept so it can be sent again. */
export interface FailedChunk {
  seq: number;
  audio: Blob;
  error: unknown;
}

export interface DictationResult {
  state: DictationState;
  /** First transcription error, if any chunk failed. */
  error: unknown;
  /** The chunks that failed, in order, with their audio (see `retryFailedChunks`). */
  failed: FailedChunk[];
  /** Nothing usable was recorded (only a sub-`MIN_RECORDING_MS` WAV). */
  tooShort: boolean;
}

/** Rejects when `promise` has not settled within `ms` (the promise itself is left alone). */
function withTimeout<T>(promise: Promise<T>, ms: number, what: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${what} timed out`)), ms);
    promise.then(
      (v) => {
        clearTimeout(timer);
        resolve(v);
      },
      (e: unknown) => {
        clearTimeout(timer);
        reject(e instanceof Error ? e : new Error(String(e)));
      },
    );
  });
}

const defaultSleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** Whether the final transcript of `audio` is `context` coming back, not speech. */
async function isEcho(
  t: Transcript,
  context: string | null,
  audio: Blob,
  preview: ChunkPreview,
): Promise<boolean> {
  // The correction pass may have changed the text: the raw output counts too.
  const text = [t.rawText, t.text].find((x) => x && repeatsContextTail(x, context));
  if (!text || preview.heard) return false;
  const bytes = await blobBytes(audio).catch(() => null);
  return !!bytes && isContextEcho(text, wavLevel(bytes), preview);
}

/**
 * Drives one dictation run. Call `run()` once the recorder has started, then
 * exactly one of `finish()` (stop and transcribe the rest) or `abandon()`.
 * At most one live request (split/peek plus its transcription) is in flight.
 */
export class Dictation {
  private state: DictationState;
  private busy = false;
  private running = false;
  private ended = false;
  private abandoned = false;
  private peekBroken = false;
  private splitBroken = false;
  /** Some captured audio (more than a bare WAV header) has been seen. */
  private audioSeen = false;
  private stallTimer: ReturnType<typeof setTimeout> | null = null;
  /** The recorder holds an open capture (false only if a restart failed). */
  private capturing = true;
  private capturedBroken = false;
  /** `captured` at the last check (-1: not read yet for this capture). */
  private samples = -1;
  /** `samples` when the latest preview was sent. */
  private previewedSamples = -1;
  private grewAt = 0;
  /** Restarts since the capture last delivered audio. */
  private restarts = 0;
  /** `run()` skips previews before this time (cut checks still happen). */
  private nextPreviewAt = 0;
  /** What the previews of the audio since the last cut found. */
  private preview: ChunkPreview = { heard: false, ms: 0 };
  /** The latest native split/peek call (finish waits for it before stopping). */
  private native: Promise<unknown> = Promise.resolve();
  private readonly finals = new Set<Promise<void>>();
  private error: unknown = null;
  private readonly failed: FailedChunk[] = [];
  /** Previews that failed in a row (see `PREVIEW_ERROR_GAP_MS`). */
  private previewFailures = 0;

  constructor(private readonly opts: DictationOptions) {
    this.state = initialDictation(opts.base);
  }

  get current(): DictationState {
    return this.state;
  }

  /**
   * Replaces the text the dictation is appended to (something was added to
   * the box meanwhile, for example an attachment's mention).
   */
  setBase(base: string): void {
    this.set({ ...this.state, base });
  }

  /** Whether the recorder can provide live text while recording. */
  get streaming(): boolean {
    const r = this.opts.recorder;
    return (!!r.peek && !this.peekBroken) || (!!r.split && !this.splitBroken);
  }

  private noteAudio(blob: Blob) {
    if (blob.size > WAV_HEADER_BYTES) this.audioSeen = true;
  }

  private clearStall() {
    if (this.stallTimer) clearTimeout(this.stallTimer);
    this.stallTimer = null;
  }

  /** Fires `onStall` once when the recorder is dead: no audio since start. */
  private checkStall() {
    this.clearStall();
    if (this.ended || this.abandoned || this.audioSeen) return;
    diag('dictation.stall');
    this.opts.onStall?.();
  }

  /**
   * Reads the capture's progress; true when it is time to restart it. A
   * capture that never delivered anything is `onStall`'s case, not this one.
   */
  private async captureStalled(): Promise<boolean> {
    const stallMs = this.opts.stallMs ?? MIC_STALL_MS;
    if (this.capturedBroken || stallMs <= 0) return false;
    const now = this.opts.now ?? Date.now;
    const due = () =>
      this.restarts < MAX_MIC_RESTARTS && now() - this.grewAt >= stallMs * 2 ** this.restarts;
    if (!this.capturing) return due();
    let samples: number | null;
    try {
      samples = (await this.opts.recorder.captured?.()) ?? null;
    } catch (err) {
      this.capturedBroken = true;
      diag('dictation.captured.broken', errorInfo(err));
      return false;
    }
    if (samples === null || this.ended) return false;
    if (samples > this.samples) {
      this.grewAt = now();
      this.samples = samples;
      if (samples > 0) this.restarts = 0;
      return false;
    }
    return (samples > 0 || this.restarts > 0) && due();
  }

  /** Commits what the stopped capture holds and opens a fresh one. */
  private async restartCapture(): Promise<void> {
    const { recorder } = this.opts;
    const now = this.opts.now ?? Date.now;
    const nativeMs = this.opts.nativeTimeoutMs ?? NATIVE_TIMEOUT_MS;
    diag('dictation.mic_stalled', {
      samples: this.samples,
      ms: now() - this.grewAt,
      restarts: this.restarts,
      capturing: this.capturing,
    });
    this.opts.onMicStalled?.();
    if (this.capturing) {
      this.capturing = false;
      // The same audio as `stop`, in case `stop` cannot deliver it.
      let audio = (await recorder.peek?.().catch(() => null)) ?? null;
      if (this.abandoned) return;
      try {
        audio = await withTimeout(recorder.stop(), nativeMs, 'microphone stop');
      } catch (err) {
        diag('dictation.mic_stop.error', errorInfo(err));
        recorder.cancel();
      }
      if (this.abandoned) return;
      if (audio && audio.size > 0 && !isShortWav(audio)) {
        const cut = cutChunk(this.state);
        this.set(cut.state);
        this.nextPreviewAt = 0;
        void this.transcribeFinal(cut.seq, audio);
      }
    }
    if (this.ended) return;
    this.restarts += 1;
    this.samples = -1;
    this.previewedSamples = -1;
    this.grewAt = now();
    try {
      await withTimeout(recorder.start(), nativeMs, 'microphone start');
      this.capturing = true;
      diag('dictation.mic_restarted', { restarts: this.restarts });
    } catch (err) {
      diag('dictation.mic_restart.error', errorInfo(err));
      recorder.cancel();
    }
  }

  private set(next: DictationState) {
    if (this.abandoned || next === this.state) return;
    this.state = next;
    this.opts.onUpdate(next);
  }

  /**
   * Adaptive loop: starts the next live request as soon as the previous one
   * resolves, but no sooner than `minGapMs` after the previous start. Every
   * round checks for a cut; an interim preview only runs once due
   * (`previewGapMs`).
   */
  async run(): Promise<void> {
    if (this.running) return;
    this.running = true;
    diag('dictation.run', { streaming: this.streaming, baseChars: this.state.base.length });
    const now = this.opts.now ?? Date.now;
    const sleep = this.opts.sleep ?? defaultSleep;
    const gap = this.opts.minGapMs ?? MIN_REQUEST_GAP_MS;
    const live = this.streaming;
    if (live && this.opts.onStall) {
      this.stallTimer = setTimeout(() => this.checkStall(), this.opts.noAudioMs ?? NO_AUDIO_MS);
    }
    while (!this.ended && this.streaming) {
      const startedAt = now();
      await this.step(startedAt >= this.nextPreviewAt);
      if (this.ended) break;
      await sleep(Math.max(0, gap - (now() - startedAt)));
    }
    // The live loop gave up (the recorder cannot answer peek/split any more):
    // without audio nothing can ever appear.
    if (live && !this.ended && !this.streaming) this.checkStall();
  }

  /**
   * One live request: commit a FINAL chunk if enough audio is buffered, else
   * refresh the INTERIM tail (only when `preview`).
   */
  async step(preview = true): Promise<void> {
    if (this.busy || this.ended) return;
    const { recorder } = this.opts;
    this.busy = true;
    try {
      if (recorder.captured && (await this.captureStalled())) {
        const restart = this.restartCapture();
        this.native = restart.catch(() => null);
        await restart;
        return;
      }
      if (this.ended || !this.capturing) return;
      if (recorder.split && !this.splitBroken) {
        const pending = recorder.split(this.opts.chunkSeconds ?? CHUNK_SECONDS).then((chunk) => {
          this.noteAudio(chunk);
          if (chunk.size > 0) diag('dictation.cut', { bytes: chunk.size });
          if (chunk.size === 0 || this.abandoned) return null;
          // Claim the sequence number synchronously, before `finish` can.
          const cut = cutChunk(this.state);
          this.set(cut.state);
          // A new, short tail: preview it promptly (also after a slow server).
          this.nextPreviewAt = 0;
          return { seq: cut.seq, chunk };
        });
        this.native = pending.catch(() => null);
        let cut: { seq: number; chunk: Blob } | null = null;
        try {
          cut = await pending;
        } catch (err) {
          this.splitBroken = true;
          diag('dictation.split.broken', errorInfo(err));
        }
        if (cut) {
          await this.transcribeFinal(cut.seq, cut.chunk);
          return;
        }
      }
      if (preview && recorder.peek && !this.peekBroken && !this.ended) {
        // A frozen capture: this audio was already sent, nothing new can come.
        if (this.samples > 0 && this.samples === this.previewedSamples) return;
        const now = this.opts.now ?? Date.now;
        const startedAt = now();
        const epoch = this.state.epoch;
        const pending = recorder.peek();
        this.native = pending.catch(() => null);
        let audio: Blob;
        try {
          audio = await pending;
        } catch (err) {
          this.peekBroken = true;
          diag('dictation.peek.broken', errorInfo(err));
          return;
        }
        this.noteAudio(audio);
        if (this.ended || epoch !== this.state.epoch || isShortWav(audio)) return;
        const sentAt = now();
        this.previewedSamples = this.samples;
        // Still counts if it answers after the cut, while the chunk is in the works.
        const seen = this.preview;
        let failed = true;
        try {
          const interim = await this.opts.transcribe(
            audio,
            contextOf(committedText(this.state)),
            false,
          );
          failed = false;
          seen.ms = Math.max(seen.ms, wavDurationMs(audio) ?? 0);
          seen.heard ||= !!interim.text.trim() || !!interim.rawText?.trim();
          if (!this.ended) this.set(applyInterim(this.state, epoch, interim.text));
        } finally {
          this.schedulePreview(epoch, startedAt, wavDurationMs(audio), now() - sentAt);
          this.previewFailures = failed ? this.previewFailures + 1 : 0;
          if (failed && epoch === this.state.epoch) {
            const wait = Math.min(
              PREVIEW_ERROR_MAX_MS,
              PREVIEW_ERROR_GAP_MS * 2 ** (this.previewFailures - 1),
            );
            this.nextPreviewAt = Math.max(this.nextPreviewAt, startedAt + wait);
            diag('dictation.preview.backoff', { failures: this.previewFailures, waitMs: wait });
          }
        }
      }
    } catch (err) {
      // Interim previews are best-effort; the final transcript is what counts.
      diag('dictation.step.error', errorInfo(err));
    } finally {
      this.busy = false;
    }
  }

  /** Sets when the next preview may start, after one that started at `startedAt`. */
  private schedulePreview(
    epoch: number,
    startedAt: number,
    audioMs: number | null,
    latencyMs: number,
  ) {
    // A cut meanwhile already reset the schedule for the new tail.
    if (epoch !== this.state.epoch) return;
    const gap = previewGapMs(audioMs, latencyMs, this.opts.minGapMs ?? MIN_REQUEST_GAP_MS);
    if (gap === null) diag('dictation.preview.paused', { latencyMs, audioMs });
    this.nextPreviewAt = gap === null ? Infinity : startedAt + gap;
  }

  private transcribeFinal(seq: number, audio: Blob): Promise<void> {
    const context = contextOf(committedText(this.state, seq));
    const preview = this.preview;
    this.preview = { heard: false, ms: 0 };
    const done = this.opts.transcribe(audio, context, true).then(
      async (t) => {
        if (await isEcho(t, context, audio, preview)) {
          diag('dictation.echo_dropped', {
            seq,
            textChars: (t.rawText ?? t.text).length,
            contextChars: context?.length ?? 0,
            audioMs: Math.round(wavDurationMs(audio) ?? 0),
            previewMs: Math.round(preview.ms),
          });
          this.set(applyFinal(this.state, seq, { text: '', corrections: [] }));
          return;
        }
        this.set(applyFinal(this.state, seq, t));
      },
      (err: unknown) => {
        this.error ??= err;
        diag('dictation.final.error', { seq, ...errorInfo(err) });
        if (!this.abandoned) this.failed.push({ seq, audio, error: err });
        this.set(failChunk(this.state, seq));
      },
    );
    this.finals.add(done);
    void done.then(() => this.finals.delete(done));
    return done;
  }

  /** Stops the recorder, transcribes the remainder and waits for every chunk. */
  async finish(): Promise<DictationResult> {
    this.ended = true;
    this.clearStall();
    diag('dictation.finish', { segments: this.state.segments.length, audioSeen: this.audioSeen });
    const nativeMs = this.opts.nativeTimeoutMs ?? NATIVE_TIMEOUT_MS;
    // A native call that never answers must not leave the composer waiting forever.
    await withTimeout(this.native, nativeMs, 'microphone').catch(() => null);
    let remainder: Blob | null = null;
    try {
      // A failed restart left nothing open; its audio is already a chunk.
      if (this.capturing) {
        remainder = await withTimeout(this.opts.recorder.stop(), nativeMs, 'microphone stop');
      }
    } catch (err) {
      this.error ??= err;
      // The recorder did not answer: make sure it is released.
      this.opts.recorder.cancel();
    }
    const hadChunks = this.state.segments.length > 0;
    diag('dictation.remainder', { bytes: remainder?.size ?? null, hadChunks });
    const shortRemainder = !remainder || isShortWav(remainder);
    if (shortRemainder) {
      this.set({ ...this.state, tail: '' });
    } else {
      const cut = cutChunk(this.state);
      this.set(cut.state);
      void this.transcribeFinal(cut.seq, remainder!);
    }
    while (this.finals.size > 0) await Promise.all([...this.finals]);
    diag('dictation.done', {
      segments: this.state.segments.length,
      textChars: composeDictation(this.state).dictated.length,
      failed: this.error !== null,
    });
    return {
      state: this.state,
      error: this.error,
      failed: [...this.failed].sort((a, b) => a.seq - b.seq),
      tooShort: !hadChunks && !!remainder && shortRemainder,
    };
  }

  /** Discards the run: releases the microphone and ignores any late results. */
  abandon(): void {
    if (this.abandoned) return;
    this.clearStall();
    this.ended = true;
    this.abandoned = true;
    diag('dictation.abandon');
    this.opts.recorder.cancel();
  }
}

/**
 * Sends the kept audio of failed chunks again ("tap to retry"), in order,
 * each with the text before it as context. A chunk that succeeds replaces its
 * stand-in text; the rest stay failed for another try.
 */
export async function retryFailedChunks(
  state: DictationState,
  failed: readonly FailedChunk[],
  transcribe: DictationTranscribe,
): Promise<{ state: DictationState; failed: FailedChunk[]; error: unknown }> {
  let next = state;
  const still: FailedChunk[] = [];
  let error: unknown = null;
  for (const chunk of [...failed].sort((a, b) => a.seq - b.seq)) {
    try {
      const t = await transcribe(chunk.audio, contextOf(committedText(next, chunk.seq)), true);
      next = applyFinal(next, chunk.seq, t);
      diag('dictation.retry.ok', { seq: chunk.seq });
    } catch (err) {
      error ??= err;
      still.push({ ...chunk, error: err });
      diag('dictation.retry.error', { seq: chunk.seq, ...errorInfo(err) });
    }
  }
  return { state: next, failed: still, error };
}
