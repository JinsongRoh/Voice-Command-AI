import { diag, diagEnabled, errorInfo } from '../diag/log';
import { bridge } from '../native/bridge';
import { captureOptions } from './captureOptions';

/** Push-to-talk audio capture (VOC-04). */
export interface Recorder {
  start(): Promise<void>;
  /** Stops capturing, releases the microphone and returns the audio. */
  stop(): Promise<Blob>;
  /**
   * Audio captured since the last `split` (or start), without stopping.
   * Optional: without it there is no live interim transcript.
   */
  peek?(): Promise<Blob>;
  /**
   * When at least `minSeconds` are buffered, cuts the buffer at a quiet point
   * and returns the audio before the cut (recording continues); otherwise
   * returns an empty Blob. Optional: without it the whole recording is
   * transcribed once on stop.
   */
  split?(minSeconds: number): Promise<Blob>;
  /**
   * Samples the current capture has received from the device so far (never
   * shrinks, `split` does not touch it), or `null` when unknown. Optional:
   * without it a capture that stops delivering audio goes unnoticed.
   */
  captured?(): Promise<number | null>;
  /** Stops without producing audio. */
  cancel(): void;
  /**
   * The live input level (RMS, 0 to 1) while recording, or `null` when it
   * cannot be measured. Optional: the mic button's motion falls back to a
   * gentle synthetic level without it. Must be cheap: read once per frame.
   */
  level?(): number | null;
}

export type RecorderFactory = () => Recorder;

/** How often `firstFrameAt` reads the capture, and for how long. */
export const FIRST_FRAME_POLL_MS = 30;
export const FIRST_FRAME_MAX_MS = 2000;

/**
 * `now()` when a just opened capture first held audio (up to `pollMs` late),
 * for the press-to-audio timing of the log. `null` when that is unknown: no
 * counter, no audio in time, or `live()` turned false meanwhile.
 */
