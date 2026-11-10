import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { MockTransport } from './mockTransport';
import type { ClientMsg, FoundAgent, ServerMsg } from './types';

async function setup(options: ConstructorParameters<typeof MockTransport>[0] = {}) {
  const t = new MockTransport({
    latencyMs: 0,
    autoTick: false,
    now: () => 1_700_000_000_000,
    ...options,
  });
  const pushed: ServerMsg[] = [];
  t.onMessage((e) => pushed.push(e.body));
  const connecting = t.connect();
  await vi.runAllTimersAsync();
  await connecting;
  let id = 0;
  const ask = async (body: ClientMsg) => {
    const p = t.send({ id: ++id, body });
    await vi.runAllTimersAsync();
    return p;
  };
  return { t, pushed, ask };
}

function found(msg: ServerMsg): FoundAgent[] {
  if (msg.type !== 'scan_report') throw new Error(`expected scan_report, got ${msg.type}`);
  return msg.found;
}

describe('mock session discovery', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('advertises the scan and reports every status', async () => {
    const { ask, pushed } = await setup();
    expect(await ask({ type: 'hello', protocol_version: 1, app_version: 't' })).toMatchObject({
      features: expect.arrayContaining(['scan_sessions']),
    });
    const rows = found(await ask({ type: 'scan_sessions' }));
    expect(rows.map((r) => r.status).sort()).toEqual([
      'hidden_by_user',
      'imported',
      'no_transcript_yet',
      'not_in_a_terminal',
      'unsupported_agent',
    ]);
    // Listed rows are on the session list.
    const listed = rows.filter((r) => r.session_id);
    expect(listed).toHaveLength(2);
    const sessions = await ask({ type: 'list_sessions' });
    if (sessions.type !== 'sessions') throw new Error('expected sessions');
    for (const r of listed) expect(sessions.sessions.some((s) => s.id === r.session_id)).toBe(true);
    expect(pushed.some((m) => m.type === 'session_updated')).toBe(true);
  });

  it('hides and lists again', async () => {
    const { ask, pushed } = await setup();
    const rows = found(await ask({ type: 'scan_sessions' }));
    const listed = rows.find((r) => r.status === 'imported')!;
    const hidden = found(await ask({ type: 'hide_observed', pid: listed.pid }));
    expect(hidden.find((r) => r.pid === listed.pid)).toMatchObject({
      status: 'hidden_by_user',
      session_id: null,
    });
    expect(pushed).toContainEqual({ type: 'session_removed', session_id: listed.session_id });

    const all = found(await ask({ type: 'unhide_observed', all: true }));
    expect(all.some((r) => r.status === 'hidden_by_user')).toBe(false);
    expect(all.filter((r) => r.session_id)).toHaveLength(3);

    expect(await ask({ type: 'unhide_observed', pid: listed.pid })).toMatchObject({
      type: 'error',
      code: 'not_found',
    });
  });

  it('acts as an older server when turned off', async () => {
    const { ask } = await setup({ sessionScan: false });
    const welcome = await ask({ type: 'hello', protocol_version: 1, app_version: 't' });
    expect(welcome.type === 'welcome' && welcome.features).not.toContain('scan_sessions');
    expect(await ask({ type: 'scan_sessions' })).toMatchObject({
      type: 'error',
      code: 'invalid_request',
    });
  });
});
