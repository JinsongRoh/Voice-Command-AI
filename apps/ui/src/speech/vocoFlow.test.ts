import { describe, expect, it } from 'vitest';

import {
  endsOpenKorean,
  endsWithCommand,
  HOLD_MAX_MS,
  IDLE_MS,
  parseSessionRef,
  parseSpokenNumber,
  SCHEDULE_SILENCE_MS,
  STALE_WAKE_MS,
  TRAILING_SEND_SILENCE_MS,
  UNDO_MS,
  voiceCommand,
  VocoFlow,
  WAKE_HEAD_MS,
  type VocoDeps,
  type VocoSettings,
  type VocoTimers,
  type VocoView,
} from './vocoFlow';
import type { HeardUtterance } from './vocoListener';

/** A clock and timer queue advanced by hand. */
function manualTimers() {
  let now = 1_000;
  const queue: { at: number; fn: () => void; dead: boolean }[] = [];
  const timers: VocoTimers & { advance(ms: number): Promise<void> } = {
    now: () => now,
    set(fn, ms) {
      const t = { at: now + ms, fn, dead: false };
      queue.push(t);
      return () => {
        t.dead = true;
      };
    },
    async advance(ms) {
      const end = now + ms;
      for (;;) {
        const next = queue.filter((t) => !t.dead && t.at <= end).sort((a, b) => a.at - b.at)[0];
        if (!next) break;
        now = next.at;
        next.dead = true;
        next.fn();
        await flush();
      }
      now = end;
      await flush();
    },
  };
  return timers;
}

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

/**
 * The flow with scripted STT: each utterance is a key; `script[key]` is
 * what the recognizer hears (the head of a long utterance is `head:<key>`).
 */
function setup(
  script: Record<string, string>,
  opts: {
    box?: string;
    settings?: Partial<VocoSettings>;
    sendOk?: boolean;
    sendHold?: Promise<void>;
    queued?: () => boolean;
    cancelQueued?: () => void;
  } = {},
) {
  const timers = manualTimers();
  const names = new Map<Blob, string>();
  const calls: { purpose: 'wake' | 'dictation'; key: string; bytes: number }[] = [];
  const spoken: string[] = [];
  const chimes: string[] = [];
  const sent: string[] = [];
  const readBacks: string[] = [];
  const selected: [number, string][] = [];
  const views: VocoView[] = [];
  let briefings = 0;
  let box = opts.box ?? '';
  const holds = new Map<string, Promise<void>>();
  const deps: VocoDeps = {
    lang: () => 'ko',
    settings: () => ({
      confirm: 'undo',
      autoSend: true,
      countdownMs: 3000,
      wakeWords: [],
      sendPhrases: [],
      ...opts.settings,
    }),
    transcribe: async (audio, purpose) => {
      const key = names.get(audio)!;
      calls.push({ purpose, key, bytes: audio.size });
      await holds.get(key);
      return { text: script[key] ?? '', corrections: [] };
    },
    speak: async (text) => {
      spoken.push(text);
      await holds.get('speak');
    },
    chime: (kind) => chimes.push(kind),
    text: () => box,
    setText: (text) => {
      box = text;
    },
    send: async (text) => {
      sent.push(text);
      await opts.sendHold;
      if (opts.sendOk === false) return false;
      box = '';
      return true;
    },
    readBack: async (text) => {
      readBacks.push(text);
    },
    ...(opts.queued ? { queued: opts.queued } : {}),
    ...(opts.cancelQueued ? { cancelQueued: opts.cancelQueued } : {}),
    sessionLabel: () => '3번 세션',
    findSession: (ref) =>
      ref.number <= 5
        ? { label: `${ref.number}번 세션`, select: (carry) => selected.push([ref.number, carry]) }
        : null,
    briefing: () => {
      briefings += 1;
    },
    onView: (v) => views.push(v),
    timers,
  };
  const flow = new VocoFlow(deps);
  const blob = (key: string) => {
    const b = new Blob([key]);
    names.set(b, key);
    return b;
  };
  const utterance = (key: string, speechMs = 1000, pausedBeforeLast = false): HeardUtterance => ({
    audio: blob(key),
    head: () => blob(`head:${key}`),
    speechMs,
    pausedBeforeLast,
    endedAt: timers.now(),
  });
  /** One utterance: speech starts, ends, and is handled. */
  const say = async (key: string, speechMs = 1000, paused = false) => {
    flow.speechStart();
    flow.speechEnd(utterance(key, speechMs, paused));
    await flow.settled();
    await flush();
  };
  return {
    flow,
    timers,
    calls,
    spoken,
    chimes,
    sent,
    readBacks,
    selected,
    views,
    holds,
    say,
    utterance,
    box: () => box,
    briefings: () => briefings,
    last: () => views[views.length - 1],
  };
}

