import { useEffect, useRef, useState, type FormEvent } from 'react';

import { useI18n } from '../i18n/context';
import { toNativeError } from '../native/bridge';
import { svgDataUri } from '../native/host';
import {
  describeSecurityError,
  isStepUpCanceled,
  type SecurityServices,
  type TotpEnrollment,
} from '../native/security';
import { describeSaveResult, saveTextFile } from '../native/saveFile';
import { refreshSecurityStatus } from '../native/useSecurityStatus';
import { copyText } from './clipboard';
import { Icon } from './Icon';
import { useStepUp } from './StepUpPrompt';
import styles from './SecuritySettings.module.css';

/**
 * Sets up (or replaces) the authenticator app: QR code and secret, a code
 * from the app to confirm, then the backup codes once. Starts at once (the
 * shell asks for local verification first).
 */
export function TotpSetupPanel({
  services,
  replace,
  afterRecovery = false,
  onDone,
  onCancel,
}: {
  services: SecurityServices;
  /** An authenticator is enrolled already: this one replaces it. */
  replace: boolean;
  /** Recovery just finished: the current authenticator's code is not needed. */
  afterRecovery?: boolean;
  onDone: () => void;
  onCancel?: () => void;
}) {
  const { t } = useI18n();
  const stepUp = useStepUp();
  const action = replace ? 'totp_reset' : 'totp_setup';
  const [enrollment, setEnrollment] = useState<TotpEnrollment | null>(null);
  const [code, setCode] = useState('');
  const [current, setCurrent] = useState('');
  const [needCurrent, setNeedCurrent] = useState(replace && !afterRecovery);
  const [codes, setCodes] = useState<string[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const started = useRef(false);

  const begin = async () => {
    setBusy(true);
    setError(null);
    try {
      setEnrollment(await stepUp.run(action, () => services.totpBegin(replace)));
    } catch (err) {
      if (isStepUpCanceled(err)) onCancel?.();
      else setError(describeSecurityError(t, err));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    if (started.current) return;
    started.current = true;
    void begin();
    // Runs once on open.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const confirm = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const done = await stepUp.run(action, () =>
        services.totpConfirm(
          code.replace(/\s+/g, ''),
          needCurrent && current.trim() ? current.replace(/\s+/g, '') : undefined,
        ),
      );
      setCodes(done.backup_codes);
      void refreshSecurityStatus(services);
    } catch (err) {
      if (isStepUpCanceled(err)) return;
      if (toNativeError(err).code === 'current_code_required') setNeedCurrent(true);
      setError(describeSecurityError(t, err));
    } finally {
      setBusy(false);
    }
  };

  if (codes)
    return (
      <div className={styles.panelBox} data-testid="totp-setup">
        <p role="status" className={styles.success}>
          {t('security.totp.doneSaveCodes')}
        </p>
        <BackupCodes codes={codes} onDone={onDone} />
      </div>
    );

  return (
    <div className={styles.panelBox} data-testid="totp-setup">
      {enrollment ? (
        <>
          <p>{t('security.totp.scan')}</p>
          <div className={styles.qrRow}>
            <img
              className={styles.qr}
              src={svgDataUri(enrollment.qr_svg)}
              alt={t('host.totpQrAlt')}
              width={200}
              height={200}
            />
            <p className={styles.secret}>
              <span className="muted">{t('security.totp.secret')}</span>
              <code data-testid="totp-secret">{enrollment.secret}</code>
            </p>
          </div>
          <form className={styles.form} onSubmit={(e) => void confirm(e)} noValidate>
            {needCurrent && (
              <div className="field">
                <label htmlFor="sec-totp-current">{t('security.totp.currentCode')}</label>
                <input
                  id="sec-totp-current"
                  type="text"
                  inputMode="numeric"
                  autoComplete="off"
                  maxLength={8}
                  value={current}
                  onChange={(e) => setCurrent(e.target.value)}
                />
              </div>
            )}
            <div className="field">
              <label htmlFor="sec-totp-code">{t('security.totp.newCode')}</label>
              <input
                id="sec-totp-code"
                type="text"
                inputMode="numeric"
                autoComplete="one-time-code"
                maxLength={8}
                value={code}
                onChange={(e) => setCode(e.target.value)}
              />
            </div>
            <div className={styles.actions}>
              <button
                type="submit"
                className="btn btn-primary"
                disabled={busy || !code.trim() || (needCurrent && !current.trim())}
              >
                {t('security.totp.confirm')}
              </button>
              {onCancel && (
                <button type="button" className="btn" disabled={busy} onClick={onCancel}>
                  {t('common.cancel')}
                </button>
              )}
            </div>
          </form>
        </>
      ) : busy ? (
        <p className="muted" role="status">
          {t('security.loading')}
        </p>
      ) : (
        <div className={styles.actions}>
          <button type="button" className="btn btn-primary" onClick={() => void begin()}>
            {t('security.retry')}
          </button>
          {onCancel && (
            <button type="button" className="btn" onClick={onCancel}>
              {t('common.cancel')}
            </button>
          )}
        </div>
      )}
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
    </div>
  );
}

/**
 * Copy and "Save to file" for backup codes shown once (also used by the
 * phone pairing screen's two-factor setup).
 */
export function BackupCodeActions({ codes }: { codes: string[] }) {
  const { t } = useI18n();
  const [note, setNote] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const text = codes.join('\n');

  const copy = async () => {
    setNote((await copyText(text)) ? t('security.codes.copied') : t('security.codes.copyFailed'));
  };

  const save = async () => {
    setSaving(true);
    try {
      setNote(describeSaveResult(t, await saveTextFile('vca-backup-codes.txt', `${text}\n`)));
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <div className={styles.actions}>
        <button type="button" className="btn btn-sm" onClick={() => void copy()}>
          <Icon name="copy" size={16} />
          {t('security.codes.copy')}
        </button>
        <button type="button" className="btn btn-sm" disabled={saving} onClick={() => void save()}>
          {t('security.codes.save')}
        </button>
      </div>
      {note && (
        <p role="status" className="muted">
          {note}
        </p>
      )}
    </>
  );
}

/**
 * Backup codes, shown once: copy or save them, confirm "I saved them",
 * then continue.
 */
export function BackupCodes({ codes, onDone }: { codes: string[]; onDone: () => void }) {
  const { t } = useI18n();
  const [saved, setSaved] = useState(false);

  return (
    <div className={styles.codesBox}>
      <ul className={styles.codes} data-testid="backup-codes">
        {codes.map((c) => (
          <li key={c}>{c}</li>
        ))}
      </ul>
      <BackupCodeActions codes={codes} />
      <label className="check">
        <input type="checkbox" checked={saved} onChange={(e) => setSaved(e.target.checked)} />
        {t('security.codes.saved')}
      </label>
      <button type="button" className="btn btn-primary" disabled={!saved} onClick={onDone}>
        {t('security.continue')}
      </button>
    </div>
  );
}
