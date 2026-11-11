import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import {
  NativeError,
  type DnsPilotRemoteTunnel,
  type DnsPilotService,
  type DnsPilotStatus,
  type DnsPilotTunnel,
  type DnsPilotZone,
} from '../native/bridge';
import { DnsPilotPanel } from './DnsPilotConnect';

const SPKI = 'ab'.repeat(32);

function tunnel(partial: Partial<DnsPilotTunnel> = {}): DnsPilotTunnel {
  return {
    hostname: 'vca-k3x9q2ab4m.example.com',
    publicPort: 443,
    tunnelId: 12,
    deviceId: 3,
    createdByApp: true,
    clientRegistered: true,
    hasAppToken: false,
    status: 'active',
    ...partial,
  };
}

const MANUAL: DnsPilotRemoteTunnel = {
  id: 13,
  hostname: 'vca-manual.example.com',
  publicPort: 443,
  localPort: 47800,
  deviceId: 9,
  deviceName: 'Studio',
  status: 'active',
  known: false,
};

/** A fake native DNSPilot service with mutable state. */
function fakeService(
  initial: Partial<DnsPilotStatus> = {},
  zones: DnsPilotZone[] | null = null,
  platform = 'windows',
) {
  let status: DnsPilotStatus = {
    connected: false,
    scope: null,
    pending: false,
    identity: { spkiSha256: SPKI, publicKeyPem: '-----BEGIN PUBLIC KEY-----' },
    tunnels: [],
    consoleUrl: 'https://dns.dnspilot.kr',
    ...initial,
  };
  let approve: (() => void) | null = null;
  const markRegistered = async (hostname: string) => {
    status = {
      ...status,
      tunnels: status.tunnels.map((x) =>
        x.hostname === hostname ? { ...x, clientRegistered: true } : x,
      ),
    };
    return status.tunnels.find((x) => x.hostname === hostname)!;
  };
  const service: DnsPilotService = {
    status: vi.fn(async () => status),
    identity: vi.fn(async () => status.identity!),
    authorizeStart: vi.fn(async () => ({
      userCode: 'BCDF-GHJK',
      verificationUri: 'https://dns.dnspilot.kr/oauth/activate',
      verificationUriComplete: 'https://dns.dnspilot.kr/oauth/activate?user_code=BCDF-GHJK',
      expiresIn: 600,
    })),
    authorizeWait: vi.fn(
      () =>
        new Promise<DnsPilotStatus>((resolve) => {
          approve = () => {
            status = { ...status, connected: true, scope: 'tunnels:read tunnels:write' };
            resolve(status);
          };
        }),
    ),
    authorizeCancel: vi.fn(async () => {}),
    openActivation: vi.fn(async () => {}),
    openConsole: vi.fn(async () => {}),
    createTunnel: vi.fn(async () => {
      const t = tunnel();
      status = { ...status, tunnels: [...status.tunnels, t] };
      return t;
    }),
    addTunnel: vi.fn(async (hostname: string, tunnelId: number | null) => {
      const t = tunnel({ hostname, tunnelId, createdByApp: false, clientRegistered: false });
      status = { ...status, tunnels: [...status.tunnels, t] };
      return t;
    }),
    setTunnelId: vi.fn(async () => status.tunnels[0]!),
    register: vi.fn(async () => ({
      result: 'needs_console' as const,
      spki_sha256: SPKI,
      tunnel_id: 13,
      reason: 'not_created_by_app',
    })),
    markRegistered: vi.fn(markRegistered),
    removeTunnel: vi.fn(async () => {}),
    issueAppToken: vi.fn(async () => tunnel()),
    disconnect: vi.fn(async () => {
      status = { ...status, connected: false };
    }),
    devices: vi.fn(async () => [
      { id: 3, name: 'Desk', hostname: 'desk', os: 'windows', online: true, agentVersion: '1.4' },
      { id: 4, name: 'Laptop', hostname: null, os: 'linux', online: false, agentVersion: null },
    ]),
    zones: vi.fn(async () => zones),
    remoteTunnels: vi.fn(async () =>
      status.tunnels.some((t) => t.tunnelId === MANUAL.id) ? [] : [MANUAL],
    ),
    useTunnel: vi.fn(async (tunnelId: number) => {
      const t = tunnel({
        hostname: MANUAL.hostname,
        tunnelId,
        createdByApp: false,
        clientRegistered: false,
      });
      status = { ...status, tunnels: [...status.tunnels, t] };
      return {
        result: 'needs_console' as const,
        spki_sha256: SPKI,
        tunnel_id: tunnelId,
        reason: 'not_created_by_app',
      };
    }),
    refresh: vi.fn(async () => status),
    localDevice: vi.fn(async () => 3),
    platform: vi.fn(async () => platform),
    enroll: vi.fn(async () => null),
    appClients: vi.fn(async () => [
      { id: 7, label: 'VCA phone', source: 'invite', lastSeenAt: null, thisApp: false },
      { id: 5, label: 'Voice Command AI app', source: 'oauth', lastSeenAt: null, thisApp: true },
    ]),
    revokeAppClient: vi.fn(async () => {}),
    invites: vi.fn(async () => [
      { id: 21, label: 'Tablet', status: 'active', expiresAt: '2026-10-31T00:10:00Z' },
      { id: 20, label: 'Old', status: 'used', expiresAt: '2026-10-30T00:10:00Z' },
    ]),
    revokeInvite: vi.fn(async () => {}),
  };
  return { service, approve: () => approve?.() };
}

