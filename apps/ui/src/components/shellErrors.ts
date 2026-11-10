import type { TranslateFn } from '../i18n/i18n';
import { RequestError } from '../state/core';

/** True for a server error with this code. */
export function isErrorCode(err: unknown, code: string): boolean {
  return err instanceof RequestError && err.code === code;
}

/**
 * The server has no authenticator enrolled (`unsupported` with a TOTP
 * message), so asking for a code is pointless.
 */
export function isTotpNotEnrolled(err: unknown): boolean {
  return isErrorCode(err, 'unsupported') && /totp|enrol/i.test((err as RequestError).message);
}

/**
 * The shell was opened on another device and this one has not joined it
 * (`totp_required` with "join" in the message): `shell_join` with a code.
 */
export function isShellJoinRequired(err: unknown): boolean {
  return isErrorCode(err, 'totp_required') && /join/i.test((err as RequestError).message);
}

/**
 * Readable message for a failed remote shell request (`open_shell`,
 * `shell_run`, shell input). The server reports several situations as
 * `forbidden`; its message tells them apart.
 */
export function describeShellError(t: TranslateFn, err: unknown): string {
  if (!(err instanceof RequestError)) {
    return t('shell.error.generic', { error: (err as Error)?.message ?? String(err) });
  }
  const message = err.message;
  switch (err.code) {
    case 'forbidden':
      if (/too many|wrong code|locked|lockout/i.test(message)) return t('shell.error.lockout');
      if (/workspace|outside/i.test(message)) return t('shell.error.workspace');
      if (/kill.?switch/i.test(message)) return t('shell.error.killSwitch');
      return t('shell.error.forbidden');
    case 'unsupported':
      if (/totp|enrol/i.test(message)) return t('shell.error.noTotp');
      return t('shell.error.unsupported');
    case 'unavailable':
      return t('shell.error.unavailable');
    case 'totp_invalid':
      return t('shell.error.totpInvalid');
    case 'totp_required':
      if (/join/i.test(message)) return t('shell.error.joinRequired');
      return t('shell.error.totpRequired');
    case 'conflict':
      return t('shell.error.closed');
    case 'not_found':
      return t('shell.error.closed');
    case 'invalid_request':
      return t('shell.error.invalid', { error: message });
    default:
      return t('shell.error.generic', { error: message });
  }
}
