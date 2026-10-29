import { describe, expect, it } from 'vitest';

import type { ServerMsg } from '../protocol/types';
import type { SessionEvent } from '../protocol/types';
import { cloudSession, localSession } from '../test/fixtures';
import {
  attentionCount,
  canSendInput,
  initialState,
  reducer,
  visibleSessions,
  type AppState,
} from './reducer';

const enc = new TextEncoder();
const server = (state: AppState, msg: ServerMsg) => reducer(state, { type: 'server', msg });
const ev = (state: AppState, event: SessionEvent, sid = 's1') =>
  server(state, { type: 'event', session_id: sid, event });

function withSessions(): AppState {
  return server(initialState, {
    type: 'sessions',
    sessions: [
      localSession(),
      localSession({
        id: 's2',
        display_name: 'docs',
        state: { type: 'awaiting_input' },
        unread: 2,
      }),
      cloudSession(),
    ],
  });
}

describe('reducer', () => {
  it('loads sessions in server order with empty views', () => {
    const s = withSessions();
    expect(s.order).toEqual(['s1', 's2', 'c1']);
    expect(s.views.s1).toEqual({ events: [], lastSeq: null, gapFrom: null });
  });

  it('keeps existing order and appends new sessions on reload', () => {
    let s = withSessions();
    s = server(s, { type: 'session_updated', session: localSession({ id: 's9' }) });
    expect(s.order).toEqual(['s1', 's2', 'c1', 's9']);
    s = server(s, { type: 'sessions', sessions: [cloudSession(), localSession()] });
    expect(s.order).toEqual(['s1', 'c1']);
  });

  it('applies events in order and ignores duplicates', () => {
    let s = withSessions();
    s = ev(s, { type: 'output', seq: 1, at: 1, data: enc.encode('\x1b[32mbuild ok\x1b[0m\r\n') });
    s = ev(s, { type: 'tool_call', seq: 2, at: 2, name: 'Bash', summary: 'pnpm test' });
    s = ev(s, { type: 'output', seq: 2, at: 2, data: enc.encode('dup') });
    expect(s.views.s1!.events.map((e) => e.seq)).toEqual([1, 2]);
    expect(s.views.s1!.lastSeq).toBe(2);
  });

  it('tracks pending decisions until the state moves on', () => {
    let s = withSessions();
    const decision = { id: 'd1', prompt: 'Allow?', options: [{ id: 'y', label: 'Yes' }] };
    s = ev(s, { type: 'decision_requested', seq: 1, at: 1, decision });
    s = ev(s, { type: 'state_changed', seq: 2, at: 2, state: { type: 'awaiting_decision' } });
    expect(s.decisions.s1).toEqual(decision);
    s = ev(s, { type: 'state_changed', seq: 3, at: 3, state: { type: 'running' } });
    expect(s.decisions.s1).toBeUndefined();
  });

  it('clears a decision optimistically when answered', () => {
    let s = withSessions();
    s = ev(s, {
      type: 'decision_requested',
      seq: 1,
      at: 1,
      decision: { id: 'd', prompt: 'p', options: [] },
    });
    s = reducer(s, { type: 'decision_answered', sessionId: 's1' });
    expect(s.decisions.s1).toBeUndefined();
  });

  it('resets a view on stream_gap and accepts the replay that follows', () => {
    let s = withSessions();
    s = ev(s, { type: 'message', seq: 1, at: 1, role: 'user', text: 'hi' });
    s = ev(s, { type: 'message', seq: 2, at: 2, role: 'assistant', text: 'hello' });
    s = server(s, { type: 'stream_gap', session_id: 's1', first_available_seq: 40 });
    expect(s.views.s1).toMatchObject({ events: [], lastSeq: null, gapFrom: 40 });
    s = ev(s, { type: 'message', seq: 40, at: 40, role: 'assistant', text: 'later' });
    expect(s.views.s1!.events).toHaveLength(1);
    expect(s.views.s1!.lastSeq).toBe(40);
  });

  it('records delivery acknowledgements', () => {
    const s = server(withSessions(), { type: 'input_ack', session_id: 's1', status: 'queued' });
    expect(s.delivery.s1?.status).toBe('queued');
  });

  it('removes sessions and clears the selection', () => {
    let s = reducer(withSessions(), { type: 'select', id: 's2' });
    s = server(s, { type: 'session_removed', session_id: 's2' });
    expect(s.order).toEqual(['s1', 'c1']);
    expect(s.selectedId).toBeNull();
    expect(s.views.s2).toBeUndefined();
  });

  it('filters the deck', () => {
    let s = withSessions();
    const ids = () => visibleSessions(s).map((x) => x.id);
    expect(ids()).toEqual(['s1', 's2', 'c1']);
    s = reducer(s, { type: 'filter', filter: 'attention' });
    expect(ids()).toEqual(['s2']);
    s = reducer(s, { type: 'filter', filter: 'cloud' });
    expect(ids()).toEqual(['c1']);
    s = reducer(s, { type: 'filter', filter: 'local' });
    expect(ids()).toEqual(['s1', 's2']);
    expect(attentionCount(s)).toBe(1);
  });

  it('caps toasts and dismisses them', () => {
    let s = initialState;
    for (let i = 0; i < 6; i++) {
      s = reducer(s, {
        type: 'toast',
        toast: { id: `t${i}`, category: 'activity', sessionId: null, title: 't', body: '' },
      });
    }
    expect(s.toasts.map((t) => t.id)).toEqual(['t2', 't3', 't4', 't5']);
    s = reducer(s, { type: 'dismiss_toast', id: 't3' });
    expect(s.toasts.map((t) => t.id)).toEqual(['t2', 't4', 't5']);
  });

  it('reset keeps only the connection state', () => {
    let s = reducer(withSessions(), { type: 'connection', state: 'connected' });
    s = reducer(s, { type: 'reset' });
    expect(s).toEqual({ ...initialState, connection: 'connected' });
  });

  it('canSendInput rejects observe-only and exited sessions', () => {
    expect(canSendInput(localSession())).toBe(true);
    expect(canSendInput(localSession({ control: 'observe_only' }))).toBe(false);
    expect(canSendInput(localSession({ state: { type: 'exited', code: 0 } }))).toBe(false);
  });

  it('keeps sent prompts with their anchor seq and delivery status', () => {
    let s = ev(withSessions(), { type: 'output', seq: 4, at: 1, data: enc.encode('hi') });
    s = reducer(s, { type: 'prompt_sent', sessionId: 's1', id: 'p1', text: 'run', at: 10 });
    expect(s.sent.s1).toEqual([{ id: 'p1', text: 'run', at: 10, afterSeq: 4, status: 'sending' }]);
    s = server(s, { type: 'input_ack', session_id: 's1', status: 'delivered' });
    expect(s.sent.s1?.[0]?.status).toBe('delivered');
    // Late or duplicate acks never move a prompt back.
    s = server(s, { type: 'input_ack', session_id: 's1', status: 'queued' });
    expect(s.sent.s1?.[0]?.status).toBe('delivered');
    s = server(s, { type: 'input_ack', session_id: 's1', status: 'acknowledged' });
    expect(s.sent.s1?.[0]?.status).toBe('acknowledged');
  });

  it('marks a failed prompt and drops prompts of removed sessions', () => {
    let s = reducer(withSessions(), {
      type: 'prompt_sent',
      sessionId: 's2',
      id: 'p1',
      text: 'x',
      at: 1,
    });
    expect(s.sent.s2?.[0]?.afterSeq).toBeNull();
    s = reducer(s, {
      type: 'prompt_status',
      sessionId: 's2',
      id: 'p1',
      status: 'failed',
      error: 'boom',
    });
    expect(s.sent.s2?.[0]).toMatchObject({ status: 'failed', error: 'boom' });
    s = server(s, { type: 'session_removed', session_id: 's2' });
    expect(s.sent.s2).toBeUndefined();
  });
});
