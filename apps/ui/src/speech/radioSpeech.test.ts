import { describe, expect, it } from 'vitest';
import { speakableRadioWords, speakableText } from './radioSpeech';

// The same cases as `radio::tests` in crates/vca-speech/src/radio.rs.
describe('speakableRadioWords', () => {
  it('makes Korean voices say the radio words in hangul', () => {
    expect(speakableRadioWords('Voco가 준비됐어요', 'ko-KR')).toBe('보코가 준비됐어요');
    expect(speakableRadioWords('VOCO, Roger.', 'ko')).toBe('보코, 로저.');
    expect(speakableRadioWords('보내려면 Over라고 말하세요', 'ko')).toBe(
      '보내려면 오버라고 말하세요',
    );
    expect(speakableRadioWords('roger that', 'ko')).toBe('로저 that');
    expect(speakableRadioWords('보코, 로저', 'ko')).toBe('보코, 로저');
  });

  it('only rewrites whole words', () => {
    expect(speakableRadioWords('overflow를 고쳐', 'ko')).toBe('overflow를 고쳐');
    expect(speakableRadioWords('Vocoder 설정', 'ko')).toBe('Vocoder 설정');
    expect(speakableRadioWords('my_over 변수', 'ko')).toBe('my_over 변수');
    expect(speakableRadioWords('Rogers', 'ko')).toBe('Rogers');
  });

  it('uses katakana for Japanese and keeps English and Spanish', () => {
    expect(speakableRadioWords('Voco, Roger', 'ja-JP')).toBe('ボコ, ラジャー');
    expect(speakableRadioWords('Voco, Roger. Over', 'en-US')).toBe('Voco, Roger. Over');
    expect(speakableRadioWords('Voco, Roger', 'es')).toBe('Voco, Roger');
    expect(speakableRadioWords('Voco, Roger', '')).toBe('Voco, Roger');
  });
});

describe('speakableText', () => {
  it('applies the radio words and the Korean number reading', () => {
    expect(speakableText('Roger. 3번 세션에 보냈어요', 'ko')).toBe('로저. 삼번 세션에 보냈어요');
    expect(speakableText('Roger. Sent to session 3', 'en')).toBe('Roger. Sent to session 3');
  });
});
