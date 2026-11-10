import { useEffect, useRef, useState, type KeyboardEvent } from 'react';

import { useI18n } from '../../i18n/context';
import { isLocalMachine } from '../../native/host';
import {
  STATUS_KEYS,
  conductorPaused,
  hooksMissing,
  requestTerminal,
  turnStalled,
} from '../../state/conductor';
import { RequestError } from '../../state/core';
import { useActions, useAppState, useConductor, useMachines } from '../../state/store';
import { Icon } from '../Icon';
import { openSettingsAt, requestSettingsView } from '../settingsNav';
import { ApprovalsTab } from './ApprovalsTab';
import { COMMAND_TABS, COMMAND_TAB_LABELS, type CommandTab } from './commandTabs';
import { ConversationTab } from './ConversationTab';
import { LogTab } from './LogTab';
import { useInboxCount } from './useInbox';
import styles from './CommandCenter.module.css';

/**
 * Command Center: a direct conversation with the conductor (its
 * session in the normal chat view, composer and voice), the Approvals inbox,
 * the conductor's log. For admin devices the conversation also has a remote
 * shell (PowerShell on Windows) in a project folder, next to its Chat and
 * Terminal views. Setup lives in Settings › Command Center; start,
 * pause and the kill switch in the header control on every page.
 */
