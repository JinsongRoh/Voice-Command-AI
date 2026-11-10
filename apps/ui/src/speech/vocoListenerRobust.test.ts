import { describe, expect, it } from 'vitest';

import { frameLevels, noiseFloor, SPEECH_RMS, speechThreshold } from './handsFreeIO';
import type { Recorder } from './recorder';
import {
  LEAD_MS,
  LISTEN_POLL_MS,
  SHORT_END_SILENCE_MS,
  STALL_MS,
  VocoListener,
  wavOf,
  type HeardUtterance,
  type VocoListenerOptions,
} from './vocoListener';

const FRAME_MS = 20;
const FRAME_SAMPLES = 320;

/** `[untilMs, level]` steps. */
type Tape = [number, number][];

function levelAt(tape: Tape, ms: number): number {
  for (const [until, level] of tape) if (ms < until) return level;
  return tape[tape.length - 1]![1];
}

function pcm(tape: Tape, fromMs: number, frames: number): Uint8Array {
  const out = new Uint8Array(frames * FRAME_SAMPLES * 2);
  const v = new DataView(out.buffer);
  for (let f = 0; f < frames; f += 1) {
    const amp = Math.round(levelAt(tape, fromMs + f * FRAME_MS) * 32767);
    for (let i = 0; i < FRAME_SAMPLES; i += 1) {
      v.setInt16((f * FRAME_SAMPLES + i) * 2, i % 2 ? amp : -amp, true);
    }
  }
  return out;
}

const durationMs = (blob: Blob) => ((blob.size - 44) / 2 / 16_000) * 1000;

/** A streaming recorder over `tape`; `frozenAfter` makes its buffer stop growing (a dead capture). */
function tapeRecorder(tape: Tape, clock: { t: number }, frozenAfter = Infinity) {
  let from = 0;
  let open = false;
  const now = () => Math.min(clock.t, frozenAfter);
  const wav = () => {
    const frames = Math.max(0, Math.floor((now() - from) / FRAME_MS));
    return new Blob([wavOf(pcm(tape, from, frames)) as BlobPart], { type: 'audio/wav' });
  };
  const recorder: Recorder = {
    start: async () => {
      open = true;
      from = now();
    },
    stop: async () => {
      open = false;
      return wav();
    },
    cancel: () => {
      open = false;
    },
    peek: async () => {
      if (!open) throw new Error('not recording');
      return wav();
    },
    split: async () => {
      const head = wav();
      from += Math.floor((now() - from) / FRAME_MS) * FRAME_MS;
      return head;
    },
  };
  return recorder;
}

/** Runs a listener for `untilMs` of fake time; `recorders` are used one per capture. */
async function run(
  untilMs: number,
  recorders: (clock: { t: number }) => () => Recorder,
  extra: Partial<VocoListenerOptions> = {},
  clock: { t: number } = { t: 0 },
  stopAt = untilMs,
) {
  const next = recorders(clock);
  const events: string[] = [];
  const utterances: HeardUtterance[] = [];
  let done: () => void = () => {};
  const finished = new Promise<void>((resolve) => (done = resolve));
  const listener: VocoListener = new VocoListener({
    createRecorder: () => {
      events.push(`open@${clock.t}`);
      return next();
    },
    onSpeechStart: () => events.push(`start@${clock.t}`),
    onSpeechEnd: (u) => {
      events.push(u ? `end@${clock.t}` : `noise@${clock.t}`);
      if (u) utterances.push(u);
    },
    now: () => clock.t,
    ...extra,
    sleep: async (ms) => {
      clock.t += ms;
      if (clock.t >= stopAt) void listener.stop().then(done);
      await Promise.resolve();
    },
  });
  listener.start();
  await finished;
  return { events, utterances };
}

