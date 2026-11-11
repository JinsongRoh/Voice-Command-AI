import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import type { ClientMsg, ServerMsg, SessionInfo } from '../protocol/types';
import { SpeechProvider } from '../speech/context';
import { VocoTestProvider } from '../speech/useVoco';
import { PINS_KEY, resetPinsCache } from '../state/sessionPins';
import { defaultSettings } from '../state/settings';
import { AppProvider, useActions, useAppState, type RestoreLocalServices } from '../state/store';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { loadDraft } from './composerDrafts';
import { LiveView } from './LiveView';
import { SessionDeck } from './SessionDeck';
import { SessionSwitcher } from './SessionSwitcher';
import { ShellProvider } from './ShellProvider';
import { TerminalToolbar } from './TerminalToolbar';
import { SendToProvider } from './SendToSession';

// xterm needs a real canvas; the screen itself is not under test here.
vi.mock('./TerminalView', () => ({
  default: ({ sessionId }: { sessionId: string }) => <div data-testid={`terminal-${sessionId}`} />,
}));

const agent = localSession({
  id: 'a1',
  display_name: 'payments-api',
  origin: {
    type: 'local',
    agent: { type: 'claude_code' },
    project_dir: '/home/dev/payments-api',
    pid: 1,
  },
});
const other = localSession({
  id: 'b2',
  display_name: 'web-frontend',
  origin: {
    type: 'local',
    agent: { type: 'codex' },
    project_dir: '/home/dev/web-frontend',
    pid: 2,
  },
  state: { type: 'idle' },
  last_activity: Date.now() - 5_000,
});
const observed = localSession({
  id: 'o3',
  display_name: 'restored-window',
  control: 'terminal_api',
});

function shellIn(folder: string, overrides: Partial<SessionInfo> = {}): SessionInfo {
  return localSession({
    id: 'sh1',
    origin: { type: 'local', agent: { type: 'shell', name: 'bash' }, project_dir: folder, pid: 9 },
    display_name: 'payments-api · bash',
    state: { type: 'idle' },
    shell: {
      idle_timeout_secs: 900,
      idle_deadline: Date.now() + 14 * 60_000,
      hard_deadline: Date.now() + 50 * 60_000,
      opened_by: 'Pixel 9',
    },
    ...overrides,
  });
}

/** A server with `sessions`; `override` answers first. */
function server(
  sessions: SessionInfo[],
  override: (msg: ClientMsg) => ServerMsg | undefined = () => undefined,
) {
  return new FakeTransport((msg) => {
    const custom = override(msg);
    if (custom) return custom;
    if (msg.type === 'list_sessions') return { type: 'sessions', sessions };
    return defaultResponse(msg);
  });
}

/** Selects the session (which subscribes it) and renders its live view. */
function Selected({ id }: { id: string }) {
  const actions = useActions();
  const selected = useAppState((s) => (s.selectedId ? s.sessions[s.selectedId] : undefined));
  const known = useAppState((s) => !!s.sessions[id]);
  useEffect(() => {
    if (known) actions.select(id);
  }, [actions, known, id]);
  return selected ? <LiveView key={selected.id} session={selected} /> : null;
}

function renderLive(id: string, transport: FakeTransport) {
  return renderWithApp(
    <ShellProvider>
      <Selected id={id} />
    </ShellProvider>,
    { transport },
  );
}

const sends = (t: FakeTransport) =>
  t.bodies('send_input') as Extract<ClientMsg, { type: 'send_input' }>[];

function phone() {
  vi.stubGlobal(
    'matchMedia',
    vi.fn((query: string) => ({
      matches: query.includes('max-width: 899px'),
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    })),
  );
}

