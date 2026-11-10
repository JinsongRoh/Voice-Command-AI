import { useState, type FormEvent } from 'react';

import { useI18n } from '../i18n/context';
import { describeNativeError } from '../native/errors';
import { useActions, type Machine } from '../state/store';
import styles from './PairingView.module.css';

/**
 * Shown when the native link reports `login_required`: the device token
 * expired, so the user re-authenticates with a TOTP or backup code.
 */
export function LoginView({ machine }: { machine: Machine }) {
  const { t } = useI18n();
  const actions = useActions();
  const [useBackup, setUseBackup] = useState(false);
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const native = machine.native;
  if (!native) return null;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const value = code.trim();
    if (busy || !value) return;
    setBusy(true);
    setError(null);
    try {
      await native.login(useBackup ? null : value, useBackup ? value : null);
      setCode('');
      await actions.reconnect();
    } catch (err) {
      setError(describeNativeError(t, err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className={`panel ${styles.pairing}`} aria-labelledby="login-title">
      <h2 id="login-title">{t('login.title', { machine: machine.name })}</h2>
      <p>{t('login.help', { machine: machine.name })}</p>
      <form className={styles.form} onSubmit={(e) => void submit(e)} noValidate>
        <div className="field">
          <label htmlFor="login-code">{useBackup ? t('login.backupCode') : t('login.totp')}</label>
          <input
            id="login-code"
            type="text"
            inputMode={useBackup ? 'text' : 'numeric'}
            autoComplete={useBackup ? 'off' : 'one-time-code'}
            autoCapitalize="off"
            spellCheck={false}
            value={code}
            onChange={(e) => setCode(e.target.value)}
          />
        </div>
        {error && (
          <p role="alert" className={styles.error}>
            {error}
          </p>
        )}
        <div className={styles.actions}>
          <button type="submit" className="btn btn-primary" disabled={busy || !code.trim()}>
            {busy ? t('login.signingIn') : t('login.submit')}
          </button>
          <button
            type="button"
            className="btn btn-ghost"
            onClick={() => {
              setUseBackup((b) => !b);
              setCode('');
              setError(null);
            }}
          >
            {useBackup ? t('login.useTotp') : t('login.useBackup')}
          </button>
        </div>
      </form>
    </section>
  );
}
