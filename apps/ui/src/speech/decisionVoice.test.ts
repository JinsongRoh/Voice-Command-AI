import { afterEach, describe, expect, it } from 'vitest';

import {
  claimDecisionVoice,
  decisionVoiceClaimed,
  decisionVoiceKey,
  resetDecisionVoice,
} from './decisionVoice';
import { voiceQueue } from './voiceQueue';

afterEach(() => {
  resetDecisionVoice();
  voiceQueue.stopAll();
});

describe('decision voice', () => {
  it('uses the spoken alert key of the decision', () => {
    // `approvals.ts`: `d:<session>:<decision>`, queued as `alert:<key>`.
    expect(decisionVoiceKey('s1', 'd1')).toBe('alert:d:s1:d1');
  });

  it('a hands-free read drops the queued alert for the same decision', async () => {
    const key = decisionVoiceKey('s1', 'd1');
    const played: string[] = [];
    let release: () => void = () => {};
    // Something plays first, so the alert waits.
    voiceQueue.enqueue({
      key: 'busy',
      priority: 'decision',
      play: () =>
        new Promise<void>((resolve) => {
          release = resolve;
        }),
      stop: () => release(),
    });
    voiceQueue.enqueue({
      key,
      priority: 'decision',
      play: async () => {
        played.push(key);
      },
      stop: () => {},
    });
    expect(voiceQueue.pending()).toEqual([key]);
    claimDecisionVoice(key);
    expect(voiceQueue.pending()).toEqual([]);
    expect(decisionVoiceClaimed(key)).toBe(true);
    expect(decisionVoiceClaimed(decisionVoiceKey('s1', 'd2'))).toBe(false);
    release();
    await new Promise((r) => setTimeout(r, 0));
    expect(played).toEqual([]);
  });

  it('a claim expires', () => {
    const key = decisionVoiceKey('s1', 'd1');
    claimDecisionVoice(key, 0);
    expect(decisionVoiceClaimed(key, 60_000)).toBe(true);
    expect(decisionVoiceClaimed(key, 31 * 60_000)).toBe(false);
  });
});
