import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from 'react';

import { useI18n } from '../i18n/context';
import { folderName, originLabel, providerLabel } from '../protocol/helpers';
import { FEATURE_SESSION_ORDER, type SessionId, type SessionInfo } from '../protocol/types';
import { isWorkSession } from '../state/reducer';
import { moveBy, moveTo, placeVisible } from '../state/sessionOrder';
import { useActions, useAppState } from '../state/store';
import { AgentIcon } from './AgentIcon';
import { Icon } from './Icon';
import { useRemoveFromVca } from './RemoveSession';
import { useDeviceScope } from './ShellProvider';
import styles from './SessionOrderPanel.module.css';

/** How long a finger rests on a handle before the row follows it. */
export const LONG_PRESS_MS = 300;
/** Movement before the long press that cancels it (the finger scrolls). */
const SLOP_PX = 8;
/** Distance from the screen edge where a drag scrolls the page. */
const EDGE_PX = 64;
/** Fastest scroll while dragging, per frame. */
const MAX_SCROLL_PX = 14;

interface Drag {
  id: SessionId;
  pointerId: number;
  /** Where the press started. */
  startX: number;
  startY: number;
  /** Last pointer position. */
  y: number;
  /** The row follows the pointer (after the long press). */
  active: boolean;
  timer: ReturnType<typeof setTimeout> | null;
  frame: number | null;
  /** The order shown while dragging. */
  preview: SessionId[];
  /** Removes the window listeners of this drag. */
  detach: () => void;
}

function placeOf(session: SessionInfo): string {
  return session.origin.type === 'local'
    ? `${originLabel(session.origin)} · ${folderName(session.origin.project_dir)}`
    : providerLabel(session.origin.provider);
}

/**
 * "Session order" in the Windows menu: every session of the PC in the order
 * the user chose, made for touch. A long press (300 ms) on a handle picks a
 * row up and it follows the finger, scrolling near the screen edges; the ▲ ▼
 * buttons do the same one step at a time. The order is kept on the PC, so
 * every device shows the same cards and numbers. Each row also offers
 * "Remove from VCA" (see RemoveSession).
 */
