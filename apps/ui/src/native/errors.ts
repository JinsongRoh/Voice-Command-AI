import type { TranslateFn } from '../i18n/i18n';
import { toNativeError } from './bridge';

/** Error codes the native layer may report that have a dedicated message. */
const KNOWN_CODES = [
  'invalid_uri',
  'expired',
  'totp_required',
  'totp_invalid',
  // Codes of `ClientError::code` in vca-client (pairing and sign-in).
  'invalid_pairing',
  'invalid_totp',
  'pairing_attempts_exhausted',
  'totp_not_enrolled',
  'rate_limited',
  'backup_code_invalid',
  'unreachable',
  'timeout',
  'rejected',
  'already_paired',
  'locked',
  'dnspilot_not_connected',
  'dnspilot_reauthorize',
  'dnspilot_denied',
  'dnspilot_expired',
  'dnspilot_forbidden',
  'dnspilot_rate_limited',
  'dnspilot_disabled',
  'dnspilot_unavailable',
  'dnspilot_device_not_allowed',
  'dnspilot_key_registration_failed',
  'dnspilot_bad_link',
  'dnspilot_bad_request',
  'dnspilot_invite_invalid',
  'dnspilot_enroll_bad_signature',
  'dnspilot_enroll_bad_cert',
  'dnspilot_tunnel_not_app_mode',
  'dnspilot_client_limit',
  'dnspilot_invite_limit',
  'dnspilot_phone_no_create',
] as const;

type KnownCode = (typeof KNOWN_CODES)[number];

function isKnown(code: string | null): code is KnownCode {
  return code !== null && (KNOWN_CODES as readonly string[]).includes(code);
}

/** Readable, localized message for a rejected native command. */
export function describeNativeError(t: TranslateFn, err: unknown): string {
  const e = toNativeError(err);
  if (e.code === 'dnspilot_rate_limited' && e.retryAfterSecs !== null) {
    return t('native.error.dnspilot_rate_limited_wait', { seconds: e.retryAfterSecs });
  }
  if (isKnown(e.code)) return t(`native.error.${e.code}`);
  return t('native.error.generic', { error: e.code ? `${e.message} (${e.code})` : e.message });
}

/** Codes that mean the server wants (another) 2-step verification code. */
export function isTotpPrompt(code: string | null): boolean {
  return code === 'totp_required' || code === 'invalid_totp' || code === 'totp_invalid';
}

/** Message and stable code of a failed pairing, for the pairing screen. */
export interface PairingFailure {
  message: string;
  /** Raw error code, shown in small text for support; `null` if none. */
  code: string | null;
}

/**
 * A pairing failure in plain words. The 2-step verification prompts name the
 * authenticator entry, so nobody mistakes the code for a DNSPilot one.
 */
export function describePairingError(t: TranslateFn, err: unknown): PairingFailure {
  const e = toNativeError(err);
  let message: string;
  if (e.code === 'totp_required') message = t('pairing.totpPrompt');
  else if (e.code === 'invalid_totp' || e.code === 'totp_invalid') message = t('pairing.totpWrong');
  else message = describeNativeError(t, err);
  return { message, code: e.code };
}
