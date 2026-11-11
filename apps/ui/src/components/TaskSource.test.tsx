import { act, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { mockTask } from '../protocol/mockTasks';
import type { HumanTask, TaskSettings } from '../protocol/tasks';
import type { ClientMsg, ServerMsg, SessionEvent, SessionInfo } from '../protocol/types';
import { clearChatFocus, currentChatFocus, requestChatFocus } from '../state/chatFocus';
import { useActions, useAppState } from '../state/store';
import { hasSource, messagesAround, taskText } from '../state/taskSource';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { LiveView } from './LiveView';
import { TasksView } from './TasksView';

vi.mock('./TerminalView', () => ({ default: () => <div data-testid="terminal" /> }));

const SETTINGS: TaskSettings = {
  extract: true,
  extract_llm: false,
  stale_hours: 8,
  remind_blocking_minutes: 30,
  digest_hour: null,
};

const message = (seq: number, role: 'user' | 'assistant' = 'assistant'): SessionEvent => ({
  type: 'message',
  seq,
  at: Date.now() - 1000,
  role,
  text: `reply ${seq}`,
});
const range = (from: number, to: number) =>
  Array.from({ length: to - from + 1 }, (_, i) => message(from + i));

const sourced = (over: Partial<HumanTask> = {}) =>
  mockTask({
    id: 'sourced',
    title: 'Install the driver',
    steps: 'Open an admin shell',
    session_id: 's1',
    session_name: 'payments-api',
    source: 'extracted',
    command: {
      text: 'pnputil /add-driver x.inf /install',
      shell: 'powershell',
      needs_admin: true,
      danger: 'caution',
      reasons: [],
      spans: [],
    },
    message_seq: 45,
    message_at: Date.now() - 1000,
    excerpt: 'Please run pnputil as administrator.',
    ...over,
  });
const plain = mockTask({ id: 'plain', title: 'Buy toner', order: 1 });

let writeText: ReturnType<typeof vi.fn>;
beforeEach(() => {
  writeText = vi.fn(async () => {});
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: { writeText },
  });
});
afterEach(() => {
  Reflect.deleteProperty(navigator, 'clipboard');
  const pending = currentChatFocus();
  if (pending) act(() => clearChatFocus(pending.token));
});

function server(
  tasks: HumanTask[],
  {
    sessions = [localSession()],
    history = (_msg: Extract<ClientMsg, { type: 'history' }>): ServerMsg | null => null,
  } = {},
) {
  return new FakeTransport((msg) => {
    switch (msg.type) {
      case 'hello':
        return {
          type: 'welcome',
          protocol_version: 1,
          server_version: 'test',
          server_name: 'test',
          features: ['human_tasks'],
        };
      case 'list_sessions':
        return { type: 'sessions', sessions };
      case 'task_list':
        return { type: 'tasks', tasks, settings: SETTINGS };
      case 'history':
        return (
          history(msg) ?? {
            type: 'history',
            session_id: msg.session_id,
            events: [],
            has_more: false,
          }
        );
      default:
        return defaultResponse(msg);
    }
  });
}

/** History around seq 45: a page of the 40 events before `before_seq`. */
function pageBefore(msg: Extract<ClientMsg, { type: 'history' }>): ServerMsg {
  const before = msg.before_seq ?? 61;
  const from = Math.max(1, before - msg.limit);
  return {
    type: 'history',
    session_id: msg.session_id,
    events: before > from ? range(from, before - 1) : [],
    has_more: from > 1,
  };
}

/** The in-app toasts, as text (the toast stack itself is not under test). */
function ToastProbe() {
  const titles = useAppState((s) => s.toasts.map((t) => t.title).join('|'));
  return <output data-testid="toasts">{titles}</output>;
}

const openTask = async (title: string) => {
  await userEvent.click(await screen.findByText(title));
};

