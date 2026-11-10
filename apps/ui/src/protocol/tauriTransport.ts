/**
 * Transport to one paired machine through the native shell (Tauri). The Rust
 * side owns the Noise channel; this class only (de)serializes envelopes and
 * matches responses to requests.
 */
import { diag } from '../diag/log';
import { bridge, NativeError, type LinkState, type LinkStatus } from '../native/bridge';
import { decodeMsgpack, encodeMsgpack } from './codec';
import { requestIdsOf, type ConnectionState, type Transport, type Unsubscribe } from './transport';
import type { ClientMsg, Envelope, ServerMsg } from './types';

export interface TauriTransportOptions {
  /** How long `connect()` waits for the link to come up (default 20 s). */
  connectTimeoutMs?: number;
  /** Default request timeout (default 30 s). */
  requestTimeoutMs?: number;
  /**
   * A fixed timeout for `transcribe` requests. By default it is sized for
   * the audio (`transcribeTimeoutFor`).
   */
  transcribeTimeoutMs?: number;
  /**
   * A request sent after the link has been silent this long is preceded by a
   * `ping` (default `QUIET_PROBE_MS`; `0` turns the probe off).
   */
  quietProbeMs?: number;
  /**
   * With requests pending, the probe runs once nothing has arrived and the
   * oldest pending request has waited this long (default
   * `PENDING_QUIET_PROBE_MS`).
   */
  pendingQuietProbeMs?: number;
  /** How long that `ping` may take before the link counts as dead (default `PROBE_TIMEOUT_MS`). */
  probeTimeoutMs?: number;
  /** Injectable clock (tests). */
  now?: () => number;
}

/**
 * Upload rate a `transcribe` timeout allows for: 8 bytes per ms (64 kbit/s,
 * a weak mobile uplink). 16 kHz 16-bit WAV is 32 bytes per ms of speech.
 */
export const SLOW_UPLINK_BYTES_PER_MS = 8;
/** Recognition time allowed on top of the upload: a final chunk may get an LLM pass. */
export const TRANSCRIBE_BASE_MS = { final: 45_000, interim: 20_000 } as const;
/** Longest `transcribe` timeout. */
export const TRANSCRIBE_MAX_MS = 120_000;

/**
 * Timeout for a `transcribe` request, sized for its audio: a 0.5 s wake check
 * gets ~22 s, a 5 s final chunk ~65 s, a 15 s chunk ~105 s. A
 * fixed two minutes left a wake check or a preview on a dead link hanging
 * (the Voco listener handles one utterance at a time).
 */
export function transcribeTimeoutFor(body: ClientMsg): number {
  if (body.type !== 'transcribe') return TRANSCRIBE_MAX_MS;
  const base = body.final ? TRANSCRIBE_BASE_MS.final : TRANSCRIBE_BASE_MS.interim;
  return Math.min(
    TRANSCRIBE_MAX_MS,
    Math.round(base + body.audio.length / SLOW_UPLINK_BYTES_PER_MS),
  );
}

/**
 * Silence after which the next request first checks the link. A phone that
 * slept or changed networks keeps a dead link "connected" until the
 * heartbeat gives up (up to a minute): a dictation sent then was lost.
 */
export const QUIET_PROBE_MS = 30_000;
/**
 * The same check while requests are pending: continuous dictation always has
 * one in flight, and a half-open link that swallows them all must not look
 * "busy" forever. A slow server still answers the `ping` at once.
 */
export const PENDING_QUIET_PROBE_MS = 18_000;
/** A `pong` slower than this means the link is dead: it is reopened. */
export const PROBE_TIMEOUT_MS = 8_000;

interface Pending {
  resolve(msg: ServerMsg): void;
  reject(err: Error): void;
  timer: ReturnType<typeof setTimeout>;
  /** When the request was registered. */
  at: number;
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
  private readonly transcribeTimeoutMs: number | null;
  private readonly quietProbeMs: number;
  private readonly pendingQuietProbeMs: number;
  private readonly probeTimeoutMs: number;
  private readonly now: () => number;
  /** When the link last delivered something (or came up). */
  private lastRxAt = 0;
  /** The running liveness probe, sent ahead of the request that started it. */
  private probe: Promise<void> | null = null;

  constructor(
    readonly machineId: string,
    options: TauriTransportOptions = {},
  ) {
    this.connectTimeoutMs = options.connectTimeoutMs ?? 20_000;
    this.requestTimeoutMs = options.requestTimeoutMs ?? 30_000;
    this.transcribeTimeoutMs = options.transcribeTimeoutMs ?? null;
    this.quietProbeMs = options.quietProbeMs ?? QUIET_PROBE_MS;
    this.pendingQuietProbeMs = options.pendingQuietProbeMs ?? PENDING_QUIET_PROBE_MS;
    this.probeTimeoutMs = options.probeTimeoutMs ?? PROBE_TIMEOUT_MS;
    this.now = options.now ?? Date.now;
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
    this.open(generation);
    return promise;
  }

