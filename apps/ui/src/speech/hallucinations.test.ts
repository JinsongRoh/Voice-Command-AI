import { describe, expect, it } from 'vitest';

import data from './hallucinationPhrases.json';
import {
  dropLoneHallucination,
  isHallucinationPhrase,
  wavLevel,
  type AudioLevel,
} from './hallucinations';

const QUIET: AudioLevel = { durationSecs: 4, rms: 0.004 };
const SHORT: AudioLevel = { durationSecs: 0.8, rms: 0.05 };
const LOUD: AudioLevel = { durationSecs: 4, rms: 0.05 };

function wav(samples: number[], rate = 16000): Uint8Array {
  const out = new Uint8Array(44 + samples.length * 2);
  const v = new DataView(out.buffer);
  const put = (at: number, s: string): void => {
    for (let i = 0; i < s.length; i += 1) out[at + i] = s.charCodeAt(i);
  };
  put(0, 'RIFF');
  v.setUint32(4, 36 + samples.length * 2, true);
  put(8, 'WAVE');
  put(12, 'fmt ');
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true);
  v.setUint16(22, 1, true);
  v.setUint32(24, rate, true);
  v.setUint32(28, rate * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  put(36, 'data');
  v.setUint32(40, samples.length * 2, true);
  samples.forEach((s, i) => v.setInt16(44 + i * 2, s, true));
  return out;
}

describe('hallucination phrases (shared cases)', () => {
  it.each(data.cases)('$text', ({ text, phrase }) => {
    expect(isHallucinationPhrase(text)).toBe(phrase);
  });
});

describe('dropLoneHallucination', () => {
  it('drops a lone phrase on quiet or short audio', () => {
    expect(dropLoneHallucination('감사합니다.', QUIET)).toBe('');
    expect(dropLoneHallucination('감사합니다.', SHORT)).toBe('');
    expect(dropLoneHallucination('시청해 주셔서 감사합니다', QUIET)).toBe('');
    expect(dropLoneHallucination('Thank you.', QUIET)).toBe('');
    expect(dropLoneHallucination('¡Gracias por ver!', SHORT)).toBe('');
  });

  it('keeps it on loud, long audio or without audio information', () => {
    expect(dropLoneHallucination('감사합니다.', LOUD)).toBe('감사합니다.');
    expect(dropLoneHallucination('감사합니다.', null)).toBe('감사합니다.');
  });

  it('never touches a phrase inside a longer sentence', () => {
    for (const t of [
      '도와주셔서 감사합니다 다음 작업 진행해',
      '로그인 고쳐줘 감사합니다',
      'Thank you for fixing it, now run the tests',
    ]) {
      expect(dropLoneHallucination(t, QUIET)).toBe(t);
    }
  });
});

describe('wavLevel', () => {
  it('measures duration and level of a PCM16 WAV', () => {
    const quiet = wavLevel(wav(new Array<number>(16000).fill(30)));
    expect(quiet?.durationSecs).toBeCloseTo(1, 5);
    expect(quiet?.rms).toBeCloseTo(30 / 32768, 5);
    const loud = wavLevel(wav(new Array<number>(8000).fill(8192)));
    expect(loud?.durationSecs).toBeCloseTo(0.5, 5);
    expect(loud?.rms).toBeCloseTo(0.25, 5);
  });

  it('returns null for other data', () => {
    expect(wavLevel(new Uint8Array(10))).toBeNull();
    expect(wavLevel(new Uint8Array(100))).toBeNull();
  });
});
