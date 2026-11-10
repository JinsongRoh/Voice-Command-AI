import { useSyncExternalStore } from 'react';

/**
 * How much smaller than the full window (px) the visible area must get, with
 * a text field focused, before it counts as an on-screen keyboard. Browser
 * bars and the iOS accessory bar alone stay below this.
 */
export const KEYBOARD_MIN_PX = 120;

/** Re-checks after a focus change: iOS reports the keyboard late, or in steps. */
const FOCUS_SETTLE_MS = [50, 150, 350, 700];

export interface ViewportState {
  /** Visible height (px). */
  height: number;
  /** How far the visible area is shifted down inside the layout viewport (px). */
  offsetTop: number;
  /** An on-screen keyboard covers part of the window. */
  keyboard: boolean;
}

let current: ViewportState = { height: 0, offsetTop: 0, keyboard: false };
const listeners = new Set<() => void>();

function publish(next: ViewportState) {
  if (
    next.height === current.height &&
    next.offsetTop === current.offsetTop &&
    next.keyboard === current.keyboard
  ) {
    return;
  }
  current = next;
  for (const listener of listeners) listener();
}

/** The last published visible area (see `trackVisualViewport`). */
export function viewportState(): ViewportState {
  return current;
}

/** Calls `listener` whenever the visible area or the keyboard state changes. */
export function subscribeViewport(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Whether an on-screen keyboard is open (always false on desktops). */
export function useKeyboardOpen(): boolean {
  return useSyncExternalStore(
    subscribeViewport,
    () => current.keyboard,
    () => false,
  );
}

const TEXT_INPUTS = new Set([
  'text',
  'search',
  'email',
  'url',
  'tel',
  'password',
  'number',
  'date',
  'datetime-local',
  'month',
  'time',
  'week',
]);

/** Elements that bring up the on-screen keyboard when focused. */
export function isTextEntry(el: Element | null): el is HTMLElement {
  if (!el) return false;
  if (el.tagName === 'TEXTAREA') return !(el as HTMLTextAreaElement).readOnly;
  if (el.tagName === 'INPUT') {
    const input = el as HTMLInputElement;
    return TEXT_INPUTS.has(input.type) && !input.readOnly;
  }
  return (el as HTMLElement).isContentEditable === true;
}

function prefersTouch(target: Window): boolean {
  try {
    return target.matchMedia?.('(pointer: coarse)').matches ?? false;
  } catch {
    return false;
  }
}

/**
 * Undoes the scrolling a browser does to reveal a focused field: the page
 * itself and `overflow: hidden` panes are never meant to scroll here (the
 * shell is sized to the visible area instead). A scrolled page or pane
 * leaves a blank band where the composer should be.
 */
function unscroll(target: Window, focused: Element | null) {
  const doc = target.document;
  if (target.scrollY !== 0 || target.scrollX !== 0) target.scrollTo(0, 0);
  const scroller = doc.scrollingElement;
  if (scroller && scroller.scrollTop !== 0) scroller.scrollTop = 0;
  if (doc.body.scrollTop !== 0) doc.body.scrollTop = 0;
  for (let el = focused?.parentElement ?? null; el && el !== doc.body; el = el.parentElement) {
    if (el.scrollTop === 0) continue;
    const overflow = target.getComputedStyle(el).overflowY;
    if (overflow === 'hidden' || overflow === 'clip') el.scrollTop = 0;
  }
}

export interface TrackOptions {
  /**
   * Whether this device uses an on-screen keyboard (default: a coarse
   * pointer). Desktops never enter the keyboard layout, whatever the
   * window size.
   */
  touch?: boolean;
}

/**
 * Publishes the visual viewport as CSS variables on `<html>`:
 * - `--vvh`: visible height (smaller while the on-screen keyboard is open);
 * - `--vv-top`: how far the visible area is shifted (iOS pans it to reveal
 *   the caret);
 * - `data-keyboard="open"` while an on-screen keyboard covers the window.
 *
 * The app shell sizes and moves itself with them, so the composer sits right
 * above the keyboard. iOS keeps the layout viewport at full size and only
 * shrinks the visual viewport (and may scroll the page or pan it); Android
 * WebViews usually shrink the window itself. Both are handled: the keyboard
 * is the difference to the tallest visible height seen at this width, and
 * any page scroll the browser does to reveal the field is undone.
 *
 * Listens to `visualViewport` `resize` and `scroll`, the window `scroll`,
 * and focus changes (re-checked while the keyboard animates in).
 */
export function trackVisualViewport(target: Window = window, options: TrackOptions = {}) {
  const vv = target.visualViewport;
  const doc = target.document;
  const root = doc.documentElement;
  if (!vv) return () => {};
  const touch = options.touch ?? prefersTouch(target);
  let baseWidth = -1;
  let baseHeight = 0;
  let lastVvh = '';
  let lastTop = '';
  const timers: ReturnType<typeof setTimeout>[] = [];

  const apply = () => {
    const width = Math.round(vv.width || target.innerWidth);
    const focused = doc.activeElement;
    const typing = touch && isTextEntry(focused);
    // A new width (rotation, split screen) starts a new full-height reference.
    if (width !== baseWidth) {
      baseWidth = width;
      baseHeight = 0;
    }
    baseHeight = Math.max(baseHeight, Math.round(target.innerHeight), Math.round(vv.height));
    const keyboard = typing && baseHeight - vv.height > KEYBOARD_MIN_PX;
    // The page never scrolls (the shell follows the visible area): undo what
    // the browser scrolled to reveal the field, also after the keyboard
    // closed. Pinch zoom pans on purpose; leave that alone.
    if (touch && vv.scale <= 1.01) unscroll(target, focused);
    const height = Math.round(vv.height);
    const offsetTop = Math.max(0, Math.round(vv.offsetTop));
    const vvh = `${height}px`;
    const top = `${offsetTop}px`;
    if (vvh !== lastVvh) root.style.setProperty('--vvh', (lastVvh = vvh));
    if (top !== lastTop) root.style.setProperty('--vv-top', (lastTop = top));
    if (keyboard) root.dataset.keyboard = 'open';
    else delete root.dataset.keyboard;
    publish({ height, offsetTop, keyboard });
  };

  const settle = () => {
    apply();
    for (const timer of timers.splice(0)) clearTimeout(timer);
    for (const ms of FOCUS_SETTLE_MS) timers.push(setTimeout(apply, ms));
  };

  apply();
  vv.addEventListener('resize', apply);
  vv.addEventListener('scroll', apply);
  target.addEventListener('scroll', apply);
  doc.addEventListener('focusin', settle);
  doc.addEventListener('focusout', settle);
  return () => {
    for (const timer of timers.splice(0)) clearTimeout(timer);
    vv.removeEventListener('resize', apply);
    vv.removeEventListener('scroll', apply);
    target.removeEventListener('scroll', apply);
    doc.removeEventListener('focusin', settle);
    doc.removeEventListener('focusout', settle);
    root.style.removeProperty('--vvh');
    root.style.removeProperty('--vv-top');
    delete root.dataset.keyboard;
    publish({ height: 0, offsetTop: 0, keyboard: false });
  };
}
