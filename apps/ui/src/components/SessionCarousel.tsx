import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react';

import { useMediaQuery } from '../hooks/useMediaQuery';
import { useI18n } from '../i18n/context';
import {
  folderName,
  isShellSession,
  middleEllipsis,
  originLabel,
  providerLabel,
  sessionLocation,
  sessionNeedsAttention,
  unreadCount,
} from '../protocol/helpers';
import type { SessionInfo } from '../protocol/types';
import { AgentIcon } from './AgentIcon';
import { Icon } from './Icon';
import { ShellBadge, ShellCountdown } from './ShellCountdown';
import { StateChip } from './StateChip';
import { SubagentBadge } from './SubagentBadge';
import { REDUCED_MOTION_QUERY } from './VoiceWave';
import styles from './SessionCarousel.module.css';

/** localStorage flag: the swipe tutorial has been shown and dismissed. */
export const SWIPE_TUTORIAL_KEY = 'vca.tutorial.sessionSwipe';
/** The tutorial dismisses itself after this long. */
export const SWIPE_TUTORIAL_MS = 6000;
/** Scroll events this soon after a programmatic scroll are not the user's. */
const PROGRAMMATIC_SCROLL_MS = 800;
/** Share of a card that must be on screen for it to count as visible. */
const VISIBLE_SHARE = 0.6;

function tutorialSeen(): boolean {
  try {
    return globalThis.localStorage?.getItem(SWIPE_TUTORIAL_KEY) === '1';
  } catch {
    return false;
  }
}

function rememberTutorial() {
  try {
    globalThis.localStorage?.setItem(SWIPE_TUTORIAL_KEY, '1');
  } catch {
    // Storage unavailable (private mode, quota): the hint may show again.
  }
}

interface Edges {
  start: boolean;
  end: boolean;
}
interface Range {
  from: number;
  to: number;
}

export interface SessionCarouselProps {
  /** Sessions to show, in deck order. */
  sessions: SessionInfo[];
  /** 1-based voice-addressing number of a session (VOC-08). */
  numberOf: (id: string) => number;
  selectedId: string | null;
  onSelect: (id: string) => void;
  /** Rename button on the selected card (it carries the session details on phones). */
  onRename?: (id: string) => void;
  /** Whether an ended session can be reopened (shells, sessions VCA started). */
  canReopen?: (session: SessionInfo) => boolean;
  /** Reopen button on the selected card when it ended. */
  onReopen?: (id: string) => void;
  /** Remove-from-list button on the selected card when it ended. */
  onRemove?: (id: string) => void;
  /** Controls shown in the row above the cards (filters, "New session"). */
  children?: ReactNode;
  /** Open blocking to-do tasks of a session ("agent is waiting on you"). */
  waitingOf?: (id: string) => number;
  /**
   * The phone session strip under the top bar: one row of short cards, no
   * head row, no per-card details or buttons (the top bar and its "more"
   * menu have them), and `trailing` (the search button) after the row.
   */
  compact?: boolean;
  /** Shown after the row of cards (compact strip only). */
  trailing?: ReactNode;
}

/**
 * Phone layout of the deck: one row of compact session cards that the user
 * swipes left and right (native horizontal scrolling with scroll snap, so
 * touch momentum works as everywhere else). The next card peeks at the edge,
 * a fade appears on each side that has more cards, and a counter shows which
 * cards are on screen. The first time there are two or more cards, a hint
 * with a swiping hand explains the gesture once.
 */
