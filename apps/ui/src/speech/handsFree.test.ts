import { describe, expect, it, vi } from 'vitest';

import type { DangerLevel, Decision } from '../protocol/types';
import { voiceQueue } from './voiceQueue';
import {
  AGAIN_WORDS,
  ALLOW_WORDS,
  CANCEL_WORDS,
  DENY_WORDS,
  SEND_WORDS,
  classifyDecisionAnswer,
  classifySendAnswer,
  confirmSend,
  decisionAskText,
  READ_BACK_CHARS,
  readBackExcerpt,
  decisionScreenText,
  excerpt,
  interruptOnTap,
  normalizeAnswer,
  readBackText,
  SEND_RETRIES,
  sameDecision,
  spokenDecision,
  subjectParticle,
  voiceChoices,
  voiceLang,
  setDictationActive,
  withVoice,
  type DecisionVoiceStatus,
  type SendStatus,
  type VoiceIO,
  type VoiceLang,
} from './handsFree';

/** A scripted VoiceIO: `answers` are returned by successive `listen` calls. */
function fakeIO(answers: (string | null)[]) {
  const spoken: string[] = [];
  const queue = [...answers];
  const io: VoiceIO & { spoken: string[]; listens: number } = {
    spoken,
    listens: 0,
    speak: vi.fn(async (text: string) => {
      spoken.push(text);
    }),
    listen: vi.fn(async () => {
      io.listens += 1;
      return queue.length > 0 ? queue.shift()! : null;
    }),
  };
  return io;
}

const LANGS: VoiceLang[] = ['ko', 'en', 'ja', 'es'];

