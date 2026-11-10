import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import type { DnsPilotService } from '../native/bridge';
import { hostBridge, type HostPairing, type HostStatus, type RemoteInfo } from '../native/host';
import { tauri } from '../test/tauriMock';
import { ConnectPhoneView } from './ConnectPhoneView';

vi.mock('@tauri-apps/api/core', () => import('../test/tauriMock'));

const T0 = Date.UTC(2026, 9, 30, 12, 0, 0);
const ONE_QR = /This one QR code registers the phone for remote access and connects it/;

function remote(over: Partial<RemoteInfo> = {}): RemoteInfo {
  return {
    enabled: true,
    listen: '0.0.0.0:47800',
    provider: 'lan',
    provider_state: 'ready',
    provider_diagnostics: [],
    totp_enrolled: true,
    require_totp: true,
    kill_switch_tripped: false,
    paired_devices: 0,
    connections: 0,
    ...over,
  };
}

function status(over: Partial<HostStatus> = {}): HostStatus {
  return {
    running: true,
    endpoint: '\\\\.\\pipe\\vca-control-me',
    server_name: 'studio-pc',
    version: '0.1.0',
    remote: remote(),
    managed: true,
    can_start: true,
    run_server: true,
    log_path: null,
    ...over,
  };
}

function pairing(n: number, over: Partial<HostPairing> = {}): HostPairing {
  return {
    invite_included: false,
    invite_skipped: null,
    uri: `vca://pair?v=1&s=secret${n}`,
    expires_at: Math.floor(nowMs / 1000) + 60,
    endpoints: ['ws://192.168.0.10:47800/v1/ws'],
    loopback_only: false,
    qr_svg: `<svg xmlns="http://www.w3.org/2000/svg"><path d="M${n} 0h1"/></svg>`,
    scope: 'command',
    server_fingerprint: 'ff',
    confirm_code: 'ABCD-EFGH-JKMN-PQRS',
    ...over,
  };
}

let nowMs = T0;
let current: HostStatus;
let pairCount = 0;

function renderView(pollMs = 1_000_000) {
  return render(
    <I18nProvider locale="en">
      <ConnectPhoneView host={hostBridge} clock={() => nowMs} pollMs={pollMs} />
    </I18nProvider>,
  );
}

/** A code is only made on request ("Create QR code"). */
async function createQr() {
  fireEvent.click(await screen.findByRole('button', { name: 'Create QR code' }));
}

function tick(ms: number) {
  act(() => {
    nowMs += ms;
    vi.advanceTimersByTime(ms);
  });
}

