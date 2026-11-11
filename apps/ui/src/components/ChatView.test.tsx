import { act, fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import type { SessionEvent } from '../protocol/types';
import type { SentPrompt } from '../state/reducer';
import { ChatView, type ChatViewProps } from './ChatView';

const enc = new TextEncoder();

function renderChat(props: Partial<ChatViewProps> = {}) {
  return render(
    <I18nProvider locale="en">
      <ChatView events={[]} gapFrom={null} {...props} />
    </I18nProvider>,
  );
}

const sent = (overrides: Partial<SentPrompt> = {}): SentPrompt => ({
  id: 'p1',
  text: 'say hello',
  at: Date.now(),
  afterSeq: null,
  status: 'sending',
  ...overrides,
});

describe('ChatView', () => {
  it('shows the progress line under the chat while the agent works', () => {
    renderChat({
      progress: {
        phase: 'working',
        since: Date.now() - 2_400,
        activity: { type: 'tool', name: 'Read', summary: '/repo/src/main.rs' },
        until: null,
      },
    });
    const line = screen.getByTestId('chat-progress');
    expect(line).toHaveTextContent('Working · 2s');
    expect(line).toHaveTextContent('Reading main.rs');
    expect(within(line).getByRole('status')).toHaveTextContent(
      'The agent is working, Reading main.rs',
    );
  });

  it('shows Thinking while the agent thinks, and no line without progress', () => {
    const { rerender } = renderChat({
      progress: {
        phase: 'working',
        since: Date.now(),
        activity: { type: 'thinking' },
        until: null,
      },
    });
    expect(screen.getByTestId('chat-progress')).toHaveTextContent('Thinking…');
    rerender(
      <I18nProvider locale="en">
        <ChatView events={[]} gapFrom={null} progress={null} />
      </I18nProvider>,
    );
    expect(screen.queryByTestId('chat-progress')).toBeNull();
  });

  it('shows sent prompts as outgoing bubbles with their delivery status', () => {
    const { rerender } = renderChat({ sent: [sent()] });
    const log = screen.getByRole('log');
    expect(within(log).getByText('say hello')).toBeInTheDocument();
    expect(within(log).getByText('Sending…')).toBeInTheDocument();
    rerender(
      <I18nProvider locale="en">
        <ChatView events={[]} gapFrom={null} sent={[sent({ status: 'failed', error: 'x' })]} />
      </I18nProvider>,
    );
    expect(within(log).getByText('Not delivered')).toHaveAttribute('title', 'x');
  });

  it('shows agent output and a terminal hint for agents without a transcript', async () => {
    const onShowTerminal = vi.fn();
    const events: SessionEvent[] = [
      { type: 'state_changed', seq: 1, at: 1, state: { type: 'running' } },
      {
        type: 'output',
        seq: 2,
        at: 2,
        data: enc.encode('say hello\r\n\x1b[1mHello there\x1b[0m\r\n'),
      },
    ];
    renderChat({
      events,
      sent: [sent({ afterSeq: 1, status: 'delivered' })],
      outputFallback: true,
      onShowTerminal,
    });
    expect(screen.getByRole('note')).toHaveTextContent(/no chat transcript/);
    expect(screen.getByText('Agent output')).toBeInTheDocument();
    expect(screen.getByText('Hello there')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Open Terminal' }));
    expect(onShowTerminal).toHaveBeenCalled();
  });

  it('hides output and the hint for transcript agents', () => {
    renderChat({
      events: [{ type: 'output', seq: 1, at: 1, data: enc.encode('raw tui') }],
    });
    expect(screen.queryByRole('note')).not.toBeInTheDocument();
    expect(screen.queryByText('raw tui')).not.toBeInTheDocument();
    expect(screen.getByText('No activity yet.')).toBeInTheDocument();
  });
});

const message = (
  seq: number,
  role: 'user' | 'assistant' | 'system' | 'tool',
  text: string,
): SessionEvent => ({ type: 'message', seq, at: seq, role, text });

describe('ChatView activity', () => {
  it('does not show machine text as the user', async () => {
    renderChat({
      events: [
        message(1, 'user', 'Fix the login page'),
        message(
          2,
          'user',
          '<task-notification>\n<status>completed</status>\n<summary>Background command "Wait for CI" completed</summary>\n</task-notification>',
        ),
        message(
          3,
          'system',
          'Another Claude session sent a message:\n<agent-message from="a">\n[Subagent hand-back] The report follows:\n  Bridge done.\n  Tests pass.\n</agent-message>',
        ),
      ],
    });
    // Exactly one bubble is attributed to the user: the prompt they typed.
    expect(screen.getAllByText('You')).toHaveLength(1);
    expect(screen.getByText('Fix the login page')).toBeInTheDocument();
    expect(screen.getByText('Background task')).toBeInTheDocument();
    expect(screen.getAllByText('Completed')).toHaveLength(2);
    const report = screen.getByRole('button', { name: /Sub-agent report/ });
    expect(report).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText(/Tests pass/)).not.toBeInTheDocument();
    await userEvent.click(report);
    expect(report).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText(/Tests pass/)).toBeInTheDocument();
  });

  it('collapses a sub-agent into a row that expands with its report', async () => {
    renderChat({
      events: [
        { type: 'tool_call', seq: 1, at: 1, name: 'Agent', summary: 'Review the plan' },
        message(2, 'tool', 'Async agent launched successfully.'),
        message(
          3,
          'system',
          '<task-notification>\n<status>completed</status>\n<summary>Agent "Review the plan" finished</summary>\n<result>Plan approved with two notes.</result>\n</task-notification>',
        ),
      ],
    });
    expect(screen.queryByText('Async agent launched successfully.')).not.toBeInTheDocument();
    const rows = screen.getAllByRole('button', { name: /Sub-agent.*Review the plan.*Completed/ });
    expect(rows).toHaveLength(2);
    const row = rows[0]!;
    expect(row).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText('Plan approved with two notes.')).not.toBeInTheDocument();
    row.focus();
    await userEvent.keyboard('{Enter}');
    expect(row).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('Plan approved with two notes.')).toBeInTheDocument();
    await userEvent.click(row);
    expect(row).toHaveAttribute('aria-expanded', 'false');
  });
});

