import { describe, expect, it } from 'vitest';

import { normalizeKoreanNumbers, sinoKorean } from './koreanNumbers';

describe('sinoKorean', () => {
  it('reads numbers the Sino-Korean way', () => {
    expect(sinoKorean(0)).toBe('영');
    expect(sinoKorean(3)).toBe('삼');
    expect(sinoKorean(10)).toBe('십');
    expect(sinoKorean(12)).toBe('십이');
    expect(sinoKorean(30)).toBe('삼십');
    expect(sinoKorean(101)).toBe('백일');
    expect(sinoKorean(2105)).toBe('이천백오');
  });
});

describe('normalizeKoreanNumbers', () => {
  it('reads a numbered session as 삼번, not 세 번', () => {
    expect(normalizeKoreanNumbers('3번 세션, 결정이 필요합니다', 'ko')).toBe(
      '삼번 세션, 결정이 필요합니다',
    );
    expect(normalizeKoreanNumbers('12번 세션 완료', 'ko-KR')).toBe('십이번 세션 완료');
  });

  it('keeps ordinals with 번째 and leaves other languages alone', () => {
    expect(normalizeKoreanNumbers('3번째 세션', 'ko')).toBe('3번째 세션');
    expect(normalizeKoreanNumbers('3 번째 세션', 'ko')).toBe('3 번째 세션');
    expect(normalizeKoreanNumbers('Session 3번', 'en')).toBe('Session 3번');
    expect(normalizeKoreanNumbers('PNPM 테스트 2개', 'ko')).toBe('PNPM 테스트 2개');
  });
});
