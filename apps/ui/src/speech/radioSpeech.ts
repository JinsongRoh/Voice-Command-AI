/**
 * Pronunciation of the radio-style voice words (wake word "Voco", send word
 * "Over", reply "Roger") for spoken replies, the client-side twin of
 * `vca_speech::radio::speakable` (crates/vca-speech/src/radio.rs).
 *
 * A Korean voice spells "Voco" letter by letter or reads "Roger" oddly, so for
 * Korean the Latin spellings become "보코", "로저" and "오버"; for Japanese
 * 「ボコ」, 「ラジャー」 and 「オーバー」. English and other languages are
 * unchanged. Matching ignores case and needs whole words ("overflow" and
 * "Vocoder" stay).
 */
import { normalizeKoreanNumbers } from './koreanNumbers';
import { applyPronunciations } from './pronunciation';

const SPOKEN: Record<string, Record<string, string>> = {
  ko: { voco: '보코', roger: '로저', over: '오버' },
  ja: { voco: 'ボコ', roger: 'ラジャー', over: 'オーバー' },
};

const WORDS = /(?<![A-Za-z0-9_])(voco|roger|over)(?![A-Za-z0-9_])/gi;

function baseLang(lang: string): string {
  return lang.toLowerCase().split(/[-_]/)[0] ?? '';
}

/** Rewrites "Voco", "Roger" and "Over" for a voice of `lang` (BCP-47). */
export function speakableRadioWords(text: string, lang: string): string {
  const forms = SPOKEN[baseLang(lang)];
  if (!forms) return text;
  return text.replace(WORDS, (word: string) => forms[word.toLowerCase()] ?? word);
}

/**
 * Text as a voice of `lang` should read it: the PC's pronunciation
 * dictionary ("SkyHarbor" -> "스카이하버", see pronunciation.ts), radio words,
 * then Korean numbers ("3번" -> "삼번"). Use this for everything that is spoken.
 */
export function speakableText(text: string, lang: string): string {
  return normalizeKoreanNumbers(speakableRadioWords(applyPronunciations(text, lang), lang), lang);
}
