import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactElement } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { defaultConductorState } from '../../protocol/mockConductor';
import type { ClientMsg, ServerMsg, SessionInfo } from '../../protocol/types';
import { defaultResponse, FakeTransport } from '../../test/fakeTransport';
import { localSession } from '../../test/fixtures';
import { renderWithApp } from '../../test/render';
import { ShellProvider } from '../ShellProvider';
import { CommandCenterView } from './CommandCenterView';
import { commandShellName, conductorFolders, defaultShellFolder } from './shellFolders';

// xterm needs a real canvas; the screen itself is not under test here.
vi.mock('../TerminalView', () => ({
  default: ({ sessionId }: { sessionId: string }) => <div data-testid={`terminal-${sessionId}`} />,
}));

const CONDUCTOR_DIR = '/home/dev/.local/share/vca/conductor';

const conductorSession = localSession({
  id: 'cond',
  display_name: 'conductor',
  conductor: true,
  origin: {
    type: 'local',
    agent: { type: 'claude_code' },
    project_dir: CONDUCTOR_DIR,
    pid: 1,
  },
  // The most recently active session: never the shell's default folder.
  last_activity: Date.now(),
});
const api = localSession({
  id: 'a1',
  display_name: 'payments-api',
  origin: {
    type: 'local',
    agent: { type: 'claude_code' },
    project_dir: '/home/dev/payments-api',
    pid: 2,
  },
  last_activity: Date.now() - 60_000,
});
const web = localSession({
  id: 'w1',
  display_name: 'web',
  origin: { type: 'local', agent: { type: 'codex' }, project_dir: '/home/dev/web', pid: 3 },
  last_activity: Date.now() - 600_000,
});

function shellIn(folder: string, overrides: Partial<SessionInfo> = {}): SessionInfo {
  return localSession({
    id: 'sh1',
    origin: { type: 'local', agent: { type: 'shell', name: 'bash' }, project_dir: folder, pid: 9 },
    display_name: 'payments-api · bash',
    state: { type: 'idle' },
    last_activity: Date.now() - 3_600_000,
    shell: {
      idle_timeout_secs: 900,
      idle_deadline: Date.now() + 14 * 60_000,
      hard_deadline: Date.now() + 50 * 60_000,
      opened_by: 'Pixel 9',
    },
    ...overrides,
  });
}

const folder = (path: string) => ({
  name: path.split(/[\\/]/).pop()!,
  path,
  is_git_repo: true,
  has_agent_history: true,
});

interface ServerOptions {
  sessions: SessionInfo[];
  /** Recent project folders; `null`: a server without the folder picker. */
  recent?: string[] | null;
  scope?: 'admin' | 'view';
  override?: (msg: ClientMsg) => ServerMsg | undefined;
}

function server({ sessions, recent = null, scope, override }: ServerOptions) {
  return new FakeTransport((msg) => {
    const custom = override?.(msg);
    if (custom) return custom;
    switch (msg.type) {
      case 'hello':
        return {
          type: 'welcome',
          protocol_version: 1,
          server_version: 'test',
          server_name: 'test',
          features: ['conductor', ...(recent ? ['project_folders'] : [])],
        };
      case 'list_sessions':
        return { type: 'sessions', sessions };
      case 'conductor_get':
        return {
          type: 'conductor',
          state: {
            ...defaultConductorState(),
            settings: { ...defaultConductorState().settings, enabled: true },
            status: 'ready',
            session_id: 'cond',
          },
        };
      case 'project_folders':
        return {
          type: 'project_folders',
          roots: [folder('/home/dev')],
          recent: (recent ?? []).map(folder),
          os: 'linux',
          separator: '/',
        };
      case 'list_devices':
        if (!scope) break;
        return {
          type: 'devices',
          devices: [
            {
              id: 'd',
              name: 'me',
              platform: 'ios',
              paired_at: 0,
              last_seen: 0,
              current: true,
              scope,
            },
          ],
        };
    }
    return defaultResponse(msg);
  });
}

function renderCenter(transport: FakeTransport, withShell = true) {
  const view: ReactElement = <CommandCenterView />;
  return renderWithApp(withShell ? <ShellProvider>{view}</ShellProvider> : view, {
    transport,
    settings: { language: 'en' },
  });
}

/** The conversation's view switch (Chat / Terminal / shell). */
async function viewModes() {
  const conversation = await screen.findByTestId('cc-conversation');
  return within(conversation).findByRole('group', { name: 'View mode' });
}