describe('vocabulary', () => {
  it('normalizes case, width, punctuation, spaces and katakana', () => {
    expect(normalizeAnswer(' Yes. ')).toBe('yes');
    expect(normalizeAnswer('보내 줘!')).toBe('보내줘');
    expect(normalizeAnswer('ハイ。')).toBe('はい');
    expect(normalizeAnswer('ＳＥＮＤ')).toBe('send');
    expect(normalizeAnswer('¡Sí!')).toBe('sí');
  });

  it.each(LANGS)('accepts the %s send, cancel and again words', (lang) => {
    for (const w of SEND_WORDS[lang]) expect(classifySendAnswer(w)).toBe('send');
    for (const w of CANCEL_WORDS[lang]) expect(classifySendAnswer(w)).toBe('cancel');
    for (const w of AGAIN_WORDS[lang]) expect(classifySendAnswer(w)).toBe('again');
  });

  it('has the required words of each language', () => {
    expect(SEND_WORDS.ko).toEqual(['응', '네', '예', '보내', '보내줘', '전송', '그래', '좋아']);
    expect(SEND_WORDS.en).toEqual(['yes', 'send']);
    expect(SEND_WORDS.ja).toEqual(['はい', '送信']);
    expect(SEND_WORDS.es).toContain('sí');
    expect(SEND_WORDS.es).toContain('enviar');
    expect(CANCEL_WORDS.ko).toEqual(expect.arrayContaining(['취소', '아니', '아니요', '하지 마']));
    expect(CANCEL_WORDS.en).toEqual(['no', 'cancel']);
    expect(CANCEL_WORDS.ja).toContain('いいえ');
    expect(CANCEL_WORDS.es).toContain('no');
    expect(AGAIN_WORDS.ko).toEqual(['다시']);
  });

  it('matches whole utterances of one answer only', () => {
    for (const heard of [
      '',
      '   ',
      '음',
      'please',
      '네 아니요',
      'yes no',
      'maybe',
      '보낼까요',
      '보낼까 말까',
      '안녕하세요',
      '감사합니다',
      '다음 단계로 가자',
      '送信しない',
      'no lo sé',
    ]) {
      expect(classifySendAnswer(heard), heard).toBe('unclear');
    }
    expect(classifySendAnswer('네.')).toBe('send');
    expect(classifySendAnswer('Send!')).toBe('send');
  });

  it('accepts send words with fillers, courtesy endings and punctuation', () => {
    for (const heard of [
      '보내',
      '보내.',
      '보내!',
      ' 보내 ',
      '어 보내',
      '어, 보내.',
      '음 보내줘',
      '네 보내주세요',
      '네, 보내 주세요.',
      '보내줘요.',
      '보내요',
      '보내세요',
      '네보내주세요',
      '그래',
      '그래요',
      '좋아',
      '좋아요',
      '전송해',
      '전송해 주세요',
      '응응',
      '보네',
      'yes please',
      'send it',
      'um, yes.',
      'Yes, send it now.',
      'はい、送信してください',
      'えっと、はい',
      'はい送信',
      'sí, por favor',
      'eh, enviar',
      'Sí, envíalo.',
    ]) {
      expect(classifySendAnswer(heard), heard).toBe('send');
    }
  });

  it('cancels only on explicit cancel words', () => {
    for (const heard of [
      '취소',
      '취소.',
      '취소해 줘',
      '아니',
      '아니요',
      '아니야',
      '어 아니',
      '하지 마',
      '하지 마세요',
      '보내지 마',
      'no',
      'No.',
      'cancel',
      'uh, cancel it',
      'いいえ',
      'キャンセル',
      'キャンセルしてください',
      'cancelar',
      'no, por favor',
    ]) {
      expect(classifySendAnswer(heard), heard).toBe('cancel');
    }
  });

  it.each(LANGS)('accepts the %s allow and deny words for decisions', (lang) => {
    for (const w of ALLOW_WORDS[lang]) expect(classifyDecisionAnswer(w)).toBe('allow');
    for (const w of DENY_WORDS[lang]) expect(classifyDecisionAnswer(w)).toBe('deny');
  });

  it('never takes "yes" or "응" as an approval', () => {
    for (const heard of ['yes', '응', '네', 'はい', 'sí', 'send', '허용해 줘', 'allow it']) {
      expect(classifyDecisionAnswer(heard)).not.toBe('allow');
    }
    expect(classifyDecisionAnswer('취소')).toBe('stop');
    expect(classifyDecisionAnswer('no')).toBe('stop');
  });

  it('picks the prompt language from the UI language', () => {
    expect(voiceLang('ko')).toBe('ko');
    expect(voiceLang('ja-JP')).toBe('ja');
    expect(voiceLang('es')).toBe('es');
    expect(voiceLang('de')).toBe('en');
  });
});