describe('wake word', () => {
  it('"보컬" wakes, "…보내줘 오버" sends the text without the radio words', async () => {
    const t = setup({ a: '보컬', b: '어 로그인 버그 고쳐줘 보내줘 오버' });
    await t.say('a');
    expect(t.flow.state).toBe('awake');
    expect(t.chimes).toEqual(['wake']);
    expect(t.spoken).toEqual(['네']);
    await t.say('b');
    expect(t.box()).toBe('어 로그인 버그 고쳐줘');
    expect(t.flow.state).toBe('undo');
    expect(t.spoken[1]).toBe('로저, 삼번 세션에 전송합니다');
    // Nothing has left the device during the undo window.
    expect(t.sent).toEqual([]);
    expect(t.last()).toEqual({ phase: 'undo', seconds: 3 });
    await t.timers.advance(UNDO_MS);
    expect(t.sent).toEqual(['어 로그인 버그 고쳐줘']);
    expect(t.flow.state).toBe('waiting');
    expect(t.last()).toEqual({ phase: 'waiting', result: 'sent' });
    expect(t.chimes).toContain('sent');
  });

  it('says so with a low tone when a short call was not the wake word', async () => {
    const t = setup({ a: '모코', b: '점심 뭐 먹지 오늘은 김밥' });
    await t.say('a', 600);
    expect(t.flow.state).toBe('waiting');
    expect(t.chimes).toEqual(['missed']);
    expect(t.last()).toEqual({ phase: 'waiting', result: 'missed' });
    expect(t.spoken).toEqual([]);
    // Longer ordinary speech is not a near miss: no tone.
    await t.say('b', 2500);
    expect(t.chimes).toEqual(['missed']);
  });

  it('does not wake mid-sentence or on ordinary speech', async () => {
    const t = setup({ a: '보컬 트레이닝 앱 만들어', b: '앱에 보코 기능 넣자', c: '점심 뭐 먹지' });
    await t.say('a');
    await t.say('b');
    await t.say('c');
    expect(t.flow.state).toBe('waiting');
    expect(t.spoken).toEqual([]);
    expect(t.box()).toBe('');
    expect(t.calls.every((c) => c.purpose === 'wake')).toBe(true);
  });

  it('only sends the head of a long utterance while waiting (privacy)', async () => {
    const t = setup({ 'head:long': '오늘 회의는 세 시에' });
    await t.say('long', 6000);
    expect(t.calls).toEqual([{ purpose: 'wake', key: 'head:long', bytes: 9 }]);
    expect(t.box()).toBe('');
  });

  it('a wake word that starts a long utterance takes the whole utterance', async () => {
    const t = setup({ 'head:x': '보코 로그인 버그', x: '보코 로그인 버그 고쳐줘 오버' });
    await t.say('x', WAKE_HEAD_MS + 1500);
    expect(t.calls.map((c) => [c.purpose, c.key])).toEqual([
      ['wake', 'head:x'],
      ['dictation', 'x'],
    ]);
    expect(t.box()).toBe('로그인 버그 고쳐줘');
    await t.timers.advance(UNDO_MS);
    expect(t.sent).toEqual(['로그인 버그 고쳐줘']);
  });

  it('goes back to 대기 quietly after 8 s of silence', async () => {
    const t = setup({ a: '보코' });
    await t.say('a');
    await t.timers.advance(IDLE_MS - 100);
    expect(t.flow.state).toBe('awake');
    await t.timers.advance(200);
    expect(t.flow.state).toBe('waiting');
    expect(t.spoken).toEqual(['네']);
    expect(t.sent).toEqual([]);
  });

  it('keeps the text after the timeout when the message did not end like a command', async () => {
    const t = setup({ a: '보코', b: '로그인 화면 말인데' });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(SCHEDULE_SILENCE_MS + 1000);
    expect(t.flow.state).toBe('awake');
    await t.timers.advance(IDLE_MS);
    expect(t.flow.state).toBe('waiting');
    expect(t.box()).toBe('로그인 화면 말인데');
    expect(t.sent).toEqual([]);
  });

  it('keeps text typed before the wake word and sends it together', async () => {
    const t = setup({ a: '보코', b: '테스트도 돌려 줘, 오버' }, { box: '@main.ts' });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(UNDO_MS);
    expect(t.sent).toEqual(['@main.ts 테스트도 돌려 줘']);
  });
});

