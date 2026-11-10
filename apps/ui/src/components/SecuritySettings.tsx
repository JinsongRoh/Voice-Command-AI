import { useEffect, useId, useState, type ReactNode } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { toNativeError } from '../native/bridge';
import {
  DEFAULT_GRACE_SECS,
  GRACE_CHOICES,
  NOTIFICATION_KINDS,
  describeSecurityError,
  isStepUpCanceled,
  type MailMode,
  type MailSecretKind,
  type MailSettingsInput,
  type MailView,
  type NotificationPrefs,
  type SecurityServices,
  type SecurityStatus,
  type TlsMode,
} from '../native/security';
import { useSecurityStatus } from '../native/useSecurityStatus';
import { BackupCodes, TotpSetupPanel } from './SecurityTotpSetup';
import { EnsureStepUp, useStepUp } from './StepUpPrompt';
import styles from './SecuritySettings.module.css';

type Tone = 'ok' | 'warn';

interface StatusItem {
  id: string;
  label: MessageKey;
  tone: Tone;
  value: string;
  detail?: string | null;
}

function isoDate(ms: number): string {
  return new Date(ms).toISOString().slice(0, 10);
}

/** The status rows (shared by Settings and the wizard summary). */
export function SecurityStatusList({ status }: { status: SecurityStatus }) {
  const { t } = useI18n();
  const v = status.verifier;
  const osOk = v.kind !== 'none' && v.available;
  const items: StatusItem[] = [
    {
      id: 'verifier',
      label: 'security.item.verifier',
      tone: osOk || status.totp.enrolled ? 'ok' : 'warn',
      value: osOk
        ? t(`security.verifier.${v.kind}`)
        : v.kind !== 'none'
          ? t('security.verifier.unavailable', { name: t(`security.verifier.${v.kind}`) })
          : t('security.verifier.none'),
      detail: v.detail,
    },
    {
      id: 'totp',
      label: 'security.item.totp',
      tone: status.totp.enrolled && status.totp.backup_codes_remaining > 0 ? 'ok' : 'warn',
      value: status.totp.enrolled
        ? t('security.totp.on', {
            count: status.totp.backup_codes_remaining,
            total: status.totp.backup_codes_total,
          })
        : t('security.totp.off'),
      detail:
        status.totp.enrolled && status.totp.enrolled_at
          ? t('security.totp.since', { date: isoDate(status.totp.enrolled_at) })
          : status.totp.enrolled && status.totp.backup_codes_remaining === 0
            ? t('security.totp.noCodesLeft')
            : null,
    },
    {
      id: 'mail',
      label: 'security.item.mail',
      tone: status.mail.configured && status.mail.notifications ? 'ok' : 'warn',
      value: !status.mail.configured
        ? t('security.mail.notSet')
        : status.mail.notifications
          ? t('security.mail.on', { to: status.mail.recipients.join(', ') })
          : t('security.mail.off'),
    },
    {
      id: 'remote-shell',
      label: 'security.item.remoteShell',
      tone: status.remote_shell.enabled && !status.totp.enrolled ? 'warn' : 'ok',
      value: status.remote_shell.enabled ? t('security.on') : t('security.off'),
    },
    {
      id: 'grace',
      label: 'security.item.grace',
      tone: 'ok',
      value: t('security.minutes', { count: Math.round(status.step_up.grace_secs / 60) }),
    },
  ];
  return (
    <ul className={styles.status} data-testid="security-status">
      {items.map((item) => (
        <li key={item.id} className={styles.statusRow} data-testid={`security-item-${item.id}`}>
          <span className={styles.statusText}>
            <span className={styles.statusLabel}>{t(item.label)}</span>
            <span className={styles.statusValue}>{item.value}</span>
            {item.detail && <span className={styles.statusDetail}>{item.detail}</span>}
          </span>
          <span
            className={`${styles.badge} ${item.tone === 'ok' ? styles.badgeOk : styles.badgeWarn}`}
          >
            {item.tone === 'ok' ? t('security.badge.ok') : t('security.badge.warn')}
          </span>
        </li>
      ))}
    </ul>
  );
}

