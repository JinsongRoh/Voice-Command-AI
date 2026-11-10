import { describe, expect, it } from 'vitest';

import { translate, type TranslateFn } from '../i18n/i18n';
import type { SessionInfo } from '../protocol/types';
import { localSession } from '../test/fixtures';
import {
  approvalTitle,
  closeAlert,
  dropSession,
  emptyAlertBook,
  freshAlerts,
  newestFirst,
  syncSessions,
  updateAlerts,
} from './approvals';
import { initialState, alertCount, alertsAndTasksCount, reducer, type AppState } from './reducer';

function waiting(id: string, decisionId: string, at = 100): SessionInfo {
  return localSession({
    id,
    display_name: id,
    state: { type: 'awaiting_decision' },
    pending_decision: {
      at,
      decision: {
        id: decisionId,
        prompt: 'Allow Bash command?\nnpm test',
        options: [{ id: 'yes', label: 'Yes' }],
      },
    },
  });
}

const running = (id: string) =>
  localSession({ id, display_name: id, state: { type: 'running' }, pending_decision: null });

function apply(state: AppState, session: SessionInfo): AppState {
  return reducer(state, { type: 'server', msg: { type: 'session_updated', session } });
}

describe('approval alerts', () => {
  it('lists a decision with its name, request and time', () => {
    const book = updateAlerts(emptyAlertBook, waiting('s1', 'd1', 42));
    expect(book.active).toEqual([
      {
        key: 'd:s1:d1',
        sessionId: 's1',
        kind: 'decision',
        name: 's1',
        request: 'Allow Bash command?',
        at: 42,
      },
    ]);
  });

  it('counts a session once and never repeats the same decision', () => {
    let book = updateAlerts(emptyAlertBook, waiting('s1', 'd1'));
    const again = updateAlerts(book, waiting('s1', 'd1'));
    expect(again).toBe(book);
    expect(freshAlerts(book, again)).toEqual([]);
    // Answered elsewhere, then the same id shows up in a replay.
    book = updateAlerts(book, running('s1'));
    const replay = updateAlerts(book, waiting('s1', 'd1'));
    expect(freshAlerts(book, replay)).toEqual([]);
  });

  it('a new decision of the session is a new alert', () => {
    const first = updateAlerts(emptyAlertBook, waiting('s1', 'd1'));
    const idle = updateAlerts(first, running('s1'));
    const second = updateAlerts(idle, waiting('s1', 'd2'));
    expect(freshAlerts(idle, second).map((a) => a.key)).toEqual(['d:s1:d2']);
  });

  it('leaves by itself when the session moves on, whoever answered', () => {
    const book = updateAlerts(emptyAlertBook, waiting('s1', 'd1'));
    expect(updateAlerts(book, running('s1')).active).toEqual([]);
  });

  it('a handled alert stays away', () => {
    const book = closeAlert(updateAlerts(emptyAlertBook, waiting('s1', 'd1')), 'd:s1:d1');
    expect(book.active).toEqual([]);
    expect(updateAlerts(book, waiting('s1', 'd1')).active).toEqual([]);
  });

  it('details arriving for a wait do not alert twice', () => {
    const bare = localSession({
      id: 's1',
      state: { type: 'awaiting_decision' },
      pending_decision: null,
    });
    const first = updateAlerts(emptyAlertBook, bare);
    expect(first.active[0]!.key).toBe('w:s1:decision');
    const refined = updateAlerts(first, waiting('s1', 'd1'));
    expect(refined.active).toHaveLength(1);
    expect(refined.active[0]!.key).toBe('d:s1:d1');
    expect(freshAlerts(first, refined)).toEqual([]);
  });

  it('a wait for input alerts again after the session worked', () => {
    const input = localSession({ id: 's1', state: { type: 'awaiting_input' } });
    const first = updateAlerts(emptyAlertBook, input);
    const handled = closeAlert(first, first.active[0]!.key);
    expect(updateAlerts(handled, input).active).toEqual([]);
    const worked = updateAlerts(handled, running('s1'));
    expect(updateAlerts(worked, input).active).toHaveLength(1);
  });

  it('marks inferred decisions, words their title and counts them', () => {
    const inferred = localSession({
      id: 's3',
      display_name: 'shop',
      state: { type: 'awaiting_decision' },
      pending_decision: {
        at: 7,
        decision: {
          id: 'claude:abc:inferred-toolu_1',
          prompt: 'Allow Bash: rm -rf build?',
          options: [],
          inferred: true,
        },
      },
    });
    const book = updateAlerts(updateAlerts(emptyAlertBook, waiting('s1', 'd1')), inferred);
    const alert = book.active.find((a) => a.sessionId === 's3')!;
    expect(alert).toMatchObject({
      kind: 'decision',
      inferred: true,
      request: 'Allow Bash: rm -rf build?',
    });
    const t: TranslateFn = (key, vars) => translate('en', key, vars);
    expect(approvalTitle(alert, t)).toBe('shop seems to need confirmation');
    expect(approvalTitle(book.active[0]!, t)).toBe('s1 needs a decision');
    expect(updateAlerts(book, inferred)).toBe(book);
    let state = apply(initialState, waiting('s1', 'd1'));
    state = apply(state, inferred);
    expect(alertCount(state)).toBe(2);
    // Answered in the terminal: the server clears it and the alert leaves.
    state = apply(state, running('s3'));
    expect(alertCount(state)).toBe(1);
  });

  it('lists a question at the end of a turn, once, until the conversation moves on', () => {
    const asking = (questionId: string, conductor = false) =>
      localSession({
        id: 's4',
        display_name: 'Orbit',
        state: { type: 'running' },
        pending_decision: null,
        conductor,
        question: { id: questionId, text: '어떻게 할까요?', at: 9 },
      });
    let state = apply(initialState, running('s4'));
    const before = state.alerts;
    state = apply(state, asking('q1'));
    const alert = state.alerts.active[0]!;
    expect(alert).toEqual({
      key: 'q:s4:q1',
      sessionId: 's4',
      kind: 'question',
      name: 'Orbit',
      request: '어떻게 할까요?',
      at: 9,
    });
    // Counted in the badge, announced once.
    expect(alertCount(state)).toBe(1);
    expect(freshAlerts(before, state.alerts)).toEqual([alert]);
    expect(freshAlerts(state.alerts, apply(state, asking('q1')).alerts)).toEqual([]);
    const ko: TranslateFn = (key, vars) => translate('ko', key, vars);
    expect(approvalTitle(alert, ko)).toBe('Orbit의 에이전트가 질문하고 있습니다');
    const en: TranslateFn = (key, vars) => translate('en', key, vars);
    expect(approvalTitle(alert, en)).toBe('Orbit has a question');
    // The person answered (the server cleared it): gone.
    state = apply(state, running('s4'));
    expect(alertCount(state)).toBe(0);
    // A decision outranks a question of the same session.
    const both = { ...waiting('s4', 'd9'), question: { id: 'q2', text: 'Ship?', at: 10 } };
    expect(updateAlerts(emptyAlertBook, both).active[0]!.kind).toBe('decision');
    // Never for the conductor's own session.
    expect(updateAlerts(emptyAlertBook, asking('q3', true)).active).toEqual([]);
  });

  it('follows whole lists and removed sessions', () => {
    let book = syncSessions(emptyAlertBook, [waiting('s1', 'd1', 1), waiting('s2', 'd2', 2)]);
    expect(newestFirst(book).map((a) => a.sessionId)).toEqual(['s2', 's1']);
    book = syncSessions(book, [waiting('s2', 'd2', 2)]);
    expect(book.active.map((a) => a.sessionId)).toEqual(['s2']);
    expect(dropSession(book, 's2').active).toEqual([]);
  });
});