describe('ChatView copy', () => {
  it('copies a message and a code block with feedback', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    renderChat({
      events: [message(1, 'assistant', 'Run:\n```sh\ncargo test\n```\nDone.')],
    });
    await userEvent.click(screen.getByRole('button', { name: 'Copy message' }));
    expect(writeText).toHaveBeenLastCalledWith('Run:\n```sh\ncargo test\n```\nDone.');
    expect(await screen.findByRole('button', { name: 'Copied' })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Copy code' }));
    expect(writeText).toHaveBeenLastCalledWith('cargo test');
    expect(screen.getByText('cargo test').tagName).toBe('CODE');
    Reflect.deleteProperty(navigator, 'clipboard');
  });

  it('keeps chat text selectable', () => {
    renderChat({ events: [message(1, 'assistant', 'select me')] });
    // No style on the log or its ancestors blocks selection.
    for (let el: HTMLElement | null = screen.getByText('select me'); el; el = el.parentElement) {
      expect(getComputedStyle(el).userSelect).not.toBe('none');
    }
  });
});

describe('ChatView scrolling', () => {
  function metrics(el: HTMLElement) {
    const box = { top: 0, height: 2000 };
    Object.defineProperty(el, 'clientHeight', { configurable: true, get: () => 500 });
    Object.defineProperty(el, 'scrollHeight', { configurable: true, get: () => box.height });
    Object.defineProperty(el, 'scrollTop', {
      configurable: true,
      get: () => box.top,
      set: (v: number) => {
        box.top = Math.max(0, Math.min(v, box.height - 500));
      },
    });
    Object.defineProperty(el, 'scrollTo', {
      configurable: true,
      value: ({ top }: { top: number }) => {
        el.scrollTop = top;
      },
    });
    return box;
  }

  const many = (n: number, from = 1) =>
    Array.from({ length: n }, (_, i) => message(from + i, 'assistant', `reply ${from + i}`));
  const view = (events: SessionEvent[]) => (
    <I18nProvider locale="en">
      <ChatView events={events} gapFrom={null} />
    </I18nProvider>
  );

  it('follows new messages at the bottom and offers a jump while reading history', async () => {
    const { rerender } = render(view(many(3)));
    const scroller = screen.getByTestId('chat-scroller');
    const box = metrics(scroller);

    rerender(view(many(4)));
    expect(box.top).toBe(1500);
    expect(screen.queryByRole('button', { name: /Jump to latest/ })).not.toBeInTheDocument();

    // The user scrolls up to read: new messages must not pull them down.
    box.top = 400;
    fireEvent.scroll(scroller);
    box.height = 2400;
    rerender(view(many(6)));
    expect(box.top).toBe(400);
    const jump = screen.getByRole('button', { name: 'Jump to latest (2 new)' });
    expect(jump).toHaveTextContent('2 new');

    await userEvent.click(jump);
    expect(box.top).toBe(1900);
    expect(screen.queryByRole('button', { name: /Jump to latest/ })).not.toBeInTheDocument();

    // Following again.
    box.height = 2600;
    rerender(view(many(7)));
    expect(box.top).toBe(2100);
  });

  it('re-sticks when the user scrolls back to the bottom', () => {
    const { rerender } = render(view(many(3)));
    const scroller = screen.getByTestId('chat-scroller');
    const box = metrics(scroller);
    rerender(view(many(4)));
    box.top = 100;
    fireEvent.scroll(scroller);
    expect(screen.getByRole('button', { name: 'Jump to latest' })).toBeInTheDocument();
    box.top = 1500;
    fireEvent.scroll(scroller);
    expect(screen.queryByRole('button', { name: /Jump to latest/ })).not.toBeInTheDocument();
  });

  it('stops following while the user expands an item to read it', async () => {
    const events: SessionEvent[] = [
      ...many(3),
      { type: 'tool_call', seq: 10, at: 10, name: 'Read', summary: 'a.txt' },
      message(11, 'tool', 'line 1\nline 2'),
    ];
    const { rerender } = render(view(many(1)));
    const scroller = screen.getByTestId('chat-scroller');
    const box = metrics(scroller);
    rerender(view(events));
    expect(box.top).toBe(1500);
    await userEvent.click(screen.getByRole('button', { name: /Read · Result/ }));
    box.height = 2400;
    rerender(view([...events, message(12, 'assistant', 'next')]));
    expect(box.top).toBe(1500);
    expect(screen.getByRole('button', { name: 'Jump to latest (1 new)' })).toBeInTheDocument();
  });
});

