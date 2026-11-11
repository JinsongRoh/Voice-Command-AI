/**
 * The pronunciation dictionary of spoken alerts: how a voice should say
 * names written in Latin letters ("SkyHarbor" -> "스카이하버"). The client
 * twin of `vca_speech::pronounce` (crates/vca-speech/src/pronounce.rs); both
 * run the cases in `pronunciationCases.json`.
 *
 * - A term is split into lower-case tokens at spaces and punctuation, at
 *   CamelCase boundaries ("QRTLink" -> `qrt link`, "pDOC" -> `p doc`) and
 *   between letters and digits. "SkyHarbor", "Sky Harbor" and "sky-harbor"
 *   are the same term.
 * - In a text the tokens match ignoring ASCII case, joined by nothing, by
 *   spaces, or by one `-` or `_`.
 * - A match never starts or ends inside a word (ASCII letters, digits, `_`);
 *   Korean particles may follow ("SkyHarbor가").
 * - Longer terms are tried first.
 *
 * The entries come from the PC (`pronunciations`); the active ones are kept
 * here so every spoken text (`speakableText`) uses them.
 */
import type { Pronunciation } from '../protocol/pronunciations';

const isAlnum = (c: string | undefined): boolean => !!c && /^[A-Za-z0-9]$/.test(c);
const isWord = (c: string | undefined): boolean => !!c && /^[A-Za-z0-9_]$/.test(c);
const isLower = (c: string | undefined): boolean => !!c && c >= 'a' && c <= 'z';
const isUpper = (c: string | undefined): boolean => !!c && c >= 'A' && c <= 'Z';
const isDigit = (c: string | undefined): boolean => !!c && c >= '0' && c <= '9';

/** Longest term and spoken form, in characters (as on the PC). */
export const MAX_TERM_CHARS = 64;
export const MAX_SPOKEN_CHARS = 100;

/** The lower-case tokens of a term; empty without ASCII letters or digits. */
export function termTokens(term: string): string[] {
  const out: string[] = [];
  for (const run of term.split(/[^A-Za-z0-9]+/)) {
    let start = 0;
    for (let i = 1; i < run.length; i += 1) {
      const prev = run[i - 1];
      const cur = run[i];
      const next = run[i + 1];
      const split =
        (isLower(prev) && isUpper(cur)) ||
        (isUpper(prev) && isUpper(cur) && isLower(next)) ||
        isDigit(prev) !== isDigit(cur);
      if (split) {
        out.push(run.slice(start, i).toLowerCase());
        start = i;
      }
    }
    if (start < run.length) out.push(run.slice(start).toLowerCase());
  }
  return out;
}

/** The identity of a term: its tokens joined by single spaces. */
export function termKey(term: string): string {
  return termTokens(term).join(' ');
}

