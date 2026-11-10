import { describe, expect, it } from 'vitest';

import { frameLevels } from './handsFreeIO';
import type { Recorder } from './recorder';
import { VocoFlow, WAKE_HEAD_MS, type VocoTimers } from './vocoFlow';
import {
  END_SILENCE_MS,
  LEAD_MS,
  LONG_END_SILENCE_MS,
  pausedBeforeLast,
  SHORT_END_SILENCE_MS,
  sliceWav,
  speechBursts,
  vocoEndSilenceMs,
  VocoListener,
  wavOf,
  type HeardUtterance,
} from './vocoListener';

const FRAME_MS = 20;
const FRAME_SAMPLES = 320;

/** Audio level over time: `[untilMs, level]` steps. */
type Tape = [number, number][];

function levelAt(tape: Tape, ms: number): number {
  for (const [until, level] of tape) if (ms < until) return level;
  return 0;
}

/** 16-bit PCM of `frames` frames starting at `fromMs`, each at the tape's level. */
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

/**
 * A streaming recorder playing `tape` against a fake clock: `peek` returns
 * the audio since the last `split`, `split` drops all of it.
 */
function tapeRecorder(tape: Tape, clock: { t: number }) {
  let from = 0;
  let open = false;
  const stats = { starts: 0, stops: 0, cancels: 0 };
  const wav = () => {
    const frames = Math.floor((clock.t - from) / FRAME_MS);
    return new Blob([wavOf(pcm(tape, from, frames)) as BlobPart], { type: 'audio/wav' });
  };
  const recorder: Recorder = {
    start: async () => {
      open = true;
      stats.starts += 1;
      from = clock.t;
    },
    stop: async () => {
      open = false;
      stats.stops += 1;
      return wav();
    },
    cancel: () => {
      open = false;
      stats.cancels += 1;
    },
    peek: async () => {
      if (!open) throw new Error('not recording');
      return wav();
    },
    split: async () => {
      const head = wav();
      from += Math.floor((clock.t - from) / FRAME_MS) * FRAME_MS;
      return head;
    },
  };
  return { recorder, stats };
}

/** Runs a listener over the whole tape (the fake clock moves with every poll). */
async function play(
  tape: Tape,
  handlers: {
    onSpeechStart?(): void;
    onSpeechEnd?(u: HeardUtterance | null): void;
    busy?(): boolean;
    endSilenceFor?(speechMs: number): number;
  } = {},
) {
  const clock = { t: 0 };
  const end = tape[tape.length - 1]![0] + 2000;
  const { recorder, stats } = tapeRecorder(tape, clock);
  const events: string[] = [];
  const utterances: HeardUtterance[] = [];
  let done: () => void = () => {};
  const finished = new Promise<void>((resolve) => (done = resolve));
  const listener: VocoListener = new VocoListener({
    createRecorder: () => recorder,
    onSpeechStart: () => {
      events.push(`start@${clock.t}`);
      handlers.onSpeechStart?.();
    },
    onSpeechEnd: (u) => {
      events.push(u ? `end@${clock.t}` : `noise@${clock.t}`);
      if (u) utterances.push(u);
      handlers.onSpeechEnd?.(u);
    },
    busy: handlers.busy,
    endSilenceFor: handlers.endSilenceFor,
    now: () => clock.t,
    sleep: async (ms) => {
      clock.t += ms;
      if (clock.t >= end) void listener.stop().then(done);
      await Promise.resolve();
    },
  });
  listener.start();
  await finished;
  return { events, utterances, stats };
}

describe('VocoListener', () => {
  it('cuts utterances at pauses and keeps only the speech', async () => {
    const tape: Tape = [
      [1000, 0],
      [2000, 0.2], // 1 s of speech
      [4000, 0],
      [4500, 0.2], // 0.5 s
      [6000, 0],
    ];
    const { utterances, events, stats } = await play(tape);
    expect(utterances).toHaveLength(2);
    expect(events.filter((e) => e.startsWith('start'))).toHaveLength(2);
    // Speech plus the lead-in before it and about 0.2 s after it.
    expect(durationMs(utterances[0]!.audio)).toBeGreaterThanOrEqual(1000 + LEAD_MS);
    expect(durationMs(utterances[0]!.audio)).toBeLessThanOrEqual(1300 + LEAD_MS);
    expect(utterances[0]!.speechMs).toBeGreaterThanOrEqual(900);
    expect(utterances[1]!.speechMs).toBeLessThan(600);
    // The microphone is released at the end (awaited stop, never a blind cancel).
    expect(stats.stops).toBe(1);
  });

  it('gives the wake audio a 0.4 s lead-in', () => {
    expect(LEAD_MS).toBe(400);
  });

  it('a click is a noise, not an utterance', async () => {
    const { utterances } = await play([
      [1000, 0],
      [1100, 0.3],
      [3000, 0],
    ]);
    expect(utterances).toHaveLength(0);
  });

  it('notices a pause right before short last words', async () => {
    const { utterances } = await play([
      [500, 0],
      [2000, 0.2], // "로그인 버그 고쳐줘"
      [2700, 0], // 0.7 s pause (shorter than the end of an utterance)
      [3300, 0.2], // "전송해"
      [5000, 0],
    ]);
    expect(utterances).toHaveLength(1);
    expect(utterances[0]!.pausedBeforeLast).toBe(true);
  });

  it('drops audio while another voice is playing', async () => {
    const { utterances } = await play(
      [
        [500, 0],
        [2500, 0.2],
        [4000, 0],
      ],
      { busy: () => true },
    );
    expect(utterances).toHaveLength(0);
  });

  it('head() is the first part of the speech only', async () => {
    const { utterances } = await play([
      [500, 0],
      [6500, 0.2], // 6 s of talk
      [8000, 0],
    ]);
    expect(utterances).toHaveLength(1);
    const head = utterances[0]!.head(WAKE_HEAD_MS);
    // The lead-in comes on top: the head reaches as far into the speech as ever.
    expect(durationMs(head)).toBe(WAKE_HEAD_MS + LEAD_MS);
    expect(durationMs(utterances[0]!.audio)).toBeGreaterThan(6000);
  });
});

