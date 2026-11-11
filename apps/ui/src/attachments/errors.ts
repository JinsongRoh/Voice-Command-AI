/**
 * Short, translated text for a failed upload, chosen by the server's error
 * code (and, where one code has several causes, a hint in its message). The
 * raw server message, which may hold long local paths, is kept as detail
 * for an on-demand "Details" view instead of being shown inline.
 *
 * Server causes (crates/vca-server/src/attachments.rs):
 * - forbidden: project outside the workspace roots, executable files, a
 *   path that left the project folder;
 * - unsupported: file type not on the allow-list, cloud session without a
 *   project folder, attachments turned off;
 * - invalid_request: too large, malformed upload;
 * - unavailable: too many uploads at once, I/O problems;
 * - not_found: the session or the upload is gone.
 */
import type { MessageKey, TranslateFn } from '../i18n/i18n';
import { RequestError } from '../state/core';

export interface AttachErrorText {
  message: string;
  /** The server's own message, for "Details"; `null` when it adds nothing. */
  detail: string | null;
}

function keyFor(code: string, message: string): MessageKey {
  const m = message.toLowerCase();
  switch (code) {
    case 'forbidden':
      if (m.includes('workspace root')) return 'attach.errOutsideRoots';
      if (m.includes('executable')) return 'attach.errExecutable';
      return 'attach.errForbidden';
    case 'unsupported':
      if (m.includes('cloud session') || m.includes('no project folder')) {
        return 'attach.errNoProject';
      }
      if (m.includes('attachments are off')) return 'attach.errDisabled';
      return 'attach.errType';
    case 'invalid_request':
      if (m.includes('too large')) return 'attach.errTooLargeServer';
      return 'attach.failed';
    case 'unavailable':
      if (m.includes('too many uploads')) return 'attach.errBusy';
      return 'attach.failed';
    case 'not_found':
      if (m.includes('no session')) return 'attach.errSessionGone';
      return 'attach.failed';
    case 'transport':
      return 'attach.errConnection';
    default:
      return 'attach.failed';
  }
}

export function describeAttachError(t: TranslateFn, err: unknown): AttachErrorText {
  const raw = err instanceof Error ? err.message : String(err);
  const code = err instanceof RequestError ? err.code : 'internal';
  const message = t(keyFor(code, raw));
  const detail = raw.trim() && raw.trim() !== message ? raw.trim() : null;
  return { message, detail };
}
