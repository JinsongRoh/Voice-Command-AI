import { useEffect, useMemo, useState } from 'react';

import { installFileDropGuard } from './attachments/fileDrop';
import { AppLockProvider, CommandCenterGate } from './components/AppLock';
import { CommandCenterView } from './components/CommandCenter/CommandCenterView';
import { useInboxCount } from './components/CommandCenter/useInbox';
import type { CommandTab } from './components/CommandCenter/commandTabs';
import { ConnectPhoneView } from './components/ConnectPhoneView';
import { Header, HeaderMenuItems } from './components/Header';
import { Icon, type IconName } from './components/Icon';
import { LiveEmpty, LiveView } from './components/LiveView';
import { LocalServerCard } from './components/LocalServerCard';
import { LoginView } from './components/LoginView';
import { MachinesSheet } from './components/MachinesPanel';
import { NewSessionDialog } from './components/NewSessionDialog';
import { PairingView } from './components/PairingView';
import { PushOpenedWatcher } from './components/PushAlertsSettings';
import { ResourcesPanel } from './components/ResourcesPanel';
import { SecurityGate } from './components/SecurityWizard';
import { SessionDeck } from './components/SessionDeck';
import { SessionSwitcher } from './components/SessionSwitcher';
import { SettingsView } from './components/SettingsView';
import { onSettingsViewRequest } from './components/settingsNav';
import { ShellProvider } from './components/ShellProvider';
import { StepUpProvider } from './components/StepUpPrompt';
import { AlertsAndTasksView, type InboxTab } from './components/TasksInbox';
import { TasksWatcher } from './components/TasksView';
import { Toasts } from './components/Toasts';
import { UpdateNotifier } from './components/UpdateSettings';
import { WindowsPanel } from './components/WindowsPanel';
import { useMediaQuery } from './hooks/useMediaQuery';
import type { AppLockVerifier } from './native/appLock';
import { I18nProvider, useI18n } from './i18n/context';
import type { MessageKey } from './i18n/i18n';
import { NativeProvider, useNative, type NativeServices } from './native/context';
import { useLinkStatus } from './native/useLinkStatus';
import { SpeechProvider, type SpeechServices } from './speech/context';
import { onConductorViewRequest, pendingCount } from './state/conductor';
import { trackTyping } from './state/inputGuard';
import { alertsAndTasksCount } from './state/reducer';
import {
  AppProvider,
  useActions,
  useAppState,
  useConductor,
  useMachines,
  useSettings,
  type Machine,
} from './state/store';
import { loadSettings, type Settings } from './state/settings';
import styles from './App.module.css';

type View =
  | 'sessions'
  | 'conductor'
  | 'alerts'
  | 'resources'
  | 'windows'
  | 'settings'
  | 'pairing'
  | 'connect';

const NAV: { view: Exclude<View, 'pairing' | 'connect'>; label: MessageKey; icon: IconName }[] = [
  { view: 'sessions', label: 'nav.sessions', icon: 'list' },
  { view: 'conductor', label: 'nav.conductor', icon: 'conductor' },
  { view: 'alerts', label: 'nav.alertsTasks', icon: 'bell' },
  { view: 'resources', label: 'nav.resources', icon: 'gauge' },
  { view: 'windows', label: 'nav.windows', icon: 'windows' },
  { view: 'settings', label: 'nav.settings', icon: 'settings' },
];

/** How long a jump waits for the decision card to appear. */
const JUMP_MS = 1500;

