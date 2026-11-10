import { describe, expect, it, vi } from 'vitest';

import { diag } from '../diag/log';
import {
  CHUNK_SECONDS,
  CONTEXT_CHARS,
  Dictation,
  MAX_MIC_RESTARTS,
  MIC_STALL_MS,
  MIN_REQUEST_GAP_MS,
  PREVIEW_BACKOFF,
  PREVIEW_SLOW_MS,
  applyFinal,
  applyInterim,
  composeDictation,
  contextOf,
  cutChunk,
  formatElapsed,
  initialDictation,
  isShortWav,
  isTooShort,
  joinText,
  previewGapMs,
  retryFailedChunks,
  wavDurationMs,
  type DictationResult,
  type DictationState,
} from './dictation';
import type { Recorder } from './recorder';
import type { Transcript } from './transcribe';
import { wavOf } from './vocoListener';

vi.mock('../diag/log', async (original) => ({
  ...(await original<typeof import('../diag/log')>()),
  diag: vi.fn(),
}));

const tr = (text: string): Transcript => ({ text, corrections: [] });

/** A WAV blob of roughly `seconds` of 16 kHz mono 16-bit audio, tagged for debugging. */
const wav = (seconds: number, tag = '') => {
  const blob = new Blob([new Uint8Array(44 + Math.round(32_000 * seconds))], {
    type: 'audio/wav',
  });
  Object.defineProperty(blob, 'tag', { value: tag });
  return blob;
};
const tagOf = (b: Blob) => (b as Blob & { tag?: string }).tag;
const EMPTY = new Blob([], { type: 'audio/wav' });

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const flush = async () => {
  for (let i = 0; i < 10; i++) await Promise.resolve();
};

function streamingRecorder(overrides: Partial<Recorder> = {}) {
  return {
    start: vi.fn(async () => {}),
    stop: vi.fn(async () => wav(1, 'rest')),
    cancel: vi.fn(),
    peek: vi.fn(async () => wav(2, 'peek')),
    split: vi.fn(async (_min: number) => EMPTY),
    ...overrides,
  };
}

describe('dictation helpers', () => {
  it('detects too-short recordings by time and WAV size', () => {
    expect(isTooShort(299)).toBe(true);
    expect(isTooShort(300)).toBe(false);
    expect(isShortWav(wav(0.2))).toBe(true);
    expect(isShortWav(wav(0.31))).toBe(false);
    expect(isShortWav(new Blob(['x'], { type: 'audio/webm' }))).toBe(false);
    expect(isTooShort(5000, wav(0.1))).toBe(true);
    expect(isTooShort(5000, new Blob())).toBe(false);
  });

  it('formats elapsed time as mm:ss', () => {
    expect(formatElapsed(0)).toBe('00:00');
    expect(formatElapsed(65_400)).toBe('01:05');
    expect(formatElapsed(-5)).toBe('00:00');
  });

  it('joins text with a single space', () => {
    expect(joinText('', 'a')).toBe('a');
    expect(joinText('a', '')).toBe('a');
    expect(joinText('a', 'b')).toBe('a b');
    expect(joinText('a\n', 'b')).toBe('a\nb');
  });

  it('limits context to the last characters of the committed text', () => {
    expect(contextOf('   ')).toBeNull();
    const long = 'x'.repeat(CONTEXT_CHARS) + 'END';
    expect(contextOf(long)).toHaveLength(CONTEXT_CHARS);
    expect(contextOf(long)!.endsWith('END')).toBe(true);
  });

  it('keeps the base, orders chunks by sequence and dims only the tail', () => {
    let s: DictationState = initialDictation('Fix');
    s = applyInterim(s, 0, 'the tests');
    expect(composeDictation(s)).toMatchObject({ text: 'Fix the tests', tailLength: 10 });
    const a = cutChunk(s);
    s = a.state;
    // The interim stands in for the chunk until its final arrives.
    expect(composeDictation(s)).toMatchObject({ text: 'Fix the tests', tailLength: 0 });
    // Stale interim (from before the cut) is ignored.
    expect(applyInterim(s, 0, 'stale')).toBe(s);
    s = applyInterim(s, 1, 'and');
    const b = cutChunk(s);
    s = b.state;
    // Chunk b completes before chunk a: still placed after it.
    s = applyFinal(s, b.seq, tr('and commit'));
    s = applyFinal(s, a.seq, {
      text: 'the tests ',
      rawText: 'uh the tests',
      corrections: [{ from: 'x', to: 'y', layer: 'base' }],
    });
    expect(composeDictation(s)).toEqual({
      text: 'Fix the tests and commit',
      tailLength: 0,
      corrections: [{ from: 'x', to: 'y', layer: 'base' }],
      // This run only, without the base; raw falls back to text when absent.
      dictated: 'the tests and commit',
      raw: 'uh the tests and commit',
    });
    expect(applyFinal(s, 99, tr('nope'))).toBe(s);
  });
});

