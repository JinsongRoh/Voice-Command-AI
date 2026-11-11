/**
 * Pure application state for one connected machine. All server messages go
 * through `reducer` via the `server` action, which makes the protocol
 * handling easy to unit-test.
 */
import type {
  Decision,
  DeliveryStatus,
  PairedDevice,
  ResourceSnapshot,
  ArrangeOverview,
  RestoreJob,
  RestoreOverview,
  ServerMsg,
  SessionEvent,
  SessionId,
  SessionInfo,
} from '../protocol/types';
import { isCloud, sessionNeedsAttention, unreadCount } from '../protocol/helpers';
import type { ConnectionState } from '../protocol/transport';
import {
  closeAlert,
  dropSession,
  emptyAlertBook,
  reopenAlert,
  syncSessions,
  updateAlerts,
  type AlertBook,
} from './approvals';
import type { AlertCategory } from './settings';
import { applyTaskMessage, blockingOpen, type TaskBook } from './tasks';

/** Events kept per session in the client (older ones are dropped). */
export const MAX_EVENTS = 1500;

export type DeckFilter = 'all' | 'attention' | 'local' | 'cloud';

export interface SessionView {
  events: SessionEvent[];
  /** Highest seq applied; used as `from_seq` when resubscribing. */
  lastSeq: number | null;
  /** Set after a `stream_gap`: events before this seq are missing. */
  gapFrom: number | null;
  /** The first page is on its way: show a placeholder, not "no activity". */
  loading?: boolean;
  /**
   * Whether earlier events can still be fetched. Unset after a `stream_gap`
   * until the first history page says; then it is the server's `has_more`.
   */
  more?: boolean;
  /** A history page is being fetched. */
  loadingOlder?: boolean;
}

export interface Delivery {
  status: DeliveryStatus | 'sending' | 'failed';
  at: number;
  error?: string;
}

/** Status of a prompt sent from this client (`sending`/`failed` are client-side). */
export type SentStatus = Delivery['status'];

/**
 * A prompt this client sent to a session, kept locally so the chat view can
 * echo it (the protocol has no event for user input).
 */
/** A file or folder that went out with a prompt (shown in its bubble). */
export interface SentAttachment {
  name: string;
  kind: 'file' | 'folder';
  /** Object URL of the image preview, when it has one. */
  thumbUrl: string | null;
}

export interface SentPrompt {
  /** Client-generated, unique within the store. */
  id: string;
  text: string;
  /** Attachments delivered with the prompt (they left the composer tray). */
  attachments?: SentAttachment[];
  /** Client clock (ms) when the prompt was sent. */
  at: number;
  /** Highest event seq seen when the prompt was sent: it belongs after that event. */
  afterSeq: number | null;
  status: SentStatus;
  error?: string;
}

/** Sent prompts kept per session (older ones are dropped). */
export const MAX_SENT = 100;

export interface Toast {
  id: string;
  category: AlertCategory;
  sessionId: SessionId | null;
  title: string;
  body: string;
  /** Security notice (remote shell opened/closed): shown whatever the settings say. */
  security?: boolean;
  /** The approval alert behind this toast: Go / Later, gone once it is answered. */
  alertKey?: string;
  /** One extra button (for example "Send anyway"); the toast closes after it. */
  action?: { label: string; run: () => void };
}