describe('ending a message', () => {
  it('"…, 전송해" after a pause mark sends after a silence; "파일 전송해" without a pause is content', async () => {
    const t = setup({ a: '보코', b: '파일 전송해', c: '로그 확인해, 전송해' });
    await t.say('a');
    await t.say('b');
    expect(t.flow.state).toBe('awake');
    expect(t.box()).toBe('파일 전송해');
    await t.say('c');
    expect(t.box()).toBe('파일 전송해 로그 확인해');
    expect(t.flow.state).toBe('awake');
    await t.timers.advance(TRAILING_SEND_SILENCE_MS);
    expect(t.flow.state).toBe('undo');
    await t.timers.advance(UNDO_MS);
    expect(t.sent).toEqual(['파일 전송해 로그 확인해']);
  });

  it('a pause in the audio before "전송해" counts as a pause', async () => {
    const t = setup({ a: '보코', b: '파일 전송해' });
    await t.say('a');
    await t.say('b', 1500, true);
    expect(t.box()).toBe('파일');
    await t.timers.advance(TRAILING_SEND_SILENCE_MS);
    expect(t.flow.state).toBe('undo');
  });

  it('"전송해" said on its own sends at once (the user already paused before it)', async () => {
    const t = setup(
      { a: '보코', b: '로그인 버그 고쳐줘', c: '전송해' },
      { settings: { autoSend: false } },
    );
    await t.say('a');
    await t.say('b');
    await t.say('c');
    expect(t.flow.state).toBe('undo');
    await t.timers.advance(UNDO_MS);
    expect(t.sent).toEqual(['로그인 버그 고쳐줘']);
  });

  it('a plain "전송해" followed by more speech was part of the sentence: no send, the words stay', async () => {
    const t = setup({ a: '보코', b: '로그 확인해, 전송해', c: '주는 기능도 만들어 볼까' });
    await t.say('a');
    await t.say('b', 3000, true);
    expect(t.box()).toBe('로그 확인해');
    await t.timers.advance(TRAILING_SEND_SILENCE_MS - 500);
    await t.say('c');
    expect(t.box()).toBe('로그 확인해 전송해 주는 기능도 만들어 볼까');
    await t.timers.advance(IDLE_MS + 1000);
    expect(t.sent).toEqual([]);
    expect(t.flow.state).toBe('waiting');
  });

  it('an imperative ending and silence schedule the send with a countdown', async () => {
    const t = setup({ a: '보코', b: '로그인 버그 고쳐줘' });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(SCHEDULE_SILENCE_MS - 100);
    expect(t.flow.state).toBe('awake');
    await t.timers.advance(100);
    expect(t.flow.state).toBe('countdown');
    expect(t.last()).toEqual({ phase: 'countdown', seconds: 3 });
    expect(t.chimes.filter((c) => c === 'tick')).toHaveLength(1);
    await t.timers.advance(1000);
    expect(t.last()).toEqual({ phase: 'countdown', seconds: 2 });
    await t.timers.advance(2000);
    expect(t.sent).toEqual(['로그인 버그 고쳐줘']);
    expect(t.chimes.filter((c) => c === 'tick')).toHaveLength(3);
    expect(t.spoken).toEqual(['네', '로저, 삼번 세션에 전송했습니다']);
    expect(t.flow.state).toBe('waiting');
  });

  it('speaking during the countdown cancels it and keeps listening', async () => {
    const t = setup({ a: '보코', b: '로그인 버그 고쳐줘', c: '그리고 로그아웃도 확인하고' });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(SCHEDULE_SILENCE_MS + 500);
    expect(t.flow.state).toBe('countdown');
    t.flow.speechStart();
    expect(t.flow.state).toBe('awake');
    await t.timers.advance(5000);
    expect(t.sent).toEqual([]);
    t.flow.speechEnd(t.utterance('c'));
    await t.flow.settled();
    expect(t.box()).toBe('로그인 버그 고쳐줘 그리고 로그아웃도 확인하고');
    // Not a command ending any more: nothing is scheduled.
    await t.timers.advance(SCHEDULE_SILENCE_MS + 1000);
    expect(t.flow.state).toBe('awake');
    expect(t.sent).toEqual([]);
  });

  it('"취소" during the countdown discards the dictation', async () => {
    const t = setup({ a: '보코', b: '로그인 버그 고쳐줘', c: '취소' }, { box: '기존' });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(SCHEDULE_SILENCE_MS + 500);
    expect(t.flow.state).toBe('countdown');
    await t.say('c');
    expect(t.box()).toBe('기존');
    expect(t.flow.state).toBe('waiting');
    expect(t.last()).toEqual({ phase: 'waiting', result: 'cancelled' });
    await t.timers.advance(10_000);
    expect(t.sent).toEqual([]);
    expect(t.spoken).toContain('취소했습니다.');
  });

  it('a tap stops the countdown and keeps the text', async () => {
    const t = setup({ a: '보코', b: '로그인 버그 고쳐줘' });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(SCHEDULE_SILENCE_MS + 500);
    t.flow.tap();
    await t.timers.advance(10_000);
    expect(t.sent).toEqual([]);
    expect(t.box()).toBe('로그인 버그 고쳐줘');
    expect(t.last()).toEqual({ phase: 'waiting', result: 'kept' });
  });

  it('the scheduled send can be turned off', async () => {
    const t = setup({ a: '보코', b: '로그인 버그 고쳐줘' }, { settings: { autoSend: false } });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(SCHEDULE_SILENCE_MS + 3000);
    expect(t.flow.state).toBe('awake');
    expect(t.sent).toEqual([]);
  });

  it('read-back mode asks before sending (the older behaviour)', async () => {
    const t = setup({ a: '보코', b: '테스트 돌려 줘 오버' }, { settings: { confirm: 'readback' } });
    await t.say('a');
    await t.say('b');
    expect(t.readBacks).toEqual(['테스트 돌려 줘']);
    expect(t.sent).toEqual([]);
    expect(t.flow.state).toBe('waiting');
  });

  it('reports a failed send honestly', async () => {
    const t = setup({ a: '보코', b: '테스트 돌려 줘 오버' }, { sendOk: false });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(UNDO_MS);
    expect(t.last()).toEqual({ phase: 'waiting', result: 'kept' });
    expect(t.spoken[t.spoken.length - 1]).toContain('전송에 실패');
  });
});

