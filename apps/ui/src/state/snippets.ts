/** Saved command snippets for the phone key bar (this device only). */

export const SNIPPETS_KEY = 'vca.snippets.v1';
export const MAX_SNIPPETS = 30;
/** Longest snippet kept, in characters. */
export const MAX_SNIPPET_CHARS = 2000;

export function loadSnippets(): string[] {
  try {
    const raw = globalThis.localStorage?.getItem(SNIPPETS_KEY);
    if (!raw) return [];
    const value: unknown = JSON.parse(raw);
    if (!Array.isArray(value)) return [];
    return value
      .filter((v): v is string => typeof v === 'string' && v.trim() !== '')
      .slice(0, MAX_SNIPPETS);
  } catch {
    return [];
  }
}

export function saveSnippets(list: string[]): void {
  try {
    globalThis.localStorage?.setItem(SNIPPETS_KEY, JSON.stringify(list.slice(0, MAX_SNIPPETS)));
  } catch {
    // Storage unavailable (private mode, quota): snippets last for this run.
  }
}

/** Adds a snippet at the top (no duplicates, trimmed, bounded). */
export function addSnippet(list: string[], text: string): string[] {
  const value = text.trim().slice(0, MAX_SNIPPET_CHARS);
  if (!value) return list;
  return [value, ...list.filter((s) => s !== value)].slice(0, MAX_SNIPPETS);
}

export function removeSnippet(list: string[], text: string): string[] {
  return list.filter((s) => s !== text);
}
