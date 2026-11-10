import {
  Fragment,
  lazy,
  Suspense,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from 'react';

import { useI18n } from '../i18n/context';
import { LOCALES, isLocale, type MessageKey } from '../i18n/i18n';
import { useNative } from '../native/context';
import { describeNativeError } from '../native/errors';
import { isLocalMachine } from '../native/host';
import {
  ALERT_CATEGORIES,
  MIC_MODES,
  SPEECH_LANGUAGES,
  type SpeechLanguage,
  type ThemeSetting,
} from '../state/settings';
import { BUILD_STAMP } from '../state/core';
import { FEATURE_PERSONAL_TERMS } from '../protocol/personalTerms';
import { APP_VERSION, useActions, useAppState, useMachines, useSettings } from '../state/store';
import { AgentHooksHint, AgentHooksSettings } from './AgentHooksSettings';
import { AppLockSettings, useAppLock } from './AppLock';
import { ApprovalModeControl } from './AlertsView';
import { AuditLog } from './AuditLog';
import { ConductorSettings } from './ConductorSettings';
import { DevicesPanel } from './DevicesPanel';
import { DnsPilotPanel } from './DnsPilotConnect';
import { EngineSetup, EngineSetupHint } from './EngineSetup';
import { HandsFreeSettings } from './HandsFree';
import { Icon } from './Icon';
import { LaunchAtLoginSettings } from './LaunchAtLoginSettings';
import { MachinesPanel } from './MachinesPanel';
import { MicAnimationSettings } from './MicAnimationSettings';
import { DiagLog } from './DiagLog';
import { ConfirmDialog } from './Modal';
import { PushAlertsSettings } from './PushAlertsSettings';
import { useDeviceScope } from './ShellProvider';
import { ResultSummarySettings } from './ResultSummarySettings';
import { MailSettings, SecurityHint, SecurityOverview, SecurityRecovery } from './SecuritySettings';
import { PersonalTermsSettings } from './PersonalTermsSettings';
import { PronunciationSettings } from './PronunciationSettings';
import { TtsSettings } from './TtsSettings';
import { SpeechServerSettings } from './SpeechServerSettings';
import { UpdateSettings } from './UpdateSettings';
import {
  SETTINGS_SECTIONS,
  SETTINGS_TAB_LABELS,
  SETTINGS_TABS,
  loadSettingsTab,
  peekSettingsTarget,
  saveSettingsTab,
  subscribeSettingsTarget,
  tabOf,
  type SettingsSectionId,
  type SettingsTab,
  type SettingsTarget,
} from './settingsNav';
import styles from './SettingsView.module.css';

export const REPO_URL = 'https://github.com/JinsongRoh/Voice-Command-AI';
export const SUPPORT_URL = `${REPO_URL}#supporting-the-project`;
/** The user manual (opens in the browser, like the other external links). */
export const USER_GUIDE_URL = `${REPO_URL}/blob/main/docs/user-guide.md`;
const SITE_URL = 'https://vca.metavu.io';
const DNSPILOT_URL = 'https://dnspilot.kr';
const DNSPILOT_CONSOLE_URL = 'https://dns.dnspilot.kr';

const THEMES: ThemeSetting[] = ['system', 'light', 'dark', 'black'];

/** Open-source licenses screen (own chunk with the notices data). */
const OpenSourceLicenses = lazy(() => import('./OpenSourceLicenses'));

/** One top-level section: its tab comes from `SETTINGS_SECTIONS`. */
interface SectionDef {
  id: SettingsSectionId;
  title: MessageKey;
  /** Settings inside the section, so the search finds them too. */
  keywords?: MessageKey[];
  /**
   * Untranslated search words (product names, technical terms people type
   * in any language): "totp", "whisper", "claude", ...
   */
  terms?: string[];
  /** Hidden where it does not apply (phones, the browser, paired PCs). */
  show?: boolean;
  render: () => ReactNode;
}

export function SettingsView({
  onPair,
  onConnectPhone,
  onOpenCommand,
}: {
  onPair: () => void;
  /** Desktop: opens "Connect a phone". */
  onConnectPhone?: () => void;
  /** Opens the Command Center (the conductor). */
  onOpenCommand?: () => void;
}) {
  const { t } = useI18n();
  const { settings, updateSettings } = useSettings();
  const { machine } = useMachines();
  const actions = useActions();
  const connection = useAppState((s) => s.connection);
  const hasMisheardWords = useAppState(
    (s) => s.connection === 'connected' && s.serverFeatures.includes(FEATURE_PERSONAL_TERMS),
  );
  const [confirmKill, setConfirmKill] = useState(false);
  const [confirmForget, setConfirmForget] = useState(false);
  const native = useNative();
  const appLock = useAppLock();
  const scope = useDeviceScope();
  const [error, setError] = useState<string | null>(null);
  const [showLicenses, setShowLicenses] = useState(false);
  const [tab, setTab] = useState<SettingsTab>(() => {
    const target = peekSettingsTarget();
    return target ? tabOf(target) : loadSettingsTab();
  });
  // A deep link to scroll to once its tab is shown (a new object per link).
  const [jump, setJump] = useState<{ target: SettingsTarget } | null>(() => {
    const target = peekSettingsTarget();
    return target ? { target } : null;
  });
  const [query, setQuery] = useState('');
  const uid = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const tabRefs = useRef<Partial<Record<SettingsTab, HTMLButtonElement | null>>>({});

  const run = (p: Promise<void>) => {
    setError(null);
    p.catch((err: Error) => setError(t('error.request', { error: err.message })));
  };

  const localEngines = !!native?.host?.engines && isLocalMachine(machine.id);

  const select = (next: SettingsTab) => {
    setTab(next);
    saveSettingsTab(next);
  };

  const goTo = (next: SettingsTarget) => {
    setQuery('');
    select(tabOf(next));
    setJump({ target: next });
  };

  // Deep links while Settings is open (e.g. the update toast).
  useEffect(
    () =>
      subscribeSettingsTarget((next) => {
        setQuery('');
        setTab(tabOf(next));
        setJump({ target: next });
      }),
    [],
  );

  // Keep the selected tab visible in the scrolling tab row (phones).
  useLayoutEffect(() => {
    const list = listRef.current;
    const el = tabRefs.current[tab];
    if (!list || !el) return;
    const start = el.offsetLeft;
    const end = start + el.offsetWidth;
    if (start < list.scrollLeft) list.scrollLeft = Math.max(0, start - 24);
    else if (end > list.scrollLeft + list.clientWidth)
      list.scrollLeft = end - list.clientWidth + 24;
  }, [tab]);

  // Land on the deep link's section (its tab is shown by now).
  useEffect(() => {
    if (!jump) return;
    const anchor = document.getElementById(jump.target);
    // A section heading stands for its whole section; other anchors for themselves.
    const el =
      anchor && /^H\d$/.test(anchor.tagName) ? (anchor.closest('section') ?? anchor) : anchor;
    if (!el) return;
    el.scrollIntoView?.({ block: 'start' });
    // A short highlight shows which section the link meant.
    const flash = styles.flash;
    if (!flash) return;
    // (Not cleared on cleanup: the class must not stay behind.)
    el.classList.add(flash);
    setTimeout(() => el.classList.remove(flash), 1600);
  }, [jump]);

  const onTabKey = (e: KeyboardEvent<HTMLButtonElement>) => {
    const i = SETTINGS_TABS.indexOf(tab);
    const last = SETTINGS_TABS.length - 1;
    const next =
      e.key === 'ArrowRight'
        ? SETTINGS_TABS[i === last ? 0 : i + 1]
        : e.key === 'ArrowLeft'
          ? SETTINGS_TABS[i === 0 ? last : i - 1]
          : e.key === 'Home'
            ? SETTINGS_TABS[0]
            : e.key === 'End'
              ? SETTINGS_TABS[last]
              : undefined;
    if (!next) return;
    e.preventDefault();
    select(next);
    tabRefs.current[next]?.focus();
  };

  const openTab = (next: SettingsTab) => {
    setQuery('');
    select(next);
    // Scrolled down (the tab row is stuck): start the new tab at its top.
    const root = rootRef.current;
    const list = listRef.current;
    if (
      root &&
      list &&
      list.getBoundingClientRect().top - root.getBoundingClientRect().top > list.offsetTop + 1
    )
      root.scrollIntoView?.({ block: 'start' });
  };

  const sections: SectionDef[] = [
    {
      id: 'set-appearance',
      title: 'settings.appearance',
      keywords: ['settings.theme', 'settings.language'],
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-appearance">
          <h3 id="set-appearance">{t('settings.appearance')}</h3>
          <div className="field">
            <span className="label" id="set-theme">
              {t('settings.theme')}
            </span>
            <div className="segmented" role="group" aria-labelledby="set-theme">
              {THEMES.map((theme) => (
                <button
                  key={theme}
                  type="button"
                  aria-pressed={settings.theme === theme}
                  onClick={() => updateSettings({ theme })}
                >
                  {t(`settings.theme.${theme}`)}
                </button>
              ))}
            </div>
          </div>
          <div className="field">
            <label htmlFor="set-language">{t('settings.language')}</label>
            <select
              id="set-language"
              value={settings.language}
              onChange={(e) =>
                isLocale(e.target.value) && updateSettings({ language: e.target.value })
              }
            >
              {LOCALES.map((l) => (
                <option key={l.code} value={l.code} lang={l.code}>
                  {l.name}
                </option>
              ))}
            </select>
          </div>
        </section>
      ),
    },
    {
      // Desktop app only: the OS's sign-in entry (not on phones or in the browser).
      id: 'set-startup',
      title: 'launchAtLogin.title',
      keywords: ['launchAtLogin.toggle'],
      terms: ['autostart', 'startup', 'login', 'boot', 'reboot', '부팅', '자동 시작', '自動起動'],
      show: !!native?.host?.launchAtLogin,
      render: () =>
        native?.host?.launchAtLogin ? (
          <section className={`panel ${styles.section}`} aria-labelledby="set-startup">
            <h3 id="set-startup">{t('launchAtLogin.title')}</h3>
            <LaunchAtLoginSettings services={native.host.launchAtLogin} />
          </section>
        ) : null,
    },
    {
      id: 'set-speech',
      title: 'settings.speech',
      keywords: [
        'settings.speechLanguage',
        'settings.micMode',
        'settings.noiseSuppression',
        'settings.micAnimation',
        'settings.speechEngine',
        'settings.speechLlm',
        'handsFree.label',
      ],
      terms: [
        'stt',
        'whisper',
        'openai',
        'llm',
        'voco',
        'mic',
        'microphone',
        'animation',
        'dictation',
        'noise',
        '소음',
        'ノイズ',
        'ruido',
        '마이크',
        '애니메이션',
        '받아쓰기',
        'マイク',
        'アニメーション',
        '音声入力',
        'micrófono',
        'animación',
        'dictado',
      ],
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-speech">
          <h3 id="set-speech">{t('settings.speech')}</h3>
          <SpeechServerSettings />
          <div className="field">
            <label htmlFor="set-speech-lang">{t('settings.speechLanguage')}</label>
            <select
              id="set-speech-lang"
              aria-describedby="set-speech-lang-help"
              value={settings.speechLanguage}
              onChange={(e) => updateSettings({ speechLanguage: e.target.value as SpeechLanguage })}
            >
              {SPEECH_LANGUAGES.map((lang) => (
                <option key={lang} value={lang}>
                  {t(`settings.speechLanguage.${lang}`)}
                </option>
              ))}
            </select>
            <p
              id="set-speech-lang-help"
              className="muted"
              style={{ margin: '6px 0 0', fontSize: 13 }}
            >
              {t('settings.speechLanguageHelp')}
            </p>
          </div>
          <div className="field">
            <span className="label" id="set-mic-mode">
              {t('settings.micMode')}
            </span>
            <div className="segmented" role="group" aria-labelledby="set-mic-mode">
              {MIC_MODES.map((micMode) => (
                <button
                  key={micMode}
                  type="button"
                  aria-pressed={settings.micMode === micMode}
                  onClick={() => updateSettings({ micMode })}
                >
                  {t(`settings.micMode.${micMode}`)}
                </button>
              ))}
            </div>
          </div>
          <div className="field">
            <label className="check">
              <input
                type="checkbox"
                checked={settings.noiseSuppression}
                aria-describedby="set-noise-help"
                onChange={(e) => updateSettings({ noiseSuppression: e.target.checked })}
              />
              {t('settings.noiseSuppression')}
            </label>
            <p id="set-noise-help" className="muted" style={{ margin: '6px 0 0', fontSize: 13 }}>
              {t('settings.noiseSuppressionHelp')}
            </p>
          </div>
          <MicAnimationSettings />
          <HandsFreeSettings />
          {!localEngines && <EngineSetupHint />}
        </section>
      ),
    },
    {
      id: 'set-misheard',
      title: 'misheard.title',
      keywords: ['misheard.word'],
      terms: ['stt', 'vocabulary', 'dictionary', '단어', '사전'],
      show: hasMisheardWords,
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-misheard">
          <h3 id="set-misheard">{t('misheard.title')}</h3>
          <PersonalTermsSettings key={machine.id} />
        </section>
      ),
    },
    {
      id: 'set-spoken',
      title: 'settings.spokenSection',
      keywords: ['settings.spoken', 'pron.title', 'summary.setting', 'settings.tts.voice'],
      terms: ['tts', 'voice'],
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-spoken">
          <h3 id="set-spoken">{t('settings.spokenSection')}</h3>
          <label className="check">
            <input
              type="checkbox"
              checked={settings.spokenAlerts}
              onChange={(e) => updateSettings({ spokenAlerts: e.target.checked })}
            />
            {t('settings.spoken')}
          </label>
          {settings.spokenAlerts && (
            <>
              <TtsSettings />
              <PronunciationSettings />
            </>
          )}
          <ResultSummarySettings />
        </section>
      ),
    },
    {
      id: 'set-engines',
      title: 'engines.title',
      terms: ['whisper', 'ollama', 'openai', 'llm', 'tts', 'stt'],
      show: localEngines,
      render: () =>
        native?.host?.engines && (
          <section className={`panel ${styles.section}`} aria-labelledby="set-engines">
            <h3 id="set-engines">{t('engines.title')}</h3>
            <EngineSetup services={native.host.engines} host={native.host} />
          </section>
        ),
    },
    {
      id: 'set-notify',
      title: 'settings.notifications',
      keywords: ['settings.quietHours', 'settings.notify.decision', 'settings.notify.error'],
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-notify">
          <h3 id="set-notify">{t('settings.notifications')}</h3>
          <ApprovalModeControl />
          {ALERT_CATEGORIES.map((c) => (
            <label key={c} className="check">
              <input
                type="checkbox"
                checked={settings.notifications[c]}
                onChange={(e) =>
                  updateSettings({
                    notifications: { ...settings.notifications, [c]: e.target.checked },
                  })
                }
              />
              {t(`settings.notify.${c}`)}
            </label>
          ))}
          <label className="check">
            <input
              type="checkbox"
              checked={settings.quietHours.enabled}
              onChange={(e) =>
                updateSettings({
                  quietHours: { ...settings.quietHours, enabled: e.target.checked },
                })
              }
            />
            {t('settings.quietHours')}
          </label>
          {settings.quietHours.enabled && (
            <div className={styles.row}>
              <div className="field">
                <label htmlFor="quiet-from">{t('settings.quietFrom')}</label>
                <input
                  id="quiet-from"
                  type="time"
                  value={settings.quietHours.start}
                  onChange={(e) =>
                    updateSettings({
                      quietHours: { ...settings.quietHours, start: e.target.value },
                    })
                  }
                />
              </div>
              <div className="field">
                <label htmlFor="quiet-to">{t('settings.quietTo')}</label>
                <input
                  id="quiet-to"
                  type="time"
                  value={settings.quietHours.end}
                  onChange={(e) =>
                    updateSettings({ quietHours: { ...settings.quietHours, end: e.target.value } })
                  }
                />
              </div>
            </div>
          )}
        </section>
      ),
    },
    {
      id: 'set-push',
      title: 'push.title',
      show: !!native?.push,
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-push">
          <h3 id="set-push">{t('push.title')}</h3>
          <PushAlertsSettings />
        </section>
      ),
    },
    {
      // Agent hooks are installed on this PC by its own app only.
      id: 'set-agent-hooks',
      title: 'agentHooks.title',
      terms: ['claude', 'codex', 'hook', 'hooks', '훅', 'フック', 'gancho'],
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-agent-hooks">
          <h3 id="set-agent-hooks">{t('agentHooks.title')}</h3>
          {native?.host?.agentHooks ? (
            <AgentHooksSettings services={native.host.agentHooks} />
          ) : (
            <AgentHooksHint />
          )}
        </section>
      ),
    },
    {
      // Desktop only: pairing a phone and two-factor sign-in.
      id: 'set-connect-phone',
      title: 'host.connectPhone',
      keywords: ['host.totpTitle'],
      terms: ['qr', 'totp', '2fa', 'authenticator', '인증 앱', 'tailscale'],
      show: !!onConnectPhone,
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-connect-phone">
          <h3 id="set-connect-phone">{t('host.connectPhone')}</h3>
          <p className="muted">{t('settings.connectPhoneHelp')}</p>
          <div className={styles.row}>
            <button type="button" className="btn btn-sm btn-primary" onClick={onConnectPhone}>
              <Icon name="qr" size={16} />
              {t('host.connectPhone')}
            </button>
          </div>
        </section>
      ),
    },
    {
      id: 'set-conn',
      title: 'settings.connectivity',
      keywords: ['settings.forget'],
      terms: ['tailscale', 'tunnel', 'lan'],
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-conn">
          <h3 id="set-conn">{t('settings.connectivity')}</h3>
          <dl className={styles.dl}>
            <dt>{t('header.machine')}</dt>
            <dd>{machine.name}</dd>
            <dt>{t('settings.provider')}</dt>
            <dd>{isLocalMachine(machine.id) ? t('host.thisPc') : machine.connectivity}</dd>
            <dt>{t('settings.status')}</dt>
            <dd>{t(`conn.${connection}`)}</dd>
          </dl>
          {native && machine.native && !isLocalMachine(machine.id) && (
            <>
              <p className="muted">{t('settings.forgetHelp')}</p>
              <button
                type="button"
                className="btn btn-sm btn-danger"
                onClick={() => setConfirmForget(true)}
              >
                {t('settings.forget')}
              </button>
            </>
          )}
        </section>
      ),
    },
    {
      id: 'set-dnspilot',
      title: 'dnspilot.title',
      terms: ['dnspilot', 'tunnel', 'spki'],
      show: !!native?.dnspilot,
      render: () =>
        native?.dnspilot && (
          <section className={`panel ${styles.section}`} aria-labelledby="set-dnspilot">
            <h3 id="set-dnspilot">{t('dnspilot.title')}</h3>
            <DnsPilotPanel service={native.dnspilot} />
          </section>
        ),
    },
    {
      id: 'set-machines',
      title: 'machines.title',
      terms: ['pc', 'tailscale'],
      render: () => <MachinesPanel onPair={onPair} />,
    },
    {
      id: 'set-devices',
      title: 'settings.devices',
      terms: ['phone', 'scope', 'admin'],
      render: () => <DevicesPanel onPair={onPair} />,
    },
    {
      // Remote shell audit log: admin devices only (`null`: not reported).
      id: 'set-audit',
      title: 'audit.title',
      show: scope === null || scope === 'admin',
      render: () => <AuditLog />,
    },
    {
      // Only where the device can verify the person (the native shells).
      id: 'set-app-lock',
      title: 'appLock.settings.title',
      keywords: ['appLock.settings.toggle', 'appLock.settings.idle', 'appLock.settings.lockNow'],
      terms: ['lock', 'face id', 'touch id', 'windows hello', 'biometric', '잠금'],
      show: appLock.available,
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-app-lock">
          <h3 id="set-app-lock">{t('appLock.settings.title')}</h3>
          <AppLockSettings />
        </section>
      ),
    },
    {
      // Local verification, 2-step verification, mail: this PC's own app only.
      id: 'set-sec-status',
      title: 'security.section.status',
      keywords: ['security.item.verifier', 'security.item.totp', 'security.item.remoteShell'],
      terms: [
        'totp',
        '2fa',
        'otp',
        'authenticator',
        '인증 앱',
        'windows hello',
        'touch id',
        'step-up',
      ],
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-sec-status">
          <h3 id="set-sec-status">{t('security.section.status')}</h3>
          {native?.security ? <SecurityOverview services={native.security} /> : <SecurityHint />}
        </section>
      ),
    },
    {
      id: 'set-sec-mail',
      title: 'security.section.mail',
      keywords: ['security.mail.test', 'security.item.mail'],
      show: !!native?.security,
      render: () =>
        native?.security && (
          <section className={`panel ${styles.section}`} aria-labelledby="set-sec-mail">
            <h3 id="set-sec-mail">{t('security.section.mail')}</h3>
            <MailSettings services={native.security} />
          </section>
        ),
    },
    {
      id: 'set-sec-recovery',
      title: 'security.section.recovery',
      keywords: ['security.recovery.start'],
      terms: ['backup', 'totp', 'recovery'],
      show: !!native?.security,
      render: () =>
        native?.security && (
          <section className={`panel ${styles.section}`} aria-labelledby="set-sec-recovery">
            <h3 id="set-sec-recovery">{t('security.section.recovery')}</h3>
            <SecurityRecovery services={native.security} />
          </section>
        ),
    },
    {
      id: 'set-security',
      title: 'settings.killSwitch',
      keywords: ['settings.security'],
      terms: ['kill switch', 'kill-switch'],
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-security">
          <h3 id="set-security">{t('settings.killSwitch')}</h3>
          <p className="muted">{t('settings.killSwitchHelp')}</p>
          <button type="button" className="btn btn-danger" onClick={() => setConfirmKill(true)}>
            <Icon name="power" size={18} />
            {t('settings.killSwitch')}
          </button>
        </section>
      ),
    },
    {
      // Command Center: the conductor is set up on the PC itself (agent,
      // wake-ups, caps, auto-allow); use is in the Command Center and the header.
      id: 'set-conductor',
      title: 'settings.conductor',
      keywords: [
        'conductor.allowlist',
        'conductor.settings.maxActions',
        'conductor.settings.agent',
      ],
      terms: ['llm', 'claude', 'codex', 'openai', 'ollama', 'conductor', 'command center'],
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-conductor">
          <h3 id="set-conductor">{t('settings.conductor')}</h3>
          <ConductorSettings onOpenCommand={onOpenCommand} />
        </section>
      ),
    },
    {
      id: 'set-about',
      title: 'settings.about',
      keywords: ['settings.manual', 'update.heading', 'settings.sourceCode', 'settings.diag.title'],
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-about">
          <h3 id="set-about">{t('settings.about')}</h3>
          <p className="muted">
            {t('app.title')} · {t('settings.version', { version: APP_VERSION })} ·{' '}
            {t('settings.build', { build: BUILD_STAMP })}
          </p>
          <div className={styles.manualRow}>
            <a
              className="btn btn-sm btn-primary"
              href={USER_GUIDE_URL}
              target="_blank"
              rel="noreferrer"
            >
              <Icon name="book" size={16} />
              {t('settings.manual')}
              <Icon name="external" size={14} />
            </a>
            <p className="muted">{t('settings.manualHelp')}</p>
          </div>
          <div className={styles.aboutLinks}>
            <a className={styles.support} href={SITE_URL} target="_blank" rel="noreferrer">
              {t('settings.website')}
            </a>
            <a className={styles.support} href={REPO_URL} target="_blank" rel="noreferrer">
              {t('settings.sourceCode')}
            </a>
            <button
              type="button"
              className={styles.linkButton}
              onClick={() => setShowLicenses(true)}
            >
              {t('oss.view')}
            </button>
          </div>
          {/* Desktop app only (absent on phones and in the browser). */}
          <UpdateSettings />
          {/* Speech diagnostics: stays on this device until copied. */}
          <DiagLog />
        </section>
      ),
    },
    {
      id: 'set-oss',
      title: 'oss.title',
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-oss">
          <h3 id="set-oss">{t('oss.title')}</h3>
          <p className="muted">{t('oss.sectionHelp')}</p>
          <div className={styles.row}>
            <button type="button" className="btn btn-sm" onClick={() => setShowLicenses(true)}>
              {t('oss.open')}
            </button>
          </div>
        </section>
      ),
    },
    {
      id: 'set-support',
      title: 'settings.support',
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-support">
          <h3 id="set-support">
            <Icon name="heart" size={16} /> {t('settings.support')}
          </h3>
          <p className="muted">{t('settings.supportIntro')}</p>
          <ul className={styles.supportList}>
            <li>
              <strong>{t('settings.supportDnspilot')}</strong>
              <span className="muted">{t('settings.supportDnspilotBody')}</span>
              <span className={styles.supportLinks}>
                <a href={DNSPILOT_URL} target="_blank" rel="noreferrer">
                  dnspilot.kr
                </a>
                <a href={DNSPILOT_CONSOLE_URL} target="_blank" rel="noreferrer">
                  {t('settings.supportDnspilotConsole')}
                </a>
              </span>
            </li>
            <li>
              <strong>{t('settings.supportContribute')}</strong>
              <span className="muted">{t('settings.supportContributeBody')}</span>
              <span className={styles.supportLinks}>
                <a href={`${REPO_URL}/issues`} target="_blank" rel="noreferrer">
                  {t('settings.supportIssues')}
                </a>
              </span>
            </li>
            <li>
              <strong>{t('settings.supportStar')}</strong>
              <span className="muted">{t('settings.supportStarBody')}</span>
              <span className={styles.supportLinks}>
                <a href={REPO_URL} target="_blank" rel="noreferrer">
                  GitHub
                </a>
              </span>
            </li>
          </ul>
          <a className={styles.support} href={SUPPORT_URL} target="_blank" rel="noreferrer">
            {t('settings.supportMore')}
          </a>
        </section>
      ),
    },
    {
      id: 'set-dev',
      title: 'settings.developer',
      show: !!machine.simulate,
      render: () => (
        <section className={`panel ${styles.section}`} aria-labelledby="set-dev">
          <h3 id="set-dev">{t('settings.developer')}</h3>
          <div className={styles.row}>
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => machine.simulate?.drop(3000, false)}
            >
              {t('settings.simulateDrop')}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => machine.simulate?.drop(3000, true)}
            >
              {t('settings.simulateGap')}
            </button>
          </div>
        </section>
      ),
    },
  ];
  const shown = sections.filter((s) => s.show !== false);

  const needle = query.trim().toLocaleLowerCase();
  const matches = needle
    ? shown.filter(
        (s) =>
          [s.title, SETTINGS_TAB_LABELS[SETTINGS_SECTIONS[s.id]], ...(s.keywords ?? [])].some((k) =>
            t(k).toLocaleLowerCase().includes(needle),
          ) || (s.terms ?? []).some((term) => term.includes(needle)),
      )
    : [];

  const tabId = (x: SettingsTab) => `${uid}-tab-${x}`;
  const panelId = `${uid}-panel`;

  return (
    <div ref={rootRef} className={styles.settings}>
      <div className={styles.titleRow}>
        <h2 className={styles.title}>{t('settings.title')}</h2>
        <label className={styles.search}>
          <Icon name="search" size={16} />
          <span className="visually-hidden">{t('settings.search')}</span>
          <input
            type="search"
            value={query}
            placeholder={t('settings.search')}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape' && query) {
                e.preventDefault();
                setQuery('');
              }
            }}
          />
        </label>
        {/* The manual: book icon, plus its label where there is room. */}
        <a
          className={styles.manual}
          href={USER_GUIDE_URL}
          target="_blank"
          rel="noreferrer"
          title={t('settings.manual')}
          data-testid="settings-manual"
        >
          <Icon name="book" size={18} />
          <span className={styles.manualLabel}>{t('settings.manual')}</span>
        </a>
      </div>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}

      <div
        ref={listRef}
        className={styles.tabs}
        role="tablist"
        aria-label={t('settings.tabs')}
        data-testid="settings-tabs"
      >
        {SETTINGS_TABS.map((x) => (
          <button
            key={x}
            ref={(el) => {
              tabRefs.current[x] = el;
            }}
            type="button"
            role="tab"
            id={tabId(x)}
            aria-selected={!needle && tab === x}
            aria-controls={panelId}
            tabIndex={tab === x ? 0 : -1}
            className={styles.tab}
            onClick={() => openTab(x)}
            onKeyDown={onTabKey}
          >
            {t(SETTINGS_TAB_LABELS[x])}
          </button>
        ))}
      </div>

      {needle ? (
        <div className={styles.results} data-testid="settings-search-results">
          <p className="visually-hidden" aria-live="polite">
            {t('settings.searchCount', { count: matches.length })}
          </p>
          {matches.length === 0 ? (
            <p className="muted">{t('settings.searchNone')}</p>
          ) : (
            <ul className={styles.resultList}>
              {matches.map((s) => (
                <li key={s.id}>
                  <button type="button" className={styles.result} onClick={() => goTo(s.id)}>
                    <span>{t(s.title)}</span>
                    <span className={styles.resultTab}>
                      {t(SETTINGS_TAB_LABELS[SETTINGS_SECTIONS[s.id]])}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : (
        <div
          role="tabpanel"
          id={panelId}
          aria-labelledby={tabId(tab)}
          className={styles.panel}
          data-testid={`settings-panel-${tab}`}
        >
          {shown
            .filter((s) => SETTINGS_SECTIONS[s.id] === tab)
            .map((s) => (
              <Fragment key={s.id}>{s.render()}</Fragment>
            ))}
        </div>
      )}

      {showLicenses && (
        <Suspense fallback={null}>
          <OpenSourceLicenses onClose={() => setShowLicenses(false)} />
        </Suspense>
      )}
      {confirmForget && native && (
        <ConfirmDialog
          title={t('settings.forget')}
          message={t('settings.forgetConfirm', { machine: machine.name })}
          confirmLabel={t('settings.forgetConfirmAction')}
          danger
          onCancel={() => setConfirmForget(false)}
          onConfirm={() => {
            setConfirmForget(false);
            setError(null);
            native
              .forget(machine.id)
              .catch((err: unknown) => setError(describeNativeError(t, err)));
          }}
        />
      )}
      {confirmKill && (
        <ConfirmDialog
          title={t('settings.killSwitch')}
          message={t('settings.killConfirm', { machine: machine.name })}
          confirmLabel={t('settings.killConfirmAction')}
          danger
          onCancel={() => setConfirmKill(false)}
          onConfirm={() => {
            setConfirmKill(false);
            run(actions.killSwitch());
          }}
        />
      )}
    </div>
  );
}
