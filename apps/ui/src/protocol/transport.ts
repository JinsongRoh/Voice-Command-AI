import type { ClientMsg, Envelope, ServerMsg } from './types';

export type ConnectionState = 'disconnected' | 'connecting' | 'connected' | 'reconnecting';

export type Unsubscribe = () => void;

/**
 * Abstract channel to one VCA Server.
 *
 * Implementations: `MockTransport` (in-memory simulation) and, later, the
 * Tauri bridge that forwards envelopes over the Noise channel.
 *
 * Contract:
 * - `send` resolves with the response body whose envelope `id` matches the
 *   request id. Error responses resolve too (as `{ type: "error" }`); the
 *   promise rejects only on transport failures (not connected, timeout).
 * - `onMessage` delivers server-initiated messages (`id = 0`). Responses to
 *   requests are delivered only through the `send` promise.
 * - After a reconnect, previous subscriptions are gone; clients re-subscribe
 *   with `from_seq` to resume their streams.
 */
export interface Transport {
  readonly state: ConnectionState;
  connect(): Promise<void>;
  disconnect(): void;
  send(envelope: Envelope<ClientMsg>): Promise<ServerMsg>;
  onMessage(listener: (envelope: Envelope<ServerMsg>) => void): Unsubscribe;
  onStateChange(listener: (state: ConnectionState) => void): Unsubscribe;
}

/** Allocates request ids (1, 2, 3, ...; 0 is reserved for server pushes). */
export class RequestIds {
  private next = 1;

  allocate(): number {
    const id = this.next;
    this.next = this.next >= Number.MAX_SAFE_INTEGER ? 1 : this.next + 1;
    return id;
  }
}

/** Convenience wrapper that assigns envelope ids. */
export class Client {
  private readonly ids = new RequestIds();

  constructor(readonly transport: Transport) {}

  request(body: ClientMsg): Promise<ServerMsg> {
    return this.transport.send({ id: this.ids.allocate(), body });
  }
}