export function SessionOrderPanel() {
  const { t } = useI18n();
  const actions = useActions();
  const scope = useDeviceScope();
  const order = useAppState((s) => s.order);
  const sessions = useAppState((s) => s.sessions);
  const custom = useAppState((s) => s.customOrder);
  const supported = useAppState(
    (s) => s.connection === 'connected' && s.serverFeatures.includes(FEATURE_SESSION_ORDER),
  );
  const canEdit = scope !== 'view';
  const [preview, setPreview] = useState<SessionId[] | null>(null);
  const [dragId, setDragId] = useState<SessionId | null>(null);
  const [announce, setAnnounce] = useState('');
  const [error, setError] = useState<string | null>(null);
  const drag = useRef<Drag | null>(null);
  const rows = useRef(new Map<SessionId, HTMLLIElement>());
  const removal = useRemoveFromVca((session, problem) =>
    setError(t('remove.failed', { name: session.display_name, error: problem })),
  );

  useEffect(
    () => () => {
      const d = drag.current;
      if (d?.timer) clearTimeout(d.timer);
      if (d?.frame != null) cancelAnimationFrame(d.frame);
      d?.detach();
    },
    [],
  );

  if (!supported) return null;

  // The conductor's session has no card and no number: it is not listed.
  const shown = (preview ?? order).filter((id) => isWorkSession(sessions[id]));
  const nameOf = (id: SessionId) => sessions[id]?.display_name ?? id;

  const apply = (arranged: SessionId[], moved: SessionId) => {
    // Hidden sessions keep their places in the full order.
    const visible = arranged.filter((id) => isWorkSession(sessions[id]));
    const next = placeVisible(order, visible);
    if (next === order || next.every((id, i) => id === order[i])) return;
    setError(null);
    const position = visible.indexOf(moved) + 1;
    setAnnounce(t('order.moved', { name: nameOf(moved), position }));
    actions
      .setSessionOrder(next)
      .catch((err: Error) => setError(t('order.failed', { error: err.message })));
  };

  // ---------------------------------------------------------- pointer drag

  /** The index the dragged row would take at the pointer's height. */
  const indexAt = (d: Drag): number => {
    const others = d.preview.filter((id) => id !== d.id);
    let index = 0;
    for (const id of others) {
      const box = rows.current.get(id)?.getBoundingClientRect();
      if (box && d.y > box.top + box.height / 2) index += 1;
    }
    return index;
  };

  const follow = (d: Drag) => {
    const next = moveTo(d.preview, d.id, indexAt(d));
    if (next !== d.preview) {
      d.preview = next;
      setPreview(next);
    }
  };

  /** Scrolls while the finger rests near the top or bottom edge. */
  const autoScroll = () => {
    const d = drag.current;
    if (!d?.active) return;
    const height = window.innerHeight;
    let step = 0;
    if (d.y < EDGE_PX) step = -Math.ceil(((EDGE_PX - d.y) / EDGE_PX) * MAX_SCROLL_PX);
    else if (d.y > height - EDGE_PX)
      step = Math.ceil(((d.y - (height - EDGE_PX)) / EDGE_PX) * MAX_SCROLL_PX);
    if (step !== 0) {
      const scroller = scrollParent(rows.current.get(d.id) ?? null);
      scroller.scrollTop += step;
      follow(d);
    }
    d.frame = requestAnimationFrame(autoScroll);
  };

  const activate = (d: Drag) => {
    d.active = true;
    d.timer = null;
    setPreview(d.preview);
    setDragId(d.id);
    navigator.vibrate?.(10);
    d.frame = requestAnimationFrame(autoScroll);
  };

  const onPointerDown = (id: SessionId) => (e: ReactPointerEvent<HTMLElement>) => {
    if (!canEdit || e.button > 0) return;
    e.preventDefault();
    if (drag.current) cancel();
    // The row moves in the list while it is dragged, which drops a pointer
    // capture: the drag follows the pointer on the window instead.
    const pointerId = e.pointerId;
    const onMove = (ev: PointerEvent) => {
      if (ev.pointerId === pointerId) onPointerMove(ev);
    };
    const onUp = (ev: PointerEvent) => {
      if (ev.pointerId === pointerId) finish(true);
    };
    const onCancel = (ev: PointerEvent) => {
      if (ev.pointerId === pointerId) finish(false);
    };
    window.addEventListener('pointermove', onMove, { passive: false });
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onCancel);
    const d: Drag = {
      id,
      pointerId,
      startX: e.clientX,
      startY: e.clientY,
      y: e.clientY,
      active: false,
      timer: null,
      frame: null,
      preview: order.filter((x) => isWorkSession(sessions[x])),
      detach: () => {
        window.removeEventListener('pointermove', onMove);
        window.removeEventListener('pointerup', onUp);
        window.removeEventListener('pointercancel', onCancel);
      },
    };
    drag.current = d;
    // A mouse drags at once; a finger holds first, so brushing past a handle
    // moves nothing (the rest of the row scrolls the page as usual).
    if (e.pointerType === 'mouse') activate(d);
    else d.timer = setTimeout(() => activate(d), LONG_PRESS_MS);
  };

  const onPointerMove = (e: PointerEvent) => {
    const d = drag.current;
    if (!d || e.pointerId !== d.pointerId) return;
    d.y = e.clientY;
    if (!d.active) {
      if (Math.hypot(e.clientX - d.startX, e.clientY - d.startY) > SLOP_PX) cancel();
      return;
    }
    e.preventDefault();
    follow(d);
  };

  const finish = (commit: boolean) => {
    const d = drag.current;
    drag.current = null;
    if (!d) return;
    d.detach();
    if (d.timer) clearTimeout(d.timer);
    if (d.frame != null) cancelAnimationFrame(d.frame);
    setPreview(null);
    setDragId(null);
    if (commit && d.active) apply(d.preview, d.id);
  };
  const cancel = () => finish(false);

  // ------------------------------------------------------------ rendering

  return (
    <section className={`panel ${styles.block}`} aria-labelledby="session-order-title">
      <div className={styles.head}>
        <h3 id="session-order-title" className={styles.title}>
          {t('order.title')}
        </h3>
        {canEdit && (
          <button
            type="button"
            className="btn btn-sm"
            disabled={!custom}
            onClick={() => {
              setError(null);
              actions
                .resetSessionOrder()
                .catch((err: Error) => setError(t('order.failed', { error: err.message })));
            }}
            data-testid="order-reset"
          >
            <Icon name="restart" size={14} /> {t('order.reset')}
          </button>
        )}
      </div>
      <p className={styles.note}>
        {t('order.hint')} {t(custom ? 'order.isCustom' : 'order.isDefault')}
      </p>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      {shown.length === 0 ? (
        <p className="muted">{t('order.empty')}</p>
      ) : (
        <ol className={styles.list} data-testid="order-list">
          {shown.map((id, i) => {
            const session = sessions[id]!;
            const dragging = dragId === id;
            return (
              <li
                key={id}
                ref={(el) => {
                  if (el) rows.current.set(id, el);
                  else rows.current.delete(id);
                }}
                className={`${styles.row} ${dragging ? styles.dragging : ''}`}
                data-testid={`order-row-${id}`}
              >
                {canEdit && (
                  <span
                    className={styles.handle}
                    role="presentation"
                    title={t('order.handle', { name: session.display_name })}
                    data-testid={`order-handle-${id}`}
                    onPointerDown={onPointerDown(id)}
                    onContextMenu={(e) => e.preventDefault()}
                  >
                    <Icon name="grip" size={20} />
                  </span>
                )}
                <span className={styles.number} aria-hidden="true">
                  {i + 1}
                </span>
                <AgentIcon origin={session.origin} />
                <span className={styles.titles}>
                  <span className={styles.name}>{session.display_name}</span>
                  <span className={styles.place}>{placeOf(session)}</span>
                </span>
                {canEdit && (
                  <span className={styles.arrows}>
                    <button
                      type="button"
                      className="icon-btn"
                      aria-label={t('order.up', { name: session.display_name })}
                      title={t('order.up', { name: session.display_name })}
                      disabled={i === 0}
                      onClick={() => apply(moveBy(order, id, -1, shown), id)}
                    >
                      <Icon name="up" size={18} />
                    </button>
                    <button
                      type="button"
                      className="icon-btn"
                      aria-label={t('order.down', { name: session.display_name })}
                      title={t('order.down', { name: session.display_name })}
                      disabled={i === shown.length - 1}
                      onClick={() => apply(moveBy(order, id, 1, shown), id)}
                    >
                      <Icon name="down" size={18} />
                    </button>
                  </span>
                )}
                {removal.offered(session) && (
                  <button
                    type="button"
                    className={`icon-btn ${styles.remove}`}
                    aria-label={t('ended.removeNamed', { name: session.display_name })}
                    title={t('remove.action')}
                    aria-haspopup={session.state.type === 'exited' ? undefined : 'dialog'}
                    data-testid={`order-remove-${id}`}
                    onClick={() => {
                      setError(null);
                      removal.ask(session);
                    }}
                  >
                    <Icon name="close" size={18} />
                  </button>
                )}
              </li>
            );
          })}
        </ol>
      )}
      <p className="visually-hidden" aria-live="polite">
        {announce}
      </p>
      {removal.dialog}
    </section>
  );
}

/** The element that scrolls `el` (the page when none of its parents do). */
function scrollParent(el: HTMLElement | null): Element {
  for (let p = el?.parentElement ?? null; p; p = p.parentElement) {
    const { overflowY } = getComputedStyle(p);
    if ((overflowY === 'auto' || overflowY === 'scroll') && p.scrollHeight > p.clientHeight) {
      return p;
    }
  }
  return document.scrollingElement ?? document.documentElement;
}
