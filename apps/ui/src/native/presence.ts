/**
 * "Is the owner holding this phone?" check before remote shell actions:
 * biometrics with the device passcode as fallback (`tauri-plugin-biometric`,
 * phones only). Desktops and the browser need no such step (the server's
 * TOTP check is the real gate; this only adds friction on a lost phone).
 */
import { bridge, isNative, toNativeError } from './bridge';

export type PresenceResult =
  /** Biometrics or the device passcode confirmed the user. */
  | 'verified'
  /** Desktop or browser: no check on this platform. */
  | 'not_required'
  /** The phone has neither biometrics nor a passcode: TOTP only. */
  | 'unavailable'
  /** The user cancelled or failed the check. */
  | 'cancelled';

export interface PresenceBackend {
  /** True on Android/iOS. */
  isPhone(): Promise<boolean>;
  authenticate(reason: string): Promise<void>;
}

/** Plugin codes meaning "this phone has no biometrics and no passcode". */
export const UNAVAILABLE_CODES: ReadonlySet<string> = new Set([
  'passcodeNotSet',
  'biometryNotAvailable',
  'biometryNotEnrolled',
]);

let phone: Promise<boolean> | null = null;

const nativeBackend: PresenceBackend = {
  isPhone() {
    if (!isNative()) return Promise.resolve(false);
    phone ??= bridge.platform().then(
      (p) => p === 'android' || p === 'ios',
      () => false,
    );
    return phone;
  },
  authenticate: (reason) => bridge.biometric.authenticate(reason, { allowDeviceCredential: true }),
};

let backend: PresenceBackend = nativeBackend;

/** Replaces the platform backend (tests, previews); `null` restores it. */
export function setPresenceBackend(next: PresenceBackend | null): void {
  backend = next ?? nativeBackend;
}

/** Whether this device asks for biometrics before shell actions (phones). */
export function presenceRequired(): Promise<boolean> {
  return backend.isPhone();
}

/** Runs the check and tells what happened. */
export async function checkUserPresence(reason: string): Promise<PresenceResult> {
  if (!(await backend.isPhone())) return 'not_required';
  try {
    await backend.authenticate(reason);
    return 'verified';
  } catch (err) {
    const { code } = toNativeError(err);
    return code && UNAVAILABLE_CODES.has(code) ? 'unavailable' : 'cancelled';
  }
}

/**
 * Resolves `true` when the action may continue: verified, not needed on this
 * platform (desktop, web), or impossible on this phone (no biometrics and no
 * passcode: the TOTP code alone protects it). `false` when cancelled.
 */
export async function requireUserPresence(reason: string): Promise<boolean> {
  return (await checkUserPresence(reason)) !== 'cancelled';
}

/** localStorage flag: the "TOTP only on this device" notice was shown. */
export const TOTP_ONLY_NOTICE_KEY = 'vca.shell.totpOnlyNotice';

/** Whether the one-time "protected by the authenticator code only" notice is still due. */
export function totpOnlyNoticeDue(): boolean {
  try {
    return globalThis.localStorage?.getItem(TOTP_ONLY_NOTICE_KEY) !== '1';
  } catch {
    return true;
  }
}

export function rememberTotpOnlyNotice(): void {
  try {
    globalThis.localStorage?.setItem(TOTP_ONLY_NOTICE_KEY, '1');
  } catch {
    // Storage unavailable: the notice may show again, which is harmless.
  }
}