/** A term the PC accepts: Latin letters, digits and a little punctuation. */
export function validTerm(term: string): boolean {
  const t = term.trim().replace(/\s+/g, ' ');
  return (
    t.length > 0 &&
    t.length <= MAX_TERM_CHARS &&
    /^[A-Za-z0-9 \-_.&+'/]+$/.test(t) &&
    termTokens(t).length > 0
  );
}

export interface Dictionary {
  /** Tokens and spoken form, longest key first. */
  readonly entries: readonly { tokens: string[]; spoken: string }[];
}

/** Builds a dictionary; of two entries with the same key, the first wins. */
export function createDictionary(pairs: Iterable<{ term: string; spoken: string }>): Dictionary {
  const entries: { tokens: string[]; spoken: string; key: string }[] = [];
  for (const { term, spoken } of pairs) {
    const tokens = termTokens(term);
    const said = spoken.trim();
    const key = tokens.join(' ');
    if (!tokens.length || !said || entries.some((e) => e.key === key)) continue;
    entries.push({ tokens, spoken: said, key });
  }
  entries.sort(
    (a, b) =>
      b.key.length - a.key.length ||
      b.tokens.length - a.tokens.length ||
      (a.key < b.key ? -1 : a.key > b.key ? 1 : 0),
  );
  return { entries: entries.map(({ tokens, spoken }) => ({ tokens, spoken })) };
}

function matchAt(text: string, pos: number, tokens: readonly string[]): number | null {
  let p = pos;
  for (let k = 0; k < tokens.length; k += 1) {
    const token = tokens[k] ?? '';
    if (k > 0) {
      if (text[p] === ' ') {
        while (text[p] === ' ') p += 1;
      } else if (text[p] === '-' || text[p] === '_') {
        p += 1;
      }
    }
    const end = p + token.length;
    if (end > text.length) return null;
    // Only ASCII is folded, as on the PC.
    const part = text.slice(p, end).replace(/[A-Z]/g, (c) => c.toLowerCase());
    if (part !== token) return null;
    p = end;
  }
  return isWord(text[p]) ? null : p;
}

/** Replaces every term in `text` with its spoken form. */
export function applyDictionary(text: string, dictionary: Dictionary): string {
  if (!dictionary.entries.length) return text;
  let out = '';
  let pos = 0;
  while (pos < text.length) {
    const c = text[pos];
    if (isAlnum(c) && (pos === 0 || !isWord(text[pos - 1]))) {
      let hit: { end: number; spoken: string } | null = null;
      for (const entry of dictionary.entries) {
        const end = matchAt(text, pos, entry.tokens);
        if (end !== null) {
          hit = { end, spoken: entry.spoken };
          break;
        }
      }
      if (hit) {
        out += hit.spoken;
        pos = hit.end;
        continue;
      }
    }
    out += c;
    pos += 1;
  }
  return out;
}

/** The words of a name written in Latin letters ("SkyHarbor 정리" -> "SkyHarbor"). */
export function latinPart(name: string): string {
  return name
    .split(/\s+/)
    .map((w) => w.replace(/^[^A-Za-z0-9]+|[^A-Za-z0-9]+$/g, ''))
    .filter((w) => /[A-Za-z]/.test(w) && validTerm(w))
    .join(' ');
}

/**
 * What the recognizer heard, as a spoken form: trailing punctuation off,
 * and the recognizer's own Hangul when a vocabulary correction turned it
 * back into Latin letters.
 */
export function heardAsSpoken(text: string, rawText?: string): string {
  const clean = (s: string) =>
    s
      .replace(/[.,!?。、！？"'“”‘’]+/g, ' ')
      .replace(/\s+/g, ' ')
      .trim();
  const said = clean(text);
  const raw = rawText ? clean(rawText) : '';
  if (raw && /[A-Za-z]/.test(said) && !/[A-Za-z]/.test(raw)) return raw;
  return said;
}

function baseLang(lang: string): string {
  return lang.toLowerCase().split(/[-_]/)[0] ?? '';
}

// --- The active entries (those of the selected PC). ---

let active: readonly Pronunciation[] = [];
const byLang = new Map<string, Dictionary>();
const listeners = new Set<() => void>();

/** Replaces the active entries (the PC's list, or `[]`). */
export function setPronunciations(entries: readonly Pronunciation[]): void {
  active = entries;
  byLang.clear();
  for (const l of listeners) l();
}

/** The active entries. */
export function getPronunciations(): readonly Pronunciation[] {
  return active;
}

/** Calls `listener` after every change; returns an unsubscribe. */
export function subscribePronunciations(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The active dictionary of a voice language (BCP-47). */
export function dictionaryFor(lang: string): Dictionary {
  const base = baseLang(lang);
  let dict = byLang.get(base);
  if (!dict) {
    dict = createDictionary(active.filter((e) => baseLang(e.language) === base));
    byLang.set(base, dict);
  }
  return dict;
}

/** `text` with the active dictionary of `lang` applied. */
export function applyPronunciations(text: string, lang: string): string {
  return applyDictionary(text, dictionaryFor(lang));
}