describe('a long dictation with thinking pauses (2026-11-02 report)', () => {
  /** The message the app sent by itself, cut before the end of the sentence. */
  const CUT =
    '이제 최종 한테 테스트를 여러 기기에서 진행을 할 건데 일단은 지금 보코 너를 불러 본 거고 ' +
    '일단 아까보다는 조금 더 낫긴 하네 일단은 보이스 커맨더 AI한테 해당 내용을';

  it('a message that stops on "…해당 내용을" is never sent, however long the pause', async () => {
    expect(endsWithCommand(CUT)).toBe(false);
    expect(endsOpenKorean(CUT)).toBe(true);
    const t = setup({ a: '보코', b: CUT });
    await t.say('a');
    await t.say('b', 20_300);
    expect(t.box()).toBe(CUT);
    for (let i = 0; i < 6; i += 1) {
      await t.timers.advance(1000);
      expect(t.flow.state).toBe('awake');
    }
    await t.timers.advance(IDLE_MS);
    // Back to Waiting quietly: the text stays in the box, nothing was sent.
    expect(t.flow.state).toBe('waiting');
    expect(t.box()).toBe(CUT);
    expect(t.sent).toEqual([]);
    expect(t.spoken).toEqual(['네']);
  });

  it('a 2.5 s pause does not end the message: the next words are appended', async () => {
    const t = setup({ a: '보코', b: CUT, c: '전달해서 수정하라고 해줘 오버' });
    await t.say('a');
    await t.say('b', 20_300);
    await t.timers.advance(2500);
    expect(t.flow.state).toBe('awake');
    expect(t.sent).toEqual([]);
    await t.say('c');
    expect(t.box()).toBe(`${CUT} 전달해서 수정하라고 해줘`);
    await t.timers.advance(UNDO_MS);
    expect(t.sent).toEqual([`${CUT} 전달해서 수정하라고 해줘`]);
  });

  it('"…해당 내용을, 전송해" (the verb of the sentence) is content, not a send command', async () => {
    const t = setup({ a: '보코', b: `${CUT}, 전송해`, c: '서 결과를 받아 보자' });
    await t.say('a');
    await t.say('b', 20_300, true);
    // Kept as dictated: no "로저", no undo window.
    expect(t.box()).toBe(`${CUT}, 전송해`);
    expect(t.spoken).toEqual(['네']);
    // Like any message ending in a command form it only goes after the
    // cancellable countdown; going on talking cancels it.
    await t.timers.advance(SCHEDULE_SILENCE_MS);
    expect(t.flow.state).toBe('countdown');
    await t.say('c');
    expect(t.flow.state).toBe('awake');
    await t.timers.advance(IDLE_MS);
    expect(t.sent).toEqual([]);
    expect(t.box()).toBe(`${CUT}, 전송해 서 결과를 받아 보자`);
  });

  it('a bare "전송" / "보내" cut by a pause after other words is content', async () => {
    for (const tail of ['전송', '보내']) {
      const t = setup({ a: '보코', b: `${CUT} ${tail}` });
      await t.say('a');
      await t.say('b', 20_300, true);
      await t.timers.advance(TRAILING_SEND_SILENCE_MS + 1000);
      expect(t.flow.state, tail).toBe('awake');
      await t.timers.advance(IDLE_MS);
      expect(t.sent, tail).toEqual([]);
    }
  });

  it('"보코" inside the dictation is content, not a call', async () => {
    const t = setup({
      a: '보코',
      b: '이제 최종 한테 테스트를 여러 기기에서 진행을 할 건데 일단은 지금',
      c: '보코 너를 불러 본 거고 일단 아까보다는 조금 더 낫긴 하네',
    });
    await t.say('a');
    await t.say('b', 6000);
    await t.say('c', 4000);
    expect(t.box()).toBe(
      '이제 최종 한테 테스트를 여러 기기에서 진행을 할 건데 일단은 지금 ' +
        '보코 너를 불러 본 거고 일단 아까보다는 조금 더 낫긴 하네',
    );
    expect(t.spoken).toEqual(['네']);
    expect(t.chimes).toEqual(['wake']);
    await t.timers.advance(SCHEDULE_SILENCE_MS + 1000);
    expect(t.flow.state).toBe('awake');
    await t.timers.advance(IDLE_MS);
    expect(t.sent).toEqual([]);
  });

  it('a call in the middle of a message still works when it stands apart or is a command', async () => {
    const t = setup({ a: '보코', b: '로그인 고쳐', c: '보코, 2번 세션으로' });
    await t.say('a');
    await t.say('b');
    await t.say('c');
    expect(t.selected).toEqual([[2, '로그인 고쳐']]);
    const u = setup({ a: '보코', b: '로그인 고쳐', c: '보코 취소' });
    await u.say('a');
    await u.say('b');
    await u.say('c');
    expect(u.flow.state).toBe('waiting');
    expect(u.box()).toBe('');
  });

  it('endsWithCommand ignores connectives that end in "해"', () => {
    for (const s of ['그 부분에 대해', '사용자를 위해', '이 API를 통해', '그거 이해', '올해']) {
      expect(endsWithCommand(s), s).toBe(false);
    }
    for (const s of ['그 부분에 대해 정리해', '빨리 진행해라', '이것 좀 확인하십시오']) {
      expect(endsWithCommand(s), s).toBe(true);
    }
  });
});

