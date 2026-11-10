import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { MachineInfo } from '../native/bridge';
import { NativeApp } from '../native/NativeApp';
import { MachineRegistry } from '../native/registry';
import type { SpeechServices } from '../speech/context';
import { defaultSettings } from '../state/settings';
import { renderWithApp } from '../test/render';
import { fakeServer, tauri } from '../test/tauriMock';
import { MachinesPanel } from './MachinesPanel';

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

const NOW = Date.now();

function info(id: string, patch: Partial<MachineInfo> = {}): MachineInfo {
  return {
    id,
    name: id,
    endpoints: ['ws://192.168.0.10:7300/v1/ws'],
    deviceId: `dev-${id}`,
    scope: 'command',
    pairedAt: NOW - 86_400_000,
    alias: null,
    isDefault: false,
    lastConnected: NOW - 60_000,
    serverFingerprint: 'ab12-cd34-ef56-7890',
    ...patch,
  };
}

let machines: MachineInfo[];

/** Shell commands backed by `machines`, like vca-client's hub. */
function nativeShell() {
  tauri.handlers.set('vca_machines', () => machines);
  tauri.handlers.set('vca_platform', () => 'ios');
  tauri.handlers.set('vca_machine_rename', (args) => {
    const { machineId, alias } = args as { machineId: string; alias: string | null };
    machines = machines.map((m) => (m.id === machineId ? { ...m, alias } : m));
    return machines;
  });
  tauri.handlers.set('vca_machine_set_default', (args) => {
    const { machineId } = args as { machineId: string | null };
    machines = machines.map((m) => ({ ...m, isDefault: m.id === machineId }));
    return machines;
  });
  tauri.handlers.set('vca_machine_move', (args) => {
    const { machineId, offset } = args as { machineId: string; offset: number };
    const from = machines.findIndex((m) => m.id === machineId);
    const to = Math.max(0, Math.min(machines.length - 1, from + offset));
    const next = [...machines];
    const [m] = next.splice(from, 1);
    next.splice(to, 0, m!);
    machines = next;
    return machines;
  });
  tauri.handlers.set('vca_forget', (args) => {
    const { machineId } = args as { machineId: string };
    machines = machines.filter((m) => m.id !== machineId);
    return undefined;
  });
}

function renderNative() {
  return render(
    <NativeApp
      registry={new MachineRegistry()}
      speech={speech}
      settings={{ ...defaultSettings(), language: 'en' }}
    />,
  );
}

const selector = () => screen.getByLabelText('Machine') as HTMLSelectElement;

