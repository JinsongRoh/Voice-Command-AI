import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { DnsPilotService } from '../native/bridge';
import { NativeProvider, type NativeServices } from '../native/context';
import type { HostServices } from '../native/host';
import type { PushService, PushStatus } from '../native/push';
import type { ClientMsg, ServerMsg } from '../protocol/types';
import { useAppState } from '../state/store';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { PushAlertsSettings, PushOpenedWatcher } from './PushAlertsSettings';

const OFF: PushStatus = {
  enabled: false,
  available: true,
  authorized: false,
  pendingApproval: false,
  notConfigured: false,
  phones: [],
};

function fakePush(initial: PushStatus = OFF) {
  let status = { ...initial };
  const push = {
    status: vi.fn(async () => status),
    setEnabled: vi.fn(async (enabled: boolean) => {
      status = { ...status, enabled };
      return status;
    }),
    setMuted: vi.fn(async (phone: string, muted: boolean) => {
      status = {
        ...status,
        phones: status.phones.map((p) => (p.phone === phone ? { ...p, muted } : p)),
      };
      return status;
    }),
    authorizeStart: vi.fn(async () => ({
      userCode: 'BCDF-GHJK',
      verificationUri: 'https://dns.example/oauth/activate',
      verificationUriComplete: null,
      expiresIn: 600,
    })),
    authorizeWait: vi.fn(async () => {
      status = { ...status, authorized: true };
      return status;
    }),
    authorizeCancel: vi.fn(async () => {}),
    openActivation: vi.fn(async () => {}),
    disconnect: vi.fn(async () => {
      status = { ...status, enabled: false, authorized: false };
      return status;
    }),
    phoneEnable: vi.fn(async () => {}),
    phoneDisable: vi.fn(async () => {}),
    takeOpened: vi.fn(async () => null as Awaited<ReturnType<PushService['takeOpened']>>),
  } satisfies PushService;
  return push;
}

function services(push: PushService, desktop: boolean, dnspilot?: DnsPilotService): NativeServices {
  return {
    pairingCode: async () => 'ABCD-EFGH',
    pair: async () => {
      throw new Error('unused');
    },
    forget: async () => {},
    platform: async () => (desktop ? 'windows' : 'ios'),
    push,
    ...(desktop ? { host: {} as HostServices } : {}),
    ...(dnspilot ? { dnspilot } : {}),
  };
}

