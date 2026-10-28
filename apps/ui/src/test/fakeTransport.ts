import type { ClientMsg, Envelope, ServerMsg } from '../protocol/types';
import type { ConnectionState, Transport } from '../protocol/transport';

/** Scriptable transport for component tests: records requests, answers synchronously. */
export class FakeTransport implements Transport {
  state: ConnectionState = 'disconnected';
  readonly sent: Envelope<ClientMsg>[] = [];
  private readonly listeners = new Set<(e: Envelope<ServerMsg>) => void>();
  private readonly stateListeners = new Set<(s: ConnectionState) => void>();

  constructor(private readonly respond: (msg: ClientMsg) => ServerMsg = defaultResponse) {}

  async connect() {
    this.setState('connected');
  }
  disconnect() {
    this.setState('disconnected');
  }
  async send(envelope: Envelope<ClientMsg>) {
    this.sent.push(envelope);
    return this.respond(envelope.body);
  }
  onMessage(l: (e: Envelope<ServerMsg>) => void) {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  }
  onStateChange(l: (s: ConnectionState) => void) {
    this.stateListeners.add(l);
    return () => this.stateListeners.delete(l);
  }
  push(body: ServerMsg) {
    for (const l of this.listeners) l({ id: 0, body });
  }
  bodies(type: ClientMsg['type']) {
    return this.sent.map((e) => e.body).filter((b) => b.type === type);
  }
  private setState(s: ConnectionState) {
    this.state = s;
    for (const l of this.stateListeners) l(s);
  }
}

export function defaultResponse(msg: ClientMsg): ServerMsg {
  switch (msg.type) {
    case 'hello':
      return { type: 'welcome', protocol_version: 1, server_version: 'test', server_name: 'test' };
    case 'list_sessions':
      return { type: 'sessions', sessions: [] };
    case 'send_input':
      return { type: 'input_ack', session_id: msg.session_id, status: 'delivered' };
    case 'list_devices':
      return { type: 'error', code: 'invalid_request', message: 'unknown message' };
    default:
      return { type: 'ok' };
  }
}
