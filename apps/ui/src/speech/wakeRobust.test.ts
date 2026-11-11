import { describe, expect, it } from 'vitest';

import { collapseStretch, jamoDistance, toJamo } from './jamo';
import { isNearWake, matchWakeWord, NEAR_WAKE_MAX_MS } from './wakeWord';

const wakes = (text: string, speechMs?: number) => matchWakeWord(text, { speechMs }) !== null;

describe('jamo helpers', () => {
  it('splits syllables and measures jamo distance', () => {
    expect(toJamo('보코')).toEqual(['ㅂ', 'ㅗ', 'ㅋ', 'ㅗ']);
    expect(toJamo('컬')).toEqual(['ㅋ', 'ㅓ', 'ㄹ']);
    expect(jamoDistance('보네', '보내')).toBe(1);
    expect(jamoDistance('보낸', '보내')).toBe(1);
    expect(jamoDistance('부내', '보내')).toBe(1);
    expect(jamoDistance('보코', '보코')).toBe(0);
  });

  it('collapses stretched calls', () => {
    expect(collapseStretch('보코오~')).toBe('보코');
    expect(collapseStretch('보오코')).toBe('보코');
    expect(collapseStretch('보코오오')).toBe('보코');
    expect(collapseStretch('보코야')).toBe('보코야');
    expect(collapseStretch('보고서')).toBe('보고서');
  });
});

describe('wake word: how people call it', () => {
  it('accepts the aliases recognizers write for a quick "보코"', () => {
    for (const w of [
      '보코',
      '보꼬',
      '뽀코',
      '보크',
      '보커',
      '보코요',
      '보코야',
      '보콜',
      '버코',
      '부코',
      '뽀꼬',
      '보꾸',
      '보코?',
      'Voco',
      'Boko',
      'Bocco',
      'Voko',
      'Boco',
    ]) {
      expect(wakes(`${w}, 테스트 돌려 줘`), w).toBe(true);
    }
  });

  it('accepts the risky real words only on their own or before a pause', () => {
    for (const w of ['보고', '포코', '보구', '보컬', '보컬아', 'Vocal', 'Poco']) {
      expect(wakes(w), w).toBe(true);
      expect(wakes(`${w}, 테스트 돌려 줘`), `${w},`).toBe(true);
      expect(wakes(`${w} 테스트 돌려 줘`), `${w} …`).toBe(false);
    }
    expect(wakes('보고서 써줘')).toBe(false);
    expect(wakes('보고 싶어')).toBe(false);
    expect(wakes('포코 아파트 분양 정보')).toBe(false);
    expect(wakes('poco a poco')).toBe(false);
  });

  it('accepts stretched calls', () => {
    for (const t of [
      '보코오~',
      '보오코',
      '보코오오',
      '보코~ 테스트 돌려 줘',
      '보코오오, 테스트 돌려 줘',
    ]) {
      expect(wakes(t), t).toBe(true);
    }
    expect(matchWakeWord('보코오~ 테스트 돌려 줘')!.rest).toBe('테스트 돌려 줘');
  });

  it('treats repeated calls as one wake', () => {
    expect(matchWakeWord('보코 보코')!.rest).toBe('');
    expect(matchWakeWord('보코, 보코')!.rest).toBe('');
    expect(matchWakeWord('보코, 보코, 테스트 돌려 줘')!.rest).toBe('테스트 돌려 줘');
    expect(matchWakeWord('Voco voco run the tests')!.rest).toBe('run the tests');
    expect(wakes('보코보코')).toBe(true);
    expect(matchWakeWord('보코보코')!.rest).toBe('');
    // As content ("…got beaten up") it never wakes.
    expect(wakes('보코보코 맞았어')).toBe(false);
    // Japanese 「ボコボコ」 stays a different word.
    expect(wakes('ボコボコにされた')).toBe(false);
  });

  it('joins a call the recognizer split into syllables', () => {
    expect(matchWakeWord('보 코')!.heard).toBe('보 코');
    expect(matchWakeWord('보, 코, 테스트 돌려 줘')!.rest).toBe('테스트 돌려 줘');
    expect(wakes('보 고')).toBe(true);
    expect(wakes('보 고 싶어')).toBe(false);
  });

  it('accepts affectionate and vocative forms, not particles', () => {
    for (const t of [
      '보코야',
      '보코아',
      '보코씨',
      '보코님',
      '보코 군',
      '보코양',
      '우리 보코',
      '보코짱',
      '헤이 보코',
      'hey Voco',
      '오케이 보코',
    ]) {
      expect(wakes(t), t).toBe(true);
    }
    expect(matchWakeWord('보코 군, 테스트 돌려 줘')!.rest).toBe('테스트 돌려 줘');
    expect(matchWakeWord('우리 보코, 테스트 돌려 줘')!.rest).toBe('테스트 돌려 줘');
    for (const t of ['보코가 이상해', '보코의 기능', '보코를 고쳐줘']) {
      expect(wakes(t), t).toBe(false);
    }
  });

  it('accepts a jamo near-hearing only alone and short', () => {
    expect(isNearWake('포코')).toBe(true);
    expect(isNearWake('볼코')).toBe(true);
    expect(isNearWake('부꼬')).toBe(false);
    expect(isNearWake('퍼크')).toBe(false);
    expect(isNearWake('모코')).toBe(false);
    expect(isNearWake('보호')).toBe(false);
    // "볼코" is in no list: only as a short utterance on its own.
    expect(wakes('볼코', 600)).toBe(true);
    expect(wakes('볼코', NEAR_WAKE_MAX_MS + 1)).toBe(false);
    expect(wakes('볼코')).toBe(false);
    expect(wakes('볼코 테스트 돌려 줘', 600)).toBe(false);
  });
});
