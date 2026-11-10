/**
 * Copies text to the clipboard. Uses the async Clipboard API and falls back
 * to a hidden textarea with `execCommand('copy')` for webviews that block
 * the API (insecure origins, older Android System WebView).
 */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Denied or unavailable: try the legacy path below.
  }
  if (typeof document === 'undefined' || typeof document.execCommand !== 'function') return false;
  const area = document.createElement('textarea');
  area.value = text;
  area.setAttribute('readonly', '');
  area.style.position = 'fixed';
  area.style.top = '0';
  area.style.left = '0';
  area.style.opacity = '0';
  document.body.appendChild(area);
  const selection = document.getSelection();
  const previous = selection && selection.rangeCount > 0 ? selection.getRangeAt(0) : null;
  area.select();
  area.setSelectionRange(0, text.length);
  let ok: boolean;
  try {
    ok = document.execCommand('copy');
  } catch {
    ok = false;
  }
  area.remove();
  if (previous && selection) {
    selection.removeAllRanges();
    selection.addRange(previous);
  }
  return ok;
}

export type TextPart =
  { type: 'text'; text: string } | { type: 'code'; lang: string; code: string };

/**
 * Splits message text into prose and fenced code blocks (```lang ... ```).
 * An unterminated fence runs to the end, as it does while a reply streams.
 */
export function splitFences(text: string): TextPart[] {
  const parts: TextPart[] = [];
  const fence = /^[ \t]*(`{3,}|~{3,})[ \t]*([^\n`]*)\n/gm;
  let pos = 0;
  for (;;) {
    fence.lastIndex = pos;
    const open = fence.exec(text);
    if (!open) break;
    const before = text.slice(pos, open.index).replace(/\n+$/, '');
    if (before.trim()) parts.push({ type: 'text', text: before });
    const marker = open[1]!;
    const bodyStart = open.index + open[0].length;
    const close = new RegExp(
      `^[ \\t]*${marker[0] === '`' ? '`' : '~'}{${marker.length},}[ \\t]*$`,
      'm',
    );
    const rest = text.slice(bodyStart);
    const end = close.exec(rest);
    const code = end ? rest.slice(0, end.index) : rest;
    parts.push({ type: 'code', lang: open[2]!.trim(), code: code.replace(/\n$/, '') });
    if (!end) {
      pos = text.length;
      break;
    }
    pos = bodyStart + end.index + end[0].length;
    if (text[pos] === '\n') pos += 1;
  }
  const tail = text.slice(pos).replace(/^\n+/, '');
  if (tail.trim()) parts.push({ type: 'text', text: tail });
  return parts.length > 0 ? parts : [{ type: 'text', text }];
}