describe('read-back text', () => {
  it('names the target session (Korean number reading) and reads short text', () => {
    expect(readBackText('테스트 돌려 줘', 'ko', { sessionName: '3번 세션' })).toBe(
      '삼번 세션에 테스트 돌려 줘. 보낼까요?',
    );
    expect(readBackText('run the tests.', 'en', { sessionName: 'api' })).toBe(
      'To api: run the tests. Send it?',
    );
    expect(readBackText('テスト', 'ja', { sessionName: 'api' })).toBe(
      'apiに、テスト。送信しますか？',
    );
    expect(readBackText('prueba', 'es', { sessionName: 'api' })).toBe('A api: prueba. ¿Lo envío?');
  });

  it('reads only about the first 8 words, then "…"', () => {
    const text = '로그인 화면의 버튼 색을 파란색으로 바꾸고 테스트를 다시 돌린 다음 결과를 알려 줘';
    expect(readBackText(text, 'ko', { sessionName: '3번 세션' })).toBe(
      '삼번 세션에 로그인 화면의 버튼 색을 파란색으로 바꾸고 테스트를 다시… 보낼까요?',
    );
    expect(readBackExcerpt('one two three four five six seven eight nine ten')).toBe(
      'one two three four five six seven eight…',
    );
    expect(readBackExcerpt('one two three four five six seven eight.')).toBe(
      'one two three four five six seven eight',
    );
  });

  it('reads at most 40 characters, cut at a word when possible', () => {
    const long = 'internationalization localization globalization accessibility';
    const head = readBackExcerpt(long);
    expect(head).toBe('internationalization localization…');
    expect(Array.from(head).length).toBeLessThanOrEqual(READ_BACK_CHARS + 1);
    // No spaces (Japanese): a hard cut at 40 characters.
    expect(readBackExcerpt('あ'.repeat(100))).toBe(`${'あ'.repeat(40)}…`);
    expect(readBackText('가'.repeat(200), 'ko', { sessionName: '12번 세션' })).toBe(
      `십이번 세션에 ${'가'.repeat(40)}… 보낼까요?`,
    );
    expect(readBackText('Fix it, then run the build and the tests please now', 'en')).toBe(
      'Message: Fix it, then run the build and the… Send it?',
    );
  });

  it('reads the whole text when the full read-back setting is on', () => {
    const text =
      '로그인 화면의 버튼 색을 파란색으로 바꾸고 테스트를 다시 돌린 다음 결과를 알려 줘.';
    expect(readBackText(text, 'ko', { sessionName: '3번 세션', full: true })).toBe(
      `삼번 세션에 ${text.slice(0, -1)}. 보낼까요?`,
    );
  });

  it('reads short text in full', () => {
    expect(readBackText('테스트 돌려 줘', 'ko')).toBe('보낼 내용: 테스트 돌려 줘. 보낼까요?');
    expect(readBackText('run the tests.', 'en')).toBe('Message: run the tests. Send it?');
    expect(readBackText('テスト', 'ja')).toBe('送信する内容: テスト。送信しますか？');
    expect(readBackText('prueba', 'es')).toBe('Mensaje: prueba. ¿Lo envío?');
  });

  it('cuts long commands of decisions at 120 characters', () => {
    expect(excerpt('가'.repeat(200))).toBe(`${'가'.repeat(120)}…`);
    expect(excerpt('a\n b   c')).toBe('a b c');
  });
});

describe('Korean decision text', () => {
  it('reads the session number in Sino-Korean with the right particle', () => {
    expect(decisionAskText('5번 세션', 'pnpm build', 'ko')).toBe(
      '오번 세션이 pnpm build 실행을 허용해 달라고 합니다. 허용할까요?',
    );
    expect(decisionAskText('Claude 2', 'ls', 'ko')).toBe(
      'Claude 2가 ls 실행을 허용해 달라고 합니다. 허용할까요?',
    );
    expect(decisionAskText('api', 'ls', 'en')).toBe('api asks to run ls. Allow it?');
    expect(decisionScreenText('3번 세션', 'ko')).toBe(
      '삼번 세션에 결정이 필요합니다. 화면에서 확인해 주세요.',
    );
  });

  it('chooses 이 or 가', () => {
    expect(subjectParticle('세션')).toBe('이');
    expect(subjectParticle('서버')).toBe('가');
    expect(subjectParticle('payments-api')).toBe('가');
    expect(subjectParticle('build')).toBe('이');
  });
});

