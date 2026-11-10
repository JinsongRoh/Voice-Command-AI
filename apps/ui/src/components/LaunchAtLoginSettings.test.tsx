import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { NativeError } from '../native/bridge';
import { NativeProvider, type NativeServices } from '../native/context';
import type { HostServices } from '../native/host';
import type { LaunchAtLoginServices, LaunchAtLoginStatus } from '../native/launchAtLogin';
import { renderWithApp } from '../test/render';
import { LaunchAtLoginSettings } from './LaunchAtLoginSettings';
import { SettingsView } from './SettingsView';

/** The OS entry, in memory. */
function fakeServices(enabled = false) {
  let on = enabled;
  const status = (): LaunchAtLoginStatus => ({ enabled: on, launched_at_login: false });
  return {
    status: vi.fn(async () => status()),
    set: vi.fn(async (next: boolean) => {
      on = next;
      return status();
    }),
    /** The entry was removed outside VCA (Task Manager, Login Items). */
    removeElsewhere: () => {
      on = false;
    },
  };
}

function toggle() {
  return screen.getByRole('checkbox', { name: 'Start VCA when I sign in' });
}

describe('Start VCA when I sign in', () => {
  it('shows the state read from the OS and is off by default', async () => {
    const services = fakeServices();
    renderWithApp(<LaunchAtLoginSettings services={services} />);
    expect(toggle()).toBeDisabled();
    await waitFor(() => expect(toggle()).toBeEnabled());
    expect(toggle()).not.toBeChecked();
    expect(services.status).toHaveBeenCalledTimes(1);
    expect(services.set).not.toHaveBeenCalled();
  });

  it('turns it on and off', async () => {
    const services = fakeServices();
    renderWithApp(<LaunchAtLoginSettings services={services} />);
    await waitFor(() => expect(toggle()).toBeEnabled());
    await userEvent.click(toggle());
    await waitFor(() => expect(toggle()).toBeChecked());
    expect(services.set).toHaveBeenLastCalledWith(true);
    await userEvent.click(toggle());
    await waitFor(() => expect(toggle()).not.toBeChecked());
    expect(services.set).toHaveBeenLastCalledWith(false);
  });

  it('reads the OS again when the section opens (removed elsewhere)', async () => {
    const services = fakeServices(true);
    const { unmount } = renderWithApp(<LaunchAtLoginSettings services={services} />);
    await waitFor(() => expect(toggle()).toBeChecked());
    unmount();
    services.removeElsewhere();
    renderWithApp(<LaunchAtLoginSettings services={services} />);
    await waitFor(() => expect(toggle()).toBeEnabled());
    expect(toggle()).not.toBeChecked();
  });

  it('reports a failure and keeps the real state', async () => {
    const services: LaunchAtLoginServices = {
      status: vi.fn(async () => ({ enabled: false, launched_at_login: false })),
      set: vi.fn(async () => {
        throw new NativeError('autostart', 'access denied');
      }),
    };
    renderWithApp(<LaunchAtLoginSettings services={services} />);
    await waitFor(() => expect(toggle()).toBeEnabled());
    await userEvent.click(toggle());
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Could not change the startup setting: access denied',
    );
    expect(toggle()).not.toBeChecked();
    expect(toggle()).toBeEnabled();
  });

  it('says so when the OS kept another state', async () => {
    const services: LaunchAtLoginServices = {
      status: vi.fn(async () => ({ enabled: false, launched_at_login: false })),
      set: vi.fn(async () => ({ enabled: false, launched_at_login: false })),
    };
    renderWithApp(<LaunchAtLoginSettings services={services} />);
    await waitFor(() => expect(toggle()).toBeEnabled());
    await userEvent.click(toggle());
    expect(await screen.findByRole('alert')).toHaveTextContent('The system kept');
    expect(toggle()).not.toBeChecked();
  });
});

describe('Settings > General > Startup', () => {
  it('is hidden without the desktop shell (browser, phones)', () => {
    renderWithApp(<SettingsView onPair={() => {}} />);
    expect(screen.getByRole('heading', { name: 'Appearance' })).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'Startup' })).toBeNull();
    expect(screen.queryByTestId('launch-at-login')).toBeNull();
  });

  it('shows the toggle in the desktop app', async () => {
    const services = fakeServices();
    const native = {
      pair: vi.fn(),
      forget: vi.fn(),
      platform: async () => 'windows',
      host: { launchAtLogin: services } as unknown as HostServices,
    } as unknown as NativeServices;
    renderWithApp(
      <NativeProvider value={native}>
        <SettingsView onPair={() => {}} />
      </NativeProvider>,
    );
    expect(screen.getByRole('heading', { name: 'Startup' })).toBeInTheDocument();
    await waitFor(() => expect(toggle()).toBeEnabled());
    expect(services.status).toHaveBeenCalled();
  });
});
