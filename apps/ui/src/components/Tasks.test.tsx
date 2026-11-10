import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { App } from '../App';
import { I18nProvider } from '../i18n/context';
import { mockTask } from '../protocol/mockTasks';
import { MockTransport } from '../protocol/mockTransport';
import type { HumanTask, TaskSettings } from '../protocol/tasks';
import type { ClientMsg, ServerMsg } from '../protocol/types';
import { SpeechProvider, type SpeechServices } from '../speech/context';
import { defaultSettings } from '../state/settings';
import { AppProvider, type Machine } from '../state/store';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { ChatView } from './ChatView';
import { TasksView } from './TasksView';

const SETTINGS: TaskSettings = {
  extract: true,
  extract_llm: false,
  stale_hours: 8,
  remind_blocking_minutes: 30,
  digest_hour: null,
};

const speech: SpeechServices = {
  transcribe: async () => ({ text: '', corrections: [] }),
  createRecorder: () => ({
    start: async () => {},
    stop: async () => new Blob(),
    cancel: () => {},
  }),
  fallbackWithoutMic: false,
};

function tasks(): HumanTask[] {
  const now = Date.now();
  return [
    mockTask({
      id: 'admin',
      title: 'Install the driver as administrator',
      session_id: 's1',
      session_name: 'payments-api',
      source: 'extracted',
      blocking: true,
      priority: 'high',
      command: {
        text: 'pnputil /add-driver x.inf /install',
        shell: 'powershell',
        needs_admin: true,
        danger: 'caution',
        reasons: [],
        spans: [],
      },
      created_at: now - 60_000,
      updated_at: now - 60_000,
    }),
    mockTask({
      id: 'login',
      title: 'Sign in to the console',
      session_id: 's1',
      session_name: 'payments-api',
      source: 'explicit',
      order: 1,
      created_at: now - 120_000,
      updated_at: now - 120_000,
    }),
    mockTask({
      id: 'old',
      title: 'Rotate the old key',
      status: 'done',
      created_at: now - 3_600_000,
      updated_at: now - 3_600_000,
    }),
  ];
}

/** A server with the to-do list; `extra` answers task requests first. */
function server(extra: (msg: ClientMsg) => ServerMsg | null = () => null) {
  return new FakeTransport((msg) => {
    const answer = extra(msg);
    if (answer) return answer;
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
        return {
          type: 'sessions',
          sessions: [localSession({ state: { type: 'awaiting_input' } })],
        };
      case 'task_list':
        return { type: 'tasks', tasks: tasks(), settings: SETTINGS };
      default:
        return defaultResponse(msg);
    }
  });
}

function renderTasks(transport: FakeTransport, machine: Partial<Machine> = {}) {
  render(
    <AppProvider
      machines={[{ id: 'm', name: 'pc', connectivity: 'LAN', transport, ...machine }]}
      settings={defaultSettings()}
      persistSettings={false}
    >
      <I18nProvider locale="en">
        <SpeechProvider value={speech}>
          <TasksView />
        </SpeechProvider>
      </I18nProvider>
    </AppProvider>,
  );
  return transport;
}

const list = () => screen.getByRole('list', { name: 'To-do list from agents' });

