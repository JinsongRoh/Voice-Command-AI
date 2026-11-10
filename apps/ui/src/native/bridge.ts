/**
 * Typed wrappers over the native (Tauri 2) commands. This is the only module
 * that calls `invoke`: the Rust side owns the Noise channel, keys and tokens;
 * the webview only exchanges MessagePack envelopes with it.
 */
import {
  Channel,
  invoke,
  isTauri,
  type InvokeArgs,
  type InvokeOptions,
} from '@tauri-apps/api/core';

export type DeviceScope = 'view' | 'command' | 'admin';

export interface MachineInfo {
  id: string;
  name: string;
  endpoints: string[];
  deviceId: string;
  scope: DeviceScope;
  pairedAt: number;
  // Local machine management (newer shells; optional for older ones).
  /** Display alias chosen on this device. */
  alias?: string | null;
  /** Opened on launch. */
  isDefault?: boolean;
  /** Last successful connection, Unix ms. */
  lastConnected?: number | null;
  /** Short fingerprint of the pinned server key. */
  serverFingerprint?: string;
}

/** `plugin:biometric|status` (field names as the plugin sends them). */
export interface BiometricStatus {
  isAvailable: boolean;
  /** 0 none, 1 Touch ID / fingerprint, 2 Face ID, 3 iris. */
  biometryType: number;
  error?: string;
  errorCode?: string;
}

/** Options of `plugin:biometric|authenticate`. */
export interface BiometricAuthOptions {
  /** Accept the device passcode / PIN / pattern when biometrics fail or are missing. */
  allowDeviceCredential?: boolean;
  title?: string;
  subtitle?: string;
  cancelTitle?: string;
  fallbackTitle?: string;
  confirmationRequired?: boolean;
}

export type LinkState =
  'disconnected' | 'connecting' | 'connected' | 'reconnecting' | 'login_required';

export interface LinkStatus {
  state: LinkState;
  error: string | null;
  code: string | null;
}

/** A DNSPilot app-mode tunnel the app knows (no secrets). */
export interface DnsPilotTunnel {
  hostname: string;
  publicPort: number;
  tunnelId: number | null;
  deviceId: number | null;
  createdByApp: boolean;
  clientRegistered: boolean;
  hasAppToken: boolean;
  /** Console status: `active`, `suspended` or `disabled`. */
  status: string | null;
}

/** An active DNSPilot Agent device (for the device picker). */
export interface DnsPilotDevice {
  id: number;
  name: string;
  hostname: string | null;
  os: string | null;
  online: boolean;
  agentVersion: string | null;
}

export interface DnsPilotZone {
  id: number;
  name: string;
}

/** An app-mode tunnel in the person's DNSPilot organization. */
export interface DnsPilotRemoteTunnel {
  id: number;
  hostname: string;
  publicPort: number;
  localPort: number | null;
  deviceId: number | null;
  deviceName: string | null;
  status: string | null;
  /** Already in this app's list. */
  known: boolean;
}

/** Public parts of this app's DNSPilot client key. */
export interface DnsPilotIdentity {
  /** 64 hex characters (SPKI SHA-256), what DNSPilot pins. */
  spkiSha256: string;
  publicKeyPem: string;
}

export interface DnsPilotStatus {
  connected: boolean;
  scope: string | null;
  pending: boolean;
  identity: DnsPilotIdentity | null;
  tunnels: DnsPilotTunnel[];
  consoleUrl: string;
}

/** What to show while the person approves VCA in DNSPilot. */
export interface DnsPilotPrompt {
  userCode: string;
  verificationUri: string;
  verificationUriComplete: string | null;
  expiresIn: number;
}

export interface DnsPilotCreateTunnel {
  deviceId: number;
  zoneId: number;
  localPort?: number | null;
}

export type DnsPilotRegistration =
  | { result: 'registered'; client_id: number }
  | { result: 'already_registered' }
  | {
      result: 'needs_console';
      spki_sha256: string;
      tunnel_id: number | null;
      /** `unknown_tunnel_id`, `not_connected` or `not_created_by_app`. */
      reason: string;
    };