export function SessionCarousel({
  sessions,
  numberOf,
  selectedId,
  onSelect,
  onRename,
  canReopen,
  onReopen,
  onRemove,
  children,
  waitingOf,
  compact = false,
  trailing,
}: SessionCarouselProps) {
  const { t } = useI18n();
  const reduced = useMediaQuery(REDUCED_MOTION_QUERY);
  const track = useRef<HTMLUListElement>(null);
  const programmaticAt = useRef(0);
  const [edges, setEdges] = useState<Edges>({ start: false, end: false });
  const [range, setRange] = useState<Range | null>(null);
  const [tutorialOpen, setTutorialOpen] = useState(() => !tutorialSeen());
  const showTutorial = tutorialOpen && sessions.length >= 2;
  const showTutorialRef = useRef(showTutorial);
  useEffect(() => {
    showTutorialRef.current = showTutorial;
  });

  const dismissTutorial = useCallback(() => {
    if (!showTutorialRef.current) return;
    showTutorialRef.current = false;
    setTutorialOpen(false);
    rememberTutorial();
  }, []);

  /** Recomputes the edge fades and the "from–to / total" counter. */
  const measure = useCallback(() => {
    const el = track.current;
    if (!el) return;
    const left = Math.abs(el.scrollLeft);
    const max = el.scrollWidth - el.clientWidth;
    const next: Edges = { start: left > 2, end: max - left > 2 };
    setEdges((prev) => (prev.start === next.start && prev.end === next.end ? prev : next));
    if (el.clientWidth === 0) {
      // Not laid out (hidden, or a test DOM): the counter follows the selection.
      setRange(null);
      return;
    }
    const viewStart = left;
    const viewEnd = left + el.clientWidth;
    let from = 0;
    let to = 0;
    const items = [...el.children] as HTMLElement[];
    items.forEach((item, i) => {
      const start = item.offsetLeft;
      const end = start + item.offsetWidth;
      const shown = Math.min(end, viewEnd) - Math.max(start, viewStart);
      if (item.offsetWidth > 0 && shown >= item.offsetWidth * VISIBLE_SHARE) {
        if (!from) from = i + 1;
        to = i + 1;
      }
    });
    if (!from) {
      setRange(null);
      return;
    }
    setRange((prev) => (prev && prev.from === from && prev.to === to ? prev : { from, to }));
  }, []);

  useLayoutEffect(() => {
    measure();
  }, [measure, sessions]);

  useEffect(() => {
    const el = track.current;
    if (!el) return;
    if (typeof ResizeObserver !== 'undefined') {
      const observer = new ResizeObserver(() => measure());
      observer.observe(el);
      return () => observer.disconnect();
    }
    window.addEventListener('resize', measure);
    return () => window.removeEventListener('resize', measure);
  }, [measure]);

  // A vertical mouse wheel scrolls the row sideways (touch needs no help).
  useEffect(() => {
    const el = track.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      if (Math.abs(e.deltaY) <= Math.abs(e.deltaX)) return;
      if (el.scrollWidth <= el.clientWidth) return;
      e.preventDefault();
      el.scrollLeft += e.deltaY;
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    return () => el.removeEventListener('wheel', onWheel);
  }, []);

  // Keep the selected card fully on screen when the selection or the set of
  // cards changes, but not on every update (output events re-render the deck
  // and must not pull the row back while the user browses it).
  const idsKey = sessions.map((s) => s.id).join('\n');
  useEffect(() => {
    const el = track.current;
    if (!el || !selectedId || el.clientWidth === 0) return;
    const item = [...el.children].find(
      (c) => (c as HTMLElement).dataset.sessionId === selectedId,
    ) as HTMLElement | undefined;
    if (!item) return;
    const left = Math.abs(el.scrollLeft);
    const start = item.offsetLeft;
    const end = start + item.offsetWidth;
    if (start >= left && end <= left + el.clientWidth) return;
    programmaticAt.current = Date.now();
    el.scrollTo?.({
      left: Math.max(0, start - (el.clientWidth - item.offsetWidth) / 2),
      behavior: reduced ? 'auto' : 'smooth',
    });
  }, [selectedId, idsKey, reduced]);

  // The hint goes away on its own after a few seconds.
  useEffect(() => {
    if (!showTutorial) return;
    const timer = setTimeout(dismissTutorial, SWIPE_TUTORIAL_MS);
    return () => clearTimeout(timer);
  }, [showTutorial, dismissTutorial]);

  const onScroll = () => {
    measure();
    if (Date.now() - programmaticAt.current > PROGRAMMATIC_SCROLL_MS) dismissTutorial();
  };

  /** Edge buttons (pointer users): one screen of cards back or forward. */
  const page = (dir: -1 | 1) => {
    const el = track.current;
    if (!el) return;
    el.scrollBy?.({ left: dir * el.clientWidth * 0.8, behavior: reduced ? 'auto' : 'smooth' });
  };

  const total = sessions.length;
  const selectedIndex = sessions.findIndex((s) => s.id === selectedId);
  const from = range?.from ?? (selectedIndex >= 0 ? selectedIndex + 1 : 1);
  const to = range?.to ?? from;
  const counter =
    from === to
      ? t('carousel.position', { current: from, total })
      : t('carousel.range', { from, to, total });

  const viewport = (
    <div
      className={styles.viewport}
      data-more-start={edges.start || undefined}
      data-more-end={edges.end || undefined}
      onPointerDown={dismissTutorial}
      onKeyDown={dismissTutorial}
      // Capture phase: keyboard and assistive activation (no pointer events)
      // dismisses the hint before the card handles the click.
      onClickCapture={dismissTutorial}
    >
      <ul
        ref={track}
        className={styles.track}
        aria-label={t('carousel.label')}
        onScroll={onScroll}
        onWheel={dismissTutorial}
      >
        {sessions.map((s) => (
          <CompactCard
            key={s.id}
            session={s}
            number={numberOf(s.id)}
            selected={s.id === selectedId}
            onSelect={onSelect}
            onRename={compact ? undefined : onRename}
            onReopen={!compact && canReopen?.(s) ? onReopen : undefined}
            onRemove={compact ? undefined : onRemove}
            waiting={waitingOf?.(s.id) ?? 0}
            compact={compact}
          />
        ))}
      </ul>
      <button
        type="button"
        className={`${styles.edge} ${styles.edgeStart}`}
        tabIndex={-1}
        aria-hidden="true"
        onClick={() => page(-1)}
      >
        <Icon name="back" size={16} />
      </button>
      <button
        type="button"
        className={`${styles.edge} ${styles.edgeEnd}`}
        tabIndex={-1}
        aria-hidden="true"
        onClick={() => page(1)}
      >
        <Icon name="back" size={16} className={styles.flip} />
      </button>
      {showTutorial && <SwipeHint label={t('carousel.hint')} />}
    </div>
  );

  if (compact) {
    return (
      <div className={`${styles.carousel} ${styles.compact}`} data-compact>
        {viewport}
        {trailing}
      </div>
    );
  }
  return (
    <div className={styles.carousel}>
      <div className={styles.head}>
        {children}
        {total > 1 && (
          <span className={styles.counter} data-testid="carousel-counter">
            <span aria-hidden="true">{counter}</span>
            <span className="visually-hidden">{t('carousel.rangeLabel', { from, to, total })}</span>
          </span>
        )}
      </div>
      {viewport}
    </div>
  );
}

