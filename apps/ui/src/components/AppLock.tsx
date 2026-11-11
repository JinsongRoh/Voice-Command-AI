/**
 * App lock: the app opens locked and locks again after a while without use,
 * so a lost or stolen device cannot drive the AI sessions. It unlocks with
 * the device's own sign-in (see `native/appLock.ts`), never a new password.
 *
 * While locked nothing below the provider is rendered: no session content,
 * no composer, no decision sheets, no hands-free / Voco (their components
 * are unmounted, which stops their microphone). With hands-free and the wake
 * word on, the lock screen keeps one listener whose only answer to the wake
 * word is "locked, please unlock" (`speech/useLockedWake.tsx`). The store keeps running, so
 * alerts still arrive as OS notifications (generic text only) and spoken
 * alerts stay silent (`isAppLocked`).
 *
 * Entering the Command Center always needs an unlock no older than
 * `COMMAND_CENTER_FRESH_MS`, even with the app lock off.
 */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type FormEvent,
  type ReactNode,
} from 'react';

import { useI18n } from '../i18n/context';
import type { AppLockVerifier } from '../native/appLock';
import { toNativeError } from '../native/bridge';
import { useNative } from '../native/context';
import { describeSecurityError, STEP_UP_CANCELED } from '../native/security';
import { diag } from '../diag/log';
import { isDictationActive } from '../speech/handsFree';
import { LockedWakeListener } from '../speech/useLockedWake';
import {
  APP_LOCK_IDLE_CHOICES,
  COMMAND_CENTER_FRESH_MS,
  IDLE_CHECK_MS,
  idleExpired,
  setAppLocked,
  unlockFresh,
  type AppLockIdleMinutes,
} from '../state/appLock';
import { lockSession, type LockSnapshot, type VerifyPurpose } from '../state/lockSession';
import { useSettings } from '../state/store';
import { BrandLogo } from './BrandLogo';
import { Icon } from './Icon';
import { Modal } from './Modal';
import { SecurityRecovery } from './SecuritySettings';
import styles from './AppLock.module.css';

export interface AppLockApi {
  /** A verifier exists (the native shells, or the simulated browser lock). */
  available: boolean;
  locked: boolean;
  /** When the person last proved it is them (Unix ms); `null`: not yet. */
  lastUnlockAt: number | null;
  /** Shows the verification dialog; resolves `true` once verified. */
  verify(reason: string): Promise<boolean>;
  /** Locks the app now (Settings: "Lock now"). */
  lock(): void;
  /** Records a verification made elsewhere (the Command Center gate). */
  noteUnlocked(): void;
  verifier: AppLockVerifier | null;
}

/** Without a provider (or a verifier): no lock at all. */
const NO_LOCK: AppLockApi = {
  available: false,
  locked: false,
  lastUnlockAt: null,
  verify: async () => true,
  lock: () => {},
  noteUnlocked: () => {},
  verifier: null,
};

const AppLockContext = createContext<AppLockApi>(NO_LOCK);

// eslint-disable-next-line react-refresh/only-export-components
export function useAppLock(): AppLockApi {
  return useContext(AppLockContext);
}

const NO_SNAPSHOT: LockSnapshot = { locked: false, lastUnlockAt: null };
const noSnapshot = () => NO_SNAPSHOT;
const noSubscribe = () => () => {};

/** What counts as using the app (resets the idle time). */
const ACTIVITY_EVENTS = ['pointerdown', 'pointermove', 'keydown', 'wheel', 'touchstart'] as const;
/** What tells the lock screen that the person is back (and the window in front). */
const RETURN_EVENTS = [...ACTIVITY_EVENTS, 'focus'] as const;

