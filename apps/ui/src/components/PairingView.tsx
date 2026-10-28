import { useI18n } from '../i18n/context';
import { Icon } from './Icon';
import styles from './PairingView.module.css';

/**
 * Placeholder for device pairing (SEC-01/02). The QR scanner and the Noise
 * handshake are provided by the native shell (Tauri) later.
 */
export function PairingView() {
  const { t } = useI18n();
  return (
    <section className={`panel ${styles.pairing}`} aria-labelledby="pair-title">
      <h2 id="pair-title">{t('pairing.title')}</h2>
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