describe('confirmSend', () => {
  const run = (
    answers: (string | null)[],
    signal = new AbortController().signal,
    sessionName: string | null = null,
  ) => {
    const io = fakeIO(answers);
    const statuses: SendStatus[] = [];
    const done = confirmSend({
      text: 'run the tests',
      lang: 'ko',
      sessionName,
      io,
      signal,
      onStatus: (s) => statuses.push(s),
    });
    return { io, statuses, done };
  };

  it('sends on yes', async () => {
    const { io, statuses, done } = run(['네']);
    expect(await done).toEqual({ outcome: 'send' });
    expect(io.spoken).toEqual(['보낼 내용: run the tests. 보낼까요?']);
    expect(statuses).toEqual(['confirming', 'listening']);
  });

  it('cancels on no', async () => {
    expect(await run(['아니']).done).toEqual({ outcome: 'cancel', reason: 'answer' });
    expect(await run(['cancel']).done).toEqual({ outcome: 'cancel', reason: 'answer' });
  });

  it('dictates again on 다시', async () => {
    expect(await run(['다시']).done).toEqual({ outcome: 'again' });
  });

  it('names the session in the question', async () => {
    const { io, done } = run(['보내'], undefined, '3번 세션');
    expect(await done).toEqual({ outcome: 'send' });
    expect(io.spoken).toEqual(['삼번 세션에 run the tests. 보낼까요?']);
  });

  // Regression: a user said "보내" and the text was cancelled. Silence (an
  // answer given while the long read-back was still playing, before the
  // microphone opened) was a cancel at once, and anything but an exact word
  // ("보내요", "어 보내") cancelled after one re-ask.
  it('sends on "보내" after silence instead of cancelling', async () => {
    const { io, done } = run([null, '보내']);
    expect(await done).toEqual({ outcome: 'send' });
    expect(io.listens).toBe(2);
    expect(io.spoken[1]).toBe('보낼까요? 응 또는 취소로 답해 주세요.');
  });

  it.each(['보내', '보내.', '어 보내', '네 보내주세요', '보내줘요.', '보네'])(
    'sends on "%s"',
    async (heard) => {
      expect(await run([heard]).done).toEqual({ outcome: 'send' });
    },
  );

  it.each([
    '어 그래 보내',
    '어 그래 보내.',
    '어그래보내',
    '그래 보내',
    '어 그래 보네',
    '그래 보네요',
    '어, 그래. 보내.',
    '어 그래 보냅시다',
    '어 그래, 보내봐',
    '어 맞아 보내',
  ])('sends on the variant "%s"', async (heard) => {
    expect(await run([heard]).done).toEqual({ outcome: 'send' });
  });

  // The cancel outcome needs a cancel word in every ordering of events.
  it('never cancels without a cancel word, whatever the silences and noise', async () => {
    const pool: (string | null)[] = [null, '음', '뭐라고', '보낼까 말까', '응 또는 취소', '네'];
    for (let seed = 0; seed < 60; seed += 1) {
      const answers = [0, 1, 2].map((i) => pool[(seed * 7 + i * 3 + (seed >> 2)) % pool.length]!);
      const result = await run(answers).done;
      const hasCancel = answers.slice(0, 3).some((a) => a === '취소');
      expect(result.outcome === 'cancel').toBe(hasCancel);
    }
  });

  it('asks again on an unclear answer, then accepts a clear one', async () => {
    const { io, done } = run(['음 글쎄', 'yes']);
    expect(await done).toEqual({ outcome: 'send' });
    expect(io.spoken).toHaveLength(2);
    expect(io.spoken[1]).toContain('보낼까요?');
  });

  it('asks up to twice more, then keeps the text unsent (never a cancel)', async () => {
    const { io, done } = run(['뭐라고', '보낼까 말까', '글쎄']);
    expect(await done).toEqual({ outcome: 'kept', reason: 'unclear' });
    expect(io.listens).toBe(SEND_RETRIES + 1);
    expect(io.spoken).toEqual([
      '보낼 내용: run the tests. 보낼까요?',
      '보낼까요? 응 또는 취소로 답해 주세요.',
      '보낼까요? 응 또는 취소로 답해 주세요.',
    ]);
  });

  it('keeps the text after silence on every ask (never a cancel)', async () => {
    const { io, done } = run([null, null, null]);
    expect(await done).toEqual({ outcome: 'kept', reason: 'timeout' });
    expect(io.listens).toBe(SEND_RETRIES + 1);
  });

  it('accepts a send word on the last re-ask', async () => {
    expect(await run([null, '뭐', '응']).done).toEqual({ outcome: 'send' });
  });

  it('cancels on an explicit cancel word after a re-ask', async () => {
    expect(await run([null, '취소']).done).toEqual({ outcome: 'cancel', reason: 'answer' });
  });

  it('stops at once when interrupted while reading back', async () => {
    const controller = new AbortController();
    const io = fakeIO(['네']);
    io.speak = vi.fn(async () => controller.abort());
    const result = await confirmSend({
      text: 'x',
      lang: 'en',
      io,
      signal: controller.signal,
      onStatus: () => {},
    });
    expect(result).toEqual({ outcome: 'cancel', reason: 'interrupted' });
    expect(io.listen).not.toHaveBeenCalled();
  });
});

