/**
 * A compact GitHub-flavored markdown parser for agent messages. It produces
 * a small tree that `ChatView` renders as React elements, so raw HTML in a
 * message is never interpreted (it shows as text) and nothing needs
 * sanitizing. Supported: paragraphs (line breaks kept), ATX headings,
 * fenced code, block quotes, ordered and unordered (nested) lists, GFM
 * tables, thematic breaks, and inline code, bold, italic, strikethrough,
 * links and autolinks (http, https and mailto only).
 *
 * `markdownRuns` lists the text runs in render order: chat search counts
 * matches per run, so its numbering lines up with the highlighted marks.
 */

export type Inline =
  | { type: 'text'; text: string }
  | { type: 'code'; text: string }
  | { type: 'strong' | 'em' | 'del'; children: Inline[] }
  | { type: 'link'; href: string; children: Inline[] };

export type Align = 'left' | 'center' | 'right' | null;

export type Block =
  | { type: 'p'; inline: Inline[] }
  | { type: 'h'; level: 1 | 2 | 3 | 4 | 5 | 6; inline: Inline[] }
  | { type: 'code'; lang: string; code: string }
  | { type: 'hr' }
  | { type: 'quote'; blocks: Block[] }
  | { type: 'list'; ordered: boolean; start: number; items: Block[][] }
  | { type: 'table'; align: Align[]; head: Inline[][]; rows: Inline[][][] };

