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
});
