import { describe, expect, it } from 'vitest';

import { defaultConductorState } from '../protocol/mockConductor';
import type { HumanTask } from '../protocol/tasks';
import type { ConductorEscalation, ConductorState, SessionInfo } from '../protocol/types';
import { localSession } from '../test/fixtures';
import type { ApprovalAlert } from './approvals';
import { buildInbox, createDeferredStore, deferItem } from './inbox';
import type { TaskBook } from './tasks';

const NOW = 1_800_000_000_000;

function alert(patch: Partial<ApprovalAlert>): ApprovalAlert {
  return {
    key: 'd:s1:dec-1',
    sessionId: 's1',
    kind: 'decision',
    name: 'payments-api',
    request: 'Allow Bash command?',
    at: NOW - 1000,
    ...patch,
  };
}

function conductor(patch: Partial<ConductorState> = {}): ConductorState {
  return {
    ...defaultConductorState(),
    settings: { ...defaultConductorState().settings, enabled: true },
    status: 'ready',
    session_id: 'cd',
    ...patch,
  };
}

function task(patch: Partial<HumanTask>): HumanTask {
  return {
    id: 't1',
    session_id: 's2',
    session_name: 'web',
    machine: 'pc',
    title: 'Sign in to the staging console',
    steps: '',
    command: null,
    source: 'agent',
    status: 'open',
    priority: 'normal',
    blocking: true,
    due: null,
    snoozed_until: null,
    created_at: NOW - 5000,
    updated_at: NOW - 5000,
    order: 0,
    result: null,
    sent_to_session: false,
    sent_at: null,
    message_seq: null,
    agent_moved_on: false,
    stale: false,
    ...patch,
  } as HumanTask;
}

function escalation(patch: Partial<ConductorEscalation> = {}): ConductorEscalation {
  return {
    id: 'ce-1',
    session_id: 's1',
    session_name: 'payments-api',
    decision_id: 'dec-1',
    asked_at: NOW - 3000,
    source: 'conductor',
    summary: '결제 API 세션이 dist 폴더를 지우고 다시 빌드하려고 합니다.',
    recommendation: '빌드 산출물이라 허용하세요.',
    request: 'Allow Bash command?\nrm -rf dist && pnpm build',
    command: 'rm -rf dist && pnpm build',
    reason: null,
    created_at: NOW - 2000,
    ...patch,
  };
}

/** `s1` waiting on decision `id`. */
function waiting(id = 'dec-1'): SessionInfo {
  return localSession({
    state: { type: 'awaiting_decision' },
    pending_decision: {
      decision: { id, prompt: 'Allow Bash command?', options: [{ id: 'yes', label: 'Yes' }] },
      at: NOW - 3000,
    },
  });
}

const book = (list: HumanTask[]): TaskBook =>
  ({ list, settings: {} as TaskBook['settings'], reminder: null }) as TaskBook;

