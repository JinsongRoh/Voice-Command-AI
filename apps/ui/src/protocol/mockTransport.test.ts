import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { WireServerMsg } from './codec';
import { MockTransport } from './mockTransport';
import type { SessionEvent } from './types';

function setup() {
  const t = new MockTransport({ latencyMs: 0, autoTick: false, now: () => 1_700_000_000_000 });
  const pushed: WireServerMsg[] = [];
  t.onMessage((e) => {
    expect(e.id).toBe(0);
    pushed.push(e.body);
  });
  let id = 0;
  const req = (body: Parameters<MockTransport['send']>[0]['body']) => t.send({ id: ++id, body });
  return { t, pushed, req };
}

const events = (pushed: WireServerMsg[], sid: string): SessionEvent[] =>
  pushed.flatMap((m) => (m.type === 'event' && m.session_id === sid ? [m.event] : []));

describe('MockTransport', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('handshakes and lists the seeded sessions', async () => {
    const { t, req } = setup();
    const connecting = t.connect();
    await vi.runAllTimersAsync();
    await connecting;
    expect(t.state).toBe('connected');
    const welcome = req({ type: 'hello', protocol_version: 1, app_version: 'test' });
    await vi.runAllTimersAsync();
    expect(await welcome).toMatchObject({ type: 'welcome', server_name: 'workstation' });
    const list = req({ type: 'list_sessions' });
    await vi.runAllTimersAsync();
    const res = await list;
    if (res.type !== 'sessions') throw new Error('expected sessions');
    expect(res.sessions).toHaveLength(6);
    expect(res.sessions.filter((s) => s.origin.type === 'cloud')).toHaveLength(2);
    expect(res.sessions.find((s) => s.id === 'wk-claude-api')?.state.type).toBe(
      'awaiting_decision',
    );
  });

  it('rejects requests while disconnected', async () => {
    const { req } = setup();
    await expect(req({ type: 'list_sessions' })).rejects.toThrow('not connected');
  });

  it('replays history on subscribe, then streams with increasing seq, and resumes from_seq', async () => {
    const { t, pushed, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    req({ type: 'subscribe', session_id: 'wk-codex-web', from_seq: null });
    await vi.runAllTimersAsync();
    const replay = events(pushed, 'wk-codex-web');
    expect(replay.length).toBeGreaterThan(0);
    expect(replay.map((e) => e.seq)).toEqual(replay.map((_, i) => i + 1));
    t.tick();
    t.tick();
    const all = events(pushed, 'wk-codex-web');
    expect(all.length).toBeGreaterThan(replay.length);
    const last = all[all.length - 1]!.seq;

    // Drop the connection; events keep being produced while away.
    t.simulateDrop(1000);
    expect(t.state).toBe('reconnecting');
    await vi.advanceTimersByTimeAsync(1000);
    expect(t.state).toBe('connected');
    pushed.length = 0;
    req({ type: 'subscribe', session_id: 'wk-codex-web', from_seq: last });
    await vi.runAllTimersAsync();
    const resumed = events(pushed, 'wk-codex-web');
    expect(resumed.length).toBeGreaterThan(0);
    expect(resumed[0]!.seq).toBe(last + 1);
    expect(pushed.some((m) => m.type === 'stream_gap')).toBe(false);
  });

  it('sends stream_gap when the requested events are no longer buffered', async () => {
    const { t, pushed, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    t.simulateDrop(500, true);
    await vi.advanceTimersByTimeAsync(500);
    req({ type: 'subscribe', session_id: 'wk-codex-web', from_seq: 1 });
    await vi.runAllTimersAsync();
    const gap = pushed.find((m) => m.type === 'stream_gap');
    expect(gap).toMatchObject({ type: 'stream_gap', session_id: 'wk-codex-web' });
    const replay = events(pushed, 'wk-codex-web');
    if (gap?.type !== 'stream_gap') throw new Error('no gap');
    expect(replay[0]!.seq).toBe(gap.first_available_seq);
  });

  it('answers a pending decision and resumes the session', async () => {
    const { t, pushed, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    const wrong = req({
      type: 'send_input',
      session_id: 'wk-claude-api',
      input: { type: 'choose', decision_id: 'nope', option_id: 'yes' },
    });
    await vi.runAllTimersAsync();
    expect(await wrong).toMatchObject({ type: 'error', code: 'not_found' });
    const ok = req({
      type: 'send_input',
      session_id: 'wk-claude-api',
      input: { type: 'choose', decision_id: 'dec-api-1', option_id: 'yes' },
    });
    await vi.runAllTimersAsync();
    expect(await ok).toEqual({
      type: 'input_ack',
      session_id: 'wk-claude-api',
      status: 'delivered',
    });
    const update = pushed.find(
      (m) => m.type === 'session_updated' && m.session.id === 'wk-claude-api',
    );
    expect(update).toMatchObject({ session: { state: { type: 'running' } } });
  });

  it('queues text for a busy session and acknowledges delivery later', async () => {
    const { t, pushed, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    const res = req({
      type: 'send_input',
      session_id: 'wk-codex-web',
      input: { type: 'text', text: 'also update the docs', submit: true },
    });
    await vi.runAllTimersAsync();
    expect(await res).toMatchObject({ type: 'input_ack', status: 'queued' });
    t.tick();
    expect(pushed).toContainEqual({
      type: 'input_ack',
      session_id: 'wk-codex-web',
      status: 'delivered',
    });
  });

  it('delivers text immediately to a session waiting for input', async () => {
    const { t, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    const res = req({
      type: 'send_input',
      session_id: 'wk-gemini-docs',
      input: { type: 'text', text: 'open a PR', submit: true },
    });
    await vi.runAllTimersAsync();
    expect(await res).toMatchObject({ type: 'input_ack', status: 'delivered' });
  });

  it('reorders and cancels queued jobs', async () => {
    const { t, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    req({ type: 'reorder_queue', job_id: 'job-103', position: 0 });
    await vi.runAllTimersAsync();
    let snap = req({ type: 'get_resources' });
    await vi.runAllTimersAsync();
    let res = await snap;
    if (res.type !== 'resources') throw new Error('expected resources');
    expect(res.snapshot.queue.map((q) => q.job_id)).toEqual(['job-103', 'job-101', 'job-102']);
    expect(res.snapshot.queue.map((q) => q.position)).toEqual([0, 1, 2]);
    req({ type: 'cancel_queued', job_id: 'job-101' });
    await vi.runAllTimersAsync();
    snap = req({ type: 'get_resources' });
    await vi.runAllTimersAsync();
    res = await snap;
    if (res.type !== 'resources') throw new Error('expected resources');
    expect(res.snapshot.queue.map((q) => q.job_id)).toEqual(['job-103', 'job-102']);
  });

  it('launches a local session that starts and asks for input', async () => {
    const { t, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    const res = req({
      type: 'launch',
      request: {
        agent: { type: 'aider' },
        project_dir: '~/code/new-thing',
        create_dir: true,
        profile: null,
        args: [],
        display_name: null,
      },
    });
    await vi.runAllTimersAsync();
    const launched = await res;
    if (launched.type !== 'launched') throw new Error('expected launched');
    expect(launched.session.display_name).toBe('new-thing');
    expect(launched.session.state.type).toBe('starting');
    for (let i = 0; i < 6; i++) t.tick();
    const list = req({ type: 'list_sessions' });
    await vi.runAllTimersAsync();
    const after = await list;
    if (after.type !== 'sessions') throw new Error('expected sessions');
    expect(after.sessions.find((s) => s.id === launched.session.id)?.state.type).toBe(
      'awaiting_input',
    );
  });

  it('kill switch drops the connection', async () => {
    const { t, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    req({ type: 'kill_switch' });
    await vi.runAllTimersAsync();
    expect(t.state).toBe('disconnected');
  });
});
