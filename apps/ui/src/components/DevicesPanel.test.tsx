import { act, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ClientMsg, DeviceScope, PairedDevice, ServerMsg } from '../protocol/types';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { DEVICES_REFRESH_MS } from './deviceInfo';
import { DevicesPanel } from './DevicesPanel';

const NOW = Date.now();

function device(id: string, patch: Partial<PairedDevice> = {}): PairedDevice {
  return {
    id,
    name: id,
    platform: 'ios',
    paired_at: NOW - 86_400_000,
    last_seen: NOW - 3_600_000,
    current: false,
    scope: 'command',
    online: false,
    connections: 0,
    last_ip: null,
    ...patch,
  };
}

/** A tiny device server with the real permission rules. */
function deviceServer(initial: PairedDevice[]) {
  let list = initial;
  const respond = (msg: ClientMsg): ServerMsg => {
    const self = list.find((d) => d.current);
    const isAdmin = !self?.scope || self.scope === 'admin';
    switch (msg.type) {
      case 'list_devices':
        return { type: 'devices', devices: list };
      case 'rename_device':
        if (!isAdmin) return { type: 'error', code: 'forbidden', message: 'no' };
        list = list.map((d) => (d.id === msg.device_id ? { ...d, name: msg.name } : d));
        return { type: 'devices', devices: list };
      case 'set_device_scope':
        if (!isAdmin || self?.id === msg.device_id)
          return { type: 'error', code: 'forbidden', message: 'no' };
        list = list.map((d) => (d.id === msg.device_id ? { ...d, scope: msg.scope } : d));
        return { type: 'devices', devices: list };
      case 'revoke_device':
        list = list.filter((d) => d.id !== msg.device_id);
        return { type: 'ok' };
      default:
        return defaultResponse(msg);
    }
  };
  return new FakeTransport(respond);
}

function admin(): PairedDevice[] {
  return [
    device('dev-old', { name: 'Old tablet', platform: 'Android 16', last_seen: NOW - 9e8 }),
    device('dev-me', {
      name: 'My phone',
      current: true,
      scope: 'admin',
      online: true,
      connections: 2,
      last_ip: '100.101.7.12',
      app_version: '0.1.0',
    }),
    device('dev-pc', {
      name: 'Desk PC',
      platform: 'windows',
      scope: 'view',
      last_ip: '192.168.0.9',
    }),
  ];
}

function renderPanel(devices: PairedDevice[], onPair = vi.fn()) {
  const transport = deviceServer(devices);
  renderWithApp(<DevicesPanel onPair={onPair} />, { transport });
  return { transport, onPair };
}

const rows = () => screen.getAllByRole('listitem').filter((li) => li.dataset.testid);

