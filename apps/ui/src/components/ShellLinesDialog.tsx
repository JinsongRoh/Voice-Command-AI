import { useId, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { SessionId } from '../protocol/types';
import { useActions } from '../state/store';
import { Icon } from './Icon';
import { Modal } from './Modal';
import { useShell } from './ShellProvider';
import { describeShellError, isErrorCode, isShellJoinRequired } from './shellErrors';
import styles from './Shell.module.css';

export interface ShellLinesRequest {
  sessionId: SessionId;
  /** The full text the server held back (`shell_confirm.command`). */
  command: string;
  /** Its number of lines (two or more). */
  lines: number;
}

/**
 * "Run these N lines?": input of several lines (a pasted script, a heredoc)
 * that the server did not type. All of it is shown; only the Run button
 * resends it with `confirm`. Dangerous lines still need the step-up (phone
 * presence check and, outside the window, an authenticator code), and a
 * shell opened elsewhere is joined first.
 */
export function ShellLinesDialog({
  request,
  onClose,
  onDone,
}: {
  request: ShellLinesRequest;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useI18n();
  const id = useId();
  const actions = useActions();
  const shell = useShell();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { sessionId, command, lines } = request;

  const send = (totp: string | null) => actions.shellRun(sessionId, command, totp, true);

  const run = async () => {
    if (busy || !shell) return;
    setBusy(true);
    setError(null);
    try {
      try {
        await send(null);
      } catch (err) {
        if (isShellJoinRequired(err)) {
          if (!(await shell.joinShell(sessionId))) {
            setBusy(false);
            return;
          }
          await send(null);
        } else if (isErrorCode(err, 'totp_required')) {
          if (!(await shell.confirmPresence(t('shell.presence.run')))) {
            setBusy(false);
            return;
          }
          const ok = await shell.withTotp(
            {
              title: t('shell.lines.dangerousTitle'),
              description: t('shell.lines.dangerousDescription'),
              submitLabel: t('shell.lines.run', { count: lines }),
            },
            async (code) => {
              await send(code);
              return true;
            },
          );
          if (!ok) {
            setBusy(false);
            return;
          }
        } else {
          throw err;
        }
      }
      onDone();
    } catch (err) {
      setError(describeShellError(t, err));
      setBusy(false);
    }
  };

  return (
    <Modal
      title={t('shell.lines.title', { count: lines })}
      onClose={onClose}
      dismissOnBackdrop={false}
      footer={
        <>
          <button type="button" className="btn" onClick={onClose} disabled={busy}>
            {t('common.cancel')}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy || !shell}
            onClick={() => void run()}
            data-testid="shell-lines-run"
          >
            <Icon name="terminal" size={16} />
            {busy ? t('runShell.running') : t('shell.lines.run', { count: lines })}
          </button>
        </>
      }
    >
      <div className={styles.run}>
        <p className={styles.warn} id={`${id}-lead`}>
          {t('shell.lines.lead')}
        </p>
        <pre
          className={styles.command}
          aria-label={t('shell.lines.label')}
          aria-describedby={`${id}-lead`}
          data-testid="shell-lines-text"
        >
          {command}
        </pre>
        {error && (
          <p role="alert" className={styles.error}>
            {error}
          </p>
        )}
      </div>
    </Modal>
  );
}