export function AppLockProvider({
  verifier,
  children,
}: {
  /** `null`: no lock (browser build without `?applock=1`, tests). */
  verifier: AppLockVerifier | null;
  children: ReactNode;
}) {
  const { settings } = useSettings();
  const enabled = !!verifier && settings.appLock;
  // The state lives in the run's lock session, not here: this provider may
  // mount more than once while the app starts (see `state/lockSession.ts`).
  const session = useMemo(() => (verifier ? lockSession(verifier) : null), [verifier]);
  // Locked at launch: decided once per run, from the setting as the app opened.
  session?.decide(enabled);
  const snapshot = useSyncExternalStore(
    session?.subscribe ?? noSubscribe,
    session?.getSnapshot ?? noSnapshot,
  );
  const locked = snapshot.locked ?? enabled;
  const lastUnlockAt = snapshot.lastUnlockAt;
  const setLocked = useCallback(
    (next: boolean, awaitReturn = false) => session?.setLocked(next, { awaitReturn }),
    [session],
  );
  const [pending, setPending] = useState<{
    reason: string;
    resolve: (ok: boolean) => void;
  } | null>(null);
  const pendingRef = useRef<typeof pending>(null);

  const noteUnlocked = useCallback(() => session?.noteUnlocked(), [session]);

  // Code outside React (spoken alerts) follows the lock.
  useEffect(() => {
    setAppLocked(locked);
    // A verification still open when the app locks is declined.
    if (locked && pendingRef.current) {
      const open = pendingRef.current;
      pendingRef.current = null;
      setPending(null);
      open.resolve(false);
    }
  }, [locked]);
  useEffect(
    () => () => {
      setAppLocked(false);
      // Unmounted with a verification open: declined, never left hanging.
      const open = pendingRef.current;
      pendingRef.current = null;
      open?.resolve(false);
    },
    [],
  );

  // Locks again after the idle time, also when the app comes back from the
  // background after longer than that (timers may not run there).
  const idleMs = settings.appLockIdleMin * 60_000;
  useEffect(() => {
    if (!enabled || locked || !session) return;
    session.touch();
    const touch = () => session.touch();
    // `back`: the app just came back to the front, so the person is here and
    // the lock screen may ask at once. The timer locks while nobody is
    // there: the lock screen then waits for them before it asks.
    const check = (back = false) => {
      // Speaking to the app counts as using it: the composer's dictation
      // (started with a tap) or a Voco message after the wake word, both
      // from this device's microphone. Spoken alerts and messages arriving
      // from the PC never do.
      if (isDictationActive()) {
        session.touch();
        return;
      }
      if (idleExpired(session.lastActivity, Date.now(), idleMs)) {
        diag('lock.idle', {
          idleMin: idleMs / 60_000,
          back,
          idleMs: Date.now() - session.lastActivity,
          visibility: document.visibilityState,
        });
        setLocked(true, !back);
      }
    };
    const onVisibility = () => {
      if (document.visibilityState === 'visible') check(true);
    };
    for (const e of ACTIVITY_EVENTS) window.addEventListener(e, touch, { passive: true });
    document.addEventListener('visibilitychange', onVisibility);
    const timer = setInterval(() => check(), IDLE_CHECK_MS);
    return () => {
      for (const e of ACTIVITY_EVENTS) window.removeEventListener(e, touch);
      document.removeEventListener('visibilitychange', onVisibility);
      clearInterval(timer);
    };
  }, [enabled, locked, idleMs, session, setLocked]);

  const verify = useCallback(
    (reason: string): Promise<boolean> => {
      if (!verifier) return Promise.resolve(true);
      // One dialog at a time: a second request is declined.
      if (pendingRef.current) return Promise.resolve(false);
      return new Promise<boolean>((resolve) => {
        const open = { reason, resolve };
        pendingRef.current = open;
        setPending(open);
      });
    },
    [verifier],
  );

  const lock = useCallback(() => {
    if (!verifier) return;
    diag('lock.manual');
    setLocked(true);
  }, [verifier, setLocked]);

  const api = useMemo<AppLockApi>(
    () =>
      verifier
        ? { available: true, locked, lastUnlockAt, verify, lock, noteUnlocked, verifier }
        : NO_LOCK,
    [verifier, locked, lastUnlockAt, verify, lock, noteUnlocked],
  );

  const finish = (ok: boolean) => {
    if (ok) noteUnlocked();
    const open = pendingRef.current;
    pendingRef.current = null;
    setPending(null);
    open?.resolve(ok);
  };

  if (verifier && locked) {
    return (
      <AppLockContext.Provider value={api}>
        <LockScreen
          verifier={verifier}
          onUnlocked={() => {
            noteUnlocked();
            setLocked(false);
          }}
        />
        <LockedWakeListener />
      </AppLockContext.Provider>
    );
  }
  return (
    <AppLockContext.Provider value={api}>
      {children}
      {pending && verifier && (
        <VerifyDialog verifier={verifier} reason={pending.reason} onDone={finish} />
      )}
    </AppLockContext.Provider>
  );
}