/** The outcome of registering this app's key with an invite code. */
export interface DnsPilotEnrollment {
  hostname: string;
  tunnelId: number;
  /** `null` when the code was used already and this app knows the tunnel. */
  clientId: number | null;
  /** Connections may still be refused for this long; 0 means connect at once. */
  readyWithinSec: number;
}

/** A registered key on a tunnel. */
export interface DnsPilotAppClient {
  id: number;
  label: string | null;
  /** `local`, `console`, `oauth` or `invite`. */
  source: string | null;
  lastSeenAt: string | null;
  /** The key of this app install. */
  thisApp: boolean;
}

/** A device invite (codes are never listed). */
export interface DnsPilotInvite {
  id: number;
  label: string | null;
  /** `active`, `used`, `expired` or `revoked`. */
  status: string | null;
  expiresAt: string | null;
}

export type NativePlatform = 'windows' | 'macos' | 'linux' | 'android' | 'ios';

/** A rejected native command, normalized from a string or `{ code, message }`. */
export class NativeError extends Error {
  constructor(
    readonly code: string | null,
    message: string,
    /** Seconds to wait, when the error is a rate limit that says so. */
    readonly retryAfterSecs: number | null = null,
  ) {
    super(message);
    this.name = 'NativeError';
  }
}

export function toNativeError(err: unknown): NativeError {
  if (err instanceof NativeError) return err;
  if (typeof err === 'string') return new NativeError(null, err);
  if (typeof err === 'object' && err !== null) {
    const { code, message, retry_after_secs } = err as {
      code?: unknown;
      message?: unknown;
      retry_after_secs?: unknown;
    };
    const text =
      typeof message === 'string' ? message : err instanceof Error ? err.message : String(err);
    return new NativeError(
      typeof code === 'string' ? code : null,
      text,
      typeof retry_after_secs === 'number' ? retry_after_secs : null,
    );
  }
  return new NativeError(null, String(err));
}

/**
 * Raw IPC bytes are not available on every platform (Android delivers JSON),
 * so binary payloads may arrive as ArrayBuffer, Uint8Array or number[].
 */
export function toBytes(payload: unknown): Uint8Array<ArrayBuffer> {
  if (payload instanceof ArrayBuffer) return new Uint8Array(payload);
  if (payload instanceof Uint8Array) return new Uint8Array(payload);
  if (ArrayBuffer.isView(payload)) {
    return new Uint8Array(
      payload.buffer.slice(
        payload.byteOffset,
        payload.byteOffset + payload.byteLength,
      ) as ArrayBuffer,
    );
  }
  if (Array.isArray(payload)) return Uint8Array.from(payload as number[]);
  throw new NativeError('invalid_payload', 'expected binary data from the native layer');
}

/** True when running inside the Tauri shell. */
export function isNative(): boolean {
  try {
    return isTauri();
  } catch {
    return false;
  }
}