export interface AppState {
  connection: ConnectionState;
  serverName: string | null;
  /** `welcome.features` of the connected server (empty before the welcome). */
  serverFeatures: string[];
  sessions: Record<SessionId, SessionInfo>;
  /**
   * Card order: the server's (`session_order`, the order the user chose on
   * that computer), new sessions appended unless the server placed them.
   * Session numbers (cards, voice, alerts) are positions in it.
   */
  order: SessionId[];
  /** The last `session_order` from the server; places sessions that arrive later. */
  orderHint: SessionId[] | null;
  /** The user chose the order (the reset action applies). */
  customOrder: boolean;
  selectedId: SessionId | null;
  filter: DeckFilter;
  views: Record<SessionId, SessionView>;
  decisions: Record<SessionId, Decision>;
  delivery: Record<SessionId, Delivery>;
  /** Prompts sent from this client, per session, oldest first. */
  sent: Record<SessionId, SentPrompt[]>;
  resources: ResourceSnapshot | null;
  devices: PairedDevice[] | null;
  toasts: Toast[];
  lastError: string | null;
  /** Sessions that need an answer, in the Alerts menu. */
  alerts: AlertBook;
  /** Set when something asks to open a session at its decision card. */
  jump: { sessionId: SessionId; token: number } | null;
  /** The Windows menu of this machine (`null` until asked for). */
  restore: RestoreOverview | null;
  /** The screen layout tab; `null` until it asked. */
  arrange: ArrangeOverview | null;
  /** The PC's human task list ("To-do"); `null` until the first `tasks`. */
  tasks: TaskBook | null;
  /**
   * Ended sessions the person removed from the list on a server without
   * `dismiss_session`: hidden here until the server drops them itself.
   */
  hidden: SessionId[];
  /** Remote shells about to close for being idle: when (ms, local clock). */
  closingSoon: Record<SessionId, number>;
  /** Shells whose closing is their lifetime cap: Keep open cannot help. */
  closingFinal: SessionId[];
  /**
   * Remote shells another device opened that this device has not joined
   * yet (the server refused to stream them): they need a code first.
   */
  shellJoinNeeded: SessionId[];
}

export const initialState: AppState = {
  connection: 'disconnected',
  serverName: null,
  serverFeatures: [],
  sessions: {},
  order: [],
  orderHint: null,
  customOrder: false,
  selectedId: null,
  filter: 'all',
  views: {},
  decisions: {},
  delivery: {},
  sent: {},
  resources: null,
  devices: null,
  toasts: [],
  lastError: null,
  alerts: emptyAlertBook,
  jump: null,
  restore: null,
  arrange: null,
  tasks: null,
  hidden: [],
  closingSoon: {},
  closingFinal: [],
  shellJoinNeeded: [],
};

export type Action =
  | { type: 'reset' }
  | { type: 'connection'; state: ConnectionState }
  | { type: 'server'; msg: ServerMsg }
  | { type: 'select'; id: SessionId | null }
  | { type: 'filter'; filter: DeckFilter }
  /** The user moved cards: shown at once, the server's answer confirms it. */
  | { type: 'reorder'; order: SessionId[] }
  | { type: 'delivery'; sessionId: SessionId; delivery: Delivery }
  | {
      type: 'prompt_sent';
      sessionId: SessionId;
      id: string;
      text: string;
      at: number;
      attachments?: SentAttachment[];
    }
  | {
      type: 'prompt_status';
      sessionId: SessionId;
      id: string;
      status: SentStatus;
      error?: string;
    }
  | { type: 'decision_answered'; sessionId: SessionId }
  | { type: 'alert_handled'; key: string }
  | { type: 'alert_reopened'; key: string; sessionId: SessionId }
  | { type: 'view_opening'; sessionId: SessionId }
  | { type: 'view_opened'; sessionId: SessionId }
  | { type: 'older_loading'; sessionId: SessionId }
  | { type: 'older_failed'; sessionId: SessionId }
  | { type: 'jump'; sessionId: SessionId }
  | { type: 'jump_done' }
  | { type: 'toast'; toast: Toast }
  /** "Remove from list" of an ended session (local, until the server drops it). */
  | { type: 'hide_session'; id: SessionId }
  /** The person kept a closing shell open (or the notice no longer applies). */
  | { type: 'closing_cleared'; id: SessionId }
  | { type: 'shell_join_needed'; id: SessionId }
  | { type: 'shell_joined'; id: SessionId }
  | { type: 'dismiss_toast'; id: string }
  | { type: 'error'; message: string | null };

const MAX_TOASTS = 4;

/**
 * Whether a toast stays until it is answered: a decision or question alert
 * offering Go / Later. An `input` alert closes by itself like a notice.
 */
export function isStickyToast(toast: Toast): boolean {
  return !!toast.alertKey && (toast.category === 'decision' || toast.category === 'question');
}

/** Kept over notices when the list is full: sticky alerts and security notices. */
function keepsPlace(toast: Toast): boolean {
  return isStickyToast(toast) || !!toast.security;
}

