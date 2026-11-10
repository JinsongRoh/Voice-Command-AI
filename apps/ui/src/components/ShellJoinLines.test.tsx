import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { classifyCommand } from '../protocol/mockShell';
import { submissionLines } from '../protocol/shell';
import type { ClientMsg, ServerMsg, SessionInfo } from '../protocol/types';
import { useActions, useAppState } from '../state/store';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { Composer } from './Composer';
import { ShellJoinBanner } from './SessionEnded';
import { ShellProvider } from './ShellProvider';

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

const JOIN_MESSAGE =
  'this shell was opened on another device; join it with a code from your authenticator app';

/** Like vcad: multi-line input is held back, dangerous lines need a code. */
function server(override: (msg: ClientMsg) => ServerMsg | undefined = () => undefined) {
  return new FakeTransport((msg) => {
    const custom = override(msg);
    if (custom) return custom;
    switch (msg.type) {
      case 'list_sessions':
        return { type: 'sessions', sessions: [shellSession()] };
      case 'classify_command':
        return { type: 'command_danger', ...classifyCommand(msg.command) };
      case 'send_input':
        if (msg.input.type === 'text' && submissionLines(msg.input.text).length > 1) {
          return {
            type: 'shell_confirm',
            session_id: msg.session_id,
            command: msg.input.text,
            lines: submissionLines(msg.input.text).length,
          };
        }
        return defaultResponse(msg);
      case 'shell_run': {
        const lines = submissionLines(msg.command);
        if (lines.length > 1 && !msg.confirm) {
          return {
            type: 'shell_confirm',
            session_id: msg.session_id,
            command: msg.command,
            lines: lines.length,
          };
        }
        const dangerous = lines.some((l) => classifyCommand(l).level === 'dangerous');
        if (dangerous && !msg.totp) {
          return { type: 'error', code: 'totp_required', message: 'a dangerous command' };
        }
        return {
          type: 'shell_ran',
          session_id: msg.session_id,
          danger: dangerous ? 'dangerous' : 'safe',
        };
      }
      default:
        return defaultResponse(msg);
    }
  });
}

function pasting(text: string) {
  const readText = vi.fn(async () => text);
  Object.defineProperty(navigator, 'clipboard', { value: { readText }, configurable: true });
  return readText;
}

beforeEach(() => localStorage.clear());
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('submission lines', () => {
  it('counts lines like the server: one trailing line break is not a line', () => {
    for (const one of ['ls', 'ls\n', 'ls\r\n', 'ls\r', '\r', '']) {
      expect(submissionLines(one)).toHaveLength(1);
    }
    expect(submissionLines('echo a\necho b\n')).toEqual(['echo a', 'echo b']);
    expect(submissionLines('a\r\nb')).toEqual(['a', 'b']);
    expect(submissionLines('a\rb')).toEqual(['a', 'b']);
    expect(submissionLines('cat <<EOF\nhi\nEOF')).toHaveLength(3);
    expect(submissionLines('echo a\n\n')).toHaveLength(2);
  });
});

