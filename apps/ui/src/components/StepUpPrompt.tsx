import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { toNativeError } from '../native/bridge';
import { useNative } from '../native/context';
import {
  describeSecurityError,
  withStepUp,
  type GatedAction,
  type SecurityServices,
  type VerifierKind,
} from '../native/security';
import { refreshSecurityStatus, useSecurityStatus } from '../native/useSecurityStatus';
import { Icon } from './Icon';
import { Modal } from './Modal';
import { requestSettingsView } from './settingsNav';
import styles from './StepUpPrompt.module.css';

/** The OS button label per verifier (`none` uses the authenticator code). */
const OS_LABEL: Record<Exclude<VerifierKind, 'none'>, MessageKey> = {
  windows_hello: 'security.stepUp.windowsHello',
  mac_local_auth: 'security.stepUp.macLocalAuth',
};

/**
 * Local verification before a gated action ("step-up"): Windows Hello or
 * Touch ID where the PC has them, the authenticator app's code otherwise
 * (or when the shell asks for it with `totp_required`). Calls `onDone(true)`
 * once verified, `onDone(false)` when closed.
 */
export function StepUpPrompt({
  services,
  action,
  reason,
  onDone,
}: {
  services: SecurityServices;
  /** `test` tries the verification and approves nothing. */
  action: GatedAction | 'test';
  /** Short text in the app language, shown here. */
  reason: string;
  onDone: (verified: boolean) => void;
}) {
  const { t, locale } = useI18n();
  const { status, loaded } = useSecurityStatus(services);
  const [forceCode, setForceCode] = useState(false);
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ text: string; error: boolean } | null>(null);

  const kind = status?.verifier.kind ?? 'none';
  const osAvailable = kind !== 'none' && !!status?.verifier.available;
  // Without a status (it failed to load) the code is the only way left.
  const codeMode = forceCode || (loaded && !osAvailable);
  // No OS verifier and no authenticator yet: a code could never be right.
  const noMethod = !forceCode && !osAvailable && status?.totp.enrolled === false;
  // Nothing can verify a first setup there: the person says it is them.
  const firstRun = noMethod && action === 'totp_setup';
  // A lost authenticator on a PC with no OS verifier: a backup code verifies.
  const backupAllowed = action === 'recovery';

  const verify = async (totp?: string, confirmed?: boolean) => {
    if (busy) return;
    setBusy(true);
    setMessage(null);
    try {
      if (action === 'test') {
        await services.stepUpTest({ language: locale, ...(totp === undefined ? {} : { totp }) });
      } else {
        await services.stepUp({
          reason,
          action,
          language: locale,
          ...(totp === undefined ? {} : { totp }),
          ...(confirmed ? { confirmed } : {}),
        });
      }
      void refreshSecurityStatus(services);
      onDone(true);
    } catch (err) {
      const e = toNativeError(err);
      if (e.code === 'totp_required') {
        setForceCode(true);
        setMessage({ text: t('security.stepUp.codeNeeded'), error: false });
      } else {
        // The prompt stays open so the person can try again.
        setMessage({ text: describeSecurityError(t, e), error: true });
        if (e.code === 'totp_invalid') setCode('');
      }
    } finally {
      setBusy(false);
    }
  };

  const submitCode = (e: FormEvent) => {
    e.preventDefault();
    const value = code.replace(/\s+/g, '');
    if (value) void verify(value);
  };

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
      <div className={styles.box} data-testid="step-up">
        <p className={styles.reason}>
          <Icon name="shield" size={18} />
          <span>{reason}</span>
        </p>
        <p className="muted">{t('security.stepUp.why')}</p>
        {!loaded ? (
          <p className="muted" role="status">
            {t('security.loading')}
          </p>
        ) : firstRun ? (
          <div className={styles.codeForm}>
            <p className={styles.note}>{t('security.stepUp.firstRun')}</p>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy}
              onClick={() => void verify(undefined, true)}
            >
              {t('security.stepUp.firstRunConfirm')}
            </button>
          </div>
        ) : noMethod ? (
          <div className={styles.codeForm}>
            <p role="alert" className={styles.error}>
              {t('security.error.verification_unavailable')}
            </p>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => {
                // Closing first: the action stays undone (canceled), and the
                // authenticator is set up in Settings, with its own prompt.
                onDone(false);
                requestSettingsView('set-sec-status');
              }}
            >
              <Icon name="settings" size={16} />
              {t('security.stepUp.openSecurity')}
            </button>
          </div>
        ) : codeMode ? (
          <form className={styles.codeForm} onSubmit={submitCode} noValidate>
            {kind === 'none' && !forceCode && (
              <p className={styles.note}>{t('security.stepUp.noOsVerifier')}</p>
            )}
            <div className="field">
              <label htmlFor="step-up-code">
                {backupAllowed ? t('security.stepUp.codeOrBackup') : t('security.codeLabel')}
              </label>
              <input
                id="step-up-code"
                type="text"
                inputMode={backupAllowed ? 'text' : 'numeric'}
                autoComplete="one-time-code"
                maxLength={backupAllowed ? 32 : 8}
                value={code}
                onChange={(e) => setCode(e.target.value)}
              />
            </div>
            <button type="submit" className="btn btn-primary" disabled={busy || !code.trim()}>
              {t('security.stepUp.confirm')}
            </button>
          </form>
        ) : (
          <button
            type="button"
            className={`btn btn-primary ${styles.osButton}`}
            disabled={busy}
            onClick={() => void verify()}
          >
            {t(OS_LABEL[kind as Exclude<VerifierKind, 'none'>])}
          </button>
        )}
        {message && (
          <p
            role={message.error ? 'alert' : 'status'}
            className={message.error ? styles.error : ''}
          >
            {message.text}
          </p>
        )}
      </div>
    </Modal>
  );
}

