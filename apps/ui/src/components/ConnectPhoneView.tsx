import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey, TranslateFn } from '../i18n/i18n';
import { toNativeError, type DeviceScope, type DnsPilotService } from '../native/bridge';
import { describeNativeError } from '../native/errors';
import { isStepUpCanceled } from '../native/security';
import {
  formatCountdown,
  isLoopbackListen,
  secondsLeft,
  svgDataUri,
  type HostPairing,
  type HostServices,
  type HostStatus,
  type RemoteInfo,
  type TotpEnrollment,
} from '../native/host';
import { TOTP_ISSUER } from '../protocol/shell';
import { ConfirmCode } from './ConfirmCode';
import { DnsPilotRelink } from './DnsPilotConnect';
import { Icon } from './Icon';
import { BackupCodeActions } from './SecurityTotpSetup';
import { openSettingsAt } from './settingsNav';
import { useStepUp } from './StepUpPrompt';
import styles from './ConnectPhoneView.module.css';

export const REACH_GUIDE_URL =
  'https://github.com/JinsongRoh/Voice-Command-AI/blob/main/docs/user-guide.md#5-reach-your-pc';

const SCOPES: DeviceScope[] = ['view', 'command', 'admin'];

const HOST_ERRORS = [
  'not_running',
  'remote_off',
  'totp_not_enrolled',
  'kill_switch',
  'sidecar_missing',
  'start_failed',
  'not_owned',
  'totp_invalid',
  'totp_enrolled',
] as const;

const INVITE_NOTES = ['not_connected', 'not_owned', 'rate_limited', 'invite_limit'] as const;
/** Reasons that reconnecting DNSPilot in this app can fix. */
const RELINK_REASONS = ['not_connected', 'not_owned'] as const;

/** Why this pairing code has no DNSPilot invite. */
function inviteNote(t: TranslateFn, reason: string): string {
  return (INVITE_NOTES as readonly string[]).includes(reason)
    ? t(`host.invite.${reason}` as MessageKey)
    : t('host.invite.failed');
}

/** Localized message for a rejected host command. */
function describeHostError(t: TranslateFn, err: unknown): string {
  const e = toNativeError(err);
  if (e.code && (HOST_ERRORS as readonly string[]).includes(e.code)) {
    return t(`host.error.${e.code}` as MessageKey, { error: e.message });
  }
  return describeNativeError(t, err);
}

/**
 * "Connect a phone" (desktop): runs the VCA server on this PC and shows a
 * one-time pairing QR code for the phone app.
 */
/**
 * A pairing link on the clipboard outlives its use: once it expired, it goes,
 * unless the person has copied something else since. Where the clipboard
 * cannot be read, it stays (expired, and so harmless).
 */
async function clearCopied(copied: { current: string | null }) {
  const link = copied.current;
  copied.current = null;
  if (!link) return;
  try {
    if ((await navigator.clipboard.readText()) === link) await navigator.clipboard.writeText('');
  } catch {
    // The browser said no; the link has expired anyway.
  }
}

