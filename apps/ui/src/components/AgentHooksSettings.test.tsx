import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import type {
  AgentHooks,
  AgentHooksServices,
  AgentHooksStatus,
  HookAgent,
} from '../native/agentHooks';
import { NativeError } from '../native/bridge';
import { NativeProvider, type NativeServices } from '../native/context';
import type { HostServices } from '../native/host';
import { renderWithApp } from '../test/render';
import { AgentHooksHint, AgentHooksSettings } from './AgentHooksSettings';
import { SettingsView } from './SettingsView';

const CLI = 'C:\\Users\\me\\AppData\\Local\\Voice Command AI\\vca.exe';
const STOPGAP = 'C:\\Users\\me\\AppData\\Local\\Programs\\vca\\vca.exe';

function agent(which: HookAgent, extra: Partial<AgentHooks> = {}): AgentHooks {
  return {
    agent: which,
    state: 'missing',
    config_path:
      which === 'claude'
        ? 'C:\\Users\\me\\.claude\\settings.json'
        : 'C:\\Users\\me\\.codex\\config.toml',
    programs: [],
    backup_path: null,
    error: null,
    ...extra,
  };
}

function fakeServices(initial: AgentHooksStatus) {
  let status = initial;
  const set = (a: AgentHooks) => {
    status = { ...status, agents: status.agents.map((x) => (x.agent === a.agent ? a : x)) };
    return a;
  };
  return {
    status: vi.fn(async () => status),
    install: vi.fn(async (which: HookAgent) =>
      set(
        agent(which, {
          state: 'installed',
          programs: [CLI],
          backup_path: `C:\\Users\\me\\.${which}\\backup.vca-backup-1`,
        }),
      ),
    ),
    uninstall: vi.fn(async (which: HookAgent) =>
      set(agent(which, { backup_path: `C:\\Users\\me\\.${which}\\backup.vca-backup-2` })),
    ),
  } satisfies AgentHooksServices;
}

const MISSING: AgentHooksStatus = {
  cli_path: CLI,
  agents: [agent('claude'), agent('codex')],
};

