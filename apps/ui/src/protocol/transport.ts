import type { ClientMsg, Envelope, ServerMsg } from './types';

export type ConnectionState = 'disconnected' | 'connecting' | 'connected' | 'reconnecting';

export type Unsubscribe = () => void;

/**
 * Abstract channel to one VCA Server.
 *
 * Implementations: `MockTransport` (in-memory simulation) and
 * `TauriTransport` (the native shell forwards envelopes over the Noise channel).
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

/** One id sequence per transport, so that every `Client` on it allocates unique ids. */
const idsByTransport = new WeakMap<Transport, RequestIds>();

/**
 * Convenience wrapper that assigns envelope ids. Clients created for the same
 * transport share the id sequence, so their requests never collide.
 */
export class Client {
  private readonly ids: RequestIds;

  constructor(readonly transport: Transport) {
    let ids = idsByTransport.get(transport);
    if (!ids) {
      ids = new RequestIds();
      idsByTransport.set(transport, ids);
    }
    this.ids = ids;
  }

  request(body: ClientMsg): Promise<ServerMsg> {
    return this.transport.send({ id: this.ids.allocate(), body });
  }
}
