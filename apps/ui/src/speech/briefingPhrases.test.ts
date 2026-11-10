import { describe, expect, it } from 'vitest';

import { isBriefingPhrase, normalizePhrase } from './briefingPhrases';

describe('briefing phrases', () => {
  it('match whole phrases in all four languages, ignoring case, spaces and punctuation', () => {
    for (const text of [
      '현황 알려줘',
      '현황 알려 줘.',
      '상태알려줘!',
      'Status',
      'status.',
      "What's going on?",
      'whats going on',
      'Give me the status',
      '状況を教えて。',
      '¿Qué está pasando?',
      'Estado',
    ]) {
      expect(isBriefingPhrase(text), text).toBe(true);
    }
  });

  it('never take a real prompt that only contains a phrase', () => {
    for (const text of [
      '',
      '   ',
      'status of the build?',
      'fix the status bar',
      '현황 알려줘 그리고 테스트 돌려줘',
      'what is going on with the tests',
      '상태 코드 확인해줘',
      'estado del repositorio',
    ]) {
      expect(isBriefingPhrase(text), text).toBe(false);
    }
  });

  it('normalizes accents and keeps Hangul intact', () => {
    expect(normalizePhrase('¿Qué está pasando?')).toBe('queestapasando');
    expect(normalizePhrase('현황 알려 줘')).toBe('현황알려줘');
  });
});
