import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import type { SessionEvent } from '../protocol/types';
import { ChatView, type ChatViewProps } from './ChatView';

const VIEWPORT = 500;
let top = 0;

const message = (seq: number): SessionEvent => ({
  type: 'message',
  seq,
  at: seq,
  role: 'assistant',
  text: `reply ${seq}`,
});
const range = (from: number, to: number) =>
  Array.from({ length: to - from + 1 }, (_, i) => message(from + i));

const isScroller = (el: Element) => el.getAttribute('data-testid') === 'chat-scroller';
/** Every list item is 60px tall; the view is 500px. */
const heightOf = (el: Element) => Math.max(VIEWPORT, el.querySelectorAll('li').length * 60);
const maxTop = (el: Element) => heightOf(el) - VIEWPORT;

beforeEach(() => {
  top = 0;
  Object.defineProperty(Element.prototype, 'scrollHeight', {
    configurable: true,
    get(this: Element) {
      return isScroller(this) ? heightOf(this) : 0;
    },
  });
  Object.defineProperty(Element.prototype, 'clientHeight', {
    configurable: true,
    get(this: Element) {
      return isScroller(this) ? VIEWPORT : 0;
    },
  });
  Object.defineProperty(Element.prototype, 'scrollTop', {
    configurable: true,
    get(this: Element) {
      return isScroller(this) ? Math.min(top, maxTop(this)) : 0;
    },
    set(this: Element, v: number) {
      if (isScroller(this)) top = Math.max(0, Math.min(v, maxTop(this)));
    },
  });
});
afterEach(() => {
  for (const key of ['scrollHeight', 'clientHeight', 'scrollTop']) {
    Reflect.deleteProperty(Element.prototype, key);
  }
});

const view = (props: Partial<ChatViewProps>) => (
  <I18nProvider locale="en">
    <ChatView events={[]} gapFrom={null} {...props} />
  </I18nProvider>
);
const scroller = () => screen.getByTestId('chat-scroller');

describe('ChatView paging', () => {
  it('shows a placeholder, not "no activity", while the first page is on its way', () => {
    render(view({ loading: true }));
    expect(screen.getByRole('status', { name: 'Loading conversation…' })).toBeInTheDocument();
    expect(screen.queryByText('No activity yet.')).not.toBeInTheDocument();
  });

  it('offers earlier messages by button, and says when it is loading them', () => {
    const onLoadOlder = vi.fn();
    const { rerender } = render(
      view({ events: range(41, 100), gapFrom: 41, more: true, onLoadOlder }),
    );
    fireEvent.click(screen.getByRole('button', { name: 'Load earlier messages' }));
    expect(onLoadOlder).toHaveBeenCalledTimes(1);
    rerender(
      view({ events: range(41, 100), gapFrom: 41, more: true, loadingOlder: true, onLoadOlder }),
    );
    expect(screen.getByRole('status')).toHaveTextContent('Loading earlier messages…');
    expect(screen.queryByRole('button', { name: 'Load earlier messages' })).not.toBeInTheDocument();
  });

  it('loads the previous page when the reader nears the top, once at a time', () => {
    const onLoadOlder = vi.fn();
    const props = { events: range(41, 100), gapFrom: 41, more: true, onLoadOlder };
    const { rerender } = render(view(props));
    expect(top).toBe(maxTop(scroller()));
    // Far from the top: nothing to load.
    top = 1000;
    fireEvent.scroll(scroller());
    expect(onLoadOlder).not.toHaveBeenCalled();
    top = 100;
    fireEvent.scroll(scroller());
    expect(onLoadOlder).toHaveBeenCalledTimes(1);
    // While a page is on its way, more scrolling asks for nothing more.
    rerender(view({ ...props, loadingOlder: true }));
    top = 50;
    fireEvent.scroll(scroller());
    expect(onLoadOlder).toHaveBeenCalledTimes(1);
  });

  it('keeps the reading position when a page is put in front', () => {
    const props = { gapFrom: 41, more: true, onLoadOlder: vi.fn() };
    const { rerender } = render(view({ ...props, events: range(41, 100) }));
    top = 120;
    fireEvent.scroll(scroller());
    const readingTop = top;
    const heightBefore = scroller().scrollHeight;
    // 40 older events arrive: the same message must stay where it was.
    rerender(view({ ...props, events: range(1, 100) }));
    const grown = scroller().scrollHeight - heightBefore;
    expect(grown).toBeGreaterThan(0);
    expect(top).toBe(readingTop + grown);
    // The older page is not announced as new messages.
    expect(screen.getByRole('button', { name: 'Jump to latest' })).toBeInTheDocument();
  });

  it('still follows the newest message after earlier pages were added', () => {
    const props = { gapFrom: 41, more: true, onLoadOlder: vi.fn() };
    const { rerender } = render(view({ ...props, events: range(41, 100) }));
    rerender(view({ ...props, events: range(1, 100) }));
    expect(top).toBe(maxTop(scroller()));
    rerender(view({ ...props, events: range(1, 101) }));
    expect(top).toBe(maxTop(scroller()));
  });

  it('marks the start of the conversation once the first message is loaded', () => {
    render(view({ events: range(1, 100), gapFrom: 41, more: false, onLoadOlder: vi.fn() }));
    expect(screen.getByText('Start of the conversation')).toBeInTheDocument();
  });

  it('says so when the earliest history is gone from the server', () => {
    render(view({ events: range(30, 100), gapFrom: 41, more: false, onLoadOlder: vi.fn() }));
    expect(screen.queryByText('Start of the conversation')).not.toBeInTheDocument();
    expect(screen.getByText(/no longer available/)).toBeInTheDocument();
  });

  it('fetches more by itself when the page is too short to scroll', () => {
    const onLoadOlder = vi.fn();
    render(view({ events: range(41, 43), gapFrom: 41, more: true, onLoadOlder }));
    expect(onLoadOlder).toHaveBeenCalled();
  });

  it('does not ask a client without paging to fetch anything', () => {
    // An older embedding: no onLoadOlder, so the plain gap note stays.
    render(view({ events: range(41, 100), gapFrom: 41 }));
    expect(screen.getByText(/no longer available/)).toBeInTheDocument();
    act(() => {
      top = 0;
      fireEvent.scroll(scroller());
    });
  });
});
