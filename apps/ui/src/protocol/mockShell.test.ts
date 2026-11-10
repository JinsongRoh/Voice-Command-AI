import { encode } from '@msgpack/msgpack';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { decodeJson, decodeMsgpack, encodeJson, encodeMsgpack } from './codec';
import { MockTransport, type MockTransportOptions } from './mockTransport';
import type { ClientMsg, Envelope, ServerMsg } from './types';

let clock = 1_700_000_000_000;

async function setup(options: MockTransportOptions = {}) {
  const t = new MockTransport({ latencyMs: 0, autoTick: false, now: () => clock, ...options });
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

const open = (totp: string, shell = 'zsh'): ClientMsg => ({
  type: 'open_shell',
  request: { shell, project_dir: '~/code/app', display_name: null, totp },
});

describe('remote shell protocol (mock server)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    clock = 1_700_000_000_000;
  });
  afterEach(() => vi.useRealTimers());

  it('round-trips the new messages through JSON and MessagePack', () => {
    const client: ClientMsg[] = [
      open('123456'),
      { type: 'shell_run', session_id: 's1', command: 'rm -rf 빌드', totp: null },
      { type: 'classify_command', command: 'ls' },
      { type: 'list_audit', limit: 50 },
    ];
    for (const body of client) {
      const env: Envelope<ClientMsg> = { id: 3, body };
      expect(JSON.parse(encodeJson(env))).toEqual(env);
      expect(encodeMsgpack(env).length).toBeGreaterThan(0);
    }
    const server: ServerMsg[] = [
      { type: 'shell_ran', session_id: 's1', danger: 'dangerous' },
      { type: 'command_danger', level: 'caution', reasons: ['delete'], spans: [[0, 2]] },
      {
        type: 'audit',
        entries: [
          {
            ts: 1,
            device_id: 'd',
            device_name: 'Pixel',
            event: 'shell_open',
            session_id: 's1',
            detail: '',
          },
        ],
      },
      {
        type: 'shell_notice',
        session_id: 's1',
        event: 'closed',
        device_name: 'Pixel',
        reason: 'idle',
      },
      {
        type: 'launched',
        session: {
          id: 's1',
          origin: {
            type: 'local',
            agent: { type: 'shell', name: 'pwsh' },
            project_dir: 'C:\\a',
            pid: 1,
          },
          display_name: 'a · pwsh',
          state: { type: 'idle' },
          control: 'managed_pty',
          profile: null,
          created_at: 1,
          last_activity: 1,
          unread: 0,
          preview: null,
          shell: { idle_timeout_secs: 900, idle_deadline: 2, hard_deadline: 3, opened_by: 'local' },
        },
      },
    ];
    for (const body of server) {
      const env: Envelope<ServerMsg> = { id: 0, body };
      expect(decodeJson(JSON.stringify(env))).toEqual(env);
      expect(decodeMsgpack(encode(env))).toEqual(env);
    }
    // Error codes added for remote shells.
    const err = decodeJson('{"id":1,"body":{"type":"error","code":"totp_required","message":"x"}}');
    expect(err.body).toMatchObject({ code: 'totp_required' });
  });

  it('opens a shell with a fresh code, rejects a replayed code and announces it', async () => {
    const { ask, pushed } = await setup();
    const launched = await ask(open('123456'));
    expect(launched).toMatchObject({
      type: 'launched',
      session: {
        origin: { agent: { type: 'shell', name: 'zsh' } },
        shell: { opened_by: 'Pixel 9' },
      },
    });
    expect(pushed).toContainEqual(
      expect.objectContaining({ type: 'shell_notice', event: 'opened', device_name: 'Pixel 9' }),
    );
    expect(await ask(open('123456'))).toMatchObject({ type: 'error', code: 'totp_invalid' });
    expect(await ask(open('654321', 'pwsh'))).toMatchObject({ type: 'error', code: 'unavailable' });
  });

  it('locks out after five wrong codes', async () => {
    const { ask } = await setup();
    for (let i = 0; i < 4; i++) {
      expect(await ask(open('000000'))).toMatchObject({ code: 'totp_invalid' });
    }
    expect(await ask(open('000000'))).toMatchObject({ code: 'forbidden' });
    expect(await ask(open('222222'))).toMatchObject({ code: 'forbidden' });
  });

  it('reports disabled shells and missing TOTP as unsupported', async () => {
    expect(await (await setup({ shells: 'disabled' })).ask(open('123456'))).toMatchObject({
      code: 'unsupported',
    });
    const noTotp = await (await setup({ shells: 'no_totp' })).ask(open('123456'));
    expect(noTotp).toMatchObject({ code: 'unsupported', message: expect.stringMatching(/TOTP/) });
  });

  it('needs a code for dangerous commands outside the step-up window', async () => {
    const { ask } = await setup();
    // The seeded shell; no step-up yet.
    expect(
      await ask({
        type: 'shell_run',
        session_id: 'wk-shell-api',
        command: 'git status',
        totp: null,
      }),
    ).toEqual({ type: 'shell_ran', session_id: 'wk-shell-api', danger: 'safe' });
    const run = { type: 'shell_run' as const, session_id: 'wk-shell-api', command: 'rm -rf dist' };
    expect(await ask({ ...run, totp: null })).toMatchObject({ code: 'totp_required' });
    expect(await ask({ ...run, totp: '111111' })).toMatchObject({ danger: 'dangerous' });
    // Inside the step-up window no code is needed; after it, again.
    expect(await ask({ ...run, totp: null })).toMatchObject({ type: 'shell_ran' });
    clock += 6 * 60_000;
    expect(await ask({ ...run, totp: null })).toMatchObject({ code: 'totp_required' });
    // Two lines are held back until confirmed; confirmed, each line is classified.
    expect(await ask({ ...run, command: 'a\nb', totp: null })).toMatchObject({
      type: 'shell_confirm',
      command: 'a\nb',
      lines: 2,
    });
    expect(
      await ask({ ...run, command: 'echo a\nrm -rf dist', totp: null, confirm: true }),
    ).toMatchObject({ code: 'totp_required' });
    expect(
      await ask({ ...run, command: 'echo a\necho b', totp: null, confirm: true }),
    ).toMatchObject({ type: 'shell_ran', danger: 'safe' });
    expect(
      await ask({ type: 'shell_run', session_id: 'wk-codex-web', command: 'ls', totp: null }),
    ).toMatchObject({ code: 'invalid_request' });
  });

  it('closes shells at their deadlines and logs everything in the audit', async () => {
    const { t, ask, pushed } = await setup();
    const launched = await ask(open('123456'));
    if (launched.type !== 'launched') throw new Error('expected launched');
    clock = launched.session.shell!.idle_deadline + 1;
    t.tick();
    expect(pushed).toContainEqual(
      expect.objectContaining({ type: 'shell_notice', event: 'closed', reason: 'idle' }),
    );
    const audit = await ask({ type: 'list_audit', limit: 0 });
    if (audit.type !== 'audit') throw new Error('expected audit');
    const mine = audit.entries.filter((e) => e.session_id === launched.session.id);
    expect(mine.map((e) => [e.event, e.detail])).toEqual([
      ['shell_close', 'reason=idle'],
      ['shell_open', 'shell=zsh dir=~/code/app'],
    ]);
    expect(audit.entries.map((e) => e.event)).toContain('step_up_ok');
    expect(JSON.stringify(audit.entries)).not.toContain('123456');
  });
});