/**
 * One card of the row. The selected card is wider and also carries what the
 * live view header shows on wide screens (folder, control tier, start time,
 * rename), so phones do not repeat the session details twice.
 */
function CompactCard({
  session,
  number,
  selected,
  onSelect,
  onRename,
  onReopen,
  onRemove,
  waiting = 0,
  compact = false,
}: {
  session: SessionInfo;
  number: number;
  selected: boolean;
  onSelect: (id: string) => void;
  onRename?: (id: string) => void;
  onReopen?: (id: string) => void;
  onRemove?: (id: string) => void;
  waiting?: number;
  /** The phone session strip: no details line on the selected card. */
  compact?: boolean;
}) {
  const ended = session.state.type === 'exited' && (!!onReopen || !!onRemove);
  const { t, relative } = useI18n();
  const nameId = useId();
  const detailsId = useId();
  const extraId = useId();
  const shell = isShellSession(session);
  const location =
    session.origin.type === 'local'
      ? folderName(session.origin.project_dir)
      : providerLabel(session.origin.provider);
  // The conductor's own session: highlighted only for a decision.
  const attention = sessionNeedsAttention(session);
  const unread = unreadCount(session);

  return (
    <li
      className={`${styles.slide} ${selected ? styles.slideSelected : ''}`}
      data-session-id={session.id}
      data-ended={(selected && ended) || undefined}
    >
      <button
        type="button"
        className={`${styles.card} ${selected ? styles.selected : ''} ${attention ? styles.attention : ''}`}
        aria-pressed={selected}
        aria-labelledby={nameId}
        aria-describedby={selected && !compact ? `${detailsId} ${extraId}` : detailsId}
        data-testid={`session-card-${session.id}`}
        title={location}
        onClick={() => onSelect(session.id)}
      >
        <AgentIcon origin={session.origin} size={28} />
        {/* Two lines: number + name (+ unread), then state + last activity. */}
        <span className={styles.body}>
          <span className={styles.name} id={nameId}>
            <span className={styles.number} aria-hidden="true">
              {number}
            </span>
            {/* The number is what you say ("Voco, session 3"): keep it in the name. */}
            <span className="visually-hidden">{t('voice.sessionNumber', { number })}, </span>
            <span className={styles.nameText}>{session.display_name}</span>
            {shell && <ShellBadge compact />}
            {session.conductor && (
              <span
                className={styles.conductor}
                title={t('card.conductorTitle')}
                data-testid="session-conductor-badge"
              >
                {t('card.conductor')}
              </span>
            )}
            {unread > 0 && (
              <span className={`badge ${styles.unread}`} aria-hidden="true">
                {unread}
              </span>
            )}
          </span>
          <span className={styles.bottom} id={detailsId}>
            <StateChip state={session.state} terminalPrompt={session.terminal_prompt} />
            <SubagentBadge count={session.subagents} compact />
            {waiting > 0 && (
              <span
                className="badge"
                title={t('tasks.card.waitingCount', { count: waiting })}
                data-testid="waiting-on-you"
              >
                <Icon name="clock" size={12} />
                <span className="visually-hidden">{t('tasks.card.waiting')}</span>
              </span>
            )}
            {selected && shell && <ShellCountdown session={session} compact />}
            {selected && !shell && (
              <span
                className={styles.tier}
                title={t('tier.howTitle', { how: t(`tier.how.${session.control}`) })}
              >
                {t(`tier.${session.control}`)}
              </span>
            )}
            <time className={styles.time} dateTime={new Date(session.last_activity).toISOString()}>
              {relative(session.last_activity)}
            </time>
            {unread > 0 && (
              <span className="visually-hidden">{t('card.unread', { count: unread })}</span>
            )}
          </span>
          {selected && !compact && (
            <span className={styles.extra} id={extraId}>
              <span className={styles.path} title={sessionLocation(session)}>
                {session.origin.type === 'local'
                  ? middleEllipsis(session.origin.project_dir, 34)
                  : originLabel(session.origin)}
              </span>
              <span className={styles.started}>
                {t('card.started', { time: relative(session.created_at) })}
              </span>
            </span>
          )}
        </span>
      </button>
      {selected && onRename && (
        <button
          type="button"
          className={`icon-btn ${styles.rename}`}
          aria-label={t('rename.title')}
          title={t('rename.action')}
          onClick={() => onRename(session.id)}
        >
          <Icon name="edit" size={14} />
        </button>
      )}
      {selected && ended && (
        <span className={styles.ended} role="group" aria-label={t('ended.title')}>
          {onReopen && (
            <button
              type="button"
              className={`icon-btn ${styles.endedBtn}`}
              aria-label={t('ended.reopenNamed', { name: session.display_name })}
              title={t('ended.reopen')}
              onClick={() => onReopen(session.id)}
            >
              <Icon name="restart" size={16} />
            </button>
          )}
          {onRemove && (
            <button
              type="button"
              className={`icon-btn ${styles.endedBtn}`}
              aria-label={t('ended.removeNamed', { name: session.display_name })}
              title={t('ended.remove')}
              onClick={() => onRemove(session.id)}
            >
              <Icon name="close" size={16} />
            </button>
          )}
        </span>
      )}
    </li>
  );
}

/** First-run hint: a hand swiping left and right over the cards. */
function SwipeHint({ label }: { label: string }) {
  return (
    <div className={styles.hint} role="status" data-testid="swipe-hint">
      <span className={styles.hintStage} aria-hidden="true">
        <span className={styles.hintTrack}>
          <Icon name="back" size={14} />
          <Icon name="back" size={14} className={styles.flip} />
        </span>
        <svg
          className={styles.hand}
          viewBox="0 0 24 24"
          width="34"
          height="34"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.7"
          strokeLinecap="round"
          strokeLinejoin="round"
          focusable="false"
        >
          <circle className={styles.touch} cx="10.5" cy="4" r="3.2" />
          <path d="M9 12V5.5a1.5 1.5 0 0 1 3 0V11" />
          <path d="M12 10.5V9.8a1.5 1.5 0 0 1 3 0V11" />
          <path d="M15 11v-.2a1.5 1.5 0 0 1 3 0V14a7 7 0 0 1-7 7h-.6a6 6 0 0 1-4.6-2.2l-2.2-2.9a1.5 1.5 0 0 1 2.3-1.9L9 16.2V12" />
        </svg>
      </span>
      <span className={styles.hintText}>{label}</span>
    </div>
  );
}
