/**
 * Pure helpers for searching a session's chat: case-insensitive matching
 * (any script, regex-escaped) and the texts each timeline item renders, in
 * render order, so match numbers line up with the highlighted `<mark>`s.
 */
import type { ChatItem } from './chatTimeline';
import { splitFences } from './clipboard';
import { markdownRuns } from './markdown';

function escape(query: string): string {
  return query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

const cache = new Map<string, RegExp>();

/** Case-insensitive global matcher for `query` (`null` for an empty query). */
export function matcher(query: string): RegExp | null {
  const q = query.trim();
  if (!q) return null;
  let re = cache.get(q);
  if (!re) {
    re = new RegExp(escape(q), 'giu');
    if (cache.size > 50) cache.clear();
    cache.set(q, re);
  }
  return re;
}

export function countMatches(text: string, query: string): number {
  const re = matcher(query);
  if (!re || !text) return 0;
  re.lastIndex = 0;
  let n = 0;
  while (re.exec(text)) n += 1;
  return n;
}

export interface MatchSegment {
  text: string;
  /** Index of the match within the text, or `null` for plain text. */
  match: number | null;
}

/** Splits `text` into plain parts and matches of `query`. */
export function splitMatches(text: string, query: string): MatchSegment[] {
  const re = matcher(query);
  if (!re || !text) return [{ text, match: null }];
  re.lastIndex = 0;
  const out: MatchSegment[] = [];
  let pos = 0;
  let n = 0;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    if (m.index > pos) out.push({ text: text.slice(pos, m.index), match: null });
    out.push({ text: m[0], match: n++ });
    pos = m.index + m[0].length;
  }
  if (pos < text.length || out.length === 0) out.push({ text: text.slice(pos), match: null });
  return out;
}

/** Texts of a message as `MessageText` renders them (prose and code parts). */
export function messageParts(text: string): string[] {
  return splitFences(text).map((p) => (p.type === 'text' ? p.text : p.code));
}

/**
 * The searchable texts of a timeline item in render order. For collapsible
 * rows the title comes first, then the body (rendered once the row opens).
 * Agent prose renders as markdown (its visible runs), unless the message is
 * switched to its source (`source`).
 */
export function itemTexts(item: ChatItem, source = false): { always: string[]; body: string[] } {
  switch (item.kind) {
    case 'sent':
      return { always: messageParts(item.prompt.text), body: [] };
    case 'output':
      return { always: [item.text], body: [] };
    case 'activity':
      return item.activity.kind === 'marker'
        ? { always: [item.activity.title], body: [] }
        : { always: [item.activity.title], body: markdownRuns(item.activity.body) };
    case 'subagent':
      return { always: [item.description || item.tool], body: markdownRuns(item.report) };
    case 'tool_result': {
      const text = item.isError ? item.text.replace(/^error:\s*/, '') : item.text;
      return { always: [firstLine(text)], body: messageParts(text) };
    }
    case 'event': {
      const e = item.event;
      switch (e.type) {
        case 'message':
          return {
            always: e.role === 'user' || source ? messageParts(e.text) : markdownRuns(e.text),
            body: [],
          };
        case 'tool_call':
          return { always: [e.name, e.summary], body: [] };
        case 'decision_requested':
          return {
            always: [e.decision.prompt, ...e.decision.options.map((o) => o.label)],
            body: [],
          };
        case 'notification':
          return { always: [e.title, e.body], body: [] };
        default:
          return { always: [], body: [] };
      }
    }
  }
}

export function firstLine(text: string): string {
  return (
    text
      .split('\n')
      .map((l) => l.trim())
      .find((l) => l !== '') ?? ''
  );
}

export interface ItemMatches {
  /** Global index of the item's first match. */
  base: number;
  /** Matches in the always-visible texts. */
  head: number;
  /** Matches in the collapsible body. */
  body: number;
}

/** Match counts per item key and the total, for one query. */
export function indexMatches(
  items: readonly ChatItem[],
  query: string,
  sourceKeys: ReadonlySet<string> = new Set(),
): { total: number; byKey: Map<string, ItemMatches> } {
  const byKey = new Map<string, ItemMatches>();
  if (!matcher(query)) return { total: 0, byKey };
  let total = 0;
  for (const item of items) {
    const { always, body } = itemTexts(item, sourceKeys.has(item.key));
    const head = always.reduce((n, t) => n + countMatches(t, query), 0);
    const inBody = body.reduce((n, t) => n + countMatches(t, query), 0);
    if (head + inBody === 0) continue;
    byKey.set(item.key, { base: total, head, body: inBody });
    total += head + inBody;
  }
  return { total, byKey };
}
