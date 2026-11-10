/**
 * Helpers of the phone session layout (LiveView with `chrome`): when the top
 * bar slides away on scroll, and what counts as empty chat space for the
 * reading mode's double tap.
 */

/** Scroll distance (px) in one direction before the bar hides or comes back. */
export const BAR_HIDE_PX = 24;
export const BAR_SHOW_PX = 16;

/** The top bar's scroll state. */
export interface BarScroll {
  hidden: boolean;
  /** Where the current scroll direction started. */
  anchor: number;
  /** The last scroll position seen. */
  last: number;
  /** The current direction (-1 toward older messages, 1 toward newer, 0 none yet). */
  dir: -1 | 0 | 1;
}

export const BAR_SCROLL_START: BarScroll = { hidden: false, anchor: 0, last: 0, dir: 0 };

/**
 * The next bar state after the chat scrolled to `top`. The newest message is
 * at the bottom: scrolling up (toward older messages) by `BAR_HIDE_PX` hides
 * the bar, scrolling back down by `BAR_SHOW_PX`, or reaching the newest
 * message (`atBottom`), shows it. A jump bigger than the visible height
 * (older pages loading above, "jump to latest") is not the person scrolling:
 * it only moves the reference point.
 */
export function barOnScroll(s: BarScroll, top: number, atBottom: boolean, viewport = 0): BarScroll {
  if (atBottom) {
    return s.hidden || s.last !== top ? { hidden: false, anchor: top, last: top, dir: 0 } : s;
  }
  const delta = top - s.last;
  if (delta === 0) return s;
  if (viewport > 0 && Math.abs(delta) > viewport) {
    return { ...s, anchor: top, last: top, dir: 0 };
  }
  const dir: -1 | 1 = delta < 0 ? -1 : 1;
  const anchor = dir === s.dir ? s.anchor : s.last;
  const moved = top - anchor;
  let hidden = s.hidden;
  if (moved <= -BAR_HIDE_PX) hidden = true;
  else if (moved >= BAR_SHOW_PX) hidden = false;
  return { hidden, anchor, last: top, dir };
}

/** Sideways travel (px) on the title that counts as a swipe. */
export const SWIPE_PX = 40;

/**
 * The session step of a pointer move on the title (`dx`, `dy` from where it
 * went down): a mostly horizontal move of `SWIPE_PX` or more. Toward the
 * left is the next session (the cards move the same way), toward the right
 * the previous one; anything else is a tap or a scroll (`0`).
 */
export function swipeDirection(dx: number, dy: number): -1 | 0 | 1 {
  if (Math.abs(dx) < SWIPE_PX || Math.abs(dx) < Math.abs(dy) * 1.5) return 0;
  return dx < 0 ? 1 : -1;
}

/** What a tap on it does something else than reading mode (links, buttons, code). */
const INTERACTIVE =
  'a, button, input, textarea, select, summary, label, details, [role="button"], [role="link"], [role="menuitem"], [contenteditable=""], [contenteditable="true"], pre, code, img, video, svg, [data-no-reading-tap]';

/**
 * Whether a tap landed on empty chat space: not on a link, button, code or
 * other control, not on text (an element with its own text is selectable
 * text), and not while text is selected.
 */
export function isEmptyChatSpace(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  if (target.closest(INTERACTIVE)) return false;
  const selection = target.ownerDocument.getSelection?.();
  if (selection && !selection.isCollapsed && selection.toString().trim()) return false;
  for (const node of Array.from(target.childNodes)) {
    if (node.nodeType === Node.TEXT_NODE && node.textContent?.trim()) return false;
  }
  return true;
}