describe('preview scheduling', () => {
  it('backs off by the audio a preview re-sends', () => {
    expect(previewGapMs(1_000, 100)).toBe(MIN_REQUEST_GAP_MS);
    expect(previewGapMs(5_000, 100)).toBe(1_500);
    expect(previewGapMs(12_000, 100)).toBe(3_000);
    expect(previewGapMs(null, 100)).toBe(MIN_REQUEST_GAP_MS);
  });

  it('leaves a slow server room and stops previews when it is too slow', () => {
    expect(previewGapMs(1_000, 900)).toBe(1_800);
    expect(previewGapMs(12_000, 900)).toBe(3_000);
    expect(previewGapMs(1_000, PREVIEW_SLOW_MS - 1)).toBe((PREVIEW_SLOW_MS - 1) * 2);
    expect(previewGapMs(1_000, PREVIEW_SLOW_MS)).toBeNull();
  });

  it('never goes below the configured minimum gap', () => {
    expect(previewGapMs(1_000, 0, 2_000)).toBe(2_000);
  });

  it('measures native WAV durations', () => {
    expect(wavDurationMs(wav(2))).toBe(2_000);
    expect(wavDurationMs(EMPTY)).toBe(0);
    expect(wavDurationMs(new Blob(['x'], { type: 'audio/webm' }))).toBeNull();
  });
});