describe('task source helpers', () => {
  it('knows which tasks have a source, joins the task text and finds the context', () => {
    expect(hasSource(sourced())).toBe(true);
    expect(hasSource(plain)).toBe(false);
    // An older task saved before excerpts: still has its message number.
    expect(hasSource(sourced({ excerpt: undefined, message_at: undefined }))).toBe(true);
    expect(hasSource(sourced({ session_id: null, message_seq: null, excerpt: null }))).toBe(false);
    expect(taskText(sourced())).toBe(
      'Install the driver\n\nOpen an admin shell\n\npnputil /add-driver x.inf /install',
    );
    const around = messagesAround(range(1, 20), 10)!;
    expect(around.map((m) => m.seq)).toEqual([7, 8, 9, 10, 11, 12, 13]);
    expect(messagesAround(range(1, 5), 9)).toBeNull();
  });
});

describe('task source in the to-do list', () => {
  it('shows the source buttons only for tasks with a source', async () => {
    renderWithApp(<TasksView />, { transport: server([sourced(), plain]) });
    await openTask('Install the driver');
    const item = screen.getByTestId('task-sourced');
    expect(
      within(item).getByRole('button', { name: 'View source conversation' }),
    ).toBeInTheDocument();
    expect(within(item).getByRole('button', { name: 'Messages around it' })).toBeInTheDocument();
    expect(within(item).getByRole('button', { name: 'Copy source message' })).toBeInTheDocument();
    await openTask('Buy toner');
    const other = screen.getByTestId('task-plain');
    expect(within(other).queryByRole('button', { name: 'View source conversation' })).toBeNull();
    expect(within(other).queryByRole('button', { name: 'Copy source message' })).toBeNull();
    // Every task can be copied.
    expect(within(other).getByRole('button', { name: 'Copy task' })).toBeInTheDocument();
  });

  it('copies the command, the task and the source message with a toast', async () => {
    const transport = server([sourced()], {
      history: (msg) =>
        msg.before_seq === 46 && msg.limit === 1
          ? {
              type: 'history',
              session_id: 's1',
              events: [{ ...message(45), text: 'Full source: run pnputil as administrator.' }],
              has_more: true,
            }
          : null,
    });
    renderWithApp(
      <>
        <TasksView />
        <ToastProbe />
      </>,
      { transport },
    );
    await openTask('Install the driver');
    const item = screen.getByTestId('task-sourced');
    await userEvent.click(within(item).getByRole('button', { name: 'Copy command' }));
    expect(writeText).toHaveBeenLastCalledWith('pnputil /add-driver x.inf /install');
    await waitFor(() => expect(screen.getByTestId('toasts')).toHaveTextContent('Copied'));
    await userEvent.click(within(item).getByRole('button', { name: 'Copy task' }));
    expect(writeText).toHaveBeenLastCalledWith(taskText(sourced()));
    // The full message once fetched (the excerpt before).
    await waitFor(() => expect(transport.bodies('history')).toHaveLength(1));
    await userEvent.click(within(item).getByRole('button', { name: 'Copy source message' }));
    await waitFor(() =>
      expect(writeText).toHaveBeenLastCalledWith('Full source: run pnputil as administrator.'),
    );
  });

  it('opens a sheet with the message and the ones around it, each with a copy button', async () => {
    const transport = server([sourced()], { history: pageBefore });
    renderWithApp(<TasksView />, { transport });
    await openTask('Install the driver');
    await userEvent.click(screen.getByRole('button', { name: 'Messages around it' }));
    const dialog = await screen.findByRole('dialog', { name: 'Source conversation' });
    const list = await within(dialog).findByRole('list', { name: 'Source conversation' });
    const items = within(list).getAllByRole('listitem');
    expect(items.map((li) => li.querySelector('p')?.textContent)).toEqual([
      'reply 42',
      'reply 43',
      'reply 44',
      'reply 45',
      'reply 46',
      'reply 47',
      'reply 48',
    ]);
    const anchor = items[3]!;
    expect(anchor).toHaveAttribute('aria-current', 'true');
    expect(within(anchor).getByText('The task came from this message')).toBeInTheDocument();
    await userEvent.click(within(anchor).getByRole('button', { name: 'Copy message' }));
    expect(writeText).toHaveBeenLastCalledWith('reply 45');
    // Keyboard: Escape closes it.
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('shows the stored excerpt with a note when the session is gone', async () => {
    const transport = server([sourced({ session_id: 'gone' })], { history: pageBefore });
    renderWithApp(<TasksView />, { transport });
    await openTask('Install the driver');
    // No session: no "messages around it", the source button opens the excerpt.
    expect(screen.queryByRole('button', { name: 'Messages around it' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'View source conversation' }));
    const dialog = await screen.findByRole('dialog', { name: 'Source conversation' });
    expect(within(dialog).getByRole('note')).toHaveTextContent(/no longer available/);
    expect(within(dialog).getByText('Please run pnputil as administrator.')).toBeInTheDocument();
    expect(transport.bodies('history')).toHaveLength(0);
  });

  it('shows the excerpt when the timeline no longer has the message', async () => {
    renderWithApp(<TasksView />, { transport: server([sourced()]) });
    await openTask('Install the driver');
    await userEvent.click(screen.getByRole('button', { name: 'Messages around it' }));
    const dialog = await screen.findByRole('dialog', { name: 'Source conversation' });
    expect(await within(dialog).findByRole('note')).toHaveTextContent(/excerpt saved/);
  });

  it('asks the session view to show the message', async () => {
    renderWithApp(<TasksView />, { transport: server([sourced()], { history: pageBefore }) });
    await openTask('Install the driver');
    await userEvent.click(screen.getByRole('button', { name: 'View source conversation' }));
    expect(currentChatFocus()).toMatchObject({
      sessionId: 's1',
      seq: 45,
      excerpt: 'Please run pnputil as administrator.',
    });
  });
});

/** Selects the session (which subscribes it) and renders its live view. */
function Selected({ session }: { session: SessionInfo }) {
  const actions = useActions();
  const selected = useAppState((s) => s.selectedId);
  const known = useAppState((s) => !!s.sessions[session.id]);
  useEffect(() => {
    if (known) actions.select(session.id);
  }, [actions, known, session.id]);
  return selected === session.id ? <LiveView session={session} /> : null;
}

describe('the chat at a task source', () => {
  it('loads earlier pages until the source message is there, then highlights it', async () => {
    const session = localSession();
    const transport = server([], { history: pageBefore });
    renderWithApp(<Selected session={session} />, { transport });
    const log = await screen.findByRole('log');
    act(() => {
      transport.push({ type: 'stream_gap', session_id: 's1', first_available_seq: 100 });
      for (const e of range(100, 110))
        transport.push({ type: 'event', session_id: 's1', event: e });
    });
    act(() => {
      requestChatFocus('s1', 45, 'Please run pnputil');
    });
    await waitFor(() => expect(log.querySelector('[data-anchor="true"]')).not.toBeNull(), {
      timeout: 3000,
    });
    const anchor = log.querySelector<HTMLElement>('[data-anchor="true"]')!;
    expect(anchor).toHaveTextContent('reply 45');
    expect(anchor).toHaveAttribute('data-seq', '45');
    expect(anchor).toHaveAttribute('aria-current', 'true');
    expect(document.activeElement).toBe(anchor);
    // Pages were loaded one by one, newest first.
    const befores = transport
      .bodies('history')
      .map((b) => (b as { before_seq: number }).before_seq);
    expect(befores[0]).toBe(100);
    expect(befores.length).toBeGreaterThanOrEqual(2);
    expect(currentChatFocus()?.status).toBe('found');
  });

  it('shows the excerpt when the message cannot be found', async () => {
    const session = localSession();
    const transport = server([]);
    renderWithApp(<Selected session={session} />, { transport });
    await screen.findByRole('log');
    act(() => {
      for (const e of range(1, 5)) transport.push({ type: 'event', session_id: 's1', event: e });
    });
    act(() => {
      requestChatFocus('s1', 3_000, 'The deploy key must be rotated.');
    });
    const dialog = await screen.findByRole('dialog', { name: 'Source conversation' });
    expect(within(dialog).getByRole('note')).toHaveTextContent(/no longer available/);
    expect(within(dialog).getByText('The deploy key must be rotated.')).toBeInTheDocument();
  });
});
