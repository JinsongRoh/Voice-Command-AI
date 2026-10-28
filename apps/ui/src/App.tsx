import { useEffect, useState } from 'react';

import { Header } from './components/Header';
import { Icon, type IconName } from './components/Icon';
import { LiveEmpty, LiveView } from './components/LiveView';
import { NewSessionDialog } from './components/NewSessionDialog';
import { PairingView } from './components/PairingView';
import { ResourcesPanel } from './components/ResourcesPanel';
import { SessionDeck } from './components/SessionDeck';
import { SettingsView } from './components/SettingsView';
import { Toasts } from './components/Toasts';
import { useMediaQuery } from './hooks/useMediaQuery';
import { I18nProvider, useI18n } from './i18n/context';
import type { MessageKey } from './i18n/i18n';
import { SpeechProvider, type SpeechServices } from './speech/context';
import { attentionCount } from './state/reducer';
import {
  AppProvider,
  useActions,
  useAppState,
  useMachines,
  useSettings,
  type Machine,
} from './state/store';
import type { Settings } from './state/settings';
import styles from './App.module.css';

type View = 'sessions' | 'resources' | 'settings' | 'pairing';

const NAV: { view: Exclude<View, 'pairing'>; label: MessageKey; icon: IconName }[] = [
  { view: 'sessions', label: 'nav.sessions', icon: 'list' },
  { view: 'resources', label: 'nav.resources', icon: 'gauge' },
  { view: 'settings', label: 'nav.settings', icon: 'settings' },
];

function Shell() {
  const { t } = useI18n();
  const actions = useActions();
  const { machine } = useMachines();
  const wide = useMediaQuery('(min-width: 900px)');
  const [view, setView] = useState<View>('sessions');
  const [detailOpen, setDetailOpen] = useState(false);
  const [newSession, setNewSession] = useState(false);
  const selected = useAppState((s) => (s.selectedId ? s.sessions[s.selectedId] : undefined));
  const attention = useAppState(attentionCount);

  const showDetail = !wide && detailOpen && !!selected && view === 'sessions';

  const openSession = (id: string) => {
    actions.select(id);
    setView('sessions');
    setDetailOpen(true);
  };

  let main;
  if (view === 'resources')
    main = (
      <div className={styles.page}>
        <ResourcesPanel />
      </div>
    );
  else if (view === 'settings')
    main = (
      <div className={styles.page}>
        <SettingsView onPair={() => setView('pairing')} />
      </div>
    );
  else if (view === 'pairing')
    main = (
      <div className={styles.page}>
        <PairingView />
      </div>
    );
  else if (wide) {
    main = (
      <div className={styles.split}>
        <div className={styles.deckPane}>
          <SessionDeck onOpen={() => {}} onNewSession={() => setNewSession(true)} />
        </div>
        <div className={styles.livePane}>
          {selected ? <LiveView session={selected} /> : <LiveEmpty />}
        </div>
      </div>
    );
  } else if (showDetail && selected) {
    main = (
      <div className={styles.detail}>
        <LiveView session={selected} onBack={() => setDetailOpen(false)} />
      </div>
    );
  } else {
    main = (
      <div className={styles.page}>
        <SessionDeck onOpen={() => setDetailOpen(true)} onNewSession={() => setNewSession(true)} />
      </div>
    );
  }

  return (
    <div className={`${styles.app} ${showDetail ? styles.noNav : ''}`}>
      <a className="skip-link" href="#main">
        {t('app.skipToContent')}
      </a>
      <Header />
      {!showDetail && (
        <nav className={styles.nav} aria-label={t('nav.main')}>
          {NAV.map((n) => (
            <button
              key={n.view}
              type="button"
              className={styles.navItem}
              aria-current={view === n.view ? 'page' : undefined}
              onClick={() => {
                setView(n.view);
                if (n.view === 'sessions') setDetailOpen(false);
              }}
            >
              <span className={styles.navIcon}>
                <Icon name={n.icon} size={22} />
                {n.view === 'sessions' && attention > 0 && (
                  <span className={`badge ${styles.navBadge}`} aria-hidden="true">
                    {attention}
                  </span>
                )}
              </span>
              <span>{t(n.label)}</span>
              {n.view === 'sessions' && attention > 0 && (
                <span className="visually-hidden">
                  {t('deck.attentionCount', { count: attention })}
                </span>
              )}
            </button>
          ))}
        </nav>
      )}
      <main id="main" className={styles.main} tabIndex={-1}>
        {main}
      </main>
      <Toasts onOpen={openSession} />
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

function Localized({ speech }: { speech: SpeechServices }) {
  const { settings } = useSettings();
  return (
    <I18nProvider locale={settings.language}>
      <SpeechProvider value={speech}>
        <DocumentSettings />
        <Shell />
      </SpeechProvider>
    </I18nProvider>
  );
}

export interface AppProps {
  machines: Machine[];
  speech: SpeechServices;
  settings?: Settings;
}

export function App({ machines, speech, settings }: AppProps) {
  return (
    <AppProvider machines={machines} settings={settings}>
      <Localized speech={speech} />
    </AppProvider>
  );
}
