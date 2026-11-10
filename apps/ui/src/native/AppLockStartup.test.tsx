/**
 * The app lock across the desktop startup sequence: the machine list starts
 * empty (pairing screen), this PC appears once its server answers, the list
 * may briefly empty again while the shell settles and the platform is known
 * late. The person verifies once per app start, whatever mounts and unmounts.
 */
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { SpeechServices } from '../speech/context';
import { requestConductorView } from '../state/conductor';
import { lockSession } from '../state/lockSession';
import { defaultSettings } from '../state/settings';
import { fakeServer, tauri } from '../test/tauriMock';
import type { AppLockVerifier, UnlockResult } from './appLock';
import { NativeError, type MachineInfo } from './bridge';
import { HOST_CHANGED_EVENT, LOCAL_MACHINE_ID, type HostStatus } from './host';
import { NativeApp } from './NativeApp';
import { MachineRegistry } from './registry';

vi.mock('@tauri-apps/api/core', () => import('../test/tauriMock'));
vi.mock('@tauri-apps/plugin-deep-link', () => ({
  getCurrent: async () => null,
  onOpenUrl: async () => () => {},
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: async (event: string, handler: (e: { payload: unknown }) => void) => {
    events.listeners.push({ event, handler });
    return () => {
      events.listeners = events.listeners.filter((l) => l.handler !== handler);
    };
  },
}));

const events: { listeners: { event: string; handler: (e: { payload: unknown }) => void }[] } = {
  listeners: [],
};

/** The shell reports that this PC's server came up or went away. */
async function hostChanged(up: boolean) {
  await act(async () => {
    for (const l of events.listeners.filter((x) => x.event === HOST_CHANGED_EVENT)) {
      l.handler({ payload: up });
    }
    // Let the registry reload settle.
    await new Promise((r) => setTimeout(r, 0));
  });
}

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

const RUNNING: HostStatus = {
  running: true,
  endpoint: '\\\\.\\pipe\\vca-control-me',
  server_name: 'studio-pc',
  version: '1.0.0',
  remote: null,
  managed: true,
  can_start: true,
  run_server: true,
  log_path: null,
};

/** A verifier whose answers the test hands out one by one. */
function fakeVerifier() {
  const answers: { resolve: (r: UnlockResult) => void; reject: (e: unknown) => void }[] = [];
  const verify = vi.fn(
    () =>
      new Promise<UnlockResult>((resolve, reject) => {
        answers.push({ resolve, reject });
      }),
  );
  const verifier: AppLockVerifier = { verify };
  const next = async (answer: UnlockResult | NativeError = 'verified') => {
    await waitFor(() => expect(answers.length).toBeGreaterThan(0));
    const a = answers.shift()!;
    await act(async () => {
      if (answer instanceof NativeError) a.reject(answer);
      else a.resolve(answer);
    });
  };
  return { verifier, verify, next };
}

