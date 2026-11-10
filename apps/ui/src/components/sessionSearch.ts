/**
 * Pure helpers for the session picker (quick switcher, "Send to session…",
 * `@` mentions): search, sections and the `@name` prefix of a message.
 */
import { folderName, sessionNeedsAttention, unreadCount } from '../protocol/helpers';
import type { SessionId, SessionInfo } from '../protocol/types';

export type PickerSection = 'attention' | 'pinned' | 'recent' | 'all' | 'results';

export interface PickerGroup {
  section: PickerSection;
  sessions: SessionInfo[];
}

/** Recently active sessions listed under "Recent". */
export const RECENT_COUNT = 5;

/** Lower case without accents or spacing differences, for matching. */
export function normalize(text: string): string {
  return text.normalize('NFKD').replace(/\p{M}/gu, '').toLowerCase().replace(/\s+/g, ' ').trim();
}

function haystack(session: SessionInfo): string {
  const folder = session.origin.type === 'local' ? session.origin.project_dir : '';
  return normalize(
    [session.display_name, session.auto_name ?? '', folder, folder ? folderName(folder) : ''].join(
      ' ',
    ),
  );
}

/** Sessions whose name or folder contains every word of `query` (in their order). */
export function searchSessions(sessions: readonly SessionInfo[], query: string): SessionInfo[] {
  const words = normalize(query).split(' ').filter(Boolean);
  if (words.length === 0) return [...sessions];
  const scored = sessions
    .map((s, i) => {
      const hay = haystack(s);
      if (!words.every((w) => hay.includes(w))) return null;
      const name = normalize(s.display_name);
      // Names that start with the query come first, then other name matches.
      const rank = name.startsWith(words.join(' ')) ? 0 : name.includes(words[0]!) ? 1 : 2;
      return { s, i, rank };
    })
    .filter((x): x is { s: SessionInfo; i: number; rank: number } => x !== null);
  scored.sort((a, b) => a.rank - b.rank || a.i - b.i);
  return scored.map((x) => x.s);
}

/**
 * Sections of the switcher: needs attention, pinned, recent, then all the
 * rest (in card order). Each session is listed once, in its first section.
 * With a query: one flat list of matches.
 */
export function groupSessions(
  sessions: readonly SessionInfo[],
  pins: readonly SessionId[],
  query: string,
): PickerGroup[] {
  if (normalize(query)) {
    const found = searchSessions(sessions, query);
    return found.length ? [{ section: 'results', sessions: found }] : [];
  }
  const used = new Set<SessionId>();
  const take = (list: SessionInfo[]) => {
    const out = list.filter((s) => !used.has(s.id));
    for (const s of out) used.add(s.id);
    return out;
  };
  const attention = take(sessions.filter((s) => sessionNeedsAttention(s) || unreadCount(s) > 0));
  const byId = new Map(sessions.map((s) => [s.id, s]));
  const pinned = take(pins.map((id) => byId.get(id)).filter((s): s is SessionInfo => !!s));
  const recent = take(
    [...sessions].sort((a, b) => b.last_activity - a.last_activity).slice(0, RECENT_COUNT),
  );
  const all = take([...sessions]);
  const groups: PickerGroup[] = [
    { section: 'attention', sessions: attention },
    { section: 'pinned', sessions: pinned },
    { section: 'recent', sessions: recent },
    { section: 'all', sessions: all },
  ];
  return groups.filter((g) => g.sessions.length > 0);
}

/** Sessions an `@` mention may name, best match first. */
export function mentionMatches(sessions: readonly SessionInfo[], query: string): SessionInfo[] {
  return searchSessions(sessions, query);
}

/**
 * The `@` word being typed at the caret (`"fix @pay"` → `"pay"`), or `null`.
 * Only at the start of the box: a mention addresses the whole message.
 */
export function mentionQuery(text: string, caret: number): string | null {
  const before = text.slice(0, caret);
  const m = /^\s*@([^\s@]*)$/u.exec(before);
  return m ? m[1]! : null;
}

/**
 * A message that starts with `@<name>` of a known session: the target and
 * the rest of the text. Names may contain spaces; the longest match wins.
 */
export function leadingMention(
  text: string,
  sessions: readonly SessionInfo[],
): { session: SessionInfo; rest: string } | null {
  const trimmed = text.replace(/^\s+/, '');
  if (!trimmed.startsWith('@')) return null;
  const body = trimmed.slice(1);
  const lower = body.toLowerCase();
  let best: SessionInfo | null = null;
  for (const s of sessions) {
    const name = s.display_name.toLowerCase();
    if (!name || !lower.startsWith(name)) continue;
    const next = body.charAt(name.length);
    // Whole names only: "@api" never matches the session "api-gateway".
    if (next && !/\s/.test(next)) continue;
    if (!best || name.length > best.display_name.length) best = s;
  }
  if (!best) return null;
  return { session: best, rest: body.slice(best.display_name.length).trim() };
}