beforeEach(() => {
  localStorage.clear();
  resetPinsCache();
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('observed session Terminal tab', () => {
  it('explains that the session runs in a PC window, and that VCA sessions show here', async () => {
    renderLive('o3', server([observed]));
    await userEvent.click(await screen.findByRole('button', { name: 'Terminal' }));
    const note = await screen.findByTestId('observed-terminal');
    expect(note).toHaveTextContent(/runs in a terminal window on the PC/);
    expect(note).toHaveTextContent(/Sessions that VCA opens itself show their screen in this tab/);
    expect(note).not.toHaveTextContent(/started directly/);
    // Not this computer's own app: no window to bring forward.
    expect(
      screen.queryByRole('button', { name: 'Bring the PC window to front' }),
    ).not.toBeInTheDocument();
  });

  it('offers to bring the PC window to front where that is possible', async () => {
    const transport = server([observed]);
    render(
      <AppProvider
        machines={[
          {
            id: 'm',
            name: 'pc',
            connectivity: 'LAN',
            transport,
            simulate: { drop: () => {}, restoreLocal: {} as RestoreLocalServices },
          },
        ]}
        settings={defaultSettings()}
        persistSettings={false}
      >
        <I18nProvider locale="en">
          <SpeechProvider
            value={{
              transcribe: async () => ({ text: '', corrections: [] }),
              createRecorder: () => ({
                start: async () => {},
                stop: async () => new Blob(),
                cancel: () => {},
              }),
              fallbackWithoutMic: false,
            }}
          >
            <Selected id="o3" />
          </SpeechProvider>
        </I18nProvider>
      </AppProvider>,
    );
    await userEvent.click(await screen.findByRole('button', { name: 'Terminal' }));
    await userEvent.click(
      await screen.findByRole('button', { name: 'Bring the PC window to front' }),
    );
    expect(transport.bodies('focus_terminal')).toEqual([
      { type: 'focus_terminal', session_id: 'o3' },
    ]);
  });

  it('says the same in Korean, formally', async () => {
    const ko = (await import('../i18n/ko.json')).default as Record<string, string>;
    expect(ko['live.noTerminalScreen']).toMatch(/PC의 터미널 창에서 실행되므로/);
    expect(ko['live.noTerminalScreen']).toMatch(/표시됩니다\.$/);
    expect(ko['live.bringToFront']).toContain('PC');
  });
});

describe('Shell tab', () => {
  it('shows only where remote shells are allowed', async () => {
    const transport = server([agent], (msg) =>
      msg.type === 'list_devices'
        ? {
            type: 'devices',
            devices: [
              {
                id: 'd',
                name: 'me',
                platform: 'ios',
                paired_at: 0,
                last_seen: 0,
                current: true,
                scope: 'view',
              },
            ],
          }
        : undefined,
    );
    renderLive('a1', transport);
    await screen.findByRole('button', { name: 'Terminal' });
    await waitFor(() => expect(screen.queryByTestId('shell-tab-button')).not.toBeInTheDocument());
  });

  it('is not offered without the shell flows, on shells or on cloud sessions', async () => {
    renderWithApp(<Selected id="a1" />, { transport: server([agent]) });
    await screen.findByRole('button', { name: 'Terminal' });
    expect(screen.queryByTestId('shell-tab-button')).not.toBeInTheDocument();
  });

  it('opens a shell in the project folder through the code prompt, with its time left', async () => {
    let opened: SessionInfo | null = null;
    const transport = server([agent], (msg) => {
      if (msg.type !== 'open_shell') return undefined;
      if (!msg.request.totp) return { type: 'error', code: 'totp_required', message: 'code' };
      opened = shellIn(msg.request.project_dir, { id: 'sh9' });
      return { type: 'launched', session: opened };
    });
    renderLive('a1', transport);
    expect(await screen.findByRole('group', { name: 'Session actions' })).toBeInTheDocument();
    await userEvent.click(await screen.findByTestId('shell-tab-button'));
    expect(screen.getByTestId('shell-tab-start')).toHaveTextContent('/home/dev/payments-api');
    // The agent's Pause/Restart/Stop must not sit next to the shell's own controls.
    expect(screen.queryByRole('group', { name: 'Session actions' })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: /Open shell/ }));
    const dialog = await screen.findByRole('dialog', { name: 'Open a remote shell' });
    await userEvent.type(within(dialog).getByLabelText('Authenticator code'), '123456');
    await userEvent.click(within(dialog).getByRole('button', { name: /Open/ }));
    expect(await screen.findByTestId('shell-tab')).toBeInTheDocument();
    const withCode = transport
      .bodies('open_shell')
      .filter((b) => b.type === 'open_shell' && b.request.totp !== '');
    expect(withCode).toEqual([
      {
        type: 'open_shell',
        request: {
          shell: 'default',
          project_dir: '/home/dev/payments-api',
          display_name: null,
          totp: '123456',
        },
      },
    ]);
    expect(within(screen.getByTestId('shell-tab')).getByTestId('shell-countdown')).toBeVisible();
    // The shell streams next to the agent session; its screen is in the tab.
    expect(transport.bodies('subscribe')).toContainEqual({
      type: 'subscribe',
      session_id: 'sh9',
      from_seq: 0,
    });
    expect(screen.getByTestId('terminal-sh9')).toBeInTheDocument();
  });

  it('reuses the shell already open for the folder, and types into it', async () => {
    const transport = server([agent, shellIn('/home/dev/payments-api/')]);
    renderLive('a1', transport);
    await userEvent.click(await screen.findByTestId('shell-tab-button'));
    expect(screen.getByTestId('shell-tab')).toHaveTextContent('payments-api · bash');
    expect(screen.queryByTestId('shell-tab-start')).not.toBeInTheDocument();
    expect(transport.bodies('open_shell')).toEqual([]);
    // The input below the tab goes to the shell, not to the agent.
    const box = screen.getByRole('textbox', { name: /payments-api · bash/ });
    await userEvent.type(box, 'ipconfig /all');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(sends(transport).length).toBe(1));
    expect(sends(transport)[0]).toMatchObject({
      session_id: 'sh1',
      input: { type: 'text', text: 'ipconfig /all' },
    });
    // Leaving the tab stops the extra stream.
    await userEvent.click(screen.getByRole('button', { name: 'Chat' }));
    await waitFor(() =>
      expect(transport.bodies('unsubscribe')).toContainEqual({
        type: 'unsubscribe',
        session_id: 'sh1',
      }),
    );
  });
});

