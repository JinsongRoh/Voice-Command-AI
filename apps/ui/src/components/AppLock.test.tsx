import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { App } from '../App';
import {
  appWindowActive,
  desktopAppLock,
  isWindowsWebView,
  nativeAppLock,
  phoneAppLock,
  simulatedAppLock,
  type AppLockVerifier,
  type UnlockResult,
} from '../native/appLock';
import { NativeError } from '../native/bridge';
import type { NativeServices } from '../native/context';
import { defaultConductorState } from '../protocol/mockConductor';
import { MockTransport } from '../protocol/mockTransport';
import { setDictationActive } from '../speech/handsFree';
import { isAppLocked, unlockFresh } from '../state/appLock';
import { defaultSettings, parseSettings, type Settings } from '../state/settings';
import { fakeSecurity, securityStatus } from '../test/fakeSecurity';

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

function renderApp(
  verifier: AppLockVerifier | null,
  settings: Partial<Settings> = {},
  { conductorOn = false, native }: { conductorOn?: boolean; native?: NativeServices } = {},
) {
  const transport = new MockTransport({
    latencyMs: 0,
    autoTick: false,
    // The Command Center guards nothing while the conductor is off.
    ...(conductorOn
      ? {
          conductor: {
            seed: {
              settings: { ...defaultConductorState().settings, enabled: true },
              status: 'ready' as const,
            },
          },
        }
      : {}),
  });
  return render(
    <App
      machines={[{ id: 'm1', name: 'workstation', connectivity: 'LAN', transport }]}
      speech={{
        transcribe: async () => ({ text: '', corrections: [] }),
        createRecorder: () => ({
          start: async () => {},
          stop: async () => new Blob(),
          cancel: () => {},
        }),
        fallbackWithoutMic: false,
      }}
      settings={{ ...defaultSettings(), language: 'en', ...settings }}
      appLock={verifier}
      native={native}
    />,
  );
}

/** Nothing of the app is on screen: no sessions, menu, composer or decisions. */
function expectNothingOfTheApp() {
  expect(screen.queryAllByTestId(/^session-card-/)).toHaveLength(0);
  expect(screen.queryByRole('navigation')).toBeNull();
  expect(screen.queryByRole('textbox')).toBeNull();
  expect(screen.queryByRole('region', { name: /Pending decision/ })).toBeNull();
  expect(screen.queryByText('payments-api')).toBeNull();
}

afterEach(() => {
  vi.useRealTimers();
  localStorage.clear();
});