describe('ChatView landing on the newest message', () => {
  const VIEWPORT = 500;
  let top = 0;
  /** Extra height from things the test grows on its own (images, fonts). */
  let extra = 0;
  let observers: Array<() => void> = [];

  const heightOf = (el: Element) => {
    let h = extra;
    el.querySelectorAll('li').forEach((li) => {
      h += 40 + (li.textContent?.length ?? 0);
    });
    return Math.max(h, VIEWPORT);
  };
  const isScroller = (el: Element) => el.getAttribute('data-testid') === 'chat-scroller';
  const maxTop = (el: Element) => heightOf(el) - VIEWPORT;

  beforeEach(() => {
    top = 0;
    extra = 0;
    observers = [];
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
    vi.stubGlobal(
      'ResizeObserver',
      class {
        constructor(cb: () => void) {
          observers.push(cb);
        }
        observe() {}
        disconnect() {}
      },
    );
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    for (const key of ['scrollHeight', 'clientHeight', 'scrollTop']) {
      Reflect.deleteProperty(Element.prototype, key);
    }
  });

  /** The browser runs the resize observers after late layout changes. */
  const layoutSettles = () => act(() => observers.forEach((cb) => cb()));
  const many = (n: number, text = 'reply') =>
    Array.from({ length: n }, (_, i) => message(i + 1, 'assistant', `${text} ${i + 1}`));
  const view = (events: SessionEvent[], key = 'a') => (
    <I18nProvider locale="en">
      <ChatView key={key} events={events} gapFrom={null} />
    </I18nProvider>
  );
  const scroller = () => screen.getByTestId('chat-scroller');
  const atEnd = () => top === maxTop(scroller());
  const jumpButton = () => screen.queryByRole('button', { name: /Jump to latest/ });

  it('lands on the newest message when history arrives after the first render', () => {
    const { rerender } = render(view([]));
    expect(atEnd()).toBe(true);
    rerender(view(many(40)));
    expect(top).toBeGreaterThan(0);
    expect(atEnd()).toBe(true);
  });

  it('keeps following a history replayed in several batches', () => {
    const { rerender } = render(view([]));
    for (const n of [5, 20, 60, 61]) {
      rerender(view(many(n)));
      expect(atEnd()).toBe(true);
    }
    expect(jumpButton()).not.toBeInTheDocument();
  });

  it('stays at the end when images or fonts make the list taller later', async () => {
    const { rerender } = render(view(many(30)));
    extra += 700;
    await layoutSettles();
    expect(atEnd()).toBe(true);
    // The scroll event of the earlier move arrives after the growth.
    fireEvent.scroll(scroller());
    extra += 300;
    await layoutSettles();
    expect(atEnd()).toBe(true);
    rerender(view(many(31)));
    expect(atEnd()).toBe(true);
    expect(jumpButton()).not.toBeInTheDocument();
  });

  it('does not take layout-driven position changes for the user scrolling away', async () => {
    render(view(many(30)));
    expect(atEnd()).toBe(true);
    const el = scroller();
    // The browser clamped the position while the list was shorter...
    top = 100;
    // ...and the list grew again before the scroll event was delivered.
    extra += 600;
    fireEvent.scroll(el);
    expect(jumpButton()).not.toBeInTheDocument();
    await layoutSettles();
    expect(atEnd()).toBe(true);
  });

  it('lets the user leave the end on purpose and come back', async () => {
    const { rerender } = render(view(many(30)));
    top -= 200;
    fireEvent.scroll(scroller());
    expect(jumpButton()).toBeInTheDocument();
    const reading = top;
    rerender(view(many(32)));
    extra += 400;
    await layoutSettles();
    expect(top).toBe(reading);
    top = maxTop(scroller());
    fireEvent.scroll(scroller());
    expect(jumpButton()).not.toBeInTheDocument();
  });

  it('stops following after even a short move up', () => {
    const { rerender } = render(view(many(30)));
    top -= 30;
    fireEvent.scroll(scroller());
    expect(jumpButton()).toBeInTheDocument();
    const reading = top;
    rerender(view(many(31)));
    expect(top).toBe(reading);
  });

  it('opens another session at its newest message', () => {
    const { rerender } = render(view(many(30), 'a'));
    top -= 300;
    fireEvent.scroll(scroller());
    expect(jumpButton()).toBeInTheDocument();
    rerender(view(many(50, 'other'), 'b'));
    expect(atEnd()).toBe(true);
    expect(jumpButton()).not.toBeInTheDocument();
  });

  it('follows a re-render that makes the last message taller', async () => {
    const { rerender } = render(view(many(30)));
    const longer = many(30);
    longer[29] = message(30, 'assistant', 'x'.repeat(900));
    rerender(view(longer));
    await layoutSettles();
    expect(atEnd()).toBe(true);
  });
});