export async function firstFrameAt(
  recorder: Recorder,
  live: () => boolean,
  o: {
    now?: () => number;
    sleep?: (ms: number) => Promise<void>;
    pollMs?: number;
    maxMs?: number;
  } = {},
): Promise<number | null> {
  const now = o.now ?? Date.now;
  const sleep = o.sleep ?? ((ms) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const until = now() + (o.maxMs ?? FIRST_FRAME_MAX_MS);
  try {
    while (recorder.captured) {
      const samples = await recorder.captured();
      if (samples === null) return null;
      if (samples > 0) return now();
      if (now() >= until) return null;
      await sleep(o.pollMs ?? FIRST_FRAME_POLL_MS);
      if (!live()) return null;
    }
  } catch {
    // An older shell: the timing goes without it.
  }
  return null;
}

/**
 * Measures the RMS level of a live stream with a Web Audio analyser. Created
 * lazily on the first read, so recordings nobody animates never open an
 * AudioContext; `null` where Web Audio is missing or refuses the stream.
 */
function streamMeter(stream: MediaStream) {
  let ctx: AudioContext | null = null;
  let analyser: AnalyserNode | null = null;
  let samples: Float32Array<ArrayBuffer> | null = null;
  let failed = false;
  let resumed = false;
  return {
    read(): number | null {
      if (failed) return null;
      if (!analyser) {
        try {
          if (typeof AudioContext === 'undefined') throw new Error('no Web Audio');
          ctx = new AudioContext();
          analyser = ctx.createAnalyser();
          analyser.fftSize = 512;
          ctx.createMediaStreamSource(stream).connect(analyser);
          samples = new Float32Array(analyser.fftSize);
        } catch {
          failed = true;
          void ctx?.close().catch(() => {});
          ctx = null;
          analyser = null;
          return null;
        }
      }
      // A context started outside a user gesture may stay suspended (it then
      // reads silence): report no level so the caller keeps its fallback.
      if (ctx?.state !== 'running') {
        if (ctx?.state === 'suspended' && !resumed) {
          resumed = true;
          void ctx.resume().catch(() => {});
        }
        return null;
      }
      analyser.getFloatTimeDomainData(samples!);
      let sum = 0;
      for (const v of samples!) sum += v * v;
      return Math.sqrt(sum / samples!.length);
    },
    close() {
      void ctx?.close().catch(() => {});
      ctx = null;
      analyser = null;
    },
  };
}

/**
 * MediaRecorder-based recorder. The microphone is released on stop/cancel.
 * It has no `peek`/`split`, so dictation is transcribed once on stop.
 */
export function createMediaRecorder(): Recorder {
  let stream: MediaStream | null = null;
  let recorder: MediaRecorder | null = null;
  let chunks: Blob[] = [];
  let meter: ReturnType<typeof streamMeter> | null = null;

  const release = () => {
    meter?.close();
    meter = null;
    stream?.getTracks().forEach((t) => t.stop());
    stream = null;
    recorder = null;
  };

  return {
    async start() {
      if (typeof navigator === 'undefined' || !navigator.mediaDevices?.getUserMedia) {
        throw new Error('audio capture is not supported here');
      }
      if (typeof MediaRecorder === 'undefined') throw new Error('MediaRecorder is not supported');
      const noiseSuppression = captureOptions().noiseSuppression;
      try {
        stream = await navigator.mediaDevices.getUserMedia({
          audio: {
            echoCancellation: true,
            noiseSuppression,
            autoGainControl: true,
            channelCount: 1,
          },
        });
      } catch (err) {
        const name = (err as { name?: unknown } | null)?.name;
        diag('mic.web.error', {
          ...errorInfo(err),
          permission: name === 'NotAllowedError' || name === 'SecurityError',
        });
        throw err;
      }
      const track = stream.getAudioTracks?.()[0];
      diag('mic.web.start', {
        noiseSuppression,
        applied: track?.getSettings?.().noiseSuppression,
      });
      chunks = [];
      meter = streamMeter(stream);
      recorder = new MediaRecorder(stream);
      recorder.ondataavailable = (e) => {
        if (e.data.size > 0) chunks.push(e.data);
      };
      recorder.start();
    },
    stop() {
      return new Promise<Blob>((resolve) => {
        const rec = recorder;
        if (!rec || rec.state === 'inactive') {
          release();
          resolve(new Blob(chunks));
          return;
        }
        rec.onstop = () => {
          const blob = new Blob(chunks, { type: rec.mimeType });
          release();
          resolve(blob);
        };
        rec.stop();
      });
    },
    cancel() {
      if (recorder && recorder.state !== 'inactive') recorder.stop();
      release();
      chunks = [];
    },
    level() {
      return meter?.read() ?? null;
    },
  };
}

/** Longest a native start/stop/cancel may hold up the next one (a hung call). */
export const MIC_OP_MAX_MS = 8000;

let micOps: Promise<unknown> = Promise.resolve();

/**
 * Runs start/stop/cancel of the one native microphone strictly in call order.
 * Native commands may otherwise be handled out of order: a fire-and-forget
 * `cancel` of an abandoned dictation that lands after the Voco listener's
 * `start` stopped the listener's fresh capture (the wake word then went
 * unanswered). A call that never answers holds the queue `MIC_OP_MAX_MS` at most.
 */
export function serialMicOp<T>(op: () => Promise<T>): Promise<T> {
  const prev = micOps;
  const run = prev.then(op, op);
  const settled = run.then(
    () => undefined,
    () => undefined,
  );
  micOps = Promise.race([
    settled,
    new Promise<void>((resolve) => setTimeout(resolve, MIC_OP_MAX_MS)),
  ]);
  return run;
}

/**
 * Recorder backed by the native shell's microphone (`vca_mic_*`). The native
 * side returns 16-bit PCM mono WAV bytes.
 */
export function createNativeRecorder(): Recorder {
  let stopPoll: (() => void) | null = null;

  /** Debug log: native capture counters while recording (no audio). */
  const poll = () => {
    if (!diagEnabled()) return () => {};
    let last = { samples: 0, at: Date.now() };
    let first = true;
    const snapshot = async () => {
      try {
        const raw = JSON.parse(await bridge.micDiag()) as {
          capture: {
            callbacks: number;
            samples: number;
            peakPpm: number;
            streamErrors: number;
            lastError: string;
            config: string;
            bufferedSamples: number | null;
          } | null;
          note?: string;
          session?: string;
        };
        const now = Date.now();
        const c = raw.capture;
        if (first) {
          first = false;
          diag('mic.native.start', { note: raw.note, session: raw.session });
        }
        diag('mic.native.stats', {
          config: c?.config,
          callbacks: c?.callbacks,
          samples: c?.samples,
          samplesPerSec: c ? Math.round(((c.samples - last.samples) * 1000) / (now - last.at)) : 0,
          peakPpm: c?.peakPpm,
          streamErrors: c?.streamErrors,
          lastError: c?.lastError,
          buffered: c?.bufferedSamples,
          session: raw.session,
        });
        if (c) last = { samples: c.samples, at: now };
      } catch (err) {
        diag('mic.native.stats.error', errorInfo(err));
      }
    };
    const first1 = setTimeout(() => void snapshot(), 700);
    const timer = setInterval(() => void snapshot(), 2_500);
    return () => {
      clearTimeout(first1);
      clearInterval(timer);
      void snapshot();
    };
  };
  const endPoll = () => {
    stopPoll?.();
    stopPoll = null;
  };

  /**
   * Every build: one snapshot of the native capture shortly after start and
   * one after stop (counters and the device kind, never audio), so the log
   * shows a stream that produced no audio.
   */
  let startSnapshot: ReturnType<typeof setTimeout> | null = null;
  const nativeSnapshot = async (event: 'mic.native.start' | 'mic.native.stop') => {
    try {
      const raw = JSON.parse(await bridge.micDiag()) as {
        capture: Record<string, string | number | boolean | null> | null;
        note?: string;
        session?: string;
      };
      const c = raw.capture ?? {};
      const str = (v: unknown) => (typeof v === 'string' && v !== '' ? v : undefined);
      const num = (v: unknown) => (typeof v === 'number' ? v : undefined);
      diag(event, {
        device: str(c.device),
        config: str(c.config),
        noiseSuppression: typeof c.noiseSuppression === 'boolean' ? c.noiseSuppression : undefined,
        callbacks: num(c.callbacks),
        samples: num(c.samples),
        peakPpm: num(c.peakPpm),
        outPeakPpm: num(c.outPeakPpm),
        suppressMs: num(c.suppressMs),
        streamErrors: num(c.streamErrors),
        lastError: str(c.lastError),
        ageMs: num(c.ageMs),
        ...(event === 'mic.native.start' ? { note: raw.note, session: raw.session } : {}),
      });
    } catch (err) {
      diag(event === 'mic.native.start' ? 'mic.native.start.error' : 'mic.native.stop.error', {
        ...errorInfo(err),
      });
    }
  };
  const clearStartSnapshot = () => {
    if (startSnapshot) clearTimeout(startSnapshot);
    startSnapshot = null;
  };

  return {
    async start() {
      const t0 = Date.now();
      const { noiseSuppression } = captureOptions();
      diag('mic.start.request', { noiseSuppression });
      try {
        await serialMicOp(() => bridge.micStart({ noiseSuppression }));
      } catch (err) {
        const info = errorInfo(err);
        diag('mic.start.error', {
          ms: Date.now() - t0,
          ...info,
          permission: info.errorCode === 'mic_permission',
        });
        throw err;
      }
      diag('mic.start.ok', { ms: Date.now() - t0 });
      stopPoll = poll();
      clearStartSnapshot();
      startSnapshot = setTimeout(() => {
        startSnapshot = null;
        void nativeSnapshot('mic.native.start');
      }, 1500);
    },
    async stop() {
      endPoll();
      clearStartSnapshot();
      const t0 = Date.now();
      try {
        const bytes = await serialMicOp(() => bridge.micStop());
        diag('mic.stop.ok', { ms: Date.now() - t0, bytes: bytes.byteLength });
        void nativeSnapshot('mic.native.stop');
        return new Blob([bytes], { type: 'audio/wav' });
      } catch (err) {
        diag('mic.stop.error', { ms: Date.now() - t0, ...errorInfo(err) });
        throw err;
      }
    },
    async peek() {
      const bytes = await bridge.micPeek();
      diag('mic.peek', { bytes: bytes.byteLength });
      return new Blob([bytes], { type: 'audio/wav' });
    },
    async split(minSeconds: number) {
      const bytes = await bridge.micSplit(minSeconds);
      diag('mic.split', { bytes: bytes.byteLength });
      return new Blob([bytes], { type: 'audio/wav' });
    },
    async captured() {
      const raw = JSON.parse(await bridge.micDiag()) as { capture: { samples: number } | null };
      return raw.capture?.samples ?? null;
    },
    cancel() {
      endPoll();
      clearStartSnapshot();
      diag('mic.cancel');
      serialMicOp(() => bridge.micCancel())
        .then(() => nativeSnapshot('mic.native.stop'))
        .catch(() => {});
    },
  };
}