/**
 * Trims the list to `MAX_TOASTS`, oldest first, but a waiting decision or a
 * security notice is never pushed out by a finished task, activity or input
 * notice: those go first, and kept toasts only make room for each other.
 */
function capToasts(toasts: Toast[]): Toast[] {
  const list = [...toasts];
  while (list.length > MAX_TOASTS) {
    const victim = list.findIndex((t) => !keepsPlace(t));
    list.splice(victim >= 0 ? victim : 0, 1);
  }
  return list;
}

/** Later delivery states never go back to earlier ones (acks may arrive out of order). */
const STATUS_RANK: Record<SentStatus, number> = {
  sending: 0,
  queued: 1,
  delivered: 2,
  acknowledged: 3,
  failed: 4,
};

function updatePrompt(
  state: AppState,
  sessionId: SessionId,
  pick: (list: SentPrompt[]) => number,
  status: SentStatus,
  error?: string,
): AppState {
  const list = state.sent[sessionId];
  if (!list || list.length === 0) return state;
  const index = pick(list);
  const prompt = list[index];
  if (!prompt) return state;
  // `failed` only replaces client-side `sending`; server acks only move forward.
  if (
    status === 'failed'
      ? prompt.status !== 'sending'
      : STATUS_RANK[status] <= STATUS_RANK[prompt.status]
  )
    return state;
  const next = [...list];
  next[index] = { ...prompt, status, ...(error === undefined ? {} : { error }) };
  return { ...state, sent: { ...state.sent, [sessionId]: next } };
}

function lastPending(list: SentPrompt[]): number {
  for (let i = list.length - 1; i >= 0; i--) if (list[i]!.status !== 'failed') return i;
  return -1;
}
const emptyView: SessionView = { events: [], lastSeq: null, gapFrom: null };

/**
 * `events` plus `event`, within `MAX_EVENTS`. A managed agent's terminal
 * repaints many times a second, so the oldest output goes first: dropping
 * the oldest event instead would push the whole conversation out during one
 * long turn. The server's event log keeps messages the same way.
 */
function withEvent(events: SessionEvent[], event: SessionEvent): SessionEvent[] {
  if (events.length < MAX_EVENTS) return [...events, event];
  const output = events.findIndex((e) => e.type === 'output');
  const at = Math.max(output, 0);
  return [...events.slice(0, at), ...events.slice(at + 1), event];
}

function applyEvent(state: AppState, sessionId: SessionId, event: SessionEvent): AppState {
  const view = state.views[sessionId] ?? emptyView;
  // Replays after a resubscribe may overlap with what we already have.
  if (view.lastSeq !== null && event.seq <= view.lastSeq) return state;
  const events = withEvent(view.events, event);
  // Losing the first event opens a gap at the front, fetchable as history.
  const trimmed = events[0] !== view.events[0] && view.events.length > 0;
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
    views: {
      ...state.views,
      [sessionId]: {
        ...view,
        events,
        lastSeq: event.seq,
        ...(trimmed ? { more: true, gapFrom: view.gapFrom ?? events[0]!.seq } : {}),
      },
    },
  };
}

/** Puts a page of earlier events (ascending) in front of the loaded ones. */
function prependHistory(
  state: AppState,
  sessionId: SessionId,
  page: SessionEvent[],
  hasMore: boolean,
): AppState {
  const view = state.views[sessionId] ?? emptyView;
  const first = view.events[0]?.seq ?? Number.POSITIVE_INFINITY;
  const older = page.filter((e) => e.seq < first);
  return {
    ...state,
    views: {
      ...state.views,
      [sessionId]: {
        ...view,
        events: older.length > 0 ? [...older, ...view.events] : view.events,
        more: hasMore,
        loadingOlder: false,
      },
    },
  };
}

/** Takes a new alert book; toasts of alerts that went away leave with them. */
function withAlerts(state: AppState, alerts: AlertBook): AppState {
  if (alerts === state.alerts) return state;
  const active = new Set(alerts.active.map((a) => a.key));
  const toasts = state.toasts.filter((t) => !t.alertKey || active.has(t.alertKey));
  return {
    ...state,
    alerts,
    toasts: toasts.length === state.toasts.length ? state.toasts : toasts,
  };
}