/** Asks for local verification and runs gated calls. */
export interface StepUpApi {
  /** Shows the prompt; resolves `true` once verified, `false` when closed. */
  request(action: GatedAction | 'test', reason?: string): Promise<boolean>;
  /**
   * Runs a gated call; on `step_up_required` it asks for verification and
   * retries once (rejects with `verification_canceled` when declined).
   */
  run<T>(action: GatedAction, call: () => Promise<T>, reason?: string): Promise<T>;
}

const StepUpContext = createContext<StepUpApi | null>(null);

/** Without a provider: no prompt, gated calls fail as they are. */
const NO_STEP_UP: StepUpApi = {
  request: async () => false,
  run: (_action, call) => call(),
};

/** The step-up prompt for everything below it (one prompt at a time). */
export function StepUpProvider({
  services,
  children,
}: {
  /** Defaults to the native security services (desktop shell only). */
  services?: SecurityServices | null;
  children: ReactNode;
}) {
  const { t } = useI18n();
  const native = useNative();
  const resolved = services === undefined ? (native?.security ?? null) : services;
  const [pending, setPending] = useState<{
    action: GatedAction | 'test';
    reason: string;
  } | null>(null);
  const waiting = useRef<{ promise: Promise<boolean>; resolve: (ok: boolean) => void } | null>(
    null,
  );

  const request = useCallback(
    (action: GatedAction | 'test', reason?: string): Promise<boolean> => {
      if (!resolved) return Promise.resolve(false);
      // A second request while one is open waits for the same answer.
      if (waiting.current) return waiting.current.promise;
      let resolve!: (ok: boolean) => void;
      const promise = new Promise<boolean>((r) => {
        resolve = r;
      });
      waiting.current = { promise, resolve };
      setPending({ action, reason: reason ?? t(`security.reason.${action}`) });
      return promise;
    },
    [resolved, t],
  );

  const api = useMemo<StepUpApi>(
    () =>
      resolved
        ? {
            request,
            run: (action, call, reason) => withStepUp(call, () => request(action, reason)),
          }
        : NO_STEP_UP,
    [resolved, request],
  );

  const done = (ok: boolean) => {
    const w = waiting.current;
    waiting.current = null;
    setPending(null);
    w?.resolve(ok);
  };

  return (
    <StepUpContext.Provider value={resolved ? api : null}>
      {children}
      {pending && resolved && (
        <StepUpPrompt
          services={resolved}
          action={pending.action}
          reason={pending.reason}
          onDone={done}
        />
      )}
    </StepUpContext.Provider>
  );
}

/** Uses the nearest step-up provider, or adds one for `services`. */
export function EnsureStepUp({
  services,
  children,
}: {
  services: SecurityServices;
  children: ReactNode;
}) {
  const outer = useContext(StepUpContext);
  if (outer) return <>{children}</>;
  return <StepUpProvider services={services}>{children}</StepUpProvider>;
}

/**
 * The step-up API: `run(action, () => gatedCall())`. Outside a
 * `StepUpProvider` gated calls run without a prompt.
 */
// eslint-disable-next-line react-refresh/only-export-components
export function useStepUp(): StepUpApi {
  return useContext(StepUpContext) ?? NO_STEP_UP;
}
