/**
 * Live dictation (VOC-01/VOC-04): turns a running recording into text that
 * grows in the composer while the user speaks.
 *
 * With a streaming recorder (`peek` + `split`, the native shell):
 * - long recordings are cut into FINAL chunks at quiet points (`split`), each
 *   transcribed once and committed in recording order (a sequence number
 *   places out-of-order completions correctly);
 * - between cuts, the audio since the last cut is transcribed as an INTERIM
 *   tail (`peek`) that is replaced on every update;
 * - on stop, the remainder is transcribed as the last FINAL chunk.
 * Recorders without `peek`/`split` (the browser MediaRecorder) are
 * transcribed once on stop.
 *
 * The state functions are pure; `Dictation` drives them against a recorder.
 */
import type { Recorder } from './recorder';
import { audioEncodingOf, type Transcript, type TranscriptCorrection } from './transcribe';

/** Recordings shorter than this are accidental taps and are discarded. */
export const MIN_RECORDING_MS = 300;
/** `split` is asked to cut once at least this many seconds are buffered. */
export const CHUNK_SECONDS = 15;
/** Minimum time between the starts of two live requests. */
export const MIN_REQUEST_GAP_MS = 700;
/** How much preceding text is sent as recognition context. */
export const CONTEXT_CHARS = 300;

/** 16 kHz, 16-bit mono PCM WAV from the native recorder. */
const WAV_HEADER_BYTES = 44;
const WAV_BYTES_PER_SECOND = 32_000;

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

export type DictationTranscribe = (audio: Blob, context: string | null) => Promise<Transcript>;

export interface DictationOptions {
  recorder: Recorder;
  transcribe: DictationTranscribe;
  /** Composer text before dictation. */
  base: string;
  onUpdate: (state: DictationState) => void;
  chunkSeconds?: number;
  minGapMs?: number;
  /** Injectable for tests. */
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
}

export interface DictationResult {
  state: DictationState;
  /** First transcription error, if any chunk failed. */
  error: unknown;
  /** Nothing usable was recorded (only a sub-`MIN_RECORDING_MS` WAV). */
  tooShort: boolean;
}

const defaultSleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

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
  /** The latest native split/peek call (finish waits for it before stopping). */
  private native: Promise<unknown> = Promise.resolve();
  private readonly finals = new Set<Promise<void>>();
  private error: unknown = null;

  constructor(private readonly opts: DictationOptions) {
    this.state = initialDictation(opts.base);
  }

  get current(): DictationState {
    return this.state;
  }

  /** Whether the recorder can provide live text while recording. */
  get streaming(): boolean {
    const r = this.opts.recorder;
    return (!!r.peek && !this.peekBroken) || (!!r.split && !this.splitBroken);
  }

  private set(next: DictationState) {
    if (this.abandoned || next === this.state) return;
    this.state = next;
    this.opts.onUpdate(next);
  }

  /**
   * Adaptive loop: starts the next live request as soon as the previous one
   * resolves, but no sooner than `minGapMs` after the previous start.
   */
  async run(): Promise<void> {
    if (this.running) return;
    this.running = true;
    const now = this.opts.now ?? Date.now;
    const sleep = this.opts.sleep ?? defaultSleep;
    const gap = this.opts.minGapMs ?? MIN_REQUEST_GAP_MS;
    while (!this.ended && this.streaming) {
      const startedAt = now();
      await this.step();
      if (this.ended) break;
      await sleep(Math.max(0, gap - (now() - startedAt)));
    }
  }

  /** One live request: commit a FINAL chunk if enough audio is buffered, else refresh the INTERIM tail. */
  async step(): Promise<void> {
    if (this.busy || this.ended) return;
    const { recorder } = this.opts;
    this.busy = true;
    try {
      if (recorder.split && !this.splitBroken) {
        const pending = recorder.split(this.opts.chunkSeconds ?? CHUNK_SECONDS).then((chunk) => {
          if (chunk.size === 0 || this.abandoned) return null;
          // Claim the sequence number synchronously, before `finish` can.
          const cut = cutChunk(this.state);
          this.set(cut.state);
          return { seq: cut.seq, chunk };
        });
        this.native = pending.catch(() => null);
        let cut: { seq: number; chunk: Blob } | null = null;
        try {
          cut = await pending;
        } catch {
          this.splitBroken = true;
        }
        if (cut) {
          await this.transcribeFinal(cut.seq, cut.chunk);
          return;
        }
      }
      if (recorder.peek && !this.peekBroken && !this.ended) {
        const epoch = this.state.epoch;
        const pending = recorder.peek();
        this.native = pending.catch(() => null);
        let audio: Blob;
        try {
          audio = await pending;
        } catch {
          this.peekBroken = true;
          return;
        }
        if (this.ended || epoch !== this.state.epoch || isShortWav(audio)) return;
        const interim = await this.opts.transcribe(audio, contextOf(committedText(this.state)));
        if (!this.ended) this.set(applyInterim(this.state, epoch, interim.text));
      }
    } catch {
      // Interim previews are best-effort; the final transcript is what counts.
    } finally {
      this.busy = false;
    }
  }

  private transcribeFinal(seq: number, audio: Blob): Promise<void> {
    const context = contextOf(committedText(this.state, seq));
    const done = this.opts.transcribe(audio, context).then(
      (t) => this.set(applyFinal(this.state, seq, t)),
      (err: unknown) => {
        this.error ??= err;
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
    await this.native;
    let remainder: Blob | null = null;
    try {
      remainder = await this.opts.recorder.stop();
    } catch (err) {
      this.error ??= err;
    }
    const hadChunks = this.state.segments.length > 0;
    const shortRemainder = !remainder || isShortWav(remainder);
    if (shortRemainder) {
      this.set({ ...this.state, tail: '' });
    } else {
      const cut = cutChunk(this.state);
      this.set(cut.state);
      void this.transcribeFinal(cut.seq, remainder!);
    }
    while (this.finals.size > 0) await Promise.all([...this.finals]);
    return {
      state: this.state,
      error: this.error,
      tooShort: !hadChunks && !!remainder && shortRemainder,
    };
  }

  /** Discards the run: releases the microphone and ignores any late results. */
  abandon(): void {
    if (this.abandoned) return;
    this.ended = true;
    this.abandoned = true;
    this.opts.recorder.cancel();
  }
}