/** Where the security settings live when this is not the PC app. */
export function SecurityHint() {
  const { t } = useI18n();
  return <p className="muted">{t('security.hint')}</p>;
}

function Loading({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  const { t } = useI18n();
  if (!error)
    return (
      <p className="muted" role="status">
        {t('security.loading')}
      </p>
    );
  return (
    <div className={styles.box}>
      <p role="alert" className={styles.error}>
        {toNativeError(error).code === 'unreachable'
          ? t('security.serverOff')
          : describeSecurityError(t, error)}
      </p>
      <button type="button" className="btn btn-sm" onClick={onRetry}>
        {t('security.retry')}
      </button>
    </div>
  );
}

/**
 * Settings → Security: the status of each protection, a verification test,
 * the authenticator, backup codes, the remote shell and the grace window.
 */
export function SecurityOverview({ services }: { services: SecurityServices }) {
  return (
    <EnsureStepUp services={services}>
      <Overview services={services} />
    </EnsureStepUp>
  );
}

function Overview({ services }: { services: SecurityServices }) {
  const { t } = useI18n();
  const stepUp = useStepUp();
  const { status, error, refresh, set } = useSecurityStatus(services);
  const [panel, setPanel] = useState<'totp' | 'confirmCodes' | null>(null);
  const [codes, setCodes] = useState<string[] | null>(null);
  const [currentCode, setCurrentCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ text: string; error: boolean } | null>(null);
  const graceId = useId();
  const currentCodeId = useId();

  if (!status) return <Loading error={error} onRetry={() => void refresh()} />;

  const act = async (work: () => Promise<void>) => {
    setBusy(true);
    setMessage(null);
    try {
      await work();
    } catch (err) {
      if (!isStepUpCanceled(err)) setMessage({ text: describeSecurityError(t, err), error: true });
    } finally {
      setBusy(false);
    }
  };

  const verifyNow = () =>
    act(async () => {
      const ok = await stepUp.request('test');
      if (ok) setMessage({ text: t('security.verified'), error: false });
    });

  const setGrace = (secs: number) =>
    act(async () => {
      set(await stepUp.run('security_settings', () => services.settings({ grace_secs: secs })));
      setMessage({ text: t('security.saved'), error: false });
    });

  const setRemoteShell = (on: boolean) =>
    act(async () => {
      set(
        await stepUp.run(on ? 'remote_shell_enable' : 'security_settings', () =>
          services.settings({ remote_shell: on }),
        ),
      );
    });

  const newCodes = () =>
    act(async () => {
      const code = currentCode.trim();
      const done = await stepUp.run('backup_codes_export', () => services.backupCodes(code));
      setPanel(null);
      setCurrentCode('');
      setCodes(done.backup_codes);
    });

  const endStepUp = () =>
    act(async () => {
      await services.stepUpEnd();
      await refresh();
    });

  const grace = status.step_up.grace_secs || DEFAULT_GRACE_SECS;
  const graceChoices: number[] = GRACE_CHOICES.includes(grace as (typeof GRACE_CHOICES)[number])
    ? [...GRACE_CHOICES]
    : [...GRACE_CHOICES, grace].sort((a, b) => a - b);

  return (
    <div className={styles.box}>
      <SecurityStatusList status={status} />

      {status.step_up.active && status.step_up.expires_at && (
        <p className={styles.inline} role="status">
          <span className="muted">
            {t('security.activeUntil', {
              time: new Date(status.step_up.expires_at).toLocaleTimeString([], {
                hour: '2-digit',
                minute: '2-digit',
              }),
            })}
          </span>
          <button
            type="button"
            className={styles.linkButton}
            disabled={busy}
            onClick={() => void endStepUp()}
          >
            {t('security.endNow')}
          </button>
        </p>
      )}

      <div className={styles.actions}>
        <button
          type="button"
          className="btn btn-sm"
          disabled={busy}
          onClick={() => void verifyNow()}
        >
          {t('security.verifyNow')}
        </button>
        {panel !== 'totp' && !codes && (
          <button
            type="button"
            className={`btn btn-sm ${status.totp.enrolled ? '' : 'btn-primary'}`}
            disabled={busy}
            onClick={() => {
              setMessage(null);
              setPanel('totp');
            }}
          >
            {status.totp.enrolled ? t('security.totp.replace') : t('security.totp.setUp')}
          </button>
        )}
        {status.totp.enrolled && panel === null && !codes && (
          <button
            type="button"
            className="btn btn-sm"
            disabled={busy}
            onClick={() => setPanel('confirmCodes')}
          >
            {t('security.codes.regenerate')}
          </button>
        )}
      </div>

      {panel === 'totp' && (
        <TotpSetupPanel
          services={services}
          replace={status.totp.enrolled}
          onDone={() => {
            setPanel(null);
            void refresh();
          }}
          onCancel={() => setPanel(null)}
        />
      )}
      {panel === 'confirmCodes' && (
        <div className={styles.panelBox}>
          <p role="alert" className={styles.warning}>
            {t('security.codes.regenerateWarn')}
          </p>
          <div className="field">
            <label htmlFor={currentCodeId}>{t('security.totp.currentCode')}</label>
            <input
              id={currentCodeId}
              type="text"
              autoComplete="one-time-code"
              maxLength={32}
              value={currentCode}
              onChange={(e) => setCurrentCode(e.target.value)}
            />
          </div>
          <div className={styles.actions}>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              disabled={busy || !currentCode.trim()}
              onClick={() => void newCodes()}
            >
              {t('security.codes.regenerateGo')}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => {
                setPanel(null);
                setCurrentCode('');
              }}
            >
              {t('common.cancel')}
            </button>
          </div>
        </div>
      )}
      {codes && (
        <div className={styles.panelBox}>
          <p role="status" className={styles.success}>
            {t('security.codes.fresh')}
          </p>
          <BackupCodes
            codes={codes}
            onDone={() => {
              setCodes(null);
              void refresh();
            }}
          />
        </div>
      )}

      <div className={styles.settingRow}>
        <div>
          <span className={styles.statusLabel}>{t('security.item.remoteShell')}</span>
          <p className={styles.help}>{t('security.remoteShellHelp')}</p>
        </div>
        <button
          type="button"
          className="btn btn-sm"
          aria-pressed={status.remote_shell.enabled}
          disabled={busy}
          onClick={() => void setRemoteShell(!status.remote_shell.enabled)}
        >
          {status.remote_shell.enabled ? t('security.turnOff') : t('security.turnOn')}
        </button>
      </div>

      <div className={styles.settingRow}>
        <div>
          <label className={styles.statusLabel} htmlFor={graceId}>
            {t('security.item.grace')}
          </label>
          <p className={styles.help}>{t('security.graceHelp')}</p>
        </div>
        <select
          id={graceId}
          value={grace}
          disabled={busy}
          onChange={(e) => void setGrace(Number(e.target.value))}
        >
          {graceChoices.map((s) => (
            <option key={s} value={s}>
              {t('security.minutes', { count: Math.round(s / 60) })}
            </option>
          ))}
        </select>
      </div>

      {message && (
        <p
          role={message.error ? 'alert' : 'status'}
          className={message.error ? styles.error : styles.success}
        >
          {message.text}
        </p>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Mail

const SECRET_SET: Record<MailSecretKind, 'password_set' | 'relay_key_set' | 'gateway_token_set'> = {
  smtp_password: 'password_set',
  relay_key: 'relay_key_set',
  gateway_token: 'gateway_token_set',
};

type Secrets = Record<MailSecretKind, string>;
const NO_SECRETS: Secrets = { smtp_password: '', relay_key: '', gateway_token: '' };

function draftOf(view: MailView): MailSettingsInput {
  return {
    mode: view.mode,
    smtp_host: view.smtp_host,
    smtp_port: view.smtp_port,
    smtp_tls: view.smtp_tls,
    smtp_username: view.smtp_username,
    relay_url: view.relay_url,
    api_key_header: view.api_key_header,
    sender_name: view.sender_name,
    sender_address: view.sender_address,
    recipients: view.recipients,
    language: view.language,
    notifications: view.notifications,
  };
}

function blankToNull(v: string | null): string | null {
  const s = (v ?? '').trim();
  return s ? s : null;
}

function parseRecipients(text: string): string[] {
  return text
    .split(/[\s,;]+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

/**
 * Settings → Mail: SMTP or a relay, the owner's address, which events send
 * a mail and a test mail. Secrets are write-only: the form only shows
 * whether one is saved. Saving asks for local verification.
 */
export function MailSettings({
  services,
  onSaved,
}: {
  services: SecurityServices;
  /** After a successful save (the wizard moves on). */
  onSaved?: (view: MailView) => void;
}) {
  return (
    <EnsureStepUp services={services}>
      <MailForm services={services} onSaved={onSaved} />
    </EnsureStepUp>
  );
}

function MailForm({
  services,
  onSaved,
}: {
  services: SecurityServices;
  onSaved?: (view: MailView) => void;
}) {
  const { t, locale } = useI18n();
  const stepUp = useStepUp();
  const { refresh } = useSecurityStatus(services);
  const uid = useId();
  const [view, setView] = useState<MailView | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [draft, setDraft] = useState<MailSettingsInput | null>(null);
  const [recipients, setRecipients] = useState('');
  const [port, setPort] = useState('');
  const [secrets, setSecrets] = useState<Secrets>(NO_SECRETS);
  const [unlocked, setUnlocked] = useState<Set<string>>(() => new Set());
  /** The server asked for the authenticator: the delivery of mail changes. */
  const [needCode, setNeedCode] = useState(false);
  const [currentCode, setCurrentCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ text: string; error: boolean } | null>(null);
  const [testResult, setTestResult] = useState<{ text: string; error: boolean } | null>(null);

  const adopt = (v: MailView) => {
    setView(v);
    setDraft(draftOf(v));
    setRecipients(v.recipients.join(', '));
    setPort(v.smtp_port === null ? '' : String(v.smtp_port));
  };

  const load = () => {
    setLoadError(null);
    services.mailGet().then(adopt, (err: unknown) => setLoadError(err));
  };

  useEffect(() => {
    let active = true;
    services.mailGet().then(
      (v) => active && adopt(v),
      (err: unknown) => active && setLoadError(err),
    );
    return () => {
      active = false;
    };
  }, [services]);

  if (!view || !draft) return <Loading error={loadError} onRetry={load} />;

  const update = (patch: Partial<MailSettingsInput>) => setDraft({ ...draft, ...patch });
  const isLocked = (field: string) => view.preset.includes(field) && !unlocked.has(field);
  const id = (field: string) => `${uid}-${field}`;

  const setTls = (tls: TlsMode) => {
    const p = Number(port);
    const next = tls === 'tls' ? 465 : tls === 'starttls' ? 587 : null;
    if (next !== null && (!port || p === 587 || p === 465)) setPort(String(next));
    update({ smtp_tls: tls });
  };

  /** A labelled field; preset fields are read-only until "Change". */
  const field = (name: string, label: MessageKey, control: (locked: boolean) => ReactNode) => {
    const locked = isLocked(name);
    return (
      <div className={`field ${styles.mailField}`} key={name}>
        <span className={styles.fieldHead}>
          <label htmlFor={id(name)}>{t(label)}</label>
          {view.preset.includes(name) && (
            <span className={styles.preset}>{t('security.mail.preset')}</span>
          )}
          {locked && (
            <button
              type="button"
              className={styles.linkButton}
              onClick={() => setUnlocked(new Set(unlocked).add(name))}
              aria-label={t('security.mail.changeField', { field: t(label) })}
            >
              {t('security.mail.change')}
            </button>
          )}
        </span>
        {control(locked)}
      </div>
    );
  };

  const text = (
    name: keyof MailSettingsInput,
    label: MessageKey,
    opts: { type?: string; placeholder?: string } = {},
  ) =>
    field(name, label, (locked) => (
      <input
        id={id(name)}
        type={opts.type ?? 'text'}
        placeholder={opts.placeholder}
        readOnly={locked}
        autoComplete="off"
        value={(draft[name] as string | null) ?? ''}
        onChange={(e) => update({ [name]: e.target.value } as Partial<MailSettingsInput>)}
      />
    ));

  const secret = (which: MailSecretKind, label: MessageKey) => {
    const isSet = view[SECRET_SET[which]];
    return (
      <div className={`field ${styles.mailField}`} key={which}>
        <span className={styles.fieldHead}>
          <label htmlFor={id(which)}>{t(label)}</label>
          {isSet && <span className={styles.savedChip}>{t('security.mail.secretSaved')}</span>}
        </span>
        <input
          id={id(which)}
          type="password"
          autoComplete="new-password"
          placeholder={isSet ? t('security.mail.secretKeep') : ''}
          value={secrets[which]}
          onChange={(e) => setSecrets({ ...secrets, [which]: e.target.value })}
        />
        {isSet && (
          <button
            type="button"
            className={styles.linkButton}
            disabled={busy}
            onClick={() => void removeSecret(which)}
          >
            {t('security.mail.secretRemove')}
          </button>
        )}
      </div>
    );
  };

  const removeSecret = async (which: MailSecretKind) => {
    setBusy(true);
    setMessage(null);
    try {
      const code = currentCode.trim() || undefined;
      adopt(await stepUp.run('mail_settings', () => services.mailSecret(which, null, code)));
      setNeedCode(false);
      setCurrentCode('');
      void refresh();
    } catch (err) {
      if (!isStepUpCanceled(err)) setMessage({ text: describeSecurityError(t, err), error: true });
    } finally {
      setBusy(false);
    }
  };

  const save = async () => {
    setBusy(true);
    setMessage(null);
    setTestResult(null);
    const input: MailSettingsInput = {
      ...draft,
      smtp_host: blankToNull(draft.smtp_host),
      smtp_port: port.trim() ? Number(port) : null,
      smtp_username: blankToNull(draft.smtp_username),
      relay_url: blankToNull(draft.relay_url),
      api_key_header: blankToNull(draft.api_key_header),
      sender_name: blankToNull(draft.sender_name),
      sender_address: blankToNull(draft.sender_address),
      recipients: parseRecipients(recipients),
      language: locale,
    };
    try {
      const code = currentCode.trim() || undefined;
      let next = await stepUp.run('mail_settings', () => services.mailSet(input, code));
      for (const which of Object.keys(secrets) as MailSecretKind[]) {
        const value = secrets[which];
        if (value)
          next = await stepUp.run('mail_settings', () => services.mailSecret(which, value, code));
      }
      setNeedCode(false);
      setCurrentCode('');
      setSecrets(NO_SECRETS);
      setUnlocked(new Set());
      adopt(next);
      void refresh();
      setMessage({
        text:
          next.mode === null
            ? t('security.mail.savedOff')
            : next.configured
              ? t('security.mail.saved')
              : t('security.mail.savedMissing', {
                  fields: next.missing.map((f) => fieldName(t, f)).join(', '),
                }),
        error: false,
      });
      onSaved?.(next);
    } catch (err) {
      if (!isStepUpCanceled(err)) {
        const e = toNativeError(err);
        if (e.code === 'current_code_required') setNeedCode(true);
        setMessage({
          text:
            e.code === 'recipient_not_allowed' && view.allowed_recipient_domains.length > 0
              ? t('security.error.recipient_domains', {
                  domains: view.allowed_recipient_domains.join(', '),
                })
              : describeSecurityError(t, e),
          error: true,
        });
      }
    } finally {
      setBusy(false);
    }
  };

  const sendTest = async () => {
    setBusy(true);
    setTestResult(null);
    try {
      const sent = await services.mailTest();
      setTestResult({
        text: t('security.mail.testSent', { to: sent.sent_to.join(', ') }),
        error: false,
      });
    } catch (err) {
      setTestResult({ text: describeSecurityError(t, err), error: true });
    } finally {
      setBusy(false);
    }
  };

  const setNotify = (key: keyof NotificationPrefs, on: boolean) =>
    update({ notifications: { ...draft.notifications, [key]: on } });

  const modes: (MailMode | null)[] = [null, 'smtp', 'relay'];
  const modeLabelId = `${uid}-mode`;
  const modeLocked = isLocked('mode');

  return (
    <div className={styles.box} data-testid="mail-settings">
      <p className="muted">{t('security.mail.intro')}</p>
      <div className="field">
        <span className={styles.fieldHead}>
          <span className="label" id={modeLabelId}>
            {t('security.mail.mode')}
          </span>
          {view.preset.includes('mode') && (
            <span className={styles.preset}>{t('security.mail.preset')}</span>
          )}
          {modeLocked && (
            <button
              type="button"
              className={styles.linkButton}
              onClick={() => setUnlocked(new Set(unlocked).add('mode'))}
            >
              {t('security.mail.change')}
            </button>
          )}
        </span>
        <div className="segmented" role="group" aria-labelledby={modeLabelId}>
          {modes.map((m) => (
            <button
              key={m ?? 'off'}
              type="button"
              aria-pressed={draft.mode === m}
              disabled={modeLocked && draft.mode !== m}
              onClick={() => update({ mode: m })}
            >
              {t(`security.mail.mode.${m ?? 'off'}`)}
            </button>
          ))}
        </div>
      </div>

      {draft.mode === 'smtp' && (
        <div className={styles.grid}>
          {text('smtp_host', 'security.mail.smtpHost', { placeholder: 'smtp.example.com' })}
          {field('smtp_tls', 'security.mail.tls', (locked) => (
            <select
              id={id('smtp_tls')}
              value={draft.smtp_tls}
              disabled={locked}
              onChange={(e) => setTls(e.target.value as TlsMode)}
            >
              <option value="starttls">STARTTLS</option>
              <option value="tls">TLS</option>
              <option value="none">{t('security.mail.tlsNone')}</option>
            </select>
          ))}
          {field('smtp_port', 'security.mail.port', (locked) => (
            <input
              id={id('smtp_port')}
              type="number"
              inputMode="numeric"
              min={1}
              max={65535}
              readOnly={locked}
              value={port}
              onChange={(e) => setPort(e.target.value)}
            />
          ))}
          {text('smtp_username', 'security.mail.username')}
          {secret('smtp_password', 'security.mail.password')}
          {text('sender_address', 'security.mail.senderAddress', {
            type: 'email',
            placeholder: 'vca@example.com',
          })}
          {text('sender_name', 'security.mail.senderName')}
        </div>
      )}
      {draft.mode === 'relay' && (
        <div className={styles.grid}>
          {text('relay_url', 'security.mail.relayUrl', {
            type: 'url',
            placeholder: 'https://mail.example.com/send',
          })}
          {text('api_key_header', 'security.mail.keyHeader', { placeholder: 'X-VCA-Key' })}
          {secret('relay_key', 'security.mail.relayKey')}
          {secret('gateway_token', 'security.mail.gatewayToken')}
          {text('sender_name', 'security.mail.senderName')}
        </div>
      )}

      {draft.mode !== null && (
        <>
          {field('recipients', 'security.mail.recipients', (locked) => (
            <input
              id={id('recipients')}
              type="text"
              inputMode="email"
              placeholder="me@example.com"
              readOnly={locked}
              autoComplete="off"
              value={recipients}
              onChange={(e) => setRecipients(e.target.value)}
            />
          ))}
          <p className={styles.help}>
            {view.allowed_recipient_domains.length > 0
              ? t('security.mail.recipientsHelpDomains', {
                  domains: view.allowed_recipient_domains.join(', '),
                })
              : t('security.mail.recipientsHelp')}
          </p>

          <fieldset className={styles.fieldset}>
            <legend>{t('security.mail.notifyTitle')}</legend>
            <label className="check">
              <input
                type="checkbox"
                checked={draft.notifications.enabled}
                onChange={(e) => setNotify('enabled', e.target.checked)}
              />
              {t('security.mail.notify.enabled')}
            </label>
            {draft.notifications.enabled &&
              NOTIFICATION_KINDS.map((k) => (
                <label key={k} className={`check ${styles.indent}`}>
                  <input
                    type="checkbox"
                    checked={draft.notifications[k]}
                    onChange={(e) => setNotify(k, e.target.checked)}
                  />
                  {t(`security.mail.notify.${k}`)}
                </label>
              ))}
          </fieldset>
        </>
      )}

      {needCode && (
        <div className="field">
          <label htmlFor={`${uid}-current-code`}>{t('security.mail.currentCodeNeeded')}</label>
          <input
            id={`${uid}-current-code`}
            type="text"
            autoComplete="one-time-code"
            maxLength={32}
            value={currentCode}
            onChange={(e) => setCurrentCode(e.target.value)}
          />
        </div>
      )}

      <div className={styles.actions}>
        <button
          type="button"
          className="btn btn-sm btn-primary"
          disabled={busy}
          onClick={() => void save()}
        >
          {t('security.mail.save')}
        </button>
        {view.configured && (
          <button
            type="button"
            className="btn btn-sm"
            disabled={busy}
            onClick={() => void sendTest()}
          >
            {t('security.mail.test')}
          </button>
        )}
      </div>
      {!view.configured && view.mode !== null && view.missing.length > 0 && (
        <p className={styles.help}>
          {t('security.mail.missing', {
            fields: view.missing.map((f) => fieldName(t, f)).join(', '),
          })}
        </p>
      )}
      {message && (
        <p
          role={message.error ? 'alert' : 'status'}
          className={message.error ? styles.error : styles.success}
        >
          {message.text}
        </p>
      )}
      {testResult && (
        <p
          role={testResult.error ? 'alert' : 'status'}
          className={testResult.error ? styles.error : styles.success}
          data-testid="mail-test-result"
        >
          {testResult.text}
        </p>
      )}
    </div>
  );
}

const FIELD_LABELS: Record<string, MessageKey> = {
  mode: 'security.mail.mode',
  smtp_host: 'security.mail.smtpHost',
  smtp_port: 'security.mail.port',
  smtp_tls: 'security.mail.tls',
  smtp_username: 'security.mail.username',
  smtp_password: 'security.mail.password',
  relay_url: 'security.mail.relayUrl',
  api_key_header: 'security.mail.keyHeader',
  relay_key: 'security.mail.relayKey',
  gateway_token: 'security.mail.gatewayToken',
  sender_name: 'security.mail.senderName',
  sender_address: 'security.mail.senderAddress',
  recipients: 'security.mail.recipients',
};

function fieldName(t: (k: MessageKey) => string, field: string): string {
  const key = FIELD_LABELS[field];
  return key ? t(key) : field;
}

// ---------------------------------------------------------------------------
// Recovery

/**
 * "I lost my authenticator app": local verification plus a mailed code (or
 * a backup code without mail), then straight into a new authenticator.
 */
export function SecurityRecovery({ services }: { services: SecurityServices }) {
  return (
    <EnsureStepUp services={services}>
      <Recovery services={services} />
    </EnsureStepUp>
  );
}

type RecoveryStep =
  | { kind: 'idle' }
  | { kind: 'mailCode'; sentTo: string[] }
  | { kind: 'backupCode' }
  | { kind: 'setup' }
  | { kind: 'done' };

function Recovery({ services }: { services: SecurityServices }) {
  const { t } = useI18n();
  const stepUp = useStepUp();
  const { status, error, refresh } = useSecurityStatus(services);
  const [step, setStep] = useState<RecoveryStep>({ kind: 'idle' });
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const codeId = useId();

  if (!status) return <Loading error={error} onRetry={() => void refresh()} />;
  const byMail = status.recovery === 'mail_and_local';

  const start = async () => {
    setMessage(null);
    setCode('');
    if (!byMail) {
      setStep({ kind: 'backupCode' });
      return;
    }
    setBusy(true);
    try {
      const sent = await stepUp.run('recovery', () => services.recoveryStart());
      setStep({ kind: 'mailCode', sentTo: sent.sent_to });
    } catch (err) {
      if (isStepUpCanceled(err)) return;
      if (toNativeError(err).code === 'mail_not_configured') {
        setStep({ kind: 'backupCode' });
        setMessage(t('security.recovery.noMailUseBackup'));
      } else setMessage(describeSecurityError(t, err));
    } finally {
      setBusy(false);
    }
  };

  const finish = async () => {
    const value = code.trim();
    if (!value) return;
    setBusy(true);
    setMessage(null);
    try {
      await stepUp.run('recovery', () =>
        services.recoveryFinish(
          step.kind === 'mailCode' ? { code: value } : { backup_code: value },
        ),
      );
      setCode('');
      setStep({ kind: 'setup' });
    } catch (err) {
      if (!isStepUpCanceled(err)) setMessage(describeSecurityError(t, err));
    } finally {
      setBusy(false);
    }
  };

  if (step.kind === 'setup')
    return (
      <div className={styles.box}>
        <p role="status" className={styles.success}>
          {t('security.recovery.verified')}
        </p>
        <TotpSetupPanel
          services={services}
          replace
          afterRecovery
          onDone={() => {
            setStep({ kind: 'done' });
            void refresh();
          }}
        />
      </div>
    );

  return (
    <div className={styles.box} data-testid="security-recovery">
      <p className="muted">
        {byMail ? t('security.recovery.howMail') : t('security.recovery.howBackup')}
      </p>
      {step.kind === 'done' && (
        <p role="status" className={styles.success}>
          {t('security.recovery.done')}
        </p>
      )}
      {step.kind === 'idle' || step.kind === 'done' ? (
        <div className={styles.actions}>
          <button
            type="button"
            className="btn btn-sm"
            disabled={busy || !status.totp.enrolled}
            onClick={() => void start()}
          >
            {t('security.recovery.start')}
          </button>
        </div>
      ) : (
        <form
          className={styles.form}
          noValidate
          onSubmit={(e) => {
            e.preventDefault();
            void finish();
          }}
        >
          {step.kind === 'mailCode' && (
            <p role="status">{t('security.recovery.sent', { to: step.sentTo.join(', ') })}</p>
          )}
          <div className="field">
            <label htmlFor={codeId}>
              {step.kind === 'mailCode'
                ? t('security.recovery.mailCode')
                : t('security.recovery.backupCode')}
            </label>
            <input
              id={codeId}
              type="text"
              autoComplete="one-time-code"
              inputMode={step.kind === 'mailCode' ? 'numeric' : 'text'}
              maxLength={24}
              value={code}
              onChange={(e) => setCode(e.target.value)}
            />
          </div>
          <div className={styles.actions}>
            <button
              type="submit"
              className="btn btn-sm btn-primary"
              disabled={busy || !code.trim()}
            >
              {t('security.recovery.verify')}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              disabled={busy}
              onClick={() => {
                setStep({ kind: 'idle' });
                setMessage(null);
              }}
            >
              {t('common.cancel')}
            </button>
          </div>
        </form>
      )}
      {!status.totp.enrolled && <p className={styles.help}>{t('security.recovery.notEnrolled')}</p>}
      {message && (
        <p role="alert" className={styles.error}>
          {message}
        </p>
      )}
    </div>
  );
}
