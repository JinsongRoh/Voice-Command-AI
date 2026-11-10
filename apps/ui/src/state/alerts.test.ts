import { describe, expect, it } from 'vitest';

import { translate } from '../i18n/i18n';
import { localSession } from '../test/fixtures';
import { alertsFor } from './alerts';
import { initialState, reducer, type AppState } from './reducer';

const t = (k: Parameters<typeof translate>[1], v?: Record<string, string | number>) =>
  translate('en', k, v);

function stateWith(session = localSession()): AppState {
  return reducer(initialState, { type: 'server', msg: { type: 'sessions', sessions: [session] } });
}

describe('alertsFor', () => {
  it('alerts when a session starts waiting for a decision', () => {
    const alerts = alertsFor(
      stateWith(),
      { type: 'session_updated', session: localSession({ state: { type: 'awaiting_decision' } }) },
      t,
    );
    expect(alerts).toHaveLength(1);
    expect(alerts[0]).toMatchObject({
      category: 'decision',
      title: 'payments-api needs a decision',
    });
  });

  it('classifies completion, errors and user stops', () => {
    const update = (state: Parameters<typeof localSession>[0]) =>
      alertsFor(stateWith(), { type: 'session_updated', session: localSession(state) }, t);
    expect(update({ state: { type: 'idle' } })[0]?.category).toBe('completed');
    expect(update({ state: { type: 'exited', code: 0 } })[0]?.category).toBe('completed');
    expect(update({ state: { type: 'exited', code: 2 } })[0]?.category).toBe('error');
    expect(update({ state: { type: 'exited', code: 130 } })).toEqual([]);
    // A watched terminal that closed has no exit code: not an error.
    expect(update({ state: { type: 'exited', code: null } })).toEqual([]);
    expect(update({ state: { type: 'error', message: 'boom' } })[0]).toMatchObject({
      category: 'error',
      body: 'boom',
    });
    expect(update({ unread: 3 })).toEqual([]);
  });

  it('ignores unknown sessions', () => {
    expect(
      alertsFor(
        initialState,
        { type: 'session_updated', session: localSession({ state: { type: 'idle' } }) },
        t,
      ),
    ).toEqual([]);
  });

  it('alerts on live notification events but not on stale replays', () => {
    const now = 1_000_000;
    const msg = (seq: number, at: number) =>
      ({
        type: 'event',
        session_id: 's1',
        event: { type: 'notification', seq, at, title: 'Tests passed', body: '52 passed' },
      }) as const;
    expect(alertsFor(stateWith(), msg(1, now - 1000), t, now)[0]).toMatchObject({
      category: 'activity',
      title: 'Tests passed',
    });
    expect(alertsFor(stateWith(), msg(1, now - 60_000), t, now)).toEqual([]);
  });

  it("keeps the conductor's own session quiet except for decisions and errors", () => {
    const before = stateWith(localSession({ conductor: true, state: { type: 'running' } }));
    const update = (patch: Parameters<typeof localSession>[0]) =>
      alertsFor(
        before,
        { type: 'session_updated', session: localSession({ conductor: true, ...patch }) },
        t,
      );
    // Turn-ends and input waits are its replies (they reach 지휘소).
    expect(update({ state: { type: 'idle' } })).toEqual([]);
    expect(update({ state: { type: 'awaiting_input' } })).toEqual([]);
    expect(update({ state: { type: 'exited', code: 0 } })).toEqual([]);
    expect(update({ unread: 2 })).toEqual([]);
    // A permission decision still reaches the person.
    expect(update({ state: { type: 'awaiting_decision' } })[0]?.category).toBe('decision');
    expect(update({ state: { type: 'error', message: 'boom' } })[0]?.category).toBe('error');
    // Its notifications are not alerts either.
    const now = 1_000_000;
    const note = alertsFor(
      before,
      {
        type: 'event',
        session_id: 's1',
        event: { type: 'notification', seq: 9, at: now, title: 'Waiting', body: 'for input' },
      },
      t,
      now,
    );
    expect(note).toEqual([]);
  });
});
