import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { NativeError } from '../native/bridge';
import { NativeProvider, type NativeServices } from '../native/context';
import type { SecurityStatus } from '../native/security';
import { fakeSecurity, securityStatus } from '../test/fakeSecurity';
import { renderWithApp } from '../test/render';
import { SecurityGate, SecurityWizard, resetSecurityBanner } from './SecurityWizard';
import { peekSettingsTarget } from './settingsNav';

const HELLO = 'Verify with Windows Hello';
const NO_TOTP: SecurityStatus['totp'] = {
  enrolled: false,
  enrolled_at: null,
  backup_codes_remaining: 0,
  backup_codes_total: 0,
};
const PENDING = securityStatus({
  totp: NO_TOTP,
  wizard: { state: 'pending', updated_at: null },
  setup_complete: false,
});

function native(security?: NativeServices['security']): NativeServices {
  return {
    pairingCode: async () => 'ABCD-EFGH',
    pair: async () => {
      throw new Error('unused');
    },
    forget: async () => {},
    platform: async () => 'windows',
    ...(security ? { security } : {}),
  };
}

function renderGate(
  security: NativeServices['security'],
  onOpenSettings = () => {},
  subscribe: (cb: () => void) => () => void = () => () => {},
) {
  return renderWithApp(
    <NativeProvider value={native(security)}>
      <SecurityGate onOpenSettings={onOpenSettings} subscribe={subscribe} />
    </NativeProvider>,
  );
}

beforeEach(() => resetSecurityBanner());