export function CommandCenterView({
  tab: controlled,
  onTab,
  onOpenSession,
  onOpenSettings,
  onOpenTasks,
}: {
  tab?: CommandTab;
  onTab?: (tab: CommandTab) => void;
  /** Opens a work session (on its terminal with `terminal`). */
  onOpenSession?: (sessionId: string, terminal: boolean) => void;
  onOpenSettings?: () => void;
  /** Opens the to-do list ("Alerts" › "To-do"). */
  onOpenTasks?: () => void;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const state = useConductor();
  const { machine } = useMachines();
  const local = isLocalMachine(machine.id);
  const connected = useAppState((s) => s.connection === 'connected');
  const [unsupported, setUnsupported] = useState(false);
  const [own, setOwn] = useState<CommandTab>('conversation');
  const tabs = COMMAND_TABS;
  const wanted = controlled ?? own;
  const tab: CommandTab = tabs.includes(wanted) ? wanted : 'conversation';
  /** The folder picked for the conversation's shell (`null`: the most recent project folder). */
  const [shellFolder, setShellFolder] = useState<string | null>(null);
  const tabRefs = useRef(new Map<CommandTab, HTMLButtonElement>());
  const inboxCount = useInboxCount();

  // Load (or refresh) the state when the view opens.
  useEffect(() => {
    if (!connected) return;
    let live = true;
    actions.conductor({ type: 'conductor_get' }).then(
      () => live && setUnsupported(false),
      (err: unknown) => {
        if (live && err instanceof RequestError && err.code === 'unsupported') setUnsupported(true);
      },
    );
    return () => {
      live = false;
    };
  }, [actions, connected]);

  const select = (next: CommandTab, focus = false) => {
    setOwn(next);
    onTab?.(next);
    if (focus) tabRefs.current.get(next)?.focus();
  };
  const onTabKey = (e: KeyboardEvent<HTMLButtonElement>, id: CommandTab) => {
    const at = tabs.indexOf(id);
    let next: CommandTab | null = null;
    if (e.key === 'ArrowRight') next = tabs[(at + 1) % tabs.length]!;
    else if (e.key === 'ArrowLeft') next = tabs[(at - 1 + tabs.length) % tabs.length]!;
    else if (e.key === 'Home') next = tabs[0]!;
    else if (e.key === 'End') next = tabs[tabs.length - 1]!;
    if (!next) return;
    e.preventDefault();
    select(next, true);
  };
  /** Bumped by "Show terminal" on the conductor's session: its view opens again on the terminal. */
  const [terminalAsk, setTerminalAsk] = useState(0);
  // The conductor's own session stays here, on the conversation (the
  // session screen never shows it): "Show terminal" switches its view there.
  const openSession = (sessionId: string, terminal: boolean) => {
    if (terminal) requestTerminal(sessionId);
    if (state?.session_id && sessionId === state.session_id) {
      if (terminal) setTerminalAsk((n) => n + 1);
      select('conversation');
    } else onOpenSession?.(sessionId, terminal);
  };

  let body;
  if (unsupported) body = <p className="muted">{t('conductor.unsupported')}</p>;
  else if (!state) body = null;
  else if (!state.settings.enabled)
    body = (
      <section className={`panel ${styles.section}`} data-testid="conductor-off">
        <p>{local ? t('conductor.offLocal') : t('conductor.offRemote')}</p>
        {local && onOpenSettings && (
          <div className={styles.buttons}>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              onClick={() => {
                openSettingsAt('set-conductor');
                onOpenSettings();
              }}
            >
              <Icon name="settings" size={16} />
              {t('conductor.openSettings')}
            </button>
          </div>
        )}
      </section>
    );
  else
    body = (
      <>
        {hooksMissing(state) && (
          <div className={styles.banner} role="alert" data-testid="cc-hooks-missing">
            <Icon name="info" size={18} />
            <div className={styles.bannerText}>
              <strong>{t('cc.hooksMissing.title')}</strong>
              <span>{t(local ? 'cc.hooksMissing.fixLocal' : 'cc.hooksMissing.fixRemote')}</span>
            </div>
            {local && (
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => requestSettingsView('set-agent-hooks')}
              >
                {t('cc.hooksMissing.open')}
              </button>
            )}
          </div>
        )}
        {turnStalled(state) && (
          <div className={styles.banner} role="alert" data-testid="cc-turn-stalled">
            <Icon name="info" size={18} />
            <div className={styles.bannerText}>
              <strong>{t('cc.turnStalled.title')}</strong>
              <span>{t(local ? 'cc.turnStalled.fixLocal' : 'cc.turnStalled.fixRemote')}</span>
            </div>
            {state.session_id && onOpenSession && (
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => openSession(state.session_id!, true)}
              >
                {t('live.showTerminal')}
              </button>
            )}
          </div>
        )}
        {conductorPaused(state) && (
          <div className={styles.banner} role="status" data-testid="cc-paused">
            <Icon name="pause" size={18} />
            <div className={styles.bannerText}>
              <span>{t('cc.paused')}</span>
            </div>
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => {
                actions.conductor({ type: 'cc_pause', paused: false }).catch((err: unknown) =>
                  actions.showToast({
                    id: `cc-resume:${Date.now()}`,
                    category: 'error',
                    sessionId: null,
                    title: t('error.request', { error: (err as Error).message }),
                    body: '',
                  }),
                );
              }}
            >
              {t('cc.control.resumeCc')}
            </button>
          </div>
        )}
        {state.kill_switch && (
          <p role="alert" className={styles.error}>
            {t('conductor.killSwitchTripped')}
          </p>
        )}
        <div className={styles.tabs} role="tablist" aria-label={t('cc.tabs')}>
          {tabs.map((id) => (
            <button
              key={id}
              ref={(el) => {
                if (el) tabRefs.current.set(id, el);
                else tabRefs.current.delete(id);
              }}
              type="button"
              role="tab"
              id={`cc-tab-${id}`}
              aria-selected={tab === id}
              aria-controls={`cc-panel-${id}`}
              tabIndex={tab === id ? 0 : -1}
              className={styles.tab}
              onClick={() => select(id)}
              onKeyDown={(e) => onTabKey(e, id)}
            >
              {t(COMMAND_TAB_LABELS[id])}
              {id === 'approvals' && inboxCount > 0 && (
                <>
                  <span className="badge" aria-hidden="true">
                    {inboxCount}
                  </span>
                  <span className="visually-hidden">({inboxCount})</span>
                </>
              )}
            </button>
          ))}
        </div>
        <div
          role="tabpanel"
          id={`cc-panel-${tab}`}
          aria-labelledby={`cc-tab-${tab}`}
          className={tab === 'conversation' ? styles.conversationPanel : styles.panel}
          tabIndex={tab === 'conversation' ? undefined : 0}
        >
          {tab === 'conversation' ? (
            <ConversationTab
              state={state}
              local={local}
              onOpenSession={openSession}
              terminalAsk={terminalAsk}
              shellFolder={shellFolder}
              onShellFolder={setShellFolder}
            />
          ) : tab === 'approvals' ? (
            <ApprovalsTab onOpenSession={openSession} onOpenTasks={onOpenTasks} />
          ) : (
            <LogTab state={state} />
          )}
        </div>
      </>
    );

  return (
    <div className={styles.view} data-testid="conductor-view">
      <div className={styles.head}>
        <h2 className={styles.title}>{t('cc.title')}</h2>
        {state?.settings.enabled && (
          <span className={styles.status} data-status={state.status} data-testid="conductor-status">
            {t(STATUS_KEYS[state.status])}
          </span>
        )}
      </div>
      {body}
    </div>
  );
}
