import { screen, render, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { SpeechServices } from '../speech/context';
import { defaultSettings } from '../state/settings';
import { fakeServer, status, tauri } from '../test/tauriMock';
import type { MachineInfo } from './bridge';
import { NativeApp } from './NativeApp';
import { MachineRegistry } from './registry';

vi.mock('@tauri-apps/api/core', () => import('../test/tauriMock'));

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

function renderNative() {
  return render(
    <NativeApp
      registry={new MachineRegistry()}
      speech={speech}
      settings={{ ...defaultSettings(), language: 'en' }}
    />,
  );
}

describe('NativeApp', () => {
  let machines: MachineInfo[];

  beforeEach(() => {
    tauri.reset();
    machines = [];
    tauri.handlers.set('vca_machines', () => machines);
    tauri.handlers.set('vca_platform', () => 'android');
  });

  it('shows the pairing screen when no machine is paired', async () => {
    renderNative();
    expect(await screen.findByRole('heading', { name: 'Pair a device' })).toBeInTheDocument();
    expect(screen.getByText(/No machine is paired yet/)).toBeInTheDocument();
    expect(await screen.findByDisplayValue('My Android')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Scan QR code/ })).toBeDisabled();
  });

  it('pairs a machine and opens it', async () => {
    fakeServer();
    tauri.handlers.set('vca_pair', () => {
      machines = [INFO];
      return INFO;
    });
    renderNative();
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText('Pairing link'), URI);
    await user.type(screen.getByLabelText('Authenticator code (optional)'), '123456');
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    expect(tauri.callsOf('vca_pair')[0]!.args).toEqual({
      uri: URI,
      deviceName: 'My Android',
      totp: '123456',
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
    const input = await screen.findByLabelText('Pairing link');
    await user.type(input, 'https://example.com');
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    expect(screen.getByRole('alert')).toHaveTextContent('This is not a valid vca://pair link.');
    expect(tauri.callsOf('vca_pair')).toHaveLength(0);

    await user.clear(input);
    await user.type(input, URI);
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The authenticator code is incorrect.',
    );

    reject = 'handshake failed';
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
    await user.click(screen.getByRole('button', { name: 'Forget this machine' }));
    await user.click(screen.getByRole('button', { name: 'Forget' }));
    expect(await screen.findByText(/No machine is paired yet/)).toBeInTheDocument();
    expect(tauri.callsOf('vca_forget')[0]!.args).toEqual({ machineId: 'mach-1' });
  });
});