export function ConnectPhoneView({
  host,
  dnspilot,
  onBack,
  clock = Date.now,
  pollMs = 4000,
}: {
  host: HostServices;
  /** This app's DNSPilot connection: fixes a code that could not carry an invite. */
  dnspilot?: DnsPilotService;
  onBack?: () => void;
  /** Current time in ms (tests). */
  clock?: () => number;
  /** Status refresh interval (detects a completed pairing). */
  pollMs?: number;
}) {
  const { t } = useI18n();
  const [status, setStatus] = useState<HostStatus | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [busy, setBusy] = useState<'start' | 'stop' | null>(null);
  const [scope, setScope] = useState<DeviceScope>('command');
  /**
   * The permission was changed while a QR code was shown: that code (made
   * for the old permission) is taken off the screen, and a new one waits for
   * a click, so changing the choice never opens a new verification prompt.
   */
  const [scopeChanged, setScopeChanged] = useState(false);
  const [pairing, setPairing] = useState<HostPairing | null>(null);
  const [pairBusy, setPairBusy] = useState(false);
  const [pairError, setPairError] = useState<string | null>(null);
  const [paired, setPaired] = useState(false);
  /** The last code ran out and was discarded; a new one needs a click. */
  const [codeExpired, setCodeExpired] = useState(false);
  /** DNSPilot was reconnected here: the next code can carry an invite. */
  const [relinked, setRelinked] = useState(false);
  const [now, setNow] = useState(() => clock());
  const [copy, setCopy] = useState<'idle' | 'copied' | 'failed'>('idle');
  /** The link this view put on the clipboard. */
  const copiedLink = useRef<string | null>(null);
  /** Paired devices when the current code was issued. */
  const baseline = useRef<number | null>(null);
  const stepUp = useStepUp();
  const linkRef = useRef<HTMLInputElement>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const applyStatus = useCallback((s: HostStatus) => {
    if (!mounted.current) return;
    setStatus(s);
    setStatusError(null);
    const count = s.remote?.paired_devices;
    if (baseline.current !== null && count !== undefined && count > baseline.current) {
      baseline.current = null;
      setPairing(null);
      setPaired(true);
    }
  }, []);

  const refresh = useCallback(async () => {
    try {
      applyStatus(await host.status());
    } catch (err) {
      if (mounted.current) setStatusError(describeHostError(t, err));
    }
  }, [host, applyStatus, t]);

  useEffect(() => {
    void refresh();
    const id = setInterval(() => void refresh(), pollMs);
    return () => clearInterval(id);
  }, [refresh, pollMs]);

  const createCode = useCallback(
    async (forScope: DeviceScope) => {
      setPairBusy(true);
      setPairError(null);
      setPaired(false);
      setCodeExpired(false);
      setScopeChanged(false);
      setRelinked(false);
      setCopy('idle');
      try {
        // The prompt asks first, so a code never appears unverified.
        const p = await stepUp.run('pairing_create', () => host.pair(forScope));
        if (!mounted.current) return;
        baseline.current = status?.remote?.paired_devices ?? null;
        setPairing(p);
        setNow(clock());
      } catch (err) {
        if (!mounted.current) return;
        setPairing(null);
        // Closing the prompt is a choice, not an error: the button stays.
        if (!isStepUpCanceled(err)) setPairError(describeHostError(t, err));
      } finally {
        if (mounted.current) setPairBusy(false);
      }
    },
    [host, status, clock, t, stepUp],
  );

  // No code appears on its own: creating one asks for verification and starts
  // its short clock, so it waits for "Create QR code" when the phone is ready.

  // Countdown. At zero the code is discarded; a new one only on a click.
  const left = pairing ? secondsLeft(pairing.expires_at, now) : 0;
  const expired = !!pairing && left === 0;
  useEffect(() => {
    if (!pairing) return;
    const id = setInterval(() => {
      const time = clock();
      setNow(time);
      if (secondsLeft(pairing.expires_at, time) === 0) {
        void clearCopied(copiedLink);
        setPairing(null);
        setCodeExpired(true);
        setCopy('idle');
      }
    }, 1000);
    return () => clearInterval(id);
  }, [pairing, clock]);

  const lifecycle = async (kind: 'start' | 'stop') => {
    setBusy(kind);
    setStatusError(null);
    try {
      applyStatus(await (kind === 'start' ? host.start() : host.stop()));
      if (kind === 'stop') setPairing(null);
    } catch (err) {
      if (mounted.current) setStatusError(describeHostError(t, err));
    } finally {
      if (mounted.current) setBusy(null);
    }
  };

  const setRunServer = async (enabled: boolean) => {
    try {
      await host.setRunServer(enabled);
      setStatus((s) => (s ? { ...s, run_server: enabled } : s));
    } catch (err) {
      setStatusError(describeHostError(t, err));
    }
  };

  const copyLink = async () => {
    if (!pairing) return;
    try {
      await navigator.clipboard.writeText(pairing.uri);
      copiedLink.current = pairing.uri;
      setCopy('copied');
    } catch {
      linkRef.current?.select();
      setCopy('failed');
    }
  };

  const remote = status?.remote ?? null;
  const running = !!status?.running;
  const needsTotp = !!remote?.enabled && remote.require_totp && !remote.totp_enrolled;
  // Before a code exists, the listen address tells whether phones can reach it.
  const loopback = pairing
    ? pairing.loopback_only
    : !!remote?.enabled && !remote.provider && isLoopbackListen(remote.listen);

  return (
    <div className={styles.screen}>
      <div className={styles.titleRow}>
        {onBack && (
          <button
            type="button"
            className="btn btn-ghost btn-sm"
            onClick={() => {
              // Back to Settings → Connection & security.
              openSettingsAt('set-connect-phone');
              onBack();
            }}
          >
            <Icon name="back" size={16} />
            {t('host.back')}
          </button>
        )}
        <h2 className={styles.title}>{t('host.title')}</h2>
      </div>
      <p className={styles.intro}>{t('host.intro')}</p>

      <section className={`panel ${styles.section}`} aria-labelledby="host-server">
        <h3 id="host-server">{t('host.server')}</h3>
        <div className={styles.serverRow}>
          <span
            className={`${styles.chip} ${running ? styles.chipOn : styles.chipOff}`}
            role="status"
          >
            <span className={styles.dot} aria-hidden="true" />
            {status === null
              ? t('host.checking')
              : running
                ? status.version
                  ? t('host.runningVersion', { version: status.version })
                  : t('host.running')
                : t('host.stopped')}
          </span>
          {status?.server_name && <strong>{status.server_name}</strong>}
          <span className={styles.spacer} />
          {status && !running && (
            <button
              type="button"
              className="btn btn-primary btn-sm"
              disabled={busy !== null || !status.can_start}
              onClick={() => void lifecycle('start')}
            >
              <Icon name="power" size={16} />
              {busy === 'start' ? t('host.starting') : t('host.start')}
            </button>
          )}
          {running && status?.managed && (
            <button
              type="button"
              className="btn btn-sm"
              disabled={busy !== null}
              onClick={() => void lifecycle('stop')}
            >
              {busy === 'stop' ? t('host.stopping') : t('host.stop')}
            </button>
          )}
        </div>
        {status && !running && !status.can_start && <p className="muted">{t('host.notBundled')}</p>}
        {running && status && !status.managed && <p className="muted">{t('host.external')}</p>}
        {statusError && (
          <p role="alert" className={styles.error}>
            {statusError}
          </p>
        )}
        {status && (
          <label className="check">
            <input
              type="checkbox"
              checked={status.run_server}
              onChange={(e) => void setRunServer(e.target.checked)}
            />
            {t('host.runServer')}
          </label>
        )}
        <p className={styles.help}>{t('host.runServerHelp')}</p>
      </section>

      {running && (
        <section className={`panel ${styles.section}`} aria-labelledby="host-reach">
          <h3 id="host-reach">{t('host.reach')}</h3>
          {remote && !remote.enabled ? (
            <p role="alert" className={styles.warning}>
              {t('host.remoteOff')}
            </p>
          ) : (
            remote && (
              <dl className={styles.dl}>
                <dt>{t('host.provider')}</dt>
                <dd>
                  {remote.provider
                    ? remote.provider_state
                      ? `${remote.provider} (${remote.provider_state})`
                      : remote.provider
                    : t('host.providerNone')}
                </dd>
                {remote.listen && (
                  <>
                    <dt>{t('host.listen')}</dt>
                    <dd className={styles.mono}>{remote.listen}</dd>
                  </>
                )}
                {pairing && pairing.endpoints.length > 0 && (
                  <>
                    <dt>{t('host.endpoints')}</dt>
                    <dd className={styles.mono}>
                      {pairing.endpoints.map((e) => (
                        <span key={e} className={styles.endpoint}>
                          {e}
                        </span>
                      ))}
                    </dd>
                  </>
                )}
              </dl>
            )
          )}
          {remote?.provider_diagnostics.map((d) => (
            <p key={d} className={styles.help}>
              {d}
            </p>
          ))}
          {remote?.kill_switch_tripped && (
            <p role="alert" className={styles.warning}>
              {t('host.killSwitch')}
            </p>
          )}
          {loopback && (
            <p role="alert" className={styles.warning}>
              {t('host.loopbackWarning')}
            </p>
          )}
          <a className={styles.guide} href={REACH_GUIDE_URL} target="_blank" rel="noreferrer">
            {t('host.guide')}
          </a>
        </section>
      )}

      {running && remote?.enabled && (
        <TotpSetup host={host} remote={remote} onChanged={() => void refresh()} />
      )}

      {running && remote?.enabled && !needsTotp && (
        <section className={`panel ${styles.section}`} aria-labelledby="host-code">
          <h3 id="host-code">{t('host.code')}</h3>
          <div className="field">
            <span className="label" id="host-scope">
              {t('host.scope')}
            </span>
            <div className="segmented" role="group" aria-labelledby="host-scope">
              {SCOPES.map((s) => (
                <button
                  key={s}
                  type="button"
                  aria-pressed={scope === s}
                  disabled={pairBusy}
                  onClick={() => {
                    if (s === scope) return;
                    setScope(s);
                    if (pairing) {
                      setPairing(null);
                      setScopeChanged(true);
                    }
                  }}
                >
                  {t(`host.scope.${s}`)}
                </button>
              ))}
            </div>
            <span className={styles.help}>{t(`host.scopeHelp.${scope}`)}</span>
          </div>

          <ol className={styles.steps} aria-label={t('pairing.guideTitle')}>
            <li>{t('pairing.guide1')}</li>
            <li>{t('pairing.guide2')}</li>
            <li>{t('pairing.guide3')}</li>
            <li>{t('pairing.guide4')}</li>
          </ol>
          <p className={styles.help}>
            {remote.require_totp ? t('host.totpPairOn') : t('host.totpPairOff')}
          </p>
          {!remote.require_totp && !remote.totp_enrolled && (
            <p role="status" className={styles.warning}>
              {t('host.totpRecommend')}
            </p>
          )}

          {codeExpired && !pairing && !scopeChanged && (
            <p role="status" className={styles.warning}>
              {t('host.expired')}
            </p>
          )}
          {scopeChanged && !pairing && (
            <p role="status" className={styles.warning}>
              {t('host.scopeChanged')}
            </p>
          )}
          {pairing && !expired && (
            <div className={styles.qrBlock}>
              <img
                className={styles.qr}
                src={svgDataUri(pairing.qr_svg)}
                alt={t('host.qrAlt')}
                width={280}
                height={280}
              />
              <p className={styles.countdown} aria-live="off">
                {t('host.expiresIn', { time: formatCountdown(left) })}
              </p>
              {pairing.confirm_code && (
                <p className={styles.help} data-testid="pair-confirm-code">
                  {t('host.confirmCode')} <ConfirmCode code={pairing.confirm_code} />
                  <br />
                  {t('host.confirmCodeHelp')}
                </p>
              )}
              {pairing.invite_included && (
                <p className={styles.inviteOk} role="status">
                  <Icon name="check" size={16} />
                  {t('host.inviteIncluded')}
                </p>
              )}
              {pairing.invite_skipped &&
                (relinked ? (
                  <p className={styles.inviteOk} role="status">
                    <Icon name="check" size={16} />
                    {t('host.invite.relinked')}
                  </p>
                ) : (
                  <InviteSkipped
                    reason={pairing.invite_skipped}
                    dnspilot={dnspilot}
                    onRelinked={() => setRelinked(true)}
                  />
                ))}
            </div>
          )}

          {paired && (
            <p role="status" className={styles.success}>
              {t('host.paired')}
            </p>
          )}
          {pairError && (
            <p role="alert" className={styles.error}>
              {pairError}
            </p>
          )}

          <div className={styles.actions}>
            <button
              type="button"
              className="btn btn-primary"
              disabled={pairBusy}
              onClick={() => void createCode(scope)}
            >
              <Icon name="qr" size={18} />
              {pairBusy
                ? t('host.creating')
                : codeExpired || scopeChanged
                  ? t('host.newQr')
                  : pairing
                    ? t('host.newCode')
                    : t('host.create')}
            </button>
          </div>

          {pairing && !expired && (
            <div className="field">
              <label htmlFor="host-link">{t('host.link')}</label>
              <div className={styles.linkRow}>
                <input
                  id="host-link"
                  ref={linkRef}
                  type="text"
                  readOnly
                  value={pairing.uri}
                  onFocus={(e) => e.target.select()}
                  spellCheck={false}
                />
                <button type="button" className="btn btn-sm" onClick={() => void copyLink()}>
                  {copy === 'copied' ? t('host.copied') : t('host.copy')}
                </button>
              </div>
              {copy === 'failed' && <span className={styles.help}>{t('host.copyFailed')}</span>}
            </div>
          )}
          <p className={styles.help}>{t('host.howTo')}</p>
          <p className={styles.note}>{t('host.single')}</p>
        </section>
      )}
    </div>
  );
}

