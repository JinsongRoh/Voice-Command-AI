import { describe, expect, it, vi } from 'vitest';

import {
  CHUNK_SECONDS,
  CONTEXT_CHARS,
  Dictation,
  MIN_REQUEST_GAP_MS,
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
  type DictationState,
} from './dictation';
import type { Recorder } from './recorder';
import type { Transcript } from './transcribe';

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

describe('Dictation', () => {
  it('shows interim text, commits split chunks as finals and passes context', async () => {
    const split = vi.fn(async (_min: number) => EMPTY);
    const recorder = streamingRecorder({ split });
    const transcribe = vi.fn(async (audio: Blob, _ctx: string | null) => tr(`<${tagOf(audio)}>`));
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
    expect(transcribe).toHaveBeenLastCalledWith(expect.anything(), 'Hello');

    split.mockResolvedValueOnce(wav(15, 'c0'));
    await d.step();
    expect(composeDictation(d.current).text).toBe('Hello <c0>');
    // Final chunk context: text before the chunk.
    expect(transcribe).toHaveBeenLastCalledWith(expect.objectContaining({ tag: 'c0' }), 'Hello');

    await d.step(); // interim again, with the committed chunk as context
    expect(transcribe).toHaveBeenLastCalledWith(expect.anything(), 'Hello <c0>');
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
    );
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
    expect(transcribe).toHaveBeenCalledWith(expect.any(Blob), 'Note:');
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