const yesNo = (prompt = 'Allow Bash command?\npnpm build'): Decision => ({
  id: 'd1',
  prompt,
  // Like the server: the command comes from the hook, next to the text.
  ...(prompt.includes('\n') ? { command: prompt.split('\n').slice(1).join('\n') } : {}),
  options: [
    { id: 'yes', label: 'Yes' },
    { id: 'always', label: "Yes, and don't ask again for this command" },
    { id: 'no', label: 'No, tell the agent what to do instead' },
  ],
});

describe('voiceChoices', () => {
  it('picks the plain yes and no, never "don\'t ask again"', () => {
    expect(voiceChoices(yesNo())).toEqual({ allow: 'yes', deny: 'no' });
  });

  it('takes one-time approvals as the plain yes, never "always" or "for the session"', () => {
    const once = (label: string): Decision => ({
      id: 'o',
      prompt: 'Allow Bash command?',
      options: [
        { id: 'once', label },
        { id: 'session', label: 'Yes, allow for this session' },
        { id: 'always', label: 'Always allow' },
        { id: 'no', label: 'No' },
      ],
    });
    for (const label of [
      'Allow once',
      'Yes, once',
      '이번만 허용',
      '今回のみ許可',
      'Permitir una vez',
    ]) {
      expect(voiceChoices(once(label))).toEqual({ allow: 'once', deny: 'no' });
    }
    expect(
      voiceChoices({
        id: 'x',
        prompt: 'p',
        options: [
          { id: 'session', label: 'Yes, allow for this session' },
          { id: 'no', label: 'No' },
        ],
      }),
    ).toBeNull();
  });

  it('refuses decisions without plain yes / no options', () => {
    const multi: Decision = {
      id: 'q',
      prompt: 'Which database?',
      options: [
        { id: 'a', label: 'Postgres' },
        { id: 'b', label: 'SQLite' },
        { id: 'c', label: 'MySQL' },
      ],
    };
    expect(voiceChoices(multi)).toBeNull();
    expect(
      voiceChoices({ id: 'x', prompt: 'p', options: [{ id: 'always', label: 'Always allow' }] }),
    ).toBeNull();
    expect(
      voiceChoices({
        id: 'x',
        prompt: 'p',
        options: [
          { id: 'y1', label: 'Yes' },
          { id: 'y2', label: 'yes' },
          { id: 'n', label: 'No' },
        ],
      }),
    ).toBeNull();
  });

  it('compares decisions by id, question and options', () => {
    expect(sameDecision(yesNo(), yesNo())).toBe(true);
    expect(sameDecision(yesNo(), { ...yesNo(), id: 'd2' })).toBe(false);
    expect(sameDecision(yesNo(), yesNo('Allow Bash command?\nrm -rf /'))).toBe(false);
    expect(sameDecision(yesNo(), undefined)).toBe(false);
  });
});