function Shell() {
  const { t } = useI18n();
  const actions = useActions();
  const { machine, switchMachine } = useMachines();
  const link = useLinkStatus(machine);
  const wide = useMediaQuery('(min-width: 900px)');
  const native = useNative();
  // A pending pairing link (it opened the app) starts on the pairing form.
  const [view, setView] = useState<View>(() =>
    native?.pairLinks?.hasPending ? 'pairing' : 'sessions',
  );
  const [newSession, setNewSession] = useState(false);
  /** Phones: the session list is showing (Back from a session), not the session. */
  const [phoneList, setPhoneList] = useState(false);
  const [manageMachines, setManageMachines] = useState(false);
  const selected = useAppState((s) => (s.selectedId ? s.sessions[s.selectedId] : undefined));
  // "알림 · 할 일": pending decisions plus open blocking tasks.
  const alerts = useAppState(alertsAndTasksCount);
  const [inboxTab, setInboxTab] = useState<InboxTab | undefined>(undefined);
  const [commandTab, setCommandTab] = useState<CommandTab>('conversation');
  // Conductor requests waiting for the person: the 지휘소 badge.
  // ... plus what the conductor's own session waits on (trust, a decision).
  const conductorState = useConductor();
  // The same count as 지휘소's approvals inbox: only what the conductor raised.
  const conductorPending = useInboxCount();
  // An alert opened its session, from anywhere (toast, OS notification,
  // automatic switch): show it.
  const jumpToken = useAppState((s) => s.jump?.token ?? null);
  // The conductor's own session has no card: it opens in 지휘소.
  const jumpToConductor = useAppState((s) => !!s.jump && !!s.sessions[s.jump.sessionId]?.conductor);
  const [shownJump, setShownJump] = useState<number | null>(null);
  if (jumpToken !== shownJump) {
    setShownJump(jumpToken);
    if (jumpToken !== null && jumpToConductor) {
      setCommandTab('conversation');
      setView('conductor');
    } else if (jumpToken !== null) {
      setView('sessions');
      setPhoneList(false);
    }
  }
  useEffect(() => {
    if (jumpToken === null) return;
    // Sessions without a decision card have nothing to take the focus: a
    // decision that comes later must not.
    const timer = setTimeout(() => actions.finishJump(), JUMP_MS);
    return () => clearTimeout(timer);
  }, [jumpToken, actions]);
  useEffect(trackTyping, []);
  // Files dropped outside a session pane must not open in the webview.
  useEffect(() => installFileDropGuard(), []);
  // The composer's "지휘 모드" shortcut.
  useEffect(
    () =>
      onConductorViewRequest(() => {
        setCommandTab('conversation');
        setView('conductor');
      }),
    [],
  );
  // "Open Settings" buttons in error messages and prompts.
  useEffect(() => onSettingsViewRequest(() => setView('settings')), []);

  // A pairing link opened the app: show the pairing form (it takes the link).
  useEffect(() => native?.pairLinks?.subscribe(() => setView('pairing')), [native]);

  const openSession = (id: string) => {
    actions.select(id);
    setView('sessions');
    setPhoneList(false);
  };
  const openCommand = (tab: CommandTab = 'conversation') => {
    setCommandTab(tab);
    setView('conductor');
  };
  // The conductor's session lives in 지휘소, never on the session screen.
  const shown = selected && !selected.conductor ? selected : undefined;

  let main;
  /** Phones, inside a session: the top bar replaces the header and the bottom menu. */
  let phoneSession = false;
  if (
    link?.state === 'login_required' &&
    view !== 'settings' &&
    view !== 'pairing' &&
    view !== 'connect'
  )
    main = (
      <div className={styles.page}>
        <LoginView machine={machine} />
      </div>
    );
  else if (view === 'conductor')
    main = (
      <div className={styles.command}>
        {/* Entering asks for a fresh unlock (older than 5 minutes), lock on or off;
            not while the conductor is off (only that notice shows). */}
        <CommandCenterGate open={conductorState?.settings.enabled === false}>
          <CommandCenterView
            tab={commandTab}
            onTab={setCommandTab}
            onOpenSettings={() => setView('settings')}
            onOpenSession={openSession}
            onOpenTasks={() => {
              setInboxTab('tasks');
              setView('alerts');
            }}
          />
        </CommandCenterGate>
      </div>
    );
  else if (view === 'alerts')
    main = (
      <div className={styles.page}>
        <AlertsAndTasksView tab={inboxTab} onTab={setInboxTab} />
      </div>
    );
  else if (view === 'windows')
    main = (
      <div className={styles.page}>
        <WindowsPanel key={machine.id} />
      </div>
    );
  else if (view === 'resources')
    main = (
      <div className={styles.page}>
        <ResourcesPanel />
      </div>
    );
  else if (view === 'settings')
    main = (
      <div className={styles.page}>
        <SettingsView
          onPair={() => setView('pairing')}
          onConnectPhone={native?.host ? () => setView('connect') : undefined}
          onOpenCommand={() => openCommand()}
        />
      </div>
    );
  else if (view === 'connect' && native?.host)
    main = (
      <div className={styles.page}>
        <ConnectPhoneView host={native.host} onBack={() => setView('settings')} />
      </div>
    );
  else if (view === 'pairing')
    main = (
      <div className={styles.page}>
        <PairingView
          onPaired={(id) => {
            switchMachine(id);
            setView('sessions');
          }}
          onConnectPhone={native?.host ? () => setView('connect') : undefined}
        />
      </div>
    );
  else if (wide) {
    main = (
      <div className={styles.split}>
        <div className={styles.deckPane}>
          <SessionDeck onOpen={() => {}} onNewSession={() => setNewSession(true)} />
        </div>
        <div className={styles.livePane}>
          {shown ? <LiveView key={shown.id} session={shown} /> : <LiveEmpty />}
        </div>
      </div>
    );
  } else if (shown && !phoneList) {
    // Phones, a session: one slim top bar (back, the session name that swipes
    // between sessions, the view, status and "more") with the session strip
    // under it, over the chat; no app header or bottom menu.
    phoneSession = true;
    main = (
      <div className={styles.phone}>
        <div className={styles.phoneLive}>
          <LiveView
            key={shown.id}
            session={shown}
            chrome={{
              onBack: () => setPhoneList(true),
              // The bottom menu (with 지휘소's badge) is hidden here: the
              // conductor's requests, listed under Alerts too, count on "more".
              alerts: alerts + pendingCount(conductorState),
              onOpenAlerts: () => {
                setInboxTab(undefined);
                setView('alerts');
              },
              headerItems: (
                <HeaderMenuItems
                  onManageMachines={() => setManageMachines(true)}
                  onAddMachine={() => setView('pairing')}
                  onOpenCommand={openCommand}
                  inSession
                />
              ),
              strip: ({ onPicked, trailing }) => (
                <SessionDeck
                  variant="strip"
                  onOpen={onPicked}
                  onNewSession={() => setNewSession(true)}
                  trailing={trailing}
                />
              ),
            }}
          />
        </div>
      </div>
    );
  } else {
    // Phones, the session list: the cards; a tap opens the session.
    main = (
      <div className={styles.page}>
        <SessionDeck onOpen={() => setPhoneList(false)} onNewSession={() => setNewSession(true)} />
      </div>
    );
  }

  return (
    <div className={styles.app} data-phone-session={phoneSession || undefined}>
      <a className="skip-link" href="#main">
        {t('app.skipToContent')}
      </a>
      {!phoneSession && (
        <Header
          onManageMachines={() => setManageMachines(true)}
          onAddMachine={() => setView('pairing')}
          onOpenCommand={openCommand}
        />
      )}
      {manageMachines && (
        <MachinesSheet
          onClose={() => setManageMachines(false)}
          onPair={() => {
            setManageMachines(false);
            setView('pairing');
          }}
        />
      )}
      {/* Hidden inside a session on phones: Back returns to the list, where
          it shows again; alerts stay reachable from the top bar's "more". */}
      {!phoneSession && (
        <nav className={styles.nav} aria-label={t('nav.main')}>
          {NAV.map((n) => (
            <button
              key={n.view}
              type="button"
              className={styles.navItem}
              aria-current={view === n.view ? 'page' : undefined}
              onClick={() => setView(n.view)}
            >
              <span className={styles.navIcon}>
                <Icon name={n.icon} size={22} />
                {n.view === 'alerts' && alerts > 0 && (
                  <span className={`badge ${styles.navBadge}`} aria-hidden="true">
                    {alerts}
                  </span>
                )}
                {n.view === 'conductor' && conductorPending > 0 && (
                  <span
                    className={`badge ${styles.navBadge}`}
                    aria-hidden="true"
                    data-testid="conductor-badge"
                  >
                    {conductorPending}
                  </span>
                )}
              </span>
              <span className={styles.navLabel}>{t(n.label)}</span>
              {n.view === 'alerts' && alerts > 0 && (
                <span className="visually-hidden">{t('alerts.count', { count: alerts })}</span>
              )}
              {n.view === 'conductor' && conductorPending > 0 && (
                <span className="visually-hidden">
                  {t('conductor.pendingCount', { count: conductorPending })}
                </span>
              )}
            </button>
          ))}
        </nav>
      )}
      <main id="main" className={styles.main} tabIndex={-1}>
        {main}
      </main>
      {/* Ctrl+K, or the session title on phones: jump to any session. */}
      <SessionSwitcher onOpen={openSession} />
      <Toasts onOpen={(id) => actions.openAlert({ sessionId: id })} />
      {/* Desktop app: automatic update checks (only ever a toast). */}
      <UpdateNotifier onOpenSettings={() => setView('settings')} />
      {/* Desktop app: first-run security wizard and its reminder. */}
      <SecurityGate onOpenSettings={() => setView('settings')} />
      <TasksWatcher
        onOpen={() => {
          setInboxTab('tasks');
          setView('alerts');
        }}
      />
      {newSession && (
        <NewSessionDialog
          projectDirs={machine.projectDirs ?? []}
          onClose={() => setNewSession(false)}
          onLaunched={(s) => {
            setNewSession(false);
            openSession(s.id);
          }}
        />
      )}
    </div>
  );
}

