/**
 * Hangul jamo helpers for fuzzy matching of short spoken words (the wake
 * word "보코", the answers "보내" / "취소", session names). Comparing jamo
 * instead of whole syllables makes "보네" one step from "보내" ("ㅔ" for "ㅐ")
 * rather than a different syllable.
 */

const SYLLABLE_FIRST = 0xac00;
const SYLLABLE_LAST = 0xd7a3;
const VOWELS = 21;
const FINALS = 28;

/** Initial consonants, vowels and final consonants in Unicode order. */
const INITIAL_JAMO = 'ㄱㄲㄴㄷㄸㄹㅁㅂㅃㅅㅆㅇㅈㅉㅊㅋㅌㅍㅎ';
const VOWEL_JAMO = 'ㅏㅐㅑㅒㅓㅔㅕㅖㅗㅘㅙㅚㅛㅜㅝㅞㅟㅠㅡㅢㅣ';
const FINAL_JAMO = ' ㄱㄲㄳㄴㄵㄶㄷㄹㄺㄻㄼㄽㄾㄿㅀㅁㅂㅄㅅㅆㅇㅈㅊㅋㅌㅍㅎ';

/** One syllable split into its jamo (`final` is '' without a final consonant). */
export interface SyllableJamo {
  initial: string;
  vowel: string;
  final: string;
}

export function isHangulSyllable(ch: string): boolean {
  const code = ch.codePointAt(0) ?? 0;
  return code >= SYLLABLE_FIRST && code <= SYLLABLE_LAST;
}

/** The jamo of one precomposed syllable, or `null` for any other character. */
export function syllableJamo(ch: string): SyllableJamo | null {
  if (!isHangulSyllable(ch)) return null;
  const index = (ch.codePointAt(0) ?? 0) - SYLLABLE_FIRST;
  const final = index % FINALS;
  const vowel = Math.floor(index / FINALS) % VOWELS;
  const initial = Math.floor(index / (FINALS * VOWELS));
  return {
    initial: INITIAL_JAMO[initial]!,
    vowel: VOWEL_JAMO[vowel]!,
    final: final === 0 ? '' : FINAL_JAMO[final]!,
  };
}

/** Builds a syllable from its jamo (`null` when they do not form one). */
export function composeSyllable(j: SyllableJamo): string | null {
  const i = INITIAL_JAMO.indexOf(j.initial);
  const v = VOWEL_JAMO.indexOf(j.vowel);
  const f = j.final ? FINAL_JAMO.indexOf(j.final) : 0;
  if (i < 0 || v < 0 || f < 0) return null;
  return String.fromCodePoint(SYLLABLE_FIRST + (i * VOWELS + v) * FINALS + f);
}

/**
 * The text as a jamo sequence: Hangul syllables are split ("보코" ->
 * "ㅂㅗㅋㅗ"), other characters are kept as they are. A silent initial "ㅇ" is
 * kept so syllable counts stay comparable.
 */
export function toJamo(text: string): string[] {
  const out: string[] = [];
  for (const ch of text) {
    const j = syllableJamo(ch);
    if (!j) out.push(ch);
    else {
      out.push(j.initial, j.vowel);
      if (j.final) out.push(j.final);
    }
  }
  return out;
}

/** Levenshtein distance of two sequences. */
export function editDistance(a: readonly string[], b: readonly string[]): number {
  let prev = Array.from({ length: b.length + 1 }, (_, j) => j);
  for (let i = 1; i <= a.length; i += 1) {
    const row = [i];
    for (let j = 1; j <= b.length; j += 1) {
      row[j] = Math.min(
        prev[j]! + 1,
        row[j - 1]! + 1,
        prev[j - 1]! + (a[i - 1] === b[j - 1] ? 0 : 1),
      );
    }
    prev = row;
  }
  return prev[b.length]!;
}

/** Edit distance between the jamo of `a` and `b`. */
export function jamoDistance(a: string, b: string): number {
  return editDistance(toJamo(a), toJamo(b));
}

/**
 * Collapses a stretched call: "~" and similar marks go, and a syllable that
 * only repeats the previous vowel ("보코오", "보오코", "보코오오") is dropped.
 * "보코오~" -> "보코". Other text is unchanged.
 */
export function collapseStretch(text: string): string {
  const chars = Array.from(text.replace(/[~〜～ー-]+/gu, ''));
  const out: string[] = [];
  for (const ch of chars) {
    const j = syllableJamo(ch);
    const prev = out.length > 0 ? syllableJamo(out[out.length - 1]!) : null;
    // "오" after "...ㅗ" (an initial "ㅇ", the same vowel, no final consonant).
    if (j && prev && j.initial === 'ㅇ' && !j.final && !prev.final && j.vowel === prev.vowel) {
      continue;
    }
    out.push(ch);
  }
  return out.join('');
}