describe('multi-line shell input', () => {
  const renderComposer = (transport = server()) => {
    renderWithApp(
      <ShellProvider>
        <Composer session={shellSession()} />
      </ShellProvider>,
      { transport },
    );
    return transport;
  };

  it('shows all lines and runs them only after an explicit confirmation', async () => {
    const script = 'echo one\necho two\necho three';
    pasting(script);
    const transport = renderComposer();
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    const paste = screen.getByRole('button', { name: 'Paste clipboard into the session' });

    // Cancel: nothing runs.
    await userEvent.click(paste);
    let dialog = await screen.findByRole('dialog', { name: 'Run these 3 lines?' });
    expect(within(dialog).getByTestId('shell-lines-text').textContent).toBe(script);
    await userEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(transport.bodies('shell_run')).toEqual([]);

    // Run: resent once, with `confirm`.
    await userEvent.click(paste);
    dialog = await screen.findByRole('dialog', { name: 'Run these 3 lines?' });
    await userEvent.click(within(dialog).getByRole('button', { name: 'Run 3 lines' }));
    await waitFor(() =>
      expect(transport.bodies('shell_run')).toEqual([
        { type: 'shell_run', session_id: 'sh1', command: script, totp: null, confirm: true },
      ]),
    );
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('asks for a code when one of the confirmed lines is dangerous', async () => {
    const script = 'echo fine\nrm -rf /tmp/x';
    pasting(script);
    const transport = renderComposer();
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    await userEvent.click(screen.getByRole('button', { name: 'Paste clipboard into the session' }));
    const dialog = await screen.findByRole('dialog', { name: 'Run these 2 lines?' });
    await userEvent.click(within(dialog).getByRole('button', { name: 'Run 2 lines' }));
    const prompt = await screen.findByRole('dialog', { name: 'Confirm dangerous lines' });
    await userEvent.type(within(prompt).getByLabelText('Authenticator code'), '123456');
    await userEvent.click(within(prompt).getByRole('button', { name: 'Run 2 lines' }));
    await waitFor(() =>
      expect(transport.bodies('shell_run')).toEqual([
        { type: 'shell_run', session_id: 'sh1', command: script, totp: null, confirm: true },
        { type: 'shell_run', session_id: 'sh1', command: script, totp: '123456', confirm: true },
      ]),
    );
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('does not ask for a single command with its Enter', async () => {
    pasting('echo hi\n');
    const transport = renderComposer();
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    await userEvent.click(screen.getByRole('button', { name: 'Paste clipboard into the session' }));
    await waitFor(() => expect(transport.bodies('send_input')).toHaveLength(1));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});

describe('joining a shell opened on another device', () => {
  function Selected({ id }: { id: string }) {
    const actions = useActions();
    const session = useAppState((s) => s.sessions[id]);
    const loaded = !!session;
    // After the list arrived (the connection resets the selection first).
    useEffect(() => {
      if (loaded) actions.select(id);
    }, [actions, id, loaded]);
    return session ? <ShellJoinBanner session={session} /> : null;
  }

  it('asks for a code once, joins, then streams the shell', async () => {
    let joined = false;
    const transport = server((msg) => {
      if (msg.type === 'subscribe' && !joined) {
        return { type: 'error', code: 'totp_required', message: JOIN_MESSAGE };
      }
      if (msg.type === 'shell_join') {
        joined = true;
        return { type: 'ok' };
      }
      return undefined;
    });
    renderWithApp(
      <ShellProvider>
        <Selected id="sh1" />
      </ShellProvider>,
      { transport },
    );
    const banner = await screen.findByTestId('shell-join');
    expect(banner).toHaveTextContent('This shell was opened on another device');
    await userEvent.click(within(banner).getByRole('button', { name: 'Join with code' }));
    const prompt = await screen.findByRole('dialog', { name: 'Join this shell' });
    expect(prompt).toHaveTextContent('payments-api · bash was opened on Pixel 9');
    await userEvent.type(within(prompt).getByLabelText('Authenticator code'), '654321');
    await userEvent.click(within(prompt).getByRole('button', { name: 'Join' }));
    await waitFor(() =>
      expect(transport.bodies('shell_join')).toEqual([
        { type: 'shell_join', session_id: 'sh1', totp: '654321' },
      ]),
    );
    await waitFor(() => expect(screen.queryByTestId('shell-join')).not.toBeInTheDocument());
    // Streamed again after joining.
    await waitFor(() => expect(transport.bodies('subscribe').length).toBeGreaterThanOrEqual(2));
  });

  it('offers to join when typing into a shell the device has not joined', async () => {
    const transport = server((msg) =>
      msg.type === 'send_input'
        ? { type: 'error', code: 'totp_required', message: JOIN_MESSAGE }
        : undefined,
    );
    renderWithApp(
      <ShellProvider>
        <Composer session={shellSession()} />
      </ShellProvider>,
      { transport },
    );
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    pasting('ls');
    await userEvent.click(screen.getByRole('button', { name: 'Paste clipboard into the session' }));
    expect(await screen.findByRole('dialog', { name: 'Join this shell' })).toBeInTheDocument();
    expect(transport.bodies('shell_run')).toEqual([]);
  });
});