describe('adaptive VAD', () => {
  it('follows the noise floor of the room', () => {
    expect(noiseFloor(new Array(10).fill(0.01))).toBeNull();
    expect(noiseFloor(new Array(100).fill(0.002))).toBeCloseTo(0.002);
    expect(speechThreshold(null)).toBe(SPEECH_RMS);
    // Quiet room: a soft voice (0.009) is above 0.006.
    expect(speechThreshold(0.002)).toBeCloseTo(0.006);
    // Noisy room: steady noise stays below the speech level.
    expect(speechThreshold(0.015)).toBeCloseTo(0.0375);
  });

  it('hears a soft "보코" the fixed level missed', async () => {
    const tape: Tape = [
      [1500, 0.002], // a quiet room
      [2100, 0.009], // a soft call, 0.6 s (below the old 0.02)
      [5000, 0.002],
    ];
    const soft = await run(5000, (c) => () => tapeRecorder(tape, c));
    expect(soft.utterances).toHaveLength(1);
    const fixed = await run(5000, (c) => () => tapeRecorder(tape, c), { threshold: SPEECH_RMS });
    expect(fixed.utterances).toHaveLength(0);
  });

  it('keeps a 0.45 s call with its pre-roll', async () => {
    const tape: Tape = [
      [1000, 0],
      [1450, 0.2], // "보코", 0.45 s
      [4000, 0],
    ];
    const { utterances } = await run(4000, (c) => () => tapeRecorder(tape, c));
    expect(utterances).toHaveLength(1);
    expect(utterances[0]!.speechMs).toBeGreaterThanOrEqual(400);
    // The lead-in before the first syllable is kept so it is not clipped.
    expect(durationMs(utterances[0]!.audio)).toBeGreaterThanOrEqual(400 + LEAD_MS);
    // Only this utterance goes to the wake check: about 1.05 s of 16 kHz PCM.
    expect(durationMs(utterances[0]!.audio)).toBeLessThan(1200);
  });

  it('ends a short call after a short silence (the wake check starts sooner)', async () => {
    const tape: Tape = [
      [1000, 0],
      [1500, 0.2],
      [5000, 0],
    ];
    const slow = await run(5000, (c) => () => tapeRecorder(tape, c));
    const fast = await run(5000, (c) => () => tapeRecorder(tape, c), {
      endSilenceFor: (ms) => (ms < 1500 ? SHORT_END_SILENCE_MS : 800),
    });
    const at = (events: string[]) => Number(events.find((e) => e.startsWith('end@'))!.slice(4));
    expect(at(fast.events)).toBeLessThan(at(slow.events));
    // Speech end -> utterance handed over within the short silence plus one poll.
    expect(at(fast.events) - 1500).toBeLessThanOrEqual(SHORT_END_SILENCE_MS + 200);
  });
});

describe('a capture that stops delivering audio', () => {
  it('is replaced by a fresh one, which hears the call', async () => {
    const tape: Tape = [
      [6000, 0],
      [6500, 0.2], // the call, after the first capture died at 1 s
      [9000, 0],
    ];
    let n = 0;
    const { events, utterances } = await run(9000, (c) => () => {
      n += 1;
      return tapeRecorder(tape, c, n === 1 ? 1000 : Infinity);
    });
    expect(events.filter((e) => e.startsWith('open@'))).toHaveLength(2);
    expect(utterances).toHaveLength(1);
  });

  it('frozen by a voice: replaced as soon as the voice ends', async () => {
    const tape: Tape = [[9000, 0]];
    let n = 0;
    let busy = false;
    let endedAt = 0;
    const clock = { t: 0 };
    const opens: number[] = [];
    await run(
      6000,
      () => () => {
        n += 1;
        opens.push(clock.t);
        return tapeRecorder(tape, clock, n === 1 ? 1000 : Infinity);
      },
      {
        busy: () => {
          busy = clock.t >= 1000 && clock.t < 4000;
          if (busy) endedAt = clock.t;
          return busy;
        },
        now: () => clock.t,
        sleep: async (ms) => {
          clock.t += ms;
          await Promise.resolve();
        },
      },
      clock,
      6000,
    );
    expect(opens).toHaveLength(2);
    // Never during the voice, and well before `STALL_MS` after it.
    expect(opens[1]).toBeGreaterThan(endedAt);
    expect(opens[1]! - endedAt).toBeLessThanOrEqual(2 * LISTEN_POLL_MS);
  });

  it('a voice over a healthy capture restarts nothing', async () => {
    const tape: Tape = [[6000, 0]];
    const clock = { t: 0 };
    const { events } = await run(
      6000,
      () => () => tapeRecorder(tape, clock),
      {
        busy: () => clock.t >= 1000 && clock.t < 4000,
        now: () => clock.t,
        sleep: async (ms) => {
          clock.t += ms;
          await Promise.resolve();
        },
      },
      clock,
      6000,
    );
    expect(events.filter((e) => e.startsWith('open@'))).toHaveLength(1);
  });

  it('a voice shorter than a poll leaves it to the stall check', async () => {
    const tape: Tape = [[6000, 0]];
    const clock = { t: 0 };
    const opens: number[] = [];
    let n = 0;
    await run(
      6000,
      () => () => {
        n += 1;
        opens.push(clock.t);
        return tapeRecorder(tape, clock, n === 1 ? 1000 : Infinity);
      },
      {
        busy: () => clock.t === 1000,
        now: () => clock.t,
        sleep: async (ms) => {
          clock.t += ms;
          await Promise.resolve();
        },
      },
      clock,
      6000,
    );
    expect(opens[1]).toBeGreaterThanOrEqual(1000 + STALL_MS);
  });

  it('a microphone that fails to open is tried again', async () => {
    const tape: Tape = [[9000, 0]];
    let n = 0;
    const seen: string[] = [];
    let listening: () => void = () => {};
    const ok = new Promise<void>((r) => (listening = r));
    const clock = { t: 0 };
    const listener = new VocoListener({
      createRecorder: () => {
        n += 1;
        const r = tapeRecorder(tape, clock);
        if (n === 1) r.start = async () => Promise.reject(new Error('busy'));
        return r;
      },
      onSpeechStart: () => {},
      onSpeechEnd: () => {},
      onError: () => seen.push('error'),
      onListening: () => {
        seen.push('listening');
        listening();
      },
      retryMs: [5],
      sleep: async (ms) => {
        clock.t += ms;
        await new Promise((r) => setTimeout(r, 0));
      },
      now: () => clock.t,
    });
    listener.start();
    await ok;
    await listener.stop();
    expect(seen).toEqual(['error', 'listening']);
    expect(n).toBe(2);
  });
});

