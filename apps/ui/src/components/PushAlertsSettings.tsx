import { useCallback, useEffect, useRef, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { TranslateFn } from '../i18n/i18n';
import { toNativeError } from '../native/bridge';
import { useNative } from '../native/context';
import { describeNativeError } from '../native/errors';
import {
  phoneAlertsWanted,
  setPhoneAlertsWanted,
  type PushOpenedTarget,
  type PushPrompt,
  type PushService,
  type PushStatus,
} from '../native/push';
import { useActions, useAppState, useMachines, useSettings } from '../state/store';
import { ConfirmDialog } from './Modal';
import styles from './PushAlertsSettings.module.css';

/** Push error codes with a dedicated message (`push.error.<code>`). */
const PUSH_CODES = [
  'push_not_configured',
  'push_not_authorized',
  'push_reauthorize',
  'push_forbidden',
  'push_conflict',
  'push_unavailable',
  'push_failed',
  'push_token_invalid',
  'push_device_gone',
  'push_bad_key',
  'push_unsupported',
  'push_permission',
  'push_token_timeout',
  'push_registration_failed',
] as const;

type PushCode = (typeof PUSH_CODES)[number];

function isPushCode(code: string | null): code is PushCode {
  return code !== null && (PUSH_CODES as readonly string[]).includes(code);
}

/** A localized line for a push failure code (or any native error). */
// eslint-disable-next-line react-refresh/only-export-components
export function describePushError(t: TranslateFn, err: unknown): string {
  const e = toNativeError(err);
  if (isPushCode(e.code)) return t(`push.error.${e.code}`);
  return describeNativeError(t, err);
}

function codeText(t: TranslateFn, code: string): string {
  return isPushCode(code) ? t(`push.error.${code}`) : t('push.error.push_failed');
}

/**
 * Desktop: "Phone approval alerts". Off by default; turning it on starts the
 * separate push-only DNSPilot approval (never the tunnel grant).
 */
export function DesktopPushAlerts({ service }: { service: PushService }) {
  const { t } = useI18n();
  const devices = useAppState((s) => s.devices);
  const [status, setStatus] = useState<PushStatus | null>(null);
  const [prompt, setPrompt] = useState<PushPrompt | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmDisconnect, setConfirmDisconnect] = useState(false);
  const mounted = useRef(true);

  const reload = useCallback(async () => {
    try {
      const next = await service.status();
      if (mounted.current) setStatus(next);
    } catch (err) {
      if (mounted.current) setError(describePushError(t, err));
    }
  }, [service, t]);

  useEffect(() => {
    mounted.current = true;
    const first = setTimeout(() => void reload(), 0);
    // The watcher registers phones in the background: keep the list fresh.
    const timer = setInterval(() => void reload(), 10_000);
    return () => {
      mounted.current = false;
      clearTimeout(first);
      clearInterval(timer);
    };
  }, [reload]);

  const run = async (action: () => Promise<PushStatus>) => {
    setBusy(true);
    setError(null);
    try {
      const next = await action();
      if (mounted.current) setStatus(next);
    } catch (err) {
      if (mounted.current) setError(describePushError(t, err));
    } finally {
      if (mounted.current) setBusy(false);
    }
  };

  /** Approves the push grant first when needed, then turns alerts on. */
  const enable = async () => {
    setError(null);
    if (!status?.authorized) {
      let started: PushPrompt;
      try {
        started = await service.authorizeStart();
      } catch (err) {
        setError(describePushError(t, err));
        return;
      }
      setPrompt(started);
      try {
        const approved = await service.authorizeWait();
        if (mounted.current) setStatus(approved);
      } catch (err) {
        if (mounted.current && toNativeError(err).code !== 'dnspilot_cancelled')
          setError(describePushError(t, err));
        return;
      } finally {
        if (mounted.current) setPrompt(null);
      }
    }
    await run(() => service.setEnabled(true));
  };

  if (!status) {
    return (
      <div className={styles.box}>
        <p className="muted">{t('push.loading')}</p>
        {error && (
          <p role="alert" className={styles.error}>
            {error}
          </p>
        )}
      </div>
    );
  }

  const nameOf = (phone: string) => devices?.find((d) => d.id === phone)?.name ?? phone;

  return (
    <div className={styles.box}>
      <p className="muted">{t('push.intro')}</p>
      <p className={`muted ${styles.note}`}>{t('push.desktopRunning')}</p>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      {!status.available ? (
        <p className={styles.note}>{t('push.unavailable')}</p>
      ) : prompt ? (
        <div role="status" aria-live="polite" className={styles.box}>
          <p>{t('push.approve', { url: prompt.verificationUri })}</p>
          <p className={styles.code} aria-label={t('dnspilot.code')}>
            {prompt.userCode}
          </p>
          <p className={`muted ${styles.note}`}>{t('push.approveSeparate')}</p>
          <div className={styles.row}>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() =>
                void service.openActivation().catch((e) => setError(describePushError(t, e)))
              }
            >
              {t('dnspilot.open')}
            </button>
            <button
              type="button"
              className="btn"
              onClick={() => void service.authorizeCancel().catch(() => {})}
            >
              {t('dnspilot.cancel')}
            </button>
          </div>
          <p className="muted">{t('dnspilot.waiting')}</p>
        </div>
      ) : (
        <label className="check">
          <input
            type="checkbox"
            checked={status.enabled}
            disabled={busy}
            onChange={(e) => {
              if (e.target.checked) void enable();
              else void run(() => service.setEnabled(false));
            }}
          />
          {t('push.enable')}
        </label>
      )}

      {status.notConfigured && (
        <p role="status" className={styles.note}>
          {t('push.notConfigured')}
        </p>
      )}

      {status.enabled && (
        <>
          <h4 className={styles.phoneName}>{t('push.phones')}</h4>
          {status.phones.length === 0 ? (
            <p className={`muted ${styles.note}`}>{t('push.noPhones')}</p>
          ) : (
            <ul className={styles.phones}>
              {status.phones.map((p) => (
                <li key={p.phone} className={styles.phone}>
                  <span>
                    <span className={styles.phoneName}>{nameOf(p.phone)}</span>{' '}
                    <span className={`muted ${styles.phoneState}`}>
                      {p.lastError
                        ? codeText(t, p.lastError)
                        : p.registered
                          ? t('push.phoneReady')
                          : t('push.phoneWaiting')}
                    </span>
                  </span>
                  <label className="check">
                    <input
                      type="checkbox"
                      checked={p.muted}
                      disabled={busy}
                      onChange={(e) => void run(() => service.setMuted(p.phone, e.target.checked))}
                    />
                    {t('push.mute')}
                  </label>
                </li>
              ))}
            </ul>
          )}
        </>
      )}

      {status.authorized && !prompt && (
        <div className={styles.row}>
          <button
            type="button"
            className="btn btn-sm"
            disabled={busy}
            onClick={() => setConfirmDisconnect(true)}
          >
            {t('push.disconnect')}
          </button>
        </div>
      )}

      {confirmDisconnect && (
        <ConfirmDialog
          title={t('push.disconnect')}
          message={t('push.disconnectConfirm')}
          confirmLabel={t('push.disconnect')}
          onCancel={() => setConfirmDisconnect(false)}
          onConfirm={() => {
            setConfirmDisconnect(false);
            void run(() => service.disconnect());
          }}
        />
      )}
    </div>
  );
}