describe('Dictation', () => {
  it('shows interim text, commits split chunks as finals and passes context and final', async () => {
    const split = vi.fn(async (_min: number) => EMPTY);
    const recorder = streamingRecorder({ split });
    const transcribe = vi.fn(async (audio: Blob, _ctx: string | null, _final: boolean) =>
      tr(`<${tagOf(audio)}>`),
    );
    const updates: string[] = [];
    const d = new Dictation({
      recorder,
      transcribe,
      base: 'Hello',
      onUpdate: (s) => updates.push(composeDictation(s).text),
    });

    await d.step();
    expect(recorder.split).toHaveBeenCalledWith(CHUNK_SECONDS);
    expect(d.current.tail).toBe('<peek>');
    // Interim previews are never final.
    expect(transcribe).toHaveBeenLastCalledWith(expect.anything(), 'Hello', false);

    split.mockResolvedValueOnce(wav(15, 'c0'));
    await d.step();
    expect(composeDictation(d.current).text).toBe('Hello <c0>');
    // Final chunk context: text before the chunk.
    expect(transcribe).toHaveBeenLastCalledWith(
      expect.objectContaining({ tag: 'c0' }),
      'Hello',
      true,
    );

    await d.step(); // interim again, with the committed chunk as context
    expect(transcribe).toHaveBeenLastCalledWith(expect.anything(), 'Hello <c0>', false);
    expect(composeDictation(d.current)).toMatchObject({ text: 'Hello <c0> <peek>' });

    const result = await d.finish();
    expect(recorder.stop).toHaveBeenCalled();
    expect(result.tooShort).toBe(false);
    expect(composeDictation(result.state)).toEqual({
      text: 'Hello <c0> <rest>',
      tailLength: 0,
      corrections: [],
      dictated: '<c0> <rest>',
      raw: '<c0> <rest>',
    });
    expect(transcribe).toHaveBeenLastCalledWith(
      expect.objectContaining({ tag: 'rest' }),
      'Hello <c0>',
      true,
    );
    // Exactly the cut chunk and the remainder were final.
    const finals = transcribe.mock.calls.filter((c) => c[2]).map((c) => tagOf(c[0]));
    expect(finals).toEqual(['c0', 'rest']);
    const interims = transcribe.mock.calls.filter((c) => !c[2]).map((c) => tagOf(c[0]));
    expect(interims.every((tag) => tag === 'peek')).toBe(true);
    expect(updates.at(-1)).toBe('Hello <c0> <rest>');
  });

  it('never overlaps live requests and orders out-of-order finals', async () => {
    const chunk = deferred<Transcript>();
    const recorder = streamingRecorder({ split: vi.fn(async () => wav(15, 'c0')) });
    const transcribe = vi.fn((audio: Blob) =>
      tagOf(audio) === 'c0' ? chunk.promise : Promise.resolve(tr('rest text')),
    );
    const d = new Dictation({ recorder, transcribe, base: '', onUpdate: () => {} });

    const first = d.step();
    await flush();
    expect(transcribe).toHaveBeenCalledTimes(1);
    await d.step(); // busy: skipped
    await d.step();
    expect(recorder.split).toHaveBeenCalledTimes(1);
    expect(recorder.peek).not.toHaveBeenCalled();

    // Stop while chunk 0 is still being transcribed; the remainder finishes first.
    const finishing = d.finish();
    await flush();
    expect(transcribe).toHaveBeenCalledTimes(2);
    expect(composeDictation(d.current).text).toBe('rest text');
    chunk.resolve(tr('chunk zero'));
    await first;
    const result = await finishing;
    expect(composeDictation(result.state).text).toBe('chunk zero rest text');
  });

  it('waits for an in-flight split before stopping so chunks keep recording order', async () => {
    const split = deferred<Blob>();
    const recorder = streamingRecorder({ split: vi.fn(() => split.promise) });
    const transcribe = vi.fn(async (audio: Blob) => tr(tagOf(audio)!));
    const d = new Dictation({ recorder, transcribe, base: '', onUpdate: () => {} });
    const stepping = d.step();
    const finishing = d.finish();
    await flush();
    expect(recorder.stop).not.toHaveBeenCalled();
    split.resolve(wav(15, 'early'));
    await stepping;
    const result = await finishing;
    expect(composeDictation(result.state).text).toBe('early rest');
  });

  it('drops interim results that arrive after a cut or after stop', async () => {
    const interim = deferred<Transcript>();
    const recorder = streamingRecorder();
    const transcribe = vi.fn((audio: Blob) =>
      tagOf(audio) === 'peek' ? interim.promise : Promise.resolve(tr('done')),
    );
    const d = new Dictation({ recorder, transcribe, base: 'x', onUpdate: () => {} });
    const stepping = d.step();
    await flush();
    const finishing = d.finish();
    interim.resolve(tr('late'));
    await stepping;
    const result = await finishing;
    expect(composeDictation(result.state).text).toBe('x done');
  });

  it('runs an adaptive loop with a minimum gap between request starts', async () => {
    let now = 0;
    const starts: number[] = [];
    const sleeps: number[] = [];
    const recorder = streamingRecorder({
      peek: vi.fn(async () => {
        starts.push(now);
        // The second request is slow (1.2 s), the others quick (0.3 s).
        now += starts.length === 2 ? 1200 : 300;
        return wav(2, 'peek');
      }),
    });
    let d: Dictation | null = null;
    const sleep = vi.fn(async (ms: number) => {
      sleeps.push(ms);
      now += ms;
      if (starts.length >= 4) void d!.finish();
    });
    d = new Dictation({
      recorder,
      transcribe: async () => tr('hi'),
      base: '',
      onUpdate: () => {},
      now: () => now,
      sleep,
    });
    await d.run();
    expect(starts).toEqual([0, MIN_REQUEST_GAP_MS, MIN_REQUEST_GAP_MS + 1200, 2600]);
    expect(sleeps.slice(0, 3)).toEqual([MIN_REQUEST_GAP_MS - 300, 0, MIN_REQUEST_GAP_MS - 300]);
  });

  it('falls back to one transcript on stop without peek/split', async () => {
    const recorder = {
      start: vi.fn(async () => {}),
      stop: vi.fn(async () => new Blob(['webm'], { type: 'audio/webm' })),
      cancel: vi.fn(),
    };
    const transcribe = vi.fn(async () => tr('whole thing'));
    const d = new Dictation({ recorder, transcribe, base: 'Note:', onUpdate: () => {} });
    expect(d.streaming).toBe(false);
    await d.run(); // returns immediately
    await d.step();
    expect(transcribe).not.toHaveBeenCalled();
    const result = await d.finish();
    expect(transcribe).toHaveBeenCalledTimes(1);
    expect(transcribe).toHaveBeenCalledWith(expect.any(Blob), 'Note:', true);
    expect(composeDictation(result.state).text).toBe('Note: whole thing');
  });

  it('stops using peek/split after they fail (e.g. an older native shell)', async () => {
    const recorder = streamingRecorder({
      peek: vi.fn(async () => {
        throw new Error('unknown command');
      }),
      split: vi.fn(async () => {
        throw new Error('unknown command');
      }),
    });
    const d = new Dictation({
      recorder,
      transcribe: async () => tr('x'),
      base: '',
      onUpdate: () => {},
    });
    await d.step();
    expect(d.streaming).toBe(false);
    await d.run();
    expect(recorder.peek).toHaveBeenCalledTimes(1);
  });

  it('reports a too-short recording without calling the server', async () => {
    const recorder = streamingRecorder({ stop: vi.fn(async () => wav(0.1)) });
    const transcribe = vi.fn(async () => tr('x'));
    const d = new Dictation({ recorder, transcribe, base: 'keep', onUpdate: () => {} });
    const result = await d.finish();
    expect(result.tooShort).toBe(true);
    expect(transcribe).not.toHaveBeenCalled();
    expect(composeDictation(result.state).text).toBe('keep');
  });

  it('keeps the interim stand-in and reports the error when a chunk fails', async () => {
    const recorder = streamingRecorder();
    const err = new Error('boom');
    const transcribe = vi
      .fn<(a: Blob, c: string | null) => Promise<Transcript>>()
      .mockResolvedValueOnce(tr('almost'))
      .mockRejectedValueOnce(err);
    const d = new Dictation({ recorder, transcribe, base: '', onUpdate: () => {} });
    await d.step();
    const result = await d.finish();
    expect(result.error).toBe(err);
    expect(composeDictation(result.state).text).toBe('almost');
  });

  it('keeps the audio of failed chunks so a retry needs no new speech', async () => {
    const split = vi.fn(async (_min: number) => EMPTY);
    split.mockResolvedValueOnce(wav(15, 'c0'));
    const recorder = streamingRecorder({ split });
    const lost = new Error('connection lost (transport_error)');
    const transcribe = vi.fn(async (audio: Blob, _ctx: string | null, _final: boolean) => {
      if (tagOf(audio) === 'c0') throw lost;
      return tr(`<${tagOf(audio)}>`);
    });
    const d = new Dictation({ recorder, transcribe, base: 'Hi', onUpdate: () => {} });
    await d.step(); // cuts c0, whose transcription fails
    const result = await d.finish();
    expect(result.error).toBe(lost);
    expect(result.failed.map((f) => [f.seq, tagOf(f.audio)])).toEqual([[0, 'c0']]);
    expect(composeDictation(result.state).text).toBe('Hi <rest>');

    // The retry sends the kept audio with the text before it as context.
    const retry = vi.fn(async (audio: Blob, _ctx: string | null, _final: boolean) =>
      tr(`[${tagOf(audio)}]`),
    );
    const again = await retryFailedChunks(result.state, result.failed, retry);
    expect(retry).toHaveBeenCalledWith(expect.objectContaining({ tag: 'c0' }), 'Hi', true);
    expect(again.failed).toEqual([]);
    expect(again.error).toBeNull();
    expect(composeDictation(again.state).text).toBe('Hi [c0] <rest>');

    // A retry that fails again keeps the chunk for the next try.
    const still = await retryFailedChunks(result.state, result.failed, async () => {
      throw lost;
    });
    expect(still.failed.map((f) => tagOf(f.audio))).toEqual(['c0']);
    expect(still.error).toBe(lost);
  });

  it('backs previews off while they fail, and resumes after one succeeds', async () => {
    let now = 0;
    const previews: number[] = [];
    let failing = true;
    const recorder = streamingRecorder();
    const transcribe = vi.fn(async (_a: Blob, _c: string | null, final: boolean) => {
      if (!final) {
        previews.push(now);
        if (failing) throw new Error('not connected');
      }
      return tr('ok');
    });
    const d = new Dictation({ recorder, transcribe, base: '', onUpdate: () => {}, now: () => now });
    // One loop round every 100 ms for 30 s.
    for (; now < 30_000; now += 100) {
      if (now === 21_000) failing = false;
      await d.step(now >= (d as unknown as { nextPreviewAt: number }).nextPreviewAt);
    }
    // 0, then +3 s, +6 s, +12 s: four tries in 21 s instead of thirty.
    expect(previews.slice(0, 4)).toEqual([0, 3000, 9000, 21_000]);
    // After the success the normal cadence is back.
    expect(previews[4]).toBe(21_000 + MIN_REQUEST_GAP_MS);
    await d.finish();
  });

  it('abandon releases the microphone and ignores late results', async () => {
    const interim = deferred<Transcript>();
    const recorder = streamingRecorder();
    const onUpdate = vi.fn();
    const d = new Dictation({ recorder, transcribe: () => interim.promise, base: '', onUpdate });
    const stepping = d.step();
    await flush();
    d.abandon();
    d.abandon();
    expect(recorder.cancel).toHaveBeenCalledTimes(1);
    interim.resolve(tr('late'));
    await stepping;
    expect(onUpdate).not.toHaveBeenCalled();
  });
});