describe('ChatView markdown', () => {
  const assistant = (text: string, seq = 1): SessionEvent => ({
    type: 'message',
    seq,
    at: seq,
    role: 'assistant',
    text,
  });
  const TABLE =
    '결과입니다.\n\n| 확인 | 결과 |\n| --- | --- |\n| 서버 상태 | **정상** |\n| 웹 앱 | 정상 |';

  it('renders agent tables, code, lists and links; user text stays plain', () => {
    renderChat({
      events: [
        assistant(TABLE, 1),
        assistant(
          '- one\n- two\n\n```bash\npnpm test\n```\n\nSee [docs](https://vca.metavu.io).',
          2,
        ),
      ],
      sent: [sent({ text: '| not | a table |\n|---|---|', afterSeq: 2, status: 'delivered' })],
    });
    const table = screen.getByRole('table');
    expect(
      within(table)
        .getAllByRole('columnheader')
        .map((c) => c.textContent),
    ).toEqual(['확인', '결과']);
    expect(within(table).getByText('정상', { selector: 'strong' })).toBeInTheDocument();
    // The table scrolls inside its own region.
    expect(table.parentElement).toHaveAttribute('role', 'region');
    expect(screen.getAllByRole('listitem').some((li) => li.textContent === 'one')).toBe(true);
    expect(screen.getByText('pnpm test').closest('pre')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Copy code' })).toBeInTheDocument();
    const link = screen.getByRole('link', { name: 'docs' });
    expect(link).toHaveAttribute('href', 'https://vca.metavu.io');
    expect(link).toHaveAttribute('target', '_blank');
    expect(link).toHaveAttribute('rel', 'noopener noreferrer');
    // The person's own message is not formatted.
    expect(screen.getByText(/\| not \| a table \|/)).toBeInTheDocument();
    expect(screen.getAllByRole('table')).toHaveLength(1);
  });

  it('escapes raw HTML', () => {
    const { container } = renderChat({
      events: [assistant('<img src=x onerror="alert(1)"> <b>bold?</b>')],
    });
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('b')).toBeNull();
    expect(screen.getByText(/<img src=x onerror="alert\(1\)">/)).toBeInTheDocument();
  });

  it('switches between formatted and source; copy keeps the markdown', async () => {
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    renderChat({ events: [assistant(TABLE)] });
    expect(screen.getByRole('table')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Show source' }));
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.getByText(/\| 확인 \| 결과 \|/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Show formatted' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    await userEvent.click(screen.getByRole('button', { name: 'Show formatted' }));
    expect(screen.getByRole('table')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Copy message' }));
    expect(writeText).toHaveBeenCalledWith(TABLE);
  });

  it('highlights search matches inside rendered markdown, numbered in order', () => {
    const onSearchCount = vi.fn();
    const { container } = renderChat({
      events: [assistant(TABLE)],
      searchQuery: '정상',
      searchIndex: 1,
      onSearchCount,
    });
    expect(onSearchCount).toHaveBeenLastCalledWith(2);
    const marks = [...container.querySelectorAll('mark[data-search-index]')];
    expect(marks.map((m) => m.getAttribute('data-search-index'))).toEqual(['0', '1']);
    expect(marks[1]).toHaveAttribute('data-current', 'true');
    expect(marks[0]!.closest('table')).not.toBeNull();
  });
});