/**
 * Why this code cannot register the phone for remote access, with the one
 * thing to do: reconnect DNSPilot here when that fixes it, otherwise the
 * console guidance.
 */
function InviteSkipped({
  reason,
  dnspilot,
  onRelinked,
}: {
  reason: string;
  dnspilot?: DnsPilotService;
  onRelinked: () => void;
}) {
  const { t } = useI18n();
  const fix =
    dnspilot && (RELINK_REASONS as readonly string[]).includes(reason)
      ? (reason as (typeof RELINK_REASONS)[number])
      : null;
  return (
    <div className={styles.inviteSkipped} role="note">
      <p className={styles.help}>{fix ? t(`host.invite.${fix}_fix`) : inviteNote(t, reason)}</p>
      {fix && dnspilot && <DnsPilotRelink service={dnspilot} onConnected={onRelinked} />}
    </div>
  );
}

/** `YYYY-MM-DD` in local time. */
function isoDate(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/**
 * Two-factor sign-in (TOTP) of this PC's server: always shows whether an
 * authenticator is set up, enrolls one, sets it up again (the old one
 * keeps working until the new code is confirmed) and makes new backup
 * codes. Desktop only: these go over vcad's owner-only local channel,
 * which paired phones never reach.
 */
function TotpSetup({
  host,
  remote,
  onChanged,
}: {
  host: HostServices;
  remote: RemoteInfo;
  onChanged: () => void;
}) {
  const { t } = useI18n();
  const [enrollment, setEnrollment] = useState<TotpEnrollment | null>(null);
  const [code, setCode] = useState('');
  const [currentCode, setCurrentCode] = useState('');
  const stepUp = useStepUp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shown, setShown] = useState<{ codes: string[]; fresh: boolean } | null>(null);
  const [confirmNewCodes, setConfirmNewCodes] = useState(false);
  const [backupProof, setBackupProof] = useState('');
  const enrolled = remote.totp_enrolled;

  const begin = async () => {
    setBusy(true);
    setError(null);
    try {
      const action = enrolled ? 'totp_reset' : 'totp_setup';
      setEnrollment(await stepUp.run(action, () => host.totpBegin(enrolled)));
    } catch (err) {
      if (!isStepUpCanceled(err)) setError(describeHostError(t, err));
    } finally {
      setBusy(false);
    }
  };

  const confirm = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const action = enrolled ? 'totp_reset' : 'totp_setup';
      const current = enrolled ? currentCode.trim() : undefined;
      const done = await stepUp.run(action, () => host.totpConfirm(code.trim(), current));
      setEnrollment(null);
      setCode('');
      setCurrentCode('');
      setShown({ codes: done.backup_codes, fresh: false });
    } catch (err) {
      if (!isStepUpCanceled(err)) setError(describeHostError(t, err));
    } finally {
      setBusy(false);
    }
  };

  const newBackupCodes = async () => {
    if (!host.totpBackupCodes) return;
    setBusy(true);
    setError(null);
    try {
      const refresh = host.totpBackupCodes.bind(host);
      const proof = backupProof.trim();
      const done = await stepUp.run('backup_codes_export', () => refresh(proof));
      setConfirmNewCodes(false);
      setBackupProof('');
      setShown({ codes: done.backup_codes, fresh: true });
    } catch (err) {
      if (!isStepUpCanceled(err)) setError(describeHostError(t, err));
    } finally {
      setBusy(false);
    }
  };

  const cancel = () => {
    setEnrollment(null);
    setCode('');
    setCurrentCode('');
    setError(null);
  };

  const entry = remote.totp_issuer
    ? remote.totp_account
      ? `${remote.totp_issuer} (${remote.totp_account})`
      : remote.totp_issuer
    : TOTP_ISSUER;
  const total = remote.backup_codes_total ?? 0;

  return (
    <section className={`panel ${styles.section}`} aria-labelledby="host-totp">
      <h3 id="host-totp">{t('host.totpTitle')}</h3>
      <div className={styles.serverRow}>
        <span
          className={`${styles.chip} ${enrolled ? styles.chipOn : styles.chipOff}`}
          role="status"
        >
          <span className={styles.dot} aria-hidden="true" />
          {!enrolled
            ? t('host.totpStatusOff')
            : remote.totp_enrolled_at
              ? t('host.totpStatusOn', { date: isoDate(remote.totp_enrolled_at) })
              : t('host.totpStatusOnUndated')}
        </span>
      </div>
      {enrolled && (
        <dl className={styles.dl}>
          <dt>{t('host.totpEntryLabel')}</dt>
          <dd>{entry}</dd>
          {total > 0 && (
            <>
              <dt>{t('host.totpBackupLabel')}</dt>
              <dd>
                {t('host.totpBackupLeft', {
                  count: remote.backup_codes_remaining ?? 0,
                  total,
                })}
              </dd>
            </>
          )}
        </dl>
      )}

      {shown ? (
        <>
          <p role="status" className={styles.success}>
            {shown.fresh ? t('host.totpNewBackupDone') : t('host.totpBackup')}
          </p>
          <ul className={styles.backup}>
            {shown.codes.map((c) => (
              <li key={c}>{c}</li>
            ))}
          </ul>
          <BackupCodeActions codes={shown.codes} />
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => {
              setShown(null);
              onChanged();
            }}
          >
            {t('host.totpDone')}
          </button>
        </>
      ) : enrollment ? (
        <>
          <p>{t('host.totpScan')}</p>
          {enrolled && <p className={styles.help}>{t('host.totpResetHelp')}</p>}
          <div className={styles.qrBlock}>
            <img
              className={styles.qr}
              src={svgDataUri(enrollment.qr_svg)}
              alt={t('host.totpQrAlt')}
              width={220}
              height={220}
            />
          </div>
          <p className={styles.help}>
            {t('host.totpSecret')}: <code className={styles.mono}>{enrollment.secret}</code>
          </p>
          <form className={styles.totpForm} onSubmit={(e) => void confirm(e)} noValidate>
            <div className="field">
              <label htmlFor="host-totp-code">{t('host.totpCode')}</label>
              <input
                id="host-totp-code"
                type="text"
                inputMode="numeric"
                autoComplete="one-time-code"
                maxLength={10}
                value={code}
                onChange={(e) => setCode(e.target.value)}
              />
            </div>
            {enrolled && (
              <div className="field">
                <label htmlFor="host-totp-current">{t('host.totpCurrentCode')}</label>
                <input
                  id="host-totp-current"
                  type="text"
                  autoComplete="off"
                  maxLength={24}
                  value={currentCode}
                  onChange={(e) => setCurrentCode(e.target.value)}
                />
              </div>
            )}
            <button
              type="submit"
              className="btn btn-primary"
              disabled={busy || !code.trim() || (enrolled && !currentCode.trim())}
            >
              {t('host.totpConfirm')}
            </button>
            {/* Nothing changes until a code is confirmed: a first setup can be
                left too. */}
            <button type="button" className="btn" disabled={busy} onClick={cancel}>
              {t('common.cancel')}
            </button>
          </form>
        </>
      ) : confirmNewCodes ? (
        <>
          <p role="alert" className={styles.warning}>
            {t('host.totpNewBackupWarn')}
          </p>
          <div className="field">
            <label htmlFor="host-backup-proof">{t('host.totpCurrentCode')}</label>
            <input
              id="host-backup-proof"
              type="text"
              autoComplete="one-time-code"
              maxLength={32}
              value={backupProof}
              onChange={(e) => setBackupProof(e.target.value)}
            />
          </div>
          <div className={styles.actions}>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || !backupProof.trim()}
              onClick={() => void newBackupCodes()}
            >
              {t('host.totpNewBackupGo')}
            </button>
            <button
              type="button"
              className="btn"
              disabled={busy}
              onClick={() => setConfirmNewCodes(false)}
            >
              {t('common.cancel')}
            </button>
          </div>
        </>
      ) : enrolled ? (
        <>
          <div className={styles.actions}>
            <button type="button" className="btn" disabled={busy} onClick={() => void begin()}>
              {t('host.totpReset')}
            </button>
            {host.totpBackupCodes && (
              <button
                type="button"
                className="btn"
                disabled={busy}
                onClick={() => setConfirmNewCodes(true)}
              >
                {t('host.totpNewBackup')}
              </button>
            )}
          </div>
          <p className={styles.help}>{t('host.totpResetHelp')}</p>
        </>
      ) : (
        <>
          <p>{remote.require_totp ? t('host.totpNeeded') : t('host.totpOptional')}</p>
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy}
            onClick={() => void begin()}
          >
            {t('host.totpStart')}
          </button>
        </>
      )}
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
    </section>
  );
}