async function openShellTab(name = 'Shell') {
  await userEvent.click(await within(await viewModes()).findByRole('button', { name }));
  return screen.findByTestId('cc-shell');
}

beforeEach(() => {
  localStorage.clear();
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('지휘소 shell tab', () => {
  it('sits in the conversation next to Chat and Terminal, not among the top tabs', async () => {
    renderCenter(server({ sessions: [conductorSession, api], scope: 'admin' }));
    const tablist = await screen.findByRole('tablist');
    expect(
      within(tablist)
        .getAllByRole('tab')
        .map((tab) => tab.textContent),
    ).toEqual(['Conversation', 'Approvals', 'Log']);
    const modes = await viewModes();
    expect(within(modes).getByRole('button', { name: /Chat/ })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    expect(within(modes).getByRole('button', { name: /Terminal/ })).toBeInTheDocument();
    const shellButton = await within(modes).findByRole('button', { name: 'Shell' });
    expect(shellButton).toHaveAttribute('aria-pressed', 'false');
    await userEvent.click(shellButton);
    expect(shellButton).toHaveAttribute('aria-pressed', 'true');
    expect(await screen.findByTestId('cc-shell')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Change folder' })).toBeInTheDocument();
  });

  it('is hidden for view-only devices', async () => {
    renderCenter(server({ sessions: [conductorSession, api], scope: 'view' }));
    const modes = await viewModes();
    await waitFor(() =>
      expect(within(modes).queryByRole('button', { name: 'Shell' })).not.toBeInTheDocument(),
    );
    expect(within(modes).getByRole('button', { name: /Terminal/ })).toBeInTheDocument();
  });

  it('is hidden without the shell flows', async () => {
    renderCenter(server({ sessions: [conductorSession, api] }), false);
    const modes = await viewModes();
    expect(within(modes).getByRole('button', { name: /Terminal/ })).toBeInTheDocument();
    expect(within(modes).queryByRole('button', { name: 'Shell' })).not.toBeInTheDocument();
  });

  it('starts in the most recent project folder, skipping the conductor’s', async () => {
    renderCenter(
      server({
        sessions: [conductorSession, api, web],
        recent: [CONDUCTOR_DIR, '/home/dev/web', '/home/dev/payments-api'],
      }),
    );
    await openShellTab();
    await waitFor(() =>
      expect(screen.getByTestId('cc-shell-folder')).toHaveTextContent('/home/dev/web'),
    );
    expect(screen.getByTestId('shell-tab-start')).toHaveTextContent('/home/dev/web');
  });

  it('falls back to the most recently active session’s folder, never the conductor’s', async () => {
    const transport = server({
      sessions: [conductorSession, api, web],
      override: (msg) => {
        if (msg.type !== 'open_shell') return undefined;
        if (!msg.request.totp) return { type: 'error', code: 'totp_required', message: 'code' };
        return {
          type: 'launched',
          session: shellIn(msg.request.project_dir, { id: 'sy' }),
        };
      },
    });
    renderCenter(transport);
    await openShellTab();
    await waitFor(() =>
      expect(screen.getByTestId('cc-shell-folder')).toHaveTextContent('/home/dev/payments-api'),
    );
    await userEvent.click(screen.getByRole('button', { name: /Open shell/ }));
    const dialog = await screen.findByRole('dialog', { name: 'Open a remote shell' });
    await userEvent.type(within(dialog).getByLabelText('Authenticator code'), '654321');
    await userEvent.click(within(dialog).getByRole('button', { name: /Open/ }));
    await waitFor(() =>
      expect(
        transport.bodies('open_shell').some((b) => b.type === 'open_shell' && b.request.totp),
      ).toBe(true),
    );
    for (const body of transport.bodies('open_shell')) {
      if (body.type !== 'open_shell') continue;
      expect(body.request.project_dir).toBe('/home/dev/payments-api');
      expect(body.request.project_dir).not.toBe(CONDUCTOR_DIR);
    }
  });

  it('reuses the shell already open in the folder, with the input row typing into it', async () => {
    const open = shellIn('/home/dev/payments-api');
    const transport = server({ sessions: [conductorSession, api, open] });
    renderCenter(transport);
    await openShellTab();
    const tab = await screen.findByTestId('shell-tab');
    expect(tab).toHaveTextContent('payments-api · bash');
    expect(screen.queryByTestId('shell-tab-start')).not.toBeInTheDocument();
    expect(screen.getByTestId('terminal-sh1')).toBeInTheDocument();
    expect(transport.bodies('open_shell')).toHaveLength(0);
    // The composer below types into the shell, not into the conductor.
    await userEvent.type(
      screen.getByRole('textbox', { name: 'Command for payments-api · bash' }),
      'git status',
    );
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(transport.bodies('send_input').length).toBeGreaterThan(0));
    for (const body of transport.bodies('send_input')) {
      if (body.type === 'send_input') expect(body.session_id).toBe('sh1');
    }
  });

  it('changes the folder with the folder picker, refusing the conductor’s folder', async () => {
    renderCenter(
      server({
        sessions: [conductorSession, api, web],
        recent: ['/home/dev/payments-api', CONDUCTOR_DIR, '/home/dev/web'],
      }),
    );
    await openShellTab();
    await waitFor(() =>
      expect(screen.getByTestId('cc-shell-folder')).toHaveTextContent('/home/dev/payments-api'),
    );
    await userEvent.click(screen.getByRole('button', { name: 'Change folder' }));
    const dialog = await screen.findByRole('dialog', { name: 'Change folder' });
    const use = within(dialog).getByRole('button', { name: 'Use this folder' });
    await userEvent.click(await within(dialog).findByRole('button', { name: /^conductor/ }));
    expect(within(dialog).getByRole('alert')).toHaveTextContent(/conductor’s own folder/);
    expect(use).toBeDisabled();
    await userEvent.click(within(dialog).getByRole('button', { name: /^web/ }));
    expect(use).toBeEnabled();
    await userEvent.click(use);
    expect(screen.queryByRole('dialog', { name: 'Change folder' })).not.toBeInTheDocument();
    expect(screen.getByTestId('cc-shell-folder')).toHaveTextContent('/home/dev/web');
  });

  it('opens PowerShell on Windows', async () => {
    const win = (dir: string, id: string, at: number) =>
      localSession({
        id,
        origin: { type: 'local', agent: { type: 'claude_code' }, project_dir: dir, pid: 4 },
        last_activity: at,
      });
    const transport = server({
      sessions: [
        win('C:\\Users\\ceo\\AppData\\Local\\vca', 'cond', Date.now()),
        win('D:\\code\\payments-api', 'a1', Date.now() - 1_000),
      ],
      override: (msg) =>
        msg.type === 'open_shell' ? { type: 'error', code: 'forbidden', message: 'no' } : undefined,
    });
    renderCenter(transport);
    const modes = await viewModes();
    expect(within(modes).getByRole('button', { name: /Chat/ })).toBeInTheDocument();
    expect(within(modes).getByRole('button', { name: /Terminal/ })).toBeInTheDocument();
    await openShellTab('PowerShell');
    expect(within(modes).getByRole('button', { name: 'PowerShell' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    await waitFor(() =>
      expect(screen.getByTestId('cc-shell-folder')).toHaveTextContent('D:\\code\\payments-api'),
    );
    await userEvent.click(screen.getByRole('button', { name: /Open shell/ }));
    await waitFor(() => expect(transport.bodies('open_shell')).toHaveLength(1));
    expect(transport.bodies('open_shell')[0]).toMatchObject({
      request: { shell: 'powershell', project_dir: 'D:\\code\\payments-api' },
    });
  });
});

describe('shell folder helpers', () => {
  const sessions = { cond: conductorSession, a1: api, w1: web };
  const excluded = conductorFolders(sessions, 'cond');

  it('excludes the conductor’s folder', () => {
    expect(excluded).toEqual([CONDUCTOR_DIR]);
    expect(defaultShellFolder([CONDUCTOR_DIR], sessions, excluded)).toBe('/home/dev/payments-api');
    expect(defaultShellFolder([], { cond: conductorSession }, excluded)).toBeNull();
  });

  it('prefers the server’s recent list, then agent sessions over shells', () => {
    expect(defaultShellFolder(['/home/dev/web'], sessions, excluded)).toBe('/home/dev/web');
    const newerShell = shellIn('/home/dev/other', { last_activity: Date.now() + 1 });
    expect(defaultShellFolder([], { ...sessions, sh1: newerShell }, excluded)).toBe(
      '/home/dev/payments-api',
    );
  });

  it('picks PowerShell only on Windows', () => {
    expect(commandShellName('windows')).toBe('powershell');
    expect(commandShellName('unix')).toBe('default');
    expect(commandShellName(null)).toBe('default');
  });
});