describe('ConnectPhoneView', () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
    tauri.reset();
    nowMs = T0;
    pairCount = 0;
    current = status();
    tauri.handlers.set('vca_host_status', () => current);
    tauri.handlers.set('vca_host_pair', (args) => {
      pairCount += 1;
      const { scope } = args as { scope: HostPairing['scope'] };
      return pairing(pairCount, { scope });
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('shows the server status and a QR code for the command scope', async () => {
    renderView();
    expect(await screen.findByText('Running · v0.1.0')).toBeInTheDocument();
    expect(screen.getByText('studio-pc')).toBeInTheDocument();
    // Nothing is created (or verified) until the person asks for a code.
    expect(screen.queryByRole('img', { name: 'Pairing QR code' })).not.toBeInTheDocument();
    expect(tauri.callsOf('vca_host_pair')).toHaveLength(0);
    await createQr();
    const qr = await screen.findByRole('img', { name: 'Pairing QR code' });
    expect(qr.getAttribute('src')).toMatch(/^data:image\/svg\+xml;charset=utf-8,%3Csvg/);
    expect(tauri.callsOf('vca_host_pair')[0]?.args).toEqual({ scope: 'command' });
    expect(screen.getByRole('button', { name: 'Command' })).toHaveAttribute('aria-pressed', 'true');
    // How phones reach this PC.
    expect(screen.getByText('lan (ready)')).toBeInTheDocument();
    expect(screen.getByText('ws://192.168.0.10:47800/v1/ws')).toBeInTheDocument();
    expect(screen.queryByText(/Phones cannot reach this address/)).not.toBeInTheDocument();
    expect(screen.getByDisplayValue('vca://pair?v=1&s=secret1')).toBeInTheDocument();
    expect(screen.getByText(/works once and expires after 60 seconds/)).toBeInTheDocument();
    // The code to compare with the phone, next to the QR code.
    // 19 characters (80 bits): monospace, and it wraps only after a dash.
    const code = within(screen.getByTestId('pair-confirm-code')).getByTestId('confirm-code');
    expect(code).toHaveTextContent(/^ABCD-EFGH-JKMN-PQRS$/);
    expect(code.className).toMatch(/code/);
    expect(code.querySelectorAll('wbr')).toHaveLength(3);
    // The order of steps, and whether the phone needs a code.
    expect(screen.getByRole('list', { name: 'Pairing steps' })).toHaveTextContent(
      /DNSPilot.*Connect a phone.*scan the QR code.*6-digit code/,
    );
    expect(screen.getByText(/This PC has 2-step verification on/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Stop server' })).toBeEnabled();
  });

  it('counts down 60 seconds, discards the code and waits for a click', async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderView();
    await createQr();
    expect(await screen.findByText('Expires in 1:00')).toBeInTheDocument();
    tick(1000);
    expect(screen.getByText('Expires in 0:59')).toBeInTheDocument();
    tick(58_000);
    expect(screen.getByText('Expires in 0:01')).toBeInTheDocument();
    tick(1000);
    expect(screen.getByText(/This code expired and was discarded/)).toBeInTheDocument();
    // Gone: no QR code, no link.
    expect(screen.queryByRole('img', { name: 'Pairing QR code' })).not.toBeInTheDocument();
    expect(screen.queryByLabelText('Pairing link')).not.toBeInTheDocument();
    // Never regenerated on its own.
    tick(120_000);
    expect(pairCount).toBe(1);

    await user.click(screen.getByRole('button', { name: 'New QR code' }));
    expect(await screen.findByText('Expires in 1:00')).toBeInTheDocument();
    expect(pairCount).toBe(2);
    expect(screen.getByDisplayValue('vca://pair?v=1&s=secret2')).toBeInTheDocument();
    expect(screen.queryByText(/This code expired/)).not.toBeInTheDocument();
  });

  it('says when the phone needs no code', async () => {
    current = status({ remote: remote({ require_totp: false, totp_enrolled: false }) });
    renderView();
    await createQr();
    await screen.findByRole('img', { name: 'Pairing QR code' });
    expect(screen.getByText(/2-step verification is off on this PC/)).toBeInTheDocument();
  });

  it('removes the QR code when the scope changes and offers a new one', async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderView();
    await createQr();
    await screen.findByRole('img', { name: 'Pairing QR code' });
    await user.click(screen.getByRole('button', { name: 'Admin' }));
    // No new code (and no new verification prompt) on its own.
    expect(pairCount).toBe(1);
    expect(screen.queryByRole('img', { name: 'Pairing QR code' })).toBeNull();
    expect(screen.getByText(/Permission changed/)).toBeInTheDocument();
    expect(screen.getByText(/including managing devices/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'New QR code' }));
    await waitFor(() => expect(pairCount).toBe(2));
    expect(tauri.callsOf('vca_host_pair')[1]?.args).toEqual({ scope: 'admin' });
    expect(await screen.findByRole('img', { name: 'Pairing QR code' })).toBeInTheDocument();
  });

  it('copies the pairing link', async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderView();
    await createQr();
    await screen.findByDisplayValue('vca://pair?v=1&s=secret1');
    await user.click(screen.getByRole('button', { name: 'Copy' }));
    expect(await screen.findByRole('button', { name: 'Copied' })).toBeInTheDocument();
    expect(await navigator.clipboard.readText()).toBe('vca://pair?v=1&s=secret1');
  });

  it.each([
    ['takes the copied link off the clipboard when it expires', 'vca://pair?v=1&s=secret1', true],
    ['leaves something else on the clipboard alone', 'my own text', false],
  ])('%s', async (_name, onClipboard, cleared) => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderView();
    await createQr();
    await screen.findByDisplayValue('vca://pair?v=1&s=secret1');
    await user.click(screen.getByRole('button', { name: 'Copy' }));
    await screen.findByRole('button', { name: 'Copied' });
    const read = vi.spyOn(navigator.clipboard, 'readText').mockResolvedValue(onClipboard);
    const write = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue();
    tick(61_000);
    await waitFor(() => expect(read).toHaveBeenCalled());
    await act(async () => {
      await Promise.resolve();
    });
    if (cleared) expect(write).toHaveBeenCalledWith('');
    else expect(write).not.toHaveBeenCalled();
    read.mockRestore();
    write.mockRestore();
  });

  it('warns when phones could only reach a loopback address', async () => {
    tauri.handlers.set('vca_host_pair', () =>
      pairing(1, { endpoints: ['ws://127.0.0.1:47800/v1/ws'], loopback_only: true }),
    );
    current = status({ remote: remote({ provider: null, listen: '127.0.0.1:47800' }) });
    renderView();
    expect(await screen.findByText(/Phones cannot reach this address/)).toBeInTheDocument();
    expect(screen.getByText('None (listen address only)')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: /How to reach your PC/ })).toHaveAttribute(
      'href',
      expect.stringContaining('user-guide.md#5-reach-your-pc'),
    );
  });

  it('starts a stopped server', async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    current = status({ running: false, remote: null, managed: false, version: null });
    tauri.handlers.set('vca_host_start', () => {
      current = status();
      return current;
    });
    renderView();
    expect(await screen.findByText('Stopped')).toBeInTheDocument();
    expect(screen.queryByRole('img', { name: 'Pairing QR code' })).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Start server' }));
    expect(await screen.findByText('Running · v0.1.0')).toBeInTheDocument();
    expect(tauri.callsOf('vca_host_start')).toHaveLength(1);
    await createQr();
    expect(await screen.findByRole('img', { name: 'Pairing QR code' })).toBeInTheDocument();
  });

  it('explains when the server is not bundled or started elsewhere', async () => {
    current = status({ running: false, remote: null, can_start: false });
    const { unmount } = renderView();
    expect(await screen.findByText(/does not include the server/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Start server' })).toBeDisabled();
    unmount();

    current = status({ managed: false });
    renderView();
    expect(await screen.findByText(/started outside the app/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Stop server' })).not.toBeInTheDocument();
  });

  it('saves the run-on-this-PC setting', async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    tauri.handlers.set('vca_host_set_run_server', () => undefined);
    renderView();
    const box = await screen.findByRole('checkbox', { name: 'Run the VCA server on this PC' });
    expect(box).toBeChecked();
    await user.click(box);
    expect(tauri.callsOf('vca_host_set_run_server')[0]?.args).toEqual({ enabled: false });
    expect(box).not.toBeChecked();
  });

  it('says when one code registers the phone for remote access and pairs it', async () => {
    tauri.handlers.set('vca_host_pair', () => pairing(1, { invite_included: true }));
    renderView();
    await createQr();
    expect(await screen.findByText(ONE_QR)).toBeInTheDocument();
  });

  it('keeps the plain code with a short note when no invite could be made', async () => {
    tauri.handlers.set('vca_host_pair', () => pairing(1, { invite_skipped: 'not_owned' }));
    renderView();
    await createQr();
    expect(await screen.findByRole('note')).toHaveTextContent('this app didn’t create it');
    expect(screen.getByRole('img', { name: /QR/i })).toBeInTheDocument();
    expect(screen.getByDisplayValue('vca://pair?v=1&s=secret1')).toBeInTheDocument();
    expect(screen.queryByText(ONE_QR)).not.toBeInTheDocument();
    // No DNSPilot service here: nothing to reconnect, the console guidance stays.
    expect(screen.queryByRole('button', { name: 'Reconnect DNSPilot' })).toBeNull();
  });

  for (const reason of ['not_owned', 'not_connected'] as const) {
    it(`offers to reconnect DNSPilot when the invite was skipped (${reason})`, async () => {
      vi.useRealTimers();
      const user = userEvent.setup();
      tauri.handlers.set('vca_host_pair', () => pairing(1, { invite_skipped: reason }));
      let approve: () => void = () => {};
      const dnspilot = {
        authorizeStart: vi.fn(async () => ({
          userCode: 'WXYZ-1234',
          verificationUri: 'https://console.example/activate',
          verificationUriComplete: null,
          expiresIn: 600,
        })),
        authorizeWait: vi.fn(() => new Promise<void>((resolve) => (approve = resolve))),
        authorizeCancel: vi.fn(async () => {}),
        openActivation: vi.fn(async () => {}),
      } as unknown as DnsPilotService;
      render(
        <I18nProvider locale="en">
          <ConnectPhoneView host={hostBridge} dnspilot={dnspilot} clock={() => nowMs} />
        </I18nProvider>,
      );
      await createQr();
      const note = await screen.findByRole('note');
      expect(note).toHaveTextContent(/cannot register the phone for remote access/);
      expect(note).not.toHaveTextContent(/console/);
      await user.click(within(note).getByRole('button', { name: 'Reconnect DNSPilot' }));
      expect(dnspilot.authorizeStart).toHaveBeenCalledOnce();
      expect(await screen.findByText('WXYZ-1234')).toBeInTheDocument();
      await act(async () => approve());
      expect(
        await screen.findByText(/DNSPilot is connected\. Create a new QR code/),
      ).toBeInTheDocument();
      expect(screen.queryByRole('note')).toBeNull();
      expect(screen.getByRole('button', { name: 'New code' })).toBeEnabled();
    });
  }

  it('shows pairing errors', async () => {
    tauri.handlers.set('vca_host_pair', () => {
      throw { code: 'not_running', message: 'the VCA server on this PC is not running' };
    });
    renderView();
    await createQr();
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The VCA server on this PC is not running.',
    );
    expect(screen.queryByRole('img', { name: 'Pairing QR code' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Create QR code' })).toBeEnabled();
  });

  it('shows status errors', async () => {
    tauri.handlers.set('vca_host_status', () => {
      throw { code: 'internal', message: 'boom' };
    });
    renderView();
    expect(await screen.findByRole('alert')).toHaveTextContent('boom');
  });

  it('says when the remote transport is off', async () => {
    current = status({ remote: remote({ enabled: false }) });
    renderView();
    expect(await screen.findByText(/Remote access is off/)).toBeInTheDocument();
    expect(screen.queryByText('Pairing code')).not.toBeInTheDocument();
    expect(tauri.callsOf('vca_host_pair')).toHaveLength(0);
  });

  it('notices when the phone paired', async () => {
    renderView(4000);
    await createQr();
    await screen.findByRole('img', { name: 'Pairing QR code' });
    current = status({ remote: remote({ paired_devices: 1 }) });
    tick(4000);
    expect(await screen.findByText(/A device was paired/)).toBeInTheDocument();
    expect(screen.queryByRole('img', { name: 'Pairing QR code' })).not.toBeInTheDocument();
  });

  it('enrolls an authenticator when the server requires one', async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    current = status({ remote: remote({ totp_enrolled: false }) });
    tauri.handlers.set('vca_host_totp_begin', () => ({
      otpauth_uri: 'otpauth://totp/VCA:owner?secret=ABC',
      secret: 'JBSWY3DPEHPK3PXP',
      qr_svg: '<svg xmlns="http://www.w3.org/2000/svg"/>',
    }));
    let confirmed = 0;
    tauri.handlers.set('vca_host_totp_confirm', (args) => {
      confirmed += 1;
      if ((args as { code: string }).code !== '123456') {
        throw { code: 'totp_invalid', message: 'wrong code' };
      }
      current = status();
      return { backup_codes: ['aaaa-bbbb', 'cccc-dddd'] };
    });
    renderView();
    expect(await screen.findByText(/asks for an authenticator code/)).toBeInTheDocument();
    expect(screen.queryByRole('img', { name: 'Pairing QR code' })).not.toBeInTheDocument();
    expect(tauri.callsOf('vca_host_pair')).toHaveLength(0);

    await user.click(screen.getByRole('button', { name: 'Set up authenticator' }));
    expect(await screen.findByRole('img', { name: 'Authenticator QR code' })).toBeInTheDocument();
    // A first setup can be left before a code is confirmed.
    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('img', { name: 'Authenticator QR code' })).not.toBeInTheDocument();
    expect(confirmed).toBe(0);
    await user.click(screen.getByRole('button', { name: 'Set up authenticator' }));
    expect(await screen.findByRole('img', { name: 'Authenticator QR code' })).toBeInTheDocument();
    expect(screen.getByText('JBSWY3DPEHPK3PXP')).toBeInTheDocument();

    await user.type(screen.getByLabelText('Code from the app'), '000000');
    await user.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('That code is not correct');

    await user.clear(screen.getByLabelText('Code from the app'));
    await user.type(screen.getByLabelText('Code from the app'), '123456');
    await user.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(await screen.findByText('aaaa-bbbb')).toBeInTheDocument();
    expect(confirmed).toBe(2);
    // The single-use codes can be copied or saved, like in Settings › Security.
    expect(screen.getByRole('button', { name: 'Copy' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Save as file' })).toBeEnabled();

    await user.click(screen.getByRole('button', { name: 'I saved them' }));
    // Pairing is offered now; the code itself waits for a click.
    expect(await screen.findByRole('button', { name: 'Create QR code' })).toBeEnabled();
    expect(tauri.callsOf('vca_host_pair')).toHaveLength(0);
  });

  const enrolledRemote = (over: Partial<RemoteInfo> = {}) =>
    remote({
      totp_enrolled_at: Date.UTC(2026, 9, 31, 12, 0, 0),
      totp_issuer: 'Voice Command AI',
      totp_account: 'owner',
      backup_codes_remaining: 10,
      backup_codes_total: 10,
      ...over,
    });

  it('always shows the two-factor status with the entry and backup codes left', async () => {
    current = status({ remote: enrolledRemote({ backup_codes_remaining: 7 }) });
    renderView();
    const section = await screen.findByRole('region', { name: '2-step verification' });
    expect(section).toHaveTextContent('Set up (2026-10-31)');
    expect(section).toHaveTextContent('Voice Command AI (owner)');
    expect(section).toHaveTextContent('7 of 10 left');
    expect(screen.getByRole('button', { name: 'Set up again' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Make new backup codes' })).toBeEnabled();
    // Pairing stays available next to it.
    expect(await screen.findByRole('button', { name: 'Create QR code' })).toBeEnabled();
  });

  it('offers the setup when nothing is enrolled even if pairing does not need it', async () => {
    current = status({ remote: remote({ totp_enrolled: false, require_totp: false }) });
    renderView();
    const section = await screen.findByRole('region', { name: '2-step verification' });
    expect(section).toHaveTextContent('Not set up');
    expect(section).toHaveTextContent('Remote shells opened from a phone need a code');
    expect(screen.getByRole('button', { name: 'Set up authenticator' })).toBeEnabled();
    expect(await screen.findByRole('button', { name: 'Create QR code' })).toBeEnabled();
  });

  it('sets the authenticator up again, keeping the old one until the new code confirms', async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    current = status({ remote: enrolledRemote() });
    tauri.handlers.set('vca_host_totp_begin', () => ({
      otpauth_uri: 'otpauth://totp/Voice%20Command%20AI:owner?secret=NEW',
      secret: 'NEWSECRETNEWSECR',
      qr_svg: '<svg xmlns="http://www.w3.org/2000/svg"/>',
    }));
    tauri.handlers.set('vca_host_totp_confirm', (args) => {
      if ((args as { code: string }).code !== '654321') {
        throw { code: 'totp_invalid', message: 'wrong code' };
      }
      current = status({
        remote: enrolledRemote({ totp_enrolled_at: Date.UTC(2026, 10, 1, 12, 0, 0) }),
      });
      return { backup_codes: ['NEW1-NEW1-NEW1-NEW1'] };
    });
    renderView();
    await user.click(await screen.findByRole('button', { name: 'Set up again' }));
    expect(tauri.callsOf('vca_host_totp_begin')[0]?.args).toEqual({ replace: true });
    expect(await screen.findByRole('img', { name: 'Authenticator QR code' })).toBeInTheDocument();
    expect(screen.getByText(/keeps working until the new one is confirmed/)).toBeInTheDocument();

    // Cancelling leaves the enrollment as it was.
    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('img', { name: 'Authenticator QR code' })).not.toBeInTheDocument();
    expect(tauri.callsOf('vca_host_totp_confirm')).toHaveLength(0);

    await user.click(screen.getByRole('button', { name: 'Set up again' }));
    await user.type(await screen.findByLabelText('Code from the app'), '111111');
    // The current authenticator must be proven too.
    expect(screen.getByRole('button', { name: 'Confirm' })).toBeDisabled();
    await user.type(screen.getByLabelText(/current app/), '222222');
    await user.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(await screen.findByText(/That code is not correct/)).toBeInTheDocument();
    await user.clear(screen.getByLabelText('Code from the app'));
    await user.type(screen.getByLabelText('Code from the app'), '654321');
    await user.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(await screen.findByText('NEW1-NEW1-NEW1-NEW1')).toBeInTheDocument();
    expect(tauri.callsOf('vca_host_totp_confirm')[0]?.args).toMatchObject({
      current_code: '222222',
    });
    await user.click(screen.getByRole('button', { name: 'I saved them' }));
    const section = screen.getByRole('region', { name: '2-step verification' });
    await waitFor(() => expect(section).toHaveTextContent('Set up (2026-11-01)'));
  });

  it('makes new backup codes after a warning', async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    current = status({ remote: enrolledRemote({ backup_codes_remaining: 2 }) });
    tauri.handlers.set('vca_host_totp_backup_codes', () => {
      current = status({ remote: enrolledRemote() });
      return { backup_codes: ['AAAA-BBBB-CCCC-DDDD', 'EEEE-FFFF-GGGG-HHHH'] };
    });
    renderView();
    await user.click(await screen.findByRole('button', { name: 'Make new backup codes' }));
    expect(screen.getByText(/current backup codes will stop working/)).toBeInTheDocument();
    expect(tauri.callsOf('vca_host_totp_backup_codes')).toHaveLength(0);
    // One approval is not enough: the authenticator has to agree.
    expect(screen.getByRole('button', { name: 'Make new codes' })).toBeDisabled();
    await user.type(screen.getByLabelText(/Code from the current app/), '123456');
    await user.click(screen.getByRole('button', { name: 'Make new codes' }));
    expect(await screen.findByText('AAAA-BBBB-CCCC-DDDD')).toBeInTheDocument();
    expect(screen.getByText(/the old ones no longer work/)).toBeInTheDocument();
    expect(tauri.callsOf('vca_host_totp_backup_codes')[0]?.args).toMatchObject({
      current_code: '123456',
    });
    await user.click(screen.getByRole('button', { name: 'I saved them' }));
    const section = screen.getByRole('region', { name: '2-step verification' });
    await waitFor(() => expect(section).toHaveTextContent('10 of 10 left'));
  });
});
