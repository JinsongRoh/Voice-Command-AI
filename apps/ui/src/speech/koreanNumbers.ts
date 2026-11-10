/**
 * Korean number reading for spoken alerts.
 *
 * TTS engines read "3번" with native Korean numbers ("세 번", three times),
 * but a numbered item such as "3번 세션" is read with Sino-Korean numbers
 * ("삼번 세션"). Ordinals with 번째 keep the native reading ("세 번째"), so
 * only a plain 번 after digits is rewritten.
 */

const DIGITS = ['', '일', '이', '삼', '사', '오', '육', '칠', '팔', '구'];
const UNITS = ['', '십', '백', '천'];

/** Sino-Korean reading of 0..9999 ("영", "삼", "십이", "이천백"). */
export function sinoKorean(n: number): string {
  if (!Number.isInteger(n) || n < 0 || n > 9999) return String(n);
  if (n === 0) return '영';
  let out = '';
  const digits = String(n).split('').map(Number);
  digits.forEach((d, i) => {
    const unit = UNITS[digits.length - 1 - i] ?? '';
    if (d === 0) return;
    // 십, 백, 천 are read without a leading 일.
    out += (d === 1 && unit ? '' : DIGITS[d]) + unit;
  });
  return out;
}

/** Rewrites "<digits>번" (not "번째") to its Sino-Korean reading for Korean. */
export function normalizeKoreanNumbers(text: string, lang: string): string {
  if (!lang.toLowerCase().startsWith('ko')) return text;
  return text.replace(
    /(\d{1,4})\s*번(?!\s*째)/g,
    (_, digits: string) => `${sinoKorean(Number(digits))}번`,
  );
}