/** Applies theme and language to the document root. */
function DocumentSettings() {
  const { settings } = useSettings();
  useEffect(() => {
    const root = document.documentElement;
    if (settings.theme === 'system') delete root.dataset.theme;
    else root.dataset.theme = settings.theme;
    root.lang = settings.language;
  }, [settings.theme, settings.language]);
  return null;
}

/** Speech services, or a factory that binds them to the selected machine. */
export type SpeechConfig = SpeechServices | ((machine: Machine) => SpeechServices);

function Localized({ speech, appLock }: { speech: SpeechConfig; appLock: AppLockVerifier | null }) {
  const { settings } = useSettings();
  const { machine } = useMachines();
  const services = useMemo(
    () => (typeof speech === 'function' ? speech(machine) : speech),
    [speech, machine],
  );
  return (
    <I18nProvider locale={settings.language}>
      <SpeechProvider value={services}>
        <DocumentSettings />
        <PushOpenedWatcher />
        {/* Locked: nothing below renders (no sessions, sends, decisions or voice). */}
        <AppLockProvider verifier={appLock}>
          <ShellProvider>
            <StepUpProvider>
              <Shell />
            </StepUpProvider>
          </ShellProvider>
        </AppLockProvider>
      </SpeechProvider>
    </I18nProvider>
  );
}

