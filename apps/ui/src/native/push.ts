/**
 * Approval alerts through DNSPilot's encrypted push relay (the Rust side is
 * `apps/shell/src/push.rs`). Desktop: the owner's opt-in, the separate
 * push-only DNSPilot approval and per-phone mute. Phone: hands its device
 * token to the PC over the Noise channel (the web view never sees it) and
 * says which session a tapped notification points at. A tap only opens a
 * view: nothing is approved from a notification.
 */

import { call } from './bridge';

export interface PushPhoneInfo {
  /** The paired device id (matches the devices list). */
  phone: string;
  platform: 'ios' | 'android';
  muted: boolean;
  /** DNSPilot knows this phone. */
  registered: boolean;
  /** Stable code of the last failure (`push_not_configured`, ...). */
  lastError: string | null;
}

export interface PushStatus {
  /** The owner turned alerts on (off by default). */
  enabled: boolean;
  /** This build knows DNSPilot's `app_id`. */
  available: boolean;
  /** The push-only DNSPilot grant is stored. */
  authorized: boolean;
  /** A DNSPilot approval is in progress. */
  pendingApproval: boolean;
  /** DNSPilot answered 409: push is not configured for this app yet. */
  notConfigured?: boolean;
  phones: PushPhoneInfo[];
}

export interface PushPrompt {
  userCode: string;
  verificationUri: string;
  verificationUriComplete: string | null;
  expiresIn: number;
}

/** Where a tapped notification leads (ids only; may be unknown). */
export interface PushOpenedTarget {
  machineId: string | null;
  sessionId: string | null;
}

export const pushBridge = {
  // --- Desktop ---
  status: () => call<PushStatus>('vca_push_status'),
  setEnabled: (enabled: boolean) => call<PushStatus>('vca_push_set_enabled', { enabled }),
  setMuted: (phone: string, muted: boolean) =>
    call<PushStatus>('vca_push_set_muted', { phone, muted }),
  /** Starts the push-only approval (`push:register push:send`); never the tunnel grant. */
  authorizeStart: () => call<PushPrompt>('vca_push_authorize_start'),
  /** Resolves once the person approved (rejects on deny, expiry or cancel). */
  authorizeWait: () => call<PushStatus>('vca_push_authorize_wait'),
  authorizeCancel: () => call<void>('vca_push_authorize_cancel'),
  openActivation: () => call<void>('vca_push_open_activation'),
  /** Revokes only the push grant; tunnels are untouched. */
  disconnect: () => call<PushStatus>('vca_push_disconnect'),

  // --- Phone ---
  phoneEnable: (machineId: string, locale: string | null) =>
    call<void>('vca_push_phone_enable', { machineId, locale }),
  phoneDisable: (machineId: string) => call<void>('vca_push_phone_disable', { machineId }),
  /** The tapped notification, once; `null` when there is none. */
  takeOpened: () => call<PushOpenedTarget | null>('vca_push_take_opened'),
};

export type PushService = typeof pushBridge;

const PHONE_KEY = 'vca.pushPhone.';

/** The phone's own choice per machine (the PC keeps the registration). */
export function phoneAlertsWanted(machineId: string): boolean {
  try {
    return localStorage.getItem(PHONE_KEY + machineId) === '1';
  } catch {
    return false;
  }
}

export function setPhoneAlertsWanted(machineId: string, wanted: boolean): void {
  try {
    if (wanted) localStorage.setItem(PHONE_KEY + machineId, '1');
    else localStorage.removeItem(PHONE_KEY + machineId);
  } catch {
    // Private mode: the toggle just does not survive a restart.
  }
}
