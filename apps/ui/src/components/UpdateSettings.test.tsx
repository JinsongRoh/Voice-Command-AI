import { act, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { NativeError } from '../native/bridge';
import { NativeProvider, type NativeServices } from '../native/context';
import type { HostServices } from '../native/host';
import {
  CHECK_INTERVAL_MS,
  FIRST_CHECK_MS,
  type AvailableUpdate,
  type DownloadEvent,
  type UpdateCheck,
  type UpdatePreflight,
  type UpdaterServices,
} from '../native/updater';
import { renderWithApp } from '../test/render';
import { SettingsView } from './SettingsView';
import { Toasts } from './Toasts';
import { UpdateNotifier, UpdateSettings } from './UpdateSettings';

const UPDATE: AvailableUpdate = {
  version: '0.2.0',
  notes: 'Faster voice commands.\nFixes.',
  date: '2026-11-01T00:00:00Z',
  install: 'auto',
  release_url: 'https://github.com/JinsongRoh/Voice-Command-AI/releases/tag/v0.2.0',
};

function fakeUpdater({
  available = UPDATE as AvailableUpdate | null,
  preflight = { managed_server: true, managed_sessions: 2 } as UpdatePreflight,
} = {}) {
  return {
    check: vi.fn(async (): Promise<UpdateCheck> => ({ current_version: '0.1.0', available })),
    download: vi.fn(async (onEvent: (e: DownloadEvent) => void) => {
      onEvent({ event: 'progress', downloaded: 512, total: 1024 });
      onEvent({ event: 'progress', downloaded: 1024, total: 1024 });
      onEvent({ event: 'finished' });
    }),
    preflight: vi.fn(async () => preflight),
    install: vi.fn(async () => {}),
  } satisfies UpdaterServices;
}

function nativeWith(updater: UpdaterServices | undefined, withHost = true): NativeServices {
  const host = { updater } as unknown as HostServices;
  return {
    pair: vi.fn(),
    forget: vi.fn(),
    platform: async () => 'windows',
    ...(withHost ? { host } : {}),
  } as unknown as NativeServices;
}

function renderSettings(updater: UpdaterServices) {
  return renderWithApp(
    <NativeProvider value={nativeWith(updater)}>
      <UpdateSettings />
    </NativeProvider>,
  );
}

beforeEach(() => localStorage.clear());
afterEach(() => vi.useRealTimers());

describe('UpdateSettings', () => {
  it('reports the latest version', async () => {
    const updater = fakeUpdater({ available: null });
    renderSettings(updater);
    await userEvent.click(screen.getByRole('button', { name: 'Check for updates' }));
    expect(await screen.findByRole('status')).toHaveTextContent(
      'You have the latest version (0.1.0).',
    );
    expect(updater.download).not.toHaveBeenCalled();
  });

  it('downloads on click, then installs only after the restart is confirmed', async () => {
    const updater = fakeUpdater();
    renderSettings(updater);
    await userEvent.click(screen.getByRole('button', { name: 'Check for updates' }));
    expect(await screen.findByText('New version 0.2.0 is available.')).toBeInTheDocument();
    expect(screen.getByTestId('update-notes').textContent).toBe('Faster voice commands.\nFixes.');
    expect(updater.download).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole('button', { name: 'Download update' }));
    expect(await screen.findByRole('status')).toHaveTextContent(
      'Version 0.2.0 is downloaded and its signature checked.',
    );
    expect(updater.install).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole('button', { name: 'Restart to update' }));
    const dialog = await screen.findByRole('dialog');
    const explain = within(dialog).getByTestId('update-confirm');
    expect(explain).toHaveTextContent('Updating may end the sessions VCA started (2 running).');
    expect(explain).toHaveTextContent('The VCA server this app started is stopped first');
    expect(explain).toHaveTextContent('which VCA only observes, are not affected');

    await userEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(updater.install).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole('button', { name: 'Restart to update' }));
    await userEvent.click(
      within(await screen.findByRole('dialog')).getByRole('button', { name: 'Update and restart' }),
    );
    await waitFor(() => expect(updater.install).toHaveBeenCalledTimes(1));
    expect(screen.getByRole('status')).toHaveTextContent('Installing the update');
  });

  it('shows download progress', async () => {
    let finish: () => void = () => {};
    const updater = fakeUpdater();
    updater.download.mockImplementation(
      (onEvent) =>
        new Promise<void>((resolve) => {
          onEvent({ event: 'progress', downloaded: 300, total: 1000 });
          finish = resolve;
        }),
    );
    renderSettings(updater);
    await userEvent.click(screen.getByRole('button', { name: 'Check for updates' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Download update' }));
    expect(await screen.findByRole('status')).toHaveTextContent('Downloading… 30%');
    expect(screen.getByRole('progressbar')).toHaveAttribute('value', '30');
    expect(screen.getByRole('button', { name: 'Check for updates' })).toBeDisabled();
    await act(async () => finish());
    expect(await screen.findByRole('button', { name: 'Restart to update' })).toBeInTheDocument();
  });

  it('without running sessions, the confirmation says nothing about ending them', async () => {
    const updater = fakeUpdater({ preflight: { managed_server: false, managed_sessions: 0 } });
    renderSettings(updater);
    await userEvent.click(screen.getByRole('button', { name: 'Check for updates' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Download update' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Restart to update' }));
    const explain = await screen.findByTestId('update-confirm');
    expect(explain).toHaveTextContent('installs version 0.2.0 and opens again');
    expect(explain).not.toHaveTextContent('may end the sessions');
    expect(explain).not.toHaveTextContent('VCA server this app started');
  });

  it('warns about sessions when the shell cannot tell', async () => {
    const updater = fakeUpdater();
    updater.preflight.mockRejectedValue(new NativeError('unreachable', 'down'));
    renderSettings(updater);
    await userEvent.click(screen.getByRole('button', { name: 'Check for updates' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Download update' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Restart to update' }));
    expect(await screen.findByTestId('update-confirm')).toHaveTextContent(
      'Updating may end the sessions VCA started.',
    );
  });

  it('links the release page when this install cannot update itself (.deb / .rpm)', async () => {
    const updater = fakeUpdater({ available: { ...UPDATE, install: 'manual' } });
    renderSettings(updater);
    await userEvent.click(screen.getByRole('button', { name: 'Check for updates' }));
    expect(await screen.findByText('New version 0.2.0 is available.')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Download update' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Restart to update' })).not.toBeInTheDocument();
    expect(screen.getByText(/can't update itself/)).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Open the release page' })).toHaveAttribute(
      'href',
      UPDATE.release_url,
    );
  });

  it('shows errors plainly and retries', async () => {
    const updater = fakeUpdater();
    updater.check.mockRejectedValueOnce(new NativeError('update_offline', 'dns error'));
    renderSettings(updater);
    await userEvent.click(screen.getByRole('button', { name: 'Check for updates' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      "Couldn't reach GitHub. Check the internet connection and try again.",
    );
    await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    expect(await screen.findByText('New version 0.2.0 is available.')).toBeInTheDocument();

    updater.download.mockRejectedValueOnce(new NativeError('update_signature', 'bad sig'));
    await userEvent.click(screen.getByRole('button', { name: 'Download update' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      "The update's signature is invalid, so it was not installed.",
    );
    expect(updater.install).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    expect(await screen.findByRole('button', { name: 'Restart to update' })).toBeInTheDocument();
    expect(updater.download).toHaveBeenCalledTimes(2);
  });

  it('remembers the automatic check choice (on by default)', async () => {
    renderSettings(fakeUpdater());
    const toggle = screen.getByRole('checkbox', { name: 'Check for updates automatically' });
    expect(toggle).toBeChecked();
    await userEvent.click(toggle);
    expect(toggle).not.toBeChecked();
    expect(localStorage.getItem('vca.updates.autoCheck')).toBe('false');
  });

  it('is hidden on phones and in the browser', () => {
    // Phones: the shell gives no host services (so no updater).
    localStorage.setItem('vca.settings.tab', 'about');
    const { unmount } = renderWithApp(
      <NativeProvider value={nativeWith(undefined, false)}>
        <SettingsView onPair={() => {}} />
      </NativeProvider>,
    );
    expect(screen.getByRole('region', { name: 'About' })).toBeInTheDocument();
    expect(screen.queryByTestId('update-settings')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Check for updates' })).not.toBeInTheDocument();
    unmount();
    // Browser mock: no native services at all.
    renderWithApp(<SettingsView onPair={() => {}} />);
    expect(screen.queryByTestId('update-settings')).not.toBeInTheDocument();
  });

  it('appears in Settings → About on the desktop app', async () => {
    renderWithApp(
      <NativeProvider value={nativeWith(fakeUpdater())}>
        <SettingsView onPair={() => {}} />
      </NativeProvider>,
    );
    await userEvent.click(screen.getByRole('tab', { name: 'About' }));
    const about = screen.getByRole('region', { name: 'About' });
    expect(within(about).getByTestId('update-settings')).toBeInTheDocument();
  });
});

describe('UpdateNotifier', () => {
  function renderNotifier(updater: UpdaterServices, onOpenSettings = vi.fn()) {
    renderWithApp(
      <NativeProvider value={nativeWith(updater)}>
        <UpdateNotifier onOpenSettings={onOpenSettings} />
        <Toasts onOpen={() => {}} />
      </NativeProvider>,
    );
    return onOpenSettings;
  }

  it('checks 30 s after start and daily, announces once, and never installs', async () => {
    vi.useFakeTimers();
    const updater = fakeUpdater();
    const onOpenSettings = renderNotifier(updater);
    await act(async () => vi.advanceTimersByTimeAsync(FIRST_CHECK_MS - 1000));
    expect(updater.check).not.toHaveBeenCalled();
    await act(async () => vi.advanceTimersByTimeAsync(1000));
    expect(updater.check).toHaveBeenCalledTimes(1);
    expect(screen.getByText('New version 0.2.0 is available')).toBeInTheDocument();

    screen.getByRole('button', { name: 'Settings' }).click();
    expect(onOpenSettings).toHaveBeenCalledTimes(1);

    await act(async () => vi.advanceTimersByTimeAsync(CHECK_INTERVAL_MS));
    expect(updater.check).toHaveBeenCalledTimes(2);
    // The same version is not announced again.
    expect(screen.queryByText('New version 0.2.0 is available')).not.toBeInTheDocument();
    expect(updater.download).not.toHaveBeenCalled();
    expect(updater.install).not.toHaveBeenCalled();
  });

  it('does not check when automatic checks are off', async () => {
    localStorage.setItem('vca.updates.autoCheck', 'false');
    vi.useFakeTimers();
    const updater = fakeUpdater();
    renderNotifier(updater);
    await act(async () => vi.advanceTimersByTimeAsync(FIRST_CHECK_MS + CHECK_INTERVAL_MS));
    expect(updater.check).not.toHaveBeenCalled();
  });

  it('stays silent when the check fails', async () => {
    vi.useFakeTimers();
    const updater = fakeUpdater();
    updater.check.mockRejectedValue(new NativeError('update_offline', 'offline'));
    renderNotifier(updater);
    await act(async () => vi.advanceTimersByTimeAsync(FIRST_CHECK_MS));
    expect(updater.check).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('listitem')).not.toBeInTheDocument();
  });

  it('does nothing without an updater (phones, browser)', async () => {
    vi.useFakeTimers();
    renderWithApp(
      <NativeProvider value={nativeWith(undefined, false)}>
        <UpdateNotifier onOpenSettings={() => {}} />
      </NativeProvider>,
    );
    await act(async () => vi.advanceTimersByTimeAsync(FIRST_CHECK_MS));
    expect(screen.queryByRole('listitem')).not.toBeInTheDocument();
  });
});