describe('every way to cancel holds until the very end', () => {
  /** "…, 전송해" and silence: the announcement of the send is being spoken. */
  const announcing = async () => {
    const t = setup({ a: '보코', b: '로그 확인해, 전송해', c: '취소' });
    await t.say('a');
    await t.say('b');
    let finishSpeech = () => {};
    t.holds.set('speak', new Promise<void>((resolve) => (finishSpeech = resolve)));
    await t.timers.advance(TRAILING_SEND_SILENCE_MS);
    expect(t.flow.state).toBe('undo');
    return { t, finishSpeech };
  };

  it('"취소" while the send is announced stops it (no countdown yet)', async () => {
    const { t, finishSpeech } = await announcing();
    t.flow.speechStart();
    t.flow.speechEnd(t.utterance('c'));
    await flush();
    await flush();
    finishSpeech();
    await t.flow.settled();
    await t.timers.advance(30_000);
    expect(t.sent).toEqual([]);
    expect(t.flow.state).toBe('waiting');
  });

  it('a tap while the send is announced stops it', async () => {
    const { t, finishSpeech } = await announcing();
    t.flow.tap();
    finishSpeech();
    await t.timers.advance(30_000);
    expect(t.sent).toEqual([]);
    expect(t.box()).toBe('로그 확인해');
  });

  it('a tap during the silence after "…, 전송해" stops it', async () => {
    const t = setup({ a: '보코', b: '로그 확인해, 전송해' });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(TRAILING_SEND_SILENCE_MS - 100);
    t.flow.tap();
    await t.timers.advance(30_000);
    expect(t.sent).toEqual([]);
    expect(t.box()).toBe('로그 확인해');
    expect(t.last()).toEqual({ phase: 'waiting', result: 'kept' });
  });

  it('speech started during the announcement holds the undo window', async () => {
    const { t, finishSpeech } = await announcing();
    t.flow.speechStart();
    finishSpeech();
    await flush();
    await t.timers.advance(UNDO_MS + 100);
    expect(t.sent).toEqual([]);
  });
});

