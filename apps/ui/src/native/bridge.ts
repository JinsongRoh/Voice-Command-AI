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
}

export type LinkState =
  'disconnected' | 'connecting' | 'connected' | 'reconnecting' | 'login_required';

export interface LinkStatus {
  state: LinkState;
  error: string | null;
  code: string | null;
}

export type NativePlatform = 'windows' | 'macos' | 'linux' | 'android' | 'ios';

/** A rejected native command, normalized from a string or `{ code, message }`. */
export class NativeError extends Error {
  constructor(
    readonly code: string | null,
    message: string,
  ) {
    super(message);
    this.name = 'NativeError';
  }
}

export function toNativeError(err: unknown): NativeError {
  if (err instanceof NativeError) return err;
  if (typeof err === 'string') return new NativeError(null, err);
  if (typeof err === 'object' && err !== null) {
    const { code, message } = err as { code?: unknown; message?: unknown };
    const text =
      typeof message === 'string' ? message : err instanceof Error ? err.message : String(err);
    return new NativeError(typeof code === 'string' ? code : null, text);
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

async function call<T>(cmd: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> {
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

  micStart: () => call<void>('vca_mic_start'),

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

  platform: () => call<string>('vca_platform'),
};
