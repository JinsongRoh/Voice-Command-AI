import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it } from 'vitest';

import { NativeError } from '../native/bridge';
import { NativeProvider, type NativeServices } from '../native/context';
import { fakeSecurity, mailView, securityStatus } from '../test/fakeSecurity';
import { renderWithApp } from '../test/render';
import { MailSettings, SecurityOverview, SecurityRecovery } from './SecuritySettings';
import { SettingsView } from './SettingsView';

const HELLO = 'Verify with Windows Hello';

beforeEach(() => localStorage.clear());

describe('SecurityOverview', () => {
  it('shows each protection with a badge', async () => {
    const { services } = fakeSecurity(
      securityStatus({
        verifier: { kind: 'none', available: false, detail: null },
        totp: {
          enrolled: false,
          enrolled_at: null,
          backup_codes_remaining: 0,
          backup_codes_total: 0,
        },
        remote_shell: { enabled: true },
      }),
    );
    renderWithApp(<SecurityOverview services={services} />);
    const verifier = await screen.findByTestId('security-item-verifier');
    expect(verifier).toHaveTextContent('the authenticator code is used');
    expect(verifier).toHaveTextContent('Needs attention');
    expect(screen.getByTestId('security-item-totp')).toHaveTextContent('Not set up');
    expect(screen.getByTestId('security-item-mail')).toHaveTextContent('Needs attention');
    expect(screen.getByTestId('security-item-remote-shell')).toHaveTextContent('Needs attention');
    expect(screen.getByTestId('security-item-grace')).toHaveTextContent('2 min');
  });

  it('shows a healthy setup with backup codes left', async () => {
    const { services } = fakeSecurity(
      securityStatus({
        mail: {
          configured: true,
          mode: 'smtp',
          recipients: ['owner@example.com'],
          notifications: true,
        },
      }),
    );
    renderWithApp(<SecurityOverview services={services} />);
    const totp = await screen.findByTestId('security-item-totp');
    expect(totp).toHaveTextContent('On, 8 of 10 backup codes left');
    expect(totp).toHaveTextContent('OK');
    expect(screen.getByTestId('security-item-verifier')).toHaveTextContent('Windows Hello');
    expect(screen.getByTestId('security-item-mail')).toHaveTextContent('owner@example.com');
  });

  it('reports a stopped server', async () => {
    const { services } = fakeSecurity();
    services.status.mockRejectedValue(new NativeError('unreachable', 'off'));
    renderWithApp(<SecurityOverview services={services} />);
    expect(await screen.findByRole('alert')).toHaveTextContent('is not running');
  });

  it('changes the grace window through step-up', async () => {
    const { services } = fakeSecurity();
    renderWithApp(<SecurityOverview services={services} />);
    const user = userEvent.setup();
    await user.selectOptions(await screen.findByLabelText('Stay verified for'), '300');
    await user.click(await screen.findByRole('button', { name: HELLO }));
    await waitFor(() => expect(services.settings).toHaveBeenCalledTimes(2));
    expect(services.settings).toHaveBeenLastCalledWith({ grace_secs: 300 });
    expect(await screen.findByTestId('security-item-grace')).toHaveTextContent('5 min');
  });

  it('turns the remote shell on through step-up and off without it', async () => {
    const { services } = fakeSecurity();
    renderWithApp(<SecurityOverview services={services} />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Turn on' }));
    expect(await screen.findByRole('dialog')).toHaveTextContent('Turn on the remote shell');
    await user.click(await screen.findByRole('button', { name: HELLO }));
    await screen.findByRole('button', { name: 'Turn off' });
    expect(services.stepUp).toHaveBeenCalledTimes(1);

    await user.click(screen.getByRole('button', { name: 'Turn off' }));
    await screen.findByRole('button', { name: 'Turn on' });
    expect(services.stepUp).toHaveBeenCalledTimes(1);
  });

  it('replaces the authenticator, then shows the backup codes once', async () => {
    const { services } = fakeSecurity();
    renderWithApp(<SecurityOverview services={services} />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Replace authenticator' }));
    await user.click(await screen.findByRole('button', { name: HELLO }));
    expect(await screen.findByTestId('totp-secret')).toHaveTextContent('NEWSECRET');
    expect(services.totpBegin).toHaveBeenLastCalledWith(true);

    await user.type(screen.getByLabelText('Code from your current authenticator app'), '111111');
    await user.type(screen.getByLabelText('6-digit code from the new entry'), '222222');
    await user.click(screen.getByRole('button', { name: 'Confirm' }));

    expect(await screen.findByTestId('backup-codes')).toHaveTextContent('AAAA-1111');
    expect(services.totpConfirm).toHaveBeenCalledWith('222222', '111111');
    const go = screen.getByRole('button', { name: 'Continue' });
    expect(go).toBeDisabled();
    await user.click(screen.getByLabelText('I saved them'));
    await user.click(go);
    expect(screen.queryByTestId('backup-codes')).toBeNull();
  });

  it('warns before making new backup codes', async () => {
    const { services } = fakeSecurity();
    renderWithApp(<SecurityOverview services={services} />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Make new backup codes' }));
    expect(screen.getByRole('alert')).toHaveTextContent('will stop working');
    expect(services.backupCodes).not.toHaveBeenCalled();
    // The approval alone is not enough: the authenticator has to agree.
    expect(screen.getByRole('button', { name: 'Make new codes' })).toBeDisabled();
    await user.type(screen.getByLabelText('Code from your current authenticator app'), '123456');
    await user.click(screen.getByRole('button', { name: 'Make new codes' }));
    await user.click(await screen.findByRole('button', { name: HELLO }));
    expect(await screen.findByTestId('backup-codes')).toHaveTextContent('CCCC-3333');
    expect(services.backupCodes).toHaveBeenLastCalledWith('123456');
  });
});

describe('MailSettings', () => {
  it('saves through step-up, retries, and sends the app language', async () => {
    const { services } = fakeSecurity();
    renderWithApp(<MailSettings services={services} />);
    const user = userEvent.setup();
    const to = await screen.findByLabelText('Send to');
    await user.clear(to);
    await user.type(to, 'a@example.com, b@example.com');
    await user.click(screen.getByRole('button', { name: 'Save' }));

    await user.click(await screen.findByRole('button', { name: HELLO }));
    expect(await screen.findByRole('status')).toHaveTextContent('Saved. Mail is ready.');
    expect(services.mailSet).toHaveBeenCalledTimes(2);
    expect(services.mailSet).toHaveBeenLastCalledWith(
      expect.objectContaining({
        recipients: ['a@example.com', 'b@example.com'],
        language: 'en',
        smtp_port: 587,
      }),
      undefined,
    );
  });

  it('asks for the authenticator when the mail moves somewhere else', async () => {
    const { services } = fakeSecurity();
    services.stepUp.mockClear();
    renderWithApp(<MailSettings services={services} />);
    const user = userEvent.setup();
    const to = await screen.findByLabelText('Send to');
    await user.clear(to);
    await user.type(to, 'new@example.com');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await user.click(await screen.findByRole('button', { name: HELLO }));
    await waitFor(() => expect(services.mailSet).toHaveBeenCalledTimes(2));

    services.mailSet.mockRejectedValueOnce(new NativeError('current_code_required', 'code'));
    await user.click(screen.getByRole('button', { name: 'Save' }));
    const code = await screen.findByLabelText(/Changing how mail is sent/);
    await user.type(code, '123456');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(services.mailSet).toHaveBeenLastCalledWith(expect.anything(), '123456'),
    );
    await waitFor(() => expect(screen.queryByLabelText(/Changing how mail is sent/)).toBeNull());
  });

  it('never shows a stored secret and clears a typed one after saving', async () => {
    const { services } = fakeSecurity(securityStatus(), mailView({ password_set: true }));
    const { container } = renderWithApp(<MailSettings services={services} />);
    const user = userEvent.setup();
    const password = await screen.findByLabelText('Password');
    expect(password).toHaveAttribute('type', 'password');
    expect(password).toHaveValue('');
    expect(screen.getByText('Saved')).toBeInTheDocument();

    await user.type(password, 'typed-secret');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await user.click(await screen.findByRole('button', { name: HELLO }));
    await waitFor(() =>
      expect(services.mailSecret).toHaveBeenCalledWith('smtp_password', 'typed-secret', undefined),
    );
    await waitFor(() => expect(screen.getByLabelText('Password')).toHaveValue(''));
    expect(container.innerHTML).not.toContain('typed-secret');
    expect(JSON.stringify(services.mailSet.mock.calls)).not.toContain('typed-secret');
  });

  it('leaves a saved secret alone when the field stays empty', async () => {
    const { services } = fakeSecurity(securityStatus(), mailView({ password_set: true }));
    renderWithApp(<MailSettings services={services} />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Save' }));
    await user.click(await screen.findByRole('button', { name: HELLO }));
    await screen.findByText('Saved. Mail is ready.');
    expect(services.mailSecret).not.toHaveBeenCalled();
  });

  it('shows preset fields read-only until Change', async () => {
    const { services } = fakeSecurity(securityStatus(), mailView({ preset: ['smtp_host'] }));
    renderWithApp(<MailSettings services={services} />);
    const user = userEvent.setup();
    const host = await screen.findByLabelText('SMTP server');
    expect(host).toHaveAttribute('readonly');
    expect(screen.getByText('Preset')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Change SMTP server' }));
    expect(host).not.toHaveAttribute('readonly');
  });

  it('shows the relay fields, with no sender address', async () => {
    const { services } = fakeSecurity(
      securityStatus(),
      mailView({ mode: 'relay', relay_url: 'https://mail.example.com/send', relay_key_set: true }),
    );
    renderWithApp(<MailSettings services={services} />);
    expect(await screen.findByLabelText('Relay URL')).toHaveValue('https://mail.example.com/send');
    expect(screen.getByLabelText('Relay key')).toHaveAttribute('type', 'password');
    expect(screen.getByLabelText('Gateway token')).toHaveAttribute('type', 'password');
    expect(screen.queryByLabelText('Sender address')).toBeNull();
    expect(screen.queryByLabelText('SMTP server')).toBeNull();
  });

  it('names the allowed domains when a recipient is refused', async () => {
    const { services } = fakeSecurity(
      securityStatus(),
      mailView({ allowed_recipient_domains: ['example.com'] }),
    );
    services.mailSet.mockRejectedValueOnce(new NativeError('recipient_not_allowed', 'no'));
    renderWithApp(<MailSettings services={services} />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Allowed domains: example.com');
  });

  it('reports the test mail result', async () => {
    const { services } = fakeSecurity();
    services.mailTest
      .mockResolvedValueOnce({ sent_to: ['owner@example.com'] })
      .mockRejectedValueOnce(new NativeError('mail_failed', 'connection refused'))
      .mockRejectedValueOnce(new NativeError('recipient_not_allowed', 'invalid_recipient'))
      .mockRejectedValueOnce(new NativeError('rate_limited', 'slow down'));
    renderWithApp(<MailSettings services={services} />);
    const user = userEvent.setup();
    const send = await screen.findByRole('button', { name: 'Send test mail' });

    await user.click(send);
    const result = () => screen.getByTestId('mail-test-result');
    await waitFor(() => expect(result()).toHaveTextContent('Test mail sent to owner@example.com'));
    await user.click(send);
    await waitFor(() => expect(result()).toHaveTextContent('connection refused'));
    await user.click(send);
    await waitFor(() => expect(result()).toHaveTextContent('This address is not allowed'));
    await user.click(send);
    await waitFor(() => expect(result()).toHaveTextContent('Too many tries'));
  });
});

describe('SecurityRecovery', () => {
  it('uses a mailed code, then goes straight into a new authenticator', async () => {
    const { services } = fakeSecurity(securityStatus({ recovery: 'mail_and_local' }));
    renderWithApp(<SecurityRecovery services={services} />);
    const user = userEvent.setup();
    expect(await screen.findByText(/code we mail you/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'I lost my authenticator app' }));
    await user.click(await screen.findByRole('button', { name: HELLO }));

    expect(await screen.findByText(/o\*\*\*@example\.com/)).toBeInTheDocument();
    await user.type(screen.getByLabelText('6-digit code from the mail'), '123456');
    await user.click(screen.getByRole('button', { name: 'Verify' }));

    expect(await screen.findByTestId('totp-secret')).toBeInTheDocument();
    expect(services.recoveryFinish).toHaveBeenLastCalledWith({ code: '123456' });
    expect(services.totpBegin).toHaveBeenLastCalledWith(true);
    expect(screen.queryByLabelText('Code from your current authenticator app')).toBeNull();
    await user.type(screen.getByLabelText('6-digit code from the new entry'), '222222');
    await user.click(screen.getByRole('button', { name: 'Confirm' }));
    await screen.findByTestId('backup-codes');
    expect(services.totpConfirm).toHaveBeenCalledWith('222222', undefined);
  });

  it('uses a backup code without mail', async () => {
    const { services } = fakeSecurity();
    renderWithApp(<SecurityRecovery services={services} />);
    const user = userEvent.setup();
    expect(await screen.findByText(/one of your backup codes/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'I lost my authenticator app' }));
    await user.type(screen.getByLabelText('Backup code'), 'AAAA-1111');
    await user.click(screen.getByRole('button', { name: 'Verify' }));
    await user.click(await screen.findByRole('button', { name: HELLO }));
    await waitFor(() =>
      expect(services.recoveryFinish).toHaveBeenLastCalledWith({ backup_code: 'AAAA-1111' }),
    );
    expect(services.recoveryStart).not.toHaveBeenCalled();
  });

  it('falls back to a backup code when mail stopped working', async () => {
    const { services, state } = fakeSecurity(securityStatus({ recovery: 'mail_and_local' }));
    state.verified = true;
    services.recoveryStart.mockRejectedValueOnce(new NativeError('mail_not_configured', 'no'));
    renderWithApp(<SecurityRecovery services={services} />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: 'I lost my authenticator app' }));
    expect(await screen.findByLabelText('Backup code')).toBeInTheDocument();
    expect(screen.getByRole('alert')).toHaveTextContent('Use a backup code instead');
  });
});

describe('Settings Security tab', () => {
  const native = (extra: Partial<NativeServices>): NativeServices => ({
    pairingCode: async () => 'ABCD-EFGH',
    pair: async () => {
      throw new Error('unused');
    },
    forget: async () => {},
    platform: async () => 'windows',
    ...extra,
  });

  it('shows the sections in the desktop app', async () => {
    const { services } = fakeSecurity();
    renderWithApp(
      <NativeProvider value={native({ security: services })}>
        <SettingsView onPair={() => {}} />
      </NativeProvider>,
    );
    await userEvent.click(screen.getByRole('tab', { name: 'Security' }));
    const panel = screen.getByRole('tabpanel');
    expect(await within(panel).findByTestId('security-status')).toBeInTheDocument();
    expect(within(panel).getByRole('heading', { name: 'Mail' })).toBeInTheDocument();
    expect(within(panel).getByRole('heading', { name: 'Lost authenticator' })).toBeInTheDocument();
  });

  it('points elsewhere when this is not the PC app', async () => {
    renderWithApp(<SettingsView onPair={() => {}} />);
    await userEvent.click(screen.getByRole('tab', { name: 'Security' }));
    const panel = screen.getByRole('tabpanel');
    expect(panel).toHaveTextContent('Security settings are on the PC');
    expect(within(panel).queryByRole('heading', { name: 'Mail' })).toBeNull();
  });
});