describe('a send on its way', () => {
  const NOT_SENT = '취소했어요. 보내지 않았어요.';

  it('never says nothing was sent while the send is in flight', async () => {
    let finish = () => {};
    const sendHold = new Promise<void>((resolve) => (finish = resolve));
    const t = setup({ a: '보코', b: '테스트 돌려 줘 오버', c: '취소' }, { sendHold });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(UNDO_MS);
    expect(t.sent).toEqual(['테스트 돌려 줘']);
    t.flow.tap();
    await t.say('c');
    expect(t.spoken).not.toContain(NOT_SENT);
    expect(t.spoken[t.spoken.length - 1]).toBe('이미 보내서 취소할 수 없어요.');
    finish();
    await flush();
    expect(t.last()).toEqual({ phase: 'waiting', result: 'sent' });
  });

  it('a cancel stops a send that still waits for its uploads', async () => {
    let finish = () => {};
    const sendHold = new Promise<void>((resolve) => (finish = resolve));
    let canceled = 0;
    const t = setup(
      { a: '보코', b: '테스트 돌려 줘 오버', c: '취소' },
      {
        sendHold,
        sendOk: false,
        queued: () => true,
        cancelQueued: () => {
          canceled += 1;
          finish();
        },
      },
    );
    await t.say('a');
    await t.say('b');
    await t.timers.advance(UNDO_MS);
    expect(t.last()).toEqual({ phase: 'waiting', result: 'queued' });
    await t.say('c');
    await flush();
    expect(canceled).toBe(1);
    expect(t.spoken).toContain(NOT_SENT);
    expect(t.last()).toEqual({ phase: 'waiting', result: 'cancelled' });
    expect(t.spoken).not.toContain('보내지 못했어요.');
  });
});

