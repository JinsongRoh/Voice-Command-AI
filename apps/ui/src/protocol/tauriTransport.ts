/**
 * Transport to one paired machine through the native shell (Tauri). The Rust
 * side owns the Noise channel; this class only (de)serializes envelopes and
 * matches responses to requests.
 */
import { bridge, NativeError, type LinkState, type LinkStatus } from '../native/bridge';
import { decodeMsgpack, encodeMsgpack } from './codec';
import type { ConnectionState, Transport, Unsubscribe } from './transport';
import type { ClientMsg, Envelope, ServerMsg } from './types';

export interface TauriTransportOptions {
  /** How long `connect()` waits for the link to come up (default 20 s). */
  connectTimeoutMs?: number;
  /** Default request timeout (default 30 s). */
  requestTimeoutMs?: number;
  /** Timeout for `transcribe` requests, which upload audio (default 120 s). */
  transcribeTimeoutMs?: number;
}

interface Pending {
  resolve(msg: ServerMsg): void;
  reject(err: Error): void;
  timer: ReturnType<typeof setTimeout>;
}

interface Waiter {
  resolve(): void;
  reject(err: Error): void;
  timer: ReturnType<typeof setTimeout>;
}

export function connectionStateOf(state: LinkState): ConnectionState {
  return state === 'login_required' ? 'disconnected' : state;
}

export class TauriTransport implements Transport {
  private _state: ConnectionState = 'disconnected';
  private _status: LinkStatus | null = null;
  private readonly pending = new Map<number, Pending>();
  private readonly listeners = new Set<(e: Envelope<ServerMsg>) => void>();
  private readonly stateListeners = new Set<(s: ConnectionState) => void>();
  private readonly statusListeners = new Set<(s: LinkStatus) => void>();
  private waiter: Waiter | null = null;
  private connecting: Promise<void> | null = null;
  private disconnecting: Promise<void> = Promise.resolve();
  /** Incremented on every connect/disconnect; callbacks of older links are ignored. */
  private generation = 0;
  private readonly connectTimeoutMs: number;
  private readonly requestTimeoutMs: number;
  private readonly transcribeTimeoutMs: number;

  constructor(
    readonly machineId: string,
    options: TauriTransportOptions = {},
  ) {
    this.connectTimeoutMs = options.connectTimeoutMs ?? 20_000;
    this.requestTimeoutMs = options.requestTimeoutMs ?? 30_000;
    this.transcribeTimeoutMs = options.transcribeTimeoutMs ?? 120_000;
  }

  get state(): ConnectionState {
    return this._state;
  }

  /** Last native link status (carries `login_required`, error and code). */
  get status(): LinkStatus | null {
    return this._status;
  }

  get loginRequired(): boolean {
    return this._status?.state === 'login_required';
  }

  connect(): Promise<void> {
    if (this._state === 'connected') return Promise.resolve();
    if (this.connecting) return this.connecting;
    const generation = ++this.generation;
    this.setState('connecting');
    const promise = new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        // Give up on this attempt entirely; a late status must not revive it.
        this.failWaiter(new NativeError('timeout', 'connection timed out'));
        this.disconnect();
      }, this.connectTimeoutMs);
      this.waiter = { resolve, reject, timer };
    });
    this.connecting = promise;
    const settle = () => {
      if (this.connecting === promise) this.connecting = null;
    };
    promise.then(settle, settle);

    void this.disconnecting.then(() => {
      if (generation !== this.generation) return;
      bridge
        .connect(
          this.machineId,
          (status) => {
            if (generation === this.generation) this.handleStatus(status);
          },
          (bytes) => {
            if (generation === this.generation) this.handleBytes(bytes);
          },
        )
        .catch((err: NativeError) => {
          if (generation !== this.generation) return;
          this.failWaiter(err);
          if (this._state !== 'connected') this.setState('disconnected');
        });
    });
    return promise;
  }

  disconnect(): void {
    this.generation += 1;
    this.failWaiter(new NativeError('disconnected', 'disconnected'));
    this.rejectAll(new Error('disconnected'));
    this.setState('disconnected');
    this.disconnecting = bridge.disconnect(this.machineId).catch(() => {});
  }

  send(envelope: Envelope<ClientMsg>): Promise<ServerMsg> {
    if (this._state !== 'connected') return Promise.reject(new Error('not connected'));
    if (this.pending.has(envelope.id)) {
      return Promise.reject(new Error(`request id ${envelope.id} is already pending`));
    }
    let bytes: Uint8Array;
    try {
      bytes = encodeMsgpack(envelope);
    } catch (err) {
      return Promise.reject(err as Error);
    }
    const timeoutMs =
      envelope.body.type === 'transcribe' ? this.transcribeTimeoutMs : this.requestTimeoutMs;
    return new Promise<ServerMsg>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(envelope.id);
        reject(new Error(`request timed out (${envelope.body.type})`));
      }, timeoutMs);
      this.pending.set(envelope.id, { resolve, reject, timer });
      bridge.send(this.machineId, bytes).catch((err: Error) => {
        const entry = this.pending.get(envelope.id);
        if (!entry) return;
        clearTimeout(entry.timer);
        this.pending.delete(envelope.id);
        reject(err);
      });
    });
  }

  onMessage(listener: (envelope: Envelope<ServerMsg>) => void): Unsubscribe {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  onStateChange(listener: (state: ConnectionState) => void): Unsubscribe {
    this.stateListeners.add(listener);
    return () => this.stateListeners.delete(listener);
  }

  /** Observes the raw native link status (e.g. to show the login prompt). */
  onStatus(listener: (status: LinkStatus) => void): Unsubscribe {
    this.statusListeners.add(listener);
    return () => this.statusListeners.delete(listener);
  }

  private handleStatus(status: LinkStatus): void {
    this._status = status;
    for (const l of [...this.statusListeners]) l(status);
    const next = connectionStateOf(status.state);
    if (next !== 'connected') this.rejectAll(new Error(status.error ?? status.state));
    this.setState(next);
    if (status.state === 'connected') {
      const waiter = this.waiter;
      if (waiter) {
        clearTimeout(waiter.timer);
        this.waiter = null;
        waiter.resolve();
      }
    } else if (status.state === 'login_required' || status.state === 'disconnected') {
      this.failWaiter(
        new NativeError(
          status.code ?? status.state,
          status.error ?? status.state.replace('_', ' '),
        ),
      );
    }
  }

  private handleBytes(bytes: Uint8Array): void {
    let envelope: Envelope<ServerMsg>;
    try {
      envelope = decodeMsgpack(bytes);
    } catch (err) {
      console.warn('vca: dropping undecodable server message', err);
      return;
    }
    if (envelope.id === 0) {
      for (const l of [...this.listeners]) l(envelope);
      return;
    }
    const entry = this.pending.get(envelope.id);
    if (!entry) return;
    clearTimeout(entry.timer);
    this.pending.delete(envelope.id);
    entry.resolve(envelope.body);
  }

  private failWaiter(err: Error): void {
    const waiter = this.waiter;
    if (!waiter) return;
    clearTimeout(waiter.timer);
    this.waiter = null;
    waiter.reject(err);
  }

  private rejectAll(err: Error): void {
    const entries = [...this.pending.values()];
    this.pending.clear();
    for (const entry of entries) {
      clearTimeout(entry.timer);
      entry.reject(err);
    }
  }

  private setState(state: ConnectionState): void {
    if (state === this._state) return;
    this._state = state;
    for (const l of [...this.stateListeners]) l(state);
  }
}
