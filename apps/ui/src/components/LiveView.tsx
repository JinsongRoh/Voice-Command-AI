import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from 'react';

import { FileDropProvider, useFileDropZone } from '../attachments/fileDrop';
import { useEditorCap } from '../hooks/useEditorCap';
import { useMediaQuery } from '../hooks/useMediaQuery';

import { useI18n } from '../i18n/context';
import {
  hasChatTranscript,
  isCloud,
  isShellSession,
  providerLabel,
  sessionCapabilities,
  sessionLocation,
} from '../protocol/helpers';
import type { SessionAction, SessionInfo } from '../protocol/types';
import { clearTerminalWish, wantsTerminal } from '../state/conductor';
import { visibleSessions } from '../state/reducer';
import { describeRequestError, type RequestErrorText } from '../state/requestErrors';
import { useVocoSupported } from '../speech/useVoco';
import { useActions, useAppState, useAppStateGetter, useSettings } from '../state/store';
import { AgentIcon } from './AgentIcon';
import { ChatSearch } from './ChatSearch';
import { chatProgress } from './chatProgress';
import { ChatView } from './ChatView';
import { Composer } from './Composer';
import { DecisionSheet } from './DecisionSheet';
import { ErrorBanner } from './ErrorBanner';
import { ErrorBoundary } from './ErrorBoundary';
import { HooksMissingBanner } from './HooksMissingBanner';
import { Icon, type IconName } from './Icon';
import { ConfirmDialog, hasOpenModal } from './Modal';
import { OverflowMenu } from './OverflowMenu';
import { useRemoveFromVca } from './RemoveSession';
import {
  barOnScroll,
  BAR_SCROLL_START,
  isEmptyChatSpace,
  swipeDirection,
  type BarScroll,
} from './phoneChrome';
import { RenameSessionDialog } from './RenameSessionDialog';
import { SendToProvider, SendToSessionDialog } from './SendToSession';
import { SessionEndedBar, ShellClosingBanner, ShellJoinBanner } from './SessionEnded';
import { ShellTab, useProjectShell, useShellTabAvailable } from './ShellTab';
import { requestSessionSwitcher } from './switcherNav';
import { TerminalInput } from './TerminalInput';
import { useBringToFront } from './WindowsPanel';
import { ShellBadge, ShellCountdown } from './ShellCountdown';
import { StateChip } from './StateChip';
import { SubagentBadge } from './SubagentBadge';
import { TaskSourceSheet, useChatAnchor } from './TaskSource';
import { useAddTaskFromMessage } from './TasksEditor';
import styles from './LiveView.module.css';

const TerminalView = lazy(() => import('./TerminalView'));

type Mode = 'chat' | 'terminal' | 'shell';

/** Phones (the carousel layout): terminal fit to width, terminal input row. */
const PHONE_QUERY = '(max-width: 899px)';

/** Pages a single "search earlier messages" press may load. */
const MAX_SEARCH_PAGES = 80;

/** How long the "why is this unavailable" note stays. */
const NOTICE_MS = 5000;

/** Two taps this close (ms, px) are a double tap. */
const DOUBLE_TAP_MS = 350;
const DOUBLE_TAP_PX = 30;

/** How long the "switched to ..." note under the bar stays. */
const SWITCH_NOTE_MS = 1800;

/**
 * Carried from one session's view to the next (the view is keyed by the
 * session, so a switch from the strip or a title swipe mounts a new one):
 * the note to show and whether the title takes the focus back.
 */
const carry: { note: string | null; focusTitle: boolean } = { note: null, focusTitle: false };

/** Hands the note and the title focus to the next session's view. */
function carryToNext(note: string | null, focusTitle: boolean) {
  carry.note = note;
  carry.focusTitle = focusTitle;
}

/** Reads (and clears) what the previous session's view handed over. */
function takeCarry(): { note: string | null; focusTitle: boolean } {
  const taken = { ...carry };
  carryToNext(null, false);
  return taken;
}

/** Renders the app's session strip (`PhoneChrome.strip`). */
function StripSlot({
  render,
  onPicked,
  trailing,
}: {
  render: NonNullable<PhoneChrome['strip']>;
  onPicked: (id: string) => void;
  trailing: ReactNode;
}) {
  return <>{render({ onPicked, trailing })}</>;
}

const MODE_ICONS: Record<Mode, IconName> = {
  chat: 'chat',
  terminal: 'terminal',
  shell: 'terminal',
};

/**
 * The phone layout of a session (max-width 899px): one slim top bar instead
 * of the app header, the view switch and the session actions; the app's own
 * controls (computer, connection, Command Center, spoken alerts) are in its "more"
 * menu. Desktop never passes this.
 */
export interface PhoneChrome {
  /** Back to the session list (where the bottom menu shows again). */
  onBack: () => void;
  /** Pending alerts and blocking tasks (the badge on "more"). */
  alerts: number;
  onOpenAlerts: () => void;
  /** The app header's controls, for the "more" menu. */
  headerItems: ReactNode;
  /**
   * The session strip under the bar (short swipeable cards), always shown:
   * the phone's one session switch control. `onPicked` runs after a card is
   * picked; `trailing` is its last item (the search button for the full
   * switcher sheet).
   */
  strip?: (props: { onPicked: (id: string) => void; trailing: ReactNode }) => ReactNode;
}