describe('DevicesPanel', () => {
  afterEach(() => vi.useRealTimers());

  it('lists devices online first with badges, count and details', async () => {
    renderPanel(admin());
    await screen.findByText('My phone');
    expect(rows().map((r) => r.dataset.testid)).toEqual([
      'device-dev-me',
      'device-dev-pc',
      'device-dev-old',
    ]);
    const heading = screen.getByRole('heading', { name: /Paired devices/ });
    expect(heading).toHaveTextContent('3');
    const me = within(rows()[0]!);
    expect(me.getByText('This device')).toBeInTheDocument();
    expect(me.getByText('Online · 2')).toBeInTheDocument();
    expect(me.getByText('Admin')).toBeInTheDocument();
    expect(within(rows()[1]!).getByText('View')).toBeInTheDocument();
    expect(within(rows()[1]!).getByText('Offline')).toBeInTheDocument();
    expect(within(rows()[1]!).getByText(/Last seen/)).toBeInTheDocument();
    // Details (last IP, app version) are behind the expand toggle.
    expect(screen.queryByText('100.101.7.12')).not.toBeInTheDocument();
    await userEvent.click(me.getByRole('button', { expanded: false }));
    expect(screen.getByText('100.101.7.12')).toBeInTheDocument();
    expect(screen.getByText('0.1.0')).toBeInTheDocument();
    // No platform filter with five devices or fewer.
    expect(screen.queryByRole('group', { name: 'Filter by platform' })).not.toBeInTheDocument();
  });

  it('renames a device through the overflow menu', async () => {
    const { transport } = renderPanel(admin());
    await screen.findByText('Desk PC');
    await userEvent.click(screen.getByRole('button', { name: 'Actions for Desk PC' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Rename' }));
    const input = screen.getByLabelText('Device name');
    const save = screen.getByRole('button', { name: 'Save' });
    await userEvent.clear(input);
    expect(save).toBeDisabled();
    await userEvent.type(input, 'bad\u0007name');
    expect(save).toBeDisabled();
    await userEvent.clear(input);
    await userEvent.type(input, '  Work PC ');
    await userEvent.click(save);
    await screen.findByText('Work PC');
    expect(transport.bodies('rename_device')).toEqual([
      { type: 'rename_device', device_id: 'dev-pc', name: 'Work PC' },
    ]);
  });

  it('changes permission with explanations', async () => {
    const { transport } = renderPanel(admin());
    await screen.findByText('Desk PC');
    await userEvent.click(screen.getByRole('button', { name: 'Actions for Desk PC' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Change permission' }));
    const dialog = screen.getByRole('dialog', { name: 'Permission of Desk PC' });
    expect(within(dialog).getByText('Watch sessions and resources only.')).toBeInTheDocument();
    expect(within(dialog).getByRole('radio', { name: /View/ })).toBeChecked();
    await userEvent.click(within(dialog).getByRole('radio', { name: /Command/ }));
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(transport.bodies('set_device_scope')).toEqual([
        { type: 'set_device_scope', device_id: 'dev-pc', scope: 'command' as DeviceScope },
      ]),
    );
    expect(await within(rows()[1]!).findByText('Command')).toBeInTheDocument();
  });

  it('revokes after confirmation and reloads the list', async () => {
    const { transport } = renderPanel(admin());
    await screen.findByText('Old tablet');
    await userEvent.click(screen.getByRole('button', { name: 'Actions for Old tablet' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Revoke' }));
    expect(transport.bodies('revoke_device')).toHaveLength(0);
    const confirm = screen.getByRole('dialog', { name: 'Revoke' });
    await userEvent.click(within(confirm).getByRole('button', { name: 'Revoke' }));
    await waitFor(() => expect(screen.queryByText('Old tablet')).not.toBeInTheDocument());
    expect(transport.bodies('revoke_device')).toEqual([
      { type: 'revoke_device', device_id: 'dev-old' },
    ]);
  });

  it('protects the current device', async () => {
    renderPanel(admin());
    await screen.findByText('My phone');
    await userEvent.click(screen.getByRole('button', { name: 'Actions for My phone' }));
    const scope = screen.getByRole('menuitem', { name: /Change permission/ });
    expect(scope).toBeDisabled();
    expect(scope).toHaveAccessibleDescription("A device can't change its own permission");
    expect(screen.getByRole('menuitem', { name: 'Rename' })).toBeEnabled();
    expect(screen.queryByRole('menuitem', { name: /Revoke/ })).not.toBeInTheDocument();
  });

  it('disables management without admin permission and explains why', async () => {
    const devices = admin().map((d) => (d.current ? { ...d, scope: 'command' as const } : d));
    renderPanel(devices);
    await screen.findByText('My phone');
    expect(screen.getByText(/no admin permission/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Actions for Desk PC' }));
    for (const name of [/Rename/, /Change permission/, /Revoke/]) {
      const item = screen.getByRole('menuitem', { name });
      expect(item).toBeDisabled();
      expect(item).toHaveAccessibleDescription('Needs admin permission');
    }
  });

  it('filters by platform above five devices and keeps the pairing button', async () => {
    const many = [
      ...admin(),
      device('a1', { name: 'Pixel', platform: 'android' }),
      device('a2', { name: 'Galaxy', platform: 'android' }),
      device('m1', { name: 'MacBook', platform: 'macos' }),
    ];
    const { onPair } = renderPanel(many);
    await screen.findByText('MacBook');
    const filter = screen.getByRole('group', { name: 'Filter by platform' });
    await userEvent.click(within(filter).getByRole('button', { name: 'Android' }));
    expect(rows().map((r) => r.dataset.testid)).toEqual([
      'device-a1',
      'device-a2',
      'device-dev-old',
    ]);
    await userEvent.click(within(filter).getByRole('button', { name: 'All' }));
    expect(rows()).toHaveLength(6);
    await userEvent.click(screen.getByRole('button', { name: 'Pair new device' }));
    expect(onPair).toHaveBeenCalled();
  });

  it('refreshes every ten seconds while open', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const { transport } = renderPanel(admin());
    await screen.findByText('My phone');
    const before = transport.bodies('list_devices').length;
    await act(() => vi.advanceTimersByTimeAsync(DEVICES_REFRESH_MS));
    expect(transport.bodies('list_devices').length).toBeGreaterThan(before);
  });

  it('explains when the server has no device management', async () => {
    renderWithApp(<DevicesPanel onPair={() => {}} />);
    expect(
      await screen.findByText('This server does not support device management yet.'),
    ).toBeInTheDocument();
  });
});