/** Shown in native mode while no machine is paired: only the pairing screen. */
function Setup({ settings }: { settings?: Settings }) {
  const [initial] = useState(() => settings ?? loadSettings());
  return (
    <I18nProvider locale={initial.language}>
      <StepUpProvider>
        <SetupContent />
      </StepUpProvider>
    </I18nProvider>
  );
}

function SetupContent() {
  const { t } = useI18n();
  const native = useNative();
  // Desktop: run the server on this PC and connect a phone to it.
  const [connect, setConnect] = useState(false);
  return (
    <div className={`${styles.app} ${styles.setup}`}>
      <main id="main" className={styles.main} tabIndex={-1}>
        <div className={styles.page}>
          {connect && native?.host ? (
            <ConnectPhoneView host={native.host} onBack={() => setConnect(false)} />
          ) : (
            <PairingView
              intro={t('pairing.noMachines')}
              onConnectPhone={native?.host ? () => setConnect(true) : undefined}
              // Desktop: this PC's own server first; pairing is for another computer.
              lead={native?.host ? <LocalServerCard host={native.host} /> : undefined}
            />
          )}
        </div>
      </main>
    </div>
  );
}

export interface AppProps {
  /** May change over time (native mode); an empty list shows the pairing screen. */
  machines: Machine[];
  speech: SpeechConfig;
  settings?: Settings;
  /** Native shell services (pairing, forget); absent in browser/mock mode. */
  native?: NativeServices;
  /** Machine selected at start (the user's default); the first one otherwise. */
  initialMachineId?: string;
  /**
   * The app lock's verifier (the device sign-in). Absent: no lock (the
   * browser build, tests). The pairing screen shown without any machine has
   * nothing to protect and is not locked; creating pairing codes is gated.
   */
  appLock?: AppLockVerifier | null;
}

export function App({
  machines,
  speech,
  settings,
  native,
  initialMachineId,
  appLock = null,
}: AppProps) {
  return (
    <NativeProvider value={native ?? null}>
      {machines.length === 0 ? (
        <Setup settings={settings} />
      ) : (
        <AppProvider machines={machines} settings={settings} initialMachineId={initialMachineId}>
          <Localized speech={speech} appLock={appLock} />
        </AppProvider>
      )}
    </NativeProvider>
  );
}