describe('machine selector and MachinesPanel', () => {
  beforeEach(() => {
    tauri.reset();
    fakeServer();
    machines = [
      info('desk', {
        name: 'DESK-PC',
        alias: 'Desk',
        endpoints: ['wss://desk.tail1234.ts.net/v1/ws'],
        scope: 'admin',
      }),
      info('laptop', { name: 'Laptop', isDefault: true }),
    ];
    nativeShell();
  });

  it('opens the default machine and shows aliases in the compact selector', async () => {
    renderNative();
    await waitFor(() => expect(selector().value).toBe('laptop'));
    const options = within(selector()).getAllByRole('option');
    expect(options.map((o) => o.textContent)).toEqual([
      'Desk',
      'Laptop',
      'Manage computers…',
      'Add computer…',
    ]);
  });

  it('"Add computer…" goes straight to pairing', async () => {
    renderNative();
    const user = userEvent.setup();
    await waitFor(() => expect(selector()).toBeInTheDocument());
    await user.selectOptions(selector(), 'Add computer…');
    expect(await screen.findByRole('heading', { name: 'Pair a device' })).toBeInTheDocument();
    // The selection itself did not change.
    expect(selector().value).toBe('laptop');
  });

  it('manages a machine from the full-screen sheet', async () => {
    renderNative();
    const user = userEvent.setup();
    await waitFor(() => expect(selector()).toBeInTheDocument());
    await user.selectOptions(selector(), 'Manage computers…');
    const sheet = await screen.findByRole('dialog', { name: 'Computers' });
    const rows = within(sheet).getAllByRole('listitem');
    expect(rows).toHaveLength(2);
    expect(within(rows[0]!).getByText('Tailscale · desk.tail1234.ts.net')).toBeInTheDocument();
    expect(within(rows[1]!).getByText('Default')).toBeInTheDocument();
    expect(within(rows[1]!).getByText('Selected')).toBeInTheDocument();
    expect(within(rows[0]!).getByRole('button', { name: 'Move Desk up' })).toBeDisabled();

    // Edit sheet: details.
    await user.click(within(sheet).getByRole('button', { name: 'Edit Desk' }));
    const edit = screen.getByRole('dialog', { name: 'Desk' });
    expect(within(edit).getByText('DESK-PC')).toBeInTheDocument();
    expect(within(edit).getByText('Tailscale')).toBeInTheDocument();
    expect(within(edit).getByText('desk.tail1234.ts.net')).toBeInTheDocument();
    expect(within(edit).getByText('ab12-cd34-ef56-7890')).toBeInTheDocument();
    expect(within(edit).getByText('Admin')).toBeInTheDocument();

    // Alias.
    const alias = within(edit).getByLabelText('Display name');
    await user.clear(alias);
    await user.type(alias, 'Studio');
    await user.click(within(edit).getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(tauri.callsOf('vca_machine_rename')[0]!.args).toEqual({
        machineId: 'desk',
        alias: 'Studio',
      }),
    );
    expect(await within(selector()).findByRole('option', { name: 'Studio' })).toBeInTheDocument();

    // Default and order.
    const editNow = () => screen.getByRole('dialog', { name: 'Studio' });
    await user.click(within(editNow()).getByLabelText('Open this computer on launch'));
    await waitFor(() =>
      expect(tauri.callsOf('vca_machine_set_default')[0]!.args).toEqual({ machineId: 'desk' }),
    );
    await user.click(within(editNow()).getByRole('button', { name: 'Move down' }));
    await waitFor(() =>
      expect(tauri.callsOf('vca_machine_move')[0]!.args).toEqual({ machineId: 'desk', offset: 1 }),
    );
    await waitFor(() =>
      expect(within(selector()).getAllByRole('option')[0]).toHaveTextContent('Laptop'),
    );

    // Forget needs a confirmation.
    await user.click(within(editNow()).getByRole('button', { name: 'Forget this machine' }));
    expect(tauri.callsOf('vca_forget')).toHaveLength(0);
    const confirm = screen.getByRole('dialog', { name: 'Forget this machine' });
    await user.click(within(confirm).getByRole('button', { name: 'Forget' }));
    await waitFor(() =>
      expect(tauri.callsOf('vca_forget')[0]!.args).toEqual({ machineId: 'desk' }),
    );
    await waitFor(() =>
      expect(within(selector()).queryByRole('option', { name: 'Studio' })).not.toBeInTheDocument(),
    );
    expect(screen.queryByRole('dialog', { name: 'Studio' })).not.toBeInTheDocument();
  });

  it('re-pair opens the pairing flow', async () => {
    renderNative();
    const user = userEvent.setup();
    await waitFor(() => expect(selector()).toBeInTheDocument());
    await user.selectOptions(selector(), 'Manage computers…');
    const sheet = await screen.findByRole('dialog', { name: 'Computers' });
    await user.click(within(sheet).getByRole('button', { name: 'Edit Laptop' }));
    await user.click(screen.getByRole('button', { name: 'Re-pair' }));
    expect(await screen.findByRole('heading', { name: 'Pair a device' })).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('is reachable from Settings and reorders with the row buttons', async () => {
    renderNative();
    const user = userEvent.setup();
    await waitFor(() => expect(selector()).toBeInTheDocument());
    await user.click(screen.getByRole('button', { name: 'Settings' }));
    await user.click(screen.getByRole('tab', { name: 'Connection & security' }));
    const section = screen.getByRole('region', { name: /Computers/ });
    await user.click(within(section).getByRole('button', { name: 'Move Laptop up' }));
    await waitFor(() =>
      expect(tauri.callsOf('vca_machine_move')[0]!.args).toEqual({
        machineId: 'laptop',
        offset: -1,
      }),
    );
    await waitFor(() =>
      expect(within(section).getAllByRole('listitem')[0]).toHaveTextContent('Laptop'),
    );
  });
});

describe('MachinesPanel without the native shell', () => {
  it('lists machines read-only and explains why', () => {
    renderWithApp(<MachinesPanel onPair={() => {}} />);
    expect(screen.getByText('test machine')).toBeInTheDocument();
    expect(screen.getByText(/needs the Voice Command AI app/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Move/ })).not.toBeInTheDocument();
  });
});
