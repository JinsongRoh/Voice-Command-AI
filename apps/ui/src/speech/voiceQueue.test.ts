import { describe, expect, it, vi } from 'vitest';

import { createAlertVoice } from './alertVoice';
import { createVoiceQueue, MAX_AGE_MS, type VoiceItem, type VoicePriority } from './voiceQueue';

/** An item whose playback the test ends by hand. */
function controlled(key: string, priority: VoicePriority = 'completion') {
  let finish: () => void = () => {};
  const item = {
    key,
    priority,
    started: false,
    stopped: 0,
    play: vi.fn(
      () =>
        new Promise<void>((resolve) => {
          item.started = true;
          finish = resolve;
        }),
    ),
    stop: vi.fn(() => {
      item.stopped += 1;
      finish();
    }),
    end: () => finish(),
  };
  return item;
}

const flush = async () => {
  for (let i = 0; i < 200; i += 1) await Promise.resolve();
};

function setup(over: { cap?: number; itemTimeoutMs?: number } = {}) {
  let clock = 0;
  const played: string[] = [];
  const queue = createVoiceQueue({
    gapMs: 0,
    sleep: async () => {},
    now: () => clock,
    ...over,
  });
  const quick = (key: string, priority: VoicePriority = 'completion'): VoiceItem => ({
    key,
    priority,
    play: async () => {
      played.push(key);
    },
    stop: () => {},
  });
  return {
    queue,
    played,
    quick,
    advance: (ms: number) => {
      clock += ms;
    },
  };
}

