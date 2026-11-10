import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  captureAnswer,
  frameLevels,
  speakAndWait,
  voiceActivity,
  type CaptureOptions,
} from './handsFreeIO';
import type { Recorder } from './recorder';

const FRAME = 320; // samples per 20 ms at 16 kHz

/** 16 kHz mono WAV: `frames` 20 ms frames, loud (speech) where `loud(i)` is true. */
function wav(frames: number, loud: (i: number) => boolean = () => false): Blob {
  const bytes = new Uint8Array(44 + frames * FRAME * 2);
  const view = new DataView(bytes.buffer);
  for (let f = 0; f < frames; f += 1) {
    const amp = loud(f) ? 8000 : 10;
    for (let s = 0; s < FRAME; s += 1) {
      view.setInt16(44 + (f * FRAME + s) * 2, s % 2 ? amp : -amp, true);
    }
  }
  return new Blob([bytes], { type: 'audio/wav' });
}

/** A virtual clock: `sleep` advances it. */
function clock() {
  let t = 0;
  return {
    now: () => t,
    sleep: async (ms: number) => {
      t += ms;
    },
  };
}

function recorder(
  over: Partial<Recorder> = {},
): Recorder & Record<string, ReturnType<typeof vi.fn>> {
  return {
    start: vi.fn(async () => {}),
    stop: vi.fn(async () => wav(60, (i) => i < 30)),
    cancel: vi.fn(),
    ...over,
  } as Recorder & Record<string, ReturnType<typeof vi.fn>>;
}

function capture(rec: Recorder, extra: Partial<CaptureOptions> = {}) {
  const c = clock();
  const transcribe = vi.fn(async () => ({ text: '네', corrections: [] }));
  const done = captureAnswer({
    recorder: rec,
    transcribe,
    signal: new AbortController().signal,
    sleep: c.sleep,
    now: c.now,
    ...extra,
  });
  return { done, transcribe, clock: c };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('voice activity', () => {
  it('measures frames and the trailing silence', async () => {
    const bytes = new Uint8Array(await wav(10, (i) => i === 3).arrayBuffer());
    const levels = frameLevels(bytes);
    expect(levels).toHaveLength(10);
    expect(voiceActivity(levels)).toEqual({ speech: true, trailingSilenceMs: 120 });
    expect(voiceActivity(frameLevels(new Uint8Array(44))).speech).toBe(false);
  });
});

describe('captureAnswer', () => {
  it('ends after speech followed by silence and transcribes once', async () => {
    let peeks = 0;
    const rec = recorder({
      // Speech in the first 10 frames, then silence that grows with each peek.
      peek: vi.fn(async () => {
        peeks += 1;
        return wav(10 + peeks * 10, (i) => i < 10);
      }),
    });
    const { done, transcribe, clock: c } = capture(rec);
    expect(await done).toBe('네');
    expect(transcribe).toHaveBeenCalledTimes(1);
    expect(rec.stop).toHaveBeenCalled();
    // 1000 ms of silence = 50 frames: the fifth peek.
    expect(peeks).toBe(5);
    expect(c.now()).toBeLessThan(5000);
  });

  it('times out without an STT request when nobody speaks', async () => {
    const rec = recorder({ peek: vi.fn(async () => wav(20)) });
    const { done, transcribe } = capture(rec);
    expect(await done).toBeNull();
    expect(transcribe).not.toHaveBeenCalled();
    expect(rec.cancel).toHaveBeenCalled();
    expect(rec.stop).not.toHaveBeenCalled();
  });

  it('records a fixed window with recorders that cannot peek', async () => {
    const rec = recorder();
    const { done, transcribe, clock: c } = capture(rec);
    expect(await done).toBe('네');
    expect(c.now()).toBe(3000);
    expect(transcribe).toHaveBeenCalledTimes(1);
  });

  it('prefers the raw transcript (filler removal may drop a lone answer)', async () => {
    const rec = recorder();
    const { done } = capture(rec, {
      transcribe: async () => ({ text: '', rawText: '응', corrections: [] }),
    });
    expect(await done).toBe('응');
  });

  it('gives up on a microphone or STT failure', async () => {
    const failing = recorder({ start: vi.fn(async () => Promise.reject(new Error('denied'))) });
    expect(await capture(failing).done).toBeNull();
    const sttDown = capture(recorder(), {
      transcribe: async () => Promise.reject(new Error('down')),
    });
    expect(await sttDown.done).toBeNull();
  });

  it('releases the microphone when interrupted', async () => {
    const controller = new AbortController();
    const rec = recorder({ peek: vi.fn(async () => wav(5)) });
    const c = clock();
    const done = captureAnswer({
      recorder: rec,
      transcribe: vi.fn(),
      signal: controller.signal,
      now: c.now,
      sleep: async (ms) => {
        await c.sleep(ms);
        controller.abort();
      },
    });
    expect(await done).toBeNull();
    expect(rec.cancel).toHaveBeenCalled();
  });

  it('does not depend on the audio route (Bluetooth headsets are a no-op)', async () => {
    const enumerateDevices = vi.fn(async () => []);
    const mediaDevices = { enumerateDevices, addEventListener: vi.fn() };
    vi.stubGlobal('navigator', { ...navigator, mediaDevices });
    const rec = recorder();
    const { done } = capture(rec);
    // A headset connecting mid-capture changes nothing.
    window.dispatchEvent(new Event('devicechange'));
    expect(await done).toBe('네');
    expect(enumerateDevices).not.toHaveBeenCalled();
    expect(mediaDevices.addEventListener).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });
});

describe('speakAndWait', () => {
  it('waits until the voice has finished', async () => {
    const c = clock();
    let busyFor = 3;
    const speak = vi.fn(async () => 'device');
    await speakAndWait('보낼까요?', new AbortController().signal, {
      speak,
      busy: () => busyFor-- > 0,
      stop: vi.fn(),
      sleep: c.sleep,
      now: c.now,
    });
    expect(speak).toHaveBeenCalledWith('보낼까요?');
    expect(busyFor).toBe(-1);
  });

  it('stops the voice when interrupted', async () => {
    const controller = new AbortController();
    const stop = vi.fn();
    const c = clock();
    await speakAndWait('x', controller.signal, {
      speak: async () => controller.abort(),
      busy: () => true,
      stop,
      sleep: c.sleep,
      now: c.now,
    });
    expect(stop).toHaveBeenCalled();
  });

  it('never waits forever on a stuck player', async () => {
    const c = clock();
    await speakAndWait('x', new AbortController().signal, {
      speak: async () => {},
      busy: () => true,
      stop: vi.fn(),
      maxMs: 2000,
      sleep: c.sleep,
      now: c.now,
    });
    expect(c.now()).toBeGreaterThanOrEqual(2000);
    expect(c.now()).toBeLessThan(3000);
  });
});
