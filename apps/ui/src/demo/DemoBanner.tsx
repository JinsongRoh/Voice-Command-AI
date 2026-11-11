import { useI18n } from '../i18n/context';
import { Icon } from '../components/Icon';
import type { DemoControls } from './context';
import styles from './DemoBanner.module.css';

/** Always on top while in the demo: what this is, and the way out. */
export function DemoBanner({ demo }: { demo: DemoControls }) {
  const { t } = useI18n();
  return (
    <div
      className={styles.banner}
      role="region"
      aria-label={t('demo.badge')}
      data-testid="demo-banner"
    >
      <span className={styles.badge}>{t('demo.badge')}</span>
      <span className={styles.text}>{t('demo.banner')}</span>
      <button type="button" className={styles.exit} onClick={demo.exit}>
        {t('demo.exit')}
      </button>
    </div>
  );
}

/** Stands in for screens that need a real computer (pairing more devices). */
export function DemoUnavailable({ demo, body }: { demo: DemoControls; body: string }) {
  const { t } = useI18n();
  return (
    <section
      className={`panel ${styles.unavailable}`}
      aria-labelledby="demo-unavailable"
      data-testid="demo-unavailable"
    >
      <h2 id="demo-unavailable">
        <Icon name="info" size={20} /> {t('demo.unavailable')}
      </h2>
      <p>{body}</p>
      <button type="button" className="btn btn-primary" onClick={demo.exit}>
        {t('demo.exit')}
      </button>
    </section>
  );
}

/** "Try the demo" on the first-run pairing screen. */
export function DemoEntry({ onTry }: { onTry: () => void }) {
  const { t } = useI18n();
  return (
    <div className={styles.entry} data-testid="demo-entry">
      <p className={styles.entryText}>{t('demo.tryHelp')}</p>
      <button type="button" className="btn" onClick={onTry}>
        <Icon name="play" size={18} />
        {t('demo.try')}
      </button>
    </div>
  );
}
