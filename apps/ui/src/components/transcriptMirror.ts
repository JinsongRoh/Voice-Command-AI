import { correctionRanges, type TranscriptCorrection } from '../speech/transcribe';

export interface MirrorPart {
  text: string;
  /** A vocabulary correction (highlighted). */
  mark: boolean;
  /** Part of the interim dictation tail (dimmed). */
  tail: boolean;
}

/** Splits `value` into mirror parts: correction marks and the dimmed live tail. */
export function mirrorParts(
  value: string,
  corrections: TranscriptCorrection[],
  tailLength = 0,
): MirrorPart[] {
  const tail = Math.min(Math.max(tailLength, 0), value.length);
  const tailStart = value.length - tail;
  const out: MirrorPart[] = [];
  const push = (start: number, end: number, mark: boolean) => {
    if (end <= start) return;
    if (start < tailStart && end > tailStart) {
      out.push({ text: value.slice(start, tailStart), mark, tail: false });
      out.push({ text: value.slice(tailStart, end), mark, tail: true });
    } else {
      out.push({ text: value.slice(start, end), mark, tail: tail > 0 && start >= tailStart });
    }
  };
  let pos = 0;
  for (const r of correctionRanges(value, corrections)) {
    push(pos, r.start, false);
    push(r.start, r.end, true);
    pos = r.end;
  }
  push(pos, value.length, false);
  return out;
}

/**
 * Sizes a textarea to its content. CSS owns the limits (`min-height` gives
 * ~3 lines on phones, `max-height` caps it at ~40% of the viewport, or ~65%
 * when expanded); beyond that the textarea scrolls.
 */
export function autoGrow(el: HTMLTextAreaElement): void {
  el.style.height = 'auto';
  el.style.height = `${el.scrollHeight}px`;
}

/**
 * Top (px, in the scrolled content's coordinates) of the line holding text
 * offset `offset` in a mirror element: the mirror wraps the text exactly
 * like the textarea above it, so this is where the textarea's caret is.
 * `null` when it cannot be measured (no layout, offset past the text).
 */
export function caretOffsetTop(mirror: HTMLElement, offset: number): number | null {
  const doc = mirror.ownerDocument;
  const walker = doc.createTreeWalker(mirror, NodeFilter.SHOW_TEXT);
  let left = offset;
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const length = node.textContent?.length ?? 0;
    if (left <= length) {
      const range = doc.createRange();
      range.setStart(node, left);
      range.setEnd(node, Math.min(length, left + 1));
      // Not every environment lays text out (jsdom has no range geometry).
      if (typeof range.getClientRects !== 'function') return null;
      const rect = range.getClientRects()[0];
      if (!rect || (rect.top === 0 && rect.height === 0)) return null;
      return rect.top - mirror.getBoundingClientRect().top + mirror.scrollTop;
    }
    left -= length;
  }
  return null;
}
