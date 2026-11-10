/**
 * The VCA server on this PC (desktop shell only): its status, starting and
 * stopping the bundled server, pairing codes for phones and TOTP enrollment.
 * Mirrors `apps/shell/src/host/mod.rs`. Pairing codes are created by Rust
 * over vcad's owner-only local channel; the webview never talks to that
 * channel directly.
 */
import { call, type DeviceScope } from './bridge';
import { agentHooksBridge, type AgentHooksServices } from './agentHooks';
import { engineBridge, type EngineServices } from './engines';
import { launchAtLoginBridge, type LaunchAtLoginServices } from './launchAtLogin';
import { updaterBridge, type UpdaterServices } from './updater';

/** Machine id of this PC's own server (paired machines use 16 hex characters). */
export const LOCAL_MACHINE_ID = 'local';

/** Event emitted by the shell when the local server comes up or goes away. */
export const HOST_CHANGED_EVENT = 'vca-host-changed';

export function isLocalMachine(machineId: string): boolean {
  return machineId === LOCAL_MACHINE_ID;
}

export interface RemoteInfo {
  enabled: boolean;
  listen: string | null;
  provider: string | null;
  provider_state: string | null;
  provider_diagnostics: string[];
  totp_enrolled: boolean;
  /** When the authenticator was enrolled (Unix ms); absent from older servers. */
  totp_enrolled_at?: number | null;
  /** The authenticator app entry (absent for older enrollments). */
  totp_issuer?: string | null;
  totp_account?: string | null;
  backup_codes_remaining?: number;
  backup_codes_total?: number;
  require_totp: boolean;
  kill_switch_tripped: boolean;
  paired_devices: number;
  connections: number;
}

export interface HostStatus {
  running: boolean;
  endpoint: string;
  server_name: string | null;
  version: string | null;
  remote: RemoteInfo | null;
  /** The running server was started by this app (so it can stop it). */
  managed: boolean;
  /** The server is bundled with this build (so it can be started). */
  can_start: boolean;
  /** "Run the VCA server on this PC" (start it when the app opens). */
  run_server: boolean;
  log_path: string | null;
}

export interface HostPairing {
  /** `vca://pair?...`: contains the one-time secret. */
  uri: string;
  /** Unix seconds. */
  expires_at: number;
  endpoints: string[];
  /** Every endpoint is a loopback address: phones cannot reach it. */
  loopback_only: boolean;
  /** SVG document of the QR code. */
  qr_svg: string;
  scope: DeviceScope;
  server_fingerprint: string;
  /** Shown next to the QR code; the phone must show the same one. */
  confirm_code: string;
  /** The code also registers the phone on the DNSPilot tunnel. */
  invite_included: boolean;
  /**
   * Why a code that goes through DNSPilot has no invite: `not_connected`,
   * `not_owned`, `rate_limited`, `invite_limit` or `failed`.
   */
  invite_skipped: string | null;
}

export interface TotpEnrollment {
  otpauth_uri: string;
  secret: string;
  qr_svg: string;
}

export interface TotpEnrolled {
  backup_codes: string[];
}

export interface HostRestoreStatus {
  supported: boolean;
  sign_in: boolean;
  sign_in_delay_secs: number;
}

export interface HostServices {
  status(): Promise<HostStatus>;
  pair(scope: DeviceScope): Promise<HostPairing>;
  start(): Promise<HostStatus>;
  stop(): Promise<HostStatus>;
  setRunServer(enabled: boolean): Promise<void>;
  /**
   * Starts an enrollment. `replace` sets an enrolled authenticator up
   * again; the old one keeps working until the new code is confirmed.
   */
  totpBegin(replace?: boolean): Promise<TotpEnrollment>;
  /** `currentCode`: from the authenticator being replaced. */
  totpConfirm(code: string, currentCode?: string): Promise<TotpEnrolled>;
  /**
   * Replaces the backup codes (the old ones stop working). `currentCode`:
   * from the authenticator, or a backup code.
   */
  totpBackupCodes?(currentCode?: string): Promise<TotpEnrolled>;
  /** Desktop shortcut and restore at sign-in (this PC only). */
  restoreLocal?: {
    status(): Promise<HostRestoreStatus>;
    createShortcut(name: string): Promise<HostRestoreStatus>;
    setSignIn(enabled: boolean, delaySecs: number): Promise<HostRestoreStatus>;
  };
  /** Speech engines, LLMs and TTS engines of this PC's server (desktop only). */
  engines?: EngineServices;
  /** Agent hooks for exact approval alerts on this PC (desktop only). */
  agentHooks?: AgentHooksServices;
  /** In-app updates of this desktop app (desktop only). */
  updater?: UpdaterServices;
  /** "Start VCA when I sign in" on this PC (desktop only). */
  launchAtLogin?: LaunchAtLoginServices;
}

export const hostBridge: HostServices = {
  status: () => call<HostStatus>('vca_host_status'),
  pair: (scope) => call<HostPairing>('vca_host_pair', { scope }),
  start: () => call<HostStatus>('vca_host_start'),
  stop: () => call<HostStatus>('vca_host_stop'),
  setRunServer: (enabled) => call<void>('vca_host_set_run_server', { enabled }),
  totpBegin: (replace = false) => call<TotpEnrollment>('vca_host_totp_begin', { replace }),
  totpConfirm: (code, currentCode) =>
    call<TotpEnrolled>('vca_host_totp_confirm', { code, current_code: currentCode ?? null }),
  totpBackupCodes: (currentCode) =>
    call<TotpEnrolled>('vca_host_totp_backup_codes', { current_code: currentCode ?? null }),
  restoreLocal: {
    status: () => call<HostRestoreStatus>('vca_host_restore_status'),
    createShortcut: (name) => call<HostRestoreStatus>('vca_host_restore_shortcut', { name }),
    setSignIn: (enabled, delaySecs) =>
      call<HostRestoreStatus>('vca_host_restore_sign_in', { enabled, delaySecs }),
  },
  engines: engineBridge,
  agentHooks: agentHooksBridge,
  updater: updaterBridge,
  launchAtLogin: launchAtLoginBridge,
};

/** True on platforms whose shell runs a local server (desktop). */
export function isDesktopPlatform(platform: string): boolean {
  return platform === 'windows' || platform === 'macos' || platform === 'linux';
}

/** An SVG document as an `<img>` source (the CSP allows `data:` images). */
export function svgDataUri(svg: string): string {
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}

/** Whole seconds until `expiresAt` (Unix seconds), never negative. */
export function secondsLeft(expiresAt: number, nowMs: number): number {
  return Math.max(0, Math.floor(expiresAt - nowMs / 1000));
}

/** `m:ss`. */
export function formatCountdown(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

/** The listen address only accepts connections from this PC. */
export function isLoopbackListen(listen: string | null): boolean {
  if (!listen) return false;
  const host = listen.startsWith('[')
    ? listen.slice(1, listen.indexOf(']'))
    : listen.slice(0, listen.lastIndexOf(':') >= 0 ? listen.lastIndexOf(':') : undefined);
  return host === 'localhost' || host === '::1' || /^127\./.test(host);
}