/** The full-screen lock: the app name and an Unlock button, nothing else. */
function LockScreen({
  verifier,
  onUnlocked,
}: {
  verifier: AppLockVerifier;
  onUnlocked: () => void;
}) {
  const { t } = useI18n();
  return (
    <div className={styles.screen} data-testid="app-lock">
      <main className={styles.card} aria-labelledby="app-lock-title">
        <BrandLogo variant="symbol" className={styles.logo} />
        <h1 id="app-lock-title" className={styles.title}>
          {t('app.title')}
        </h1>
        <p className={styles.state}>
          <Icon name="shield" size={18} />
          <span>{t('appLock.locked')}</span>
        </p>
        <p className="muted">{t('appLock.lockedHelp')}</p>
        <UnlockPanel
          verifier={verifier}
          reason={t('appLock.reason')}
          purpose="unlock"
          auto={verifier.autoPrompt !== false}
          onUnlocked={onUnlocked}
        />
      </main>
    </div>
  );
}

/** Verification for one action (turning the lock off), over the app. */
function VerifyDialog({
  verifier,
  reason,
  onDone,
}: {
  verifier: AppLockVerifier;
  reason: string;
  onDone: (ok: boolean) => void;
}) {
  const { t } = useI18n();
  return (
    <Modal
      title={t('security.stepUp.title')}
      onClose={() => onDone(false)}
      dismissOnBackdrop={false}
      footer={
        <button type="button" className="btn" onClick={() => onDone(false)}>
          {t('common.cancel')}
        </button>
      }
    >
      <div className={styles.dialog} data-testid="app-lock-verify">
        <p className={styles.reason}>
          <Icon name="shield" size={18} />
          <span>{reason}</span>
        </p>
        <UnlockPanel verifier={verifier} reason={reason} auto onUnlocked={() => onDone(true)} />
      </div>
    </Modal>
  );
}

/**
 * Runs the verification (once by itself with `auto`) and offers the Unlock
 * button, the authenticator code where the PC has no OS verifier, starting
 * this PC's server, or opening without verification where nothing can check.
 */
