/**
 * Pure application state for one connected machine. All server messages go
 * through `reducer` via the `server` action, which makes the protocol
 * handling easy to unit-test.
 */
import type { WireServerMsg } from '../protocol/codec';
import { decodeOutput, isCloud, needsAttention, stripAnsi } from '../protocol/helpers';
import type { PairedDevice } from '../protocol/proposed';
import type { ConnectionState } from '../protocol/transport';
import type {
  Decision,
  DeliveryStatus,
  ResourceSnapshot,
  SessionEvent,
  SessionId,
  SessionInfo,
} from '../protocol/types';
import type { AlertCategory } from './settings';

/** Events kept per session in the client (older ones are dropped). */
export const MAX_EVENTS = 1500;

export type DeckFilter = 'all' | 'attention' | 'local' | 'cloud';

export interface SessionView {
  events: SessionEvent[];
  /** Highest seq applied; used as `from_seq` when resubscribing. */
  lastSeq: number | null;
  /** Set after a `stream_gap`: events before this seq are missing. */
  gapFrom: number | null;
  /** Last line of output or last message, for the card preview. */
  preview: string;
}

export interface Delivery {
  status: DeliveryStatus | 'sending' | 'failed';
  at: number;
  error?: string;
}

export interface Toast {
  id: string;
  category: AlertCategory;
  sessionId: SessionId | null;
  title: string;
  body: string;
}

export interface AppState {
  connection: ConnectionState;
  serverName: string | null;
  sessions: Record<SessionId, SessionInfo>;
  /** Card order (server order, new sessions appended). */
  order: SessionId[];
  selectedId: SessionId | null;
  filter: DeckFilter;
  views: Record<SessionId, SessionView>;
  decisions: Record<SessionId, Decision>;
  delivery: Record<SessionId, Delivery>;
  resources: ResourceSnapshot | null;
  devices: PairedDevice[] | null;
  toasts: Toast[];
  lastError: string | null;
}

export const initialState: AppState = {
  connection: 'disconnected',
  serverName: null,
  sessions: {},
  order: [],
  selectedId: null,
  filter: 'all',
  views: {},
  decisions: {},
  delivery: {},
  resources: null,
  devices: null,
  toasts: [],
  lastError: null,
};

export type Action =
  | { type: 'reset' }
  | { type: 'connection'; state: ConnectionState }
  | { type: 'server'; msg: WireServerMsg }
  | { type: 'select'; id: SessionId | null }
  | { type: 'filter'; filter: DeckFilter }
  | { type: 'delivery'; sessionId: SessionId; delivery: Delivery }
  | { type: 'decision_answered'; sessionId: SessionId }
  | { type: 'toast'; toast: Toast }
  | { type: 'dismiss_toast'; id: string }
  | { type: 'error'; message: string | null };

const MAX_TOASTS = 4;
const emptyView: SessionView = { events: [], lastSeq: null, gapFrom: null, preview: '' };

function previewOf(event: SessionEvent): string | null {
  switch (event.type) {
    case 'output': {
      const lines = stripAnsi(decodeOutput(event.data))
        .split(/\r?\n|\r/)
        .map((l) => l.trim())
        .filter(Boolean);
      return lines[lines.length - 1] ?? null;
    }
    case 'message':
      return event.text.split('\n')[0] ?? null;
    case 'tool_call':
      return `${event.name}: ${event.summary}`;
    case 'decision_requested':
      return event.decision.prompt.split('\n')[0] ?? null;
    case 'notification':
      return `${event.title}: ${event.body}`;
    case 'state_changed':
      return null;
  }
}

function applyEvent(state: AppState, sessionId: SessionId, event: SessionEvent): AppState {
  const view = state.views[sessionId] ?? emptyView;
  // Replays after a resubscribe may overlap with what we already have.
  if (view.lastSeq !== null && event.seq <= view.lastSeq) return state;
  const events =
    view.events.length >= MAX_EVENTS
      ? [...view.events.slice(view.events.length - MAX_EVENTS + 1), event]
      : [...view.events, event];
  const preview = previewOf(event) ?? view.preview;
  let decisions = state.decisions;
  if (event.type === 'decision_requested') {
    decisions = { ...decisions, [sessionId]: event.decision };
  } else if (
    event.type === 'state_changed' &&
    event.state.type !== 'awaiting_decision' &&
    decisions[sessionId]
  ) {
    decisions = { ...decisions };
    delete decisions[sessionId];
  }
  return {
    ...state,
    decisions,
    views: { ...state.views, [sessionId]: { ...view, events, lastSeq: event.seq, preview } },
  };
}