describe('spokenDecision', () => {
  function setup(
    answers: (string | null)[],
    {
      level = 'safe',
      decision = yesNo(),
      current,
    }: {
      level?: DangerLevel | Error;
      decision?: Decision;
      current?: () => Decision | undefined;
    } = {},
  ) {
    const io = fakeIO(answers);
    const statuses: DecisionVoiceStatus[] = [];
    const choose = vi.fn(async () => {});
    const classify = vi.fn(async () => {
      if (level instanceof Error) throw level;
      return level;
    });
    const done = spokenDecision({
      decision,
      sessionName: '5번 세션',
      lang: 'ko',
      io,
      signal: new AbortController().signal,
      classify,
      current: current ?? (() => decision),
      choose,
      onStatus: (s) => statuses.push(s),
    });
    return { io, statuses, choose, classify, done };
  }

  it('approves a safe request by voice with the plain yes option', async () => {
    const { io, choose, classify, statuses, done } = setup(['허용']);
    expect(await done).toEqual({ result: 'approved', optionId: 'yes' });
    expect(classify).toHaveBeenCalledWith('pnpm build');
    expect(io.spoken[0]).toBe('오번 세션이 pnpm build 실행을 허용해 달라고 합니다. 허용할까요?');
    expect(choose).toHaveBeenCalledExactlyOnceWith('yes');
    expect(statuses).toEqual(['checking', 'reading', 'listening', 'approved']);
  });

  it('denies a safe request by voice with the plain no option', async () => {
    const { choose, done } = setup(['거절']);
    expect(await done).toEqual({ result: 'denied', optionId: 'no' });
    expect(choose).toHaveBeenCalledExactlyOnceWith('no');
  });

  it.each(['en', 'ja', 'es'] as const)('takes the %s allow word', async (lang) => {
    const { choose, done } = setup([ALLOW_WORDS[lang][0]]);
    expect((await done).result).toBe('approved');
    expect(choose).toHaveBeenCalledWith('yes');
  });

  it.each(['caution', 'dangerous'] as const)(
    'never takes a voice answer for %s commands',
    async (level) => {
      const { io, choose, statuses, done } = setup(['허용'], { level });
      expect(await done).toEqual({ result: 'screen', reason: level });
      expect(io.listen).not.toHaveBeenCalled();
      expect(choose).not.toHaveBeenCalled();
      // The command itself is not read out; the screen is asked for.
      expect(io.spoken).toEqual(['오번 세션에 결정이 필요합니다. 화면에서 확인해 주세요.']);
      expect(statuses).toContain('screen');
    },
  );

  it('sends the request to the screen when classification fails', async () => {
    const { io, choose, done } = setup(['허용'], { level: new Error('offline') });
    expect(await done).toEqual({ result: 'screen', reason: 'unclassified' });
    expect(io.listen).not.toHaveBeenCalled();
    expect(choose).not.toHaveBeenCalled();
  });

  it('refuses multi-option decisions and decisions without a command', async () => {
    const multi: Decision = {
      id: 'm',
      prompt: 'Run command?\nls',
      command: 'ls',
      options: [
        { id: 'a', label: 'Once' },
        { id: 'b', label: 'Always' },
        { id: 'c', label: 'Never' },
      ],
    };
    const a = setup(['허용'], { decision: multi });
    expect(await a.done).toEqual({ result: 'screen', reason: 'options' });
    expect(a.classify).not.toHaveBeenCalled();
    expect(a.choose).not.toHaveBeenCalled();
    const b = setup(['허용'], { decision: { ...yesNo('Edit src/main.rs?'), id: 'e' } });
    expect(await b.done).toEqual({ result: 'screen', reason: 'no_command' });
    expect(b.choose).not.toHaveBeenCalled();
  });

  it('never takes the command from the text on the screen', async () => {
    // A prompt can say anything; only what the hook reported counts.
    const displayOnly = yesNo('Allow Bash command?\nls');
    delete displayOnly.command;
    const { choose, classify, done } = setup(['허용'], { decision: displayOnly });
    expect(await done).toEqual({ result: 'screen', reason: 'no_command' });
    expect(classify).not.toHaveBeenCalled();
    expect(choose).not.toHaveBeenCalled();
  });

  it('does not act on a decision that changed while listening', async () => {
    let now: Decision | undefined = yesNo();
    const io = fakeIO([]);
    io.listen = vi.fn(async () => {
      now = { ...yesNo('Allow Bash command?\nrm -rf dist'), id: 'd1' };
      return '허용';
    });
    const choose = vi.fn(async () => {});
    const result = await spokenDecision({
      decision: yesNo(),
      sessionName: 'api',
      lang: 'en',
      io,
      signal: new AbortController().signal,
      classify: async () => 'safe',
      current: () => now,
      choose,
      onStatus: () => {},
    });
    expect(result).toEqual({ result: 'changed' });
    expect(choose).not.toHaveBeenCalled();
  });

  it('does not act on a decision that disappeared while listening', async () => {
    let gone = false;
    const { choose, done, io } = setup([], { current: () => (gone ? undefined : yesNo()) });
    io.listen = vi.fn(async () => {
      gone = true;
      return '허용';
    });
    expect(await done).toEqual({ result: 'changed' });
    expect(choose).not.toHaveBeenCalled();
  });

  it('asks again once, then stops on unclear answers', async () => {
    const { io, choose, done } = setup(['응', 'yes']);
    expect(await done).toEqual({ result: 'stopped', reason: 'unclear' });
    expect(io.listens).toBe(2);
    expect(io.spoken[1]).toBe('허용 또는 거절로 답해 주세요. 허용할까요?');
    expect(choose).not.toHaveBeenCalled();
  });

  it('stops on silence and on a cancel word without answering', async () => {
    const a = setup([null]);
    expect(await a.done).toEqual({ result: 'stopped', reason: 'timeout' });
    const b = setup(['취소']);
    expect(await b.done).toEqual({ result: 'stopped', reason: 'answer' });
    expect(a.choose).not.toHaveBeenCalled();
    expect(b.choose).not.toHaveBeenCalled();
  });
});

