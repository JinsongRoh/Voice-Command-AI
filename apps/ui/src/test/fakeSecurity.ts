import { vi } from 'vitest';

import { NativeError } from '../native/bridge';
import type {
  MailSettingsInput,
  MailView,
  SecurityServices,
  SecurityStatus,
} from '../native/security';

export function securityStatus(extra: Partial<SecurityStatus> = {}): SecurityStatus {
  return {
    verifier: { kind: 'windows_hello', available: true, detail: null },
    totp: { enrolled: true, enrolled_at: null, backup_codes_remaining: 8, backup_codes_total: 10 },
    mail: { configured: false, mode: null, recipients: [], notifications: false },
    step_up: { active: false, expires_at: null, grace_secs: 120 },
    wizard: { state: 'done', updated_at: null },
    remote_shell: { enabled: false },
    recovery: 'backup_code_and_local',
    setup_complete: true,
    ...extra,
  };
}

export function mailView(extra: Partial<MailView> = {}): MailView {
  return {
    mode: 'smtp',
    smtp_host: 'smtp.example.com',
    smtp_port: 587,
    smtp_tls: 'starttls',
    smtp_username: 'owner',
    relay_url: null,
    api_key_header: null,
    sender_name: 'VCA',
    sender_address: 'vca@example.com',
    recipients: ['owner@example.com'],
    language: 'en',
    notifications: {
      enabled: true,
      device_paired: true,
      auth_failures: true,
      totp_changes: true,
      recovery: true,
      mail_changes: true,
    },
    allowed_recipient_domains: [],
    preset: [],
    password_set: false,
    relay_key_set: false,
    gateway_token_set: false,
    configured: true,
    missing: [],
    ...extra,
  };
}

/**
 * Security services that behave like the shell: gated calls fail with
 * `step_up_required` until `stepUp` succeeds.
 */
export function fakeSecurity(initial: SecurityStatus = securityStatus(), mail = mailView()) {
  const state = { status: initial, mail, verified: false };
  const gated = <T>(run: () => T) =>
    vi.fn(async (..._args: unknown[]): Promise<T> => {
      if (!state.verified) throw new NativeError('step_up_required', 'verify first');
      return run();
    });
  const services = {
    status: vi.fn(async () => state.status),
    stepUp: vi.fn(async () => {
      state.verified = true;
      return { method: 'windows_hello' as const, expires_at: Date.now() + 120_000 };
    }),
    stepUpTest: vi.fn(async () => {}),
    stepUpEnd: vi.fn(async () => {
      state.verified = false;
      return null;
    }),
    totpBegin: gated(() => ({
      otpauth_uri: 'otpauth://totp/VCA',
      secret: 'NEWSECRET',
      qr_svg: '<svg xmlns="http://www.w3.org/2000/svg"/>',
    })),
    totpConfirm: gated(() => {
      state.status = {
        ...state.status,
        totp: { ...state.status.totp, enrolled: true },
        setup_complete: true,
      };
      return { backup_codes: ['AAAA-1111', 'BBBB-2222'] };
    }),
    backupCodes: gated(() => ({ backup_codes: ['CCCC-3333'] })),
    totpDisable: gated(() => state.status),
    settings: vi.fn(async (patch: { grace_secs?: number; remote_shell?: boolean }) => {
      if (!state.verified && patch.remote_shell !== false)
        throw new NativeError('step_up_required', 'verify first');
      state.status = {
        ...state.status,
        remote_shell: { enabled: patch.remote_shell ?? state.status.remote_shell.enabled },
        step_up: {
          ...state.status.step_up,
          grace_secs: patch.grace_secs ?? state.status.step_up.grace_secs,
        },
      };
      return state.status;
    }),
    wizard: vi.fn(async (wizardState: 'done' | 'skipped') => {
      state.status = {
        ...state.status,
        wizard: { state: wizardState, updated_at: Date.now() },
      };
      return state.status;
    }),
    mailGet: vi.fn(async () => state.mail),
    mailSet: vi.fn(async (settings: MailSettingsInput) => {
      if (!state.verified) throw new NativeError('step_up_required', 'verify first');
      state.mail = { ...state.mail, ...settings };
      return state.mail;
    }),
    mailSecret: vi.fn(async (which: string, secret: string | null) => {
      if (!state.verified) throw new NativeError('step_up_required', 'verify first');
      const field =
        which === 'smtp_password'
          ? 'password_set'
          : which === 'relay_key'
            ? 'relay_key_set'
            : 'gateway_token_set';
      state.mail = { ...state.mail, [field]: secret !== null };
      return state.mail;
    }),
    mailTest: vi.fn(async () => ({ sent_to: ['owner@example.com'] })),
    recoveryStart: gated(() => ({ sent_to: ['o***@example.com'], expires_at: Date.now() + 1000 })),
    recoveryFinish: gated(() => ({ grant_expires_at: Date.now() + 600_000 })),
  } satisfies SecurityServices;
  return { services, state };
}