/** Invokes a native command; rejections become `NativeError`s (also used by `host.ts`). */
export async function call<T>(cmd: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> {
  try {
    return await invoke<T>(cmd, args, options);
  } catch (err) {
    throw toNativeError(err);
  }
}

export const bridge = {
  machines: () => call<MachineInfo[]>('vca_machines'),

  pair: (uri: string, deviceName: string, totp: string | null) =>
    call<MachineInfo>('vca_pair', { uri, deviceName, totp }),

  /** The code to compare with the PC before pairing with `uri`. */
  pairingCode: (uri: string) => call<string>('vca_pairing_code', { uri }),

  /**
   * Opens (or reuses) the native link to a machine. Status updates and server
   * envelopes (msgpack bytes) arrive through the callbacks.
   */
  connect(
    machineId: string,
    onStatus: (status: LinkStatus) => void,
    onMessage: (bytes: Uint8Array<ArrayBuffer>) => void,
    onBadPayload?: (err: Error) => void,
  ): Promise<void> {
    const status = new Channel<LinkStatus>();
    status.onmessage = onStatus;
    const message = new Channel<unknown>();
    message.onmessage = (payload) => {
      let bytes: Uint8Array<ArrayBuffer>;
      try {
        bytes = toBytes(payload);
      } catch (err) {
        onBadPayload?.(err as Error);
        return;
      }
      onMessage(bytes);
    };
    return call<void>('vca_connect', { machineId, onStatus: status, onMessage: message });
  },

  send: (machineId: string, bytes: Uint8Array) =>
    call<void>('vca_send', bytes, { headers: { 'vca-machine': machineId } }),

  status: (machineId: string) => call<LinkStatus>('vca_status', { machineId }),

  login: (machineId: string, totp: string | null, backupCode: string | null) =>
    call<void>('vca_login', { machineId, totp, backupCode }),

  disconnect: (machineId: string) => call<void>('vca_disconnect', { machineId }),

  forget: (machineId: string) => call<void>('vca_forget', { machineId }),

  // Local machine management; each returns the updated machine list.
  /** Sets the local alias (`null` or blank clears it). */
  renameMachine: (machineId: string, alias: string | null) =>
    call<MachineInfo[]>('vca_machine_rename', { machineId, alias }),

  /** Opens this machine on launch (`null` clears the choice). */
  setDefaultMachine: (machineId: string | null) =>
    call<MachineInfo[]>('vca_machine_set_default', { machineId }),

  /** Moves a machine `offset` places in the list (negative: up). */
  moveMachine: (machineId: string, offset: number) =>
    call<MachineInfo[]>('vca_machine_move', { machineId, offset }),

  /** `noiseSuppression`: the shell's RNNoise stage (on when omitted). */
  micStart: (options: { noiseSuppression: boolean } = { noiseSuppression: true }) =>
    call<void>('vca_mic_start', { noiseSuppression: options.noiseSuppression }),

  async micStop(): Promise<Uint8Array<ArrayBuffer>> {
    return toBytes(await call<unknown>('vca_mic_stop'));
  },

  /**
   * WAV (16 kHz mono 16-bit) of the audio captured since the last split,
   * without stopping the recording.
   */
  async micPeek(): Promise<Uint8Array<ArrayBuffer>> {
    return toBytes(await call<unknown>('vca_mic_peek'));
  },

  /**
   * Cuts the recording buffer at the quietest 100 ms of the last few seconds
   * when at least `minSeconds` are buffered and returns the WAV of the part
   * before the cut (recording continues). Returns empty bytes when there is
   * not enough audio yet.
   */
  async micSplit(minSeconds: number): Promise<Uint8Array<ArrayBuffer>> {
    const payload = await call<unknown>('vca_mic_split', { minSeconds });
    // An empty body may surface as null on some IPC paths.
    return payload == null ? new Uint8Array(0) : toBytes(payload);
  },

  micCancel: () => call<void>('vca_mic_cancel'),

  /**
   * Debug log only: a JSON snapshot of the native capture (callback and
   * sample counters, peak level, stream errors) and the iOS audio session.
   * Contains no audio.
   */
  micDiag: () => call<string>('vca_mic_diag'),

  platform: () => call<string>('vca_platform'),

  /**
   * `tauri-plugin-biometric` (registered on phones only). `status` reports
   * whether biometrics are enrolled; `authenticate` with
   * `allowDeviceCredential` falls back to the device passcode. Rejections
   * carry the plugin's code (`userCancel`, `passcodeNotSet`, ...).
   */
  biometric: {
    status: () => call<BiometricStatus>('plugin:biometric|status'),
    authenticate: (reason: string, options: BiometricAuthOptions) =>
      call<void>('plugin:biometric|authenticate', { reason, ...options }),
  },

  dnspilot: {
    status: () => call<DnsPilotStatus>('vca_dnspilot_status'),
    identity: () => call<DnsPilotIdentity>('vca_dnspilot_identity'),
    authorizeStart: () => call<DnsPilotPrompt>('vca_dnspilot_authorize_start'),
    /** Resolves once the person approved (rejects on deny, expiry or cancel). */
    authorizeWait: () => call<DnsPilotStatus>('vca_dnspilot_authorize_wait'),
    authorizeCancel: () => call<void>('vca_dnspilot_authorize_cancel'),
    openActivation: () => call<void>('vca_dnspilot_open_activation'),
    openConsole: () => call<void>('vca_dnspilot_open_console'),
    createTunnel: (request: DnsPilotCreateTunnel) =>
      call<DnsPilotTunnel>('vca_dnspilot_create_tunnel', { request }),
    addTunnel: (hostname: string, tunnelId: number | null, publicPort?: number) =>
      call<DnsPilotTunnel>('vca_dnspilot_add_tunnel', {
        hostname,
        tunnelId,
        publicPort: publicPort ?? null,
      }),
    setTunnelId: (hostname: string, tunnelId: number) =>
      call<DnsPilotTunnel>('vca_dnspilot_set_tunnel_id', { hostname, tunnelId }),
    register: (hostname: string) =>
      call<DnsPilotRegistration>('vca_dnspilot_register', { hostname }),
    markRegistered: (hostname: string) =>
      call<DnsPilotTunnel>('vca_dnspilot_mark_registered', { hostname }),
    removeTunnel: (hostname: string) => call<void>('vca_dnspilot_remove_tunnel', { hostname }),
    issueAppToken: (hostname: string, expiresDays?: number) =>
      call<DnsPilotTunnel>('vca_dnspilot_issue_app_token', {
        hostname,
        expiresDays: expiresDays ?? null,
      }),
    disconnect: () => call<void>('vca_dnspilot_disconnect'),
    devices: () => call<DnsPilotDevice[]>('vca_dnspilot_devices'),
    /** `null` while DNSPilot does not offer the zones call. */
    zones: () => call<DnsPilotZone[] | null>('vca_dnspilot_zones'),
    remoteTunnels: () => call<DnsPilotRemoteTunnel[]>('vca_dnspilot_remote_tunnels'),
    useTunnel: (tunnelId: number) =>
      call<DnsPilotRegistration>('vca_dnspilot_use_tunnel', { tunnelId }),
    refresh: () => call<DnsPilotStatus>('vca_dnspilot_refresh'),
    /** DNSPilot device id of the Agent on this computer (desktop). */
    localDevice: () => call<number | null>('vca_dnspilot_local_device'),
    /** The platform (phones do not create tunnels). */
    platform: () => call<string>('vca_platform'),
    /**
     * Registers this app's key with the invite in a console link or a
     * `vca://pair` link; `null` when a pairing link has no invite.
     */
    enroll: (link: string) => call<DnsPilotEnrollment | null>('vca_dnspilot_enroll', { link }),
    appClients: (tunnelId: number) =>
      call<DnsPilotAppClient[]>('vca_dnspilot_app_clients', { tunnelId }),
    revokeAppClient: (tunnelId: number, clientId: number) =>
      call<void>('vca_dnspilot_revoke_app_client', { tunnelId, clientId }),
    invites: (tunnelId: number) => call<DnsPilotInvite[]>('vca_dnspilot_invites', { tunnelId }),
    revokeInvite: (tunnelId: number, inviteId: number) =>
      call<void>('vca_dnspilot_revoke_invite', { tunnelId, inviteId }),
  },
  /**
   * Launch links the native side captured itself (iOS cold start, where the
   * deep-link plugin misses the URL). Returned once, then cleared.
   */
  launchLinks: () => call<string[]>('vca_launch_links'),
};

/** The DNSPilot setup calls (a type so tests can fake them). */
export type DnsPilotService = typeof bridge.dnspilot;
