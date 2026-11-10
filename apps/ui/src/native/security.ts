/**
 * Security package of this PC's server (desktop shell only): local
 * verification ("step-up" with Windows Hello, Touch ID or an authenticator
 * code), two-step verification (TOTP) and backup codes, mail notifications
 * and recovery of a lost authenticator.
 *
 * The step-up token never reaches the webview: the shell keeps it and
 * attaches it to every gated call. A gated call without a valid token fails
 * with `step_up_required`; `withStepUp` asks for verification and retries the
 * call once. The server chooses what is being approved from that call, so a
 * verification can only follow a gated call that failed, for its own action.
 * A token works for one request, except for the flows of several requests
 * (authenticator setup, recovery, mail settings, conductor confirmations).
 */
import type { TranslateFn } from '../i18n/i18n';
import { call, NativeError, toNativeError } from './bridge';
import { describeNativeError } from './errors';
import type { TotpEnrolled, TotpEnrollment } from './host';

export type { TotpEnrolled, TotpEnrollment } from './host';

export type VerifierKind = 'windows_hello' | 'mac_local_auth' | 'none';

export type GatedAction =
  | 'pairing_create'
  | 'totp_setup'
  | 'totp_reset'
  | 'totp_disable'
  | 'device_approve'
  | 'remote_shell_enable'
  | 'security_settings'
  | 'mail_settings'
  | 'backup_codes_export'
  | 'recovery'
  | 'conductor_setup'
  | 'conductor_decide'
  | 'conductor_kill'
  | 'task_settings'
  | 'engine_url';

export const GATED_ACTIONS: readonly GatedAction[] = [
  'pairing_create',
  'totp_setup',
  'totp_reset',
  'totp_disable',
  'device_approve',
  'remote_shell_enable',
  'security_settings',
  'mail_settings',
  'backup_codes_export',
  'recovery',
  'conductor_setup',
  'conductor_decide',
  'conductor_kill',
  'task_settings',
  'engine_url',
];

export type WizardState = 'pending' | 'done' | 'skipped';
export type MailMode = 'smtp' | 'relay';
/** `none` is only accepted for loopback hosts. */
export type TlsMode = 'starttls' | 'tls' | 'none';
export type MailSecretKind = 'smtp_password' | 'relay_key' | 'gateway_token';

export interface SecurityStatus {
  verifier: { kind: VerifierKind; available: boolean; detail: string | null };
  totp: {
    enrolled: boolean;
    /** Unix ms. */
    enrolled_at: number | null;
    backup_codes_remaining: number;
    backup_codes_total: number;
  };
  mail: {
    /** Everything needed to send is present. */
    configured: boolean;
    mode: MailMode | null;
    recipients: string[];
    /** Master switch (on by default once configured). */
    notifications: boolean;
  };
  /** `expires_at`: Unix ms. */
  step_up: { active: boolean; expires_at: number | null; grace_secs: number };
  wizard: { state: WizardState; updated_at: number | null };
  remote_shell: { enabled: boolean };
  /** `mail_and_local` when mail is configured, else `backup_code_and_local`. */
  recovery: 'mail_and_local' | 'backup_code_and_local';
  /** A local verification method exists (OS verifier or TOTP) and TOTP is enrolled. */
  setup_complete: boolean;
}

export interface StepUpResult {
  method: 'windows_hello' | 'mac_local_auth' | 'totp' | 'backup_code' | 'presence';
  /** Unix ms. */
  expires_at: number;
}

export interface NotificationPrefs {
  /** Master switch. */
  enabled: boolean;
  device_paired: boolean;
  auth_failures: boolean;
  totp_changes: boolean;
  recovery: boolean;
  mail_changes: boolean;
}

export const NOTIFICATION_KINDS = [
  'device_paired',
  'auth_failures',
  'totp_changes',
  'recovery',
  'mail_changes',
] as const satisfies readonly (keyof NotificationPrefs)[];

