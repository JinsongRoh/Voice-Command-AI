import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { NativeError, type LinkStatus } from '../native/bridge';
import {
  decodeClient,
  serverBytes,
  status,
  tauri,
  type FakeChannel,
  type Link,
} from '../test/tauriMock';
import { TauriTransport } from './tauriTransport';
import type { ConnectionState } from './transport';

vi.mock('@tauri-apps/api/core', () => import('../test/tauriMock'));

/** Records `vca_connect` channels without answering; the test drives them. */
function captureLinks() {
  const links: Link[] = [];
  tauri.handlers.set('vca_connect', (args) => {
    const { onStatus, onMessage } = args as {
      onStatus: FakeChannel<LinkStatus>;
      onMessage: FakeChannel<unknown>;
    };
    links.push({ status: onStatus, message: onMessage });
    return undefined;
  });
  tauri.handlers.set('vca_send', () => undefined);
  tauri.handlers.set('vca_disconnect', () => undefined);
  return links;
}

const flush = () => vi.advanceTimersByTimeAsync(0);

async function connected(t: TauriTransport, links: Link[]) {
  const p = t.connect();
  await flush();
  links.at(-1)!.status.onmessage(status('connected'));
  await p;
  return links.at(-1)!;
}

describe('TauriTransport', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    tauri.reset();
  });
  afterEach(() => vi.useRealTimers());

  it('connects through vca_connect and maps link states', async () => {
    const links = captureLinks();
    const t = new TauriTransport('m1');
    const states: ConnectionState[] = [];
    t.onStateChange((s) => states.push(s));
    const p = t.connect();
    expect(t.state).toBe('connecting');
    await flush();
    expect(tauri.callsOf('vca_connect')[0]!.args).toMatchObject({ machineId: 'm1' });
    links[0]!.status.onmessage(status('connecting'));
    links[0]!.status.onmessage(status('connected'));
    await expect(p).resolves.toBeUndefined();
    expect(t.state).toBe('connected');
    links[0]!.status.onmessage(status('reconnecting'));
    expect(t.state).toBe('reconnecting');
    expect(states).toEqual(['connecting', 'connected', 'reconnecting']);
  });

  it('rejects connect on login_required and exposes the status', async () => {
    const links = captureLinks();
    const t = new TauriTransport('m1');
    const seen: LinkStatus[] = [];
    t.onStatus((s) => seen.push(s));
    const p = t.connect();
    await flush();
    links[0]!.status.onmessage(status('login_required', 'token_expired'));
    await expect(p).rejects.toMatchObject({ name: 'NativeError', code: 'token_expired' });
    expect(t.state).toBe('disconnected');
    expect(t.loginRequired).toBe(true);
    expect(t.status?.state).toBe('login_required');
    expect(seen).toHaveLength(1);
  });

  it('rejects connect when the link reports disconnected or the command fails', async () => {
    const links = captureLinks();
    const t = new TauriTransport('m1');
    const p = t.connect();
    await flush();
    links[0]!.status.onmessage(status('disconnected', 'unreachable'));
    await expect(p).rejects.toMatchObject({ code: 'unreachable' });

    tauri.handlers.set('vca_connect', () => {
      throw { code: 'unknown_machine', message: 'no such machine' };
    });
    const p2 = t.connect();
    const assertion = expect(p2).rejects.toEqual(
      new NativeError('unknown_machine', 'no such machine'),
    );
    await flush();
    await assertion;
    expect(t.state).toBe('disconnected');
  });

  it('times out when the link never comes up', async () => {
    captureLinks();
    const t = new TauriTransport('m1', { connectTimeoutMs: 1000 });
    const p = t.connect();
    const assertion = expect(p).rejects.toMatchObject({ code: 'timeout' });
    await vi.advanceTimersByTimeAsync(1000);
    await assertion;
    expect(t.state).toBe('disconnected');
    expect(tauri.callsOf('vca_disconnect')).toHaveLength(1);
  });

  it('matches responses to requests by envelope id', async () => {
    const links = captureLinks();
    const t = new TauriTransport('m1');
    const link = await connected(t, links);

    const a = t.send({ id: 7, body: { type: 'list_sessions' } });
    const b = t.send({ id: 8, body: { type: 'get_resources' } });
    await flush();
    const sends = tauri.callsOf('vca_send');
    expect(sends).toHaveLength(2);
    expect(sends[0]!.args).toBeInstanceOf(Uint8Array);
    expect(sends[0]!.options).toEqual({ headers: { 'vca-machine': 'm1' } });
    expect(decodeClient(sends[0]!.args)).toEqual({ id: 7, body: { type: 'list_sessions' } });

    // Out of order, and in the three payload shapes the native layer may use.
    link.message.onmessage(
      new Uint8Array(
        serverBytes({ id: 8, body: { type: 'error', code: 'forbidden', message: 'no' } }),
      ),
    );
    link.message.onmessage(
      Array.from(new Uint8Array(serverBytes({ id: 7, body: { type: 'sessions', sessions: [] } }))),
    );
    // Error responses resolve (Transport contract).
    await expect(b).resolves.toEqual({ type: 'error', code: 'forbidden', message: 'no' });
    await expect(a).resolves.toEqual({ type: 'sessions', sessions: [] });
  });

  it('delivers server pushes (id 0) to onMessage listeners only', async () => {
    const links = captureLinks();
    const t = new TauriTransport('m1');
    const link = await connected(t, links);
    const pushes: unknown[] = [];
    t.onMessage((e) => pushes.push(e));
    link.message.onmessage(
      serverBytes({ id: 0, body: { type: 'session_removed', session_id: 's' } }),
    );
    link.message.onmessage(serverBytes({ id: 99, body: { type: 'ok' } })); // unknown id: dropped
    expect(pushes).toEqual([{ id: 0, body: { type: 'session_removed', session_id: 's' } }]);
  });

  it('rejects requests on timeout, with a longer budget for transcribe', async () => {
    const links = captureLinks();
    const t = new TauriTransport('m1', { requestTimeoutMs: 1000, transcribeTimeoutMs: 5000 });
    await connected(t, links);
    const req = t.send({ id: 1, body: { type: 'list_sessions' } });
    const stt = t.send({
      id: 2,
      body: {
        type: 'transcribe',
        audio: new Uint8Array([1, 2]),
        format: 'wav',
        sample_rate: null,
        language: null,
        session_id: null,
      },
    });
    let sttDone = false;
    stt.catch(() => (sttDone = true));
    const reqAssertion = expect(req).rejects.toThrow(/timed out/);
    await vi.advanceTimersByTimeAsync(1000);
    await reqAssertion;
    expect(sttDone).toBe(false);
    await vi.advanceTimersByTimeAsync(4000);
    await expect(stt).rejects.toThrow(/timed out/);
  });

  it('rejects pending requests on disconnect and when the link drops', async () => {
    const links = captureLinks();
    const t = new TauriTransport('m1');
    const link = await connected(t, links);
    const a = t.send({ id: 1, body: { type: 'list_sessions' } });
    link.status.onmessage(status('reconnecting'));
    await expect(a).rejects.toThrow();

    link.status.onmessage(status('connected'));
    const b = t.send({ id: 2, body: { type: 'list_sessions' } });
    t.disconnect();
    await expect(b).rejects.toThrow('disconnected');
    expect(t.state).toBe('disconnected');
    await flush();
    expect(tauri.callsOf('vca_disconnect')[0]!.args).toEqual({ machineId: 'm1' });
    // Messages from the old link are ignored after disconnect.
    link.status.onmessage(status('connected'));
    expect(t.state).toBe('disconnected');
  });

  it('refuses to send while not connected and surfaces vca_send failures', async () => {
    const links = captureLinks();
    const t = new TauriTransport('m1');
    await expect(t.send({ id: 1, body: { type: 'list_sessions' } })).rejects.toThrow(
      'not connected',
    );
    await connected(t, links);
    tauri.handlers.set('vca_send', () => {
      throw 'link closed';
    });
    await expect(t.send({ id: 2, body: { type: 'list_sessions' } })).rejects.toThrow('link closed');
  });
});
