import { useRef, useState, type ReactNode, type TouchEvent } from 'react';

import styles from './PullToRefresh.module.css';

/** Pull distance (px) that triggers a refresh on release. */
export const PULL_THRESHOLD = 64;
/** The indicator stops growing here. */
const PULL_MAX = 96;

export interface PullLabels {
  pull: string;
  release: string;
  refreshing: string;
}

/**
 * Pull down on the wrapped content (touch) to refresh. Sideways drags (the
 * session row swipes left and right) never count as a pull.
 */
export function PullToRefresh({
  onRefresh,
  labels,
  disabled = false,
  children,
}: {
  onRefresh: () => Promise<unknown>;
  labels: PullLabels;
  disabled?: boolean;
  children: ReactNode;
}) {
  const start = useRef<{ x: number; y: number } | null>(null);
  const [pull, setPull] = useState(0);
  const [busy, setBusy] = useState(false);

  const onTouchStart = (e: TouchEvent) => {
    const touch = e.touches[0];
    start.current = disabled || busy || !touch ? null : { x: touch.clientX, y: touch.clientY };
  };
  const onTouchMove = (e: TouchEvent) => {
    const from = start.current;
    const touch = e.touches[0];
    if (!from || !touch) return;
    const dx = touch.clientX - from.x;
    const dy = touch.clientY - from.y;
    if (dy <= 0 || Math.abs(dx) >= dy) {
      if (pull !== 0) setPull(0);
      return;
    }
    setPull(Math.min(dy, PULL_MAX));
  };
  const onTouchEnd = () => {
    const pulled = pull;
    start.current = null;
    setPull(0);
    if (pulled < PULL_THRESHOLD || busy) return;
    setBusy(true);
    void onRefresh()
      .catch(() => {})
      .finally(() => setBusy(false));
  };

  const shown = busy || pull > 0;
  return (
    <div
      className={styles.wrap}
      onTouchStart={onTouchStart}
      onTouchMove={onTouchMove}
      onTouchEnd={onTouchEnd}
      onTouchCancel={() => {
        start.current = null;
        setPull(0);
      }}
    >
      {shown && (
        <div
          className={styles.indicator}
          role="status"
          data-testid="pull-indicator"
          style={{ height: busy ? 28 : Math.max(16, pull / 2) }}
        >
          <span className={busy ? styles.spin : undefined} aria-hidden="true">
            ↻
          </span>
          <span>
            {busy ? labels.refreshing : pull >= PULL_THRESHOLD ? labels.release : labels.pull}
          </span>
        </div>
      )}
      {children}
    </div>
  );
}
