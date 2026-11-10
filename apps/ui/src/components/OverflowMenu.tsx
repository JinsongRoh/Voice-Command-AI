import { useEffect, useId, useRef, useState, type ReactNode } from 'react';

import { Icon, type IconName } from './Icon';
import { hasOpenModal } from './Modal';
import styles from './OverflowMenu.module.css';

/**
 * A "more" button with a small panel of actions (the header on narrow phones,
 * the session top bar on phones). Closes on Escape (focus returns to the
 * button) and on a press outside, but never while a dialog opened from one
 * of its items is still up (that dialog may live inside the panel).
 */
export function OverflowMenu({
  label,
  badge,
  children,
  testId,
  onOpenChange,
  icon = 'more',
  buttonRef: outerRef,
}: {
  /** The button's accessible name (include any badge count in it). */
  label: string;
  /** A small count drawn on the button (screen readers hear it in `label`). */
  badge?: number;
  /** The panel's items; `close` closes it (call it before an item's action). */
  children: (close: () => void) => ReactNode;
  testId?: string;
  onOpenChange?: (open: boolean) => void;
  /** The button's icon (a tab menu shows the current tab). */
  icon?: IconName;
  /** The button element, for callers that move the focus back to it. */
  buttonRef?: (el: HTMLButtonElement | null) => void;
}) {
  const [open, setOpenState] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const panelId = useId();
  const setOpen = (next: boolean) => {
    setOpenState(next);
    onOpenChange?.(next);
  };
  const setOpenRef = useRef(setOpen);
  useEffect(() => {
    setOpenRef.current = setOpen;
  });

  useEffect(() => {
    if (!open) return;
    const onPointer = (e: PointerEvent) => {
      if (hasOpenModal()) return;
      if (!wrapRef.current?.contains(e.target as Node)) setOpenRef.current(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || hasOpenModal()) return;
      setOpenRef.current(false);
      buttonRef.current?.focus();
    };
    document.addEventListener('pointerdown', onPointer);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('pointerdown', onPointer);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  return (
    <div className={styles.overflow} ref={wrapRef}>
      <button
        ref={(el) => {
          buttonRef.current = el;
          outerRef?.(el);
        }}
        type="button"
        className={`icon-btn ${styles.button}`}
        aria-label={label}
        title={label}
        aria-haspopup="true"
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        data-testid={testId}
        onClick={() => setOpen(!open)}
      >
        <Icon name={icon} size={20} />
        {!!badge && badge > 0 && (
          <span className={`badge ${styles.badge}`} aria-hidden="true">
            {badge}
          </span>
        )}
      </button>
      {open && (
        <div id={panelId} className={styles.menu} role="group" aria-label={label}>
          {children(() => setOpen(false))}
        </div>
      )}
    </div>
  );
}
