import { useI18n } from '../i18n/context';
import { Icon } from './Icon';
import styles from './ConnectPhoneView.module.css';

/** Desktop pairing screen: the way to the other direction (phone to this PC). */
export function ConnectPhoneCard({ onOpen }: { onOpen: () => void }) {
  const { t } = useI18n();
  return (
    <section className={`panel ${styles.section}`} aria-labelledby="connect-phone-card">
      <h2 id="connect-phone-card" className={styles.cardTitle}>
        {t('host.connectPhone')}
      </h2>
      <p className={styles.help}>{t('host.connectPhoneHelp')}</p>
      <button type="button" className="btn btn-primary" onClick={onOpen}>
        <Icon name="qr" size={18} />
        {t('host.showQr')}
      </button>
    </section>
  );
}
