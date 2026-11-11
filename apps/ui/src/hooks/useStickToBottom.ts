import { type RefObject, useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';

/** Within this many pixels of the end counts as "at the bottom". */
export const STICK_THRESHOLD = 48;

export interface ScrollMetrics {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

export function isAtBottom(m: ScrollMetrics, threshold = STICK_THRESHOLD): boolean {
  return m.scrollHeight - m.scrollTop - m.clientHeight <= threshold;
}

/** Pixels the position may sit off the very end and still count as exact. */
const EXACT = 2;

/**
 * Whether the view keeps following new content after a scroll event.
 *
 * Reaching the bottom sticks; moving up (a finger drag, wheel or scrollbar)
 * unsticks, however small the move. Anything else keeps the previous state:
 * content growing below the viewport, the browser clamping the position, or
 * `layoutShifted` (the content or viewport size changed since the position
 * was last recorded), so late layout such as images, fonts or a keyboard is
 * never mistaken for the user scrolling away. Settling at the very end after
 * a bounce counts as reaching the bottom.
 */
export function nextStick(
  stick: boolean,
  lastTop: number,
  m: ScrollMetrics,
  layoutShifted = false,
): boolean {
  if (isAtBottom(m, layoutShifted ? STICK_THRESHOLD : EXACT)) return true;
  if (layoutShifted) return stick;
  const movedUp = m.scrollTop < lastTop - 1;
  if (movedUp) return false;
  // Moving down or standing still: near the end sticks, otherwise unchanged.
  return isAtBottom(m) ? true : stick;
}

export interface StickToBottom<S extends HTMLElement, C extends HTMLElement> {
  /** The scrolling element. */
  scrollRef: RefObject<S | null>;
  /** The element holding the content (observed for size changes). */
  contentRef: RefObject<C | null>;
  onScroll: () => void;
  /** Whether the view follows new content. */
  stuck: boolean;
  /** Items added while the user was reading history. */
  unseen: number;
  /** Scrolls to the end and follows new content again. */
  jumpToLatest: () => void;
  /** Stops following, for example while the user expands an item to read it. */
  release: () => void;
}

/**
 * Chat-style scrolling: start at the bottom, follow new content while the
 * user is at the bottom, and leave them alone while they read history.
 *
 * `count` is the number of items (new ones are counted while unstuck) and
 * `tail` changes whenever the last item grows in place.
 */
export function useStickToBottom<S extends HTMLElement, C extends HTMLElement>(
  count: number,
  tail: string,
): StickToBottom<S, C> {
  const scrollRef = useRef<S>(null);
  const contentRef = useRef<C>(null);
  const stick = useRef(true);
  const lastTop = useRef(0);
  /** Content and viewport height when the position was last recorded. */
  const lastSize = useRef({ height: 0, client: 0 });
  const lastCount = useRef(count);
  const [stuck, setStuck] = useState(true);
  const [unseen, setUnseen] = useState(0);

  const setStick = useCallback((value: boolean) => {
    stick.current = value;
    setStuck(value);
    if (value) setUnseen(0);
  }, []);

  const record = useCallback((el: HTMLElement) => {
    lastTop.current = el.scrollTop;
    lastSize.current = { height: el.scrollHeight, client: el.clientHeight };
  }, []);

  const toBottom = useCallback(
    (smooth = false) => {
      const el = scrollRef.current;
      if (!el) return;
      const top = el.scrollHeight;
      if (smooth && typeof el.scrollTo === 'function') {
        el.scrollTo({ top, behavior: 'smooth' });
      } else {
        el.scrollTop = top;
      }
      record(el);
    },
    [record],
  );

  // New or growing content: follow it, or count it while the user reads.
  useLayoutEffect(() => {
    const added = count - lastCount.current;
    lastCount.current = count;
    if (stick.current) {
      toBottom();
      // Once more after the browser has laid out the new content.
      const frame = requestAnimationFrame(() => {
        if (stick.current) toBottom();
      });
      return () => cancelAnimationFrame(frame);
    }
    if (added > 0) setUnseen((n) => n + added);
    return undefined;
  }, [count, tail, toBottom]);

  // Late layout changes the height without a new item: images and fonts
  // loading, rows expanding, the keyboard or composer resizing the view.
  useEffect(() => {
    const el = scrollRef.current;
    const content = contentRef.current;
    const onResize = () => {
      if (stick.current) toBottom();
      else if (el && isAtBottom(el, EXACT)) setStick(true);
    };
    let observer: ResizeObserver | undefined;
    if (typeof ResizeObserver !== 'undefined') {
      observer = new ResizeObserver(onResize);
      if (el) observer.observe(el);
      if (content) observer.observe(content);
    }
    let alive = true;
    void document.fonts?.ready.then(() => {
      if (alive) onResize();
    });
    return () => {
      alive = false;
      observer?.disconnect();
    };
  }, [toBottom, setStick]);

  const onScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const shifted =
      el.scrollHeight !== lastSize.current.height || el.clientHeight !== lastSize.current.client;
    const next = nextStick(stick.current, lastTop.current, el, shifted);
    record(el);
    if (next !== stick.current) setStick(next);
  }, [setStick, record]);

  const jumpToLatest = useCallback(() => {
    setStick(true);
    toBottom(true);
  }, [setStick, toBottom]);

  const release = useCallback(() => {
    if (stick.current) setStick(false);
  }, [setStick]);

  return { scrollRef, contentRef, onScroll, stuck, unseen, jumpToLatest, release };
}
