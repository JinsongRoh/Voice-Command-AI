import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { SpeechServices } from '../speech/context';
import { defaultSettings } from '../state/settings';
import { fakeServer, tauri } from '../test/tauriMock';
import type { MachineInfo } from './bridge';
import { hostBridge, type HostStatus, LOCAL_MACHINE_ID } from './host';
import { NativeApp } from './NativeApp';
import { MachineRegistry } from './registry';

vi.mock('@tauri-apps/api/core', () => import('../test/tauriMock'));
vi.mock('@tauri-apps/plugin-deep-link', () => ({
  getCurrent: async () => null,
  onOpenUrl: async () => () => {},
}));

const speech: SpeechServices = {
  transcribe: async () => ({ text: '', corrections: [] }),
  createRecorder: () => ({ start: async () => {}, stop: async () => new Blob(), cancel: () => {} }),
  fallbackWithoutMic: false,
};

const LOCAL: MachineInfo = {
  id: LOCAL_MACHINE_ID,
  name: 'studio-pc',
  endpoints: [],
  deviceId: LOCAL_MACHINE_ID,
  scope: 'admin',
  pairedAt: 0,
};

const STOPPED: HostStatus = {
  running: false,
  endpoint: '\\\\.\\pipe\\vca-control-me',
  server_name: null,
  version: null,
  remote: null,
  managed: false,
  can_start: true,
  run_server: true,
  log_path: null,
};

function renderNative(host = hostBridge) {
  return render(
    <NativeApp
      registry={new MachineRegistry()}
      speech={speech}
      settings={{ ...defaultSettings(), language: 'en' }}
      host={host}
    />,
  );
}

describe('NativeApp on desktop', () => {
  let machines: MachineInfo[];

  beforeEach(() => {
    tauri.reset();
    machines = [];
    tauri.handlers.set('vca_machines', () => machines);
    tauri.handlers.set('vca_platform', () => 'windows');
    tauri.handlers.set('vca_host_status', () => STOPPED);
  });

  it('offers both "Connect a phone" and "Add a computer" on the first-run pairing screen', async () => {
    const user = userEvent.setup();
    renderNative();
    // This PC as a server for a phone, and remote computers by link.
    expect(
      await screen.findByRole('heading', { name: 'Add a computer (paste link)' }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText('Pairing link')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Connect a phone' })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Show QR code' }));
    expect(await screen.findByText('Stopped')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Back' }));
    expect(
      await screen.findByRole('heading', { name: 'Add a computer (paste link)' }),
    ).toBeInTheDocument();
  });

  it('lists this PC next to paired computers and switches between them', async () => {
    const user = userEvent.setup();
    const mac: MachineInfo = {
      id: '0011223344556677',
      name: 'studio-mac',
      endpoints: ['wss://studio-mac.example.ts.net/v1/ws'],
      deviceId: 'dev-1',
      scope: 'command',
      pairedAt: 1,
    };
    const linux: MachineInfo = { ...mac, id: '8899aabbccddeeff', name: 'build-linux' };
    machines = [mac, linux, LOCAL];
    fakeServer();
    renderNative();
    const selector = await screen.findByRole('combobox', { name: 'Machine' });
    const options = screen.getAllByRole('option').map((o) => o.textContent);
    expect(options).toEqual([
      'studio-mac',
      'build-linux',
      'This PC · studio-pc',
      'Manage computers…',
      'Add computer…',
    ]);
    expect(selector).toHaveValue(mac.id);
    await user.selectOptions(selector, LOCAL_MACHINE_ID);
    expect(selector).toHaveValue(LOCAL_MACHINE_ID);
    await user.selectOptions(selector, linux.id);
    expect(selector).toHaveValue(linux.id);
    // More computers are added from the pairing screen.
    await user.selectOptions(selector, '__add__');
    expect(
      await screen.findByRole('heading', { name: 'Add a computer (paste link)' }),
    ).toBeInTheDocument();
  });

  it('shows no host features without host services (mobile)', async () => {
    renderNative(null as never);
    expect(await screen.findByRole('heading', { name: 'Pair a device' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Show QR code' })).not.toBeInTheDocument();
    expect(screen.queryByText('Add a computer (paste link)')).not.toBeInTheDocument();
  });

  it('lists this PC as a machine that cannot be forgotten, with a way to connect a phone', async () => {
    const user = userEvent.setup();
    machines = [LOCAL];
    fakeServer();
    renderNative();
    await user.click(await screen.findByRole('button', { name: 'Settings' }));
    expect(screen.getByText('This PC (local)')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Forget this machine' })).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Connect a phone' }));
    expect(await screen.findByRole('heading', { name: 'Connect a phone' })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Back' }));
    expect(await screen.findByRole('heading', { name: 'Settings' })).toBeInTheDocument();
  });
});
