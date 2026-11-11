import { useEffect, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { useNative } from '../native/context';
import { listenForHostChanges } from '../native/hostEvents';
import {
  describeSecurityError,
  type SecurityServices,
  type SecurityStatus,
  type VerifierKind,
} from '../native/security';
import { useSecurityStatus } from '../native/useSecurityStatus';
import { Icon } from './Icon';
import { Modal } from './Modal';
import { MailSettings, SecurityStatusList } from './SecuritySettings';
import { TotpSetupPanel } from './SecurityTotpSetup';
import { EnsureStepUp, useStepUp } from './StepUpPrompt';
import { openSettingsAt } from './settingsNav';
import styles from './SecurityWizard.module.css';

const STEPS = ['verify', 'totp', 'mail', 'summary'] as const;
type Step = (typeof STEPS)[number];

const STEP_TITLES: Record<Step, MessageKey> = {
  verify: 'security.wizard.verifyTitle',
  totp: 'security.wizard.totpTitle',
  mail: 'security.wizard.mailTitle',
  summary: 'security.wizard.summaryTitle',
};

const OS_LABEL: Record<Exclude<VerifierKind, 'none'>, MessageKey> = {
  windows_hello: 'security.stepUp.windowsHello',
  mac_local_auth: 'security.stepUp.macLocalAuth',
};

/**
 * First-run security setup of this PC's server, four short steps: local
 * verification, two-step verification with backup codes, mail (optional)
 * and a summary. "Later" records `skipped`; finishing records `done`.
 */
export function SecurityWizard({
  services,
  onClose,
}: {
  services: SecurityServices;
  /** The wizard was finished or skipped (with the server's new status). */
  onClose: (status: SecurityStatus | null) => void;
}) {
  return (
    <EnsureStepUp services={services}>
      <Wizard services={services} onClose={onClose} />
    </EnsureStepUp>
  );
}

function Wizard({
  services,
  onClose,
}: {
  services: SecurityServices;
  onClose: (status: SecurityStatus | null) => void;
}) {
  const { t } = useI18n();
  const stepUp = useStepUp();
  const { status, refresh, set } = useSecurityStatus(services);
  const [step, setStep] = useState<Step>('verify');
  const [checked, setChecked] = useState(false);
  const [settingUp, setSettingUp] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const index = STEPS.indexOf(step);
  const go = (next: Step) => {
    setError(null);
    setStep(next);
  };

  const close = async (state: 'done' | 'skipped') => {
    setBusy(true);
    setError(null);
    try {
      const next = await services.wizard(state);
      set(next);
      onClose(next);
    } catch (err) {
      setError(describeSecurityError(t, err));
      setBusy(false);
    }
  };

  const check = async () => {
    setBusy(true);
    setError(null);
    try {
      if (await stepUp.request('test')) setChecked(true);
    } finally {
      setBusy(false);
    }
  };

  const kind = status?.verifier.kind ?? 'none';
  const osOk = kind !== 'none' && !!status?.verifier.available;

  let body;
  let next: { label: MessageKey; enabled: boolean; onClick: () => void } | null = null;
  if (!status) {
    body = (
      <p className="muted" role="status">
        {t('security.loading')}
      </p>
    );
  } else if (step === 'verify') {
    body = (
      <>
        <p>{t('security.wizard.verifyIntro')}</p>
        {osOk ? (
          <>
            <button
              type="button"
              className={`btn ${checked ? '' : 'btn-primary'} ${styles.big}`}
              disabled={busy || checked}
              onClick={() => void check()}
            >
              {checked && <Icon name="check" size={18} />}
              {t(OS_LABEL[kind as Exclude<VerifierKind, 'none'>])}
            </button>
            {checked && (
              <p role="status" className={styles.success}>
                {t('security.wizard.verifyOk')}
              </p>
            )}
          </>
        ) : (
          <p className={styles.note}>
            {kind === 'none'
              ? t('security.wizard.verifyNone')
              : t('security.wizard.verifyUnavailable', {
                  name: t(`security.verifier.${kind}`),
                })}
          </p>
        )}
      </>
    );
    next = { label: 'security.next', enabled: !osOk || checked, onClick: () => go('totp') };
  } else if (step === 'totp') {
    body =
      status.totp.enrolled && !settingUp ? (
        <p role="status" className={styles.success}>
          {t('security.wizard.totpAlready')}
        </p>
      ) : settingUp ? (
        <TotpSetupPanel
          services={services}
          replace={false}
          onDone={() => {
            setSettingUp(false);
            void refresh();
            go('mail');
          }}
          onCancel={() => setSettingUp(false)}
        />
      ) : (
        <>
          <p>{t('security.wizard.totpIntro')}</p>
          <button
            type="button"
            className={`btn btn-primary ${styles.big}`}
            onClick={() => setSettingUp(true)}
          >
            {t('security.totp.setUp')}
          </button>
        </>
      );
    next =
      status.totp.enrolled && !settingUp
        ? { label: 'security.next', enabled: true, onClick: () => go('mail') }
        : null;
  } else if (step === 'mail') {
    body = (
      <>
        <p>{t('security.wizard.mailIntro')}</p>
        <MailSettings services={services} />
      </>
    );
    next = {
      label: status.mail.configured ? 'security.next' : 'security.wizard.mailSkip',
      enabled: true,
      onClick: () => {
        void refresh();
        go('summary');
      },
    };
  } else {
    body = (
      <>
        <p>
          {status.setup_complete
            ? t('security.wizard.summaryDone')
            : t('security.wizard.summaryIncomplete')}
        </p>
        <SecurityStatusList status={status} />
        <p className={styles.note}>{t('security.wizard.summaryWhere')}</p>
      </>
    );
    next = { label: 'security.wizard.finish', enabled: !busy, onClick: () => void close('done') };
  }

  return (
    <Modal
      title={t('security.wizard.title')}
      wide
      onClose={() => void close('skipped')}
      footer={
        <div className={styles.footer}>
          {step !== 'summary' && (
            <button
              type="button"
              className="btn"
              disabled={busy}
              onClick={() => void close('skipped')}
            >
              {t('security.wizard.later')}
            </button>
          )}
          <span className={styles.spacer} />
          {index > 0 && (
            <button
              type="button"
              className="btn"
              disabled={busy}
              onClick={() => {
                setSettingUp(false);
                go(STEPS[index - 1]!);
              }}
            >
              {t('security.back')}
            </button>
          )}
          {next && (
            <button
              type="button"
              className="btn btn-primary"
              disabled={!next.enabled}
              onClick={next.onClick}
            >
              {t(next.label)}
            </button>
          )}
        </div>
      }
    >
      <div className={styles.wizard} data-testid="security-wizard">
        <ol className={styles.steps} aria-label={t('security.wizard.progress')}>
          {STEPS.map((s, i) => (
            <li
              key={s}
              className={i === index ? styles.current : i < index ? styles.past : ''}
              aria-current={i === index ? 'step' : undefined}
            >
              <span className={styles.num}>{i + 1}</span>
              <span className={styles.stepLabel}>{t(STEP_TITLES[s])}</span>
            </li>
          ))}
        </ol>
        <h3 className={styles.stepTitle}>
          {t('security.wizard.stepOf', { step: index + 1, total: STEPS.length })} ·{' '}
          {t(STEP_TITLES[step])}
        </h3>
        {body}
        {error && (
          <p role="alert" className={styles.error}>
            {error}
          </p>
        )}
      </div>
    </Modal>
  );
}

/** The gentle reminder was closed in this app launch. */
let bannerDismissed = false;

/** Tests: start as a fresh app launch. */
// eslint-disable-next-line react-refresh/only-export-components
export function resetSecurityBanner() {
  bannerDismissed = false;
}

/**
 * Mounted once by the app (desktop): shows the first-run wizard while this
 * PC's server runs and its security setup is pending, and a small,
 * dismissible reminder (once per launch) when it was skipped.
 */
export function SecurityGate({
  onOpenSettings,
  subscribe = listenForHostChanges,
}: {
  /** Shows the Settings view (already pointed at the Security tab). */
  onOpenSettings: () => void;
  /** Calls back when the local server comes up or goes away. */
  subscribe?: (onChange: () => void) => () => void;
}) {
  const native = useNative();
  const services = native?.security;
  if (!services) return null;
  return (
    <EnsureStepUp services={services}>
      <Gate services={services} onOpenSettings={onOpenSettings} subscribe={subscribe} />
    </EnsureStepUp>
  );
}

function Gate({
  services,
  onOpenSettings,
  subscribe,
}: {
  services: SecurityServices;
  onOpenSettings: () => void;
  subscribe: (onChange: () => void) => () => void;
}) {
  const { t } = useI18n();
  const { status, refresh } = useSecurityStatus(services);
  // The wizard was closed in this launch (it does not come back until restart).
  const [closed, setClosed] = useState(false);
  const [hidden, setHidden] = useState(bannerDismissed);

  useEffect(() => subscribe(() => void refresh()), [subscribe, refresh]);

  if (!status || status.setup_complete || closed) return null;

  if (status.wizard.state === 'pending')
    return <SecurityWizard services={services} onClose={() => setClosed(true)} />;

  if (status.wizard.state !== 'skipped' || hidden) return null;

  const dismiss = () => {
    bannerDismissed = true;
    setHidden(true);
  };

  return (
    <div className={styles.banner} role="status" data-testid="security-banner">
      <Icon name="shield" size={18} />
      <span className={styles.bannerText}>{t('security.banner.text')}</span>
      <button
        type="button"
        className="btn btn-sm btn-primary"
        onClick={() => {
          dismiss();
          openSettingsAt('set-sec-status');
          onOpenSettings();
        }}
      >
        {t('security.banner.open')}
      </button>
      <button
        type="button"
        className="icon-btn"
        aria-label={t('security.banner.dismiss')}
        onClick={dismiss}
      >
        <Icon name="close" size={16} />
      </button>
    </div>
  );
}