/** The idle-close notice of `session` no longer applies (it ended, or input moved the deadline). */
function closingDone(session: SessionInfo, deadline: number): boolean {
  if (session.state.type === 'exited') return true;
  const idle = session.shell?.idle_deadline;
  return idle !== undefined && idle > deadline + 1000;
}

function upsertSession(state: AppState, session: SessionInfo): AppState {
  if (state.hidden.includes(session.id)) {
    // Removed from the list while ended: stays hidden unless it runs again.
    if (session.state.type === 'exited') return state;
    state = { ...state, hidden: state.hidden.filter((x) => x !== session.id) };
  }
  const deadline = state.closingSoon[session.id];
  if (deadline !== undefined && closingDone(session, deadline)) {
    const closingSoon = { ...state.closingSoon };
    delete closingSoon[session.id];
    state = { ...state, closingSoon };
  }
  const known = session.id in state.sessions;
  let decisions = state.decisions;
  if (session.state.type !== 'awaiting_decision' && decisions[session.id]) {
    decisions = { ...decisions };
    delete decisions[session.id];
  } else if (session.state.type === 'awaiting_decision' && session.pending_decision) {
    // The card works without the session's event stream.
    decisions = { ...decisions, [session.id]: session.pending_decision.decision };
  }
  return withAlerts(
    {
      ...state,
      decisions,
      sessions: { ...state.sessions, [session.id]: session },
      order: known ? state.order : placeNew(state.order, state.orderHint, session.id),
    },
    updateAlerts(state.alerts, session),
  );
}

/**
 * Where a new card goes: at its place in the server's order when the server
 * already listed it (an order push can arrive before the card), else last.
 */
function placeNew(order: SessionId[], hint: SessionId[] | null, id: SessionId): SessionId[] {
  const rank = hint?.indexOf(id) ?? -1;
  if (rank < 0) return [...order, id];
  const before = order.findIndex((other) => {
    const r = hint!.indexOf(other);
    return r > rank;
  });
  if (before < 0) return [...order, id];
  return [...order.slice(0, before), id, ...order.slice(before)];
}

/**
 * Puts the known cards in the order of `ids`; cards it does not list keep
 * their relative order after the listed ones.
 */
export function arrangeOrder(order: SessionId[], ids: SessionId[]): SessionId[] {
  const known = new Set(order);
  const listed = ids.filter((id, i) => known.has(id) && ids.indexOf(id) === i);
  const seen = new Set(listed);
  const next = [...listed, ...order.filter((id) => !seen.has(id))];
  return next.every((id, i) => id === order[i]) ? order : next;
}

function removeSession(state: AppState, id: SessionId): AppState {
  if (!(id in state.sessions)) return state;
  const sessions = { ...state.sessions };
  delete sessions[id];
  const views = { ...state.views };
  delete views[id];
  const decisions = { ...state.decisions };
  delete decisions[id];
  const sent = { ...state.sent };
  delete sent[id];
  const closingSoon = { ...state.closingSoon };
  delete closingSoon[id];
  return withAlerts(
    {
      ...state,
      sessions,
      views,
      decisions,
      sent,
      closingSoon,
      order: state.order.filter((x) => x !== id),
      selectedId: state.selectedId === id ? null : state.selectedId,
    },
    dropSession(state.alerts, id),
  );
}

function applyRestoreJob(state: AppState, job: RestoreJob): AppState {
  // A dry run is only a plan: it never shows as the current run.
  if (job.dry_run || !state.restore) return state;
  return { ...state, restore: { ...state.restore, job } };
}