/** Renders the live view of `a1`; `b2` is another session. */
async function openChatWithMessage(transport: FakeTransport) {
  renderLive('a1', transport);
  await screen.findByRole('log');
  act(() => {
    transport.push({
      type: 'event',
      session_id: 'a1',
      event: { type: 'message', seq: 1, at: 1, role: 'assistant', text: 'Fix the login bug' },
    });
  });
  await screen.findByText('Fix the login bug');
}

describe('Send to session', () => {
  it('puts a chat message into the target session draft, naming the target first', async () => {
    const transport = server([agent, other]);
    await openChatWithMessage(transport);
    const log = screen.getByRole('log');
    await userEvent.click(within(log).getAllByRole('button', { name: 'Send to session…' })[0]!);
    const picker = await screen.findByRole('dialog', { name: 'Send to session' });
    // The session the text comes from is not a target.
    expect(within(picker).queryByTestId('pick-a1')).not.toBeInTheDocument();
    await userEvent.type(within(picker).getByRole('searchbox'), 'web');
    await userEvent.click(within(picker).getByTestId('pick-b2'));
    const step = await screen.findByRole('dialog', { name: 'Send to session' });
    expect(within(step).getByTestId('sendto-target')).toHaveTextContent('web-frontend');
    await userEvent.click(within(step).getByRole('button', { name: 'Put in its message box' }));
    expect(loadDraft('b2')?.text).toBe('Fix the login bug');
    expect(sends(transport)).toEqual([]);
  });

  it('names a cloud target once, with its provider instead of repeating the name', async () => {
    const cloud = localSession({
      id: 'c4',
      display_name: 'mobile-app: upgrade RN',
      origin: { type: 'cloud', provider: 'jules', remote_id: 'sessions/1', url: null },
      control: 'cloud',
      state: { type: 'idle' },
    });
    const transport = server([agent, cloud]);
    await openChatWithMessage(transport);
    await userEvent.click(
      within(screen.getByRole('log')).getAllByRole('button', { name: 'Send to session…' })[0]!,
    );
    await userEvent.click(await screen.findByTestId('pick-c4'));
    const target = await screen.findByTestId('sendto-target');
    expect(target).toHaveTextContent('Jules');
    expect(target.textContent!.match(/upgrade RN/g)).toHaveLength(1);
  });

  it('sends now only after a confirmation that shows the target name', async () => {
    const transport = server([agent, other]);
    await openChatWithMessage(transport);
    await userEvent.click(
      within(screen.getByRole('log')).getAllByRole('button', { name: 'Send to session…' })[0]!,
    );
    await userEvent.click(await screen.findByTestId('pick-b2'));
    await userEvent.click(await screen.findByRole('button', { name: 'Send now…' }));
    const confirm = await screen.findByRole('dialog', { name: 'Send to web-frontend now?' });
    expect(within(confirm).getByTestId('sendto-confirm-target')).toHaveTextContent('web-frontend');
    expect(sends(transport)).toEqual([]);
    await userEvent.click(within(confirm).getByRole('button', { name: 'Send to web-frontend' }));
    await waitFor(() => expect(sends(transport)).toHaveLength(1));
    // Only to the chosen session, never to the one on screen, never as a prompt answer.
    expect(sends(transport)[0]).toEqual({
      type: 'send_input',
      session_id: 'b2',
      input: { type: 'text', text: 'Fix the login bug', submit: true },
    });
  });

  it('never sends into a prompt the target has open, nor to an ended session', async () => {
    const waiting = { ...other, state: { type: 'awaiting_decision' as const } };
    const ended = localSession({
      id: 'e4',
      display_name: 'old-run',
      state: { type: 'exited', code: 0 },
    });
    const transport = server([agent, waiting, ended]);
    await openChatWithMessage(transport);
    await userEvent.click(
      within(screen.getByRole('log')).getAllByRole('button', { name: 'Send to session…' })[0]!,
    );
    await userEvent.click(await screen.findByTestId('pick-b2'));
    expect(screen.getByRole('button', { name: 'Send now…' })).toBeDisabled();
    expect(screen.getByText(/waiting for an answer/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Choose another session' }));
    await userEvent.click(await screen.findByTestId('pick-e4'));
    expect(screen.getByRole('button', { name: 'Send now…' })).toBeDisabled();
    expect(sends(transport)).toEqual([]);
  });

  it('sends a code block and a terminal selection', async () => {
    const transport = server([agent, other]);
    renderLive('a1', transport);
    await screen.findByRole('log');
    act(() => {
      transport.push({
        type: 'event',
        session_id: 'a1',
        event: {
          type: 'message',
          seq: 1,
          at: 1,
          role: 'assistant',
          text: 'Run:\n```bash\npnpm test\n```',
        },
      });
    });
    const code = (await screen.findByText('pnpm test')).closest('div')!.parentElement!;
    await userEvent.click(within(code).getByRole('button', { name: 'Send to session…' }));
    expect(await screen.findByRole('dialog', { name: 'Send to session' })).toHaveTextContent(
      'pnpm test',
    );

    const open = vi.fn();
    render(
      <I18nProvider locale="en">
        <SendToProvider value={open}>
          <TerminalToolbar
            selection="error: boom"
            zoomable={false}
            zoom={1}
            onZoom={() => {}}
            onFit={() => {}}
          />
        </SendToProvider>
      </I18nProvider>,
    );
    await userEvent.click(screen.getByTestId('terminal-send-selection'));
    expect(open).toHaveBeenCalledWith('error: boom');
  });
});

describe('@mention in the composer', () => {
  it('opens the picker on @, shows a To chip, and sends there without leaving', async () => {
    const transport = server([agent, other]);
    renderLive('a1', transport);
    const box = await screen.findByRole('textbox', { name: /payments-api/ });
    await userEvent.type(box, '@we');
    const list = screen.getByRole('listbox', { name: 'Send to session' });
    expect(within(list).getByRole('option', { name: /web-frontend/ })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await userEvent.keyboard('{Enter}');
    expect(box).toHaveValue('@web-frontend ');
    await userEvent.type(box, 'run the tests');
    const chip = screen.getByTestId('mention-chip');
    expect(chip).toHaveTextContent('web-frontend');
    await userEvent.click(screen.getByRole('button', { name: 'Send to web-frontend' }));
    await waitFor(() => expect(sends(transport)).toHaveLength(1));
    expect(sends(transport)[0]).toMatchObject({
      session_id: 'b2',
      input: { type: 'text', text: 'run the tests', submit: true },
    });
    expect(box).toHaveValue('');
    // Still on the same session.
    expect(screen.getByRole('textbox', { name: /payments-api/ })).toBe(box);
  });

  it('cancels the redirect when the chip is removed', async () => {
    const transport = server([agent, other]);
    renderLive('a1', transport);
    const box = await screen.findByRole('textbox', { name: /payments-api/ });
    fireEvent.change(box, { target: { value: '@web-frontend hello' } });
    expect(screen.getByTestId('mention-chip')).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole('button', { name: /Remove web-frontend as the target/ }),
    );
    expect(screen.queryByTestId('mention-chip')).not.toBeInTheDocument();
    expect(box).toHaveValue('hello');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(sends(transport)).toHaveLength(1));
    expect(sends(transport)[0]).toMatchObject({ session_id: 'a1' });
  });

  it("keeps the target's guards: a waiting prompt keeps the text", async () => {
    const waiting = { ...other, state: { type: 'awaiting_decision' as const } };
    const transport = server([agent, waiting]);
    renderLive('a1', transport);
    const box = await screen.findByRole('textbox', { name: /payments-api/ });
    fireEvent.change(box, { target: { value: '@web-frontend yes' } });
    await userEvent.click(screen.getByRole('button', { name: 'Send to web-frontend' }));
    expect(await screen.findByText(/waiting for an answer/)).toBeInTheDocument();
    expect(sends(transport)).toEqual([]);
    expect(box).toHaveValue('@web-frontend yes');
  });
});

describe('quick session switcher', () => {
  function Shown() {
    const selected = useAppState((s) => s.selectedId);
    return <output data-testid="selected">{selected ?? ''}</output>;
  }
  function Switcher() {
    const actions = useActions();
    return (
      <>
        <SessionSwitcher onOpen={(id) => actions.select(id)} />
        <Shown />
      </>
    );
  }
  const urgent = localSession({
    id: 'u5',
    display_name: 'billing',
    state: { type: 'awaiting_decision' },
    last_activity: Date.now() - 3_600_000,
  });
  const old = localSession({
    id: 'z9',
    display_name: 'zeta-old',
    state: { type: 'idle' },
    last_activity: Date.now() - 9_000_000,
  });

  it('searches every session while a category filter is on, and lists the filter without a query', async () => {
    renderWithApp(<Switcher />, { transport: server([agent, other, urgent]) });
    await waitFor(() => expect(screen.getByTestId('selected')).toBeInTheDocument());
    await userEvent.keyboard('{Control>}k{/Control}');
    const dialog = await screen.findByRole('dialog', { name: 'Switch session' });
    await userEvent.click(within(dialog).getByRole('button', { name: 'Needs attention' }));
    // The filter shapes the list...
    expect(within(dialog).queryByTestId('pick-b2')).not.toBeInTheDocument();
    expect(within(dialog).getByTestId('pick-u5')).toBeInTheDocument();
    // ...but a search still finds a session outside it.
    await userEvent.type(within(dialog).getByRole('searchbox'), 'front');
    expect(within(dialog).getByTestId('pick-b2')).toBeInTheDocument();
    expect(within(dialog).queryByText('No session matches.')).not.toBeInTheDocument();
  });

  it('opens with Ctrl+K, searches and switches', async () => {
    renderWithApp(<Switcher />, { transport: server([agent, other, urgent]) });
    await waitFor(() => expect(screen.getByTestId('selected')).toBeInTheDocument());
    await userEvent.keyboard('{Control>}k{/Control}');
    const dialog = await screen.findByRole('dialog', { name: 'Switch session' });
    const search = within(dialog).getByRole('searchbox');
    expect(search).toHaveFocus();
    await userEvent.type(search, 'front');
    expect(within(dialog).getByRole('heading', { name: 'Matches' })).toBeInTheDocument();
    expect(within(dialog).queryByTestId('pick-a1')).not.toBeInTheDocument();
    await userEvent.keyboard('{Enter}');
    expect(screen.getByTestId('selected')).toHaveTextContent('b2');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('groups needs attention, pinned, recent and all; pins stay on this device', async () => {
    const many = Array.from({ length: 6 }, (_, i) =>
      localSession({
        id: `r${i}`,
        display_name: `recent-${i}`,
        state: { type: 'idle' },
        last_activity: Date.now() - i * 1000,
      }),
    );
    renderWithApp(<Switcher />, { transport: server([urgent, old, ...many]) });
    await waitFor(() => expect(screen.getByTestId('selected')).toBeInTheDocument());
    await userEvent.keyboard('{Control>}k{/Control}');
    let dialog = await screen.findByRole('dialog', { name: 'Switch session' });
    const section = (name: string) =>
      within(dialog).getByRole('heading', { name }).closest('section')!;
    expect(within(section('Needs attention')).getByTestId('pick-u5')).toBeInTheDocument();
    expect(within(section('Recent')).getByTestId('pick-r0')).toBeInTheDocument();
    expect(within(section('All sessions')).getByTestId('pick-z9')).toBeInTheDocument();
    expect(within(dialog).queryByRole('heading', { name: 'Pinned' })).not.toBeInTheDocument();

    await userEvent.click(
      within(dialog).getByRole('button', { name: 'Pin zeta-old on this device' }),
    );
    expect(within(section('Pinned')).getByTestId('pick-z9')).toBeInTheDocument();
    expect(JSON.parse(localStorage.getItem(PINS_KEY)!)).toEqual(['z9']);
    // Escape closes; the pin is still there next time.
    await userEvent.keyboard('{Escape}');
    await userEvent.keyboard('{Control>}k{/Control}');
    dialog = await screen.findByRole('dialog', { name: 'Switch session' });
    expect(
      within(section('Pinned')).getByRole('button', { name: 'Unpin zeta-old' }),
    ).toHaveAttribute('aria-pressed', 'true');
  });

  it('keeps working when storage is unavailable', async () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('quota');
    });
    renderWithApp(<Switcher />, { transport: server([agent, old]) });
    await waitFor(() => expect(screen.getByTestId('selected')).toBeInTheDocument());
    await userEvent.keyboard('{Control>}k{/Control}');
    const dialog = await screen.findByRole('dialog', { name: 'Switch session' });
    await userEvent.click(
      within(dialog).getByRole('button', { name: 'Pin zeta-old on this device' }),
    );
    expect(within(dialog).getByRole('heading', { name: 'Pinned' })).toBeInTheDocument();
  });

  it('opens from the session title (the header without the phone chrome)', async () => {
    phone();
    renderWithApp(
      <ShellProvider>
        <Selected id="a1" />
        <Switcher />
      </ShellProvider>,
      { transport: server([agent, other]) },
    );
    await userEvent.click(await screen.findByTestId('live-title-switch'));
    const dialog = await screen.findByRole('dialog', { name: 'Switch session' });
    await userEvent.click(within(dialog).getByTestId('pick-b2'));
    expect(screen.getByTestId('selected')).toHaveTextContent('b2');
  });
});

