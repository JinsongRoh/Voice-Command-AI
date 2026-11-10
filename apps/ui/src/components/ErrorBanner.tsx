import { useEffect, useState } from 'react';

import { useI18n } from '../i18n/context';
import { Icon } from './Icon';
import styles from './ErrorBanner.module.css';

/** How long a banner stays when nobody looks at its details. */
export const ERROR_BANNER_MS = 6000;

/**
 * A short, friendly error with an optional "Details" toggle for the raw
 * server text and a close button. It goes away on its own after
 * `ERROR_BANNER_MS`, unless the details are open.
 */
export function ErrorBanner({
  message,
  detail,
  onDismiss,
  autoHideMs = ERROR_BANNER_MS,
}: {
  message: string;
  detail?: string | null;
  onDismiss(): void;
  autoHideMs?: number;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (open || autoHideMs <= 0) return;
    const timer = setTimeout(onDismiss, autoHideMs);
    return () => clearTimeout(timer);
  }, [open, autoHideMs, onDismiss, message]);
  return (
    <div className={styles.banner} role="alert">
      <div className={styles.row}>
        <span className={styles.message}>{message}</span>
        {detail && (
          <button
            type="button"
            className={styles.details}
            aria-expanded={open}
            onClick={() => setOpen((v) => !v)}
          >
            {t(open ? 'attach.hideDetails' : 'attach.details')}
          </button>
        )}
        <button
          type="button"
          className={styles.close}
          aria-label={t('common.close')}
          onClick={onDismiss}
        >
          <Icon name="close" size={16} />
        </button>
      </div>
      {open && detail && <p className={styles.detail}>{detail}</p>}
    </div>
  );
}