describe('SecurityWizard', () => {
  it('walks through the four steps and finishes', async () => {
    const { services } = fakeSecurity(PENDING);
    const onClose = vi.fn();
    renderWithApp(<SecurityWizard services={services} onClose={onClose} />);
    const user = userEvent.setup();

    // 1: local verification must work before moving on.
    expect(await screen.findByText('Step 1 of 4 · Check this PC')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Next' })).toBeDisabled();
    await user.click(screen.getByRole('button', { name: HELLO }));
    const dialogs = await screen.findAllByRole('button', { name: HELLO });
    await user.click(dialogs[dialogs.length - 1]!);
    expect(await screen.findByText('It works.')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Next' }));

    // 2: authenticator, and the backup codes must be confirmed.
    // The check in step 1 approved nothing: this asks for itself.
    await user.click(await screen.findByRole('button', { name: 'Set up authenticator' }));
    const prompts = await screen.findAllByRole('button', { name: HELLO });
    await user.click(prompts[prompts.length - 1]!);
    expect(await screen.findByTestId('totp-secret')).toHaveTextContent('NEWSECRET');
    expect(services.totpBegin).toHaveBeenCalledWith(false);
    expect(screen.queryByRole('button', { name: 'Next' })).toBeNull();
    await user.type(screen.getByLabelText('6-digit code from the new entry'), '123456');
    await user.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(await screen.findByTestId('backup-codes')).toHaveTextContent('AAAA-1111');
    const go = screen.getByRole('button', { name: 'Continue' });
    expect(go).toBeDisabled();
    await user.click(screen.getByLabelText('I saved them'));
    await user.click(go);

    // 3: mail is optional.
    expect(await screen.findByText('Step 3 of 4 · Mail (optional)')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Skip' }));

    // 4: summary.
    expect(await screen.findByText('Step 4 of 4 · Summary')).toBeInTheDocument();
    expect(screen.getByTestId('security-status')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Finish' }));
    await waitFor(() => expect(services.wizard).toHaveBeenCalledWith('done'));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('explains the authenticator fallback without an OS verifier', async () => {
    const { services } = fakeSecurity({
      ...PENDING,
      verifier: { kind: 'none', available: false, detail: null },
    });
    renderWithApp(<SecurityWizard services={services} onClose={() => {}} />);
    expect(await screen.findByText(/authenticator code will be used/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Next' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: HELLO })).toBeNull();
  });

  it('does not ask for an authenticator that is already set up', async () => {
    const { services } = fakeSecurity({
      ...PENDING,
      totp: { ...NO_TOTP, enrolled: true },
      verifier: { kind: 'none', available: false, detail: null },
    });
    renderWithApp(<SecurityWizard services={services} onClose={() => {}} />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Next' }));
    expect(await screen.findByText('2-step verification is already on.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Next' })).toBeEnabled();
  });

  it('can be skipped with Later', async () => {
    const { services } = fakeSecurity(PENDING);
    const onClose = vi.fn();
    renderWithApp(<SecurityWizard services={services} onClose={onClose} />);
    await userEvent.click(await screen.findByRole('button', { name: 'Later' }));
    await waitFor(() => expect(services.wizard).toHaveBeenCalledWith('skipped'));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('stays open and reports when saving the choice fails', async () => {
    const { services } = fakeSecurity(PENDING);
    services.wizard.mockRejectedValueOnce(new NativeError('unreachable', 'off'));
    const onClose = vi.fn();
    renderWithApp(<SecurityWizard services={services} onClose={onClose} />);
    await userEvent.click(await screen.findByRole('button', { name: 'Later' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('is not running');
    expect(onClose).not.toHaveBeenCalled();
  });
});

describe('SecurityGate', () => {
  it('shows nothing without the desktop services', () => {
    renderGate(undefined);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('shows the wizard while setup is pending', async () => {
    const { services } = fakeSecurity(PENDING);
    renderGate(services);
    expect(await screen.findByTestId('security-wizard')).toBeInTheDocument();
  });

  it('shows nothing once setup is complete, even if pending', async () => {
    const { services } = fakeSecurity(
      securityStatus({ wizard: { state: 'pending', updated_at: null } }),
    );
    renderGate(services);
    await waitFor(() => expect(services.status).toHaveBeenCalled());
    await Promise.resolve();
    expect(screen.queryByTestId('security-wizard')).toBeNull();
    expect(screen.queryByTestId('security-banner')).toBeNull();
  });

  it('waits for the server, then shows the wizard when it comes up', async () => {
    const { services } = fakeSecurity(PENDING);
    services.status.mockRejectedValueOnce(new NativeError('unreachable', 'off'));
    let notify = () => {};
    renderGate(services, undefined, (cb) => {
      notify = cb;
      return () => {};
    });
    await waitFor(() => expect(services.status).toHaveBeenCalledTimes(1));
    expect(screen.queryByTestId('security-wizard')).toBeNull();
    notify();
    expect(await screen.findByTestId('security-wizard')).toBeInTheDocument();
  });

  it('does not nag right after Later', async () => {
    const { services } = fakeSecurity(PENDING);
    renderGate(services);
    await userEvent.click(await screen.findByRole('button', { name: 'Later' }));
    await waitFor(() => expect(screen.queryByTestId('security-wizard')).toBeNull());
    expect(screen.queryByTestId('security-banner')).toBeNull();
  });

  it('reminds once per launch after a skip, and links to Settings', async () => {
    const skipped = { ...PENDING, wizard: { state: 'skipped' as const, updated_at: 1 } };
    const first = fakeSecurity(skipped);
    const { unmount } = renderGate(first.services);
    await userEvent.click(
      (await screen.findByTestId('security-banner')).querySelector('[aria-label="Dismiss"]')!,
    );
    expect(screen.queryByTestId('security-banner')).toBeNull();
    unmount();

    // Same launch: stays away.
    const second = fakeSecurity(skipped);
    const again = renderGate(second.services);
    await waitFor(() => expect(second.services.status).toHaveBeenCalled());
    await Promise.resolve();
    expect(screen.queryByTestId('security-banner')).toBeNull();
    again.unmount();

    // New launch: back, and its button opens Settings at Security.
    resetSecurityBanner();
    const third = fakeSecurity(skipped);
    const onOpenSettings = vi.fn();
    renderGate(third.services, onOpenSettings);
    await screen.findByTestId('security-banner');
    await userEvent.click(screen.getByRole('button', { name: 'Set up' }));
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
    expect(peekSettingsTarget()).toBe('set-sec-status');
    expect(screen.queryByTestId('security-banner')).toBeNull();
  });
});
