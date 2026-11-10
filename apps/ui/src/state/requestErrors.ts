/**
 * Friendly, translated text for a failed request, chosen by error code (and
 * a few well-known server messages). The raw server text, in English and
 * sometimes with ids or paths, is kept as detail for a "Details" view.
 */
import type { MessageKey, TranslateFn } from '../i18n/i18n';
import { RequestError } from './core';

export interface RequestErrorText {
  message: string;
  detail: string | null;
}

function keyFor(code: string, raw: string): MessageKey {
  const m = raw.toLowerCase();
  switch (code) {
    case 'not_found':
      return m.includes('session') ? 'error.sessionNotFound' : 'error.notFound';
    case 'unsupported':
      if (m.includes('did not start') || m.includes('only managed') || m.includes('observed')) {
        return 'error.notStartedByVca';
      }
      if (m.includes('cloud sessions can only be stopped')) return 'error.cloudStopOnly';
      if (m.includes('broker is off')) return 'error.brokerOff';
      return 'error.unsupported';
    case 'conflict':
      return 'error.conflict';
    case 'unavailable':
      return 'error.unavailable';
    case 'forbidden':
      return 'error.forbidden';
    case 'unauthorized':
      return 'error.unauthorized';
    case 'invalid_request':
      return 'error.invalid';
    case 'transport':
      return 'error.transport';
    default:
      return 'error.failed';
  }
}

export function describeRequestError(t: TranslateFn, err: unknown): RequestErrorText {
  const raw = (err instanceof Error ? err.message : String(err)).trim();
  const code = err instanceof RequestError ? err.code : 'internal';
  const message = t(keyFor(code, raw));
  return { message, detail: raw && raw !== message ? raw : null };
}
