/**
 * Fake `@tauri-apps/api/core` for tests. Use with:
 *
 *   vi.mock('@tauri-apps/api/core', () => import('../test/tauriMock'));
 *
 * Commands are answered by handlers registered in `tauri.handlers`; channels
 * passed to `invoke` are `FakeChannel`s whose `onmessage` the test can drive.
 */
import { decode, encode } from '@msgpack/msgpack';
import { vi } from 'vitest';

import type { LinkStatus } from '../native/bridge';
import type { ClientMsg, Envelope, ServerMsg } from '../protocol/types';
import { defaultResponse } from './fakeTransport';

export class FakeChannel<T = unknown> {
  onmessage: (message: T) => void;
  constructor(onmessage?: (message: T) => void) {
    this.onmessage = onmessage ?? (() => {});
  }
}

export type Handler = (args: unknown, options: unknown) => unknown;

export interface Call {
  cmd: string;
  args: unknown;
  options: unknown;
}

export const tauri = {
  handlers: new Map<string, Handler>(),
  calls: [] as Call[],
  native: true,
  reset() {
    this.handlers.clear();
    this.calls.length = 0;
    this.native = true;
  },
  callsOf(cmd: string) {
    return this.calls.filter((c) => c.cmd === cmd);
  },
};

export const invoke = vi.fn(async (cmd: string, args?: unknown, options?: unknown) => {
  tauri.calls.push({ cmd, args, options });
  const handler = tauri.handlers.get(cmd);
  if (!handler) throw `unhandled command ${cmd}`;
  return handler(args, options);
});

export const isTauri = () => tauri.native;

export { FakeChannel as Channel };

/** Channels of the latest `vca_connect` call for a machine. */
export interface Link {
  status: FakeChannel<LinkStatus>;
  message: FakeChannel<unknown>;
}

export function status(state: LinkStatus['state'], code: string | null = null): LinkStatus {
  return { state, error: code ? `error ${code}` : null, code };
}

export function serverBytes(envelope: Envelope<ServerMsg>): ArrayBuffer {
  const bytes = encode(envelope);
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

export function decodeClient(bytes: unknown): Envelope<ClientMsg> {
  return decode(bytes as Uint8Array) as Envelope<ClientMsg>;
}

/**
 * Simulated native side for full-app tests: `vca_connect` reports the given
 * status (default `connected`) and `vca_send` answers with `respond`.
 */
export function fakeServer({
  onConnect = () => status('connected'),
  respond = defaultResponse,
}: {
  onConnect?: (machineId: string) => LinkStatus;
  respond?: (msg: ClientMsg) => ServerMsg;
} = {}) {
  const links = new Map<string, Link>();
  tauri.handlers.set('vca_connect', (args) => {
    const { machineId, onStatus, onMessage } = args as {
      machineId: string;
      onStatus: FakeChannel<LinkStatus>;
      onMessage: FakeChannel<unknown>;
    };
    links.set(machineId, { status: onStatus, message: onMessage });
    const s = onConnect(machineId);
    queueMicrotask(() => onStatus.onmessage(s));
    return undefined;
  });
  tauri.handlers.set('vca_send', (args, options) => {
    const machineId = (options as { headers: Record<string, string> }).headers['vca-machine']!;
    const envelope = decodeClient(args);
    const link = links.get(machineId);
    const reply = respond(envelope.body);
    queueMicrotask(() => link?.message.onmessage(serverBytes({ id: envelope.id, body: reply })));
    return undefined;
  });
  tauri.handlers.set('vca_disconnect', () => undefined);
  return links;
}