describe('to-do list', () => {
  it('filters by status, session, admin and text', async () => {
    renderTasks(server());
    await screen.findByText('Install the driver as administrator');
    // Active tasks only by default, highest priority first.
    const titles = () =>
      within(list())
        .getAllByRole('button', { expanded: false })
        .map((b) => b.querySelector('span')?.textContent);
    expect(titles()).toEqual(['Install the driver as administrator', 'Sign in to the console']);
    await userEvent.click(screen.getByRole('button', { name: 'Done' }));
    expect(titles()).toEqual(['Rotate the old key']);
    await userEvent.click(screen.getByRole('button', { name: 'All' }));
    expect(titles()).toHaveLength(3);
    await userEvent.selectOptions(screen.getByRole('combobox', { name: 'Session' }), 's1');
    expect(titles()).toHaveLength(2);
    await userEvent.click(screen.getByRole('checkbox', { name: 'Needs admin only' }));
    expect(titles()).toEqual(['Install the driver as administrator']);
    await userEvent.click(screen.getByRole('checkbox', { name: 'Needs admin only' }));
    await userEvent.type(screen.getByRole('searchbox', { name: 'Search tasks' }), 'console');
    expect(titles()).toEqual(['Sign in to the console']);
    await userEvent.clear(screen.getByRole('searchbox', { name: 'Search tasks' }));
    await userEvent.type(screen.getByRole('searchbox', { name: 'Search tasks' }), 'nothing');
    expect(screen.getByText('No task matches the filter.')).toBeInTheDocument();
  });

  it('checks a task off and back on', async () => {
    const transport = renderTasks(
      server((msg) => {
        if (msg.type !== 'task_update' || msg.change.op !== 'status') return null;
        const task = tasks().find((t) => t.id === msg.task_id)!;
        return { type: 'task_saved', task: { ...task, status: msg.change.status } };
      }),
    );
    const box = await screen.findByRole('checkbox', { name: 'Check off Sign in to the console' });
    await userEvent.click(box);
    expect(transport.bodies('task_update')).toEqual([
      { type: 'task_update', task_id: 'login', change: { op: 'status', status: 'done' } },
    ]);
    // Done tasks leave the "To do" filter.
    await waitFor(() => expect(screen.queryByText('Sign in to the console')).toBeNull());
    await userEvent.click(screen.getByRole('button', { name: 'Done' }));
    await userEvent.click(screen.getByRole('checkbox', { name: 'Reopen Sign in to the console' }));
    expect(transport.bodies('task_update')[1]).toMatchObject({
      change: { op: 'status', status: 'open' },
    });
  });

  it('sends a result after a preview and checks the task off', async () => {
    const composed =
      'Result of the task you asked me to do: "Sign in to the console"\n\nDone, signed in.';
    const transport = renderTasks(
      server((msg) => {
        if (msg.type !== 'task_send_result') return null;
        const task = tasks().find((t) => t.id === msg.task_id)!;
        return msg.preview
          ? { type: 'task_result_sent', task, text: composed, sent: false }
          : {
              type: 'task_result_sent',
              task: { ...task, status: 'done', sent_to_session: true },
              text: composed,
              sent: true,
            };
      }),
    );
    await userEvent.click(await screen.findByText('Sign in to the console'));
    await userEvent.click(screen.getByRole('button', { name: 'Send result' }));
    const dialog = await screen.findByRole('dialog', { name: 'Send result' });
    expect(within(dialog).getByText('To payments-api, as a message')).toBeInTheDocument();
    // Nothing to send yet.
    await userEvent.click(within(dialog).getByRole('button', { name: 'Send and check off' }));
    expect(within(dialog).getByRole('alert')).toHaveTextContent('Write a result');
    expect(transport.bodies('task_send_result')).toHaveLength(0);
    await userEvent.type(within(dialog).getByLabelText('Result'), 'Done, signed in.');
    await userEvent.type(within(dialog).getByLabelText('Command output'), 'ok');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Preview' }));
    expect(await within(dialog).findByLabelText('Preview')).toHaveTextContent('Done, signed in.');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Send and check off' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    const sends = transport.bodies('task_send_result');
    expect(sends).toHaveLength(2);
    expect(sends[0]).toMatchObject({ preview: true });
    expect(sends[1]).toEqual({
      type: 'task_send_result',
      task_id: 'login',
      result: { text: 'Done, signed in.', attachments: [], mentions: [], output: 'ok' },
      preview: false,
    });
    // Checked off: it left the "To do" list.
    await waitFor(() => expect(screen.queryByText('Sign in to the console')).toBeNull());
  });

  it('shows the danger of a command and opens an admin shell only on the PC', async () => {
    const transport = renderTasks(
      server((msg) => (msg.type === 'task_open_admin_shell' ? { type: 'ok' } : null)),
      {
        simulate: {
          drop: () => {},
          restoreLocal: {
            status: async () => ({ supported: true, sign_in: false, sign_in_delay_secs: 0 }),
            createShortcut: async () => ({
              supported: true,
              sign_in: false,
              sign_in_delay_secs: 0,
            }),
            setSignIn: async () => ({ supported: true, sign_in: false, sign_in_delay_secs: 0 }),
          },
        },
      },
    );
    await userEvent.click(await screen.findByText('Install the driver as administrator'));
    expect(screen.getByTestId('command-preview')).toHaveTextContent('pnputil /add-driver');
    expect(screen.getAllByText('Caution').length).toBeGreaterThan(0);
    expect(screen.getByText(/never run: paste it and press Enter yourself/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Open admin PowerShell' }));
    expect(transport.bodies('task_open_admin_shell')).toEqual([
      { type: 'task_open_admin_shell', task_id: 'admin' },
    ]);
    // The to-do settings are offered on the PC only.
    expect(screen.getByText('To-do settings (this PC)')).toBeInTheDocument();
  });

  it('adds a new task by hand', async () => {
    const transport = renderTasks(
      server((msg) =>
        msg.type === 'task_create'
          ? { type: 'task_saved', task: mockTask({ id: 'n1', title: msg.draft.title }) }
          : null,
      ),
    );
    await userEvent.click(await screen.findByRole('button', { name: 'New task' }));
    const dialog = await screen.findByRole('dialog', { name: 'New task' });
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    expect(within(dialog).getByRole('alert')).toHaveTextContent('Give the task a title.');
    await userEvent.type(within(dialog).getByLabelText('Title'), 'Restart the router');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(transport.bodies('task_create')[0]).toMatchObject({
      draft: { title: 'Restart the router', session_id: null, command: null },
    });
  });

  it('explains when the server has no to-do list', async () => {
    renderTasks(new FakeTransport());
    expect(await screen.findByText(/has no to-do list yet/)).toBeInTheDocument();
  });
});

describe('alerts and to-do tabs', () => {
  function renderApp(seed: HumanTask[]) {
    const transport = new MockTransport({ latencyMs: 0, autoTick: false, tasks: { seed } });
    render(
      <App
        machines={[{ id: 'm1', name: 'workstation', connectivity: 'LAN', transport }]}
        speech={speech}
        settings={defaultSettings()}
      />,
    );
    return transport;
  }
  const nav = () => within(screen.getByRole('navigation', { name: 'Main navigation' }));

  it('puts decisions and blocking tasks on one badge and two tabs (phone layout)', async () => {
    // No matchMedia in the test DOM: this is the phone layout.
    renderApp([
      mockTask({
        id: 'b1',
        title: 'Approve the login on the router',
        session_id: 'wk-claude-api',
        blocking: true,
        command: {
          text: 'netsh advfirewall set allprofiles state on',
          shell: 'cmd',
          needs_admin: true,
          danger: 'caution',
          reasons: [],
          spans: [],
        },
      }),
      mockTask({ id: 'n1', title: 'Read the release notes', order: 1 }),
    ]);
    await screen.findByTestId('session-card-wk-claude-api');
    // The session card says its agent waits on the person.
    expect(
      within(screen.getByTestId('session-card-wk-claude-api')).getByTestId('waiting-on-you'),
    ).toHaveTextContent('Waiting on you');
    const alertsButton = () => nav().getByRole('button', { name: /Alerts/ });
    await waitFor(() => expect(alertsButton().querySelector('.badge')).not.toBeNull());
    await userEvent.click(alertsButton());
    const decisions = screen.getAllByRole('button', { name: /^Open .* at its request$/ }).length;
    // Badge = pending decisions + open blocking tasks.
    expect(Number(alertsButton().querySelector('.badge')!.textContent)).toBe(decisions + 1);
    const tabs = screen.getByRole('tablist', { name: 'Alerts and to-do list' });
    const todo = within(tabs).getByRole('tab', { name: /To-do/ });
    expect(todo).toHaveTextContent('2');
    expect(within(tabs).getByRole('tab', { name: /Alerts/ })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await userEvent.click(todo);
    expect(todo).toHaveAttribute('aria-selected', 'true');
    await userEvent.click(screen.getByText('Approve the login on the router'));
    // Not the PC's own app: no administrator shell, run it on the PC.
    expect(screen.queryByRole('button', { name: 'Open admin PowerShell' })).toBeNull();
    expect(screen.getByText(/Needs to run on the PC itself/)).toBeInTheDocument();
    expect(screen.queryByText('To-do settings (this PC)')).toBeNull();
    // Checking it off lowers the badge.
    await userEvent.click(
      screen.getByRole('checkbox', { name: 'Check off Approve the login on the router' }),
    );
    await waitFor(() =>
      expect(Number(alertsButton().querySelector('.badge')?.textContent ?? '0')).toBe(decisions),
    );
  });

  it('turns an alert into a task only when asked', async () => {
    renderApp([]);
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(nav().getByRole('button', { name: /Alerts/ }));
    const todo = () => screen.getByRole('tab', { name: /To-do/ });
    expect(todo()).not.toHaveTextContent(/\d/);
    await userEvent.click(
      screen.getByRole('button', { name: "Add payments-api's request to the to-do list" }),
    );
    const dialog = await screen.findByRole('dialog', { name: 'New task' });
    expect((within(dialog).getByLabelText('Title') as HTMLInputElement).value).not.toBe('');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(todo()).toHaveTextContent('1'));
    // The alert itself stays until it is answered.
    expect(
      screen.getByRole('button', { name: 'Open payments-api at its request' }),
    ).toBeInTheDocument();
  });

  it('announces a reminder pushed by the PC', async () => {
    const transport = renderApp([mockTask({ id: 'b1', title: 'Approve it', blocking: true })]);
    await screen.findByTestId('session-card-wk-claude-api');
    act(() =>
      (transport as unknown as { push(body: ServerMsg): void }).push({
        type: 'task_reminder',
        reminder: {
          kind: 'blocking',
          task_id: 'b1',
          title: 'Approve it',
          session_id: null,
          open: 1,
          blocking: 1,
          at: 5,
        },
      }),
    );
    const toast = (await screen.findByText('An agent is waiting on a task for you.')).closest(
      'li',
    )!;
    await userEvent.click(within(toast).getByRole('button', { name: 'Open' }));
    expect(await screen.findByRole('tab', { name: /To-do/ })).toHaveAttribute(
      'aria-selected',
      'true',
    );
  });
});

describe('chat message to task', () => {
  it('offers "Add to to-do" on agent messages only', async () => {
    const onAddTask = vi.fn();
    render(
      <I18nProvider locale="en">
        <ChatView
          events={[
            { type: 'message', seq: 3, at: 1, role: 'user', text: 'please fix it' },
            {
              type: 'message',
              seq: 4,
              at: 2,
              role: 'assistant',
              text: 'Run this as administrator:\n```\nnet stop spooler\n```',
            },
          ]}
          gapFrom={null}
          onAddTask={onAddTask}
        />
      </I18nProvider>,
    );
    const buttons = screen.getAllByRole('button', { name: 'Add to to-do' });
    expect(buttons).toHaveLength(1);
    await userEvent.click(buttons[0]!);
    expect(onAddTask).toHaveBeenCalledWith(
      'Run this as administrator:\n```\nnet stop spooler\n```',
      4,
    );
  });
});