export interface MailSettingsInput {
  /** `null`: mail off. */
  mode: MailMode | null;
  smtp_host: string | null;
  /** Default 587 (starttls) / 465 (tls). */
  smtp_port: number | null;
  smtp_tls: TlsMode;
  smtp_username: string | null;
  /** https only (http only for loopback). */
  relay_url: string | null;
  /** Relay: the header that carries the relay key, e.g. `X-VCA-Key`. */
  api_key_header: string | null;
  sender_name: string | null;
  /** SMTP only; the relay decides the sender. */
  sender_address: string | null;
  /** Owner address(es), 1 to 5. */
  recipients: string[];
  /** App language; templates exist for ko and en, others fall back to en. */
  language: string;
  notifications: NotificationPrefs;
}

export interface MailView extends MailSettingsInput {
  /** From the install preset; empty: any domain. */
  allowed_recipient_domains: string[];
  /** Field names that came from the install preset. */
  preset: string[];
  password_set: boolean;
  relay_key_set: boolean;
  gateway_token_set: boolean;
  configured: boolean;
  /** Field names still needed, e.g. `['smtp_password', 'recipients']`. */
  missing: string[];
}

export interface StepUpRequest {
  /** Short text in the app language, shown inside the OS dialog. */
  reason: string;
  action: GatedAction;
  /** Authenticator code, when the PC has no OS verifier. */
  totp?: string;
  /** The person confirmed the first setup on a PC with nothing to verify against. */
  confirmed?: boolean;
  /** The app language, for the text of the OS prompt. */
  language?: string;
}

/** Tries the verification; nothing is approved by it. */
export interface StepUpTestRequest {
  totp?: string;
  language?: string;
}

export interface RecoveryStarted {
  /** Masked addresses. */
  sent_to: string[];
  /** Unix ms. */
  expires_at: number;
}

export interface SecurityServices {
  status(): Promise<SecurityStatus>;
  stepUp(request: StepUpRequest): Promise<StepUpResult>;
  stepUpTest(request: StepUpTestRequest): Promise<void>;
  /** Ends the current verification window. */
  stepUpEnd(): Promise<null>;
  /** Gated (`totp_setup` / `totp_reset`). */
  totpBegin(replace: boolean): Promise<TotpEnrollment>;
  /** Gated. Replacing needs `currentCode` unless recovery just finished. */
  totpConfirm(code: string, currentCode?: string): Promise<TotpEnrolled>;
  /**
   * Gated (`backup_codes_export`): new backup codes, the old ones stop working.
   * Needs `currentCode` too: one approval must not mint a recovery proof.
   */
  backupCodes(currentCode?: string): Promise<TotpEnrolled>;
  /** Gated (`totp_disable`). */
  totpDisable(currentCode?: string): Promise<SecurityStatus>;
  /** Gated (`security_settings`; turning the remote shell on: `remote_shell_enable`). */
  settings(patch: { grace_secs?: number; remote_shell?: boolean }): Promise<SecurityStatus>;
  wizard(state: 'done' | 'skipped'): Promise<SecurityStatus>;
  mailGet(): Promise<MailView>;
  /**
   * Gated (`mail_settings`). Changing where mail goes needs `currentCode` while
   * an authenticator is set up (`current_code_required` without it).
   */
  mailSet(settings: MailSettingsInput, currentCode?: string): Promise<MailView>;
  /**
   * Gated (`mail_settings`). `null` deletes the secret. Changing or deleting a
   * stored one needs `currentCode` while an authenticator is set up.
   */
  mailSecret(which: MailSecretKind, secret: string | null, currentCode?: string): Promise<MailView>;
  mailTest(): Promise<{ sent_to: string[] }>;
  /** Gated (`recovery`): mails a 6-digit code. */
  recoveryStart(): Promise<RecoveryStarted>;
  /** Gated (`recovery`): the mailed code or a backup code. */
  recoveryFinish(proof: { code?: string; backup_code?: string }): Promise<{
    grant_expires_at: number;
  }>;
}

