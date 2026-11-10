/**
 * Sessions in spoken phrases: by the name the user gave them ("Orbit
 * Platform", "Voice Command AI"), not by their card number, which people do
 * not remember. The number is added only when two sessions share a name.
 * Voices read the name through the pronunciation dictionary
 * (`speakableText`, applied when speaking), so "Orbit" is said "오빗".
 *
 * The other way round, `matchSessionName` finds the session a spoken name
 * means ("보코, 오빗 플랫폼", "보코, 체크 AI 세션"): normalized text, the
 * Korean reading from the pronunciation dictionary and a jamo-level fuzzy
 * match with a threshold; two close candidates are ambiguous (the caller
 * asks "어느 세션인가요?").
 */
import type { VoiceLang } from './handsFree';
import { editDistance, toJamo } from './jamo';
import { applyPronunciations } from './pronunciation';

/** Longer names are shortened for speech (at a word boundary when possible). */
export const MAX_SPOKEN_NAME_CHARS = 24;
/** Least similarity (0..1) for a spoken name to mean a session. */
export const NAME_MATCH_MIN = 0.75;
/** Two candidates this close in similarity are ambiguous. */
export const NAME_MATCH_MARGIN = 0.08;

export interface NamedSession {
  id: string;
  display_name: string;
}

/**
 * The name to say: the display name without a generated " · agent" suffix,
 * shortened to about `MAX_SPOKEN_NAME_CHARS` characters (whole words).
 */
export function shortSessionName(name: string): string {
  let n = name.replace(/\s+/g, ' ').trim();
  // "<folder> · <agent>" (a generated name): the folder is the name.
  if (n.includes(' · ')) n = n.split(' · ')[0]!.trim();
  const chars = Array.from(n);
  if (chars.length <= MAX_SPOKEN_NAME_CHARS) return n;
  const head = chars.slice(0, MAX_SPOKEN_NAME_CHARS).join('');
  // The cut falls between two words: the head is whole words already.
  if (chars[MAX_SPOKEN_NAME_CHARS] === ' ') return head.trimEnd();
  const space = head.lastIndexOf(' ');
  return (space >= MAX_SPOKEN_NAME_CHARS / 2 ? head.slice(0, space) : head).replace(
    /[\s\p{P}]+$/u,
    '',
  );
}

const NUMBERED: Record<VoiceLang, (name: string, n: number) => string> = {
  ko: (name, n) => `${name} ${n}번`,
  en: (name, n) => `${name} number ${n}`,
  ja: (name, n) => `${name} ${n}番`,
  es: (name, n) => `${name} número ${n}`,
};

/**
 * The spoken name of `session`: its short name, plus its card number when
 * another session in `order` has the same one ("Voice Command AI 2번").
 */
export function spokenSessionName(
  session: NamedSession,
  sessions: Readonly<Record<string, NamedSession | undefined>>,
  order: readonly string[],
  lang: VoiceLang,
): string {
  const name = shortSessionName(session.display_name) || session.display_name.trim();
  const key = normalizeName(name);
  const twins = order.filter((id) => {
    const other = sessions[id];
    return other && normalizeName(shortSessionName(other.display_name)) === key;
  });
  const index = order.indexOf(session.id);
  if (twins.length > 1 && index >= 0) return NUMBERED[lang](name, index + 1);
  return name;
}

/**
 * The name as the target of a phrase: "Orbit Platform 세션" in Korean and
 * Japanese (unless the name already says so), the bare name otherwise.
 */
export function sessionLabel(name: string, lang: VoiceLang): string {
  const n = name.trim();
  switch (lang) {
    case 'ko':
      return /세션$/u.test(n) ? n : `${n} 세션`;
    case 'ja':
      return /セッション$/u.test(n) ? n : `${n}セッション`;
    default:
      return n;
  }
}

/** Lower case, NFKC, without spaces and punctuation. */
export function normalizeName(text: string): string {
  return text
    .normalize('NFKC')
    .toLowerCase()
    .replace(/[\s\p{P}\p{S}]+/gu, '');
}

/** Words around a spoken name that are not part of it ("…세션으로 가", "session …"). */
const NAME_TAILS = [
  '세션으로이동해줘',
  '세션으로이동해',
  '세션으로이동',
  '세션으로가줘',
  '세션으로가',
  '세션으로',
  '세션',
  '으로가',
  '으로',
  'session',
  'セッション',
  'sesion',
  'sesión',
];
const NAME_HEADS = ['session', 'sesion', 'sesión', 'goto', 'switchto', 'セッション'];

function stripAround(text: string): string {
  let t = normalizeName(text);
  for (const h of NAME_HEADS) if (t.startsWith(h) && t.length > h.length) t = t.slice(h.length);
  for (const tail of NAME_TAILS) {
    if (t.endsWith(tail) && t.length > tail.length) {
      t = t.slice(0, -tail.length);
      break;
    }
  }
  return t;
}

/** Similarity (0..1) of two normalized names, by jamo for Korean. */
function similarity(a: string, b: string): number {
  if (!a || !b) return 0;
  if (a === b) return 1;
  const x = toJamo(a);
  const y = toJamo(b);
  const sim = 1 - editDistance(x, y) / Math.max(x.length, y.length);
  // A clear beginning of the name ("오빗" for "오빗 플랫폼").
  const minPrefix = /^\p{Script=Hangul}+$/u.test(a) ? 2 : 3;
  if (Array.from(a).length >= minPrefix && b.startsWith(a)) return Math.max(sim, 0.8);
  return sim;
}

export type NameMatch =
  { kind: 'one'; id: string; score: number } | { kind: 'ambiguous'; ids: string[] } | null;

/**
 * The session `spoken` names (see the module comment), compared with each
 * session's display name, its short name and their Korean reading from the
 * pronunciation dictionary.
 */
export function matchSessionName(spoken: string, sessions: readonly NamedSession[]): NameMatch {
  const said = stripAround(spoken);
  if (Array.from(said).length < 2) return null;
  const scored = sessions
    .map((s) => {
      const forms = new Set<string>();
      for (const n of [s.display_name, shortSessionName(s.display_name)]) {
        forms.add(normalizeName(n));
        forms.add(normalizeName(applyPronunciations(n, 'ko')));
        forms.add(stripAround(n));
      }
      const score = Math.max(...[...forms].map((f) => similarity(said, f)));
      return { id: s.id, score };
    })
    .filter((s) => s.score >= NAME_MATCH_MIN)
    .sort((a, b) => b.score - a.score);
  if (scored.length === 0) return null;
  const best = scored[0]!;
  const close = scored.filter((s) => best.score - s.score < NAME_MATCH_MARGIN);
  if (close.length > 1) return { kind: 'ambiguous', ids: close.map((s) => s.id) };
  return { kind: 'one', id: best.id, score: best.score };
}
