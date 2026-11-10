import { screen, render, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { SpeechServices } from '../speech/context';
import { defaultSettings } from '../state/settings';
import { fakeServer, status, tauri } from '../test/tauriMock';
import type { MachineInfo } from './bridge';
import { NativeApp } from './NativeApp';
import { PairLinks } from './pairLinks';
import type { QrScanner, QrScanResult } from './qrScanner';
import { MachineRegistry } from './registry';

vi.mock('@tauri-apps/api/core', () => import('../test/tauriMock'));
vi.mock('@tauri-apps/plugin-deep-link', () => ({
  getCurrent: async () => deepLink.launch,
  onOpenUrl: async (cb: (urls: string[]) => void) => {
    deepLink.open = cb;
    return () => {
      deepLink.open = null;
    };
  },
}));

const deepLink: { launch: string[] | null; open: ((urls: string[]) => void) | null } = {
  launch: null,
  open: null,
};

const speech: SpeechServices = {
  transcribe: async () => ({ text: '', corrections: [] }),
  createRecorder: () => ({ start: async () => {}, stop: async () => new Blob(), cancel: () => {} }),
  fallbackWithoutMic: false,
};

const INFO: MachineInfo = {
  id: 'mach-1',
  name: 'studio-pc',
  endpoints: ['tcp://192.168.0.10:7420'],
  deviceId: 'dev-1',
  scope: 'admin',
  pairedAt: 1,
};

const URI = 'vca://pair?v=1&pk=abc&ep=tcp%3A%2F%2F192.168.0.10%3A7420';

function renderNative(extra: { qrScanner?: QrScanner; pairLinks?: PairLinks } = {}) {
  return render(
    <NativeApp
      registry={new MachineRegistry()}
      speech={speech}
      settings={{ ...defaultSettings(), language: 'en' }}
      {...extra}
    />,
  );
}

/** A camera scanner whose scan resolves when the test says so. */
function fakeScanner() {
  let finish: (r: QrScanResult) => void = () => {};
  const scanner = {
    scan: vi.fn(
      () =>
        new Promise<QrScanResult>((resolve) => {
          finish = resolve;
        }),
    ),
    cancel: vi.fn(async () => finish({ kind: 'cancelled' })),
    openSettings: vi.fn(async () => {}),
  } satisfies QrScanner;
  return { scanner, finish: (r: QrScanResult) => finish(r) };
}

describe('NativeApp', () => {
  let machines: MachineInfo[];

  beforeEach(() => {
    tauri.reset();
    tauri.handlers.set('vca_pairing_code', () => 'ABCD-EFGH');
    deepLink.launch = null;
    deepLink.open = null;
    machines = [];
    tauri.handlers.set('vca_machines', () => machines);
    tauri.handlers.set('vca_platform', () => 'android');
  });

  it('shows the pairing screen when no machine is paired', async () => {
    renderNative();
    expect(await screen.findByRole('heading', { name: 'Pair a device' })).toBeInTheDocument();
    expect(screen.getByText(/No machine is paired yet/)).toBeInTheDocument();
    expect(await screen.findByDisplayValue('My Android')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Scan QR code/ })).toBeEnabled();
  });

  it('keeps the paste flow on desktops: no camera scanning', async () => {
    tauri.handlers.set('vca_platform', () => 'windows');
    const { scanner } = fakeScanner();
    renderNative({ qrScanner: scanner });
    expect(await screen.findByDisplayValue('My Windows')).toBeInTheDocument();
    // No camera: no scan button, the link field is the way in.
    expect(screen.queryByRole('button', { name: /Scan QR code/ })).toBeNull();
    expect(screen.getByText(/paste the pairing link/)).toBeInTheDocument();
    expect(screen.getByLabelText('Pairing link')).toBeVisible();
  });

  it('fills the pairing link from a scanned QR code but waits for the tap', async () => {
    tauri.handlers.set('vca_platform', () => 'ios');
    const { scanner, finish } = fakeScanner();
    renderNative({ qrScanner: scanner });
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    // The camera runs behind a transparent page with a cancel overlay.
    expect(screen.getByRole('dialog')).toHaveTextContent(
      'Point the camera at the pairing QR code.',
    );
    expect(document.documentElement).toHaveClass('vca-scanning');
    finish({ kind: 'link', uri: URI });
    expect(await screen.findByDisplayValue(URI)).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(document.documentElement).not.toHaveClass('vca-scanning');
    // The confirm code check (step 1) comes before anything is sent.
    expect(screen.getByTestId('step-scan')).toHaveAttribute('data-state', 'active');
    expect(tauri.callsOf('vca_pair')).toHaveLength(0);
  });

  it('cancels a scan from the overlay', async () => {
    tauri.handlers.set('vca_platform', () => 'android');
    const { scanner } = fakeScanner();
    renderNative({ qrScanner: scanner });
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(scanner.cancel).toHaveBeenCalled();
    expect(await screen.findByText('Scanning cancelled.')).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.getByLabelText('Pairing link')).toHaveValue('');
  });

  it('explains a refused camera and offers the settings', async () => {
    tauri.handlers.set('vca_platform', () => 'ios');
    const { scanner, finish } = fakeScanner();
    renderNative({ qrScanner: scanner });
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    finish({ kind: 'denied' });
    expect(await screen.findByRole('alert')).toHaveTextContent(/Camera access is off/);
    await user.click(screen.getByRole('button', { name: 'Open Settings' }));
    expect(scanner.openSettings).toHaveBeenCalledOnce();
  });

  it('rejects a QR code that is not a pairing link', async () => {
    tauri.handlers.set('vca_platform', () => 'ios');
    const { scanner, finish } = fakeScanner();
    renderNative({ qrScanner: scanner });
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    finish({ kind: 'invalid' });
    expect(await screen.findByRole('alert')).toHaveTextContent(
      /not a Voice Command AI pairing code/,
    );
    expect(screen.getByLabelText('Pairing link')).toHaveValue('');
  });

  it('stops the camera when the pairing screen goes away', async () => {
    tauri.handlers.set('vca_platform', () => 'ios');
    const { scanner } = fakeScanner();
    const view = renderNative({ qrScanner: scanner });
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    view.unmount();
    expect(scanner.cancel).toHaveBeenCalled();
    expect(document.documentElement).not.toHaveClass('vca-scanning');
  });

  it('fills the form from a link buffered before the app rendered (cold start)', async () => {
    const links = new PairLinks();
    links.pushLaunch(URI);
    renderNative({ pairLinks: links });
    expect(await screen.findByDisplayValue(URI)).toBeInTheDocument();
    expect(tauri.callsOf('vca_pair')).toHaveLength(0);
  });

  it('fills the form from a launch link captured natively', async () => {
    tauri.handlers.set('vca_launch_links', () => [URI]);
    renderNative();
    expect(await screen.findByDisplayValue(URI)).toBeInTheDocument();
    expect(tauri.callsOf('vca_pair')).toHaveLength(0);
  });

  it('pairs a machine and opens it', async () => {
    fakeServer();
    tauri.handlers.set('vca_pair', () => {
      machines = [INFO];
      return INFO;
    });
    renderNative();
    const user = userEvent.setup();
    // With a camera, pasting is the secondary option, folded away.
    await user.click(await screen.findByText('Paste a link instead'));
    const field = screen.getByLabelText('Pairing link');
    expect(field).toBeVisible();
    await user.type(field, URI);
    await user.click(await screen.findByRole('checkbox', { name: /same code/ }));
    // The link does not ask for a 2-step code: no code step.
    expect(screen.queryByLabelText('VCA 2-step verification code (6 digits)')).toBeNull();
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    expect(tauri.callsOf('vca_pair')[0]!.args).toEqual({
      uri: URI,
      deviceName: 'My Android',
      totp: null,
    });
    // The new machine is selected and connected.
    expect(await screen.findByRole('combobox', { name: 'Machine' })).toHaveValue('mach-1');
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('Connected'));
    expect(tauri.callsOf('vca_connect')[0]!.args).toMatchObject({ machineId: 'mach-1' });
  });

  it('shows pairing errors by code', async () => {
    let reject: unknown = { code: 'totp_invalid', message: 'bad code' };
    tauri.handlers.set('vca_pair', () => {
      throw reject;
    });
    renderNative();
    const user = userEvent.setup();
    await user.click(await screen.findByText('Paste a link instead'));
    const input = screen.getByLabelText('Pairing link');
    await user.type(input, 'https://example.com');
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    expect(screen.getByRole('alert')).toHaveTextContent('This is not a valid vca://pair link.');
    expect(tauri.callsOf('vca_pair')).toHaveLength(0);

    await user.clear(input);
    await user.click(input);
    await user.paste(URI);
    await user.click(await screen.findByRole('checkbox', { name: /same code/ }));
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('That code was not accepted.');

    // The code is now known to be needed; a pasted link still needs the tap.
    reject = 'handshake failed';
    await user.type(screen.getByLabelText('VCA 2-step verification code (6 digits)'), '123456');
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    await waitFor(() =>
      expect(screen.getByRole('alert')).toHaveTextContent('Something went wrong: handshake failed'),
    );
  });

  it('prompts for a code on login_required, then reconnects', async () => {
    machines = [INFO];
    let loggedIn = false;
    fakeServer({ onConnect: () => (loggedIn ? status('connected') : status('login_required')) });
    tauri.handlers.set('vca_login', () => {
      loggedIn = true;
    });
    renderNative();
    const user = userEvent.setup();
    expect(
      await screen.findByRole('heading', { name: 'Sign in to studio-pc' }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Use a backup code instead' }));
    await user.type(screen.getByLabelText('Backup code'), 'abcd-efgh');
    await user.click(screen.getByRole('button', { name: 'Sign in' }));
    expect(tauri.callsOf('vca_login')[0]!.args).toEqual({
      machineId: 'mach-1',
      totp: null,
      backupCode: 'abcd-efgh',
    });
    await waitFor(() =>
      expect(screen.queryByRole('heading', { name: /Sign in to/ })).not.toBeInTheDocument(),
    );
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('Connected'));
  });

  it('forgets a machine from settings and falls back to pairing', async () => {
    machines = [INFO];
    fakeServer();
    tauri.handlers.set('vca_forget', () => {
      machines = [];
    });
    renderNative();
    const user = userEvent.setup();
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('Connected'));
    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(screen.getByRole('tab', { name: 'Connection & security' }));
    await user.click(screen.getByRole('button', { name: 'Forget this machine' }));
    await user.click(screen.getByRole('button', { name: 'Forget' }));
    expect(await screen.findByText(/No machine is paired yet/)).toBeInTheDocument();
    expect(tauri.callsOf('vca_forget')[0]!.args).toEqual({ machineId: 'mach-1' });
  });

  it('fills the pairing form from a launch link but waits for the tap', async () => {
    deepLink.launch = ['https://example.com/ignored', URI];
    renderNative();
    expect(await screen.findByDisplayValue(URI)).toBeInTheDocument();
    expect(tauri.callsOf('vca_pair')).toHaveLength(0);
  });

  it('switches to the pairing form when a link arrives while connected', async () => {
    fakeServer();
    machines = [INFO];
    renderNative();
    await waitFor(() => expect(deepLink.open).not.toBeNull());
    expect(await screen.findByText('studio-pc')).toBeInTheDocument();
    deepLink.open!([URI]);
    expect(await screen.findByDisplayValue(URI)).toBeInTheDocument();
    expect(tauri.callsOf('vca_pair')).toHaveLength(0);
  });
});
