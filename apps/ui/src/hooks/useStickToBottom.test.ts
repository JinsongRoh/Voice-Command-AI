import { describe, expect, it } from 'vitest';

import { isAtBottom, nextStick, STICK_THRESHOLD } from './useStickToBottom';

const m = (scrollTop: number, scrollHeight = 2000, clientHeight = 500) => ({
  scrollTop,
  scrollHeight,
  clientHeight,
});

describe('isAtBottom', () => {
  it('allows a small threshold', () => {
    expect(isAtBottom(m(1500))).toBe(true);
    expect(isAtBottom(m(1500 - STICK_THRESHOLD))).toBe(true);
    expect(isAtBottom(m(1500 - STICK_THRESHOLD - 1))).toBe(false);
  });
});

describe('nextStick', () => {
  it('unsticks when the user scrolls up', () => {
    expect(nextStick(true, 1500, m(1200))).toBe(false);
  });

  it('stays stuck when content grows below the viewport', () => {
    // scrollTop unchanged, the list got taller (an image loaded).
    expect(nextStick(true, 1500, m(1500, 2600))).toBe(true);
  });

  it('stays unstuck while reading history, even when scrolling down a bit', () => {
    expect(nextStick(false, 800, m(900))).toBe(false);
    expect(nextStick(false, 800, m(800, 2600))).toBe(false);
  });

  it('re-sticks when the user returns to the bottom', () => {
    expect(nextStick(false, 1000, m(1480))).toBe(true);
  });

  it('ignores sub-pixel jitter', () => {
    expect(nextStick(true, 1000.5, m(1000))).toBe(true);
  });

  it('unsticks on a small deliberate move up, not only past the threshold', () => {
    expect(nextStick(true, 1500, m(1470))).toBe(false);
  });

  it('keeps following when a layout change (not the user) moved the position', () => {
    // The browser clamped the position while the list was shorter and the
    // list has grown again before the scroll event arrived.
    expect(nextStick(true, 1500, m(500, 2400), true)).toBe(true);
    // The same numbers without a layout change are the user scrolling up.
    expect(nextStick(true, 1500, m(500, 2400), false)).toBe(false);
  });

  it('does not re-stick on a layout change while the user reads history', () => {
    expect(nextStick(false, 800, m(300, 2600), true)).toBe(false);
  });

  it('treats settling at the very end after a bounce as the bottom', () => {
    expect(nextStick(true, 1530, m(1500))).toBe(true);
  });
});
