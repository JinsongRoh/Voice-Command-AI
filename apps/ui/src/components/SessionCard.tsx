import {
  useEffect,
  useId,
  useRef,
  useState,
  type DragEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent,
} from 'react';

import { useI18n } from '../i18n/context';
import {
  folderName,
  isCloud,
  originLabel,
  originTitle,
  providerLabel,
  sessionNeedsAttention,
  unreadCount,
} from '../protocol/helpers';
import type { SessionInfo } from '../protocol/types';
import { AgentIcon } from './AgentIcon';
import { Icon } from './Icon';
import { StateChip } from './StateChip';
import { SubagentBadge } from './SubagentBadge';
import styles from './SessionCard.module.css';

/** Moving the card in the session order (desktop list). */
export interface CardReorder {
  /** Alt+Up / Alt+Down on the focused card. */
  move(offset: -1 | 1): void;
  /** This card is being dragged. */
  dragging: boolean;
  /** A dragged card would land before or after this one. */
  dropAt: 'before' | 'after' | null;
  /**
   * A mouse press on the handle: drags with pointer events, because HTML
   * drag and drop does not start reliably in the Windows desktop app.
   */
  onHandlePointerDown(e: PointerEvent<HTMLElement>): void;
  /** The drag starts at the card's handle (touch; other browsers). */
  onDragStart(e: DragEvent<HTMLElement>): void;
  onDragEnd(): void;
  onDragOver(e: DragEvent<HTMLLIElement>): void;
  onDragLeave(e: DragEvent<HTMLLIElement>): void;
  onDrop(e: DragEvent<HTMLLIElement>): void;
}

export interface SessionCardProps {
  session: SessionInfo;
  preview: string;
  selected: boolean;
  /** 1-based card number (used for voice addressing, VOC-08). */
  index: number;
  onSelect: (id: string) => void;
  /** Shows the card menu (overflow button, long press, right click) with "Rename". */
  onRename?: (id: string) => void;
  /**
   * Adds "Bring to front" to the card menu: this computer's own app only,
   * for sessions running in a terminal here.
   */
  onBringToFront?: (id: string) => void;
  /** Adds "Fix pronunciation" to the card menu (servers with the pronunciation dictionary). */
  onFixPronunciation?: (id: string) => void;
  /** Adds "Reopen" to the menu of an ended session (shells and sessions VCA started). */
  onReopen?: (id: string) => void;
  /** Adds "Remove from VCA" to the card menu (any session; see RemoveSession). */
  onRemove?: (id: string) => void;
  /** Open blocking to-do tasks of this session: "agent is waiting on you". */
  waitingOnYou?: number;
  /** A drag handle and Alt+Up / Alt+Down move the card in the session order. */
  reorder?: CardReorder;
}

/** How long a touch must rest on a card to open its menu. */
const LONG_PRESS_MS = 500;
/** Movement that turns a press into a scroll. */
const LONG_PRESS_SLOP_PX = 10;

