import { useId, useRef, useState, type FormEvent } from 'react';

import { useI18n } from '../i18n/context';
import { isBackupCode, isTotpCode, normalizeBackupCode, TOTP_ISSUER } from '../protocol/shell';
import { Icon } from './Icon';
import { Modal } from './Modal';
import { describeShellError, isErrorCode, isTotpNotEnrolled } from './shellErrors';
import styles from './Shell.module.css';

export interface TotpPromptProps {
  title: string;
  /** What the code unlocks (shown above the field). */
  description: string;
  /** One-time notice shown above the field (e.g. "TOTP only on this device"). */
  notice?: string | null;
  submitLabel: string;
  /**
   * The server has no authenticator enrolled: explain how to set one up
   * instead of asking for a code.
   */
  notEnrolled?: boolean;
  /**
   * Uses the code (an authenticator code, or a backup code in the same
   * field). Rejecting with `totp_invalid` keeps the prompt open with an
   * error; "not enrolled" switches to the explanation; any other rejection
   * is handed to `onFail`.
   */
  onSubmit: (code: string) => Promise<void>;
  onCancel: () => void;
  onFail: (err: unknown) => void;
  /**
   * Own message for a rejection, shown in the prompt, which stays open
   * (the code field is cleared for `totp_invalid`). `null`: the usual
   * handling above.
   */
  describeError?: (err: unknown) => string | null;
}

type Mode = 'totp' | 'backup';

/** Longest backup code as typed: `XXXX-XXXX-XXXX-XXXX` (with a few spaces). */
const BACKUP_MAX = 24;

/**
 * Authenticator code prompt: says which entry of which app to read, takes
 * a backup code in the same field, and explains how to set the
 * authenticator up again. Six-digit codes use the numeric keyboard and
 * one-time-code autofill and submit by themselves.
 */
export function TotpPrompt({
  title,
  description,
  notice,
  submitLabel,
  notEnrolled: notEnrolledProp = false,
  onSubmit,
  onCancel,
  onFail,
  describeError,
}: TotpPromptProps) {
  const { t } = useI18n();
  const id = useId();
  const [mode, setMode] = useState<Mode>('totp');
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notEnrolled, setNotEnrolled] = useState(notEnrolledProp);
  const input = useRef<HTMLInputElement>(null);
  const busyRef = useRef(false);

  const valid = mode === 'totp' ? isTotpCode(code) : isBackupCode(code);

  const submit = async (value: string, as: Mode) => {
    if (busyRef.current) return;
    if (as === 'totp' ? !isTotpCode(value) : !isBackupCode(value)) {
      setError(t(as === 'totp' ? 'totp.sixDigits' : 'totp.backupFormat'));
      input.current?.focus();
      return;
    }
    busyRef.current = true;
    setBusy(true);
    setError(null);
    try {
      await onSubmit(as === 'totp' ? value : normalizeBackupCode(value));
    } catch (err) {
      const own = describeError?.(err) ?? null;
      if (own !== null) {
        setError(own);
        if (isErrorCode(err, 'totp_invalid')) setCode('');
        input.current?.focus();
      } else if (isErrorCode(err, 'totp_invalid')) {
        setError(describeShellError(t, err));
        setCode('');
        input.current?.focus();
      } else if (isTotpNotEnrolled(err)) {
        setNotEnrolled(true);
      } else {
        onFail(err);
      }
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  };

  const onFormSubmit = (e: FormEvent) => {
    e.preventDefault();
    void submit(code, mode);
  };

  const switchMode = () => {
    setMode((m) => (m === 'totp' ? 'backup' : 'totp'));
    setCode('');
    setError(null);
    input.current?.focus();
  };

  const describedBy = [`${id}-desc`, `${id}-where`, error ? `${id}-err` : null]
    .filter(Boolean)
    .join(' ');

  // One dialog for both states, so switching keeps focus and the title.
  return (
    <Modal
      title={title}
      onClose={onCancel}
      dismissOnBackdrop={false}
      footer={
        notEnrolled ? (
          <button type="button" className="btn btn-primary" onClick={onCancel}>
            {t('common.close')}
          </button>
        ) : (
          <>
            <button type="button" className="btn" onClick={onCancel}>
              {t('common.cancel')}
            </button>
            <button
              type="submit"
              form={`${id}-form`}
              className="btn btn-primary"
              disabled={busy || !valid}
            >
              {busy ? t('totp.checking') : submitLabel}
            </button>
          </>
        )
      }
    >
      {notEnrolled ? (
        <div className={styles.totp}>
          <p className={styles.notice} role="alert">
            <Icon name="info" size={16} />
            <span>{t('totp.notEnrolled')}</span>
          </p>
          <p className={styles.totpHelp}>{t('totp.setupFirst')}</p>
        </div>
      ) : (
        <form id={`${id}-form`} onSubmit={onFormSubmit} className={styles.totp} noValidate>
          {notice && (
            <p className={styles.notice} role="note">
              <Icon name="info" size={16} />
              <span>{notice}</span>
            </p>
          )}
          <p className={styles.totpLead} id={`${id}-desc`}>
            {description}
          </p>
          <p className={styles.totpHelp} id={`${id}-where`}>
            {mode === 'totp' ? t('totp.where', { issuer: TOTP_ISSUER }) : t('totp.backupWhere')}
          </p>
          <label htmlFor={`${id}-code`} className="label">
            {mode === 'totp' ? t('totp.label') : t('totp.backupLabel')}
          </label>
          {mode === 'totp' ? (
            <input
              key="totp"
              ref={input}
              id={`${id}-code`}
              className={styles.totpInput}
              type="text"
              inputMode="numeric"
              autoComplete="one-time-code"
              pattern="[0-9]*"
              maxLength={6}
              placeholder="000000"
              aria-describedby={describedBy}
              aria-invalid={error ? true : undefined}
              value={code}
              disabled={busy}
              onChange={(e) => {
                const digits = e.target.value.replace(/\D/g, '').slice(0, 6);
                setCode(digits);
                if (error) setError(null);
                // Autofill and paste deliver all six digits at once.
                if (digits.length === 6 && e.target.value.length >= 6 && code.length === 0) {
                  void submit(digits, 'totp');
                }
              }}
            />
          ) : (
            <input
              key="backup"
              ref={input}
              id={`${id}-code`}
              className={`${styles.totpInput} ${styles.backupInput}`}
              type="text"
              inputMode="text"
              autoComplete="off"
              autoCapitalize="characters"
              autoCorrect="off"
              spellCheck={false}
              maxLength={BACKUP_MAX}
              placeholder="XXXX-XXXX-XXXX-XXXX"
              aria-describedby={describedBy}
              aria-invalid={error ? true : undefined}
              value={code}
              disabled={busy}
              onChange={(e) => {
                setCode(e.target.value.toUpperCase().replace(/[^A-Z0-9\s-]/g, ''));
                if (error) setError(null);
              }}
            />
          )}
          {error && (
            <p id={`${id}-err`} role="alert" className={styles.error}>
              {error}
            </p>
          )}
          <div className={styles.totpAlt}>
            <button
              type="button"
              className={styles.linkButton}
              onClick={switchMode}
              disabled={busy}
            >
              {mode === 'totp' ? t('totp.useBackup') : t('totp.useTotp')}
            </button>
            {mode === 'totp' && <span className={styles.totpHelp}>{t('totp.backupHint')}</span>}
          </div>
          <details className={styles.totpMore}>
            <summary>{t('totp.noApp')}</summary>
            <p className={styles.totpHelp}>{t('totp.setupWhere')}</p>
          </details>
        </form>
      )}
    </Modal>
  );
}