describe('undo window', () => {
  it('"취소" within 3 s recalls the message (nothing was sent)', async () => {
    const t = setup({ a: '보코', b: '테스트 돌려 줘 오버', c: '취소' });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(1000);
    await t.say('c');
    await t.timers.advance(10_000);
    expect(t.sent).toEqual([]);
    expect(t.spoken[t.spoken.length - 1]).toBe('취소했어요. 보내지 않았어요.');
    expect(t.box()).toBe('테스트 돌려 줘');
    expect(t.last()).toEqual({ phase: 'waiting', result: 'cancelled' });
  });

  it('waits for an utterance that started inside the window', async () => {
    const t = setup({ a: '보코', b: '테스트 돌려 줘 오버', c: '취소' });
    await t.say('a');
    await t.say('b');
    let finish: () => void = () => {};
    t.holds.set('c', new Promise<void>((resolve) => (finish = resolve)));
    await t.timers.advance(2500);
    t.flow.speechStart();
    t.flow.speechEnd(t.utterance('c'));
    await t.timers.advance(1500);
    // The window is over but "취소" is still being transcribed: no send yet.
    expect(t.sent).toEqual([]);
    finish();
    await t.flow.settled();
    await t.timers.advance(10_000);
    expect(t.sent).toEqual([]);
  });

  it('other speech in the window does not stop the send', async () => {
    const t = setup({ a: '보코', b: '테스트 돌려 줘 오버', c: '음 그래' });
    await t.say('a');
    await t.say('b');
    await t.say('c');
    await t.timers.advance(UNDO_MS);
    expect(t.sent).toEqual(['테스트 돌려 줘']);
  });

  it('going on with the message in the window takes it back and keeps listening', async () => {
    const t = setup({ a: '보코', b: '테스트 돌려 줘 오버', c: '그리고 결과는 요약해서 알려 주고' });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(1000);
    await t.say('c');
    expect(t.flow.state).toBe('awake');
    await t.timers.advance(UNDO_MS + HOLD_MAX_MS);
    expect(t.sent).toEqual([]);
    expect(t.box()).toBe('테스트 돌려 줘 그리고 결과는 요약해서 알려 주고');
  });

  it('a held-back "전송해" goes back into the text when the user goes on in the window', async () => {
    const t = setup({ a: '보코', b: '로그 확인해, 전송해', c: '두고 결과를 정리해 두고' });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(TRAILING_SEND_SILENCE_MS);
    expect(t.flow.state).toBe('undo');
    await t.say('c');
    expect(t.flow.state).toBe('awake');
    expect(t.box()).toBe('로그 확인해 전송해 두고 결과를 정리해 두고');
    await t.timers.advance(UNDO_MS + HOLD_MAX_MS);
    expect(t.sent).toEqual([]);
  });

  it('never sends while the user is still speaking after the window', async () => {
    const t = setup({ a: '보코', b: '테스트 돌려 줘 오버', c: '그리고 로그도 봐 주고' });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(2000);
    t.flow.speechStart();
    // A long sentence: well past the window and the old 4 s cap.
    await t.timers.advance(UNDO_MS + HOLD_MAX_MS + 8000);
    expect(t.sent).toEqual([]);
    t.flow.speechEnd(t.utterance('c', 12_000));
    await t.flow.settled();
    await flush();
    await t.timers.advance(5000);
    expect(t.sent).toEqual([]);
    expect(t.box()).toBe('테스트 돌려 줘 그리고 로그도 봐 주고');
  });

  it('"취소" after the send says honestly that it cannot be recalled', async () => {
    const t = setup({ a: '보코', b: '테스트 돌려 줘 오버', c: '취소' });
    await t.say('a');
    await t.say('b');
    await t.timers.advance(UNDO_MS);
    await t.say('c');
    expect(t.sent).toEqual(['테스트 돌려 줘']);
    expect(t.spoken[t.spoken.length - 1]).toBe('이미 보내서 취소할 수 없어요.');
  });
});

describe('commands', () => {
  it('"보코, 3번 세션" switches the target', async () => {
    const t = setup({ a: '보코, 3번 세션' });
    await t.say('a');
    expect(t.selected).toEqual([[3, '']]);
    expect(t.spoken).toEqual(['로저, 삼번 세션']);
  });

  it('a switch takes the text dictated so far along', async () => {
    const t = setup({ a: '보코', b: '로그인 고쳐', c: '보코, 2번 세션으로' }, { box: '기존' });
    await t.say('a');
    await t.say('b');
    await t.say('c');
    expect(t.selected).toEqual([[2, '로그인 고쳐']]);
    expect(t.box()).toBe('기존');
    expect(t.sent).toEqual([]);
  });

  it('an unknown session is said so', async () => {
    const t = setup({ a: '보코, 9번 세션' });
    await t.say('a');
    expect(t.selected).toEqual([]);
    expect(t.spoken).toEqual(['그 세션을 찾지 못했어요.']);
  });

  it('"보코, 현황" plays the briefing', async () => {
    const t = setup({ a: '보코, 현황' });
    await t.say('a');
    expect(t.briefings()).toBe(1);
    expect(t.flow.state).toBe('waiting');
  });

  it('"다시" restarts the dictation and keeps listening', async () => {
    const t = setup({ a: '보코', b: '로그인 고쳐', c: '다시', d: '로그아웃 고쳐줘 오버' });
    await t.say('a');
    await t.say('b');
    await t.say('c');
    expect(t.box()).toBe('');
    expect(t.flow.state).toBe('awake');
    await t.say('d');
    await t.timers.advance(UNDO_MS);
    expect(t.sent).toEqual(['로그아웃 고쳐줘']);
  });

  it('"보코, 취소" discards the dictation', async () => {
    const t = setup({ a: '보코', b: '로그인 고쳐', c: '보코, 취소' });
    await t.say('a');
    await t.say('b');
    await t.say('c');
    expect(t.box()).toBe('');
    expect(t.flow.state).toBe('waiting');
  });

  it('wakes straight into a message after a spoken switch', async () => {
    const t = setup({ a: '테스트 돌려 줘 오버' });
    t.flow.wakeNow('로그인 고쳐');
    expect(t.flow.state).toBe('awake');
    await t.say('a');
    await t.timers.advance(UNDO_MS);
    expect(t.sent).toEqual(['로그인 고쳐 테스트 돌려 줘']);
  });
});