export function SessionCard({
  session,
  preview,
  selected,
  index,
  onSelect,
  onRename,
  onBringToFront,
  onFixPronunciation,
  onReopen,
  onRemove,
  waitingOnYou = 0,
  reorder,
}: SessionCardProps) {
  const ended = session.state.type === 'exited';
  const { t, relative } = useI18n();
  const cloud = isCloud(session.origin);
  const location =
    session.origin.type === 'local'
      ? folderName(session.origin.project_dir)
      : providerLabel(session.origin.provider);
  // The conductor's own session: highlighted only for a decision.
  const attention = sessionNeedsAttention(session);
  const unread = unreadCount(session);
  const nameId = useId();
  const detailsId = useId();
  const unreadId = useId();
  const stateId = useId();
  const previewId = useId();
  const menuId = useId();
  const [menuOpen, setMenuOpen] = useState(false);
  const menuButton = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const press = useRef<{
    timer: ReturnType<typeof setTimeout> | null;
    opened: boolean;
    x: number;
    y: number;
  }>({ timer: null, opened: false, x: 0, y: 0 });

  // Focus the first item; close on Escape or a press outside the menu.
  useEffect(() => {
    if (!menuOpen) return;
    menuRef.current?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    const onPointerDown = (e: Event) => {
      const target = e.target as Node;
      if (menuRef.current?.contains(target) || menuButton.current?.contains(target)) return;
      setMenuOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.stopPropagation();
      setMenuOpen(false);
      menuButton.current?.focus();
    };
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [menuOpen]);

  useEffect(() => {
    const p = press.current;
    return () => {
      if (p.timer) clearTimeout(p.timer);
    };
  }, []);

  /** Closes the menu and puts the focus back on its button. */
  const closeMenu = () => {
    menuButton.current?.focus();
    setMenuOpen(false);
  };

  /** Menu keys: arrows and Home / End move between items; Tab closes it. */
  const onMenuKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const items = Array.from(
      menuRef.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [],
    );
    if (items.length === 0) return;
    const at = items.indexOf(document.activeElement as HTMLElement);
    let next: number | null = null;
    if (e.key === 'ArrowDown') next = at < 0 ? 0 : (at + 1) % items.length;
    else if (e.key === 'ArrowUp') next = at <= 0 ? items.length - 1 : at - 1;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = items.length - 1;
    else if (e.key === 'Tab') {
      e.preventDefault();
      closeMenu();
      return;
    }
    if (next === null) return;
    e.preventDefault();
    items[next]!.focus();
  };

  const cancelPress = () => {
    if (press.current.timer) clearTimeout(press.current.timer);
    press.current.timer = null;
  };
  const openFromPress = () => {
    press.current.opened = true;
    setMenuOpen(true);
  };
  const pressHandlers = onRename
    ? {
        onPointerDown: (e: PointerEvent) => {
          press.current.opened = false;
          if (e.pointerType === 'mouse') return;
          cancelPress();
          press.current.x = e.clientX;
          press.current.y = e.clientY;
          press.current.timer = setTimeout(() => {
            press.current.timer = null;
            openFromPress();
          }, LONG_PRESS_MS);
        },
        onPointerMove: (e: PointerEvent) => {
          const dx = e.clientX - press.current.x;
          const dy = e.clientY - press.current.y;
          if (Math.hypot(dx, dy) > LONG_PRESS_SLOP_PX) cancelPress();
        },
        onPointerUp: cancelPress,
        onPointerCancel: cancelPress,
        onPointerLeave: cancelPress,
        // Right click, and long press on platforms that report it this way.
        onContextMenu: (e: { preventDefault(): void }) => {
          e.preventDefault();
          cancelPress();
          openFromPress();
        },
      }
    : {};

  const onKeyDown = reorder
    ? (e: ReactKeyboardEvent) => {
        if (!e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
        if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return;
        e.preventDefault();
        reorder.move(e.key === 'ArrowUp' ? -1 : 1);
      }
    : undefined;
  const itemClass = [
    styles.item,
    onRename ? styles.hasMenu : '',
    reorder ? styles.reorderable : '',
    reorder?.dragging ? styles.dragging : '',
    reorder?.dropAt === 'before' ? styles.dropBefore : '',
    reorder?.dropAt === 'after' ? styles.dropAfter : '',
  ]
    .filter(Boolean)
    .join(' ');

  return (
    <li
      className={itemClass}
      data-testid={`session-item-${session.id}`}
      data-session-item={session.id}
      {...(reorder
        ? {
            onDragOver: reorder.onDragOver,
            onDragLeave: reorder.onDragLeave,
            onDrop: reorder.onDrop,
          }
        : {})}
    >
      <button
        type="button"
        className={`${styles.card} ${selected ? styles.selected : ''} ${attention ? styles.attention : ''}`}
        aria-pressed={selected}
        aria-labelledby={nameId}
        aria-describedby={[detailsId, unread > 0 ? unreadId : '', stateId, previewId]
          .filter(Boolean)
          .join(' ')}
        onClick={() => {
          // The click that ends a long press only opens the menu.
          if (press.current.opened) {
            press.current.opened = false;
            return;
          }
          onSelect(session.id);
        }}
        data-testid={`session-card-${session.id}`}
        onKeyDown={onKeyDown}
        {...pressHandlers}
      >
        <div className={styles.top}>
          <AgentIcon origin={session.origin} />
          <div className={styles.titles}>
            <span className={styles.name} id={nameId}>
              <span className={styles.number} aria-hidden="true">
                {index}
              </span>
              {session.display_name}
            </span>
            <span className={styles.meta} id={detailsId}>
              <span className={styles.agent} title={originTitle(session.origin)}>
                {originLabel(session.origin)}
              </span>
              <span aria-hidden="true">·</span>
              <span
                className={styles.location}
                title={
                  session.origin.type === 'local'
                    ? session.origin.project_dir
                    : session.origin.remote_id
                }
              >
                {location}
              </span>
              {cloud && <span className={styles.cloud}>{t('card.cloud')}</span>}
              {session.conductor && (
                <span
                  className={styles.conductor}
                  title={t('card.conductorTitle')}
                  data-testid="session-conductor-badge"
                >
                  {t('card.conductor')}
                </span>
              )}
              {session.profile && <span className={styles.profile}>{session.profile}</span>}
            </span>
          </div>
          {unread > 0 && (
            <span className="badge" id={unreadId}>
              <span aria-hidden="true">{unread}</span>
              <span className="visually-hidden">{t('card.unread', { count: unread })}</span>
            </span>
          )}
        </div>
        <p className={styles.preview} id={previewId}>
          {preview || t('card.noOutput')}
        </p>
        <div className={styles.bottom} id={stateId}>
          <StateChip state={session.state} terminalPrompt={session.terminal_prompt} />
          <SubagentBadge count={session.subagents} />
          {waitingOnYou > 0 && (
            <span
              className="badge"
              title={t('tasks.card.waitingCount', { count: waitingOnYou })}
              data-testid="waiting-on-you"
            >
              {t('tasks.card.waiting')}
            </span>
          )}
          <time className={styles.time} dateTime={new Date(session.last_activity).toISOString()}>
            {t('card.lastActivity', { time: relative(session.last_activity) })}
          </time>
        </div>
      </button>
      {reorder && (
        // Mouse only: keyboards use Alt+Up / Alt+Down on the card, phones
        // the order list in the Windows menu.
        <span
          className={styles.handle}
          draggable
          title={t('order.cardHandle', { name: session.display_name })}
          aria-hidden="true"
          data-testid={`session-handle-${session.id}`}
          onPointerDown={reorder.onHandlePointerDown}
          onDragStart={reorder.onDragStart}
          onDragEnd={reorder.onDragEnd}
        >
          <Icon name="grip" size={18} />
        </span>
      )}
      {onRename && (
        <>
          <button
            ref={menuButton}
            type="button"
            className={`icon-btn ${styles.menuButton}`}
            aria-label={t('card.menu', { name: session.display_name })}
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            aria-controls={menuOpen ? menuId : undefined}
            onClick={() => setMenuOpen((open) => !open)}
          >
            <Icon name="more" size={18} />
          </button>
          {menuOpen && (
            <div
              ref={menuRef}
              id={menuId}
              role="menu"
              aria-label={t('card.menu', { name: session.display_name })}
              className={styles.menu}
              onKeyDown={onMenuKeyDown}
            >
              <button
                type="button"
                role="menuitem"
                tabIndex={-1}
                className={styles.menuItem}
                onClick={() => {
                  // The dialog returns focus to the menu button when it closes.
                  closeMenu();
                  onRename(session.id);
                }}
              >
                <Icon name="edit" size={16} />
                {t('rename.action')}
              </button>
              {onFixPronunciation && (
                <button
                  type="button"
                  role="menuitem"
                  tabIndex={-1}
                  className={styles.menuItem}
                  onClick={() => {
                    closeMenu();
                    onFixPronunciation(session.id);
                  }}
                >
                  <Icon name="speaker" size={16} />
                  {t('pron.fix')}
                </button>
              )}
              {onBringToFront && (
                <button
                  type="button"
                  role="menuitem"
                  tabIndex={-1}
                  className={styles.menuItem}
                  onClick={() => {
                    closeMenu();
                    onBringToFront(session.id);
                  }}
                >
                  <Icon name="windows" size={16} />
                  {t('windows.front')}
                </button>
              )}
              {ended && onReopen && (
                <button
                  type="button"
                  role="menuitem"
                  tabIndex={-1}
                  className={styles.menuItem}
                  onClick={() => {
                    closeMenu();
                    onReopen(session.id);
                  }}
                >
                  <Icon name="restart" size={16} />
                  {t('ended.reopen')}
                </button>
              )}
              {onRemove && (
                <button
                  type="button"
                  role="menuitem"
                  tabIndex={-1}
                  className={styles.menuItem}
                  onClick={() => {
                    closeMenu();
                    onRemove(session.id);
                  }}
                >
                  <Icon name="close" size={16} />
                  {t('remove.action')}
                </button>
              )}
            </div>
          )}
        </>
      )}
    </li>
  );
}
