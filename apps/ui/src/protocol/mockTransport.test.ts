import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ServerMsg } from './types';
import { MOCK_PAGE_EVENTS, MockTransport } from './mockTransport';
import type { SessionEvent } from './types';

function setup() {
  const t = new MockTransport({ latencyMs: 0, autoTick: false, now: () => 1_700_000_000_000 });
  const pushed: ServerMsg[] = [];
  t.onMessage((e) => {
    expect(e.id).toBe(0);
    pushed.push(e.body);
  });
  let id = 0;
  const req = (body: Parameters<MockTransport['send']>[0]['body']) => t.send({ id: ++id, body });
  return { t, pushed, req };
}

const events = (pushed: ServerMsg[], sid: string): SessionEvent[] =>
  pushed.flatMap((m) => (m.type === 'event' && m.session_id === sid ? [m.event] : []));

describe('MockTransport', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('offers speech profiles and keeps this device’s choice', async () => {
    const { t, req } = setup();
    const connecting = t.connect();
    await vi.runAllTimersAsync();
    await connecting;
    const ask = async (body: Parameters<typeof req>[0]) => {
      const p = req(body);
      await vi.runAllTimersAsync();
      return p;
    };
    const list = await ask({ type: 'list_speech_options' });
    if (list.type !== 'speech_options') throw new Error('expected speech_options');
    expect(list.current_engine).toBe('whisper-local');
    expect(list.current_llm).toBeNull();
    expect(list.llms.some((l) => !l.configured)).toBe(true);
    const saved = await ask({ type: 'set_speech_preferences', engine: null, llm: 'local-llm' });
    expect(saved).toMatchObject({ type: 'speech_options', current_llm: 'local-llm' });
    const refused = await ask({ type: 'set_speech_preferences', engine: null, llm: 'claude' });
    expect(refused).toMatchObject({ type: 'error', code: 'invalid_request' });
    // A final chunk comes back with a topic; interim previews without one.
    const base = {
      type: 'transcribe' as const,
      audio: new Uint8Array([1]),
      format: 'wav' as const,
      sample_rate: null,
      language: 'en',
      session_id: null,
    };
    expect(await ask({ ...base, final: true })).toMatchObject({ topic: 'Testing' });
    expect(await ask(base)).toMatchObject({ topic: null });
  });

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
    expect(res.sessions).toHaveLength(7);
    expect(res.sessions.filter((s) => s.origin.type === 'cloud')).toHaveLength(2);
    expect(res.sessions.find((s) => s.id === 'wk-claude-api')?.state.type).toBe(
      'awaiting_decision',
    );
  });

  it('rejects requests while disconnected', async () => {
    const { req } = setup();
    await expect(req({ type: 'list_sessions' })).rejects.toThrow('not connected');
  });

  it('streams only new events without from_seq', async () => {
    const { t, pushed, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    req({ type: 'subscribe', session_id: 'wk-codex-web', from_seq: null });
    await vi.runAllTimersAsync();
    expect(events(pushed, 'wk-codex-web')).toHaveLength(0);
  });

  it('replays history on subscribe, then streams with increasing seq, and resumes from_seq', async () => {
    const { t, pushed, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    req({ type: 'subscribe', session_id: 'wk-codex-web', from_seq: 0 });
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

  it('replays only the newest page and serves earlier ones by history request', async () => {
    const { t, pushed, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    for (let i = 0; i < 200; i++) t.tick();
    req({ type: 'subscribe', session_id: 'wk-codex-web', from_seq: 0 });
    await vi.runAllTimersAsync();
    const page = events(pushed, 'wk-codex-web');
    expect(page.length).toBe(MOCK_PAGE_EVENTS);
    const gap = pushed.find((m) => m.type === 'stream_gap');
    if (gap?.type !== 'stream_gap') throw new Error('expected a gap marking the page start');
    expect(gap.first_available_seq).toBe(page[0]!.seq);
    expect(gap.first_available_seq).toBeGreaterThan(1);

    pushed.length = 0;
    const reply = await req({
      type: 'history',
      session_id: 'wk-codex-web',
      before_seq: gap.first_available_seq,
      limit: 25,
    });
    if (reply.type !== 'history') throw new Error('expected history');
    expect(reply.events).toHaveLength(25);
    expect(reply.events.at(-1)!.seq).toBe(gap.first_available_seq - 1);
    expect(reply.has_more).toBe(true);

    // Walking back to the beginning ends with has_more = false.
    let before = reply.events[0]!.seq;
    let more = reply.has_more;
    while (more) {
      const r = await req({
        type: 'history',
        session_id: 'wk-codex-web',
        before_seq: before,
        limit: 500,
      });
      if (r.type !== 'history') throw new Error('expected history');
      before = r.events[0]!.seq;
      more = r.has_more;
    }
    expect(before).toBe(1);
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

  it('renames sessions, pushes the new name and resets it', async () => {
    const { t, pushed, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    const rename = async (name: string | null, sessionId = 'wk-codex-web') => {
      const res = req({ type: 'rename_session', session_id: sessionId, name });
      await vi.runAllTimersAsync();
      return res;
    };
    const updates = () =>
      pushed.flatMap((m) =>
        m.type === 'session_updated' && m.session.id === 'wk-codex-web' ? [m.session] : [],
      );
    const original = updates().length;
    expect(await rename('  Web refactor ')).toEqual({ type: 'ok' });
    const renamed = updates().at(-1)!;
    expect(renamed.display_name).toBe('Web refactor');
    expect(renamed.custom_name).toBe(true);
    const auto = renamed.auto_name!;
    expect(auto).not.toBe('Web refactor');

    expect(await rename('a\nb')).toMatchObject({ type: 'error', code: 'invalid_request' });
    expect(await rename('x'.repeat(81))).toMatchObject({ type: 'error', code: 'invalid_request' });
    expect(await rename('x', 'nope')).toMatchObject({ type: 'error', code: 'not_found' });

    expect(await rename(null)).toEqual({ type: 'ok' });
    const reset = updates().at(-1)!;
    expect(reset.display_name).toBe(auto);
    expect(reset.custom_name).toBe(false);
    expect(updates().length).toBe(original + 2);
  });

  it('renames devices and changes scopes with the server rules', async () => {
    const { t, req } = setup();
    void t.connect();
    await vi.runAllTimersAsync();
    const call = async (body: Parameters<typeof req>[0]) => {
      const p = req(body);
      await vi.runAllTimersAsync();
      return p;
    };
    const renamed = await call({ type: 'rename_device', device_id: 'dev-ipad', name: ' Kids ' });
    if (renamed.type !== 'devices') throw new Error(`expected devices: ${renamed.type}`);
    expect(renamed.devices.find((d) => d.id === 'dev-ipad')?.name).toBe('Kids');
    expect(await call({ type: 'rename_device', device_id: 'dev-ipad', name: '  ' })).toMatchObject({
      type: 'error',
      code: 'invalid_request',
    });
    const scoped = await call({ type: 'set_device_scope', device_id: 'dev-ipad', scope: 'admin' });
    if (scoped.type !== 'devices') throw new Error('expected devices');
    expect(scoped.devices.find((d) => d.id === 'dev-ipad')?.scope).toBe('admin');
    // The current device cannot change its own scope.
    expect(
      await call({ type: 'set_device_scope', device_id: 'dev-pixel', scope: 'view' }),
    ).toMatchObject({ type: 'error', code: 'forbidden' });
    expect(
      await call({ type: 'set_device_scope', device_id: 'dev-nope', scope: 'view' }),
    ).toMatchObject({ type: 'error', code: 'not_found' });
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

describe('commonly misheard words', () => {
  const connected = async () => {
    const t = new MockTransport({ latencyMs: 0, autoTick: false });
    await t.connect();
    return t;
  };
  const list = async (t: MockTransport) => {
    const reply = await t.send({ id: 1, body: { type: 'list_personal_terms' } });
    if (reply.type !== 'personal_terms') throw new Error(reply.type);
    return reply.terms;
  };

  it('offers the feature and keeps a saved list', async () => {
    const t = await connected();
    const hello = await t.send({
      id: 1,
      body: { type: 'hello', protocol_version: 1, app_version: 'test' },
    });
    expect(hello.type === 'welcome' && hello.features).toContain('personal_terms');
    const terms = [{ term: 'Zentorix', variants: ['zen torix'] }];
    await t.send({ id: 2, body: { type: 'save_personal_terms', terms } });
    expect(await list(t)).toEqual(terms);
  });

  it('refuses duplicates like the server and keeps the list', async () => {
    const t = await connected();
    const before = await list(t);
    const reply = await t.send({
      id: 2,
      body: {
        type: 'save_personal_terms',
        terms: [
          { term: 'Same', variants: [] },
          { term: 'same', variants: [] },
        ],
      },
    });
    expect(reply).toMatchObject({ type: 'error', code: 'invalid_request' });
    expect(await list(t)).toEqual(before);
  });
});
