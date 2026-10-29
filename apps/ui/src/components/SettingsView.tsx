import { useState } from 'react';

import { useI18n } from '../i18n/context';
import { LOCALES, isLocale } from '../i18n/i18n';
import { useNative } from '../native/context';
import { describeNativeError } from '../native/errors';
import {
  ALERT_CATEGORIES,
  MIC_MODES,
  STT_ENGINES,
  type SttEngine,
  type ThemeSetting,
} from '../state/settings';
import { APP_VERSION, useActions, useAppState, useMachines, useSettings } from '../state/store';
import { Icon } from './Icon';
import { ConfirmDialog } from './Modal';
import styles from './SettingsView.module.css';

export const SUPPORT_URL = 'https://github.com/JinsongRoh/Voice-Command-AI#supporting-the-project';

const THEMES: ThemeSetting[] = ['system', 'light', 'dark'];

export function SettingsView({ onPair }: { onPair: () => void }) {
  const { t, relative } = useI18n();
  const { settings, updateSettings } = useSettings();
  const { machine } = useMachines();
  const actions = useActions();
  const devices = useAppState((s) => s.devices);
  const connection = useAppState((s) => s.connection);
  const [revoke, setRevoke] = useState<{ id: string; name: string } | null>(null);
  const [confirmKill, setConfirmKill] = useState(false);
  const [confirmForget, setConfirmForget] = useState(false);
  const native = useNative();
  const [error, setError] = useState<string | null>(null);

  const run = (p: Promise<void>) => {
    setError(null);
    p.catch((err: Error) => setError(t('error.request', { error: err.message })));
  };

  return (
    <div className={styles.settings}>
      <h2 className={styles.title}>{t('settings.title')}</h2>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}

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

      <section className={`panel ${styles.section}`} aria-labelledby="set-notify">
        <h3 id="set-notify">{t('settings.notifications')}</h3>
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
            checked={settings.spokenAlerts}
            onChange={(e) => updateSettings({ spokenAlerts: e.target.checked })}
          />
          {t('settings.spoken')}
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.quietHours.enabled}
            onChange={(e) =>
              updateSettings({ quietHours: { ...settings.quietHours, enabled: e.target.checked } })
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
                  updateSettings({ quietHours: { ...settings.quietHours, start: e.target.value } })
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

      <section className={`panel ${styles.section}`} aria-labelledby="set-speech">
        <h3 id="set-speech">{t('settings.speech')}</h3>
        <div className="field">
          <label htmlFor="set-stt">{t('settings.sttEngine')}</label>
          <select
            id="set-stt"
            value={settings.sttEngine}
            onChange={(e) => updateSettings({ sttEngine: e.target.value as SttEngine })}
          >
            {STT_ENGINES.map((engine) => (
              <option key={engine} value={engine}>
                {t(`settings.stt.${engine}`)}
              </option>
            ))}
          </select>
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
      </section>

      <section className={`panel ${styles.section}`} aria-labelledby="set-conn">
        <h3 id="set-conn">{t('settings.connectivity')}</h3>
        <dl className={styles.dl}>
          <dt>{t('header.machine')}</dt>
          <dd>{machine.name}</dd>
          <dt>{t('settings.provider')}</dt>
          <dd>{machine.connectivity}</dd>
          <dt>{t('settings.status')}</dt>
          <dd>{t(`conn.${connection}`)}</dd>
        </dl>
        {native && machine.native && (
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

      <section className={`panel ${styles.section}`} aria-labelledby="set-devices">
        <h3 id="set-devices">{t('settings.devices')}</h3>
        {devices === null ? (
          <p className="muted">{t('settings.devicesUnsupported')}</p>
        ) : (
          <ul className={styles.devices}>
            {devices.map((d) => (
              <li key={d.id} className={styles.device}>
                <Icon name="laptop" />
                <span className={styles.deviceText}>
                  <strong>
                    {d.name}{' '}
                    {d.current && (
                      <span className={styles.current}>{t('settings.thisDevice')}</span>
                    )}
                  </strong>
                  <span className="muted">
                    {d.platform} · {t('settings.lastSeen', { time: relative(d.last_seen) })}
                  </span>
                </span>
                {!d.current && (
                  <button
                    type="button"
                    className="btn btn-sm"
                    onClick={() => setRevoke({ id: d.id, name: d.name })}
                  >
                    {t('settings.revoke')}
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
        <button type="button" className="btn btn-sm" onClick={onPair}>
          <Icon name="qr" size={16} />
          {t('nav.pairing')}
        </button>
      </section>

      <section className={`panel ${styles.section}`} aria-labelledby="set-security">
        <h3 id="set-security">{t('settings.security')}</h3>
        <p className="muted">{t('settings.killSwitchHelp')}</p>
        <button type="button" className="btn btn-danger" onClick={() => setConfirmKill(true)}>
          <Icon name="power" size={18} />
          {t('settings.killSwitch')}
        </button>
      </section>

      {machine.simulate && (
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
      )}

      <section className={`panel ${styles.section}`} aria-labelledby="set-about">
        <h3 id="set-about">{t('settings.about')}</h3>
        <p className="muted">
          {t('app.title')} · {t('settings.version', { version: APP_VERSION })}
        </p>
        <a className={styles.support} href={SUPPORT_URL} target="_blank" rel="noreferrer">
          <Icon name="heart" size={14} />
          {t('settings.support')}
        </a>
      </section>

      {revoke && (
        <ConfirmDialog
          title={t('settings.revoke')}
          message={t('settings.revokeConfirm', { name: revoke.name })}
          confirmLabel={t('settings.revoke')}
          danger
          onCancel={() => setRevoke(null)}
          onConfirm={() => {
            run(actions.revokeDevice(revoke.id));
            setRevoke(null);
          }}
        />
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