// ---------------------------------------------------------------- blocks

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([^`\s]*)[^`]*$/;
const HR = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/;
const QUOTE = /^ {0,3}> ?(.*)$/;
const LIST_ITEM = /^( {0,3})([-*+]|\d{1,9}[.)])([ \t]+(.*))?$/;
const TABLE_DELIM = /^ {0,3}\|?[ \t]*:?-+:?[ \t]*(\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/;

const isBlank = (line: string) => line.trim() === '';

function splitRow(line: string): string[] {
  let s = line.trim();
  if (s.startsWith('|')) s = s.slice(1);
  if (s.endsWith('|') && !s.endsWith('\\|')) s = s.slice(0, -1);
  const cells: string[] = [];
  let cell = '';
  let inCode = false;
  for (let i = 0; i < s.length; i++) {
    const c = s[i]!;
    if (c === '\\' && s[i + 1] === '|') {
      cell += '|';
      i++;
    } else if (c === '`') {
      inCode = !inCode;
      cell += c;
    } else if (c === '|' && !inCode) {
      cells.push(cell.trim());
      cell = '';
    } else {
      cell += c;
    }
  }
  cells.push(cell.trim());
  return cells;
}

function isTableStart(lines: string[], i: number): boolean {
  const head = lines[i];
  const delim = lines[i + 1];
  if (head === undefined || delim === undefined) return false;
  if (!head.includes('|') || !TABLE_DELIM.test(delim) || !delim.includes('-')) return false;
  // A lone "---" under text is a setext heading, not a table.
  if (!delim.includes('|') && !head.trim().startsWith('|')) return false;
  return splitRow(head).length === splitRow(delim).length;
}

/** Starts a block other than a paragraph (ends a running paragraph). */
function startsBlock(lines: string[], i: number): boolean {
  const line = lines[i]!;
  return (
    FENCE.test(line) ||
    HR.test(line) ||
    HEADING.test(line) ||
    QUOTE.test(line) ||
    LIST_ITEM.test(line) ||
    isTableStart(lines, i)
  );
}

function parseLines(lines: string[], depth: number): Block[] {
  const out: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i]!;
    if (isBlank(line)) {
      i++;
      continue;
    }
    const fence = FENCE.exec(line);
    if (fence) {
      const marker = fence[1]!;
      const body: string[] = [];
      i++;
      while (i < lines.length) {
        const l = lines[i]!;
        const t = l.trim();
        if (t.startsWith(marker[0]!.repeat(marker.length)) && /^[`~]+$/.test(t)) {
          i++;
          break;
        }
        body.push(l);
        i++;
      }
      out.push({ type: 'code', lang: fence[2] ?? '', code: body.join('\n') });
      continue;
    }
    if (HR.test(line)) {
      out.push({ type: 'hr' });
      i++;
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      out.push({
        type: 'h',
        level: heading[1]!.length as 1 | 2 | 3 | 4 | 5 | 6,
        inline: parseInline(heading[2] ?? ''),
      });
      i++;
      continue;
    }
    if (QUOTE.test(line) && depth < 8) {
      const inner: string[] = [];
      while (i < lines.length && QUOTE.test(lines[i]!)) {
        inner.push(QUOTE.exec(lines[i]!)![1]!);
        i++;
      }
      out.push({ type: 'quote', blocks: parseLines(inner, depth + 1) });
      continue;
    }
    if (isTableStart(lines, i)) {
      const head = splitRow(line).map(parseInline);
      const align: Align[] = splitRow(lines[i + 1]!).map((c) => {
        const l = c.startsWith(':');
        const r = c.endsWith(':');
        return l && r ? 'center' : r ? 'right' : l ? 'left' : null;
      });
      i += 2;
      const rows: Inline[][][] = [];
      while (i < lines.length && !isBlank(lines[i]!) && lines[i]!.includes('|')) {
        const cells = splitRow(lines[i]!);
        rows.push(align.map((_, c) => parseInline(cells[c] ?? '')));
        i++;
      }
      out.push({ type: 'table', align, head, rows });
      continue;
    }
    const item = LIST_ITEM.exec(line);
    if (item && depth < 8) {
      const ordered = /\d/.test(item[2]!);
      const indent = item[1]!.length;
      const start = ordered ? parseInt(item[2]!, 10) : 1;
      const items: Block[][] = [];
      while (i < lines.length) {
        const m = LIST_ITEM.exec(lines[i]!);
        if (!m || m[1]!.length !== indent || /\d/.test(m[2]!) !== ordered) break;
        const contentIndent = indent + m[2]!.length + 1;
        const body: string[] = [m[4] ?? ''];
        i++;
        while (i < lines.length) {
          const l = lines[i]!;
          if (isBlank(l)) {
            // A blank line continues the item only if indented text follows.
            const next = lines[i + 1];
            if (
              next !== undefined &&
              !isBlank(next) &&
              leading(next) >= Math.min(contentIndent, indent + 2)
            ) {
              body.push('');
              i++;
              continue;
            }
            break;
          }
          const lead = leading(l);
          if (lead >= Math.min(contentIndent, indent + 2)) {
            body.push(l.slice(Math.min(lead, contentIndent)));
            i++;
            continue;
          }
          // Lazy continuation of the item's paragraph.
          if (!startsBlock(lines, i)) {
            body.push(l.trim());
            i++;
            continue;
          }
          break;
        }
        items.push(parseLines(body, depth + 1));
        if (i < lines.length && isBlank(lines[i]!)) {
          // A blank line between items keeps the list going.
          const next = lines[i + 1];
          const nm = next === undefined ? null : LIST_ITEM.exec(next);
          if (nm && nm[1]!.length === indent && /\d/.test(nm[2]!) === ordered) i++;
          else break;
        }
      }
      out.push({ type: 'list', ordered, start, items });
      continue;
    }
    // Paragraph: until a blank line or another block.
    const para: string[] = [line.trim()];
    i++;
    while (i < lines.length && !isBlank(lines[i]!) && !startsBlock(lines, i)) {
      para.push(lines[i]!.trim());
      i++;
    }
    out.push({ type: 'p', inline: parseInline(para.join('\n')) });
  }
  return out;
}

function leading(line: string): number {
  let n = 0;
  for (const c of line) {
    if (c === ' ') n += 1;
    else if (c === '\t') n += 4;
    else break;
  }
  return n;
}

// ---------------------------------------------------------------- inline

const ESCAPABLE = /[\\`*_{}[\]()#+\-.!|~<>"']/;
const AUTOLINK = /^<((?:https?:\/\/|mailto:)[^>\s]+)>/i;
const BARE_URL = /^https?:\/\/[^\s<>]*[^\s<>.,:;"'!?)\]]/i;

/** Only web and mail links; anything else (javascript:, file:, data:) stays text. */
export function safeHref(href: string): string | null {
  const h = href.trim();
  return /^(https?:\/\/|mailto:)/i.test(h) ? h : null;
}

function isWordChar(c: string | undefined): boolean {
  return c !== undefined && /[\p{L}\p{N}_]/u.test(c);
}

export function parseInline(src: string, depth = 0): Inline[] {
  const out: Inline[] = [];
  let text = '';
  const flush = () => {
    if (text) out.push({ type: 'text', text });
    text = '';
  };
  let i = 0;
  while (i < src.length) {
    const c = src[i]!;
    if (c === '\\' && i + 1 < src.length && ESCAPABLE.test(src[i + 1]!)) {
      text += src[i + 1];
      i += 2;
      continue;
    }
    if (c === '<') {
      // Agents write `<br>` for line breaks inside table cells; show a line
      // break, never the tag. No other HTML is interpreted.
      const br = /^<br\s*\/?>/i.exec(src.slice(i, i + 8));
      if (br) {
        text += '\n';
        i += br[0].length;
        continue;
      }
    }
    if (c === '`') {
      let n = 1;
      while (src[i + n] === '`') n++;
      const ticks = '`'.repeat(n);
      const end = src.indexOf(ticks, i + n);
      if (end > 0 && src[end + n] !== '`') {
        flush();
        let code = src.slice(i + n, end);
        if (code.startsWith(' ') && code.endsWith(' ') && code.trim()) code = code.slice(1, -1);
        out.push({ type: 'code', text: code.replace(/\n/g, ' ') });
        i = end + n;
        continue;
      }
      text += ticks;
      i += n;
      continue;
    }
    if (depth < 6) {
      const pair = src.slice(i, i + 2);
      if (pair === '**' || pair === '__' || pair === '~~') {
        const end = findClose(src, pair, i + 2);
        if (end > i + 2 && !(pair === '__' && isWordChar(src[i - 1]))) {
          flush();
          out.push({
            type: pair === '~~' ? 'del' : 'strong',
            children: parseInline(src.slice(i + 2, end), depth + 1),
          });
          i = end + 2;
          continue;
        }
      }
      if ((c === '*' || c === '_') && src[i + 1] !== c && src[i + 1] && !/\s/.test(src[i + 1]!)) {
        if (!(c === '_' && isWordChar(src[i - 1]))) {
          const end = findClose(src, c, i + 1);
          if (end > i + 1 && !(c === '_' && isWordChar(src[end + 1]))) {
            flush();
            out.push({ type: 'em', children: parseInline(src.slice(i + 1, end), depth + 1) });
            i = end + 1;
            continue;
          }
        }
      }
      if (c === '[') {
        const link = parseLink(src, i);
        if (link) {
          flush();
          const children = parseInline(link.label, depth + 1);
          const href = safeHref(link.href);
          if (href) out.push({ type: 'link', href, children });
          else out.push(...children);
          i = link.end;
          continue;
        }
      }
    }
    if (c === '<') {
      const m = AUTOLINK.exec(src.slice(i));
      if (m) {
        flush();
        out.push({ type: 'link', href: m[1]!, children: [{ type: 'text', text: m[1]! }] });
        i += m[0].length;
        continue;
      }
    }
    if ((c === 'h' || c === 'H') && !isWordChar(src[i - 1])) {
      const m = BARE_URL.exec(src.slice(i));
      if (m) {
        flush();
        out.push({ type: 'link', href: m[0], children: [{ type: 'text', text: m[0] }] });
        i += m[0].length;
        continue;
      }
    }
    text += c;
    i++;
  }
  flush();
  return out;
}

/** Index of the closing delimiter (not preceded by whitespace), or -1. */
function findClose(src: string, delim: string, from: number): number {
  let i = from;
  while (i < src.length) {
    if (src[i] === '\\') {
      i += 2;
      continue;
    }
    if (src[i] === '`') {
      const end = src.indexOf('`', i + 1);
      if (end < 0) return -1;
      i = end + 1;
      continue;
    }
    if (src.startsWith(delim, i) && !/\s/.test(src[i - 1] ?? ' ')) {
      // `**` inside `*…*` is not the end of the emphasis.
      if (delim.length === 1 && src[i + 1] === delim) {
        i += 2;
        continue;
      }
      return i;
    }
    i++;
  }
  return -1;
}

function parseLink(src: string, at: number): { label: string; href: string; end: number } | null {
  let depth = 0;
  let close = -1;
  for (let i = at; i < src.length; i++) {
    if (src[i] === '\\') {
      i++;
      continue;
    }
    if (src[i] === '[') depth++;
    else if (src[i] === ']' && --depth === 0) {
      close = i;
      break;
    }
  }
  if (close < 0 || src[close + 1] !== '(') return null;
  // The destination may hold balanced parentheses: (https://x/a_(b)).
  let end = -1;
  let parens = 0;
  for (let i = close + 2; i < src.length; i++) {
    if (src[i] === '(') parens++;
    else if (src[i] === ')' && parens-- === 0) {
      end = i;
      break;
    }
  }
  if (end < 0) return null;
  let href = src.slice(close + 2, end).trim();
  // Drop an optional title: [x](url "title").
  href = href.replace(/\s+("[^"]*"|'[^']*')$/, '');
  if (href.startsWith('<') && href.endsWith('>')) href = href.slice(1, -1);
  if (/\s/.test(href)) return null;
  return { label: src.slice(at + 1, close), href, end: end + 1 };
}

// ---------------------------------------------------------------- api

const cache = new Map<string, Block[]>();

export function parseMarkdown(text: string): Block[] {
  let blocks = cache.get(text);
  if (!blocks) {
    blocks = parseLines(text.replace(/\r\n?/g, '\n').split('\n'), 0);
    if (cache.size > 300) cache.clear();
    cache.set(text, blocks);
  }
  return blocks;
}

function inlineRuns(nodes: Inline[], out: string[]) {
  for (const n of nodes) {
    if (n.type === 'text' || n.type === 'code') out.push(n.text);
    else inlineRuns(n.children, out);
  }
}

function blockRuns(blocks: Block[], out: string[]) {
  for (const b of blocks) {
    switch (b.type) {
      case 'p':
      case 'h':
        inlineRuns(b.inline, out);
        break;
      case 'code':
        out.push(b.code);
        break;
      case 'quote':
        blockRuns(b.blocks, out);
        break;
      case 'list':
        for (const item of b.items) blockRuns(item, out);
        break;
      case 'table':
        for (const cell of b.head) inlineRuns(cell, out);
        for (const row of b.rows) for (const cell of row) inlineRuns(cell, out);
        break;
      case 'hr':
        break;
    }
  }
}

/** The visible text runs of rendered markdown, in render order (for search). */
export function markdownRuns(text: string): string[] {
  const out: string[] = [];
  blockRuns(parseMarkdown(text), out);
  return out;
}

/** Whether rendering changes anything (otherwise no source toggle is offered). */
export function hasMarkdown(text: string): boolean {
  const blocks = parseMarkdown(text);
  return blocks.some(
    (b) =>
      b.type !== 'p' || b.inline.some((n) => n.type !== 'text') || /\\[\\`*_[\]#|~]/.test(text),
  );
}