describe('end of speech in a long dictation', () => {
  it('the short end silence is only for a call while waiting', () => {
    expect(vocoEndSilenceMs(true, 600)).toBe(SHORT_END_SILENCE_MS);
    expect(vocoEndSilenceMs(true, 3000)).toBe(END_SILENCE_MS);
    // Listening to a message: never the short value, longer once it runs long.
    expect(vocoEndSilenceMs(false, 600)).toBe(END_SILENCE_MS);
    expect(vocoEndSilenceMs(false, 20_000)).toBe(LONG_END_SILENCE_MS);
    expect(LONG_END_SILENCE_MS).toBeGreaterThan(END_SILENCE_MS);
  });

  it('a 1 s thinking pause keeps a long dictation in one piece', async () => {
    const tape: Tape = [
      [500, 0],
      [8500, 0.2], // 8 s of dictation
      [9500, 0], // 1 s pause to think
      [11_500, 0.2], // the sentence goes on
      [14_000, 0],
    ];
    const awake = await play(tape, { endSilenceFor: (ms) => vocoEndSilenceMs(false, ms) });
    expect(awake.utterances).toHaveLength(1);
    expect(awake.utterances[0]!.speechMs).toBeGreaterThan(10_000);
    // The old fixed 0.8 s cut it in two.
    const fixed = await play(tape);
    expect(fixed.utterances).toHaveLength(2);
  });
});

describe('privacy before the wake word', () => {
  it('only short wake checks reach STT until "보코"; then the message is transcribed', async () => {
    const tape: Tape = [
      [1000, 0],
      [5000, 0.2], // people talking near the phone (4 s)
      [6500, 0],
      [7000, 0.2], // "보코"
      [8500, 0],
      [10_000, 0.2], // "로그인 버그 고쳐줘 오버"
      [12_000, 0],
    ];
    const heard = ['점심 메뉴는 김치찌개로 하자 그리고', '보코', '로그인 버그 고쳐줘 오버'];
    const calls: { purpose: string; ms: number }[] = [];
    const sent: string[] = [];
    let box = '';
    let now = 0;
    const timers: VocoTimers = { now: () => now, set: () => () => {} };
    const flow = new VocoFlow({
      lang: () => 'ko',
      settings: () => ({
        confirm: 'undo',
        autoSend: true,
        countdownMs: 3000,
        wakeWords: [],
        sendPhrases: [],
      }),
      transcribe: async (audio, purpose) => {
        calls.push({ purpose, ms: durationMs(audio) });
        return { text: heard[calls.length - 1] ?? '', corrections: [] };
      },
      speak: async () => {},
      chime: () => {},
      text: () => box,
      setText: (t) => {
        box = t;
      },
      send: async (t) => {
        sent.push(t);
        return true;
      },
      readBack: async () => {},
      sessionLabel: () => '1번 세션',
      findSession: () => null,
      briefing: () => {},
      onView: () => {},
      timers,
    });
    await play(tape, {
      onSpeechStart: () => flow.speechStart(),
      onSpeechEnd: (u) => {
        now += 1;
        flow.speechEnd(u);
      },
    });
    await flow.settled();
    expect(calls.map((c) => c.purpose)).toEqual(['wake', 'wake', 'dictation']);
    // The 4 s of nearby talk was checked with a short head only.
    expect(calls[0]!.ms).toBeLessThanOrEqual(WAKE_HEAD_MS + LEAD_MS);
    expect(box).toBe('로그인 버그 고쳐줘');
    expect(flow.state).toBe('undo');
    expect(sent).toEqual([]);
  });
});

describe('helpers', () => {
  it('bursts and the pause before the last words', () => {
    const f = (ms: number, level: number) => Array<number>(ms / FRAME_MS).fill(level);
    const levels = [...f(400, 0.1), ...f(700, 0), ...f(400, 0.1), ...f(100, 0)];
    const bursts = speechBursts(levels);
    expect(bursts).toHaveLength(2);
    expect(pausedBeforeLast(bursts)).toBe(true);
    expect(pausedBeforeLast(speechBursts([...f(400, 0.1), ...f(300, 0), ...f(400, 0.1)]))).toBe(
      false,
    );
  });

  it('slices WAV frames', async () => {
    const wav = wavOf(pcm([[1000, 0.5]], 0, 50));
    const part = sliceWav(wav, 10, 20);
    expect(part.size).toBe(44 + 10 * 640);
    const levels = frameLevels(new Uint8Array(await part.arrayBuffer()));
    expect(levels).toHaveLength(10);
    expect(levels[0]).toBeCloseTo(0.5, 2);
  });
});
