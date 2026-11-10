import { describe, expect, it } from 'vitest';

import { classifySendAnswer, classifySendAnswerRule, confirmSend, type VoiceIO } from './handsFree';
import { captureAnswer } from './handsFreeIO';
import { wavOf } from './vocoListener';

describe('the answer to "보낼까요?"', () => {
  it('accepts every send variant', () => {
    for (const heard of [
      '보내',
      '보낸',
      '보내요',
      '보내줘',
      '보내라',
      '보낼게',
      '보내자',
      '보네',
      '보내기',
      '보내 줘',
      '응',
      '네',
      '예',
      '그래',
      '좋아',
      '오케이',
      '오키',
      '전송',
      '전송해',
      '고고',
      '보내 보내',
      '응 보내',
    ]) {
      expect(classifySendAnswer(heard), heard).toBe('send');
    }
  });

  it('accepts every cancel variant', () => {
    for (const heard of [
      '취소',
      '취소해',
      '취소해 줘',
      '보내지 마',
      '보내지마',
      '안 보내',
      '하지 마',
      '그만',
      '아니',
      '아니야',
      '아니요',
      '노',
      '됐어',
      '취소할게',
      '보내는 거 취소해',
    ]) {
      expect(classifySendAnswer(heard), heard).toBe('cancel');
    }
  });

  it('accepts near-misses one jamo away (fuzzy), and only those', () => {
    for (const heard of ['부내', '보넨', '보낸다', '취쏘', '전숑']) {
      expect(classifySendAnswerRule(heard).rule, heard).toBe('fuzzy');
    }
    expect(classifySendAnswer('부내')).toBe('send');
    expect(classifySendAnswer('보넨')).toBe('send');
    expect(classifySendAnswer('취쏘')).toBe('cancel');
    // Two jamo away from any answer word: not an answer.
    expect(classifySendAnswer('부네요잉')).toBe('unclear');
    expect(classifySendAnswer('모래')).toBe('unclear');
  });

  it('lets an explicit refusal win (negation)', () => {
    expect(classifySendAnswerRule('네 보내지 마')).toEqual({ answer: 'cancel', rule: 'negation' });
    expect(classifySendAnswerRule('보내는 거 취소해').answer).toBe('cancel');
    // Decision: any explicit cancel word makes it a cancel ("when in doubt, nothing is sent").
    expect(classifySendAnswer('보내 아니 취소')).toBe('cancel');
    expect(classifySendAnswer('응 보내지 말고')).toBe('cancel');
  });

  it('asks again on contradictions without an explicit cancel word', () => {
    for (const heard of ['네 아니요', '아니 보내', '응 아니', '응 또는 취소', '보낼까 말까']) {
      expect(classifySendAnswer(heard), heard).toBe('unclear');
    }
  });

  it('labels the rule (logged instead of the words)', () => {
    expect(classifySendAnswerRule('보내').rule).toBe('exact');
    expect(classifySendAnswerRule('부내').rule).toBe('fuzzy');
    expect(classifySendAnswerRule('네 보내지 마').rule).toBe('negation');
    expect(classifySendAnswerRule('글쎄').rule).toBe('none');
  });
});

function scriptedIO(answers: (string | null)[]) {
  const spoken: string[] = [];
  const io: VoiceIO = {
    speak: async (text) => {
      spoken.push(text);
    },
    listen: async () => (answers.length ? answers.shift()! : null),
  };
  return { io, spoken };
}

describe('two-choice re-ask', () => {
  it('asks "보내기 또는 취소" when speech came but no words', async () => {
    const { io, spoken } = scriptedIO(['', '보내']);
    const result = await confirmSend({
      text: 'run the tests',
      lang: 'ko',
      io,
      signal: new AbortController().signal,
      onStatus: () => {},
    });
    expect(result).toEqual({ outcome: 'send' });
    expect(spoken[1]).toBe('보내기 또는 취소로 말씀해 주세요.');
  });

  it('captureAnswer reports speech without text as an empty answer', async () => {
    const pcm = (level: number, frames: number) => {
      const out = new Uint8Array(frames * 640);
      const v = new DataView(out.buffer);
      for (let i = 0; i < frames * 320; i += 1)
        v.setInt16(i * 2, Math.round((i % 2 ? 1 : -1) * level * 32767), true);
      return out;
    };
    // 0.6 s of soft speech (0.01, under the old fixed level) after 1 s of room noise.
    const audio = new Uint8Array([...pcm(0.002, 50), ...pcm(0.01, 30), ...pcm(0.002, 60)]);
    let clock = 0;
    const blob = new Blob([wavOf(audio) as BlobPart], { type: 'audio/wav' });
    const heard = await captureAnswer({
      recorder: {
        start: async () => {},
        stop: async () => blob,
        cancel: () => {},
        peek: async () => blob,
      },
      transcribe: async () => ({ text: '', corrections: [] }),
      signal: new AbortController().signal,
      sleep: async (ms) => {
        clock += ms;
      },
      now: () => clock,
    });
    expect(heard).toBe('');
  });
});
