import { describe, expect, it } from 'vitest';

import { defaultConductorState } from '../protocol/mockConductor';
import { conductorStopped, hooksMissing, turnsToEvents } from './conductor';
import { placeVisible } from './sessionOrder';

describe('turnsToEvents (built-in conductor chat in the chat view)', () => {
  it('maps the person, the conductor and wake-up events', () => {
    expect(
      turnsToEvents([
        { role: 'user', text: 'which sessions need me?', at: 1 },
        { role: 'assistant', text: 'None.', at: 2 },
        { role: 'event', text: 'turn ended in api', at: 3 },
      ]),
    ).toEqual([
      { type: 'message', seq: 1, at: 1, role: 'user', text: 'which sessions need me?' },
      { type: 'message', seq: 2, at: 2, role: 'assistant', text: 'None.' },
      { type: 'message', seq: 3, at: 3, role: 'system', text: 'turn ended in api' },
    ]);
  });
});

describe('conductor state helpers', () => {
  it('reports missing hooks only when the server says so', () => {
    const state = defaultConductorState();
    expect(hooksMissing(state)).toBe(false);
    expect(hooksMissing({ ...state, problem: null })).toBe(false);
    expect(hooksMissing({ ...state, problem: 'hooks_missing' })).toBe(true);
    expect(hooksMissing(null)).toBe(false);
  });

  it('knows when the conductor is stopped', () => {
    const state = defaultConductorState();
    expect(conductorStopped(state)).toBe(true);
    expect(conductorStopped({ ...state, status: 'revoked' })).toBe(true);
    expect(conductorStopped({ ...state, status: 'ready' })).toBe(false);
  });
});

describe('placeVisible (order panel without the conductor card)', () => {
  it('keeps hidden sessions on their places', () => {
    expect(placeVisible(['cd', 'a', 'b', 'c'], ['c', 'a', 'b'])).toEqual(['cd', 'c', 'a', 'b']);
    expect(placeVisible(['a', 'cd', 'b'], ['b', 'a'])).toEqual(['b', 'cd', 'a']);
    const order = ['a', 'cd', 'b'];
    expect(placeVisible(order, ['a', 'b'])).toBe(order);
  });
});