function upsertSession(state: AppState, session: SessionInfo): AppState {
  const known = session.id in state.sessions;
  let decisions = state.decisions;
  if (session.state.type !== 'awaiting_decision' && decisions[session.id]) {
    decisions = { ...decisions };
    delete decisions[session.id];
  }
  return {
    ...state,
    decisions,
    sessions: { ...state.sessions, [session.id]: session },
    order: known ? state.order : [...state.order, session.id],
  };
}

function removeSession(state: AppState, id: SessionId): AppState {
  if (!(id in state.sessions)) return state;
  const sessions = { ...state.sessions };
  delete sessions[id];
  const views = { ...state.views };
  delete views[id];
  const decisions = { ...state.decisions };
  delete decisions[id];
  return {
    ...state,
    sessions,
    views,
    decisions,
    order: state.order.filter((x) => x !== id),
    selectedId: state.selectedId === id ? null : state.selectedId,
  };
}

function applyServer(state: AppState, msg: WireServerMsg): AppState {
  switch (msg.type) {
    case 'welcome':
      return { ...state, serverName: msg.server_name };
    case 'sessions': {
      const sessions: Record<SessionId, SessionInfo> = {};
      for (const s of msg.sessions) sessions[s.id] = s;
      // Keep the existing order for known sessions; append new ones.
      const order = [
        ...state.order.filter((id) => id in sessions),
        ...msg.sessions.map((s) => s.id).filter((id) => !state.order.includes(id)),
      ];
      const views: Record<SessionId, SessionView> = {};
      for (const id of order) views[id] = state.views[id] ?? emptyView;
      return {
        ...state,
        sessions,
        order,
        views,
        selectedId: state.selectedId && sessions[state.selectedId] ? state.selectedId : null,
      };
    }
    case 'session_updated':
    case 'launched':
      return upsertSession(state, msg.session);
    case 'session_removed':
      return removeSession(state, msg.session_id);
    case 'event':
      return applyEvent(state, msg.session_id, msg.event);
    case 'stream_gap': {
      const view = state.views[msg.session_id] ?? emptyView;
      return {
        ...state,
        views: {
          ...state.views,
          // Reload: drop what we have; the replay that follows refills the view.
          [msg.session_id]: {
            ...view,
            events: [],
            lastSeq: null,
            gapFrom: msg.first_available_seq,
          },
        },
      };
    }
    case 'input_ack':
      return {
        ...state,
        delivery: { ...state.delivery, [msg.session_id]: { status: msg.status, at: Date.now() } },
      };
    case 'resources':
      return { ...state, resources: msg.snapshot };
    case 'devices':
      return { ...state, devices: msg.devices };
    case 'error':
      return { ...state, lastError: `${msg.code}: ${msg.message}` };
    case 'pong':
    case 'ok':
      return state;
  }
}

export function reducer(state: AppState, action: Action): AppState {
  switch (action.type) {
    case 'reset':
      return { ...initialState, connection: state.connection };
    case 'connection':
      return { ...state, connection: action.state };
    case 'server':
      return applyServer(state, action.msg);
    case 'select':
      return { ...state, selectedId: action.id };
    case 'filter':
      return { ...state, filter: action.filter };
    case 'delivery':
      return { ...state, delivery: { ...state.delivery, [action.sessionId]: action.delivery } };
    case 'decision_answered': {
      if (!state.decisions[action.sessionId]) return state;
      const decisions = { ...state.decisions };
      delete decisions[action.sessionId];
      return { ...state, decisions };
    }
    case 'toast': {
      const toasts = [...state.toasts.filter((t) => t.id !== action.toast.id), action.toast];
      return { ...state, toasts: toasts.slice(-MAX_TOASTS) };
    }
    case 'dismiss_toast':
      return { ...state, toasts: state.toasts.filter((t) => t.id !== action.id) };
    case 'error':
      return { ...state, lastError: action.message };
  }
}

// ---------------------------------------------------------------- selectors

export function matchesFilter(session: SessionInfo, filter: DeckFilter): boolean {
  switch (filter) {
    case 'all':
      return true;
    case 'attention':
      return needsAttention(session.state) || session.unread > 0;
    case 'local':
      return !isCloud(session.origin);
    case 'cloud':
      return isCloud(session.origin);
  }
}

export function visibleSessions(state: AppState): SessionInfo[] {
  return state.order
    .map((id) => state.sessions[id])
    .filter((s): s is SessionInfo => !!s && matchesFilter(s, state.filter));
}

export function attentionCount(state: AppState): number {
  return state.order.filter((id) => {
    const s = state.sessions[id];
    return !!s && needsAttention(s.state);
  }).length;
}

/** Whether the session accepts input from this client at all. */
export function canSendInput(session: SessionInfo): boolean {
  return session.control !== 'observe_only' && session.state.type !== 'exited';
}
