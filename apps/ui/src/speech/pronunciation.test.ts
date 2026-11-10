import { afterEach, describe, expect, it } from 'vitest';

import type { Pronunciation } from '../protocol/pronunciations';
import cases from './pronunciationCases.json';
import {
  applyDictionary,
  applyPronunciations,
  createDictionary,
  dictionaryFor,
  setPronunciations,
  subscribePronunciations,
  termKey,
  termTokens,
  validTerm,
} from './pronunciation';
import { speakableRadioWords, speakableText } from './radioSpeech';

const entry = (term: string, spoken: string, language = 'ko'): Pronunciation => ({
  term,
  spoken,
  language,
  source: 'user',
  updated_at: 1,
});

afterEach(() => setPronunciations([]));

// The same file runs in `pronounce::tests` (crates/vca-speech/src/pronounce.rs).
describe('shared pronunciation cases', () => {
  const ko = createDictionary(cases.entries);

  it.each(cases.tokens)('splits $term', ({ term, tokens }) => {
    expect(termTokens(term)).toEqual(tokens);
  });

  it.each(cases.cases)('[$lang] $text', ({ lang, text, want }) => {
    const dict = lang.startsWith('ko') ? ko : createDictionary([]);
    // The PC applies the dictionary, then the radio words (no number rule there).
    expect(speakableRadioWords(applyDictionary(text, dict), lang)).toBe(want);
  });
});

describe('dictionary', () => {
  it('treats CamelCase, spaced and hyphenated spellings as one term', () => {
    expect(termKey('SkyHarbor')).toBe('sky harbor');
    expect(termKey('Sky Harbor')).toBe(termKey('sky-harbor'));
    const d = createDictionary([
      { term: 'SkyHarbor', spoken: '하나' },
      { term: 'Sky Harbor', spoken: '둘' },
      { term: '-', spoken: 'x' },
      { term: 'Q', spoken: ' ' },
    ]);
    expect(d.entries).toHaveLength(1);
    expect(applyDictionary('sky harbor', d)).toBe('하나');
  });

  it('accepts only Latin terms', () => {
    expect(validTerm(' Sky  Harbor ')).toBe(true);
    expect(validTerm('R&D Lab')).toBe(true);
    expect(validTerm('')).toBe(false);
    expect(validTerm('--')).toBe(false);
    expect(validTerm('스카이')).toBe(false);
    expect(validTerm('a'.repeat(65))).toBe(false);
  });

  it('uses the active entries of the voice language in every spoken text', () => {
    let changes = 0;
    const off = subscribePronunciations(() => (changes += 1));
    setPronunciations([entry('SkyHarbor', '스카이하버'), entry('SkyHarbor', 'スカイ', 'ja')]);
    expect(changes).toBe(1);
    off();
    expect(applyPronunciations('Sky-Harbor 끝', 'ko-KR')).toBe('스카이하버 끝');
    expect(applyPronunciations('SkyHarbor', 'ja-JP')).toBe('スカイ');
    expect(applyPronunciations('SkyHarbor', 'en')).toBe('SkyHarbor');
    expect(dictionaryFor('ko')).toBe(dictionaryFor('ko-KR'));
    // Dictionary, radio words, then Korean numbers.
    expect(speakableText('SkyHarbor 3번 끝, Roger', 'ko')).toBe('스카이하버 삼번 끝, 로저');
    setPronunciations([]);
    expect(speakableText('SkyHarbor', 'ko')).toBe('SkyHarbor');
  });
});
