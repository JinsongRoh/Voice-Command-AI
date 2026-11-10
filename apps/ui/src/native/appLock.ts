/**
 * Who may open the app: the device's own sign-in, never a new password.
 *
 * - Windows and macOS: the local verification of this PC's server (Windows
 *   Hello, Touch ID), the same one gated actions use. vcad shows the OS
 *   prompt "Unlock Voice Command AI" (`app_unlock`, which approves nothing
 *   and issues no token; an older vcad without it answers `invalid_request`
 *   and the lock falls back to `step_up_test`, whose prompt reads "Test the
 *   verification"). Without an OS verifier vcad asks for the authenticator
 *   code instead (`totp_required`).
 * - Android and iOS: biometrics with the device passcode / PIN as fallback
 *   (`tauri-plugin-biometric`).
 * - Nothing to verify with (no Windows Hello, Touch ID or authenticator; a
 *   phone with no biometrics and no passcode): the app opens with a warning
 *   (`unprotected`).
 *
 * The browser build has no verifier: the lock is off there, or simulated
 * with `?applock=1` (see `simulatedAppLock`).
 */
import { bridge, call, NativeError, toNativeError } from './bridge';
import { hostBridge, isDesktopPlatform, type HostServices } from './host';
import { UNAVAILABLE_CODES } from './presence';
import {
  securityBridge,
  STEP_UP_CANCELED,
  type SecurityServices,
  type StepUpTestRequest,
} from './security';

/** `verified`: the person proved it is them; `unprotected`: nothing could check. */
export type UnlockResult = 'verified' | 'unprotected';

export interface UnlockRequest {
  /** Shown in the OS prompt where the platform shows one (phones). */
  reason: string;
  /** The app language, for the text of the OS prompt. */
  language: string;
  /** The authenticator code, after `totp_required`. */
  totp?: string;
}

export interface AppLockVerifier {
  /**
   * Resolves once the person is verified. Rejects with a `NativeError`:
   * `totp_required` (ask for the authenticator code and call again with it),
   * `verification_canceled`, `verification_failed`, `totp_invalid`,
   * `unreachable` (the server on this PC is not running), ...
   */
  verify(request: UnlockRequest): Promise<UnlockResult>;
  /**
   * Desktop: starts this PC's server, which holds the verifier. Resolves
   * `false` when this build cannot start it.
   */
  startServer?(): Promise<boolean>;
  /**
   * Whether an OS prompt can be answered right now: the app's window is
   * shown and has focus. Windows parents its prompt to the app's window and
   * answers "canceled" when that window is not in front (an app that started
   * in the background, a PC that is locked), so the lock screen waits for
   * this before it tries by itself. Absent: always.
   */
  promptable?(): boolean;
  /**
   * `false`: the lock screens do not open the OS prompt by themselves; the
   * person presses Unlock. Windows makes its prompt modal to the app's
   * window, so a prompt opened at start pins the window in place until it is
   * answered. Absent: they do.
   */
  autoPrompt?: boolean;
}

/** The app's window is shown and has focus (see `AppLockVerifier.promptable`). */
export function appWindowActive(doc: Document | undefined = globalThis.document): boolean {
  if (!doc) return true;
  if (doc.visibilityState === 'hidden') return false;
  return typeof doc.hasFocus !== 'function' || doc.hasFocus();
}

/** Plugin codes of a phone check the person cancelled. */
const CANCEL_CODES = new Set(['userCancel', 'appCancel', 'systemCancel', 'canceled', 'cancelled']);

/** The server may still be starting when the app opens. */
const UNREACHABLE_RETRIES = 6;
const UNREACHABLE_RETRY_MS = 1_000;

/** vcad's `app_unlock`: the OS prompt says "Unlock Voice Command AI". */
export type AppUnlockCall = (request: StepUpTestRequest) => Promise<void>;

/** What an older vcad answers a call it does not know. */
const UNKNOWN_CALL = 'invalid_request';

export const appUnlockBridge: AppUnlockCall = ({ totp, language }) =>
  call<void>('vca_host_security_app_unlock', {
    totp: totp ?? null,
    language: language ?? null,
  });

