import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { installFakeViewport, type FakeViewport } from '../test/fakeViewport';
import {
  KEYBOARD_MIN_PX,
  subscribeViewport,
  trackVisualViewport,
  useKeyboardOpen,
  viewportState,
} from './visualViewport';

const root = document.documentElement;
const cssVar = (name: string) => root.style.getPropertyValue(name);

let viewport: FakeViewport;
let stop: () => void = () => {};
let field: HTMLTextAreaElement;

beforeEach(() => {
  viewport = installFakeViewport(390, 844);
  field = document.createElement('textarea');
  document.body.append(field);
});
afterEach(() => {
  stop();
  stop = () => {};
  field.remove();
  viewport.restore();
  vi.restoreAllMocks();
});

describe('trackVisualViewport', () => {
  it('publishes the visible height and offset, on resize and on scroll', () => {
    stop = trackVisualViewport(window, { touch: true });
    expect(cssVar('--vvh')).toBe('844px');
    expect(cssVar('--vv-top')).toBe('0px');

    viewport.keyboard(500);
    expect(cssVar('--vvh')).toBe('500px');

    // iOS pans the visible area to reveal the caret: only `scroll` fires.
    viewport.pan(120);
    expect(cssVar('--vv-top')).toBe('120px');
    expect(viewportState()).toMatchObject({ height: 500, offsetTop: 120 });
  });

  it('reports the iOS keyboard: the visual viewport shrinks while a text field has focus', () => {
    stop = trackVisualViewport(window, { touch: true });
    field.focus();
    viewport.keyboard(500);
    expect(viewportState().keyboard).toBe(true);
    expect(root.dataset.keyboard).toBe('open');

    viewport.close();
    expect(viewportState().keyboard).toBe(false);
    expect(root.dataset.keyboard).toBeUndefined();
  });

  it('reports the Android keyboard: the whole window shrinks', () => {
    stop = trackVisualViewport(window, { touch: true });
    field.focus();
    viewport.resizeWindow(500);
    expect(viewportState().keyboard).toBe(true);
    expect(cssVar('--vvh')).toBe('500px');
  });

  it('ignores small changes (browser bars, the accessory bar alone)', () => {
    stop = trackVisualViewport(window, { touch: true });
    field.focus();
    viewport.keyboard(844 - KEYBOARD_MIN_PX + 10);
    expect(viewportState().keyboard).toBe(false);
  });

  it('needs a focused text field (a shorter window alone is no keyboard)', () => {
    stop = trackVisualViewport(window, { touch: true });
    viewport.keyboard(500);
    expect(viewportState().keyboard).toBe(false);
  });

  it('starts a new reference height on rotation', () => {
    stop = trackVisualViewport(window, { touch: true });
    field.focus();
    // Landscape: shorter and wider, no keyboard.
    viewport.resizeWindow(390, 844);
    expect(viewportState().keyboard).toBe(false);
  });

  it('never enters the keyboard layout on desktops', () => {
    stop = trackVisualViewport(window, { touch: false });
    field.focus();
    viewport.resizeWindow(400);
    expect(viewportState().keyboard).toBe(false);
    expect(root.dataset.keyboard).toBeUndefined();
  });

  it('checks again while the keyboard animates in after a focus', () => {
    vi.useFakeTimers();
    try {
      stop = trackVisualViewport(window, { touch: true });
      // The keyboard is already reported, but no event follows the focus.
      Object.defineProperty(window.visualViewport, 'height', {
        configurable: true,
        get: () => 480,
      });
      field.focus();
      vi.advanceTimersByTime(1000);
      expect(viewportState().keyboard).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it('undoes the page and pane scrolling the browser did to reveal the field', () => {
    const pane = document.createElement('div');
    pane.style.overflowY = 'hidden';
    pane.append(field);
    document.body.append(pane);
    // jsdom has no layout: give the pane a scroll position it keeps.
    let paneTop = 90;
    Object.defineProperty(pane, 'scrollTop', {
      configurable: true,
      get: () => paneTop,
      set: (v: number) => {
        paneTop = v;
      },
    });
    const savedScrollY = Object.getOwnPropertyDescriptor(window, 'scrollY');
    Object.defineProperty(window, 'scrollY', { configurable: true, value: 200 });
    const scrollTo = vi.spyOn(window, 'scrollTo').mockImplementation(() => {});
    try {
      stop = trackVisualViewport(window, { touch: true });
      field.focus();
      viewport.keyboard(500, 200);
      expect(scrollTo).toHaveBeenCalledWith(0, 0);
      expect(pane.scrollTop).toBe(0);
    } finally {
      if (savedScrollY) Object.defineProperty(window, 'scrollY', savedScrollY);
      else delete (window as unknown as Record<string, unknown>).scrollY;
      pane.remove();
    }
  });

  it('notifies subscribers and the hook', () => {
    stop = trackVisualViewport(window, { touch: true });
    const listener = vi.fn();
    const unsubscribe = subscribeViewport(listener);
    const { result } = renderHook(() => useKeyboardOpen());
    expect(result.current).toBe(false);
    act(() => {
      field.focus();
      viewport.keyboard(500);
    });
    expect(result.current).toBe(true);
    expect(listener).toHaveBeenCalled();
    unsubscribe();
  });

  it('cleans up its listeners and variables', () => {
    stop = trackVisualViewport(window, { touch: true });
    field.focus();
    viewport.keyboard(500);
    stop();
    stop = () => {};
    expect(cssVar('--vvh')).toBe('');
    expect(root.dataset.keyboard).toBeUndefined();
    viewport.keyboard(400);
    expect(cssVar('--vvh')).toBe('');
  });
});