describe('lead-in of an utterance', () => {
  /** Like the native `split`: the cut leaves the last second in the buffer. */
  const keepsASecond = (tape: Tape) => (clock: { t: number }) => (): Recorder => {
    let from = 0;
    let open = false;
    const frames = () => Math.floor((clock.t - from) / FRAME_MS);
    const wav = () =>
      new Blob([wavOf(pcm(tape, from, frames())) as BlobPart], { type: 'audio/wav' });
    return {
      start: async () => {
        open = true;
        from = clock.t;
      },
      stop: async () => {
        open = false;
        return wav();
      },
      cancel: () => {
        open = false;
      },
      peek: async () => {
        if (!open) throw new Error('not recording');
        return wav();
      },
      split: async () => {
        const head = Math.max(0, frames() * FRAME_MS - 1000);
        from += head;
        return new Blob([new Uint8Array(44 + head * 32)], { type: 'audio/wav' });
      },
    };
  };

  it('reaches back over the silence a drop left in the buffer', async () => {
    // A silence drop at 3 s; the speech starts 0.1 s after it.
    const tape: Tape = [
      [3100, 0],
      [4100, 0.2],
      [7000, 0],
    ];
    const { utterances } = await run(7000, keepsASecond(tape));
    expect(utterances).toHaveLength(1);
    expect(durationMs(utterances[0]!.audio)).toBeGreaterThanOrEqual(1000 + LEAD_MS);
  });

  it('never reaches back into a voice that was playing', async () => {
    const tape: Tape = [
      [2200, 0.3],
      [2300, 0],
      [3300, 0.2],
      [6000, 0],
    ];
    const clock = { t: 0 };
    const { utterances } = await run(
      6000,
      keepsASecond(tape),
      { busy: () => clock.t < 2400 },
      clock,
    );
    expect(utterances).toHaveLength(1);
    const levels = frameLevels(new Uint8Array(await utterances[0]!.audio.arrayBuffer()));
    expect(Math.max(...levels.slice(0, 4))).toBeLessThan(0.1);
  });
});

describe('giving the microphone up', () => {
  it('does not wait for the next poll', async () => {
    let polling: () => void = () => {};
    const polled = new Promise<void>((resolve) => (polling = resolve));
    const recorder = tapeRecorder([[60_000, 0]], { t: 0 });
    const listener = new VocoListener({
      createRecorder: () => recorder,
      onSpeechStart: () => {},
      onSpeechEnd: () => {},
      // A poll wait that outlasts the test.
      sleep: () => {
        polling();
        return new Promise(() => {});
      },
    });
    listener.start();
    await polled;
    const released = await Promise.race([
      listener.pause().then(() => true),
      new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 100)),
    ]);
    expect(released).toBe(true);
  });

  it('a press during the opening still waits for that capture to close', async () => {
    let opened: () => void = () => {};
    const opening = new Promise<void>((resolve) => (opened = resolve));
    const recorder = tapeRecorder([[60_000, 0]], { t: 0 });
    const order: string[] = [];
    const listener = new VocoListener({
      createRecorder: () => ({
        ...recorder,
        start: () => opening,
        stop: async () => {
          order.push('closed');
          return new Blob();
        },
      }),
      onSpeechStart: () => {},
      onSpeechEnd: () => {},
    });
    listener.start();
    const pausing = listener.pause().then(() => order.push('released'));
    await Promise.resolve();
    opened();
    await pausing;
    expect(order).toEqual(['closed', 'released']);
  });
});
