import { describe, expect, it, vi } from 'vitest';

import { firstFrameAt, type Recorder } from './recorder';

const recorderWith = (captured?: Recorder['captured']): Recorder => ({
  start: async () => {},
  stop: async () => new Blob(),
  cancel: () => {},
  ...(captured ? { captured } : {}),
});

describe('firstFrameAt', () => {
  const clock = () => {
    const c = { t: 1000 };
    return {
      now: () => c.t,
      sleep: async (ms: number) => {
        c.t += ms;
      },
    };
  };

  it('is the first time the capture has samples', async () => {
    const captured = vi
      .fn<() => Promise<number | null>>()
      .mockResolvedValueOnce(0)
      .mockResolvedValueOnce(0)
      .mockResolvedValue(160);
    const at = await firstFrameAt(recorderWith(captured), () => true, { ...clock(), pollMs: 30 });
    expect(at).toBe(1060);
    expect(captured).toHaveBeenCalledTimes(3);
  });

  it('is unknown without a counter, when it fails, or when no audio comes', async () => {
    const opts = { ...clock(), pollMs: 30, maxMs: 90 };
    expect(await firstFrameAt(recorderWith(), () => true, opts)).toBeNull();
    const broken = recorderWith(() => Promise.reject(new Error('older shell')));
    expect(await firstFrameAt(broken, () => true, opts)).toBeNull();
    const silent = vi.fn(async () => 0);
    expect(await firstFrameAt(recorderWith(silent), () => true, opts)).toBeNull();
    expect(silent.mock.calls.length).toBeLessThanOrEqual(4);
    expect(
      await firstFrameAt(
        recorderWith(async () => null),
        () => true,
        opts,
      ),
    ).toBeNull();
  });

  it('stops reading once the recording is over', async () => {
    const { now, sleep } = clock();
    const silent = vi.fn(async () => 0);
    let live = true;
    const at = await firstFrameAt(recorderWith(silent), () => live, {
      now,
      sleep: async (ms) => {
        await sleep(ms);
        live = false;
      },
    });
    expect(at).toBeNull();
    expect(silent).toHaveBeenCalledTimes(1);
  });
});
