import { describe, expect, it, vi } from 'vitest';

import { NativeError } from './bridge';
import { ENROLL_RETRY_MS, retryWhileEnrolling, secondsUntil } from './enrollWait';

const refused = new NativeError('unreachable', 'server unreachable');

describe('retryWhileEnrolling', () => {
  it('keeps the retry delay between 3 and 5 seconds', () => {
    expect(ENROLL_RETRY_MS).toBeGreaterThanOrEqual(3000);
    expect(ENROLL_RETRY_MS).toBeLessThanOrEqual(5000);
  });

  it('retries refused connections until one succeeds', async () => {
    let time = 0;
    const sleep = vi.fn(async (ms: number) => {
      time += ms;
    });
    const attempt = vi
      .fn<() => Promise<string>>()
      .mockRejectedValueOnce(refused)
      .mockRejectedValueOnce(new NativeError('timeout', 'slow'))
      .mockResolvedValue('paired');
    await expect(
      retryWhileEnrolling(attempt, { until: 30_000, now: () => time, sleep }),
    ).resolves.toBe('paired');
    expect(attempt).toHaveBeenCalledTimes(3);
    expect(sleep.mock.calls).toEqual([[4000], [4000]]);
  });

  it('gives up with the real error only when the window ends', async () => {
    let time = 0;
    const sleep = vi.fn(async (ms: number) => {
      time += ms;
    });
    const attempt = vi.fn(async () => {
      throw refused;
    });
    await expect(
      retryWhileEnrolling(attempt, { until: 30_000, now: () => time, sleep }),
    ).rejects.toBe(refused);
    expect(time).toBe(30_000);
    // Attempts at 0, 4, ... 28 s and a last one when the window closes.
    expect(attempt).toHaveBeenCalledTimes(9);
  });

  it('connects at once, without retrying, when there is no window', async () => {
    const sleep = vi.fn(async () => {});
    const attempt = vi.fn(async () => {
      throw refused;
    });
    await expect(retryWhileEnrolling(attempt, { until: 0, sleep })).rejects.toBe(refused);
    expect(attempt).toHaveBeenCalledOnce();
    expect(sleep).not.toHaveBeenCalled();
  });

  it('never retries an error that a retry cannot fix', async () => {
    const sleep = vi.fn(async () => {});
    const failure = new NativeError('totp_required', 'code needed');
    const attempt = vi.fn(async () => {
      throw failure;
    });
    await expect(retryWhileEnrolling(attempt, { until: Date.now() + 30_000, sleep })).rejects.toBe(
      failure,
    );
    expect(attempt).toHaveBeenCalledOnce();
  });

  it('rounds the seconds left up and never below zero', () => {
    expect(secondsUntil(30_000, () => 0)).toBe(30);
    expect(secondsUntil(1_001, () => 1)).toBe(1);
    expect(secondsUntil(0, () => 5_000)).toBe(0);
  });
});