function renderPanel(service: DnsPilotService) {
  render(
    <I18nProvider locale="en">
      <DnsPilotPanel service={service} />
    </I18nProvider>,
  );
}

describe('DnsPilotConnect', () => {
  it('runs the device flow and creates a tunnel on the picked PC', async () => {
    const user = userEvent.setup();
    const { service, approve } = fakeService();
    renderPanel(service);
    // Nothing is loaded until the person opens the panel.
    expect(service.status).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    await user.click(await screen.findByRole('button', { name: 'Connect DNSPilot' }));

    expect(await screen.findByText('BCDF-GHJK')).toBeInTheDocument();
    expect(screen.getByText(/dns\.dnspilot\.kr\/oauth\/activate/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Open' }));
    expect(service.openActivation).toHaveBeenCalled();

    approve();
    expect(await screen.findByText('Connected to DNSPilot')).toBeInTheDocument();

    // The PC this app runs on is preselected; the zones call is not live, so
    // the zone id is typed in.
    const picker = await screen.findByLabelText('PC');
    expect(picker).toHaveValue('3');
    expect(screen.getByRole('option', { name: 'Desk · windows · online (this PC)' })).toBeVisible();
    expect(screen.getByRole('option', { name: 'Laptop · linux · offline' })).toBeInTheDocument();
    await user.type(screen.getByLabelText('Zone ID'), '7');
    await user.click(screen.getByRole('button', { name: 'Create tunnel on Desk' }));
    // Nothing is sent before the confirmation names the device and the zone.
    expect(service.createTunnel).not.toHaveBeenCalled();
    const dialog = await screen.findByRole('dialog', { name: 'Create this tunnel?' });
    expect(dialog).toHaveTextContent('on Desk, under the domain #7');
    await user.click(within(dialog).getByRole('button', { name: 'Create tunnel' }));
    expect(service.createTunnel).toHaveBeenCalledWith({ deviceId: 3, zoneId: 7 });

    expect(await screen.findByText('vca-k3x9q2ab4m.example.com')).toBeInTheDocument();
    expect(screen.getByText('Ready')).toBeInTheDocument();
    expect(screen.getByText('Active')).toBeInTheDocument();
    expect(screen.getByText(/kind = "dnspilot"/)).toHaveTextContent(
      'hostname = "vca-k3x9q2ab4m.example.com"',
    );
    // No manual tunnel-id step after creating.
    expect(screen.queryByLabelText('Tunnel ID')).not.toBeInTheDocument();
    expect(service.register).not.toHaveBeenCalled();
  });

  it('picks the zone from the list when DNSPilot offers it', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true }, [
      { id: 7, name: 'example.com' },
      { id: 8, name: 'example.net' },
    ]);
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    await user.selectOptions(await screen.findByLabelText('Domain'), 'example.net');
    await user.selectOptions(screen.getByLabelText('PC'), '4');
    await user.click(screen.getByRole('button', { name: 'Create tunnel on Laptop' }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('on Laptop, under the domain example.net');
    await user.click(within(dialog).getByRole('button', { name: 'Create tunnel' }));
    expect(service.createTunnel).toHaveBeenCalledWith({ deviceId: 4, zoneId: 8 });
  });

  it('never preselects a device other than this PC', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true }, [{ id: 7, name: 'example.com' }]);
    vi.mocked(service.localDevice).mockResolvedValue(null);
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    expect(await screen.findByLabelText('PC')).toHaveValue('');
    expect(screen.getByRole('button', { name: 'Create tunnel' })).toBeDisabled();
    await user.selectOptions(screen.getByLabelText('PC'), '3');
    expect(screen.getByRole('button', { name: 'Create tunnel on Desk' })).toBeEnabled();
  });

  it('offers no way to create a tunnel on a phone', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true }, null, 'ios');
    vi.mocked(service.localDevice).mockResolvedValue(3);
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    expect(await screen.findByText(/Tunnels are created from the app on your PC/)).toBeVisible();
    expect(screen.queryByRole('button', { name: /^Create tunnel/ })).not.toBeInTheDocument();
    expect(screen.queryByLabelText('PC')).not.toBeInTheDocument();
    expect(service.devices).not.toHaveBeenCalled();
    // Using an existing tunnel stays available.
    expect(screen.getByRole('button', { name: 'Use vca-manual.example.com' })).toBeEnabled();
  });

  it('treats an unknown platform like a phone', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true });
    vi.mocked(service.platform).mockRejectedValue(new Error('no'));
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    expect(await screen.findByText(/Tunnels are created from the app on your PC/)).toBeVisible();
  });

  it('points to the existing tunnel instead of offering a second one', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true });
    vi.mocked(service.remoteTunnels).mockResolvedValue([{ ...MANUAL, deviceId: 3 }]);
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    expect(await screen.findByRole('status')).toHaveTextContent(
      'Desk already has a tunnel to VCA (vca-manual.example.com)',
    );
    expect(screen.queryByRole('button', { name: /^Create tunnel/ })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Use vca-manual.example.com' })).toBeEnabled();
  });

  it('sends one request for a double click, and blocks while busy', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true }, [{ id: 7, name: 'example.com' }]);
    let finish: () => void = () => {};
    vi.mocked(service.createTunnel).mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = () => resolve(tunnel());
        }),
    );
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    await user.click(await screen.findByRole('button', { name: 'Create tunnel on Desk' }));
    const dialog = await screen.findByRole('dialog');
    await user.dblClick(within(dialog).getByRole('button', { name: 'Create tunnel' }));
    expect(service.createTunnel).toHaveBeenCalledTimes(1);
    expect(await screen.findByRole('button', { name: 'Create tunnel on Desk' })).toBeDisabled();
    finish();
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Create tunnel on Desk' })).toBeEnabled(),
    );
    expect(service.createTunnel).toHaveBeenCalledTimes(1);
  });

  it('says when the key could not be registered, and shows the tunnel', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true }, [{ id: 7, name: 'example.com' }]);
    vi.mocked(service.createTunnel).mockRejectedValueOnce(
      new NativeError('dnspilot_key_registration_failed', 'failed'),
    );
    vi.mocked(service.status).mockResolvedValue({
      connected: true,
      scope: null,
      pending: false,
      identity: null,
      tunnels: [tunnel({ clientRegistered: false })],
      consoleUrl: 'https://dns.dnspilot.kr',
    });
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    await user.click(await screen.findByRole('button', { name: 'Create tunnel on Desk' }));
    await user.click(
      within(await screen.findByRole('dialog')).getByRole('button', { name: 'Create tunnel' }),
    );
    expect(await screen.findByRole('alert')).toHaveTextContent('Don’t create another tunnel');
    expect(await screen.findByText('Key not registered')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Register this app on the tunnel' })).toBeEnabled();
  });

  it('warns plainly before disconnecting', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true });
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    await user.click(await screen.findByRole('button', { name: 'Disconnect DNSPilot' }));
    expect(service.disconnect).not.toHaveBeenCalled();
    const dialog = await screen.findByRole('dialog', { name: 'Disconnect DNSPilot?' });
    expect(dialog).toHaveTextContent('turns off every tunnel this app created');
    expect(dialog).toHaveTextContent('a new approval cannot manage those tunnels');
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(service.disconnect).not.toHaveBeenCalled();

    await user.click(screen.getByRole('button', { name: 'Disconnect DNSPilot' }));
    await user.click(screen.getByRole('button', { name: 'Disconnect and turn off tunnels' }));
    expect(service.disconnect).toHaveBeenCalledOnce();
  });

  it('lists registered devices and pending invites, with revoke', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true, tunnels: [tunnel()] });
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    await user.click(await screen.findByRole('button', { name: 'Registered devices' }));
    expect(await screen.findByText('VCA phone')).toBeInTheDocument();
    expect(screen.getByText('invite code')).toBeInTheDocument();
    expect(screen.getByText('VCA app · this app')).toBeInTheDocument();
    expect(await screen.findByText('Tablet')).toBeInTheDocument();
    // Used invites are history, not pending.
    expect(screen.queryByText('Old')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Revoke VCA phone' }));
    expect(service.revokeAppClient).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Revoke now' }));
    expect(service.revokeAppClient).toHaveBeenCalledWith(12, 7);

    await user.click(await screen.findByRole('button', { name: 'Revoke invite Tablet' }));
    await user.click(screen.getByRole('button', { name: 'Revoke now' }));
    expect(service.revokeInvite).toHaveBeenCalledWith(12, 21);
  });

  it('shows friendly errors from the device list', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true, tunnels: [tunnel()] });
    vi.mocked(service.revokeInvite).mockRejectedValueOnce(
      new NativeError('dnspilot_rate_limited', 'limited', 90),
    );
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    await user.click(await screen.findByRole('button', { name: 'Registered devices' }));
    await user.click(await screen.findByRole('button', { name: 'Revoke invite Tablet' }));
    await user.click(screen.getByRole('button', { name: 'Revoke now' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Try again in 90 seconds');
  });

  it('has no device list on a phone', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true, tunnels: [tunnel()] }, null, 'android');
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    expect(await screen.findByText('vca-k3x9q2ab4m.example.com')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Registered devices' })).not.toBeInTheDocument();
  });

  it('reuses an existing tunnel and guides the console registration', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true });
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    await user.click(await screen.findByRole('button', { name: 'Use vca-manual.example.com' }));
    expect(service.useTunnel).toHaveBeenCalledWith(13);

    expect(await screen.findByText(/was not created by VCA/)).toBeInTheDocument();
    expect(screen.getByText(`dnspilot client add 13 --spki ${SPKI}`)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Open DNSPilot console' }));
    expect(service.openConsole).toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'I added it' }));
    expect(await screen.findByText('Ready')).toBeInTheDocument();
    // The app-token fallback only exists under Advanced, for tunnels VCA created.
    expect(screen.queryByText('Advanced')).not.toBeInTheDocument();
  });

  it('keeps the app token under Advanced with a warning', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true, tunnels: [tunnel()] });
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    await user.click(await screen.findByText('Advanced'));
    expect(screen.getByText(/a token alone opens the tunnel/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Issue an app token (7 days)' }));
    expect(service.issueAppToken).toHaveBeenCalledWith('vca-k3x9q2ab4m.example.com');
  });

  it('explains a PC that was not selected on approval', async () => {
    const user = userEvent.setup();
    const { service } = fakeService({ connected: true });
    vi.mocked(service.createTunnel).mockRejectedValueOnce(
      new NativeError('dnspilot_device_not_allowed', 'refused'),
    );
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    await user.type(await screen.findByLabelText('Zone ID'), '7');
    await user.click(screen.getByRole('button', { name: 'Create tunnel on Desk' }));
    await user.click(
      within(await screen.findByRole('dialog')).getByRole('button', { name: 'Create tunnel' }),
    );
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'This PC wasn’t selected when you approved VCA',
    );
  });

  it('shows denied approvals and stays quiet on cancel', async () => {
    const user = userEvent.setup();
    const { service } = fakeService();
    vi.mocked(service.authorizeWait).mockRejectedValueOnce(
      new NativeError('dnspilot_denied', 'denied'),
    );
    renderPanel(service);
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    await user.click(await screen.findByRole('button', { name: 'Connect DNSPilot' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('denied in DNSPilot');

    vi.mocked(service.authorizeWait).mockRejectedValueOnce(
      new NativeError('dnspilot_cancelled', 'cancelled'),
    );
    await user.click(screen.getByRole('button', { name: 'Connect DNSPilot' }));
    await waitFor(() => expect(screen.queryByRole('alert')).not.toBeInTheDocument());
  });
});