export function UnlockPanel({
  verifier,
  reason,
  purpose = 'verify',
  auto = false,
  onUnlocked,
}: {
  verifier: AppLockVerifier;
  /** Shown in the OS prompt where the platform shows one. */
  reason: string;
  /** `unlock`: the lock screen (a success opens the app even if this panel is gone). */
  purpose?: VerifyPurpose;
  auto?: boolean;
  onUnlocked: () => void;
}) {
  const { t, locale } = useI18n();
  const session = lockSession(verifier);
  const [busy, setBusy] = useState(false);
  const [needCode, setNeedCode] = useState(false);
  const [code, setCode] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [unprotected, setUnprotected] = useState(false);
  const [serverDown, setServerDown] = useState(false);
  /** The lock screen: "Lost your authenticator app?" is open. */
  const [recovering, setRecovering] = useState(false);
  const native = useNative();
  // Locked, the app (and Settings › Security with recovery) is gone: the lock
  // screen offers the same recovery, checked by the server as in Settings.
  const recovery = purpose === 'unlock' && needCode ? native?.security : undefined;
  const started = useRef(false);
  const alive = useRef(true);
  const unlockButton = useRef<HTMLButtonElement>(null);
  // No prompt by itself (Windows): the Unlock button takes the focus, so
  // Enter opens the sign-in once the window is where the person wants it.
  useEffect(() => {
    if (!auto && purpose === 'unlock') unlockButton.current?.focus();
  }, [auto, purpose]);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const run = useCallback(
    async (totp?: string, byItself = false) => {
      setBusy(true);
      setError(null);
      try {
        // Through the run's lock session: one prompt at a time, shared by
        // every panel (an open prompt is joined, not opened again).
        const result = await session.verify(
          { reason, language: locale, ...(totp === undefined ? {} : { totp }) },
          purpose,
        );
        if (!alive.current) return;
        if (result === 'verified') onUnlocked();
        else setUnprotected(true);
      } catch (err) {
        if (!alive.current) return;
        const e = toNativeError(err);
        // The lock screen's own try ran out while nobody could answer (the
        // window went to the back, the PC was locked): no error asking for a
        // second sign-in; it asks once more when the person is back.
        if (
          byItself &&
          purpose === 'unlock' &&
          e.code === STEP_UP_CANCELED &&
          !(verifier.promptable?.() ?? true)
        ) {
          session.rearmAutoUnlock();
          started.current = false;
          return;
        }
        if (e.code === 'totp_required') {
          setNeedCode(true);
        } else if (e.code === 'unreachable') {
          setServerDown(true);
        } else {
          setServerDown(false);
          setError(describeSecurityError(t, e));
          if (e.code === 'totp_invalid' || e.code === 'totp_replayed') setCode('');
        }
      } finally {
        if (alive.current) setBusy(false);
      }
    },
    [session, verifier, reason, purpose, locale, onUnlocked, t],
  );

  // Starts the verification by itself, once, when it can be answered: the
  // window is in front (Windows answers "canceled" to a prompt whose window
  // is not) and, after an idle lock, the person is back.
  useEffect(() => {
    if (!auto) return;
    const attempt = (returned: boolean) => {
      if (started.current || !alive.current) return;
      const unlock = purpose === 'unlock';
      // A prompt already open is followed whatever the window does.
      if (!session.verifying) {
        if (returned && unlock) session.noteReturn();
        if (unlock && session.awaitingReturn) return;
        if (!(verifier.promptable?.() ?? true)) return;
      }
      started.current = true;
      // Right after the commit (not inside the effect): a panel unmounted at
      // once does not use up the lock screen's own try.
      queueMicrotask(() => {
        if (!alive.current) return;
        // A prompt already open (this screen mounted again while it is up):
        // follow it. The lock screen tries by itself once per lock, not once
        // per mount; after that the Unlock button asks again.
        if (session.verifying || !unlock || session.claimAutoUnlock()) void run(undefined, true);
      });
    };
    const onReturn = () => attempt(true);
    const onVisibility = () => {
      if (document.visibilityState === 'visible') attempt(true);
    };
    attempt(false);
    for (const e of RETURN_EVENTS) window.addEventListener(e, onReturn, { passive: true });
    document.addEventListener('visibilitychange', onVisibility);
    return () => {
      for (const e of RETURN_EVENTS) window.removeEventListener(e, onReturn);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, [auto, run, session, purpose, verifier]);

  const startServer = async () => {
    if (!verifier.startServer) return;
    setBusy(true);
    setError(null);
    try {
      const ok = await verifier.startServer();
      if (!alive.current) return;
      if (!ok) {
        setError(t('appLock.cannotStart'));
        setBusy(false);
        return;
      }
      setServerDown(false);
    } catch (err) {
      if (!alive.current) return;
      setError(describeSecurityError(t, err));
      setBusy(false);
      return;
    }
    await run();
  };

  const submitCode = (e: FormEvent) => {
    e.preventDefault();
    const value = code.replace(/\s+/g, '');
    if (value) void run(value);
  };

  if (unprotected) {
    return (
      <div className={styles.panel}>
        <p role="alert" className={styles.warning}>
          {t('appLock.unprotected')}
        </p>
        <button type="button" className="btn" onClick={onUnlocked}>
          {t('appLock.continueUnprotected')}
        </button>
      </div>
    );
  }

  return (
    <div className={styles.panel}>
      {needCode ? (
        <form className={styles.codeForm} onSubmit={submitCode} noValidate>
          <p className={styles.note}>{t('appLock.codeHelp')}</p>
          <div className="field">
            <label htmlFor="app-lock-code">{t('security.codeLabel')}</label>
            <input
              id="app-lock-code"
              type="text"
              inputMode="numeric"
              autoComplete="one-time-code"
              maxLength={8}
              value={code}
              onChange={(e) => setCode(e.target.value)}
            />
          </div>
          <button type="submit" className="btn btn-primary" disabled={busy || !code.trim()}>
            {t('appLock.unlock')}
          </button>
          {recovery && (
            <button
              type="button"
              className="btn btn-sm"
              aria-expanded={recovering}
              onClick={() => setRecovering((v) => !v)}
            >
              {t('appLock.lostAuthenticator')}
            </button>
          )}
        </form>
      ) : (
        <button
          ref={unlockButton}
          type="button"
          className={`btn btn-primary ${styles.unlock}`}
          disabled={busy}
          onClick={() => void run()}
        >
          <Icon name="shield" size={18} />
          {t('appLock.unlock')}
        </button>
      )}
      {busy && (
        <p className="muted" role="status">
          {t('appLock.verifying')}
        </p>
      )}
      {serverDown && !busy && (
        <div className={styles.serverDown}>
          <p role="alert" className={styles.error}>
            {t('appLock.serverDown')}
          </p>
          {verifier.startServer && (
            <button type="button" className="btn" onClick={() => void startServer()}>
              {t('appLock.startServer')}
            </button>
          )}
        </div>
      )}
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      {recovery && recovering && <SecurityRecovery services={recovery} />}
    </div>
  );
}

/**
 * The Command Center: shown when the last unlock is at most 5 minutes old,
 * otherwise after a new verification. Checked on entry (each mount), so a
 * long stay inside is not interrupted.
 */
export function CommandCenterGate({
  open = false,
  children,
}: {
  /**
   * Nothing to guard yet (the conductor is off: only that notice shows).
   * Once it turns on while shown, a stale unlock asks again.
   */
  open?: boolean;
  children: ReactNode;
}) {
  const { t } = useI18n();
  const lock = useAppLock();
  const [granted, setGranted] = useState(
    () => !lock.verifier || unlockFresh(lock.lastUnlockAt, Date.now(), COMMAND_CENTER_FRESH_MS),
  );
  if (open || granted || !lock.verifier) return <>{children}</>;
  return (
    <div className={styles.gate} data-testid="command-center-lock">
      <div className={styles.card}>
        <h2 className={styles.gateTitle}>
          <Icon name="shield" size={20} />
          {t('appLock.commandTitle')}
        </h2>
        <p className="muted">{t('appLock.commandHelp')}</p>
        <UnlockPanel
          verifier={lock.verifier}
          reason={t('appLock.commandReason')}
          auto={lock.verifier.autoPrompt !== false}
          onUnlocked={() => {
            lock.noteUnlocked();
            setGranted(true);
          }}
        />
      </div>
    </div>
  );
}

/** Settings: the app lock switch (off needs a verification) and the idle time. */
export function AppLockSettings() {
  const { t } = useI18n();
  const { settings, updateSettings } = useSettings();
  const lock = useAppLock();
  const [busy, setBusy] = useState(false);

  const toggle = async (on: boolean) => {
    if (on) {
      updateSettings({ appLock: true });
      return;
    }
    setBusy(true);
    try {
      if (await lock.verify(t('appLock.settings.offReason'))) updateSettings({ appLock: false });
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <p className="muted">{t('appLock.settings.help')}</p>
      <label className="check">
        <input
          type="checkbox"
          checked={settings.appLock}
          disabled={busy}
          onChange={(e) => void toggle(e.target.checked)}
        />
        {t('appLock.settings.toggle')}
      </label>
      <div className="field">
        <label htmlFor="set-app-lock-idle">{t('appLock.settings.idle')}</label>
        <select
          id="set-app-lock-idle"
          value={settings.appLockIdleMin}
          disabled={!settings.appLock}
          onChange={(e) =>
            updateSettings({ appLockIdleMin: Number(e.target.value) as AppLockIdleMinutes })
          }
        >
          {APP_LOCK_IDLE_CHOICES.map((m) => (
            <option key={m} value={m}>
              {t('appLock.settings.minutes', { count: m })}
            </option>
          ))}
        </select>
      </div>
      <p className="muted">{t('appLock.settings.commandNote')}</p>
      {settings.appLock && (
        <button type="button" className="btn" onClick={lock.lock}>
          <Icon name="shield" size={16} />
          {t('appLock.settings.lockNow')}
        </button>
      )}
    </>
  );
}
