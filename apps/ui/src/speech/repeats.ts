/**
 * Repetition-loop guard for transcripts, the client-side twin of
 * `vca_speech::cleanup::collapse_repeats` (crates/vca-speech/src/cleanup.rs).
 *
 * Speech engines (Whisper in particular) emit loops on silence or noise:
 * "O.O.O.O.…", "아. 아. 아. 아.", "음음음음음", "thank you thank you …". The
 * PC's speech pipeline collapses them already; the client runs the same pass
 * on every transcript it receives, so a PC on an older version (or any other
 * text source) cannot put a loop in the composer. Keep both implementations
 * and their tests in step.
 */

/** Units of at most this many characters are checked for repetition loops. */
const MAX_UNIT_CHARS = 4;
/** Consecutive repetitions of a short unit that make a loop. */
const MIN_REPEATS = 4;
/** A single ASCII letter without separators needs this many ("AAAA" survives). */
const MIN_ASCII_LETTER_RUN = 5;
/** A unit with a digit needs this many ("1.1.1.1" survives); bare digit runs never loop. */
const MIN_DIGIT_REPEATS = 8;
/** Runs of one punctuation mark up to this length are kept ("......"). */
const MAX_PUNCT_RUN = 6;
/** Longer punctuation runs are shortened to this many marks. */
const PUNCT_RUN_KEPT = 3;
/** Separator characters allowed between two repetitions of a unit. */
const MAX_SEPARATOR_CHARS = 2;
/** A transcript that is at least this share one loop... */
const DROP_SHARE = 0.8;
/** ...of at least this many repetitions is dropped. */
const DROP_MIN_REPEATS = 6;
/** Words in a repeated phrase, and the repetitions that make a phrase loop. */
const MAX_PHRASE_WORDS = 6;
const MIN_PHRASE_REPEATS = 3;

const WHITESPACE = /\s/u;
const ALNUM = /[\p{L}\p{N}]/u;
const NUMERIC = /\p{N}/u;
const ASCII_LETTER = /[A-Za-z]/;
const SEPARATORS = new Set([',', '.', '、', '。', '，', '!', '?', '…', '·', '-', '~', ';', ':']);

const isSpace = (c: string): boolean => WHITESPACE.test(c);
const isSeparator = (c: string): boolean => isSpace(c) || SEPARATORS.has(c);

/** Characters of URL-, e-mail- or path-like tokens, which are never collapsed. */
function protectedMask(chars: readonly string[]): boolean[] {
  const mask = chars.map(() => false);
  let i = 0;
  while (i < chars.length) {
    if (isSpace(chars[i]!)) {
      i += 1;
      continue;
    }
    const start = i;
    while (i < chars.length && !isSpace(chars[i]!)) i += 1;
    const token = chars.slice(start, i).join('').toLowerCase();
    if (token.includes('://') || token.startsWith('www.') || /[@/\\]/.test(token)) {
      mask.fill(true, start, i);
    }
  }
  return mask;
}

/** Repetitions a loop of `unit` needs, or null when it never loops. */
function minRepeats(unit: readonly string[], usedSeparator: boolean): number | null {
  if (unit.some((c) => NUMERIC.test(c))) {
    if (!usedSeparator && unit.every((c) => NUMERIC.test(c))) return null;
    return MIN_DIGIT_REPEATS;
  }
  if (!usedSeparator && unit.length === 1 && ASCII_LETTER.test(unit[0]!)) {
    return MIN_ASCII_LETTER_RUN;
  }
  return MIN_REPEATS;
}

interface Loop {
  unit: number;
  end: number;
  reps: number;
  keep: number;
}

