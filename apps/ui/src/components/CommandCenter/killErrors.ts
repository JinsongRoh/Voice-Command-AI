import type { TranslateFn } from '../../i18n/i18n';
import { RequestError } from '../../state/core';

/** Refusals of a phone's kill switch that have their own message. */
const KILL_ERRORS = [
  'totp_required',
  'totp_invalid',
  'forbidden',
  'unsupported',
  'unavailable',
] as const;

/**
 * Message for a refused kill switch from a paired device (it needs the
 * owner's authenticator code); `null` for any other failure.
 */
export function killErrorText(t: TranslateFn, err: unknown): string | null {
  if (!(err instanceof RequestError)) return null;
  const code = KILL_ERRORS.find((c) => c === err.code);
  return code ? t(`cc.kill.error.${code}`) : null;
}
