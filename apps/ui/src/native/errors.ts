import type { TranslateFn } from '../i18n/i18n';
import { toNativeError } from './bridge';

/** Error codes the native layer may report that have a dedicated message. */
const KNOWN_CODES = [
  'invalid_uri',
  'expired',
  'totp_required',
  'totp_invalid',
  'backup_code_invalid',
  'unreachable',
  'timeout',
  'rejected',
  'already_paired',
  'locked',
] as const;

type KnownCode = (typeof KNOWN_CODES)[number];

function isKnown(code: string | null): code is KnownCode {
  return code !== null && (KNOWN_CODES as readonly string[]).includes(code);
}

/** Readable, localized message for a rejected native command. */
export function describeNativeError(t: TranslateFn, err: unknown): string {
  const e = toNativeError(err);
  if (isKnown(e.code)) return t(`native.error.${e.code}`);
  return t('native.error.generic', { error: e.code ? `${e.message} (${e.code})` : e.message });
}