describe('App lock during the desktop startup', () => {
  let machines: MachineInfo[];
  let resolvePlatform: (p: string) => void;

  beforeEach(() => {
    tauri.reset();
    events.listeners = [];
    machines = [];
    tauri.handlers.set('vca_machines', () => machines);
    // The platform is known only after the first render.
    const platform = new Promise<string>((resolve) => {
      resolvePlatform = resolve;
    });
    tauri.handlers.set('vca_platform', () => platform);
    tauri.handlers.set('vca_host_status', () => RUNNING);
    fakeServer();
  });

  /** NativeApp as main.tsx builds it: one registry, one verifier, per app start. */
  async function start(verifier: AppLockVerifier) {
    render(
      <NativeApp
        registry={new MachineRegistry()}
        speech={speech}
        settings={{ ...defaultSettings(), language: 'en' }}
        appLock={verifier}
      />,
    );
    // No machine yet: the pairing screen, which is not locked.
    expect(await screen.findByRole('heading', { name: 'Pair a device' })).toBeInTheDocument();
    await act(async () => resolvePlatform('windows'));
    await waitFor(() => expect(events.listeners.length).toBeGreaterThan(0));
    // This PC's server answers: it appears as a machine, the app locks.
    machines = [LOCAL];
    await hostChanged(true);
    expect(await screen.findByTestId('app-lock')).toBeInTheDocument();
  }

  /** The shell settles: the list briefly empties and this PC comes back. */
  async function bounce() {
    machines = [];
    await hostChanged(false);
    // Everything under the machine store unmounted, the lock provider too.
    expect(
      await screen.findByRole('heading', { name: 'Add a computer (paste link)' }),
    ).toBeInTheDocument();
    machines = [LOCAL];
    await hostChanged(true);
  }

  it('asks once per app start, even when the tree mounts again while the prompt is open', async () => {
    const { verifier, verify, next } = fakeVerifier();
    await start(verifier);
    await waitFor(() => expect(verify).toHaveBeenCalledTimes(1));

    // The lock screen mounts again with the prompt still up: it follows it.
    await bounce();
    expect(await screen.findByTestId('app-lock')).toBeInTheDocument();
    expect(verify).toHaveBeenCalledTimes(1);

    await next('verified');
    await waitFor(() => expect(screen.queryByTestId('app-lock')).toBeNull());

    // Mounting again after the unlock does not lock again.
    await bounce();
    expect(screen.queryByTestId('app-lock')).toBeNull();
    expect(await screen.findByRole('navigation')).toBeInTheDocument();
    expect(verify).toHaveBeenCalledTimes(1);
  });

  it('does not start a second prompt by itself after a cancelled one', async () => {
    const { verifier, verify, next } = fakeVerifier();
    await start(verifier);
    await next(new NativeError('verification_canceled', 'closed'));
    await bounce();
    expect(await screen.findByTestId('app-lock')).toBeInTheDocument();
    // Only the person's click asks again.
    expect(verify).toHaveBeenCalledTimes(1);
    await userEvent.click(screen.getByRole('button', { name: 'Unlock' }));
    expect(verify).toHaveBeenCalledTimes(2);
    await next('verified');
    await waitFor(() => expect(screen.queryByTestId('app-lock')).toBeNull());
  });

  it('opens the Command Center at launch with one prompt in total', async () => {
    const { verifier, verify, next } = fakeVerifier();
    await start(verifier);
    await next('verified');
    await waitFor(() => expect(screen.queryByTestId('app-lock')).toBeNull());
    // Straight into the Command Center, then again after a remount.
    act(() => requestConductorView());
    expect(screen.queryByTestId('command-center-lock')).toBeNull();
    expect(screen.getByTestId('conductor-view')).toBeInTheDocument();
    await bounce();
    expect(await screen.findByRole('navigation')).toBeInTheDocument();
    act(() => requestConductorView());
    expect(screen.queryByTestId('command-center-lock')).toBeNull();
    expect(screen.queryByTestId('app-lock')).toBeNull();
    expect(screen.getByTestId('conductor-view')).toBeInTheDocument();
    expect(verify).toHaveBeenCalledTimes(1);
  });

  it('keeps one prompt when Unlock is clicked while the first one is open', async () => {
    const { verifier, verify, next } = fakeVerifier();
    await start(verifier);
    await waitFor(() => expect(verify).toHaveBeenCalledTimes(1));
    const lock = screen.getByTestId('app-lock');
    fireEvent.click(within(lock).getByRole('button', { name: 'Unlock' }));
    await bounce();
    fireEvent.click(within(screen.getByTestId('app-lock')).getByRole('button', { name: 'Unlock' }));
    expect(verify).toHaveBeenCalledTimes(1);
    await next('verified');
    await waitFor(() => expect(screen.queryByTestId('app-lock')).toBeNull());
    expect(verify).toHaveBeenCalledTimes(1);
  });
});

describe('lockSession', () => {
  it('runs one verification at a time and records the unlock', async () => {
    const { verifier, verify, next } = fakeVerifier();
    const session = lockSession(verifier);
    expect(lockSession(verifier)).toBe(session);
    session.decide(true);
    session.decide(false);
    expect(session.getSnapshot().locked).toBe(true);

    const first = session.verify({ reason: 'Unlock', language: 'en' }, 'unlock');
    const second = session.verify({ reason: 'Unlock', language: 'en' }, 'unlock');
    expect(session.verifying).toBe(true);
    await next('verified');
    await expect(first).resolves.toBe('verified');
    await expect(second).resolves.toBe('verified');
    expect(verify).toHaveBeenCalledTimes(1);
    expect(session.verifying).toBe(false);
    expect(session.getSnapshot()).toMatchObject({
      locked: false,
      lastUnlockAt: expect.any(Number),
    });
  });

  it('tries by itself once per lock', () => {
    const { verifier } = fakeVerifier();
    const session = lockSession(verifier);
    session.decide(true);
    expect(session.claimAutoUnlock()).toBe(true);
    expect(session.claimAutoUnlock()).toBe(false);
    session.setLocked(false);
    session.setLocked(true);
    expect(session.claimAutoUnlock()).toBe(true);
  });
});
