import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import type { AgentHooksServices } from '../native/agentHooks';
import { NativeError } from '../native/bridge';
import { renderWithApp } from '../test/render';
import { localSession } from '../test/fixtures';
import { HooksMissingBanner } from './HooksMissingBanner';

const missing = localSession({ problem: 'hooks_missing' });

function services(install: AgentHooksServices['install']): AgentHooksServices {
  return {
    status: vi.fn(),
    install: vi.fn(install),
    uninstall: vi.fn(),
  } as unknown as AgentHooksServices;
}

describe('HooksMissingBanner', () => {
  it('shows nothing while the hooks work', () => {
    renderWithApp(<HooksMissingBanner session={localSession()} services={null} />);
    expect(screen.queryByTestId('hooks-missing')).toBeNull();
  });

  it('points phones and other devices to the PC app', () => {
    renderWithApp(<HooksMissingBanner session={missing} services={null} />);
    const banner = screen.getByTestId('hooks-missing');
    expect(banner).toHaveTextContent('The Claude connection is missing');
    expect(banner).toHaveTextContent('Settings › Alerts › Agent connection');
    expect(screen.queryByTestId('hooks-repair')).toBeNull();
  });

  it('repairs only when Repair is pressed', async () => {
    const hooks = services(async () => ({
      agent: 'claude',
      state: 'installed',
      config_path: 'settings.json',
      programs: [],
      backup_path: null,
      error: null,
    }));
    renderWithApp(<HooksMissingBanner session={missing} services={hooks} />);
    expect(hooks.install).not.toHaveBeenCalled();
    await userEvent.click(screen.getByTestId('hooks-repair'));
    expect(hooks.install).toHaveBeenCalledWith('claude');
    expect(await screen.findByText(/Repaired\./)).toBeInTheDocument();
  });

  it('says why a repair failed', async () => {
    const hooks = services(async () => {
      throw new NativeError('hooks', 'settings.json is not valid JSON');
    });
    renderWithApp(<HooksMissingBanner session={missing} services={hooks} />);
    await userEvent.click(screen.getByTestId('hooks-repair'));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Repair failed: settings.json is not valid JSON',
    );
  });

  it('can be closed', async () => {
    renderWithApp(<HooksMissingBanner session={missing} services={null} />);
    await userEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(screen.queryByTestId('hooks-missing')).toBeNull();
  });
});