function applyServer(state: AppState, msg: ServerMsg): AppState {
  switch (msg.type) {
    case 'welcome':
      return { ...state, serverName: msg.server_name, serverFeatures: msg.features ?? [] };
    case 'sessions': {
      // Ended sessions removed from the list stay hidden; ids the server no
      // longer has (or that run again) are forgotten.
      const hidden = state.hidden.filter((id) =>
        msg.sessions.some((s) => s.id === id && s.state.type === 'exited'),
      );
      const listed = msg.sessions.filter((s) => !hidden.includes(s.id));
      const sessions: Record<SessionId, SessionInfo> = {};
      for (const s of listed) sessions[s.id] = s;
      // Keep the existing order for known sessions; append new ones.
      const order = [
        ...state.order.filter((id) => id in sessions),
        ...listed.map((s) => s.id).filter((id) => !state.order.includes(id)),
      ];
      const views: Record<SessionId, SessionView> = {};
      for (const id of order) views[id] = state.views[id] ?? emptyView;
      const sent: Record<SessionId, SentPrompt[]> = {};
      for (const id of order) if (state.sent[id]) sent[id] = state.sent[id];
      const closingSoon: Record<SessionId, number> = {};
      for (const [id, at] of Object.entries(state.closingSoon)) {
        const s = sessions[id];
        if (s && !closingDone(s, at)) closingSoon[id] = at;
      }
      const decisions = { ...state.decisions };
      for (const s of listed) {
        if (s.state.type === 'awaiting_decision' && s.pending_decision) {
          decisions[s.id] = s.pending_decision.decision;
        }
      }
      return withAlerts(
        {
          ...state,
          sessions,
          order,
          views,
          sent,
          decisions,
          hidden,
          closingSoon,
          selectedId: state.selectedId && sessions[state.selectedId] ? state.selectedId : null,
        },
        syncSessions(state.alerts, listed),
      );
    }
    case 'session_updated':
    case 'launched':
      return upsertSession(state, msg.session);
    case 'session_removed': {
      const next = removeSession(state, msg.session_id);
      return next.hidden.includes(msg.session_id)
        ? { ...next, hidden: next.hidden.filter((x) => x !== msg.session_id) }
        : next;
    }
    case 'shell_closing_soon':
      if (!state.sessions[msg.session_id]) return state;
      return {
        ...state,
        closingSoon: {
          ...state.closingSoon,
          [msg.session_id]: Date.now() + Math.max(0, msg.seconds_left) * 1000,
        },
        closingFinal:
          msg.reason === 'lifetime'
            ? [...state.closingFinal.filter((x) => x !== msg.session_id), msg.session_id]
            : state.closingFinal.filter((x) => x !== msg.session_id),
      };
    case 'session_order':
      return {
        ...state,
        order: arrangeOrder(state.order, msg.session_ids),
        orderHint: msg.session_ids,
        customOrder: msg.custom,
      };
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
            // The page that follows starts here; anything before it may be
            // fetched (the first history reply tells).
            more: msg.first_available_seq > 1,
            loadingOlder: false,
          },
        },
      };
    }
    case 'history':
      return prependHistory(state, msg.session_id, msg.events, msg.has_more);
    case 'input_ack': {
      const next = {
        ...state,
        delivery: { ...state.delivery, [msg.session_id]: { status: msg.status, at: Date.now() } },
      };
      // Acks are not correlated with a prompt; they refer to the latest input.
      return updatePrompt(next, msg.session_id, lastPending, msg.status);
    }
    case 'resources':
      return { ...state, resources: msg.snapshot };
    case 'devices':
      return { ...state, devices: msg.devices };
    case 'error':
      return { ...state, lastError: `${msg.code}: ${msg.message}` };
    case 'restore_list':
      return { ...state, restore: msg.overview };
    case 'restore_progress':
      return applyRestoreJob(state, msg.job);
    case 'arrange_list':
    case 'arrange_done':
      return { ...state, arrange: msg.overview };
    case 'tasks':
    case 'task_saved':
    case 'task_result_sent':
    case 'task_exported':
    case 'task_reminder': {
      const tasks = applyTaskMessage(state.tasks, msg);
      return tasks === state.tasks ? state : { ...state, tasks };
    }
    // Request/response only: the caller reads these from the reply. Shell
    // notices only raise a toast (alerts.ts); the shell session itself
    // arrives as `launched` / `session_updated`.
    case 'cloud_providers':
    case 'speech_options':
    case 'transcript':
    case 'audio':
    case 'tts_engines':
    case 'briefing':
    case 'summary':
    case 'project_folders':
    case 'folder':
    case 'folder_path':
    case 'scan_report':
    case 'conductor':
    case 'conductor_reply':
    case 'pronunciations':
    case 'personal_terms':
    case 'attach_ready':
    case 'attach_folder_ready':
    case 'attached':
    case 'shell_ran':
    case 'shell_confirm':
    case 'command_danger':
    case 'audit':
    case 'shell_notice':
    case 'pong':
    case 'ok':
      return state;
  }
}