function loopAt(
  chars: readonly string[],
  folded: readonly string[],
  prot: readonly boolean[],
  i: number,
): Loop | null {
  const n = chars.length;
  const c = chars[i]!;
  if (prot[i] || isSpace(c)) return null;
  if (!ALNUM.test(c)) {
    let end = i + 1;
    while (end < n && chars[end] === c && !prot[end]) end += 1;
    const reps = end - i;
    return reps > MAX_PUNCT_RUN ? { unit: 1, end, reps, keep: PUNCT_RUN_KEPT } : null;
  }
  for (let len = 1; len <= MAX_UNIT_CHARS && i + len <= n; len += 1) {
    const unit = folded.slice(i, i + len);
    if (unit.some(isSpace) || prot.slice(i, i + len).includes(true)) break;
    const matches = (pos: number): boolean => {
      if (pos + len > n) return false;
      for (let k = 0; k < len; k += 1) {
        if (folded[pos + k] !== unit[k] || prot[pos + k]) return false;
      }
      return true;
    };
    let reps = 1;
    let end = i + len;
    let usedSeparator = false;
    for (;;) {
      if (matches(end)) {
        reps += 1;
        end += len;
        continue;
      }
      let next = 0;
      for (let s = 1; s <= MAX_SEPARATOR_CHARS && end + s <= n; s += 1) {
        if (!isSeparator(chars[end + s - 1]!)) break;
        if (matches(end + s)) {
          next = s;
          break;
        }
      }
      if (next === 0) break;
      reps += 1;
      end += next + len;
      usedSeparator = true;
    }
    const need = minRepeats(unit, usedSeparator);
    if (need !== null && reps >= need) return { unit: len, end, reps, keep: 1 };
  }
  return null;
}

/** Collapses short-unit loops; null when the whole text is one loop. */
function collapseUnits(text: string): string | null {
  const chars = Array.from(text);
  const folded = chars.map((c) => {
    const lower = c.toLowerCase();
    return Array.from(lower).length === 1 ? lower : c;
  });
  const prot = protectedMask(chars);
  const visible = (from: number, to: number): number => {
    let count = 0;
    for (let k = from; k < to; k += 1) if (!isSpace(chars[k]!)) count += 1;
    return count;
  };
  const total = visible(0, chars.length);
  let out = '';
  let i = 0;
  while (i < chars.length) {
    const l = loopAt(chars, folded, prot, i);
    if (l) {
      if (l.reps >= DROP_MIN_REPEATS && visible(i, l.end) >= DROP_SHARE * total) return null;
      out += chars
        .slice(i, i + l.unit)
        .join('')
        .repeat(l.keep);
      i = l.end;
    } else {
      out += chars[i];
      i += 1;
    }
  }
  return out;
}

/** Keeps a word or a phrase of up to six words repeated three or more times once. */
function collapsePhrases(text: string): string {
  const words = text.split(/\s+/u).filter((w) => w.length > 0);
  if (words.length < MIN_PHRASE_REPEATS) return text;
  const keys = words.map((w) => w.replace(/^[^\p{L}\p{N}]+|[^\p{L}\p{N}]+$/gu, '').toLowerCase());
  const same = (a: number, b: number, n: number): boolean => {
    for (let k = 0; k < n; k += 1) if (keys[a + k] !== keys[b + k]) return false;
    return true;
  };
  const kept: string[] = [];
  let changed = false;
  let i = 0;
  outer: while (i < words.length) {
    for (let n = 1; n <= MAX_PHRASE_WORDS; n += 1) {
      if (i + MIN_PHRASE_REPEATS * n > words.length) continue;
      if (keys.slice(i, i + n).every((k) => k === '')) continue;
      let reps = 1;
      while (i + (reps + 1) * n <= words.length && same(i + reps * n, i, n)) reps += 1;
      if (reps >= MIN_PHRASE_REPEATS) {
        kept.push(...words.slice(i, i + n));
        i += reps * n;
        changed = true;
        continue outer;
      }
    }
    kept.push(words[i]!);
    i += 1;
  }
  return changed ? kept.join(' ') : text;
}

/**
 * Collapses repetition loops (see the module comment):
 *
 * - a unit of one to four characters ("O.", "ㅋ", "아.", "네") repeated four
 *   or more times in a row, with up to two separator characters between the
 *   repetitions, is kept once; digits need eight repetitions and never loop
 *   without separators ("1.1.1.1", "10000"), a single ASCII letter needs five
 *   ("AAAA"), punctuation runs are kept up to six ("......") and longer ones
 *   become three, and URL-, e-mail- and path-like tokens are left alone;
 * - a word or a phrase of up to six words repeated three or more times is
 *   kept once;
 * - a transcript that is at least 80% one loop of six or more repetitions
 *   becomes empty.
 */
export function collapseRepeats(text: string): string {
  const collapsed = collapseUnits(text);
  return collapsed === null ? '' : collapsePhrases(collapsed);
}

/** Whether `text` is a repetition-loop hallucination that `collapseRepeats` empties. */
export function isRepetitionHallucination(text: string): boolean {
  return text.trim() !== '' && collapseUnits(text) === null;
}
