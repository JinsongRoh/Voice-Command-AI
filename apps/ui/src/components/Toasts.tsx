import { useEffect } from 'react';

import { useI18n } from '../i18n/context';
import type { Toast } from '../state/reducer';
import { useActions, useAppState } from '../state/store';
import { Icon } from './Icon';
import styles from './Toasts.module.css';

const AUTO_DISMISS_MS = 7000;

function ToastItem({ toast, onOpen }: { toast: Toast; onOpen: (id: string) => void }) {
  const { t } = useI18n();
  const actions = useActions();
  useEffect(() => {
    const timer = setTimeout(() => actions.dismissToast(toast.id), AUTO_DISMISS_MS);
    return () => clearTimeout(timer);
  }, [actions, toast.id]);

  return (
    <li className={`${styles.toast} ${styles[toast.category]}`}>
      <Icon name="bell" size={18} />
      <div className={styles.text}>
        <strong>{toast.title}</strong>
        {toast.body && <span>{toast.body}</span>}
      </div>
      {toast.sessionId && (
        <button
          type="button"
          className="btn btn-sm"
          onClick={() => {
            onOpen(toast.sessionId!);
            actions.dismissToast(toast.id);
          }}
        >
          {t('toast.open')}
        </button>
      )}
      <button
        type="button"
        className="icon-btn"
        aria-label={t('toast.dismiss')}
        onClick={() => actions.dismissToast(toast.id)}
      >
        <Icon name="close" size={16} />
      </button>
    </li>
  );
}

/** In-app alerts (NTF-02); announced politely to screen readers. */
export function Toasts({ onOpen }: { onOpen: (id: string) => void }) {
  const { t } = useI18n();
  const toasts = useAppState((s) => s.toasts);
  return (
    <section className={styles.region} aria-label={t('toast.region')}>
      <ol className={styles.list} aria-live="polite">
        {toasts.map((toast) => (
          <ToastItem key={toast.id} toast={toast} onOpen={onOpen} />
        ))}
      </ol>
    </section>
  );
}