describe('voice during a dictation', () => {
  const alert = (played: string[], key: string) => ({
    key,
    priority: 'decision' as const,
    play: async () => void played.push(key),
    stop: () => {},
  });
  const settle = async () => {
    for (let i = 0; i < 50; i += 1) await Promise.resolve();
  };

  it('waits until the dictation ends, then plays without losing anything', async () => {
    const played: string[] = [];
    setDictationActive(true);
    setDictationActive(true); // the composer says it twice: one hold, one release
    voiceQueue.enqueue(alert(played, 'held'));
    await settle();
    expect(played).toEqual([]);
    setDictationActive(false);
    await settle();
    expect(played).toEqual(['held']);
  });

  it('a hands-free flow during a dictation does not wait for it or release it', async () => {
    const played: string[] = [];
    setDictationActive(true);
    voiceQueue.enqueue(alert(played, 'held-too'));
    await expect(withVoice(async () => 'done')).resolves.toBe('done');
    await settle();
    expect(played).toEqual([]);
    setDictationActive(false);
    await settle();
    expect(played).toEqual(['held-too']);
  });
});

describe('sharing the microphone', () => {
  it('runs voice flows one at a time', async () => {
    const order: string[] = [];
    let release!: () => void;
    const first = withVoice(
      () =>
        new Promise<void>((resolve) => {
          order.push('first start');
          release = () => {
            order.push('first end');
            resolve();
          };
        }),
    );
    const second = withVoice(async () => {
      order.push('second');
    });
    await Promise.resolve();
    release();
    await Promise.all([first, second]);
    expect(order).toEqual(['first start', 'first end', 'second']);
  });

  it('stops a flow on a tap or a key press', () => {
    const target = new EventTarget();
    const a = new AbortController();
    const cleanup = interruptOnTap(a, target);
    target.dispatchEvent(new Event('pointerdown'));
    expect(a.signal.aborted).toBe(true);
    cleanup();
    const b = new AbortController();
    interruptOnTap(b, target);
    target.dispatchEvent(new Event('keydown'));
    expect(b.signal.aborted).toBe(true);
  });
});
