import { useEffect, useId, useRef, type ReactNode } from 'react';

import { useI18n } from '../i18n/context';
import { Icon } from './Icon';
import styles from './Modal.module.css';

/** Open dialogs, oldest first. */
const openModals: object[] = [];

/** True while a dialog is open. */
// eslint-disable-next-line react-refresh/only-export-components
export function hasOpenModal(): boolean {
  return openModals.length > 0;
}

/**
 * Registers a dialog layer (for overlays that are not a `Modal`, such as the
 * sketch sheet) so that only the newest layer handles Escape and Tab.
 */
// eslint-disable-next-line react-refresh/only-export-components
export function openModalLayer(): { isTop(): boolean; close(): void } {
  const token = {};
  openModals.push(token);
  return {
    isTop: () => openModals[openModals.length - 1] === token,
    close: () => {
      const i = openModals.indexOf(token);
      if (i >= 0) openModals.splice(i, 1);
    },
  };
}

/**
 * Accessible modal dialog: focus moves into the dialog, Tab is trapped,
 * Escape closes and focus returns to the previously focused element.
 * Dialogs may nest (a sheet opening a confirmation); the newest one wins.
 */
export function Modal({
  title,
  onClose,
  children,
  footer,
  fullScreen,
  wide,
  dismissOnBackdrop = true,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  /** Full-screen sheet on phones (a normal dialog on wider screens). */
  fullScreen?: boolean;
  /** Wider dialog on desktop (long lists and texts). */
  wide?: boolean;
  /**
   * A press on the backdrop closes the dialog (default). Forms pass `false`
   * so a stray tap next to the dialog does not throw away what was typed;
   * Escape, the close button and Cancel still close them.
   */
  dismissOnBackdrop?: boolean;
}) {
  const { t } = useI18n();
  const titleId = useId();
  const ref = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  useEffect(() => {
    onCloseRef.current = onClose;
  }, [onClose]);

  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const node = ref.current;
    const focusables = () =>
      Array.from(
        node?.querySelectorAll<HTMLElement>(
          'button:not(:disabled), [href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])',
        ) ?? [],
      );
    (focusables()[1] ?? focusables()[0] ?? node)?.focus();
    const layer = openModalLayer();
    const onKey = (e: KeyboardEvent) => {
      // Only the topmost of nested dialogs handles keys.
      if (!layer.isTop()) return;
      if (e.key === 'Escape') {
        e.stopPropagation();
        onCloseRef.current();
      } else if (e.key === 'Tab') {
        const items = focusables();
        if (items.length === 0) return;
        const first = items[0]!;
        const last = items[items.length - 1]!;
        if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('keydown', onKey);
      layer.close();
      previous?.focus?.();
    };
  }, []);

  return (
    <div
      className={styles.backdrop}
      onMouseDown={(e) => dismissOnBackdrop && e.target === e.currentTarget && onClose()}
    >
      <div
        ref={ref}
        className={`${styles.dialog} ${fullScreen ? styles.full : ''} ${wide ? styles.wide : ''}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
      >
        <header className={styles.header}>
          <h2 id={titleId} className={styles.title}>
            {title}
          </h2>
          <button
            type="button"
            className="icon-btn"
            onClick={onClose}
            aria-label={t('common.close')}
          >
            <Icon name="close" />
          </button>
        </header>
        <div className={styles.body}>{children}</div>
        {footer && <footer className={styles.footer}>{footer}</footer>}
      </div>
    </div>
  );
}

export function ConfirmDialog({
  title,
  message,
  confirmLabel,
  danger,
  onConfirm,
  onCancel,
}: {
  title: string;
  message: string;
  confirmLabel: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const { t } = useI18n();
  return (
    <Modal
      title={title}
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" onClick={onCancel}>
            {t('common.cancel')}
          </button>
          <button
            type="button"
            className={`btn ${danger ? 'btn-danger' : 'btn-primary'}`}
            onClick={onConfirm}
          >
            {confirmLabel}
          </button>
        </>
      }
    >
      <p>{message}</p>
    </Modal>
  );
}
