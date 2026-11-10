import { useEffect, useRef, useState, type FocusEvent } from 'react';

import { useI18n } from '../i18n/context';
import { isStickyToast, type Toast } from '../state/reducer';
import { useActions, useAppState } from '../state/store';
import { Icon } from './Icon';
import styles from './Toasts.module.css';

const AUTO_DISMISS_MS = 7000;

/**
 * One toast. Plain notices close by themselves after a few seconds; the timer
 * pauses while the pointer is over the toast or focus is inside it, so it does
 * not vanish while being read or used. A decision or question alert stays
 * until the person answers it with Go or Later; an input alert offers the
 * same buttons but closes by itself.
 */
function ToastItem({ toast, onOpen }: { toast: Toast; onOpen: (id: string) => void }) {
  const { t } = useI18n();
  const actions = useActions();
  const sticky = isStickyToast(toast);
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const paused = hovered || focused;
  /** Time left before the toast closes; kept across pauses. */
  const remaining = useRef(AUTO_DISMISS_MS);
  useEffect(() => {
    if (sticky || paused) return;
    const started = Date.now();
    const timer = setTimeout(() => actions.dismissToast(toast.id), remaining.current);
    return () => {
      clearTimeout(timer);
      remaining.current = Math.max(0, remaining.current - (Date.now() - started));
    };
  }, [actions, toast.id, sticky, paused]);

  const onBlur = (e: FocusEvent<HTMLLIElement>) => {
    // Focus moving between the toast's own buttons keeps it paused.
    if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setFocused(false);
  };

  return (
    <li
      className={`${styles.toast} ${styles[toast.category]}`}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      onFocus={() => setFocused(true)}
      onBlur={onBlur}
    >
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
          {t(toast.alertKey ? 'alerts.go' : 'toast.open')}
        </button>
      )}
      {toast.action && (
        <button
          type="button"
          className="btn btn-sm"
          onClick={() => {
            toast.action?.run();
            actions.dismissToast(toast.id);
          }}
        >
          {toast.action.label}
        </button>
      )}
      {toast.alertKey ? (
        <button type="button" className="btn btn-sm" onClick={() => actions.dismissToast(toast.id)}>
          {t('alerts.later')}
        </button>
      ) : (
        <button
          type="button"
          className="icon-btn"
          aria-label={t('toast.dismiss')}
          onClick={() => actions.dismissToast(toast.id)}
        >
          <Icon name="close" size={16} />
        </button>
      )}
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
