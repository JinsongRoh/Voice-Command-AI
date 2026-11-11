import { useEffect, useId, useState, type FormEvent } from 'react';

import { useI18n } from '../i18n/context';
import { isOpenShell, isShellSession } from '../protocol/helpers';
import { commandProblem } from '../protocol/shell';
import type { SessionInfo } from '../protocol/types';
import { canSendInput } from '../state/reducer';
import { useActions } from '../state/store';
import { ControlKeys } from './ControlKeys';
import { Icon } from './Icon';
import { useShell } from './ShellProvider';
import { describeShellError, isErrorCode, isShellJoinRequired } from './shellErrors';
import styles from './TerminalInput.module.css';

/** How long a key or send error stays. */
const ERROR_MS = 4000;

/**
 * The phone terminal's own input: one line that is typed into the session
 * with Enter, plus the key bar (Enter, Esc, Tab, arrows, Ctrl+C...). The
 * person can work in the terminal without switching to Chat.
 *
 * It uses the composer's input path: `send_input` text the person typed (so
 * it may answer what the terminal shows), and on a remote shell a line the
 * server holds back for a code goes to the "Run in shell" sheet.
 */
export function TerminalInput({
  session,
  onOpenComposer,
}: {
  session: SessionInfo;
  /** Shows the full message box (voice, attachments) instead. */
  onOpenComposer?: () => void;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const shell = useShell();
  const id = useId();
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const enabled = canSendInput(session);

  useEffect(() => {
    if (!error) return;
    const timer = setTimeout(() => setError(null), ERROR_MS);
    return () => clearTimeout(timer);
  }, [error]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const value = text.trim();
    if (!enabled || busy) return;
    setError(null);
    if (!value) {
      // An empty line is just Enter (confirm a menu, a fresh prompt).
      actions
        .sendKey(session.id, 'enter')
        .catch((err: Error) => setError(describeShellError(t, err)));
      return;
    }
    setBusy(true);
    try {
      await actions.sendText(session.id, value, undefined, true);
      setText('');
    } catch (err) {
      if (isShellJoinRequired(err) && shell) {
        // A shell opened on another device: join it with a code first.
        void shell.joinShell(session.id);
      } else if (
        isErrorCode(err, 'totp_required') &&
        isShellSession(session) &&
        isOpenShell(session) &&
        shell &&
        !commandProblem(value)
      ) {
        // Dangerous outside the step-up window: classification, confirmation, code.
        shell.runInShell({ command: value, sessionId: session.id });
        setText('');
      } else {
        setError(describeShellError(t, err));
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <section
      className={styles.input}
      aria-label={t('terminal.inputLabel', { name: session.display_name })}
      data-testid="terminal-input"
    >
      <form className={styles.row} onSubmit={(e) => void submit(e)}>
        <label htmlFor={`${id}-line`} className="visually-hidden">
          {t('terminal.inputLabel', { name: session.display_name })}
        </label>
        <span className={styles.prompt} aria-hidden="true">
          ›
        </span>
        <input
          id={`${id}-line`}
          className={styles.line}
          value={text}
          disabled={!enabled}
          placeholder={t('terminal.inputPlaceholder')}
          autoCapitalize="off"
          autoCorrect="off"
          autoComplete="off"
          spellCheck={false}
          enterKeyHint="send"
          onChange={(e) => setText(e.target.value)}
        />
        <button
          type="submit"
          className={`btn btn-primary ${styles.send}`}
          disabled={!enabled || busy}
          aria-label={t('terminal.sendLine')}
          title={t('terminal.sendLine')}
        >
          <Icon name="send" size={18} />
        </button>
        {onOpenComposer && (
          <button
            type="button"
            className={`icon-btn ${styles.more}`}
            aria-label={t('terminal.openComposer')}
            title={t('terminal.openComposer')}
            onClick={onOpenComposer}
          >
            <Icon name="chat" size={18} />
          </button>
        )}
      </form>
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
      <ControlKeys
        sessionId={session.id}
        disabled={!enabled}
        onError={setError}
        className={styles.keys}
      />
    </section>
  );
}