describe('alerts in the app state', () => {
  it('counts, and takes the toast along when answered elsewhere', () => {
    let state = apply(initialState, waiting('s1', 'd1'));
    state = apply(state, waiting('s2', 'd2'));
    expect(alertCount(state)).toBe(2);
    state = reducer(state, {
      type: 'toast',
      toast: {
        id: 'alert:d:s1:d1',
        category: 'decision',
        sessionId: 's1',
        title: 't',
        body: '',
        alertKey: 'd:s1:d1',
      },
    });
    state = apply(state, running('s1'));
    expect(alertCount(state)).toBe(1);
    expect(state.toasts).toEqual([]);
  });

  it('fills the decision card from the session card', () => {
    const state = apply(initialState, waiting('s1', 'd1'));
    expect(state.decisions.s1?.id).toBe('d1');
  });

  it('answering here clears the alert', () => {
    let state = apply(initialState, waiting('s1', 'd1'));
    state = reducer(state, { type: 'decision_answered', sessionId: 's1' });
    expect(alertCount(state)).toBe(0);
  });

  it('remembers across a change of machine', () => {
    let state = apply(initialState, waiting('s1', 'd1'));
    state = reducer(state, { type: 'alert_handled', key: 'd:s1:d1' });
    state = reducer(state, { type: 'reset' });
    expect(apply(state, waiting('s1', 'd1')).alerts.active).toEqual([]);
  });

  it("lists only the conductor session's decisions, marked as such", () => {
    const conductor = (patch: Partial<SessionInfo>) =>
      localSession({ id: 'cd', display_name: 'Conductor', conductor: true, ...patch });
    // Its input waits are turn-ends: no alert, no count.
    let state = apply(initialState, conductor({ state: { type: 'awaiting_input' } }));
    expect(alertCount(state)).toBe(0);
    expect(alertsAndTasksCount(state)).toBe(0);
    // A permission decision: in the list and the badges, as "Conductor session".
    state = apply(state, { ...waiting('cd', 'd9'), display_name: 'Conductor', conductor: true });
    expect(alertCount(state)).toBe(1);
    expect(alertsAndTasksCount(state)).toBe(1);
    expect(state.alerts.active[0]).toMatchObject({ sessionId: 'cd', conductor: true });
    state = apply(state, conductor({ state: { type: 'running' } }));
    expect(alertCount(state)).toBe(0);
  });
});
