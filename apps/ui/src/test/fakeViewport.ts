/**
 * A controllable `window.visualViewport` for jsdom (which has none), plus a
 * window height that can shrink like an Android WebView's does.
 */
export interface FakeViewport {
  /** iOS: only the visual viewport shrinks (the layout viewport stays). */
  keyboard(height: number, offsetTop?: number): void;
  /** Android WebView: the whole window shrinks with the keyboard. */
  resizeWindow(height: number, width?: number): void;
  /** iOS pans the visible area (`visualViewport` `scroll`). */
  pan(offsetTop: number): void;
  close(): void;
  restore(): void;
}

export function installFakeViewport(width = 390, height = 844): FakeViewport {
  const events = new EventTarget();
  const state = { width, height, vvHeight: height, offsetTop: 0, scale: 1 };
  const vv = {
    get width() {
      return state.width;
    },
    get height() {
      return state.vvHeight;
    },
    get offsetTop() {
      return state.offsetTop;
    },
    get offsetLeft() {
      return 0;
    },
    get scale() {
      return state.scale;
    },
    addEventListener: events.addEventListener.bind(events),
    removeEventListener: events.removeEventListener.bind(events),
  };
  const saved = {
    vv: Object.getOwnPropertyDescriptor(window, 'visualViewport'),
    innerHeight: Object.getOwnPropertyDescriptor(window, 'innerHeight'),
    innerWidth: Object.getOwnPropertyDescriptor(window, 'innerWidth'),
  };
  Object.defineProperty(window, 'visualViewport', { configurable: true, get: () => vv });
  Object.defineProperty(window, 'innerHeight', { configurable: true, get: () => state.height });
  Object.defineProperty(window, 'innerWidth', { configurable: true, get: () => state.width });
  const fire = (type: string) => events.dispatchEvent(new Event(type));
  return {
    keyboard(h, offsetTop = 0) {
      state.vvHeight = h;
      state.offsetTop = offsetTop;
      fire('resize');
      if (offsetTop) fire('scroll');
    },
    resizeWindow(h, w = state.width) {
      state.height = h;
      state.width = w;
      state.vvHeight = h;
      fire('resize');
    },
    pan(offsetTop) {
      state.offsetTop = offsetTop;
      fire('scroll');
    },
    close() {
      state.vvHeight = state.height;
      state.offsetTop = 0;
      fire('resize');
    },
    restore() {
      for (const [key, desc] of Object.entries({
        visualViewport: saved.vv,
        innerHeight: saved.innerHeight,
        innerWidth: saved.innerWidth,
      })) {
        if (desc) Object.defineProperty(window, key, desc);
        else delete (window as unknown as Record<string, unknown>)[key];
      }
    },
  };
}