describe('Dictation stall safety net', () => {
  const header = () => new Blob([new Uint8Array(44)], { type: 'audio/wav' });

  it('reports a recorder that delivers no audio, and only once', async () => {
    vi.useFakeTimers();
    try {
      const onStall = vi.fn();
      const recorder = streamingRecorder({ peek: vi.fn(async () => header()) });
      const d = new Dictation({
        recorder,
        transcribe: async () => tr('x'),
        base: '',
        onUpdate: () => {},
        onStall,
        noAudioMs: 8000,
        sleep: () => new Promise(() => {}),
      });
      void d.run();
      await vi.advanceTimersByTimeAsync(7999);
      expect(onStall).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(2);
      expect(onStall).toHaveBeenCalledTimes(1);
      d.abandon();
    } finally {
      vi.useRealTimers();
    }
  });

  it('does not stall once audio has been captured', async () => {
    vi.useFakeTimers();
    try {
      const onStall = vi.fn();
      const d = new Dictation({
        recorder: streamingRecorder(),
        transcribe: async () => tr('x'),
        base: '',
        onUpdate: () => {},
        onStall,
        sleep: () => new Promise(() => {}),
      });
      void d.run();
      await vi.advanceTimersByTimeAsync(20_000);
      expect(onStall).not.toHaveBeenCalled();
      d.abandon();
    } finally {
      vi.useRealTimers();
    }
  });

  it('stays quiet after finish or abandon', async () => {
    vi.useFakeTimers();
    try {
      const onStall = vi.fn();
      const recorder = streamingRecorder({ peek: vi.fn(async () => header()) });
      const d = new Dictation({
        recorder,
        transcribe: async () => tr('x'),
        base: '',
        onUpdate: () => {},
        onStall,
        sleep: () => new Promise(() => {}),
      });
      void d.run();
      await vi.advanceTimersByTimeAsync(1000);
      await d.finish();
      await vi.advanceTimersByTimeAsync(20_000);
      expect(onStall).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it('backs previews off as the tail grows, keeps cut checks fast and finalizes the whole recording', async () => {
    let now = 0;
    const previews: { at: number; audioMs: number }[] = [];
    let splits = 0;
    const recorder = streamingRecorder({
      // The tail since start grows with the clock (no cut: no quiet point yet).
      peek: vi.fn(async () => {
        previews.push({ at: now, audioMs: now });
        return wav(now / 1000, 'peek');
      }),
      split: vi.fn(async (_min: number) => {
        splits += 1;
        return EMPTY;
      }),
      stop: vi.fn(async () => wav(15, 'all')),
    });
    const transcribe = vi.fn(async (audio: Blob, _ctx: string | null, final: boolean) => {
      if (!final) now += 200; // a quick server
      return tr(final ? `<${tagOf(audio)}>` : 'interim');
    });
    let d: Dictation | null = null;
    const fin: { done?: Promise<DictationResult> } = {};
    d = new Dictation({
      recorder,
      transcribe,
      base: '',
      onUpdate: () => {},
      now: () => now,
      sleep: async (ms) => {
        now += ms;
        if (now >= 15_000) fin.done ??= d!.finish();
      },
    });
    await d.run();
    const result = (await fin.done)!;

    // Every round still checks for a cut; previews are far fewer.
    expect(splits).toBeGreaterThanOrEqual(20);
    expect(previews.length).toBeLessThanOrEqual(12);
    expect(previews.length).toBeLessThan(splits * 0.6);
    for (let i = 1; i < previews.length; i++) {
      const prev = previews[i - 1]!;
      const gap = previews[i]!.at - prev.at;
      expect(gap).toBeGreaterThanOrEqual(previewGapMs(prev.audioMs, 200)!);
    }
    // Short tails stay snappy; long ones wait the longest step.
    expect(previews[1]!.at - previews[0]!.at).toBe(MIN_REQUEST_GAP_MS);
    const last = previews.slice(-2);
    expect(last[1]!.at - last[0]!.at).toBeGreaterThanOrEqual(PREVIEW_BACKOFF.at(-1)!.gapMs);
    // The final transcript still covers the whole recording.
    expect(transcribe).toHaveBeenLastCalledWith(
      expect.objectContaining({ tag: 'all' }),
      null,
      true,
    );
    expect(composeDictation(result.state).text).toBe('<all>');
  });

  it('pauses previews while the server is slow and resumes after a cut', async () => {
    let now = 0;
    const previews: number[] = [];
    let cutAt: number | null = null;
    const recorder = streamingRecorder({
      peek: vi.fn(async () => {
        previews.push(now);
        return wav(2, 'peek');
      }),
      split: vi.fn(async (_min: number) => {
        if (cutAt === null && now >= 6_000) {
          cutAt = now;
          return wav(6, 'c0');
        }
        return EMPTY;
      }),
    });
    let slow = true;
    const transcribe = vi.fn(async (_audio: Blob, _ctx: string | null, final: boolean) => {
      if (!final && slow) {
        now += PREVIEW_SLOW_MS;
        slow = false;
      } else if (!final) now += 100;
      return tr('x');
    });
    let d: Dictation | null = null;
    const fin: { done?: Promise<DictationResult> } = {};
    d = new Dictation({
      recorder,
      transcribe,
      base: '',
      onUpdate: () => {},
      now: () => now,
      sleep: async (ms) => {
        now += ms;
        if (now >= 9_000) fin.done ??= d!.finish();
      },
    });
    await d.run();
    await fin.done;
    // One slow preview, nothing until the cut, then previews again.
    expect(previews[0]).toBe(0);
    expect(cutAt).not.toBeNull();
    expect(previews.filter((t) => t > 0 && t < cutAt!)).toEqual([]);
    expect(previews.filter((t) => t >= cutAt!).length).toBeGreaterThan(0);
  });

  it('a direct step still previews (the schedule only gates the loop)', async () => {
    const recorder = streamingRecorder();
    const d = new Dictation({
      recorder,
      transcribe: async () => tr('a'),
      base: '',
      onUpdate: () => {},
    });
    await d.step();
    await d.step();
    expect(recorder.peek).toHaveBeenCalledTimes(2);
    await d.step(false);
    expect(recorder.peek).toHaveBeenCalledTimes(2);
    expect(recorder.split).toHaveBeenCalledTimes(3);
  });

  it('finish does not wait forever for a native stop that never answers', async () => {
    vi.useFakeTimers();
    try {
      const recorder = streamingRecorder({ stop: vi.fn(() => new Promise<Blob>(() => {})) });
      const d = new Dictation({
        recorder,
        transcribe: async () => tr('x'),
        base: 'keep',
        onUpdate: () => {},
        nativeTimeoutMs: 1000,
      });
      const finishing = d.finish();
      await vi.advanceTimersByTimeAsync(1500);
      const result = await finishing;
      expect((result.error as Error).message).toMatch(/timed out/);
      expect(recorder.cancel).toHaveBeenCalled();
      expect(composeDictation(result.state).text).toBe('keep');
    } finally {
      vi.useRealTimers();
    }
  });
});

/**
 * A fake of the iPhone case from the field log: the first capture delivers
 * audio until `freezeAt`, then its counter stops without an error (a real
 * input route is not involved, this only models the counters). Every later
 * capture grows again unless `dead` says otherwise.
 */
function freezingRecorder(
  clock: { t: number },
  { freezeAt = 3850, dead = (_capture: number): boolean => false } = {},
) {
  let capture = 0;
  let startedAt = 0;
  const samples = () => {
    if (dead(capture)) return 0;
    const until = capture === 0 ? Math.min(clock.t, freezeAt) : clock.t;
    return Math.floor((until - startedAt) * 48);
  };
  const audio = () => wav(samples() / 48_000, `capture${capture}`);
  return streamingRecorder({
    captured: vi.fn(async () => samples()),
    peek: vi.fn(async () => audio()),
    stop: vi.fn(async () => audio()),
    start: vi.fn(async () => {
      capture += 1;
      startedAt = clock.t;
    }),
  });
}

function stalledRun(recorder: Recorder, clock: { t: number }, stopAt: number, stallMs?: number) {
  const onMicStalled = vi.fn();
  const transcribe = vi.fn(async (audio: Blob, _ctx: string | null, final: boolean) =>
    tr(final ? `<${tagOf(audio)}>` : `~${tagOf(audio)}`),
  );
  const fin: { done?: Promise<DictationResult> } = {};
  const d: Dictation = new Dictation({
    recorder,
    transcribe,
    base: '',
    onUpdate: () => {},
    onMicStalled,
    stallMs,
    now: () => clock.t,
    sleep: async (ms) => {
      clock.t += ms;
      if (clock.t >= stopAt) fin.done ??= d.finish();
    },
  });
  return { d, fin, onMicStalled, transcribe };
}

describe('a capture that stops growing', () => {
  it('keeps the audio before the gap, restarts and transcribes both sides', async () => {
    vi.mocked(diag).mockClear();
    const clock = { t: 0 };
    const recorder = freezingRecorder(clock);
    const { d, fin, onMicStalled } = stalledRun(recorder, clock, 9000);
    await d.run();
    const result = (await fin.done)!;

    expect(composeDictation(result.state).text).toBe('<capture0> <capture1>');
    expect(result.error).toBeNull();
    expect(recorder.start).toHaveBeenCalledTimes(1);
    expect(recorder.stop).toHaveBeenCalledTimes(2);
    expect(onMicStalled).toHaveBeenCalledTimes(1);
    expect(diag).toHaveBeenCalledWith(
      'dictation.mic_stalled',
      expect.objectContaining({ restarts: 0 }),
    );
  });

  it('restarts about a second after the freeze', async () => {
    const clock = { t: 0 };
    let restartedAt = 0;
    const recorder = freezingRecorder(clock);
    const start = recorder.start;
    recorder.start = vi.fn(async () => {
      restartedAt = clock.t;
      await start();
    });
    const { d, fin } = stalledRun(recorder, clock, 9000);
    await d.run();
    await fin.done;
    expect(restartedAt).toBeGreaterThanOrEqual(3850 + MIC_STALL_MS);
    expect(restartedAt).toBeLessThan(3850 + MIC_STALL_MS + 2 * MIN_REQUEST_GAP_MS);
  });

  it('does not send the same audio again as previews while frozen', async () => {
    const clock = { t: 0 };
    const recorder = freezingRecorder(clock);
    // Too long a threshold to restart: the capture stays frozen.
    const { d, fin, transcribe } = stalledRun(recorder, clock, 20_000, 1e9);
    await d.run();
    await fin.done;
    const previews = transcribe.mock.calls.filter(([, , final]) => !final).map(([a]) => a.size);
    expect(previews.length).toBeGreaterThan(2);
    expect(new Set(previews).size).toBe(previews.length);
  });

  it('gives up after a few restarts that bring no audio, keeping what it has', async () => {
    const clock = { t: 0 };
    // Only the first capture ever has audio.
    const recorder = freezingRecorder(clock, { dead: (c) => c > 0 });
    const { d, fin, transcribe, onMicStalled } = stalledRun(recorder, clock, 30_000);
    await d.run();
    const result = (await fin.done)!;
    expect(recorder.start).toHaveBeenCalledTimes(MAX_MIC_RESTARTS);
    expect(onMicStalled).toHaveBeenCalledTimes(MAX_MIC_RESTARTS);
    // What was captured survives, and no error is made up for the empty rest.
    expect(composeDictation(result.state).text).toBe('<capture0>');
    expect(result.error).toBeNull();
    const previews = transcribe.mock.calls.filter(([, , final]) => !final).map(([a]) => a.size);
    expect(new Set(previews).size).toBe(previews.length);
  });

  it('keeps the audio through a failed restart and tries again', async () => {
    const clock = { t: 0 };
    const recorder = freezingRecorder(clock);
    const start = recorder.start;
    let tries = 0;
    recorder.start = vi.fn(async () => {
      tries += 1;
      if (tries === 1) throw new Error('busy');
      await start();
    });
    const { d, fin } = stalledRun(recorder, clock, 12_000);
    await d.run();
    const result = (await fin.done)!;
    expect(tries).toBe(2);
    expect(composeDictation(result.state).text).toBe('<capture0> <capture1>');
    expect(result.error).toBeNull();
  });

  it('finishes without stopping when no capture could be opened', async () => {
    const clock = { t: 0 };
    const recorder = freezingRecorder(clock);
    recorder.start = vi.fn(async () => {
      throw new Error('busy');
    });
    const { d, fin } = stalledRun(recorder, clock, 30_000);
    await d.run();
    const result = (await fin.done)!;
    expect(recorder.start).toHaveBeenCalledTimes(MAX_MIC_RESTARTS);
    expect(recorder.stop).toHaveBeenCalledTimes(1);
    expect(composeDictation(result.state).text).toBe('<capture0>');
    expect(result.error).toBeNull();
    expect(result.tooShort).toBe(false);
  });

  it('falls back to the peeked audio when stop fails', async () => {
    const clock = { t: 0 };
    const recorder = freezingRecorder(clock);
    const stop = recorder.stop;
    recorder.stop = vi.fn(async () => {
      if (vi.mocked(recorder.stop).mock.calls.length === 1) throw new Error('thread gone');
      return stop();
    });
    const { d, fin } = stalledRun(recorder, clock, 9000);
    await d.run();
    const result = (await fin.done)!;
    expect(composeDictation(result.state).text).toBe('<capture0> <capture1>');
    expect(recorder.cancel).toHaveBeenCalledTimes(1);
  });

  it('finish waits for a restart in progress and keeps its order', async () => {
    const clock = { t: 0 };
    const recorder = freezingRecorder(clock);
    const start = recorder.start;
    const gate = deferred<void>();
    recorder.start = vi.fn(async () => {
      await start();
      await gate.promise;
    });
    const d = new Dictation({
      recorder,
      transcribe: async (audio, _ctx, final) => tr(final ? `<${tagOf(audio)}>` : 'x'),
      base: '',
      onUpdate: () => {},
      now: () => clock.t,
    });
    clock.t = 3000;
    await d.step();
    clock.t = 4000;
    await d.step();
    clock.t = 4000 + MIC_STALL_MS;
    const stepping = d.step();
    await vi.waitFor(() => expect(recorder.start).toHaveBeenCalledTimes(1));
    const finished = d.finish();
    await flush();
    expect(recorder.stop).toHaveBeenCalledTimes(1);
    clock.t = 6000;
    gate.resolve();
    await stepping;
    const result = await finished;
    expect(recorder.stop).toHaveBeenCalledTimes(2);
    expect(composeDictation(result.state).text).toBe('<capture0> <capture1>');
  });

  it('stopping during the restart does not reopen the microphone', async () => {
    const clock = { t: 0 };
    const recorder = freezingRecorder(clock);
    const stop = deferred<Blob>();
    recorder.stop = vi.fn(() => stop.promise);
    const d = new Dictation({
      recorder,
      transcribe: async (audio, _ctx, final) => tr(final ? `<${tagOf(audio)}>` : 'x'),
      base: '',
      onUpdate: () => {},
      now: () => clock.t,
    });
    clock.t = 3000;
    await d.step();
    clock.t = 4000;
    await d.step();
    clock.t = 4000 + MIC_STALL_MS;
    const stepping = d.step();
    await vi.waitFor(() => expect(recorder.stop).toHaveBeenCalledTimes(1));
    const finished = d.finish();
    stop.resolve(wav(3.85, 'capture0'));
    await stepping;
    const result = await finished;
    expect(recorder.start).not.toHaveBeenCalled();
    expect(recorder.stop).toHaveBeenCalledTimes(1);
    expect(composeDictation(result.state).text).toBe('<capture0>');
  });

  it('abandon during the restart keeps the microphone closed', async () => {
    const clock = { t: 0 };
    const recorder = freezingRecorder(clock);
    const stop = deferred<Blob>();
    recorder.stop = vi.fn(() => stop.promise);
    const transcribe = vi.fn(async () => tr('x'));
    const d = new Dictation({
      recorder,
      transcribe,
      base: '',
      onUpdate: () => {},
      now: () => clock.t,
    });
    clock.t = 3000;
    await d.step();
    clock.t = 4000;
    await d.step();
    clock.t = 4000 + MIC_STALL_MS;
    const stepping = d.step();
    await vi.waitFor(() => expect(recorder.stop).toHaveBeenCalledTimes(1));
    d.abandon();
    stop.resolve(wav(3.85, 'capture0'));
    await stepping;
    expect(recorder.start).not.toHaveBeenCalled();
    expect(transcribe).not.toHaveBeenCalledWith(expect.anything(), null, true);
  });

  it('leaves a capture that never delivered audio to onStall', async () => {
    const clock = { t: 0 };
    const recorder = freezingRecorder(clock, { dead: () => true });
    const { d, fin, onMicStalled } = stalledRun(recorder, clock, 10_000);
    await d.run();
    await fin.done;
    expect(recorder.start).not.toHaveBeenCalled();
    expect(onMicStalled).not.toHaveBeenCalled();
  });

  it('a recorder that cannot report progress is left alone', async () => {
    const clock = { t: 0 };
    const recorder = freezingRecorder(clock);
    recorder.captured = vi.fn(async () => {
      throw new Error('older shell');
    });
    const { d, fin, onMicStalled, transcribe } = stalledRun(recorder, clock, 6000);
    await d.run();
    await fin.done;
    expect(recorder.captured).toHaveBeenCalledTimes(1);
    expect(recorder.start).not.toHaveBeenCalled();
    expect(onMicStalled).not.toHaveBeenCalled();
    expect(transcribe).toHaveBeenCalled();
  });
});

describe('a final chunk that is the context coming back', () => {
  /** About the 31 characters of the field log. */
  const ECHO = '그리고 일정도 다시 한번 확인해서 저에게 알려 주세요.';
  const SAID = `오늘 회의 내용을 정리해서 팀 전체에 공유해 주세요. ${ECHO}`;

  /** A real 16 kHz PCM WAV (the echo check reads its level). */
  const pcmWav = (seconds: number, level: number) => {
    const samples = Math.round(16_000 * seconds);
    const pcm = new DataView(new ArrayBuffer(samples * 2));
    for (let i = 0; i < samples; i += 1) {
      pcm.setInt16(i * 2, Math.round((i % 2 ? level : -level) * 32767), true);
    }
    return new Blob([wavOf(new Uint8Array(pcm.buffer)) as BlobPart], { type: 'audio/wav' });
  };
  const LOUD = 0.2;

  /** One dictation after `SAID`: `previews` answer in turn, then `final` on stop. */
  async function dictate(o: {
    previews?: { seconds: number; text: string }[];
    audio: Blob;
    final: Transcript;
  }) {
    const previews = [...(o.previews ?? [])];
    const recorder = streamingRecorder({
      peek: vi.fn(async () => pcmWav(previews[0]?.seconds ?? 0, LOUD)),
      stop: vi.fn(async () => o.audio),
    });
    const d = new Dictation({
      recorder,
      transcribe: async (_audio, _context, final) =>
        final ? o.final : tr(previews.shift()?.text ?? ''),
      base: SAID,
      onUpdate: () => {},
    });
    vi.mocked(diag).mockClear();
    for (let n = previews.length; n > 0; n -= 1) await d.step();
    const result = await d.finish();
    const dropped = vi.mocked(diag).mock.calls.filter((c) => c[0] === 'dictation.echo_dropped');
    return { result, text: composeDictation(result.state).text, dropped };
  }
  const SILENT_PREVIEW = [{ seconds: 2.6, text: '' }];

  it('drops the field case: 3.4 s, 31 characters, nothing in the 2.6 s preview', async () => {
    const { text, result, dropped } = await dictate({
      previews: SILENT_PREVIEW,
      audio: pcmWav(3.4, LOUD),
      final: tr(ECHO),
    });
    expect(text).toBe(SAID);
    expect(result.tooShort).toBe(false);
    expect(composeDictation(result.state).dictated).toBe('');
    expect(dropped).toHaveLength(1);
  });

  it('logs lengths only, never the text', async () => {
    const { dropped } = await dictate({
      previews: SILENT_PREVIEW,
      audio: pcmWav(3.4, LOUD),
      final: tr(ECHO),
    });
    const data = dropped[0]![1]!;
    expect(data).toMatchObject({ audioMs: 3400, previewMs: 2600, contextChars: SAID.length });
    expect(Object.values(data).every((v) => typeof v === 'number')).toBe(true);
  });

  it('checks the recognizer output too (the correction pass may have changed the text)', async () => {
    const { text } = await dictate({
      previews: SILENT_PREVIEW,
      audio: pcmWav(3.4, LOUD),
      final: {
        text: '그리고 일정도 재확인해서 저에게 알려 주세요.',
        rawText: ECHO,
        corrections: [],
      },
    });
    expect(text).toBe(SAID);
  });

  it('drops the tail repeated over and over', async () => {
    const { text } = await dictate({
      previews: SILENT_PREVIEW,
      audio: pcmWav(3.4, LOUD),
      final: tr(`${ECHO} ${ECHO}`),
    });
    expect(text).toBe(SAID);
  });

  it('keeps a repeat the preview heard', async () => {
    const { text, dropped } = await dictate({
      previews: [{ seconds: 2.6, text: '다시 한번' }],
      audio: pcmWav(3.4, LOUD),
      final: tr(ECHO),
    });
    expect(text).toBe(`${SAID} ${ECHO}`);
    expect(dropped).toHaveLength(0);
  });

  it('keeps a repeat when any preview of the chunk heard it, even an earlier one', async () => {
    const { text } = await dictate({
      previews: [
        { seconds: 1.3, text: '그리고' },
        { seconds: 2.6, text: '' },
      ],
      audio: pcmWav(3.4, LOUD),
      final: tr(ECHO),
    });
    expect(text).toBe(`${SAID} ${ECHO}`);
  });

  it('counts a preview that answers while the chunk is being transcribed', async () => {
    const late = deferred<Transcript>();
    const final = deferred<Transcript>();
    const recorder = streamingRecorder({
      peek: vi.fn(async () => pcmWav(2.6, LOUD)),
      stop: vi.fn(async () => pcmWav(3.4, LOUD)),
    });
    const d = new Dictation({
      recorder,
      transcribe: (_audio, _context, isFinal) => (isFinal ? final.promise : late.promise),
      base: SAID,
      onUpdate: () => {},
    });
    const stepping = d.step();
    await flush();
    const finishing = d.finish();
    await flush();
    late.resolve(tr('다시 한번'));
    await stepping;
    final.resolve(tr(ECHO));
    expect(composeDictation((await finishing).state).text).toBe(`${SAID} ${ECHO}`);
  });

  it('keeps a short repeat that fits in the audio no preview covered', async () => {
    const { text } = await dictate({
      previews: SILENT_PREVIEW,
      audio: pcmWav(3.4, LOUD),
      final: tr('다시 한번 확인해서'),
    });
    expect(text).toBe(`${SAID} 다시 한번 확인해서`);
  });

  it('keeps a loud repeat no preview answered for', async () => {
    const { text } = await dictate({ audio: pcmWav(3.4, LOUD), final: tr(ECHO) });
    expect(text).toBe(`${SAID} ${ECHO}`);
  });

  it('drops an echo of a quiet chunk no preview answered for', async () => {
    const { text, dropped } = await dictate({ audio: pcmWav(1, 0.001), final: tr(ECHO) });
    expect(text).toBe(SAID);
    expect(dropped).toHaveLength(1);
  });

  it('keeps a word that merely occurs in the context', async () => {
    const { text } = await dictate({ audio: pcmWav(1, 0.001), final: tr('일정') });
    expect(text).toBe(`${SAID} 일정`);
  });

  it('keeps text that is not in the tail of the context', async () => {
    const other = '내일 회의 내용을 정리해서 팀 전체에 공유해 주세요.';
    const { text } = await dictate({
      previews: SILENT_PREVIEW,
      audio: pcmWav(3.4, LOUD),
      final: tr(other),
    });
    expect(text).toBe(`${SAID} ${other}`);
  });

  it('keeps a chunk that is not short, and one with no context to echo', async () => {
    const long = await dictate({
      previews: [{ seconds: 5.9, text: '' }],
      audio: pcmWav(6, LOUD),
      final: tr(ECHO),
    });
    expect(long.text).toBe(`${SAID} ${ECHO}`);
    const d = new Dictation({
      recorder: streamingRecorder({ stop: vi.fn(async () => pcmWav(1, 0.001)) }),
      transcribe: async () => tr(ECHO),
      base: '',
      onUpdate: () => {},
    });
    expect(composeDictation((await d.finish()).state).text).toBe(ECHO);
  });
});
