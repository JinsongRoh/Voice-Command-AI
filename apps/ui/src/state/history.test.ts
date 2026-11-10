import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ServerMsg, SessionEvent } from '../protocol/types';
import { localSession } from '../test/fixtures';
import { createStore, OPEN_EMPTY_MS, OPEN_MAX_MS, OPEN_SETTLE_MS } from './core';
import {
  HISTORY_PAGE,
  HISTORY_PAGE_MAX,
  historyPageSize,
  initialState,
  MAX_EVENTS,
  reducer,
  type AppState,
} from './reducer';

const msg = (seq: number): SessionEvent => ({
  type: 'message',
  seq,
  at: seq,
  role: 'assistant',
  text: `m${seq}`,
});
const range = (from: number, to: number) =>
  Array.from({ length: to - from + 1 }, (_, i) => msg(from + i));
const server = (state: AppState, m: ServerMsg) => reducer(state, { type: 'server', msg: m });
const withSession = () => server(initialState, { type: 'sessions', sessions: [localSession()] });
const seqs = (state: AppState) => state.views.s1!.events.map((e) => e.seq);

describe('paged history in the reducer', () => {
  it('a gap that starts after seq 1 means earlier events may be fetched', () => {
    let s = withSession();
    s = server(s, { type: 'stream_gap', session_id: 's1', first_available_seq: 941 });
    expect(s.views.s1).toMatchObject({ gapFrom: 941, more: true, events: [] });
    for (const e of range(941, 1000)) s = server(s, { type: 'event', session_id: 's1', event: e });
    expect(s.views.s1!.lastSeq).toBe(1000);
  });

  it('puts a history page in front, ignores overlap and takes has_more from the server', () => {
    let s = withSession();
    s = server(s, { type: 'stream_gap', session_id: 's1', first_available_seq: 101 });
    for (const e of range(101, 160)) s = server(s, { type: 'event', session_id: 's1', event: e });
    s = reducer(s, { type: 'older_loading', sessionId: 's1' });
    expect(s.views.s1!.loadingOlder).toBe(true);
    s = server(s, {
      type: 'history',
      session_id: 's1',
      events: range(45, 105),
      has_more: true,
    });
    expect(seqs(s)).toEqual(range(45, 160).map((e) => e.seq));
    expect(s.views.s1).toMatchObject({ more: true, loadingOlder: false, lastSeq: 160 });
    s = server(s, { type: 'history', session_id: 's1', events: range(1, 44), has_more: false });
    expect(seqs(s)[0]).toBe(1);
    expect(s.views.s1!.more).toBe(false);
    expect(seqs(s)).toHaveLength(160);
  });

  it('a failed page request can be tried again', () => {
    let s = withSession();
    s = reducer(s, { type: 'older_loading', sessionId: 's1' });
    s = reducer(s, { type: 'older_failed', sessionId: 's1' });
    expect(s.views.s1!.loadingOlder).toBe(false);
  });

  it('opening marks only an empty view as loading', () => {
    let s = withSession();
    s = reducer(s, { type: 'view_opening', sessionId: 's1' });
    expect(s.views.s1!.loading).toBe(true);
    s = reducer(s, { type: 'view_opened', sessionId: 's1' });
    expect(s.views.s1!.loading).toBe(false);
    s = server(s, { type: 'event', session_id: 's1', event: msg(1) });
    const again = reducer(s, { type: 'view_opening', sessionId: 's1' });
    expect(again).toBe(s);
  });

  it('dropping the oldest events on overflow makes earlier history fetchable', () => {
    let s = withSession();
    for (const e of range(1, MAX_EVENTS))
      s = server(s, { type: 'event', session_id: 's1', event: e });
    expect(s.views.s1!.more).toBeUndefined();
    s = server(s, { type: 'event', session_id: 's1', event: msg(MAX_EVENTS + 1) });
    expect(s.views.s1!.more).toBe(true);
    // The view offers "load older" only with a gap to load from.
    expect(s.views.s1!.gapFrom).toBe(2);
    expect(seqs(s)[0]).toBe(2);
  });

  it('keeps messages when terminal output overflows the view', () => {
    const output = (seq: number): SessionEvent => ({
      type: 'output',
      seq,
      at: seq,
      data: new Uint8Array([65]),
    });
    let s = withSession();
    s = server(s, { type: 'event', session_id: 's1', event: msg(1) });
    for (let seq = 2; seq <= MAX_EVENTS * 2; seq++) {
      s = server(s, { type: 'event', session_id: 's1', event: output(seq) });
    }
    s = server(s, { type: 'event', session_id: 's1', event: msg(MAX_EVENTS * 2 + 1) });
    const view = s.views.s1!;
    expect(view.events).toHaveLength(MAX_EVENTS);
    expect(view.events[0]!.seq).toBe(1);
    expect(view.events.at(-1)!.seq).toBe(MAX_EVENTS * 2 + 1);
    expect(view.lastSeq).toBe(MAX_EVENTS * 2 + 1);
    expect(view.gapFrom).toBeNull();
  });
});