/**
 * A shell tab of the caller's own in place of the session Shell tab:
 * the Command Center's remote shell (PowerShell on Windows) in a folder picked there.
 * Its content has its own folder control and input row.
 */
export interface FolderShellTab {
  /** The tab's name: the shell, e.g. "PowerShell". */
  label: string;
  /** The tab button's tooltip. */
  hint: string;
  render: (fitWidth: boolean) => ReactNode;
}

export function LiveView({
  session,
  onBack,
  chrome,
  folderShell,
}: {
  session: SessionInfo;
  onBack?: () => void;
  /** Phones: the top bar layout (see `PhoneChrome`). */
  chrome?: PhoneChrome;
  /** Command Center: its shell tab (see `FolderShellTab`); only where shells are allowed. */
  folderShell?: FolderShellTab;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const view = useAppState((s) => s.views[session.id]);
  const decision = useAppState((s) => s.decisions[session.id]);
  const sent = useAppState((s) => s.sent[session.id]);
  // "Add to to-do" on agent messages (servers with a to-do list).
  const addTask = useAddTaskFromMessage(session.id);
  // Shells have no chat transcript: they open on the terminal.
  const pauseWhyId = useId();
  const restartWhyId = useId();
  const stopWhyId = useId();
  const switchHintId = useId();
  // The Command Center's "Show terminal" opens the session on its terminal too.
  const [mode, setMode] = useState<Mode>(() =>
    isShellSession(session) || wantsTerminal(session.id) ? 'terminal' : 'chat',
  );
  useEffect(() => clearTerminalWish(session.id), [session.id]);
  // A to-do's "View source conversation": the chat, at the source message.
  const anchor = useChatAnchor(session.id, view);
  const [anchorToken, setAnchorToken] = useState<number | null>(null);
  if (anchor.pending && anchor.pending.token !== anchorToken) {
    // Derived during render: a new request switches to the chat once.
    setAnchorToken(anchor.pending.token);
    setMode('chat');
  }
  const [confirmStop, setConfirmStop] = useState(false);
  const [searchQuery, setSearchQuery] = useState('');
  const [searchIndex, setSearchIndex] = useState(0);
  const [searchCount, setSearchCount] = useState(0);
  const [searchingOlder, setSearchingOlder] = useState(false);
  // Read after a page arrives, when the closure of the loop is long stale.
  const latest = useRef({ more: view?.more, count: 0 });
  useEffect(() => {
    latest.current = { more: view?.more, count: searchCount };
  });
  const loadOlder = useCallback(() => {
    actions.loadOlder(session.id).catch(() => {});
  }, [actions, session.id]);
  /** Bumped to stop a running "search earlier messages" loop between pages. */
  const searchRun = useRef(0);
  const stopSearchOlder = useCallback(() => {
    searchRun.current++;
    setSearchingOlder(false);
  }, []);
  /** Pages in earlier history until the search finds more, or there is no more. */
  const searchOlder = useCallback(async () => {
    const run = ++searchRun.current;
    setSearchingOlder(true);
    try {
      for (let i = 0; i < MAX_SEARCH_PAGES; i++) {
        if (searchRun.current !== run) return;
        const before = latest.current.count;
        if (latest.current.more === false) break;
        await actions.loadOlder(session.id);
        if (searchRun.current !== run) return;
        // Let the page render and the match count update.
        await new Promise((resolve) => setTimeout(resolve, 0));
        if (latest.current.count > before) break;
      }
    } catch {
      // The reply failed; the button stays for another try.
    } finally {
      if (searchRun.current === run) setSearchingOlder(false);
    }
  }, [actions, session.id]);
  const [renaming, setRenaming] = useState(false);
  const [error, setError] = useState<RequestErrorText | null>(null);
  const dismissError = useCallback(() => setError(null), []);
  /** Why an action is unavailable (shown when its dimmed button is pressed). */
  const [notice, setNotice] = useState<string | null>(null);
  useEffect(() => {
    if (!notice) return;
    const timer = setTimeout(() => setNotice(null), NOTICE_MS);
    return () => clearTimeout(timer);
  }, [notice]);
  const cloud = isCloud(session.origin);
  const shell = isShellSession(session);
  // The composer's text box never squeezes the conversation to nothing.
  const liveRef = useRef<HTMLElement>(null);
  useEditorCap(liveRef);
  // The Shell tab: a remote shell in the session's folder, inside this view.
  const shellTab = useShellTabAvailable(session) || !!folderShell;
  const projectShell = useProjectShell(session);
  const effectiveMode: Mode = cloud ? 'chat' : mode === 'shell' && !shellTab ? 'chat' : mode;
  // A running "search earlier messages" stops when the query changes or the
  // search closes, when the chat is left for another tab, and on unmount.
  useEffect(() => stopSearchOlder, [effectiveMode, searchQuery, stopSearchOlder]);
  const phone = useMediaQuery(PHONE_QUERY);
  // --- Phone top bar: auto-hide on scroll, reading mode, its menus. ---
  const [bar, setBar] = useState<BarScroll>(BAR_SCROLL_START);
  const [reading, setReading] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [tabMenuOpen, setTabMenuOpen] = useState(false);
  /** The chat search row under the bar (opened from "more"). */
  const [searchOpen, setSearchOpen] = useState(false);
  // --- Switching sessions: the strip under the bar, or a swipe on the
  // title (a plain name, not a control: it only takes the focus back). ---
  const titleRef = useRef<HTMLHeadingElement>(null);
  const getState = useAppStateGetter();
  /** "Switched to ..." (a short note under the bar after a switch). */
  const [switchNote, setSwitchNote] = useState<string | null>(() => carry.note);
  useEffect(() => {
    if (takeCarry().focusTitle) titleRef.current?.focus();
  }, []);
  useEffect(() => {
    if (!switchNote) return;
    const timer = setTimeout(() => setSwitchNote(null), SWITCH_NOTE_MS);
    return () => clearTimeout(timer);
  }, [switchNote]);
  /** A horizontal swipe on the title: the previous or next session, in card order. */
  const swipeRef = useRef<{ id: number; x: number; y: number } | null>(null);
  const stepSession = (dir: -1 | 1) => {
    const list = visibleSessions(getState());
    const at = list.findIndex((s) => s.id === session.id);
    // Off the filtered list (an answered "needs attention" session): the
    // swipe still moves, to the first or the last session left in it.
    const target = at < 0 ? list[dir > 0 ? 0 : list.length - 1] : list[at + dir];
    if (!target) {
      setSwitchNote(t(dir < 0 ? 'strip.first' : 'strip.last'));
      return;
    }
    // The view is keyed by the session: the next one shows the note and
    // gives the focus back to its title.
    carryToNext(t('strip.switched', { name: target.display_name }), true);
    actions.select(target.id);
  };
  const onTitlePointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if ((e.button ?? 0) > 0) return;
    swipeRef.current = { id: e.pointerId, x: e.clientX ?? 0, y: e.clientY ?? 0 };
  };
  const onTitlePointerUp = (e: ReactPointerEvent<HTMLDivElement>) => {
    const start = swipeRef.current;
    swipeRef.current = null;
    if (!start || start.id !== e.pointerId) return;
    const dir = swipeDirection((e.clientX ?? 0) - start.x, (e.clientY ?? 0) - start.y);
    if (!dir) return;
    stepSession(dir);
  };
  const chromeRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const readingButtonRef = useRef<HTMLButtonElement>(null);
  const exitReadingRef = useRef<HTMLButtonElement>(null);
  const lastTapRef = useRef<{ at: number; x: number; y: number } | null>(null);
  // Hands-free with the wake word: "Voco" is heard by the message box, so on
  // phones the terminal and shell tabs open with it (the input row is one
  // tap away) instead of silently leaving the wake word unheard.
  const { settings } = useSettings();
  const vocoSupported = useVocoSupported();
  const vocoOn = settings.handsFree && settings.vocoWake && vocoSupported;
  /** Phones, terminal or shell tab: the full message box instead of the input row. */
  const [composerOpen, setComposerOpen] = useState(vocoOn);
  const [shownMode, setShownMode] = useState(effectiveMode);
  const [shownVoco, setShownVoco] = useState(vocoOn);
  if (shownMode !== effectiveMode || shownVoco !== vocoOn) {
    setShownMode(effectiveMode);
    setShownVoco(vocoOn);
    setComposerOpen(vocoOn);
  }
  // The bar never hides while something needs the person: a decision for
  // this session, any pending alert (its badge is on the bar), an open menu
  // or the search row.
  const barPinned =
    session.state.type === 'awaiting_decision' ||
    !!decision ||
    (chrome?.alerts ?? 0) > 0 ||
    menuOpen ||
    tabMenuOpen ||
    searchOpen;
  const barHidden = !!chrome && effectiveMode === 'chat' && bar.hidden && !barPinned;
  // Scrolling the chat toward older messages slides the bar away; scrolling
  // back, or reaching the newest message, brings it back (see phoneChrome.ts).
  const hasChrome = !!chrome;
  const chatChrome = hasChrome && effectiveMode === 'chat';
  useEffect(() => {
    const content = contentRef.current;
    if (!chatChrome || !content) return;
    const onScroll = (e: Event) => {
      const el = e.target;
      if (!(el instanceof HTMLElement)) return;
      const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
      setBar((prev) => barOnScroll(prev, el.scrollTop, atBottom, el.clientHeight));
    };
    // Scroll events do not bubble: listen in the capture phase.
    content.addEventListener('scroll', onScroll, true);
    return () => content.removeEventListener('scroll', onScroll, true);
  }, [chatChrome]);
  // The chat starts below the bar (and the card row, banners, search row).
  useLayoutEffect(() => {
    const el = chromeRef.current;
    const live = liveRef.current;
    if (!hasChrome || !el || !live || typeof ResizeObserver === 'undefined') return;
    const update = () => {
      const h = el.getBoundingClientRect().height;
      if (h > 0) live.style.setProperty('--chrome-h', `${Math.ceil(h)}px`);
    };
    const observer = new ResizeObserver(update);
    observer.observe(el);
    update();
    return () => observer.disconnect();
  }, [hasChrome]);
  // Reading mode moves the focus to its exit button, and back again.
  const readingShown = useRef(false);
  useEffect(() => {
    if (reading) exitReadingRef.current?.focus();
    else if (readingShown.current) readingButtonRef.current?.focus();
    readingShown.current = reading;
  }, [reading]);
  /** A double tap on empty chat space switches reading mode. */
  const onContentPointerUp = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!chatChrome || (e.button ?? 0) > 0) return;
    if (!isEmptyChatSpace(e.target)) {
      lastTapRef.current = null;
      return;
    }
    const now = Date.now();
    const x = e.clientX ?? 0;
    const y = e.clientY ?? 0;
    const prev = lastTapRef.current;
    if (
      prev &&
      now - prev.at < DOUBLE_TAP_MS &&
      Math.hypot(x - prev.x, y - prev.y) < DOUBLE_TAP_PX
    ) {
      lastTapRef.current = null;
      setReading((v) => !v);
    } else {
      lastTapRef.current = { at: now, x, y };
    }
  };
  const bringToFront = useBringToFront();
  /** "Send to session…": the text being sent (`null`: closed). */
  const [sendText, setSendText] = useState<string | null>(null);
  const st = session.state.type;
  const can = sessionCapabilities(session);
  // Sessions VCA did not start (found in the user's own terminal): pause,
  // restart and stop stay visible but dimmed, and explain themselves instead
  // of failing on the server. The key bar's interrupt still works there.
  const showProcessActions = !cloud && !shell;
  const events = view?.events ?? [];
  /** "Working · 42s" under the chat while the agent works on a prompt. */
  const progress = chatProgress(session, sent);
  // Files dragged from Explorer, Finder or a file manager onto the pane are
  // attached by the composer (the same upload as its paperclip).
  const fileDrop = useFileDropZone();

  /**
   * Where typed input goes: the session itself, or on the Shell tab its
   * project shell (none until one is open). A caller's shell tab has its own.
   */
  const inputTarget =
    effectiveMode === 'shell' ? (folderShell ? undefined : projectShell) : session;
  /** Terminal-like tabs: on phones they get the terminal input row. */
  const termLike = effectiveMode === 'terminal' || effectiveMode === 'shell';

  // "Remove from VCA" of a running session (an ended one has the ended bar).
  const removal = useRemoveFromVca((_, message) => setError({ message, detail: null }));

  const control = (action: SessionAction) => {
    setError(null);
    setNotice(null);
    actions
      .control(session.id, action)
      .catch((err: Error) => setError(describeRequestError(t, err)));
  };

  const connection = useAppState((s) => s.connection);
  const shellTabLabel = folderShell?.label ?? t('shellTab.tab');
  const modeLabel = (m: Mode) =>
    m === 'chat' ? t('live.chat') : m === 'terminal' ? t('live.terminal') : shellTabLabel;
  /** Chat / Terminal / Shell (the desktop toolbar, the phone tab menu). */
  const modeButtons = (close?: () => void) => {
    const pick = (m: Mode) => {
      close?.();
      setMode(m);
    };
    return (
      <>
        <button type="button" aria-pressed={effectiveMode === 'chat'} onClick={() => pick('chat')}>
          <Icon name="chat" size={14} /> {t('live.chat')}
        </button>
        <button
          type="button"
          aria-pressed={effectiveMode === 'terminal'}
          onClick={() => pick('terminal')}
          disabled={cloud}
          title={cloud ? t('live.noTerminal') : undefined}
        >
          <Icon name="terminal" size={14} /> {t('live.terminal')}
        </button>
        {shellTab && (
          <button
            type="button"
            aria-pressed={effectiveMode === 'shell'}
            onClick={() => pick('shell')}
            title={folderShell?.hint ?? t('shellTab.hint')}
            data-testid="shell-tab-button"
          >
            <Icon name="terminal" size={14} /> {shellTabLabel}
          </button>
        )}
      </>
    );
  };
  /** Pause / resume / restart / stop (the desktop toolbar, the phone "more" menu). */
  const actionButtons = (before?: () => void) => (
    <>
      {session.origin.type === 'cloud' && session.origin.url && (
        <a
          className="btn btn-sm"
          href={session.origin.url}
          target="_blank"
          rel="noreferrer"
          aria-label={t('live.openProvider', {
            provider: providerLabel(session.origin.provider),
          })}
        >
          <Icon name="external" size={14} />
          <span className={styles.btnLabel}>
            {t('live.openProvider', { provider: providerLabel(session.origin.provider) })}
          </span>
        </a>
      )}
      {showProcessActions && st !== 'exited' && st !== 'paused' && (
        <button
          type="button"
          className="btn btn-sm"
          aria-label={t('action.pause')}
          title={can.pause ? t('action.pause') : t('live.cantPause')}
          aria-disabled={can.pause ? undefined : true}
          aria-describedby={can.pause ? undefined : pauseWhyId}
          data-unavailable={can.pause ? undefined : true}
          onClick={() => {
            before?.();
            if (can.pause) control('pause');
            else setNotice(t('live.cantPause'));
          }}
        >
          <Icon name="pause" size={14} />
          <span className={styles.btnLabel}>{t('action.pause')}</span>
        </button>
      )}
      {showProcessActions && can.pause && st === 'paused' && (
        <button
          type="button"
          className="btn btn-sm"
          aria-label={t('action.resume')}
          title={t('action.resume')}
          onClick={() => {
            before?.();
            control('resume');
          }}
        >
          <Icon name="play" size={14} />
          <span className={styles.btnLabel}>{t('action.resume')}</span>
        </button>
      )}
      {showProcessActions && (
        <button
          type="button"
          className="btn btn-sm"
          aria-label={t('action.restart')}
          title={can.restart ? t('action.restart') : t('live.cantRestart')}
          aria-disabled={can.restart ? undefined : true}
          aria-describedby={can.restart ? undefined : restartWhyId}
          data-unavailable={can.restart ? undefined : true}
          onClick={() => {
            before?.();
            if (can.restart) control('restart');
            else setNotice(t('live.cantRestart'));
          }}
        >
          <Icon name="restart" size={14} />
          <span className={styles.btnLabel}>{t('action.restart')}</span>
        </button>
      )}
      {st !== 'exited' && (
        <button
          type="button"
          className="btn btn-sm"
          aria-label={shell ? t('shell.close') : t('action.stop')}
          title={can.stop ? (shell ? t('shell.close') : t('action.stop')) : t('live.cantStop')}
          aria-disabled={can.stop ? undefined : true}
          aria-describedby={can.stop ? undefined : stopWhyId}
          data-unavailable={can.stop ? undefined : true}
          onClick={() => {
            before?.();
            if (can.stop) setConfirmStop(true);
            else setNotice(t('live.cantStop'));
          }}
        >
          <Icon name={shell ? 'close' : 'stop'} size={14} />
          <span className={styles.btnLabel}>{shell ? t('shell.close') : t('action.stop')}</span>
        </button>
      )}
      {st !== 'exited' && removal.offered(session) && (
        <button
          type="button"
          className="btn btn-sm"
          aria-label={t('ended.removeNamed', { name: session.display_name })}
          title={t('remove.action')}
          aria-haspopup="dialog"
          data-testid="live-remove"
          onClick={() => {
            before?.();
            removal.ask(session);
          }}
        >
          <Icon name="close" size={14} />
          <span className={styles.btnLabel}>{t('remove.action')}</span>
        </button>
      )}
    </>
  );

  return (
    <section
      ref={liveRef}
      className={styles.live}
      aria-labelledby="live-title"
      data-chrome={chrome ? 'phone' : undefined}
      data-mode={effectiveMode}
      data-reading={(chrome && reading && effectiveMode === 'chat') || undefined}
      data-dropping={fileDrop.dragging || undefined}
      {...fileDrop.dropProps}
    >
      {fileDrop.dragging && (
        <div className={styles.dropOverlay} data-testid="drop-overlay" aria-hidden="true">
          <span className={styles.dropCard}>
            <Icon name="upload" size={28} />
            <span>{t('attach.dropOverlay')}</span>
          </span>
        </div>
      )}
      {chrome ? (
        <div
          ref={chromeRef}
          className={styles.topChrome}
          data-hidden={barHidden || undefined}
          data-testid="session-topbar"
        >
          <div className={styles.topBar}>
            <button
              type="button"
              className={`icon-btn ${styles.barButton}`}
              onClick={chrome.onBack}
              aria-label={t('live.back')}
              title={t('live.back')}
            >
              <Icon name="back" />
            </button>
            {/* Only the title area takes the sideways swipe (the chat keeps
                text selection, the terminal its own horizontal scroll). */}
            <div
              className={styles.barTitles}
              data-testid="live-title-area"
              onPointerDown={onTitlePointerDown}
              onPointerUp={onTitlePointerUp}
              onPointerCancel={() => {
                swipeRef.current = null;
              }}
            >
              {/* The session name only: the strip below switches sessions. */}
              <h2
                id="live-title"
                ref={titleRef}
                tabIndex={-1}
                className={styles.barTitle}
                data-testid="live-title"
              >
                {session.display_name}
              </h2>
              <span className={styles.barSub}>
                <StateChip state={session.state} terminalPrompt={session.terminal_prompt} />
                {shell && <ShellBadge />}
              </span>
            </div>
            {!cloud && (
              <OverflowMenu
                icon={MODE_ICONS[effectiveMode]}
                label={t('topbar.view', { mode: modeLabel(effectiveMode) })}
                testId="topbar-view"
                onOpenChange={setTabMenuOpen}
              >
                {(close) => (
                  <div className={styles.tabMenu} role="group" aria-label={t('live.viewMode')}>
                    {modeButtons(close)}
                  </div>
                )}
              </OverflowMenu>
            )}
            {effectiveMode === 'chat' && (
              <button
                ref={readingButtonRef}
                type="button"
                className={`icon-btn ${styles.barButton}`}
                onClick={() => setReading(true)}
                aria-label={t('reading.enter')}
                title={t('reading.enter')}
                aria-describedby={`${switchHintId}-reading`}
                data-testid="reading-enter"
              >
                <Icon name="expand" size={18} />
              </button>
            )}
            <span id={`${switchHintId}-reading`} hidden>
              {t('reading.hint')}
            </span>
            <span
              className={styles.barConn}
              data-state={connection}
              role="status"
              title={t(`conn.${connection}`)}
            >
              <span className={styles.barDot} aria-hidden="true" />
              {/* Only the healthy state is a bare dot; trouble is spelled out, not just coloured. */}
              <span className={connection === 'connected' ? 'visually-hidden' : styles.barConnText}>
                {t(`conn.${connection}`)}
              </span>
            </span>
            <OverflowMenu
              label={
                chrome.alerts > 0
                  ? t('topbar.moreAlerts', { count: chrome.alerts })
                  : t('header.more')
              }
              badge={chrome.alerts}
              testId="topbar-more"
              onOpenChange={setMenuOpen}
            >
              {(close) => (
                <>
                  <button
                    type="button"
                    className="btn btn-sm"
                    onClick={() => {
                      close();
                      chrome.onOpenAlerts();
                    }}
                    aria-label={
                      chrome.alerts > 0
                        ? `${t('nav.alertsTasks')}, ${t('alerts.count', { count: chrome.alerts })}`
                        : t('nav.alertsTasks')
                    }
                  >
                    <Icon name="bell" size={16} />
                    <span>{t('nav.alertsTasks')}</span>
                    {chrome.alerts > 0 && (
                      <span className="badge" aria-hidden="true">
                        {chrome.alerts}
                      </span>
                    )}
                  </button>
                  <div
                    className={`${styles.menuActions} ${styles.actions}`}
                    role="group"
                    aria-label={t('live.actions')}
                  >
                    {effectiveMode === 'chat' && (
                      <button
                        type="button"
                        className="btn btn-sm"
                        onClick={() => {
                          close();
                          setSearchOpen(true);
                        }}
                      >
                        <Icon name="search" size={14} />
                        <span className={styles.btnLabel}>{t('search.open')}</span>
                      </button>
                    )}
                    <button
                      type="button"
                      className="btn btn-sm"
                      onClick={() => {
                        close();
                        setRenaming(true);
                      }}
                    >
                      <Icon name="edit" size={14} />
                      <span className={styles.btnLabel}>{t('rename.action')}</span>
                    </button>
                    {actionButtons(close)}
                  </div>
                  <hr className={styles.menuRule} />
                  {/* The app's own controls close the menu after their action
                      too, unless they open a menu or dialog of their own. */}
                  <div onClick={(e) => closeAfterAction(e.target, close)}>{chrome.headerItems}</div>
                </>
              )}
            </OverflowMenu>
          </div>
          {searchOpen && effectiveMode === 'chat' && (
            <div className={styles.searchRow}>
              <ChatSearch
                initialOpen
                onClosed={() => setSearchOpen(false)}
                count={searchCount}
                index={searchIndex}
                onQuery={(q) => {
                  setSearchQuery(q);
                  setSearchIndex(0);
                }}
                onIndex={setSearchIndex}
                canSearchOlder={view?.gapFrom != null && view.more !== false}
                searchingOlder={searchingOlder}
                onSearchOlder={() => void searchOlder()}
                onStopSearchOlder={stopSearchOlder}
              />
            </div>
          )}
          {chrome.strip && (
            <div className={styles.strip}>
              <StripSlot
                render={chrome.strip}
                onPicked={(id) => {
                  // Another session mounts a new view: its title takes the focus.
                  if (id !== session.id) carryToNext(null, true);
                }}
                trailing={
                  <button
                    type="button"
                    className={`icon-btn ${styles.stripSearch}`}
                    aria-label={t('switcher.search')}
                    title={t('switcher.search')}
                    aria-haspopup="dialog"
                    data-testid="strip-search"
                    onClick={requestSessionSwitcher}
                  >
                    <Icon name="search" size={18} />
                  </button>
                }
              />
            </div>
          )}
          {switchNote && (
            <p className={styles.switchNote} role="status" data-testid="switch-note">
              {switchNote}
            </p>
          )}
          <div className={styles.banners}>
            {error && (
              <ErrorBanner message={error.message} detail={error.detail} onDismiss={dismissError} />
            )}
            {session.terminal_prompt && st !== 'exited' && effectiveMode === 'chat' && (
              <p role="status" className={styles.notice} data-testid="terminal-choice">
                <Icon name="terminal" size={14} />
                <span>
                  {session.terminal_prompt === 'trust_folder'
                    ? t('live.terminalTrust')
                    : t('live.terminalChoice')}
                </span>
                <button type="button" className="btn btn-sm" onClick={() => setMode('terminal')}>
                  {t('live.showTerminal')}
                </button>
              </p>
            )}
            <HooksMissingBanner session={session} />
            {notice && (
              <p role="status" className={styles.notice}>
                <Icon name="info" size={14} />
                <span>{notice}</span>
              </p>
            )}
          </div>
        </div>
      ) : (
        <>
          <header className={styles.header}>
            {onBack && (
              <button
                type="button"
                className="icon-btn"
                onClick={onBack}
                aria-label={t('live.back')}
              >
                <Icon name="back" />
              </button>
            )}
            <AgentIcon origin={session.origin} size={36} />
            <div className={styles.titles}>
              <div className={styles.titleRow}>
                <h2 id="live-title" className={styles.title}>
                  {/* A tap (or Ctrl+K) opens the quick switcher. */}
                  <button
                    type="button"
                    className={styles.titleButton}
                    onClick={requestSessionSwitcher}
                    aria-haspopup="dialog"
                    aria-describedby={switchHintId}
                    title={t('switcher.open')}
                    data-testid="live-title-switch"
                  >
                    <span className={styles.titleText}>{session.display_name}</span>
                    <Icon name="down" size={16} className={styles.titleChevron} />
                  </button>
                </h2>
                <span id={switchHintId} hidden>
                  {t('switcher.open')}
                </span>
                {shell && <ShellBadge />}
                <button
                  type="button"
                  className={`icon-btn ${styles.rename}`}
                  onClick={() => setRenaming(true)}
                  aria-label={t('rename.title')}
                  title={t('rename.action')}
                >
                  <Icon name="edit" size={16} />
                </button>
              </div>
              <span className={styles.sub}>
                <StateChip state={session.state} terminalPrompt={session.terminal_prompt} />
                <SubagentBadge count={session.subagents} />
                <span className={styles.location} title={sessionLocation(session)}>
                  {sessionLocation(session)}
                </span>
                {shell ? (
                  <ShellCountdown session={session} />
                ) : (
                  <span
                    className={styles.tier}
                    title={t('tier.howTitle', { how: t(`tier.how.${session.control}`) })}
                  >
                    {t(`tier.${session.control}`)}
                  </span>
                )}
              </span>
            </div>
          </header>

          <div className={styles.toolbar}>
            <div className="segmented" role="group" aria-label={t('live.viewMode')}>
              {modeButtons()}
            </div>
            {effectiveMode === 'chat' && (
              <ChatSearch
                count={searchCount}
                index={searchIndex}
                onQuery={(q) => {
                  setSearchQuery(q);
                  setSearchIndex(0);
                }}
                onIndex={setSearchIndex}
                canSearchOlder={view?.gapFrom != null && view.more !== false}
                searchingOlder={searchingOlder}
                onSearchOlder={() => void searchOlder()}
                onStopSearchOlder={stopSearchOlder}
              />
            )}
            {/* The Shell tab has its own Close bar; Pause/Restart/Stop here would hit the agent. */}
            {effectiveMode !== 'shell' && (
              <div className={styles.actions} role="group" aria-label={t('live.actions')}>
                {actionButtons()}
              </div>
            )}
          </div>
          {error && (
            <ErrorBanner message={error.message} detail={error.detail} onDismiss={dismissError} />
          )}
          {session.terminal_prompt && st !== 'exited' && effectiveMode === 'chat' && (
            <p role="status" className={styles.notice} data-testid="terminal-choice">
              <Icon name="terminal" size={14} />
              <span>
                {session.terminal_prompt === 'trust_folder'
                  ? t('live.terminalTrust')
                  : t('live.terminalChoice')}
              </span>
              <button type="button" className="btn btn-sm" onClick={() => setMode('terminal')}>
                {t('live.showTerminal')}
              </button>
            </p>
          )}
          <HooksMissingBanner session={session} />
          {notice && (
            <p role="status" className={styles.notice}>
              <Icon name="info" size={14} />
              <span>{notice}</span>
            </p>
          )}
        </>
      )}
      {chrome && reading && (
        <button
          ref={exitReadingRef}
          type="button"
          className={styles.exitReading}
          onClick={() => setReading(false)}
          // The bar (and the alert badge on "more") is hidden: the count is here.
          aria-label={
            chrome.alerts > 0
              ? `${t('reading.exit')}, ${t('alerts.count', { count: chrome.alerts })}`
              : t('reading.exit')
          }
          title={t('reading.exit')}
          data-testid="reading-exit"
        >
          <Icon name="collapse" size={18} />
          {chrome.alerts > 0 && (
            <span className={`badge ${styles.exitReadingBadge}`} aria-hidden="true">
              {chrome.alerts}
            </span>
          )}
        </button>
      )}
      {!can.pause && (
        <span id={pauseWhyId} hidden>
          {t('live.cantPause')}
        </span>
      )}
      {!can.restart && (
        <span id={restartWhyId} hidden>
          {t('live.cantRestart')}
        </span>
      )}
      {!can.stop && (
        <span id={stopWhyId} hidden>
          {t('live.cantStop')}
        </span>
      )}

      <SendToProvider value={setSendText}>
        <div
          ref={contentRef}
          className={styles.content}
          data-live-content
          onPointerUp={chrome ? onContentPointerUp : undefined}
        >
          <ErrorBoundary
            resetKey={`${session.id}:${effectiveMode}`}
            fallback={(err, retry) => (
              <div role="alert" className={styles.viewError}>
                <p>{t('live.viewFailed', { error: err.message })}</p>
                <div className={styles.viewErrorActions}>
                  {effectiveMode === 'terminal' && (
                    <button type="button" className="btn" onClick={() => setMode('chat')}>
                      {t('live.backToChat')}
                    </button>
                  )}
                  <button type="button" className="btn" onClick={retry}>
                    {t('live.retry')}
                  </button>
                </div>
              </div>
            )}
          >
            {effectiveMode === 'chat' ? (
              <ChatView
                // A fresh view per session: it opens scrolled to the latest message.
                key={session.id}
                events={events}
                gapFrom={view?.gapFrom ?? null}
                loading={view?.loading ?? false}
                more={view?.more}
                loadingOlder={view?.loadingOlder ?? false}
                onLoadOlder={loadOlder}
                sent={sent}
                outputFallback={!hasChatTranscript(session.origin)}
                onShowTerminal={() => setMode('terminal')}
                searchQuery={searchQuery}
                searchIndex={searchIndex}
                onSearchCount={setSearchCount}
                onAddTask={shell ? undefined : addTask.open}
                highlightSeq={anchor.highlight}
                progress={progress}
              />
            ) : effectiveMode === 'shell' ? (
              folderShell ? (
                folderShell.render(phone)
              ) : (
                <ShellTab session={session} fitWidth={phone} />
              )
            ) : can.terminal ? (
              <Suspense fallback={<p className="muted">…</p>}>
                <TerminalView sessionId={session.id} events={events} fitWidth={phone} />
              </Suspense>
            ) : (
              <div className={styles.noScreen} data-testid="observed-terminal">
                <p className="muted">{t('live.noTerminalScreen')}</p>
                {bringToFront && session.origin.type === 'local' && (
                  <button
                    type="button"
                    className="btn btn-sm"
                    onClick={() => void bringToFront(session.id, session.display_name)}
                  >
                    <Icon name="windows" size={14} />
                    {t('live.bringToFront')}
                  </button>
                )}
              </div>
            )}
          </ErrorBoundary>
        </div>
      </SendToProvider>

      {(st === 'awaiting_decision' || decision) && (
        <DecisionSheet session={session} decision={decision} />
      )}
      {st === 'exited' && <SessionEndedBar session={session} />}
      {shell && <ShellClosingBanner session={session} />}
      {shell && <ShellJoinBanner session={session} />}
      {inputTarget && phone && termLike && !composerOpen ? (
        <TerminalInput
          key={inputTarget.id}
          session={inputTarget}
          onOpenComposer={() => setComposerOpen(true)}
        />
      ) : (
        inputTarget && (
          <>
            {phone && termLike && (
              <button
                type="button"
                className={`btn btn-sm ${styles.backToKeys}`}
                onClick={() => setComposerOpen(false)}
              >
                <Icon name="terminal" size={14} />
                {t('terminal.backToInput')}
              </button>
            )}
            <FileDropProvider value={fileDrop.register}>
              <Composer
                key={inputTarget.id}
                session={inputTarget}
                layout={chrome ? 'phone' : undefined}
                endedBarShown={inputTarget.id === session.id && st === 'exited'}
                // A decision card keeps the full composer row: the floating
                // button never covers the card.
                reading={
                  chrome
                    ? reading &&
                      effectiveMode === 'chat' &&
                      !(st === 'awaiting_decision' || decision)
                    : undefined
                }
              />
            </FileDropProvider>
          </>
        )
      )}
      {sendText !== null && (
        <SendToSessionDialog
          text={sendText}
          sourceId={session.id}
          onClose={() => setSendText(null)}
        />
      )}

      {renaming && <RenameSessionDialog session={session} onClose={() => setRenaming(false)} />}
      {addTask.dialog}
      {anchor.missing && (
        <TaskSourceSheet
          sessionId={session.id}
          seq={anchor.missing.seq}
          excerpt={anchor.missing.excerpt}
          missing
          onClose={anchor.dismissMissing}
        />
      )}
      {removal.dialog}
      {confirmStop && (
        <ConfirmDialog
          title={shell ? t('shell.close') : t('action.stop')}
          message={
            shell
              ? t('shell.confirmClose', { name: session.display_name })
              : t('live.confirmStop', { name: session.display_name })
          }
          confirmLabel={shell ? t('shell.close') : t('action.stop')}
          danger
          onCancel={() => setConfirmStop(false)}
          onConfirm={() => {
            setConfirmStop(false);
            control('stop');
          }}
        />
      )}
    </section>
  );
}

/**
 * Closes a menu after a press on one of its plain buttons: not on a button
 * that opens something of its own (a submenu, a dialog), and not while a
 * dialog opened by the press is up (it may live inside the menu).
 */
function closeAfterAction(target: EventTarget, close: () => void) {
  if (!(target instanceof Element)) return;
  const button = target.closest('button, a[href]');
  if (!button || button.hasAttribute('aria-expanded') || button.hasAttribute('aria-haspopup')) {
    return;
  }
  setTimeout(() => {
    if (!hasOpenModal()) close();
  }, 0);
}

export function LiveEmpty() {
  const { t } = useI18n();
  return (
    <div className={styles.empty}>
      <Icon name="list" size={32} />
      <p>{t('live.noSelection')}</p>
    </div>
  );
}