export interface DesktopLockDeps {
  security: Pick<SecurityServices, 'stepUpTest'>;
  /** Preferred over `security.stepUpTest`, which stays the fallback for older servers. */
  appUnlock?: AppUnlockCall;
  host?: Pick<HostServices, 'start' | 'status'>;
  /** Waits between tries while the server starts (tests pass a no-op). */
  wait?: (ms: number) => Promise<void>;
}

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/** Windows Hello / Touch ID through this PC's server; the code where it has neither. */
export function desktopAppLock({
  security,
  appUnlock,
  host,
  wait = sleep,
}: DesktopLockDeps): AppLockVerifier {
  // Set once the server turned `app_unlock` down as unknown (older vcad).
  let legacy = !appUnlock;
  return {
    async verify({ language, totp }) {
      const request = { language, ...(totp === undefined ? {} : { totp }) };
      for (let attempt = 0; ; attempt++) {
        try {
          if (!legacy && appUnlock) {
            try {
              await appUnlock(request);
              return 'verified';
            } catch (err) {
              if (toNativeError(err).code !== UNKNOWN_CALL) throw err;
              legacy = true;
            }
          }
          await security.stepUpTest(request);
          return 'verified';
        } catch (err) {
          const e = toNativeError(err);
          // No Windows Hello / Touch ID and no authenticator: nothing can check.
          if (e.code === 'verification_unavailable') return 'unprotected';
          if (e.code === 'unreachable' && attempt < UNREACHABLE_RETRIES) {
            await wait(UNREACHABLE_RETRY_MS);
            continue;
          }
          throw e;
        }
      }
    },
    ...(host
      ? {
          async startServer() {
            const status = await host.status().catch(() => null);
            if (status?.running) return true;
            if (status && !status.can_start) return false;
            await host.start();
            return true;
          },
        }
      : {}),
  };
}

/** Biometrics, with the device passcode as fallback. */
export function phoneAppLock(
  authenticate: (reason: string) => Promise<void> = (reason) =>
    bridge.biometric.authenticate(reason, { allowDeviceCredential: true }),
): AppLockVerifier {
  return {
    async verify({ reason }) {
      try {
        await authenticate(reason);
        return 'verified';
      } catch (err) {
        const { code, message } = toNativeError(err);
        if (code && UNAVAILABLE_CODES.has(code)) return 'unprotected';
        if (code && CANCEL_CODES.has(code)) throw new NativeError(STEP_UP_CANCELED, message);
        throw new NativeError('verification_failed', message);
      }
    },
  };
}

/** The verifier of the platform the shell reports (detected on first use). */
export function nativeAppLock(
  platform: () => Promise<string> = () => bridge.platform(),
  windowActive: () => boolean = appWindowActive,
  windows: boolean = isWindowsWebView(),
): AppLockVerifier {
  let resolved: Promise<AppLockVerifier | null> | null = null;
  const pick = () =>
    (resolved ??= platform().then(
      (p) =>
        isDesktopPlatform(p)
          ? desktopAppLock({
              security: securityBridge,
              appUnlock: appUnlockBridge,
              host: hostBridge,
            })
          : p === 'android' || p === 'ios'
            ? phoneAppLock()
            : null,
      () => null,
    ));
  return {
    async verify(request) {
      const verifier = await pick();
      // A platform without a known verifier: open, with the warning.
      return verifier ? verifier.verify(request) : 'unprotected';
    },
    async startServer() {
      const verifier = await pick();
      return verifier?.startServer ? verifier.startServer() : false;
    },
    promptable: windowActive,
    autoPrompt: !windows,
  };
}

/** The shell's web view runs on Windows (WebView2 says so in its user agent). */
export function isWindowsWebView(nav: Navigator | undefined = globalThis.navigator): boolean {
  return /\bWindows\b/i.test(nav?.userAgent ?? '');
}

/**
 * The browser build (development, demos): a stand-in prompt that succeeds
 * after a short pause, so the lock screens can be tried without a native
 * shell. On with `?applock=1` in the address; off otherwise.
 */
export function simulatedAppLock(search: string, delayMs = 800): AppLockVerifier | null {
  if (new URLSearchParams(search).get('applock') !== '1') return null;
  return {
    verify: async () => {
      await sleep(delayMs);
      return 'verified';
    },
  };
}