describe('opening a session in one piece', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const setup = () => {
    const store = createStore(withSession());
    const seen: boolean[] = [];
    store.subscribe(() => seen.push(store.getState().views.s1?.loading ?? false));
    return { store, seen };
  };
  const event = (seq: number): Parameters<ReturnType<typeof createStore>['dispatch']>[0] => ({
    type: 'server',
    msg: { type: 'event', session_id: 's1', event: msg(seq) },
  });

  it('shows the replayed page with a single notification', () => {
    const { store, seen } = setup();
    store.dispatch({ type: 'view_opening', sessionId: 's1' });
    expect(seen).toEqual([true]);
    store.dispatch({
      type: 'server',
      msg: { type: 'stream_gap', session_id: 's1', first_available_seq: 41 },
    });
    for (let i = 41; i <= 100; i++) {
      store.dispatch(event(i));
      vi.advanceTimersByTime(1);
    }
    // The state is current, but nobody was told yet: no streaming in.
    expect(store.getState().views.s1!.events).toHaveLength(60);
    expect(seen).toEqual([true]);
    vi.advanceTimersByTime(OPEN_SETTLE_MS);
    expect(seen).toEqual([true, false]);
    // Live events afterwards are shown at once.
    store.dispatch(event(101));
    expect(seen).toEqual([true, false, false]);
  });

  it('does not hold the page back for ever when events keep coming', () => {
    const { store, seen } = setup();
    store.dispatch({ type: 'view_opening', sessionId: 's1' });
    for (let i = 1; i <= 100; i++) {
      store.dispatch(event(i));
      vi.advanceTimersByTime(OPEN_SETTLE_MS - 1);
      if (seen.length > 1) break;
    }
    expect(seen.at(-1)).toBe(false);
    expect(OPEN_MAX_MS).toBeGreaterThan(OPEN_SETTLE_MS);
  });

  it('stops showing the placeholder for a session with nothing to say', () => {
    const { store, seen } = setup();
    store.dispatch({ type: 'view_opening', sessionId: 's1' });
    vi.advanceTimersByTime(OPEN_EMPTY_MS);
    expect(seen).toEqual([true, false]);
    expect(store.getState().views.s1!.loading).toBe(false);
  });
});

describe('history page size', () => {
  const output = (seq: number): SessionEvent => ({
    type: 'output',
    seq,
    at: seq,
    data: new Uint8Array([120]),
  });

  it('asks for the largest page while the loaded events show nothing in the chat', () => {
    // A managed agent repaints its terminal all the time: pages of output only.
    expect(historyPageSize([output(1), output(2)])).toBe(HISTORY_PAGE_MAX);
    expect(historyPageSize([])).toBe(HISTORY_PAGE_MAX);
  });

  it('asks for a normal page once a message is loaded', () => {
    expect(historyPageSize([output(1), msg(2)])).toBe(HISTORY_PAGE);
  });
});