describe('App lock', () => {
  it('is locked at launch and starts the verification once by itself', async () => {
    const { verifier, verify, next } = fakeVerifier();
    renderApp(verifier);
    const lock = screen.getByTestId('app-lock');
    expect(within(lock).getByRole('heading', { name: 'Voice Command AI' })).toBeInTheDocument();
    expect(within(lock).getByRole('button', { name: 'Unlock' })).toBeInTheDocument();
    expectNothingOfTheApp();
    await waitFor(() => expect(verify).toHaveBeenCalledTimes(1));
    expect(verify.mock.calls[0]).toEqual([
      expect.objectContaining({ reason: 'Unlock Voice Command AI', language: 'en' }),
    ]);
    expect(isAppLocked()).toBe(true);

    await next('verified');
    expect(await screen.findByTestId('session-card-wk-claude-api')).toBeInTheDocument();
    expect(screen.queryByTestId('app-lock')).toBeNull();
    expect(isAppLocked()).toBe(false);
  });

  it('renders nothing of the app while locked, also after a failed try', async () => {
    const { verifier, verify, next } = fakeVerifier();
    renderApp(verifier);
    await next(new NativeError('verification_canceled', 'closed'));
    expect(await screen.findByRole('alert')).toHaveTextContent('Verification was canceled');
    expectNothingOfTheApp();
    // Only the first try starts by itself; the button tries again.
    expect(verify).toHaveBeenCalledTimes(1);
    await userEvent.click(screen.getByRole('button', { name: 'Unlock' }));
    expect(verify).toHaveBeenCalledTimes(2);
    await next('verified');
    expect(await screen.findByTestId('session-card-wk-claude-api')).toBeInTheDocument();
  });

  it('asks for the authenticator code where the PC has no OS verifier', async () => {
    const { verifier, verify, next } = fakeVerifier();
    renderApp(verifier);
    await next(new NativeError('totp_required', 'code'));
    const input = await screen.findByLabelText(/6-digit code/);
    await userEvent.type(input, '123 456');
    await userEvent.click(screen.getByRole('button', { name: 'Unlock' }));
    expect(verify).toHaveBeenLastCalledWith(expect.objectContaining({ totp: '123456' }));
    await next('verified');
    expect(await screen.findByTestId('session-card-wk-claude-api')).toBeInTheDocument();
  });

  it('offers recovery of a lost authenticator on the lock screen, through the server', async () => {
    const { verifier, next } = fakeVerifier();
    const { services } = fakeSecurity(
      securityStatus({ verifier: { kind: 'none', available: false, detail: null } }),
    );
    const native = { security: services } as unknown as NativeServices;
    renderApp(verifier, {}, { native });
    await next(new NativeError('totp_required', 'code'));
    const lost = await screen.findByRole('button', { name: 'Lost your authenticator app?' });
    expect(screen.queryByTestId('security-recovery')).toBeNull();
    await userEvent.click(lost);
    expect(lost).toHaveAttribute('aria-expanded', 'true');
    // The same recovery as Settings > Security: a backup code, checked by the server.
    const recovery = await screen.findByTestId('security-recovery');
    expect(recovery).toHaveTextContent(/one of your backup codes/);
    await userEvent.click(
      within(recovery).getByRole('button', { name: 'I lost my authenticator app' }),
    );
    expect(within(recovery).getByLabelText('Backup code')).toBeInTheDocument();
    // Still locked: none of the app is there.
    expect(screen.queryAllByTestId(/^session-card-/)).toHaveLength(0);
    expect(screen.queryByRole('navigation')).toBeNull();
  });

  it('keeps recovery off the lock screen where the OS verifies (no code asked)', async () => {
    const { verifier, next } = fakeVerifier();
    const { services } = fakeSecurity();
    renderApp(verifier, {}, { native: { security: services } as unknown as NativeServices });
    await next(new NativeError('verification_canceled', 'closed'));
    await screen.findByRole('alert');
    expect(screen.queryByRole('button', { name: 'Lost your authenticator app?' })).toBeNull();
  });

  it('opens with a clear warning where nothing can verify the person', async () => {
    const { verifier, next } = fakeVerifier();
    renderApp(verifier);
    await next('unprotected');
    expect(await screen.findByRole('alert')).toHaveTextContent('anyone holding it can open');
    expectNothingOfTheApp();
    await userEvent.click(screen.getByRole('button', { name: 'Open without verification' }));
    expect(await screen.findByTestId('session-card-wk-claude-api')).toBeInTheDocument();
  });

  it('is off without a verifier (browser build) and when turned off', async () => {
    renderApp(null);
    expect(await screen.findByTestId('session-card-wk-claude-api')).toBeInTheDocument();
    expect(screen.queryByTestId('app-lock')).toBeNull();
  });

  it('does not lock at launch when turned off in Settings', async () => {
    const { verifier, verify } = fakeVerifier();
    renderApp(verifier, { appLock: false });
    expect(await screen.findByTestId('session-card-wk-claude-api')).toBeInTheDocument();
    expect(verify).not.toHaveBeenCalled();
  });

  it('locks again after the idle time', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const { verifier, verify, next } = fakeVerifier();
    renderApp(verifier, { appLockIdleMin: 1 });
    await next('verified');
    await screen.findByTestId('session-card-wk-claude-api');

    // Activity keeps it open...
    act(() => vi.advanceTimersByTime(40_000));
    fireEvent.pointerDown(window);
    act(() => vi.advanceTimersByTime(40_000));
    expect(screen.queryByTestId('app-lock')).toBeNull();
    // ...a minute without any locks it.
    act(() => vi.advanceTimersByTime(25_000));
    expect(screen.getByTestId('app-lock')).toBeInTheDocument();
    expectNothingOfTheApp();
    // Nobody is there: the prompt waits for the person instead of running
    // out unanswered (and asking for a second sign-in later).
    act(() => vi.advanceTimersByTime(60_000));
    expect(verify).toHaveBeenCalledTimes(1);
    fireEvent.pointerMove(window);
    await waitFor(() => expect(verify).toHaveBeenCalledTimes(2));
    fireEvent.pointerMove(window);
    fireEvent.keyDown(window);
    await next('verified');
    await screen.findByTestId('session-card-wk-claude-api');
    expect(verify).toHaveBeenCalledTimes(2);
  });

  it('counts speaking to the app (dictation, a Voco message) as use, not only taps', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const { verifier, next } = fakeVerifier();
    renderApp(verifier, { appLockIdleMin: 1 });
    await next('verified');
    await screen.findByTestId('session-card-wk-claude-api');
    try {
      // A dictation longer than the idle time: still open.
      setDictationActive(true);
      act(() => vi.advanceTimersByTime(3 * 60_000));
      expect(screen.queryByTestId('app-lock')).toBeNull();
      // The idle time counts from its end.
      setDictationActive(false);
      act(() => vi.advanceTimersByTime(40_000));
      expect(screen.queryByTestId('app-lock')).toBeNull();
      act(() => vi.advanceTimersByTime(25_000));
      expect(screen.getByTestId('app-lock')).toBeInTheDocument();
    } finally {
      setDictationActive(false);
    }
  });

  it('locks when the app comes back from the background after the idle time', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const { verifier, next } = fakeVerifier();
    renderApp(verifier, { appLockIdleMin: 5 });
    await next('verified');
    await screen.findByTestId('session-card-wk-claude-api');
    // Timers may not run in the background: only the clock moves.
    vi.setSystemTime(Date.now() + 6 * 60_000);
    act(() => {
      document.dispatchEvent(new Event('visibilitychange'));
    });
    expect(screen.getByTestId('app-lock')).toBeInTheDocument();
  });

  it('asks again on entering the Command Center when the last unlock is older than 5 minutes', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const { verifier, verify, next } = fakeVerifier();
    renderApp(verifier, { appLockIdleMin: 60 }, { conductorOn: true });
    await next('verified');
    await screen.findByTestId('session-card-wk-claude-api');

    // Fresh (just unlocked): straight in.
    await user.click(screen.getAllByRole('button', { name: /^Command/ })[0]!);
    expect(screen.queryByTestId('command-center-lock')).toBeNull();
    await user.click(screen.getAllByRole('button', { name: /^Sessions/ })[0]!);

    // Six minutes later: a new verification before anything shows.
    act(() => vi.advanceTimersByTime(6 * 60_000));
    await user.click(screen.getAllByRole('button', { name: /^Command/ })[0]!);
    const gate = screen.getByTestId('command-center-lock');
    expect(within(gate).getByText('The Command Center is locked')).toBeInTheDocument();
    await waitFor(() => expect(verify).toHaveBeenCalledTimes(2));
    expect(verify).toHaveBeenLastCalledWith(
      expect.objectContaining({ reason: 'Open the Command Center' }),
    );
    await next('verified');
    expect(screen.queryByTestId('command-center-lock')).toBeNull();
  });

  it('does not ask for a verification just to say the conductor is off', async () => {
    const { verifier, verify } = fakeVerifier();
    renderApp(verifier, { appLock: false });
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(screen.getAllByRole('button', { name: /^Command/ })[0]!);
    expect(await screen.findByTestId('conductor-off')).toBeInTheDocument();
    expect(screen.queryByTestId('command-center-lock')).toBeNull();
    expect(verify).not.toHaveBeenCalled();
  });

  it('guards the Command Center even with the app lock off', async () => {
    const { verifier, verify, next } = fakeVerifier();
    renderApp(verifier, { appLock: false }, { conductorOn: true });
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(screen.getAllByRole('button', { name: /^Command/ })[0]!);
    expect(screen.getByTestId('command-center-lock')).toBeInTheDocument();
    await waitFor(() => expect(verify).toHaveBeenCalledTimes(1));
    await next(new NativeError('verification_canceled', 'closed'));
    expect(screen.getByTestId('command-center-lock')).toBeInTheDocument();
  });

  it('needs a verification to turn the lock off in Settings', async () => {
    const { verifier, verify, next } = fakeVerifier();
    renderApp(verifier);
    await next('verified');
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(screen.getAllByRole('button', { name: 'Settings' })[0]!);
    await userEvent.click(await screen.findByRole('tab', { name: 'Security' }));
    const toggle = await screen.findByRole('checkbox', { name: 'Lock the app' });
    expect(toggle).toBeChecked();
    expect(screen.getByRole('combobox', { name: 'Lock after' })).toHaveValue('15');

    // Cancelled: stays on.
    await userEvent.click(toggle);
    const dialog = await screen.findByTestId('app-lock-verify');
    expect(within(dialog).getByText('Turn off the app lock')).toBeInTheDocument();
    await waitFor(() => expect(verify).toHaveBeenCalledTimes(2));
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByTestId('app-lock-verify')).toBeNull();
    expect(toggle).toBeChecked();

    // Verified: off. The OS prompt of the cancelled dialog is still open (the
    // dialog cannot close it): the new dialog follows it, no second prompt.
    await userEvent.click(toggle);
    await screen.findByTestId('app-lock-verify');
    await next('verified');
    await waitFor(() => expect(toggle).not.toBeChecked());
    // Turning it back on needs nothing.
    await userEvent.click(toggle);
    expect(toggle).toBeChecked();
    expect(verify).toHaveBeenCalledTimes(2);
  });

  it('hides the App lock section without a verifier', async () => {
    renderApp(null);
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(screen.getAllByRole('button', { name: 'Settings' })[0]!);
    await userEvent.click(await screen.findByRole('tab', { name: 'Security' }));
    expect(screen.queryByRole('checkbox', { name: 'Lock the app' })).toBeNull();
  });

  it('waits for the window to be in front before asking by itself (Windows cancels otherwise)', async () => {
    const { verifier: base, verify, next } = fakeVerifier();
    let front = false;
    const verifier: AppLockVerifier = { ...base, promptable: () => front };
    renderApp(verifier);
    expect(screen.getByTestId('app-lock')).toBeInTheDocument();
    // The app started behind other windows: no prompt that Windows would cancel.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(verify).not.toHaveBeenCalled();
    fireEvent.pointerMove(window);
    expect(verify).not.toHaveBeenCalled();
    // It comes to the front: one prompt, however many focus events follow.
    front = true;
    act(() => {
      window.dispatchEvent(new Event('focus'));
      window.dispatchEvent(new Event('focus'));
      document.dispatchEvent(new Event('visibilitychange'));
    });
    await waitFor(() => expect(verify).toHaveBeenCalledTimes(1));
    await next('verified');
    expect(await screen.findByTestId('session-card-wk-claude-api')).toBeInTheDocument();
    expect(verify).toHaveBeenCalledTimes(1);
  });

  it('Windows: no prompt by itself (it would pin the window); Unlock, focused, asks once', async () => {
    const { verifier: base, verify, next } = fakeVerifier();
    const verifier: AppLockVerifier = { ...base, promptable: () => true, autoPrompt: false };
    renderApp(verifier);
    expect(screen.getByTestId('app-lock')).toBeInTheDocument();
    act(() => {
      window.dispatchEvent(new Event('focus'));
      fireEvent.pointerMove(window);
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(verify).not.toHaveBeenCalled();
    const unlock = screen.getByRole('button', { name: 'Unlock' });
    expect(unlock).toHaveFocus();
    await userEvent.keyboard('{Enter}');
    await waitFor(() => expect(verify).toHaveBeenCalledTimes(1));
    await next('verified');
    expect(await screen.findByTestId('session-card-wk-claude-api')).toBeInTheDocument();
  });

  it('one sign-in unlocks: the Windows dialog closing (focus, visibility) neither locks nor asks again', async () => {
    const { verifier: base, verify, next } = fakeVerifier();
    const verifier: AppLockVerifier = { ...base, promptable: () => true };
    renderApp(verifier, {}, { conductorOn: true });
    await waitFor(() => expect(verify).toHaveBeenCalledTimes(1));
    await next('verified');
    expect(await screen.findByTestId('session-card-wk-claude-api')).toBeInTheDocument();
    act(() => {
      window.dispatchEvent(new Event('blur'));
      document.dispatchEvent(new Event('visibilitychange'));
      window.dispatchEvent(new Event('focus'));
      fireEvent.pointerMove(window);
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    expect(screen.queryByTestId('app-lock')).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
    expect(verify).toHaveBeenCalledTimes(1);
  });

  it('a prompt that ran out while the window was in the back shows no error and asks once more on return', async () => {
    const { verifier: base, verify, next } = fakeVerifier();
    let front = true;
    const verifier: AppLockVerifier = { ...base, promptable: () => front };
    renderApp(verifier);
    await waitFor(() => expect(verify).toHaveBeenCalledTimes(1));
    // The PC was locked (or the app sent to the back) while the prompt was up.
    front = false;
    await next(new NativeError('verification_canceled', 'closed'));
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByTestId('app-lock')).toBeInTheDocument();
    expect(verify).toHaveBeenCalledTimes(1);
    // The person is back: exactly one more prompt, which unlocks.
    front = true;
    act(() => {
      window.dispatchEvent(new Event('focus'));
      fireEvent.pointerMove(window);
    });
    await waitFor(() => expect(verify).toHaveBeenCalledTimes(2));
    await next('verified');
    expect(await screen.findByTestId('session-card-wk-claude-api')).toBeInTheDocument();
    expect(verify).toHaveBeenCalledTimes(2);
  });

  it('a prompt canceled in front of the person still says so (no prompt loop)', async () => {
    const { verifier: base, verify, next } = fakeVerifier();
    const verifier: AppLockVerifier = { ...base, promptable: () => true };
    renderApp(verifier);
    await next(new NativeError('verification_canceled', 'closed'));
    expect(await screen.findByRole('alert')).toHaveTextContent('Verification was canceled');
    act(() => {
      window.dispatchEvent(new Event('focus'));
      fireEvent.pointerMove(window);
    });
    expect(verify).toHaveBeenCalledTimes(1);
  });

  it('locks at once with "Lock now"', async () => {
    const { verifier, next } = fakeVerifier();
    renderApp(verifier);
    await next('verified');
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(screen.getAllByRole('button', { name: 'Settings' })[0]!);
    await userEvent.click(await screen.findByRole('tab', { name: 'Security' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Lock now' }));
    expect(screen.getByTestId('app-lock')).toBeInTheDocument();
    expectNothingOfTheApp();
  });
});

describe('App lock verifiers', () => {
  it('native: an OS prompt is offered only while the window is shown and focused', async () => {
    const doc = (visibilityState: string, focused: boolean) =>
      ({ visibilityState, hasFocus: () => focused }) as unknown as Document;
    expect(appWindowActive(doc('visible', true))).toBe(true);
    expect(appWindowActive(doc('visible', false))).toBe(false);
    expect(appWindowActive(doc('hidden', true))).toBe(false);
    let active = false;
    const lock = nativeAppLock(
      async () => 'windows',
      () => active,
    );
    expect(lock.promptable?.()).toBe(false);
    active = true;
    expect(lock.promptable?.()).toBe(true);
  });

  it('native: Windows waits for Unlock; other desktops and phones ask by themselves', () => {
    expect(
      nativeAppLock(
        async () => 'windows',
        () => true,
        true,
      ).autoPrompt,
    ).toBe(false);
    expect(
      nativeAppLock(
        async () => 'macos',
        () => true,
        false,
      ).autoPrompt,
    ).toBe(true);
    const ua = (userAgent: string) => ({ userAgent }) as Navigator;
    expect(
      isWindowsWebView(ua('Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/130')),
    ).toBe(true);
    expect(isWindowsWebView(ua('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)'))).toBe(false);
    expect(isWindowsWebView(ua('Mozilla/5.0 (Linux; Android 14)'))).toBe(false);
  });

  it('desktop: Windows Hello / Touch ID through the server, the code without them', async () => {
    const stepUpTest = vi.fn(async () => {});
    const lock = desktopAppLock({ security: { stepUpTest }, wait: async () => {} });
    await expect(lock.verify({ reason: 'r', language: 'ko' })).resolves.toBe('verified');
    expect(stepUpTest).toHaveBeenCalledWith({ language: 'ko' });
    await lock.verify({ reason: 'r', language: 'en', totp: '123456' });
    expect(stepUpTest).toHaveBeenLastCalledWith({ language: 'en', totp: '123456' });
  });

  it('desktop: asks the server to unlock the app, the test prompt only on older servers', async () => {
    const stepUpTest = vi.fn(async () => {});
    const appUnlock = vi.fn(async () => {});
    const lock = desktopAppLock({ security: { stepUpTest }, appUnlock, wait: async () => {} });
    await expect(lock.verify({ reason: 'r', language: 'ko' })).resolves.toBe('verified');
    expect(appUnlock).toHaveBeenCalledWith({ language: 'ko' });
    await lock.verify({ reason: 'r', language: 'en', totp: '123456' });
    expect(appUnlock).toHaveBeenLastCalledWith({ language: 'en', totp: '123456' });
    expect(stepUpTest).not.toHaveBeenCalled();

    // An older vcad does not know `app_unlock`: the test call, from then on.
    const old = vi.fn(async () => {
      throw new NativeError('invalid_request', 'unknown variant `app_unlock`');
    });
    const legacy = desktopAppLock({
      security: { stepUpTest },
      appUnlock: old,
      wait: async () => {},
    });
    await expect(legacy.verify({ reason: 'r', language: 'en' })).resolves.toBe('verified');
    await legacy.verify({ reason: 'r', language: 'en' });
    expect(old).toHaveBeenCalledTimes(1);
    expect(stepUpTest).toHaveBeenCalledTimes(2);

    // A real refusal is not a reason to fall back.
    const canceled = desktopAppLock({
      security: { stepUpTest },
      appUnlock: async () => {
        throw new NativeError('verification_canceled', 'no');
      },
      wait: async () => {},
    });
    await expect(canceled.verify({ reason: 'r', language: 'en' })).rejects.toMatchObject({
      code: 'verification_canceled',
    });
    expect(stepUpTest).toHaveBeenCalledTimes(2);
  });

  it('desktop: nothing to verify with opens unprotected; other failures stay locked', async () => {
    const fail = (code: string) =>
      desktopAppLock({
        security: {
          stepUpTest: async () => {
            throw new NativeError(code, code);
          },
        },
        wait: async () => {},
      }).verify({ reason: 'r', language: 'en' });
    await expect(fail('verification_unavailable')).resolves.toBe('unprotected');
    await expect(fail('totp_required')).rejects.toMatchObject({ code: 'totp_required' });
    await expect(fail('verification_canceled')).rejects.toMatchObject({
      code: 'verification_canceled',
    });
  });

  it('desktop: waits for a server that is still starting, then gives up', async () => {
    let calls = 0;
    const stepUpTest = vi.fn(async () => {
      calls++;
      if (calls < 3) throw new NativeError('unreachable', 'down');
    });
    const wait = vi.fn(async () => {});
    const lock = desktopAppLock({ security: { stepUpTest }, wait });
    await expect(lock.verify({ reason: 'r', language: 'en' })).resolves.toBe('verified');
    expect(wait).toHaveBeenCalledTimes(2);

    const down = desktopAppLock({
      security: {
        stepUpTest: async () => {
          throw new NativeError('unreachable', 'down');
        },
      },
      wait: async () => {},
    });
    await expect(down.verify({ reason: 'r', language: 'en' })).rejects.toMatchObject({
      code: 'unreachable',
    });
  });

  it('phone: biometrics or passcode; none set up opens unprotected', async () => {
    const authenticate = vi.fn(async () => {});
    await expect(
      phoneAppLock(authenticate).verify({ reason: 'Unlock', language: 'en' }),
    ).resolves.toBe('verified');
    expect(authenticate).toHaveBeenCalledWith('Unlock');
    const failing = (code: string) =>
      phoneAppLock(async () => {
        throw { code, message: code };
      }).verify({ reason: 'r', language: 'en' });
    await expect(failing('passcodeNotSet')).resolves.toBe('unprotected');
    await expect(failing('biometryNotEnrolled')).resolves.toBe('unprotected');
    await expect(failing('userCancel')).rejects.toMatchObject({ code: 'verification_canceled' });
    await expect(failing('authenticationFailed')).rejects.toMatchObject({
      code: 'verification_failed',
    });
  });

  it('browser: off unless simulated with ?applock=1', () => {
    expect(simulatedAppLock('')).toBeNull();
    expect(simulatedAppLock('?applock=1')).not.toBeNull();
  });
});

describe('App lock settings and rules', () => {
  it('defaults to on with 15 minutes and ignores unknown idle times', () => {
    const defaults = defaultSettings();
    expect(defaults.appLock).toBe(true);
    expect(defaults.appLockIdleMin).toBe(15);
    expect(parseSettings('{"appLock":false,"appLockIdleMin":30}', defaults)).toMatchObject({
      appLock: false,
      appLockIdleMin: 30,
    });
    expect(parseSettings('{"appLockIdleMin":7}', defaults).appLockIdleMin).toBe(15);
  });

  it('counts an unlock as fresh for 5 minutes', () => {
    const at = 1_000_000;
    expect(unlockFresh(null, at, 300_000)).toBe(false);
    expect(unlockFresh(at, at + 300_000, 300_000)).toBe(true);
    expect(unlockFresh(at, at + 300_001, 300_000)).toBe(false);
    // A clock that went back does not count.
    expect(unlockFresh(at, at - 1, 300_000)).toBe(false);
  });
});