/** Phone: "Enable alerts on this phone" for the selected PC. */
export function PhonePushAlerts({ service }: { service: PushService }) {
  const { t } = useI18n();
  const { machine } = useMachines();
  const { settings } = useSettings();
  const [wanted, setWanted] = useState(() => phoneAlertsWanted(machine.id));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shownMachine, setShownMachine] = useState(machine.id);
  if (shownMachine !== machine.id) {
    setShownMachine(machine.id);
    setWanted(phoneAlertsWanted(machine.id));
    setError(null);
  }

  const toggle = async (on: boolean) => {
    setBusy(true);
    setError(null);
    try {
      if (on) await service.phoneEnable(machine.id, settings.language);
      else await service.phoneDisable(machine.id);
      setPhoneAlertsWanted(machine.id, on);
      setWanted(on);
    } catch (err) {
      // Turning off locally still stops the re-registration on launch.
      if (!on) {
        setPhoneAlertsWanted(machine.id, false);
        setWanted(false);
      }
      setError(describePushError(t, err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={styles.box}>
      <label className="check">
        <input
          type="checkbox"
          checked={wanted}
          disabled={busy}
          onChange={(e) => void toggle(e.target.checked)}
        />
        {t('push.phoneToggle')}
      </label>
      <p className={`muted ${styles.note}`}>{t('push.phoneHelp', { machine: machine.name })}</p>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
    </div>
  );
}

/** How long a tapped notification waits for its machine and session. */
const OPEN_WAIT_MS = 20_000;

/**
 * Phone: on launch and on resume, asks which session a tapped notification
 * points at and opens it. Only navigates; nothing is approved from a
 * notification. Also registers again on launch where the person turned
 * alerts on (device tokens change).
 */
export function PushOpenedWatcher() {
  const native = useNative();
  const service = native?.push && !native.host ? native.push : null;
  const { machine, machines, switchMachine } = useMachines();
  const { settings } = useSettings();
  const actions = useActions();
  const connection = useAppState((s) => s.connection);
  const sessions = useAppState((s) => s.sessions);
  const [target, setTarget] = useState<(PushOpenedTarget & { at: number }) | null>(null);

  useEffect(() => {
    if (!service) return;
    let active = true;
    const take = () => {
      service
        .takeOpened()
        .then((t) => {
          if (active && t) setTarget({ ...t, at: Date.now() });
        })
        .catch(() => {});
    };
    take();
    const onVisible = () => {
      if (document.visibilityState === 'visible') take();
    };
    document.addEventListener('visibilitychange', onVisible);
    window.addEventListener('focus', onVisible);
    return () => {
      active = false;
      document.removeEventListener('visibilitychange', onVisible);
      window.removeEventListener('focus', onVisible);
    };
  }, [service]);

  // Re-register once per launch and machine, once connected.
  const registered = useRef(new Set<string>());
  useEffect(() => {
    if (!service || connection !== 'connected') return;
    if (registered.current.has(machine.id) || !phoneAlertsWanted(machine.id)) return;
    registered.current.add(machine.id);
    service.phoneEnable(machine.id, settings.language).catch(() => {});
  }, [service, connection, machine.id, settings.language]);

  // Go to the notification's machine, then to its session once it is known.
  useEffect(() => {
    if (!target) return;
    let retry: ReturnType<typeof setTimeout> | undefined;
    const step = () => {
      if (Date.now() - target.at > OPEN_WAIT_MS) {
        setTarget(null);
        return;
      }
      if (
        target.machineId &&
        target.machineId !== machine.id &&
        machines.some((m) => m.id === target.machineId)
      ) {
        switchMachine(target.machineId);
        return;
      }
      const id =
        target.sessionId ??
        Object.values(sessions)
          .filter((s) => s.pending_decision)
          .sort((a, b) => (b.pending_decision?.at ?? 0) - (a.pending_decision?.at ?? 0))[0]?.id ??
        null;
      if (id && sessions[id]) {
        actions.openAlert({ sessionId: id });
        setTarget(null);
        return;
      }
      // Not loaded yet: look again shortly (a new session list also re-runs this).
      retry = setTimeout(() => setTarget((t) => (t ? { ...t } : t)), 1000);
    };
    const now = setTimeout(step, 0);
    return () => {
      clearTimeout(now);
      clearTimeout(retry);
    };
  }, [target, machine.id, machines, switchMachine, sessions, actions]);

  return null;
}

/** The settings section: desktop or phone variant, native only. */
export function PushAlertsSettings() {
  const native = useNative();
  if (!native?.push) return null;
  return native.host ? (
    <DesktopPushAlerts service={native.push} />
  ) : (
    <PhonePushAlerts service={native.push} />
  );
}