describe('voice queue', () => {
  it('plays in order without cutting the current item off, urgent items first', async () => {
    const { queue, played, quick } = setup();
    const first = controlled('first');
    queue.enqueue(first);
    await flush();
    expect(first.started).toBe(true);
    queue.enqueue(quick('done-1', 'completion'));
    queue.enqueue(quick('error-1', 'error'));
    queue.enqueue(quick('approve-1', 'decision'));
    queue.enqueue(quick('done-2', 'completion'));
    queue.enqueue(quick('brief', 'briefing'));
    expect(queue.pending()).toEqual(['approve-1', 'error-1', 'brief', 'done-1', 'done-2']);
    // Nothing interrupts the item that is playing.
    expect(first.stop).not.toHaveBeenCalled();
    expect(queue.speaking).toBe(true);
    first.end();
    await flush();
    expect(played).toEqual(['approve-1', 'error-1', 'brief', 'done-1', 'done-2']);
    expect(queue.speaking).toBe(false);
  });

  it('drops duplicates of a queued or playing key', async () => {
    const { queue, played, quick } = setup();
    const first = controlled('a');
    expect(queue.enqueue(first)).toBe(true);
    await flush();
    expect(queue.enqueue(quick('a'))).toBe(false);
    expect(queue.enqueue(quick('b'))).toBe(true);
    expect(queue.enqueue(quick('b'))).toBe(false);
    first.end();
    await flush();
    expect(played).toEqual(['b']);
  });

  it('caps the queue, dropping the least urgent and newest first', async () => {
    const { queue, played, quick } = setup({ cap: 3 });
    const first = controlled('playing');
    queue.enqueue(first);
    await flush();
    queue.enqueue(quick('done-1'));
    queue.enqueue(quick('done-2'));
    queue.enqueue(quick('err', 'error'));
    // Over the cap: the newest completion goes.
    expect(queue.pending()).toEqual(['err', 'done-1', 'done-2']);
    expect(queue.enqueue(quick('done-3'))).toBe(false);
    expect(queue.enqueue(quick('approve', 'decision'))).toBe(true);
    expect(queue.pending()).toEqual(['approve', 'err', 'done-1']);
    first.end();
    await flush();
    expect(played).toEqual(['approve', 'err', 'done-1']);
  });

  it('drops items that waited too long', async () => {
    const { queue, played, quick, advance } = setup();
    const first = controlled('long');
    queue.enqueue(first);
    await flush();
    queue.enqueue(quick('stale'));
    advance(61_000);
    queue.enqueue(quick('fresh'));
    first.end();
    await flush();
    expect(played).toEqual(['fresh']);
  });

  it('stop all silences the current item and empties the queue', async () => {
    const { queue, played, quick } = setup();
    const first = controlled('now');
    queue.enqueue(first);
    await flush();
    queue.enqueue(quick('later'));
    queue.stopAll();
    await flush();
    expect(first.stop).toHaveBeenCalled();
    expect(played).toEqual([]);
    expect(queue.speaking).toBe(false);
    // The queue keeps working afterwards.
    queue.enqueue(quick('next'));
    await flush();
    expect(played).toEqual(['next']);
  });

  it('pauses for exclusive use (hands-free) and replays the interrupted item after', async () => {
    const { queue, played, quick } = setup();
    const first = controlled('interrupted', 'decision');
    queue.enqueue(first);
    await flush();
    let release: () => void = () => {};
    const readBack = queue.exclusive(
      () =>
        new Promise<string>((resolve) => {
          release = () => resolve('sent');
        }),
    );
    await flush();
    expect(first.stop).toHaveBeenCalledTimes(1);
    expect(queue.speaking).toBe(false);
    queue.enqueue(quick('meanwhile'));
    await flush();
    // Nothing plays while the read-back owns the voice.
    expect(played).toEqual([]);
    release();
    expect(await readBack).toBe('sent');
    await flush();
    // The interrupted alert is heard again, then the rest.
    expect(first.play).toHaveBeenCalledTimes(2);
    first.end();
    await flush();
    expect(played).toEqual(['meanwhile']);
  });

  it('a long pause does not age what waits, and pauses nest', async () => {
    const { queue, played, quick, advance } = setup();
    let endHold: () => void = () => {};
    const hold = queue.exclusive(
      () =>
        new Promise<void>((resolve) => {
          endHold = resolve;
        }),
    );
    queue.enqueue(quick('alert'));
    advance(MAX_AGE_MS * 3);
    // A voice flow during the pause runs at once and does not end it.
    await queue.exclusive(async () => {});
    await flush();
    expect(played).toEqual([]);
    endHold();
    await hold;
    await flush();
    expect(played).toEqual(['alert']);
  });

  it('a stuck item is stopped after its time limit and the queue moves on', async () => {
    const { queue, played, quick } = setup({ itemTimeoutMs: 20 });
    const stuck = controlled('stuck');
    // `stop` does not end this player either: it is truly stuck.
    stuck.stop.mockImplementation(() => {
      stuck.stopped += 1;
    });
    queue.enqueue(stuck);
    queue.enqueue(quick('after'));
    await vi.waitFor(() => expect(played).toEqual(['after']));
    expect(stuck.stopped).toBe(1);
  });

  it('device-voice and PC-voice items share the queue and wait for their ends', async () => {
    const events: string[] = [];
    let endDevice: () => void = () => {};
    let endClip: () => void = () => {};
    const voice = createAlertVoice({
      speakDeviceToEnd: (text) =>
        new Promise<void>((resolve) => {
          events.push(`device:${text}`);
          endDevice = resolve;
        }),
      playToEnd: () =>
        new Promise<void>((resolve) => {
          events.push('pc:clip');
          endClip = resolve;
        }),
      stopAll: () => events.push('stop'),
    });
    const synthesize = vi.fn(async () => ({ format: 'mp3', data: new Uint8Array([1]) }));
    const { queue } = setup();
    queue.enqueue({
      key: 'device',
      priority: 'decision',
      play: () => voice.play('one', 'en', { serverEngine: null }, synthesize),
      stop: () => voice.stop(),
    });
    queue.enqueue({
      key: 'pc',
      priority: 'decision',
      play: () => voice.play('two', 'en', { serverEngine: 'local-tts' }, synthesize),
      stop: () => voice.stop(),
    });
    await flush();
    expect(events).toEqual(['device:one']);
    endDevice();
    await vi.waitFor(() => expect(events).toEqual(['device:one', 'pc:clip']));
    expect(synthesize).toHaveBeenCalledWith('two', 'en-US', 'local-tts', null);
    expect(queue.speaking).toBe(true);
    endClip();
    await vi.waitFor(() => expect(queue.speaking).toBe(false));
    // Stopping goes to both voices.
    queue.enqueue({
      key: 'x',
      priority: 'error',
      play: () => voice.play('three', 'en', { serverEngine: null }, synthesize),
      stop: () => voice.stop(),
    });
    await flush();
    queue.stopAll();
    expect(events.at(-1)).toBe('stop');
  });
});