  /** Opens the native link for `generation` once the previous one is closed. */
  private open(generation: number): void {
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
  }

  /**
   * Replaces a link that stopped answering with a fresh one. Its pending
   * requests fail now (callers may retry once it is back) instead of waiting
   * for the heartbeat; listeners see `reconnecting`, then `connected`.
   */
  private recycle(): void {
    diag('link.recycle', { pending: this.pending.size });
    const generation = ++this.generation;
    this.probe = null;
    this.rejectAll(new Error('connection lost (no answer)'));
    this.setState('reconnecting');
    this.disconnecting = bridge.disconnect(this.machineId).catch(() => {});
    this.open(generation);
  }

  /**
   * After a quiet spell, a `ping` goes out ahead of the next request. No
   * `pong` within `probeTimeoutMs` (while nothing else arrived either) means
   * the link is dead: it is recycled.
   */
  private probeIfQuiet(): Promise<void> | null {
    if (this.quietProbeMs <= 0 || this.probe) return this.probe;
    const now = this.now();
    const quietMs = now - this.lastRxAt;
    if (this.pending.size === 0) {
      if (quietMs < this.quietProbeMs) return null;
    } else {
      // A request that only just went out may simply not be answered yet;
      // one unanswered this long, with nothing else heard, points at the link.
      let oldest = now;
      for (const p of this.pending.values()) oldest = Math.min(oldest, p.at);
      if (now - Math.max(this.lastRxAt, oldest) < this.pendingQuietProbeMs) return null;
    }
    const generation = this.generation;
    const sentAt = this.now();
    const ping: Envelope<ClientMsg> = {
      id: requestIdsOf(this).allocate(),
      body: { type: 'ping', at: sentAt },
    };
    const sent = this.dispatch(ping, this.probeTimeoutMs);
    const done = sent.reply.then(
      () => diag('link.probe.ok', { quietMs, ms: this.now() - sentAt }),
      (err: Error) => {
        if (generation !== this.generation || this._state !== 'connected') return;
        // Anything heard meanwhile (e.g. a slow server answered something
        // else) shows the link itself is alive.
        if (this.lastRxAt > sentAt) return;
        diag('link.probe.dead', { quietMs, error: err.message });
        this.recycle();
      },
    );
    // The request waits only until the ping is handed to the link (so it goes
    // first), not for the pong.
    const handedOver = sent.handedOver;
    this.probe = handedOver;
    void done.finally(() => {
      if (this.probe === handedOver) this.probe = null;
    });
    return handedOver;
  }

  disconnect(): void {
    this.generation += 1;
    this.probe = null;
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
    const timeoutMs =
      envelope.body.type === 'transcribe'
        ? (this.transcribeTimeoutMs ?? transcribeTimeoutFor(envelope.body))
        : this.requestTimeoutMs;
    const probe = envelope.body.type === 'ping' ? null : this.probeIfQuiet();
    return this.dispatch(envelope, timeoutMs, probe).reply;
  }

  /**
   * Registers `envelope` as pending and hands it to the native link (after
   * `after`, when given). `handedOver` settles once the link took it.
   */
  private dispatch(
    envelope: Envelope<ClientMsg>,
    timeoutMs: number,
    after: Promise<void> | null = null,
  ): { reply: Promise<ServerMsg>; handedOver: Promise<void> } {
    let bytes: Uint8Array;
    try {
      bytes = encodeMsgpack(envelope);
    } catch (err) {
      return { reply: Promise.reject(err as Error), handedOver: Promise.resolve() };
    }
    let handedOver: Promise<void> = Promise.resolve();
    const generation = this.generation;
    const reply = new Promise<ServerMsg>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(envelope.id);
        reject(new Error(`request timed out (${envelope.body.type})`));
        // An unanswered recognition is the symptom of a link that died under
        // it (a network change). The link is replaced at once, so the caller
        // sees `reconnecting` and may send a final chunk once more.
        if (
          envelope.body.type === 'transcribe' &&
          generation === this.generation &&
          this._state === 'connected'
        ) {
          diag('link.transcribe.timeout', { timeoutMs });
          this.recycle();
        }
      }, timeoutMs);
      this.pending.set(envelope.id, { resolve, reject, timer, at: this.now() });
      const fail = (err: Error) => {
        const entry = this.pending.get(envelope.id);
        if (!entry) return;
        clearTimeout(entry.timer);
        this.pending.delete(envelope.id);
        reject(err);
      };
      const hand = () => {
        // Failed or answered meanwhile (e.g. the link was recycled).
        if (!this.pending.has(envelope.id)) return Promise.resolve();
        return bridge.send(this.machineId, bytes).then(() => undefined, fail);
      };
      handedOver = after ? after.then(hand, hand) : hand();
    });
    return { reply, handedOver };
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
      this.lastRxAt = this.now();
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
    this.lastRxAt = this.now();
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
    diag('link.state', { from: this._state, to: state, pending: this.pending.size });
    this._state = state;
    for (const l of [...this.stateListeners]) l(state);
  }
}