describe('helpers', () => {
  it('endsWithCommand: a small fixed list of command endings', () => {
    for (const s of [
      '로그인 버그 고쳐줘',
      '로그인 버그 고쳐 줘.',
      '테스트 실행해',
      '빌드 돌려',
      '설정 화면 만들어',
      '결과 알려줘',
      '확인해 주세요',
      'Fix the login bug.',
      'Run the tests!',
      'check the logs please',
      'ログインのバグを直してください',
      'arregla el login.',
    ]) {
      expect(endsWithCommand(s), s).toBe(true);
    }
    for (const s of [
      '로그인 화면 말인데',
      '버그가 있는 것 같은데',
      '이거 고쳐줘?',
      'the login bug',
      'Fix the login bug?',
      'I think the build is broken.',
      '',
    ]) {
      expect(endsWithCommand(s), s).toBe(false);
    }
  });

  it('parses session references and spoken numbers', () => {
    expect(parseSessionRef('3번 세션')).toEqual({ number: 3 });
    expect(parseSessionRef('삼번 세션으로')).toEqual({ number: 3 });
    expect(parseSessionRef('12번 세션으로 이동')).toEqual({ number: 12 });
    expect(parseSessionRef('세션 2')).toEqual({ number: 2 });
    expect(parseSessionRef('Session three')).toEqual({ number: 3 });
    expect(parseSessionRef('セッション4')).toEqual({ number: 4 });
    expect(parseSessionRef('sesión 5')).toEqual({ number: 5 });
    expect(parseSessionRef('3번 세션 로그 확인해')).toBeNull();
    expect(parseSpokenNumber('십이')).toBe(12);
    expect(parseSpokenNumber('이십')).toBe(20);
    expect(parseSpokenNumber('삼')).toBe(3);
    expect(parseSpokenNumber('삼삼')).toBeNull();
  });

  it('voiceCommand: only explicit words', () => {
    expect(voiceCommand('취소')).toEqual({ kind: 'cancel' });
    expect(voiceCommand('취소해 줘')).toEqual({ kind: 'cancel' });
    expect(voiceCommand('아니')).toBeNull();
    expect(voiceCommand('다시')).toEqual({ kind: 'again' });
    expect(voiceCommand('현황')).toEqual({ kind: 'briefing' });
    expect(voiceCommand('')).toEqual({ kind: 'empty' });
    expect(voiceCommand('로그인 고쳐')).toBeNull();
  });
});

describe('wake checks on a slow link', () => {
  it('skips stale talk queued behind a slow check but still hears the call', async () => {
    const t = setup({ 'head:a': '점심 뭐 먹지', 'head:b': '오늘 회의는 세 시', c: '보코' });
    let release!: () => void;
    t.holds.set('head:a', new Promise<void>((resolve) => (release = resolve)));
    for (const [key, ms] of [
      ['a', 2500],
      ['b', 2500],
      ['c', 600],
    ] as const) {
      t.flow.speechStart();
      t.flow.speechEnd(t.utterance(key, ms));
    }
    await flush();
    await t.timers.advance(STALE_WAKE_MS + 1000);
    release();
    await t.flow.settled();
    await flush();
    // b waited too long behind a and is only talk: skipped; the call is checked.
    expect(t.calls.map((c) => c.key)).toEqual(['head:a', 'c']);
    expect(t.flow.state).toBe('awake');
  });

  it('checks every utterance when the link keeps up', async () => {
    const t = setup({ 'head:a': '점심 뭐 먹지', 'head:b': '오늘 회의는 세 시' });
    await t.say('a', 2500);
    await t.timers.advance(STALE_WAKE_MS + 1000);
    await t.say('b', 2500);
    expect(t.calls.map((c) => c.key)).toEqual(['head:a', 'head:b']);
  });
});