export const securityBridge: SecurityServices = {
  status: () => call<SecurityStatus>('vca_host_security_status'),
  stepUp: ({ action, totp, confirmed, language }) =>
    call<StepUpResult>('vca_host_security_step_up', {
      action,
      totp: totp ?? null,
      confirmed: confirmed ?? false,
      language: language ?? null,
    }),
  stepUpTest: ({ totp, language }) =>
    call<void>('vca_host_security_step_up_test', {
      totp: totp ?? null,
      language: language ?? null,
    }),
  stepUpEnd: () => call<null>('vca_host_security_step_up_end'),
  totpBegin: (replace) => call<TotpEnrollment>('vca_host_security_totp_begin', { replace }),
  totpConfirm: (code, currentCode) =>
    call<TotpEnrolled>('vca_host_security_totp_confirm', {
      code,
      current_code: currentCode ?? null,
    }),
  backupCodes: (currentCode) =>
    call<TotpEnrolled>('vca_host_security_backup_codes', { current_code: currentCode ?? null }),
  totpDisable: (currentCode) =>
    call<SecurityStatus>('vca_host_security_totp_disable', { current_code: currentCode ?? null }),
  settings: ({ grace_secs, remote_shell }) =>
    call<SecurityStatus>('vca_host_security_settings', {
      grace_secs: grace_secs ?? null,
      remote_shell: remote_shell ?? null,
    }),
  wizard: (state) => call<SecurityStatus>('vca_host_security_wizard', { state }),
  mailGet: () => call<MailView>('vca_host_mail_get'),
  mailSet: (settings, currentCode) =>
    call<MailView>('vca_host_mail_set', { settings, current_code: currentCode ?? null }),
  mailSecret: (which, secret, currentCode) =>
    call<MailView>('vca_host_mail_secret', {
      which,
      secret,
      current_code: currentCode ?? null,
    }),
  mailTest: () => call<{ sent_to: string[] }>('vca_host_mail_test'),
  recoveryStart: () => call<RecoveryStarted>('vca_host_recovery_start'),
  recoveryFinish: ({ code, backup_code }) =>
    call<{ grant_expires_at: number }>('vca_host_recovery_finish', {
      code: code ?? null,
      backup_code: backup_code ?? null,
    }),
};

/** The error a gated call fails with when it needs local verification first. */
export const STEP_UP_REQUIRED = 'step_up_required';

/** The person closed the verification prompt (callers usually show nothing). */
export const STEP_UP_CANCELED = 'verification_canceled';

export function isStepUpCanceled(err: unknown): boolean {
  return err instanceof NativeError && err.code === STEP_UP_CANCELED;
}

/**
 * Runs a gated call. When it fails with `step_up_required`, `verify` asks
 * for local verification (resolving `true` once verified) and the call is
 * retried once. A declined verification rejects with `verification_canceled`.
 */
export async function withStepUp<T>(
  run: () => Promise<T>,
  verify: () => Promise<boolean>,
): Promise<T> {
  try {
    return await run();
  } catch (err) {
    const e = toNativeError(err);
    if (e.code !== STEP_UP_REQUIRED) throw e;
    if (!(await verify())) throw new NativeError(STEP_UP_CANCELED, 'verification canceled');
    return await run();
  }
}

/** Codes of the security commands that have a dedicated message. */
const SECURITY_CODES = [
  'step_up_required',
  'verification_canceled',
  'verification_failed',
  'verification_unavailable',
  'presence_required',
  'totp_required',
  'totp_invalid',
  'totp_replayed',
  'remote_shell_open',
  'conflict',
  'current_code_required',
  'recipient_not_allowed',
  'mail_not_configured',
  'rate_limited',
  'code_invalid',
  'code_expired',
  'attempts_exceeded',
  'unreachable',
] as const;

type SecurityCode = (typeof SECURITY_CODES)[number];

/** Readable, localized message for a failed security command. */
export function describeSecurityError(t: TranslateFn, err: unknown): string {
  const e = toNativeError(err);
  if (e.code === 'mail_failed') return t('security.error.mail_failed', { reason: e.message });
  if (e.code === 'invalid_request')
    return t('security.error.invalid_request', { reason: e.message });
  if (e.code !== null && (SECURITY_CODES as readonly string[]).includes(e.code))
    return t(`security.error.${e.code as SecurityCode}`);
  return describeNativeError(t, e);
}

/** Grace windows offered in Settings (seconds). */
export const GRACE_CHOICES = [60, 120, 300] as const;
export const DEFAULT_GRACE_SECS = 120;
