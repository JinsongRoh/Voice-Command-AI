import { useId, useState, type FormEvent } from 'react';

import { useI18n } from '../i18n/context';
import { automaticName, checkSessionName } from '../protocol/helpers';
import { MAX_SESSION_NAME_CHARS, type SessionInfo } from '../protocol/types';
import { useActions } from '../state/store';
import { Modal } from './Modal';
import styles from './RenameSessionDialog.module.css';

/**
 * Renames a session card. Enter saves, Escape cancels (Modal); a blank name
 * or "Reset" goes back to the automatic name. The new name arrives through
 * `session_updated`, so the dialog only closes once the server accepted it.
 */
export function RenameSessionDialog({
  session,
  onClose,
  onFixPronunciation,
}: {
  session: SessionInfo;
  onClose: () => void;
  /** Shows "Fix pronunciation" (phones reach it from here). */
  onFixPronunciation?: () => void;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const id = useId();
  const [value, setValue] = useState(session.display_name);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const auto = automaticName(session);
  const { name, problem } = checkSessionName(value);
  const unchanged = session.custom_name
    ? name === session.display_name
    : name === null || name === auto;
  const problemText =
    problem === 'too_long'
      ? t('rename.tooLong', { max: MAX_SESSION_NAME_CHARS })
      : problem === 'control_chars'
        ? t('rename.controlChars')
        : null;

  const apply = async (next: string | null) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await actions.renameSession(session.id, next);
      onClose();
    } catch (err) {
      setError(t('rename.error', { error: (err as Error).message }));
      setBusy(false);
    }
  };

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (problem) return;
    if (unchanged) {
      onClose();
      return;
    }
    // A name equal to the automatic one is a reset, so the card keeps
    // following the automatic name.
    void apply(name === auto ? null : name);
  };

  return (
    <Modal
      title={t('rename.title')}
      onClose={onClose}
      dismissOnBackdrop={false}
      footer={
        <>
          {session.custom_name && (
            <button
              type="button"
              className={`btn btn-ghost ${styles.reset}`}
              onClick={() => void apply(null)}
              disabled={busy}
            >
              {t('rename.reset')}
            </button>
          )}
          <button type="button" className="btn" onClick={onClose}>
            {t('common.cancel')}
          </button>
          <button
            type="submit"
            form={`${id}-form`}
            className="btn btn-primary"
            disabled={busy || !!problem}
          >
            {busy ? t('rename.saving') : t('rename.save')}
          </button>
        </>
      }
    >
      <form id={`${id}-form`} onSubmit={submit} noValidate>
        <div className="field">
          <label htmlFor={`${id}-name`}>{t('rename.label')}</label>
          <input
            id={`${id}-name`}
            type="text"
            value={value}
            onChange={(e) => setValue(e.target.value)}
            onFocus={(e) => e.currentTarget.select()}
            placeholder={auto ?? undefined}
            enterKeyHint="done"
            autoComplete="off"
            spellCheck={false}
            aria-invalid={problem ? true : undefined}
            aria-describedby={`${id}-hint`}
          />
          <span id={`${id}-hint`} className={problemText ? styles.problem : styles.hint}>
            {problemText ?? (auto ? t('rename.automatic', { name: auto }) : '')}
          </span>
        </div>
        {error && (
          <p role="alert" className={styles.problem}>
            {error}
          </p>
        )}
        {onFixPronunciation && (
          <button type="button" className="btn btn-ghost btn-sm" onClick={onFixPronunciation}>
            {t('pron.fix')}
          </button>
        )}
      </form>
    </Modal>
  );
}
