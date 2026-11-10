import { act, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { setPresenceBackend, TOTP_ONLY_NOTICE_KEY, type PresenceBackend } from '../native/presence';
import { classifyCommand } from '../protocol/mockShell';
import type { ClientMsg, ServerMsg, SessionInfo } from '../protocol/types';
import { SNIPPETS_KEY } from '../state/snippets';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { AuditLog } from './AuditLog';
import { ChatView } from './ChatView';
import { Composer } from './Composer';
import { DecisionSheet } from './DecisionSheet';
import { NewSessionDialog } from './NewSessionDialog';
import { ShellCountdown } from './ShellCountdown';
import { ShellProvider } from './ShellProvider';

const agent = localSession({ state: { type: 'awaiting_decision' } });

function shellSession(overrides: Partial<SessionInfo> = {}): SessionInfo {
  return localSession({
    id: 'sh1',
    origin: {
      type: 'local',
      agent: { type: 'shell', name: 'bash' },
      project_dir: '/home/dev/payments-api',
      pid: 7,
    },
    display_name: 'payments-api · bash',
    state: { type: 'idle' },
    profile: null,
    shell: {
      idle_timeout_secs: 900,
      idle_deadline: Date.now() + 14 * 60_000,
      hard_deadline: Date.now() + 50 * 60_000,
      opened_by: 'Pixel 9',
    },
    ...overrides,
  });
}

const decision = {
  id: 'dec-1',
  prompt: 'Allow Bash command?\nrm -rf 빌드 && pnpm build',
  options: [
    { id: 'yes', label: 'Yes' },
    { id: 'always', label: "Yes, and don't ask again" },
    { id: 'no', label: 'No, tell the agent' },
  ],
};

/** A server with an agent session and an open shell; `override` answers first. */
function server(
  override: (msg: ClientMsg) => ServerMsg | undefined = () => undefined,
  sessions: SessionInfo[] = [agent, shellSession()],
) {
  return new FakeTransport((msg) => {
    const custom = override(msg);
    if (custom) return custom;
    switch (msg.type) {
      case 'list_sessions':
        return { type: 'sessions', sessions };
      case 'classify_command':
        return { type: 'command_danger', ...classifyCommand(msg.command) };
      case 'open_shell':
        // Like vcad: without a code the answer is `totp_required`.
        return msg.request.totp
          ? { type: 'launched', session: shellSession({ id: 'sh2' }) }
          : { type: 'error', code: 'totp_required', message: 'enter a code' };
      case 'shell_run':
        return {
          type: 'shell_ran',
          session_id: msg.session_id,
          danger: classifyCommand(msg.command).level,
        };
      default:
        return defaultResponse(msg);
    }
  });
}

function phone(result: 'ok' | 'cancel' | 'none' = 'ok'): PresenceBackend & {
  authenticate: ReturnType<typeof vi.fn>;
} {
  const authenticate = vi.fn(async () => {
    if (result === 'cancel') throw { code: 'userCancel', message: 'cancelled' };
    if (result === 'none') throw { code: 'passcodeNotSet', message: 'no passcode' };
  });
  return { isPhone: async () => true, authenticate };
}

/** `open_shell` requests that carried a code (not the code-less first ask). */
function withCode(transport: FakeTransport) {
  return transport
    .bodies('open_shell')
    .filter((b) => b.type === 'open_shell' && b.request.totp !== '');
}

async function typeCode(code: string, dialogName = 'Open a remote shell') {
  const dialog = await screen.findByRole('dialog', { name: dialogName });
  const input = within(dialog).getByLabelText('Authenticator code');
  expect(input).toHaveAttribute('inputmode', 'numeric');
  expect(input).toHaveAttribute('autocomplete', 'one-time-code');
  await userEvent.type(input, code);
  return dialog;
}

beforeEach(() => {
  localStorage.clear();
});
afterEach(() => {
  setPresenceBackend(null);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('new shell session', () => {
  const renderDialog = (transport = server(), onLaunched = vi.fn()) => {
    renderWithApp(
      <ShellProvider>
        <NewSessionDialog
          projectDirs={['/home/dev/payments-api']}
          onClose={() => {}}
          onLaunched={onLaunched}
        />
      </ShellProvider>,
      { transport },
    );
    return onLaunched;
  };

  it('asks for biometrics, then the code, then opens the shell', async () => {
    const backend = phone('ok');
    setPresenceBackend(backend);
    const transport = server();
    const onLaunched = renderDialog(transport);
    await userEvent.click(screen.getByRole('button', { name: 'Shell' }));
    const picker = screen.getByLabelText('Shell');
    expect(
      within(picker)
        .getAllByRole('option')
        .map((o) => o.textContent),
    ).toEqual(["Computer's default", 'bash', 'zsh', 'sh']);
    await userEvent.selectOptions(picker, 'zsh');
    await userEvent.click(screen.getByRole('button', { name: 'Open shell' }));
    const dialog = await typeCode('123456');
    expect(backend.authenticate).toHaveBeenCalledTimes(1);
    expect(withCode(transport)).toEqual([]);
    await userEvent.click(within(dialog).getByRole('button', { name: 'Open shell' }));
    await waitFor(() => expect(onLaunched).toHaveBeenCalled());
    // First asked without a code (never opens a shell), then with it.
    expect(transport.bodies('open_shell')).toEqual([
      {
        type: 'open_shell',
        request: {
          shell: 'zsh',
          project_dir: '/home/dev/payments-api',
          display_name: null,
          totp: '',
        },
      },
      {
        type: 'open_shell',
        request: {
          shell: 'zsh',
          project_dir: '/home/dev/payments-api',
          display_name: null,
          totp: '123456',
        },
      },
    ]);
  });

  it('stops when the biometric check is cancelled', async () => {
    setPresenceBackend(phone('cancel'));
    const transport = server();
    renderDialog(transport);
    await userEvent.click(screen.getByRole('button', { name: 'Shell' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open shell' }));
    await waitFor(() => expect(screen.getByRole('button', { name: 'Open shell' })).toBeEnabled());
    expect(screen.queryByRole('dialog', { name: 'Open a remote shell' })).not.toBeInTheDocument();
    expect(withCode(transport)).toEqual([]);
  });

  it('shows the TOTP-only notice once on phones without biometrics or passcode', async () => {
    setPresenceBackend(phone('none'));
    renderDialog();
    await userEvent.click(screen.getByRole('button', { name: 'Shell' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open shell' }));
    const dialog = await screen.findByRole('dialog', { name: 'Open a remote shell' });
    expect(dialog).toHaveTextContent('protected by the authenticator code only');
    expect(localStorage.getItem(TOTP_ONLY_NOTICE_KEY)).toBe('1');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open shell' }));
    const again = await screen.findByRole('dialog', { name: 'Open a remote shell' });
    expect(again).not.toHaveTextContent('protected by the authenticator code only');
  });

  it('keeps the prompt open on a wrong code and explains each server error', async () => {
    let answer: ServerMsg = { type: 'error', code: 'totp_invalid', message: 'wrong code' };
    const transport = server((msg) =>
      msg.type === 'open_shell' && msg.request.totp ? answer : undefined,
    );
    renderDialog(transport);
    await userEvent.click(screen.getByRole('button', { name: 'Shell' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open shell' }));
    let dialog = await typeCode('111111');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Open shell' }));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent(
      'The code is wrong or was already used',
    );
    const cases: [ServerMsg, string][] = [
      [{ type: 'error', code: 'forbidden', message: 'scope' }, 'admin permission'],
      [{ type: 'error', code: 'forbidden', message: 'too many wrong codes' }, 'Wait 5 minutes'],
      [{ type: 'error', code: 'unsupported', message: 'remote shells are disabled' }, 'turned off'],
      [{ type: 'error', code: 'unavailable', message: 'pwsh not installed' }, 'not installed'],
    ];
    for (const [reply, text] of cases) {
      answer = reply;
      dialog = screen.getByRole('dialog', { name: 'Open a remote shell' });
      const input = within(dialog).getByLabelText('Authenticator code');
      await userEvent.clear(input);
      await userEvent.type(input, '222222');
      await userEvent.click(within(dialog).getByRole('button', { name: 'Open shell' }));
      await waitFor(() =>
        expect(
          screen.queryByRole('dialog', { name: 'Open a remote shell' }),
        ).not.toBeInTheDocument(),
      );
      expect(screen.getByRole('alert')).toHaveTextContent(text);
      await userEvent.click(screen.getByRole('button', { name: 'Open shell' }));
      await screen.findByRole('dialog', { name: 'Open a remote shell' });
    }
  });

  it('explains where the code comes from and takes a backup code in the same field', async () => {
    const transport = server();
    const onLaunched = renderDialog(transport);
    await userEvent.click(screen.getByRole('button', { name: 'Shell' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open shell' }));
    const dialog = await screen.findByRole('dialog', { name: 'Open a remote shell' });
    expect(dialog).toHaveTextContent(
      "the 6-digit number shown for the 'Voice Command AI' entry in the authenticator app you set up on your PC",
    );
    expect(dialog).toHaveTextContent('A backup code works in the same field');
    expect(within(dialog).getByText('No authenticator app?')).toBeInTheDocument();
    expect(dialog).toHaveTextContent(
      'VCA app › Settings › Security › Security status › 2-step verification',
    );

    await userEvent.click(within(dialog).getByRole('button', { name: 'Use a backup code' }));
    const input = within(dialog).getByLabelText('Backup code');
    expect(input).not.toHaveAttribute('inputmode', 'numeric');
    await userEvent.type(input, 'abcd-efgh-ijkl-mnop');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Open shell' }));
    await waitFor(() => expect(onLaunched).toHaveBeenCalled());
    expect(withCode(transport)).toEqual([
      expect.objectContaining({
        request: expect.objectContaining({ totp: 'ABCDEFGHIJKLMNOP' }),
      }),
    ]);
  });

  it('says so instead of asking for a code when no authenticator is enrolled', async () => {
    const backend = phone('ok');
    setPresenceBackend(backend);
    const transport = server((msg) =>
      msg.type === 'open_shell'
        ? { type: 'error', code: 'unsupported', message: 'TOTP is not set up on this computer' }
        : undefined,
    );
    renderDialog(transport);
    await userEvent.click(screen.getByRole('button', { name: 'Shell' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open shell' }));
    const dialog = await screen.findByRole('dialog', { name: 'Open a remote shell' });
    expect(dialog).toHaveTextContent('not set up on this computer yet');
    expect(dialog).toHaveTextContent(
      'VCA app › Settings › Security › Security status › 2-step verification',
    );
    expect(within(dialog).queryByRole('textbox')).not.toBeInTheDocument();
    expect(backend.authenticate).not.toHaveBeenCalled();
    expect(withCode(transport)).toEqual([]);
    const close = within(dialog).getAllByRole('button', { name: 'Close' });
    await userEvent.click(close[close.length - 1]!);
    expect(screen.queryByRole('dialog', { name: 'Open a remote shell' })).not.toBeInTheDocument();
  });

  it('switches to the explanation when an older server reports it only after the code', async () => {
    const transport = server((msg) =>
      msg.type === 'open_shell' && msg.request.totp
        ? { type: 'error', code: 'unsupported', message: 'TOTP is not enrolled' }
        : undefined,
    );
    renderDialog(transport);
    await userEvent.click(screen.getByRole('button', { name: 'Shell' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open shell' }));
    const dialog = await typeCode('123456');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Open shell' }));
    expect(await within(dialog).findByText(/not set up on this computer yet/)).toBeInTheDocument();
    expect(within(dialog).queryByRole('textbox')).not.toBeInTheDocument();
  });

  it('disables the shell choice for devices without admin permission', async () => {
    const transport = server((msg) =>
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
    renderDialog(transport);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Shell' })).toBeDisabled());
    expect(screen.getByText(/admin permission/)).toBeInTheDocument();
  });
});

describe('run in shell', () => {
  it('runs a decision command after target choice and confirmation, retrying with a code', async () => {
    setPresenceBackend(phone('ok'));
    const transport = server((msg) =>
      msg.type === 'shell_run' && !msg.totp
        ? { type: 'error', code: 'totp_required', message: 'needs a code' }
        : undefined,
    );
    renderWithApp(
      <ShellProvider>
        <DecisionSheet session={agent} decision={decision} />
      </ShellProvider>,
      { transport },
    );
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    await userEvent.click(await screen.findByRole('button', { name: 'Run in shell' }));
    const sheet = await screen.findByRole('dialog', { name: 'Run in shell' });
    // The open shell in the same folder is preselected.
    expect(within(sheet).getByRole('radio', { name: /payments-api · bash/ })).toBeChecked();
    expect(transport.bodies('shell_run')).toEqual([]);
    await userEvent.click(within(sheet).getByRole('button', { name: 'Next' }));
    await waitFor(() => expect(sheet.querySelector('mark')).toHaveTextContent('rm -rf'));
    expect(sheet).toHaveTextContent('Dangerous');
    expect(sheet).toHaveTextContent('Deletes files and folders recursively');
    expect(transport.bodies('shell_run')).toEqual([]);
    await userEvent.click(within(sheet).getByRole('button', { name: /Run anyway/ }));
    const prompt = await typeCode('123456', 'Confirm a dangerous command');
    await userEvent.click(within(prompt).getByRole('button', { name: 'Run' }));
    await waitFor(() =>
      expect(screen.queryByRole('dialog', { name: 'Run in shell' })).not.toBeInTheDocument(),
    );
    expect(transport.bodies('shell_run')).toEqual([
      { type: 'shell_run', session_id: 'sh1', command: 'rm -rf 빌드 && pnpm build', totp: null },
      {
        type: 'shell_run',
        session_id: 'sh1',
        command: 'rm -rf 빌드 && pnpm build',
        totp: '123456',
      },
    ]);
  });

  it('opens a new shell in the project folder when chosen', async () => {
    const transport = server(undefined, [agent]);
    renderWithApp(
      <ShellProvider>
        <DecisionSheet
          session={agent}
          decision={{ ...decision, prompt: 'Allow Bash command?\nls' }}
        />
      </ShellProvider>,
      { transport },
    );
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    await userEvent.click(screen.getByRole('button', { name: 'Run in shell' }));
    const sheet = await screen.findByRole('dialog', { name: 'Run in shell' });
    expect(within(sheet).getByRole('radio', { name: /Open a new shell/ })).toBeChecked();
    expect(within(sheet).getByLabelText('Project folder')).toHaveValue('/home/dev/payments-api');
    await userEvent.click(within(sheet).getByRole('button', { name: 'Next' }));
    await waitFor(() => expect(sheet).toHaveTextContent('Safe'));
    await userEvent.click(within(sheet).getByRole('button', { name: /^Run$/ }));
    const prompt = await typeCode('654321');
    await userEvent.click(within(prompt).getByRole('button', { name: 'Open shell' }));
    await waitFor(() => expect(transport.bodies('shell_run')).toHaveLength(1));
    expect(withCode(transport)[0]).toMatchObject({
      request: { project_dir: '/home/dev/payments-api', totp: '654321' },
    });
    expect(transport.bodies('shell_run')[0]).toMatchObject({ session_id: 'sh2', command: 'ls' });
  });

  it('sends a chat code block to the terminal through the same sheet', async () => {
    const transport = server();
    renderWithApp(
      <ShellProvider>
        <ChatView
          events={[
            {
              type: 'message',
              seq: 1,
              at: 1,
              role: 'assistant',
              text: 'Run this:\n```bash\n$ git status\n```\nand this script:\n```bash\na\nb\n```',
            },
          ]}
          gapFrom={null}
        />
      </ShellProvider>,
      { transport },
    );
    const buttons = screen.getAllByRole('button', { name: 'Send to terminal' });
    // Only the one-line block can be run.
    expect(buttons).toHaveLength(1);
    await userEvent.click(buttons[0]!);
    const sheet = await screen.findByRole('dialog', { name: 'Run in shell' });
    expect(within(sheet).getByTestId('command-preview')).toHaveTextContent(/^git status$/);
    expect(transport.bodies('shell_run')).toEqual([]);
  });

  it('shows no shell actions without the shell provider', () => {
    renderWithApp(<DecisionSheet session={agent} decision={decision} />);
    expect(screen.queryByRole('button', { name: 'Run in shell' })).not.toBeInTheDocument();
  });
});

describe('decision card on phones', () => {
  const phone = () =>
    vi.stubGlobal(
      'matchMedia',
      vi.fn((query: string) => ({
        matches: query.includes('max-width: 899px'),
        media: query,
        addEventListener: () => {},
        removeEventListener: () => {},
      })),
    );

  it('starts open when the request carries a command, and collapses to its summary', async () => {
    phone();
    renderWithApp(<DecisionSheet session={agent} decision={decision} />);
    const region = screen.getByRole('region', { name: /Pending decision/ });
    expect(region).toHaveAttribute('data-expanded', 'true');
    expect(within(region).getByRole('button', { name: "Yes, and don't ask again" })).toBeVisible();
    await userEvent.click(screen.getByRole('button', { name: 'Collapse the decision card' }));
    expect(region).toHaveAttribute('data-expanded', 'false');
    // The command stays readable (up to three lines), not only in a tooltip.
    expect(region).toHaveTextContent('rm -rf 빌드 && pnpm build');
    expect(
      within(region)
        .getAllByRole('button')
        .map((b) => b.textContent),
    ).toEqual(['', 'Yes', 'No, tell the agent', '+1 more']);
    await userEvent.click(screen.getByRole('button', { name: 'Show the whole decision' }));
    expect(region).toHaveAttribute('data-expanded', 'true');
  });

  it('starts collapsed for a request without a command', () => {
    phone();
    renderWithApp(
      <DecisionSheet
        session={agent}
        decision={{ ...decision, prompt: 'Continue with the plan?' }}
      />,
    );
    const region = screen.getByRole('region', { name: /Pending decision/ });
    expect(region).toHaveAttribute('data-expanded', 'false');
    expect(region).toHaveTextContent('Continue with the plan?');
  });
});

describe('shell countdown', () => {
  it('counts down to the deadline that comes first', () => {
    vi.useFakeTimers();
    try {
      const now = Date.now();
      const session = shellSession({
        shell: {
          idle_timeout_secs: 900,
          idle_deadline: now + 90_000,
          hard_deadline: now + 3_600_000,
          opened_by: 'x',
        },
      });
      renderWithApp(<ShellCountdown session={session} />);
      const el = screen.getByTestId('shell-countdown');
      expect(el).toHaveTextContent('Closes in 1:30 if idle');
      expect(el).toHaveAttribute('data-warn', 'true');
      act(() => {
        vi.advanceTimersByTime(30_000);
      });
      expect(el).toHaveTextContent('Closes in 1:00 if idle');
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('key bar', () => {
  it('sends Ctrl combinations', async () => {
    const transport = server();
    renderWithApp(<Composer session={shellSession()} />, { transport });
    const ctrl = screen.getByRole('button', { name: 'Ctrl modifier' });
    await userEvent.click(ctrl);
    expect(ctrl).toHaveAttribute('aria-pressed', 'true');
    await userEvent.click(screen.getByRole('button', { name: 'Ctrl+D' }));
    expect(ctrl).toHaveAttribute('aria-pressed', 'false');
    await userEvent.click(ctrl);
    await userEvent.click(screen.getByRole('button', { name: 'Ctrl+C' }));
    await waitFor(() =>
      expect(transport.bodies('send_input')).toEqual([
        {
          type: 'send_input',
          session_id: 'sh1',
          input: { type: 'text', text: '\x04', submit: false, at_prompt: true },
        },
        { type: 'send_input', session_id: 'sh1', input: { type: 'key', key: 'interrupt' } },
      ]),
    );
  });

  it('pastes the clipboard, and routes dangerous shell text to the confirm sheet', async () => {
    const readText = vi.fn(async () => 'echo hi');
    Object.defineProperty(navigator, 'clipboard', { value: { readText }, configurable: true });
    let refuse = false;
    const transport = server((msg) =>
      refuse && msg.type === 'send_input'
        ? { type: 'error', code: 'totp_required', message: 'dangerous' }
        : undefined,
    );
    renderWithApp(
      <ShellProvider>
        <Composer session={shellSession()} />
      </ShellProvider>,
      { transport },
    );
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    await userEvent.click(screen.getByRole('button', { name: 'Paste clipboard into the session' }));
    await waitFor(() =>
      expect(transport.bodies('send_input')).toEqual([
        {
          type: 'send_input',
          session_id: 'sh1',
          input: { type: 'text', text: 'echo hi', submit: false, at_prompt: true },
        },
      ]),
    );
    refuse = true;
    readText.mockResolvedValue('rm -rf /tmp/x');
    await userEvent.click(screen.getByRole('button', { name: 'Paste clipboard into the session' }));
    const sheet = await screen.findByRole('dialog', { name: 'Run in shell' });
    expect(within(sheet).getByTestId('command-preview')).toHaveTextContent('rm -rf /tmp/x');
    expect(transport.bodies('shell_run')).toEqual([]);
  });

  it('lets a key bar error fade out after a few seconds', async () => {
    const readText = vi.fn(async () => '');
    Object.defineProperty(navigator, 'clipboard', { value: { readText }, configurable: true });
    const transport = server(() => undefined);
    renderWithApp(<Composer session={shellSession()} />, { transport });
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    await userEvent.click(screen.getByRole('button', { name: 'Paste clipboard into the session' }));
    expect(await screen.findByText('The clipboard is empty.')).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText('The clipboard is empty.')).toBeNull(), {
      timeout: 4500,
    });
  }, 8000);

  it('saves, inserts and removes snippets (stored on this device)', async () => {
    renderWithApp(<Composer session={shellSession()} />);
    const input = screen.getByRole('textbox');
    await userEvent.type(input, 'git pull --rebase');
    await userEvent.click(screen.getByRole('button', { name: 'Saved snippets' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save text box' }));
    expect(JSON.parse(localStorage.getItem(SNIPPETS_KEY)!)).toEqual(['git pull --rebase']);
    await userEvent.clear(input);
    await userEvent.click(
      screen.getByRole('button', { name: 'Put in the text box: git pull --rebase' }),
    );
    expect(input).toHaveValue('git pull --rebase');
    await userEvent.click(screen.getByRole('button', { name: 'Saved snippets' }));
    await userEvent.click(
      screen.getByRole('button', { name: 'Remove snippet: git pull --rebase' }),
    );
    expect(JSON.parse(localStorage.getItem(SNIPPETS_KEY)!)).toEqual([]);
  });

  it('keeps working when storage is unavailable', async () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('quota');
    });
    renderWithApp(<Composer session={shellSession()} />);
    await userEvent.type(screen.getByRole('textbox'), 'ls -la');
    await userEvent.click(screen.getByRole('button', { name: 'Saved snippets' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save text box' }));
    expect(screen.getByRole('button', { name: 'Put in the text box: ls -la' })).toBeInTheDocument();
  });
});

describe('activity log', () => {
  it('lists audit entries and refreshes', async () => {
    let calls = 0;
    const transport = server((msg) => {
      if (msg.type !== 'list_audit') return undefined;
      calls += 1;
      return {
        type: 'audit',
        entries: [
          {
            ts: Date.now() - 60_000,
            device_id: 'd1',
            device_name: 'Pixel 9',
            event: 'shell_run',
            session_id: 'sh1',
            detail: calls > 1 ? 'danger=dangerous' : 'danger=safe',
          },
          {
            ts: Date.now() - 120_000,
            device_id: 'd2',
            device_name: 'iPad',
            event: 'step_up_failed',
            session_id: null,
            detail: 'wrong code',
          },
        ],
      };
    });
    renderWithApp(<AuditLog />, { transport });
    const table = await screen.findByRole('table');
    const rows = within(table).getAllByRole('row');
    expect(rows).toHaveLength(3);
    expect(rows[1]).toHaveTextContent('Pixel 9');
    expect(rows[1]).toHaveTextContent('Command run');
    expect(rows[1]).toHaveTextContent('payments-api · bash');
    expect(rows[2]).toHaveTextContent('Wrong code');
    expect(rows[2]).toHaveAttribute('data-alert', 'true');
    await userEvent.click(screen.getByRole('button', { name: 'Refresh' }));
    await waitFor(() => expect(table).toHaveTextContent('danger=dangerous'));
    expect(transport.bodies('list_audit')[0]).toEqual({ type: 'list_audit', limit: 200 });
  });

  it('explains when the device may not read it', async () => {
    const transport = server((msg) =>
      msg.type === 'list_audit'
        ? { type: 'error', code: 'forbidden', message: 'scope' }
        : undefined,
    );
    renderWithApp(<AuditLog />, { transport });
    expect(await screen.findByRole('alert')).toHaveTextContent('admin permission');
  });
});

describe('shell notices', () => {
  it('shows a toast when a remote shell opens, even with activity alerts off', async () => {
    const transport = server();
    const { Toasts } = await import('./Toasts');
    renderWithApp(<Toasts onOpen={() => {}} />, { transport });
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    act(() =>
      transport.push({
        type: 'shell_notice',
        session_id: 'sh1',
        event: 'opened',
        device_name: 'Pixel 9',
        reason: null,
      }),
    );
    expect(await screen.findByText('Remote shell opened by Pixel 9')).toBeInTheDocument();
    act(() =>
      transport.push({
        type: 'shell_notice',
        session_id: 'sh1',
        event: 'closed',
        device_name: 'Pixel 9',
        reason: 'idle',
      }),
    );
    expect(await screen.findByText('Remote shell closed: payments-api · bash')).toBeInTheDocument();
    expect(screen.getByText('No input for too long')).toBeInTheDocument();
    fireEvent.click(screen.getAllByRole('button', { name: 'Dismiss' })[0]!);
  });
});
