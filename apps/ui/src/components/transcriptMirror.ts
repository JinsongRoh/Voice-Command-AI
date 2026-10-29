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