describe('AgentHooksSettings', () => {
  it('shows each agent and installs nothing until confirmed', async () => {
    const services = fakeServices(MISSING);
    renderWithApp(<AgentHooksSettings services={services} />);
    const claude = await screen.findByTestId('agent-hooks-claude');
    expect(claude).toHaveTextContent('Claude Code');
    expect(claude).toHaveTextContent('Not installed');
    expect(claude).toHaveTextContent('.claude\\settings.json');
    expect(screen.getByTestId('agent-hooks-codex')).toHaveTextContent('Not installed');
    expect(screen.getByText(/--scope project/)).toBeInTheDocument();

    await userEvent.click(within(claude).getByRole('button', { name: 'Install' }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('Install Claude Code hooks?');
    const explain = within(dialog).getByTestId('agent-hooks-confirm');
    expect(explain).toHaveTextContent('approval prompts and turn ends reach VCA instantly');
    expect(explain).toHaveTextContent('existing hooks and settings are kept');
    expect(explain).toHaveTextContent('backup of the file');
    expect(explain).toHaveTextContent('fail open');
    expect(explain).toHaveTextContent('may need a restart');

    // Cancel: nothing installed.
    await userEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(services.install).not.toHaveBeenCalled();

    await userEvent.click(within(claude).getByRole('button', { name: 'Install' }));
    await userEvent.click(
      within(await screen.findByRole('dialog')).getByRole('button', { name: 'Install' }),
    );
    await waitFor(() => expect(services.install).toHaveBeenCalledWith('claude'));
    expect(await screen.findByRole('status')).toHaveTextContent(
      'Claude Code hooks are installed. Backup: C:\\Users\\me\\.claude\\backup.vca-backup-1',
    );
    expect(screen.getByTestId('agent-hooks-claude')).toHaveTextContent('Installed');
    expect(services.install).toHaveBeenCalledTimes(1);
  });

  it('explains the Codex notify chain', async () => {
    const services = fakeServices(MISSING);
    renderWithApp(<AgentHooksSettings services={services} />);
    const codex = await screen.findByTestId('agent-hooks-codex');
    await userEvent.click(within(codex).getByRole('button', { name: 'Install' }));
    expect(await screen.findByTestId('agent-hooks-confirm')).toHaveTextContent('VCA chains it');
  });

  it('offers Repair for hooks that run another vca', async () => {
    const services = fakeServices({
      cli_path: CLI,
      agents: [
        agent('claude', { state: 'other_path', programs: [STOPGAP] }),
        agent('codex', { state: 'outdated', programs: [CLI] }),
      ],
    });
    renderWithApp(<AgentHooksSettings services={services} />);
    const claude = await screen.findByTestId('agent-hooks-claude');
    expect(claude).toHaveTextContent('Points to another vca');
    expect(claude).toHaveTextContent(`The hooks run ${STOPGAP}`);
    const codex = screen.getByTestId('agent-hooks-codex');
    expect(codex).toHaveTextContent('Outdated');
    expect(codex).toHaveTextContent('Some hooks are missing or changed');

    await userEvent.click(within(claude).getByRole('button', { name: 'Repair' }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('Repair Claude Code hooks?');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Repair' }));
    await waitFor(() => expect(services.install).toHaveBeenCalledWith('claude'));
    expect(await screen.findByTestId('agent-hooks-claude')).toHaveTextContent('Installed');
  });

  it('uninstalls after confirmation and shows the backup', async () => {
    const services = fakeServices({
      cli_path: CLI,
      agents: [agent('claude'), agent('codex', { state: 'installed', programs: [CLI] })],
    });
    renderWithApp(<AgentHooksSettings services={services} />);
    const codex = await screen.findByTestId('agent-hooks-codex');
    await userEvent.click(within(codex).getByRole('button', { name: 'Uninstall' }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('Remove Codex hooks?');
    expect(dialog).toHaveTextContent('notify program VCA chained is restored');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Uninstall' }));
    await waitFor(() => expect(services.uninstall).toHaveBeenCalledWith('codex'));
    expect(await screen.findByRole('status')).toHaveTextContent(
      'Codex hooks are removed. Backup: C:\\Users\\me\\.codex\\backup.vca-backup-2',
    );
  });

  it('cannot install without the bundled CLI', async () => {
    const services = fakeServices({ ...MISSING, cli_path: null });
    renderWithApp(<AgentHooksSettings services={services} />);
    expect(await screen.findByTestId('agent-hooks-not-bundled')).toBeInTheDocument();
    for (const button of screen.getAllByRole('button', { name: 'Install' })) {
      expect(button).toBeDisabled();
    }
  });

  it('shows the PC hint when the shell refuses (forbidden)', async () => {
    const services = fakeServices(MISSING);
    services.status.mockRejectedValueOnce(
      new NativeError('forbidden', 'agent hooks can only be installed in the PC app'),
    );
    renderWithApp(<AgentHooksSettings services={services} />);
    expect(await screen.findByTestId('agent-hooks-hint')).toHaveTextContent(
      "Exact approval alerts are turned on in the PC app's settings.",
    );
    expect(screen.queryByRole('button', { name: 'Install' })).not.toBeInTheDocument();
  });

  it('reports other errors', async () => {
    const services = fakeServices(MISSING);
    services.install.mockRejectedValueOnce(new NativeError('hooks', 'disk full'));
    renderWithApp(<AgentHooksSettings services={services} />);
    const claude = await screen.findByTestId('agent-hooks-claude');
    await userEvent.click(within(claude).getByRole('button', { name: 'Install' }));
    await userEvent.click(
      within(await screen.findByRole('dialog')).getByRole('button', { name: 'Install' }),
    );
    expect(await screen.findByRole('alert')).toHaveTextContent('disk full');
  });
});

describe('agent hooks in Settings', () => {
  it('phones and browsers only get the hint', async () => {
    renderWithApp(<AgentHooksHint />);
    expect(screen.getByTestId('agent-hooks-hint')).toBeInTheDocument();
    renderWithApp(<SettingsView onPair={() => {}} />);
    await userEvent.click(screen.getByRole('tab', { name: 'Alerts' }));
    expect(
      await screen.findByRole('heading', { name: 'Agent connection (exact approval alerts)' }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId('agent-hooks')).not.toBeInTheDocument();
    expect(screen.getAllByTestId('agent-hooks-hint').length).toBeGreaterThan(0);
  });

  it('the desktop app shows the controls', async () => {
    const services = fakeServices(MISSING);
    const native = {
      pair: vi.fn(),
      forget: vi.fn(),
      platform: async () => 'windows',
      host: { agentHooks: services } as unknown as HostServices,
    } as unknown as NativeServices;
    renderWithApp(
      <NativeProvider value={native}>
        <SettingsView onPair={() => {}} />
      </NativeProvider>,
    );
    await userEvent.click(screen.getByRole('tab', { name: 'Alerts' }));
    expect(await screen.findByTestId('agent-hooks-claude')).toBeInTheDocument();
    expect(services.status).toHaveBeenCalled();
    expect(services.install).not.toHaveBeenCalled();
    expect(screen.queryByTestId('agent-hooks-hint')).not.toBeInTheDocument();
  });
});
