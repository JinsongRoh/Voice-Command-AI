import { useEffect, useState, type FormEvent } from 'react';

import { useI18n } from '../i18n/context';
import { useNative, type NativeServices } from '../native/context';
import { describeNativeError } from '../native/errors';
import { Icon } from './Icon';
import styles from './PairingView.module.css';

const PLATFORM_NAMES: Record<string, string> = {
  windows: 'Windows',
  macos: 'Mac',
  linux: 'Linux',
  android: 'Android',
  ios: 'iPhone',
};

/** Pairing links look like `vca://pair?...`. */
function isPairingUri(value: string): boolean {
  return /^vca:\/\/pair(\?|\/\?)/i.test(value.trim());
}

/**
 * Device pairing (SEC-01/02). In the native shell the pairing link is pasted
 * and handed to Rust (`vca_pair`), which runs the Noise handshake; in the
 * browser/mock build this is an explanatory placeholder.
 */
export function PairingView({
  onPaired,
  intro,
}: {
  onPaired?: (machineId: string) => void;
  /** Optional lead text (e.g. when no machine is paired yet). */
  intro?: string;
}) {
  const native = useNative();
  return native ? (
    <NativePairing native={native} onPaired={onPaired} intro={intro} />
  ) : (
    <PairingPlaceholder intro={intro} />
  );
}

function PairingPlaceholder({ intro }: { intro?: string }) {
  const { t } = useI18n();
  return (
    <section className={`panel ${styles.pairing}`} aria-labelledby="pair-title">
      <h2 id="pair-title">{t('pairing.title')}</h2>
      {intro && <p>{intro}</p>}
      <div className={styles.qr} aria-hidden="true">
        <Icon name="qr" size={96} />
      </div>
      <ol className={styles.steps}>
        <li>{t('pairing.step1')}</li>
        <li>{t('pairing.step2')}</li>
        <li>{t('pairing.step3')}</li>
      </ol>
      <button
        type="button"
        className="btn btn-primary"
        disabled
        aria-describedby="pair-unavailable"
      >
        <Icon name="qr" size={18} />
        {t('pairing.scan')}
      </button>
      <p id="pair-unavailable" className="muted">
        {t('pairing.unavailable')}
      </p>
      <p className={styles.note}>{t('pairing.note')}</p>
    </section>
  );
}

function NativePairing({
  native,
  onPaired,
  intro,
}: {
  native: NativeServices;
  onPaired?: (machineId: string) => void;
  intro?: string;
}) {
  const { t } = useI18n();
  const [uri, setUri] = useState('');
  const [deviceName, setDeviceName] = useState('');
  const [nameTouched, setNameTouched] = useState(false);
  const [totp, setTotp] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [paired, setPaired] = useState<string | null>(null);

  // Default device name from the platform ("My Android"), unless the user typed one.
  useEffect(() => {
    if (nameTouched) return;
    let active = true;
    native
      .platform()
      .then((p) => {
        if (active) setDeviceName(t('pairing.defaultDevice', { platform: PLATFORM_NAMES[p] ?? p }));
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [native, nameTouched, t]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    setError(null);
    setPaired(null);
    const link = uri.trim();
    if (!isPairingUri(link)) {
      setError(t('native.error.invalid_uri'));
      return;
    }
    setBusy(true);
    try {
      const info = await native.pair(link, deviceName.trim() || 'VCA', totp.trim() || null);
      setPaired(t('pairing.success', { name: info.name }));
      setUri('');
      setTotp('');
      onPaired?.(info.id);
    } catch (err) {
      setError(describeNativeError(t, err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className={`panel ${styles.pairing}`} aria-labelledby="pair-title">
      <h2 id="pair-title">{t('pairing.title')}</h2>
      {intro && <p>{intro}</p>}
      <form className={styles.form} onSubmit={(e) => void submit(e)} noValidate>
        <div className="field">
          <label htmlFor="pair-uri">{t('pairing.uri')}</label>
          <input
            id="pair-uri"
            type="text"
            value={uri}
            onChange={(e) => setUri(e.target.value)}
            placeholder="vca://pair?..."
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            aria-describedby="pair-uri-help"
            required
          />
          <span id="pair-uri-help" className={styles.help}>
            {t('pairing.uriHelp')}
          </span>
        </div>
        <div className="field">
          <label htmlFor="pair-name">{t('pairing.deviceName')}</label>
          <input
            id="pair-name"
            type="text"
            value={deviceName}
            maxLength={64}
            onChange={(e) => {
              setNameTouched(true);
              setDeviceName(e.target.value);
            }}
          />
        </div>
        <div className="field">
          <label htmlFor="pair-totp">{t('pairing.totp')}</label>
          <input
            id="pair-totp"
            type="text"
            inputMode="numeric"
            autoComplete="one-time-code"
            maxLength={10}
            value={totp}
            onChange={(e) => setTotp(e.target.value)}
            aria-describedby="pair-totp-help"
          />
          <span id="pair-totp-help" className={styles.help}>
            {t('pairing.totpHelp')}
          </span>
        </div>
        {error && (
          <p role="alert" className={styles.error}>
            {error}
          </p>
        )}
        {paired && (
          <p role="status" className={styles.success}>
            {paired}
          </p>
        )}
        <div className={styles.actions}>
          <button type="submit" className="btn btn-primary" disabled={busy}>
            {busy ? t('pairing.pairing') : t('pairing.submit')}
          </button>
          <button type="button" className="btn" disabled aria-describedby="pair-scan-later">
            <Icon name="qr" size={18} />
            {t('pairing.scan')}
          </button>
        </div>
        <p id="pair-scan-later" className="muted">
          {t('pairing.scanLater')}
        </p>
      </form>
      <p className={styles.note}>{t('pairing.note')}</p>
    </section>
  );
}