export function reducer(state: AppState, action: Action): AppState {
  switch (action.type) {
    case 'reset':
      // What was alerted or handled stays so, across a change of machine.
      return {
        ...initialState,
        connection: state.connection,
        alerts: { ...emptyAlertBook, seen: state.alerts.seen, handled: state.alerts.handled },
      };
    case 'connection':
      return { ...state, connection: action.state };
    case 'hide_session': {
      const session = state.sessions[action.id];
      if (!session || session.state.type !== 'exited') return state;
      const next = removeSession(state, action.id);
      return { ...next, hidden: [...next.hidden, action.id] };
    }
    case 'shell_join_needed':
      if (state.shellJoinNeeded.includes(action.id)) return state;
      return { ...state, shellJoinNeeded: [...state.shellJoinNeeded, action.id] };
    case 'shell_joined':
      if (!state.shellJoinNeeded.includes(action.id)) return state;
      return { ...state, shellJoinNeeded: state.shellJoinNeeded.filter((id) => id !== action.id) };
    case 'closing_cleared': {
      if (!(action.id in state.closingSoon)) return state;
      const closingSoon = { ...state.closingSoon };
      delete closingSoon[action.id];
      return { ...state, closingSoon };
    }
    case 'server':
      return applyServer(state, action.msg);
    case 'select':
      return { ...state, selectedId: action.id };
    case 'filter':
      return { ...state, filter: action.filter };
    case 'reorder': {
      const order = arrangeOrder(state.order, action.order);
      return order === state.order ? state : { ...state, order };
    }
    case 'delivery':
      return { ...state, delivery: { ...state.delivery, [action.sessionId]: action.delivery } };
    case 'prompt_sent': {
      const prompt: SentPrompt = {
        id: action.id,
        text: action.text,
        at: action.at,
        afterSeq: state.views[action.sessionId]?.lastSeq ?? null,
        status: 'sending',
        ...(action.attachments?.length ? { attachments: action.attachments } : {}),
      };
      const list = [...(state.sent[action.sessionId] ?? []), prompt].slice(-MAX_SENT);
      return { ...state, sent: { ...state.sent, [action.sessionId]: list } };
    }
    case 'prompt_status':
      return updatePrompt(
        state,
        action.sessionId,
        (list) => list.findIndex((p) => p.id === action.id),
        action.status,
        action.error,
      );
    case 'decision_answered': {
      const alert = state.alerts.active.find((a) => a.sessionId === action.sessionId);
      const next = alert ? withAlerts(state, closeAlert(state.alerts, alert.key)) : state;
      if (!next.decisions[action.sessionId]) return next;
      const decisions = { ...next.decisions };
      delete decisions[action.sessionId];
      return { ...next, decisions };
    }
    case 'alert_handled':
      return withAlerts(state, closeAlert(state.alerts, action.key));
    case 'alert_reopened':
      return withAlerts(
        state,
        reopenAlert(state.alerts, action.key, state.sessions[action.sessionId]),
      );
    case 'jump':
      return {
        ...state,
        jump: { sessionId: action.sessionId, token: (state.jump?.token ?? 0) + 1 },
      };
    case 'view_opening': {
      const view = state.views[action.sessionId] ?? emptyView;
      if (view.events.length > 0 || view.loading) return state;
      return {
        ...state,
        views: { ...state.views, [action.sessionId]: { ...view, loading: true } },
      };
    }
    case 'view_opened': {
      const view = state.views[action.sessionId];
      if (!view?.loading) return state;
      return {
        ...state,
        views: { ...state.views, [action.sessionId]: { ...view, loading: false } },
      };
    }
    case 'older_loading': {
      const view = state.views[action.sessionId] ?? emptyView;
      if (view.loadingOlder) return state;
      return {
        ...state,
        views: { ...state.views, [action.sessionId]: { ...view, loadingOlder: true } },
      };
    }
    case 'older_failed': {
      const view = state.views[action.sessionId];
      if (!view?.loadingOlder) return state;
      return {
        ...state,
        views: { ...state.views, [action.sessionId]: { ...view, loadingOlder: false } },
      };
    }
    case 'jump_done':
      return state.jump ? { ...state, jump: null } : state;
    case 'toast': {
      const toasts = [...state.toasts.filter((t) => t.id !== action.toast.id), action.toast];
      return { ...state, toasts: capToasts(toasts) };
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
      // The conductor's own session: only for a pending decision.
      return sessionNeedsAttention(session) || unreadCount(session) > 0;
    case 'local':
      return !isCloud(session.origin);
    case 'cloud':
      return isCloud(session.origin);
  }
}

/**
 * A work session: every session except the conductor's own (the conductor
 * session), which lives in the Command Center and never shows as a card.
 */
export function isWorkSession(session: SessionInfo | undefined): session is SessionInfo {
  return !!session && !session.conductor;
}

let workMemo: {
  order: SessionId[];
  sessions: Record<SessionId, SessionInfo>;
  result: SessionId[];
} | null = null;

/**
 * The card order without the conductor's session: what the deck, the
 * phone carousel, the order panel and every session number ("3번 세션"
 * (session 3), "보코, 3번" (Voco, number 3)) count. Stable while nothing in
 * it changes, so it can be a `useAppState` selector.
 */
export function workOrder(state: Pick<AppState, 'order' | 'sessions'>): SessionId[] {
  if (workMemo && workMemo.order === state.order && workMemo.sessions === state.sessions) {
    return workMemo.result;
  }
  const next = state.order.filter((id) => isWorkSession(state.sessions[id]));
  const previous = workMemo?.result;
  const result =
    previous && previous.length === next.length && previous.every((id, i) => id === next[i])
      ? previous
      : next;
  workMemo = { order: state.order, sessions: state.sessions, result };
  return result;
}

/** The card number of a session (1-based), or `null` (unknown, or the conductor). */
export function sessionNumber(
  state: Pick<AppState, 'order' | 'sessions'>,
  id: SessionId | null | undefined,
): number | null {
  if (!id) return null;
  const index = workOrder(state).indexOf(id);
  return index >= 0 ? index + 1 : null;
}

/**
 * Events asked for per page of earlier history. A managed agent's terminal
 * repaints constantly, so a page may hold nothing the chat shows (output
 * only): while the loaded events show nothing, ask for the most the server
 * gives instead of paging through them 60 at a time (and flashing).
 */
export const HISTORY_PAGE = 60;
export const HISTORY_PAGE_MAX = 500;
export function historyPageSize(events: readonly SessionEvent[]): number {
  return events.some((e) => e.type !== 'output') ? HISTORY_PAGE : HISTORY_PAGE_MAX;
}

export function visibleSessions(state: AppState): SessionInfo[] {
  return workOrder(state)
    .map((id) => state.sessions[id])
    .filter((s): s is SessionInfo => !!s && matchesFilter(s, state.filter));
}

/** The number on the Alerts menu. */
export function alertCount(state: AppState): number {
  return state.alerts.active.length;
}

/** The "Alerts" (alerts and to-dos) badge: pending decisions plus open blocking tasks. */
export function alertsAndTasksCount(state: AppState): number {
  return state.alerts.active.length + blockingOpen(state.tasks).length;
}

export function attentionCount(state: AppState): number {
  return workOrder(state).filter((id) => {
    const s = state.sessions[id];
    return !!s && sessionNeedsAttention(s);
  }).length;
}

/** Whether the session accepts input from this client at all. */
export function canSendInput(session: SessionInfo): boolean {
  return inputBlockReason(session) === null;
}

/**
 * Why the session takes no input: `exited` (it ended; start it again) or
 * `observe` (started in a terminal on the PC, so it can only be watched).
 * `null`: it takes input.
 */
export function inputBlockReason(session: SessionInfo): 'exited' | 'observe' | null {
  if (session.state.type === 'exited') return 'exited';
  if (session.control === 'observe_only') return 'observe';
  return null;
}

/**
 * The agent waits for an answer to a prompt. Text that goes out on its own
 * (voice, hands-free) must not land in that prompt: only text a person typed
 * by hand may, and the server refuses the rest. The Composer keeps such text
 * in the box and points at the decision.
 */
export function awaitsDecision(session: SessionInfo): boolean {
  return !!session.pending_decision || session.state.type === 'awaiting_decision';
}