describe('desktop phone alerts', () => {
  it('is off by default and says the desktop app must run', async () => {
    const push = fakePush();
    renderWithApp(
      <NativeProvider value={services(push, true)}>
        <PushAlertsSettings />
      </NativeProvider>,
    );
    const toggle = await screen.findByRole('checkbox', {
      name: 'Send approval alerts to my phones',
    });
    expect(toggle).not.toBeChecked();
    expect(screen.getByText('Alerts need the desktop app running on this PC.')).toBeVisible();
    expect(push.setEnabled).not.toHaveBeenCalled();
    expect(push.authorizeStart).not.toHaveBeenCalled();
  });

  it('enabling runs the push-only approval and never the tunnel grant', async () => {
    const push = fakePush();
    const dnspilot = {
      authorizeStart: vi.fn(),
      authorizeWait: vi.fn(),
      disconnect: vi.fn(),
    } as unknown as DnsPilotService;
    let approve: () => void = () => {};
    push.authorizeWait.mockImplementationOnce(
      () =>
        new Promise<PushStatus>((resolve) => {
          approve = () => resolve({ ...OFF, authorized: true });
        }),
    );
    renderWithApp(
      <NativeProvider value={services(push, true, dnspilot)}>
        <PushAlertsSettings />
      </NativeProvider>,
    );
    await userEvent.click(
      await screen.findByRole('checkbox', { name: 'Send approval alerts to my phones' }),
    );
    expect(await screen.findByText('BCDF-GHJK')).toBeVisible();
    await userEvent.click(screen.getByRole('button', { name: 'Open' }));
    expect(push.openActivation).toHaveBeenCalled();
    await act(async () => approve());
    await waitFor(() => expect(push.setEnabled).toHaveBeenCalledWith(true));
    expect(
      await screen.findByRole('checkbox', { name: 'Send approval alerts to my phones' }),
    ).toBeChecked();
    expect(dnspilot.authorizeStart).not.toHaveBeenCalled();
    expect(dnspilot.authorizeWait).not.toHaveBeenCalled();
  });

  it('lists phones with mute and disconnects only the push grant', async () => {
    const dnspilot = { disconnect: vi.fn() } as unknown as DnsPilotService;
    const push = fakePush({
      ...OFF,
      enabled: true,
      authorized: true,
      phones: [
        { phone: 'dev_1', platform: 'ios', muted: false, registered: true, lastError: null },
      ],
    });
    renderWithApp(
      <NativeProvider value={services(push, true, dnspilot)}>
        <PushAlertsSettings />
      </NativeProvider>,
    );
    expect(await screen.findByText('Ready')).toBeVisible();
    await userEvent.click(screen.getByRole('checkbox', { name: 'Mute' }));
    expect(push.setMuted).toHaveBeenCalledWith('dev_1', true);
    expect(await screen.findByRole('checkbox', { name: 'Mute' })).toBeChecked();

    await userEvent.click(screen.getByRole('button', { name: 'Disconnect alerts' }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('Tunnels and phone connections stay as they are.');
    await userEvent.click(
      screen.getAllByRole('button', { name: 'Disconnect alerts' }).at(-1) as HTMLElement,
    );
    await waitFor(() => expect(push.disconnect).toHaveBeenCalled());
    expect(dnspilot.disconnect).not.toHaveBeenCalled();
  });

  it('says push is not configured yet when DNSPilot answered 409', async () => {
    const push = fakePush({
      ...OFF,
      enabled: true,
      authorized: true,
      notConfigured: true,
      phones: [
        {
          phone: 'dev_1',
          platform: 'android',
          muted: false,
          registered: false,
          lastError: 'push_not_configured',
        },
      ],
    });
    renderWithApp(
      <NativeProvider value={services(push, true)}>
        <PushAlertsSettings />
      </NativeProvider>,
    );
    expect(await screen.findByRole('status')).toHaveTextContent(
      'has not configured push for VCA yet',
    );
    expect(screen.getByText('DNSPilot has not configured push for VCA yet.')).toBeVisible();
  });

  it('explains a refused enable with its code', async () => {
    const push = fakePush({ ...OFF, authorized: true });
    push.setEnabled.mockRejectedValueOnce({ code: 'push_not_configured', message: 'x' });
    renderWithApp(
      <NativeProvider value={services(push, true)}>
        <PushAlertsSettings />
      </NativeProvider>,
    );
    await userEvent.click(
      await screen.findByRole('checkbox', { name: 'Send approval alerts to my phones' }),
    );
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'has not configured push for VCA yet',
    );
    expect(push.authorizeStart).not.toHaveBeenCalled();
  });
});

describe('phone alerts', () => {
  beforeEach(() => localStorage.clear());

  it('the toggle registers and unregisters this phone with the PC', async () => {
    const push = fakePush();
    renderWithApp(
      <NativeProvider value={services(push, false)}>
        <PushAlertsSettings />
      </NativeProvider>,
    );
    const toggle = screen.getByRole('checkbox', { name: 'Enable alerts on this phone' });
    expect(toggle).not.toBeChecked();
    await userEvent.click(toggle);
    expect(push.phoneEnable).toHaveBeenCalledWith('m', 'en');
    await waitFor(() => expect(toggle).toBeChecked());
    await userEvent.click(toggle);
    expect(push.phoneDisable).toHaveBeenCalledWith('m');
    await waitFor(() => expect(toggle).not.toBeChecked());
  });

  it('reports a missing permission', async () => {
    const push = fakePush();
    push.phoneEnable.mockRejectedValueOnce({ code: 'push_permission', message: 'no' });
    renderWithApp(
      <NativeProvider value={services(push, false)}>
        <PushAlertsSettings />
      </NativeProvider>,
    );
    await userEvent.click(screen.getByRole('checkbox', { name: 'Enable alerts on this phone' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Allow them in the system settings');
    expect(screen.getByRole('checkbox', { name: 'Enable alerts on this phone' })).not.toBeChecked();
  });
});

function Selected() {
  const id = useAppState((s) => s.selectedId);
  return <p data-testid="selected">{id ?? 'none'}</p>;
}

describe('a tapped notification', () => {
  beforeEach(() => localStorage.clear());

  function transport() {
    const session = localSession({
      id: 's1',
      pending_decision: {
        decision: { id: 'd1', prompt: 'Allow?', options: [{ id: 'y', label: 'Yes' }] },
        at: 1,
      },
    } as Parameters<typeof localSession>[0]);
    return new FakeTransport((msg: ClientMsg): ServerMsg => {
      if (msg.type === 'list_sessions') return { type: 'sessions', sessions: [session] };
      return defaultResponse(msg);
    });
  }

  it('opens its session and approves nothing', async () => {
    const push = fakePush();
    push.takeOpened.mockResolvedValueOnce({ machineId: 'm', sessionId: 's1' });
    const t = transport();
    renderWithApp(
      <NativeProvider value={services(push, false)}>
        <PushOpenedWatcher />
        <Selected />
      </NativeProvider>,
      { transport: t },
    );
    await waitFor(() => expect(screen.getByTestId('selected')).toHaveTextContent('s1'), {
      timeout: 4000,
    });
    // Navigation only: no input or decision answer went to the PC.
    expect(t.bodies('send_input')).toEqual([]);
    expect(t.sent.map((e) => e.body.type)).not.toContain('control');
  });

  it('looks again when the app comes back to the front', async () => {
    const push = fakePush();
    renderWithApp(
      <NativeProvider value={services(push, false)}>
        <PushOpenedWatcher />
        <Selected />
      </NativeProvider>,
      { transport: transport() },
    );
    await waitFor(() => expect(push.takeOpened).toHaveBeenCalledTimes(1));
    push.takeOpened.mockResolvedValueOnce({ machineId: null, sessionId: null });
    act(() => {
      document.dispatchEvent(new Event('visibilitychange'));
    });
    await waitFor(() => expect(push.takeOpened).toHaveBeenCalledTimes(2));
    // Without ids: the newest pending decision.
    await waitFor(() => expect(screen.getByTestId('selected')).toHaveTextContent('s1'), {
      timeout: 4000,
    });
  });

  it('is not active on the desktop', async () => {
    const push = fakePush();
    renderWithApp(
      <NativeProvider value={services(push, true)}>
        <PushOpenedWatcher />
      </NativeProvider>,
    );
    await new Promise((r) => setTimeout(r, 20));
    expect(push.takeOpened).not.toHaveBeenCalled();
  });

  it('registers again on launch where the person turned alerts on', async () => {
    localStorage.setItem('vca.pushPhone.m', '1');
    const push = fakePush();
    renderWithApp(
      <NativeProvider value={services(push, false)}>
        <PushOpenedWatcher />
      </NativeProvider>,
      { transport: transport() },
    );
    await waitFor(() => expect(push.phoneEnable).toHaveBeenCalledWith('m', 'en'));
    expect(push.phoneEnable).toHaveBeenCalledTimes(1);
  });
});