describe('phone terminal', () => {
  it('has an input row and the key bar on the Terminal tab itself', async () => {
    phone();
    const transport = server([agent]);
    renderLive('a1', transport);
    await userEvent.click(await screen.findByRole('button', { name: 'Terminal' }));
    const input = await screen.findByTestId('terminal-input');
    const keys = within(input).getByRole('group', { name: 'Control keys' });
    for (const name of ['Enter', 'Esc', 'Tab', 'Up', 'Down', 'Left', 'Right']) {
      expect(within(keys).getByRole('button', { name })).toBeInTheDocument();
    }
    await userEvent.type(within(input).getByRole('textbox'), 'y');
    await userEvent.click(within(input).getByRole('button', { name: 'Send line (Enter)' }));
    await userEvent.click(within(keys).getByRole('button', { name: 'Esc' }));
    await userEvent.click(within(keys).getByRole('button', { name: 'Ctrl modifier' }));
    await userEvent.click(within(keys).getByRole('button', { name: 'Ctrl+C' }));
    await waitFor(() => expect(sends(transport)).toHaveLength(3));
    expect(sends(transport).map((b) => b.input)).toEqual([
      { type: 'text', text: 'y', submit: true, at_prompt: true },
      { type: 'key', key: 'escape' },
      { type: 'key', key: 'interrupt' },
    ]);
    // The full message box is one tap away (voice, attachments), and back.
    await userEvent.click(within(input).getByRole('button', { name: 'Open the full message box' }));
    expect(screen.queryByTestId('terminal-input')).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Back to the terminal input' }));
    expect(screen.getByTestId('terminal-input')).toBeInTheDocument();
  });

  it('opens with the message box while hands-free listens for "Voco", the input row a tap away', async () => {
    phone();
    const listener = {
      start: vi.fn(),
      pause: vi.fn(async () => {}),
      resume: vi.fn(),
      stop: vi.fn(async () => {}),
    };
    renderWithApp(
      <VocoTestProvider hooks={{ createListener: () => listener }}>
        <ShellProvider>
          <Selected id="a1" />
        </ShellProvider>
      </VocoTestProvider>,
      {
        transport: server([agent]),
        settings: { handsFree: true, handsFreeNoteSeen: true, vocoWake: true },
      },
    );
    await userEvent.click(await screen.findByRole('button', { name: 'Terminal' }));
    // The composer (where the wake word is heard) stays, not the input row.
    expect(screen.queryByTestId('terminal-input')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Send' })).toBeInTheDocument();
    await waitFor(() => expect(listener.start).toHaveBeenCalled());
    // The input row is still one tap away.
    await userEvent.click(screen.getByRole('button', { name: 'Back to the terminal input' }));
    expect(screen.getByTestId('terminal-input')).toBeInTheDocument();
  });

  it('keeps the composer on desktops', async () => {
    renderLive('a1', server([agent]));
    await userEvent.click(await screen.findByRole('button', { name: 'Terminal' }));
    expect(screen.queryByTestId('terminal-input')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Send' })).toBeInTheDocument();
  });
});

describe('ended sessions', () => {
  const endedShell = shellIn('/home/dev/payments-api', {
    id: 'sx',
    state: { type: 'exited', code: null },
    display_name: 'payments-api · bash',
  });

  it('reopens a remote shell in the same folder with the same shell, through the code', async () => {
    const transport = server([endedShell], (msg) => {
      if (msg.type !== 'open_shell') return undefined;
      if (!msg.request.totp) return { type: 'error', code: 'totp_required', message: 'code' };
      return { type: 'launched', session: shellIn(msg.request.project_dir, { id: 'sy' }) };
    });
    renderLive('sx', transport);
    const bar = await screen.findByTestId('ended-bar');
    expect(screen.queryByText(/Use Reopen above/)).not.toBeInTheDocument(); // the bar says it once
    await userEvent.click(within(bar).getByRole('button', { name: /Reopen/ }));
    const dialog = await screen.findByRole('dialog', { name: 'Open a remote shell' });
    await userEvent.type(within(dialog).getByLabelText('Authenticator code'), '654321');
    await userEvent.click(within(dialog).getByRole('button', { name: /Open/ }));
    await waitFor(() =>
      expect(
        transport.bodies('open_shell').filter((b) => b.type === 'open_shell' && b.request.totp),
      ).toEqual([
        {
          type: 'open_shell',
          request: {
            shell: 'bash',
            project_dir: '/home/dev/payments-api',
            display_name: null,
            totp: '654321',
          },
        },
      ]),
    );
  });

  it('restarts a session VCA started', async () => {
    const ended = { ...agent, state: { type: 'exited' as const, code: 1 } };
    const transport = server([ended]);
    renderLive('a1', transport);
    await userEvent.click(
      within(await screen.findByTestId('ended-bar')).getByRole('button', { name: /Reopen/ }),
    );
    await waitFor(() =>
      expect(transport.bodies('control')).toEqual([
        { type: 'control', session_id: 'a1', action: 'restart' },
      ]),
    );
  });

  it('offers no reopen for a session that ran in a PC window, and says why', async () => {
    const ended = { ...observed, state: { type: 'exited' as const, code: 0 } };
    renderLive('o3', server([ended]));
    const bar = await screen.findByTestId('ended-bar');
    expect(bar).toHaveTextContent(/ran in a terminal window on the PC/);
    expect(within(bar).queryByRole('button', { name: /Reopen/ })).not.toBeInTheDocument();
    expect(screen.queryByText(/cannot be reopened from VCA/)).not.toBeInTheDocument();
  });

  it('removes with dismiss_session when the server has it', async () => {
    const transport = new FakeTransport((msg) => {
      if (msg.type === 'hello')
        return {
          type: 'welcome',
          protocol_version: 1,
          server_version: 't',
          server_name: 't',
          features: ['dismiss_session'],
        };
      if (msg.type === 'list_sessions') return { type: 'sessions', sessions: [endedShell] };
      return defaultResponse(msg);
    });
    renderLive('sx', transport);
    await userEvent.click(
      within(await screen.findByTestId('ended-bar')).getByRole('button', {
        name: /Remove from VCA/,
      }),
    );
    await waitFor(() =>
      expect(transport.bodies('dismiss_session')).toEqual([
        { type: 'dismiss_session', session_id: 'sx' },
      ]),
    );
    expect(screen.queryByTestId('ended-bar')).not.toBeInTheDocument();
  });

  it('hides locally on servers without it, until the server drops the session', async () => {
    const transport = server([endedShell, agent]);
    function Order() {
      return <output data-testid="order">{useAppState((s) => s.order).join(',')}</output>;
    }
    renderWithApp(
      <ShellProvider>
        <Selected id="sx" />
        <Order />
      </ShellProvider>,
      { transport },
    );
    await userEvent.click(
      within(await screen.findByTestId('ended-bar')).getByRole('button', {
        name: /Remove from VCA/,
      }),
    );
    expect(transport.bodies('dismiss_session')).toEqual([]);
    expect(screen.getByTestId('order')).toHaveTextContent(/^a1$/);
    // A reconnect's list still has it (ended): it stays hidden.
    act(() => transport.push({ type: 'session_updated', session: endedShell }));
    expect(screen.getByTestId('order')).toHaveTextContent(/^a1$/);
  });

  it('warns before an idle shell closes, and keeps it open', async () => {
    const live = shellIn('/home/dev/payments-api', { id: 'sk' });
    const transport = server([live]);
    renderLive('sk', transport);
    await screen.findByRole('button', { name: 'Terminal' });
    act(() => transport.push({ type: 'shell_closing_soon', session_id: 'sk', seconds_left: 45 }));
    const banner = await screen.findByTestId('shell-closing');
    expect(banner).toHaveTextContent(/closes in 0:4\d/);
    // Announced once, not as a ticking assertive region.
    expect(banner).not.toHaveAttribute('role', 'alert');
    expect(within(banner).getAllByRole('alert')).toHaveLength(1);
    await userEvent.click(within(banner).getByRole('button', { name: 'Keep open' }));
    await waitFor(() =>
      expect(transport.bodies('shell_keep_alive')).toEqual([
        { type: 'shell_keep_alive', session_id: 'sk' },
      ]),
    );
    expect(screen.queryByTestId('shell-closing')).not.toBeInTheDocument();
  });
});

describe('ended sessions in the card menu and the phone carousel', () => {
  const endedAgent = { ...agent, state: { type: 'exited' as const, code: 1 } };
  const endedObserved = { ...observed, state: { type: 'exited' as const, code: 0 } };

  it('offers Reopen and Remove from VCA in the card menu', async () => {
    const transport = server([endedAgent, endedObserved]);
    renderWithApp(
      <ShellProvider>
        <SessionDeck onOpen={() => {}} onNewSession={() => {}} />
      </ShellProvider>,
      { transport },
    );
    await userEvent.click(
      await screen.findByRole('button', { name: 'More actions for payments-api' }),
    );
    let menu = screen.getByRole('menu');
    await userEvent.click(within(menu).getByRole('menuitem', { name: 'Reopen' }));
    await waitFor(() =>
      expect(transport.bodies('control')).toEqual([
        { type: 'control', session_id: 'a1', action: 'restart' },
      ]),
    );
    // A session that ran in a PC window: Remove only.
    await userEvent.click(screen.getByRole('button', { name: 'More actions for restored-window' }));
    menu = screen.getByRole('menu');
    expect(within(menu).queryByRole('menuitem', { name: 'Reopen' })).not.toBeInTheDocument();
    await userEvent.click(within(menu).getByRole('menuitem', { name: 'Remove from VCA' }));
    await waitFor(() =>
      expect(screen.queryByRole('button', { name: /restored-window/ })).not.toBeInTheDocument(),
    );
  });

  it('shows Reopen and Remove on the selected ended card of the carousel', async () => {
    const transport = server([endedAgent]);
    function Select() {
      const actions = useActions();
      const known = useAppState((s) => !!s.sessions.a1);
      useEffect(() => {
        if (known) actions.select('a1');
      }, [actions, known]);
      return null;
    }
    renderWithApp(
      <ShellProvider>
        <Select />
        <SessionDeck onOpen={() => {}} onNewSession={() => {}} variant="carousel" />
      </ShellProvider>,
      { transport },
    );
    await userEvent.click(await screen.findByRole('button', { name: 'Reopen payments-api' }));
    await waitFor(() => expect(transport.bodies('control')).toHaveLength(1));
    await userEvent.click(screen.getByRole('button', { name: 'Remove payments-api from VCA' }));
    await waitFor(() => expect(screen.queryByTestId('session-card-a1')).not.toBeInTheDocument());
  });
});

describe('search earlier messages', () => {
  // A chat taller than its view: the chat does not fetch earlier pages by itself.
  beforeEach(() => {
    Object.defineProperty(Element.prototype, 'scrollHeight', { configurable: true, value: 5000 });
    Object.defineProperty(Element.prototype, 'clientHeight', { configurable: true, value: 500 });
  });
  afterEach(() => {
    Reflect.deleteProperty(Element.prototype, 'scrollHeight');
    Reflect.deleteProperty(Element.prototype, 'clientHeight');
  });

  /** A server whose history pages wait for `release` (they never match the query). */
  function pagedServer() {
    const pending: Array<() => void> = [];
    const transport = server([agent], (msg) => {
      if (msg.type !== 'history') return undefined;
      const before = msg.before_seq ?? 1000;
      const page: ServerMsg = {
        type: 'history',
        session_id: msg.session_id,
        events: [
          { type: 'message', seq: before - 1, at: before - 1, role: 'assistant', text: 'ok' },
        ],
        has_more: true,
      };
      return new Promise<ServerMsg>((resolve) =>
        pending.push(() => resolve(page)),
      ) as unknown as ServerMsg;
    });
    return { transport, pending };
  }

  async function startSearch(transport: FakeTransport) {
    renderLive('a1', transport);
    await waitFor(() => expect(transport.bodies('subscribe')).toHaveLength(1));
    act(() => {
      transport.push({ type: 'stream_gap', session_id: 'a1', first_available_seq: 1000 });
      transport.push({
        type: 'event',
        session_id: 'a1',
        event: { type: 'message', seq: 1000, at: 1000, role: 'assistant', text: 'latest' },
      });
    });
    await userEvent.click(await screen.findByRole('button', { name: 'Search this chat' }));
    await userEvent.type(screen.getByRole('searchbox'), 'zzz');
    await userEvent.click(await screen.findByRole('button', { name: 'Search earlier messages' }));
    await waitFor(() => expect(transport.bodies('history')).toHaveLength(1));
  }

  const releaseAll = async (pending: Array<() => void>) => {
    await act(async () => {
      while (pending.length) pending.shift()!();
      await new Promise((r) => setTimeout(r, 20));
    });
  };

  it('stops paging with the Stop button', async () => {
    const { transport, pending } = pagedServer();
    await startSearch(transport);
    await userEvent.click(screen.getByRole('button', { name: 'Stop searching' }));
    await releaseAll(pending);
    expect(transport.bodies('history')).toHaveLength(1);
    expect(screen.getByRole('button', { name: 'Search earlier messages' })).toBeInTheDocument();
  });

  it('stops paging when the search closes', async () => {
    const { transport, pending } = pagedServer();
    await startSearch(transport);
    // Paging goes on while the search is open...
    await releaseAll(pending);
    await waitFor(() => expect(transport.bodies('history')).toHaveLength(2));
    // ...and stops once it closes.
    await userEvent.click(screen.getByRole('button', { name: 'Close search' }));
    await releaseAll(pending);
    await act(async () => {
      await new Promise((r) => setTimeout(r, 200));
    });
    await releaseAll(pending);
    expect(transport.bodies('history')).toHaveLength(2);
  });
});