describe('buildInbox', () => {
  it('shows only what the conductor raised: its requests, escalations and its to-dos', () => {
    const items = buildInbox({
      alerts: [
        // Other sessions' raw alerts stay under Alerts.
        alert({}),
        alert({ key: 'q:s2:q1', sessionId: 's2', kind: 'question', name: 'web', at: NOW }),
      ],
      conductor: conductor({
        escalations: [escalation()],
        pending: [
          {
            id: 'cr-1',
            session_id: 's2',
            session_name: 'web',
            text: 'npm test',
            created_at: NOW - 9000,
            expires_at: NOW + 60_000,
          },
        ],
      }),
      tasks: book([
        task({ session_id: 'cd', session_name: 'Conductor' }),
        // A to-do found in another session's messages belongs to Alerts · To-do.
        task({ id: 't4' }),
        task({ id: 't2', session_id: 'cd', blocking: false }),
        task({ id: 't3', session_id: 'cd', status: 'done' }),
      ]),
      sessions: { s1: waiting() },
      now: NOW,
    });
    expect(items.map((i) => i.key)).toEqual(['cr:cr-1', 'esc:ce-1', 'task:t1']);
    const send = items[0]!;
    expect(send).toMatchObject({ kind: 'conductor_send', name: 'web', text: 'npm test' });
    expect(items[1]).toMatchObject({
      kind: 'escalation',
      sessionId: 's1',
      name: 'payments-api',
      text: '결제 API 세션이 dist 폴더를 지우고 다시 빌드하려고 합니다.',
    });
    expect(items[1]!.escalation?.recommendation).toBe('빌드 산출물이라 허용하세요.');
    expect(items[2]).toMatchObject({
      kind: 'task',
      name: 'Conductor',
      text: 'Sign in to the staging console',
    });
  });

  it('drops an escalation once its decision was answered or replaced', () => {
    const sources = {
      alerts: [],
      conductor: conductor({ escalations: [escalation()] }),
      tasks: null,
      now: NOW,
    };
    expect(buildInbox({ ...sources, sessions: { s1: waiting() } })).toHaveLength(1);
    expect(buildInbox({ ...sources, sessions: { s1: waiting('dec-2') } })).toEqual([]);
    expect(buildInbox({ ...sources, sessions: { s1: localSession() } })).toEqual([]);
    // A fallback has no summary: the request's first line stands in.
    const [fallback] = buildInbox({
      ...sources,
      conductor: conductor({
        escalations: [escalation({ source: 'fallback', summary: '', reason: 'timeout' })],
      }),
      sessions: { s1: waiting() },
    });
    expect(fallback).toMatchObject({ kind: 'escalation', text: 'Allow Bash command?' });
  });

  it('shows no escalation while the conductor is off', () => {
    const items = buildInbox({
      alerts: [alert({})],
      conductor: conductor({
        settings: { ...defaultConductorState().settings, enabled: false },
        status: 'off',
        escalations: [escalation()],
      }),
      tasks: null,
      sessions: { s1: waiting() },
      now: NOW,
    });
    expect(items).toEqual([]);
  });

  it('keeps the conductor’s decisions and drops its input waits', () => {
    const items = buildInbox({
      alerts: [
        alert({ key: 'd:cd:d-bash', sessionId: 'cd', name: 'Conductor', conductor: true }),
        alert({ key: 'w:cd:input', sessionId: 'cd', kind: 'input', name: 'Conductor' }),
        alert({ key: 'q:cd:q1', sessionId: 'cd', kind: 'question', name: 'Conductor' }),
      ],
      conductor: conductor(),
      tasks: null,
      sessions: {},
      now: NOW,
    });
    expect(items.map((i) => i.key)).toEqual(['d:cd:d-bash']);
    expect(items[0]!.conductor).toBe(true);
  });

  it('dedupes by key and adds the conductor’s first-run trust question once', () => {
    const cd = localSession({ id: 'cd', display_name: 'Conductor', conductor: true });
    const own = alert({ key: 'd:cd:d1', sessionId: 'cd', name: 'Conductor', conductor: true });
    const items = buildInbox({
      alerts: [own, own, alert({})],
      conductor: conductor(),
      tasks: null,
      sessions: {
        cd: { ...cd, terminal_prompt: 'trust_folder', state: { type: 'awaiting_input' } },
      },
      now: NOW,
    });
    expect(items.map((i) => i.key)).toEqual(['trust:cd', 'd:cd:d1']);
    expect(items[0]).toMatchObject({ kind: 'conductor_trust', conductor: true });
  });

  it('leaves out snoozed to-dos and works without a conductor', () => {
    const items = buildInbox({
      alerts: [],
      conductor: null,
      tasks: book([task({ snoozed_until: NOW + 60_000 })]),
      sessions: {},
      now: NOW,
    });
    expect(items).toEqual([]);
  });
});

describe('deferred items', () => {
  it('moves an item to later and back', () => {
    const store = createDeferredStore();
    let calls = 0;
    store.subscribe(() => calls++);
    deferItem('cr:1', true, store);
    expect(store.get().has('cr:1')).toBe(true);
    deferItem('cr:1', false, store);
    expect(store.get().has('cr:1')).toBe(false);
    expect(calls).toBe(2);
  });
});
