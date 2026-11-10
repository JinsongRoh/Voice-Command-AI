/**
 * The Command Center's Approvals inbox (Phase A): what the conductor raised
 * for the person, built on the client from data the app already has. Other
 * sessions' approvals, questions and to-dos stay under Alerts · To-do.
 *
 * - other sessions' decisions the conductor escalated, or that VCA raised
 *   because the conductor did not handle them in time
 *   (`ConductorState.escalations`); the raw alerts stay under Alerts;
 * - the conductor's own permission decisions (`state/approvals.ts`);
 * - the sends the conductor wants to make (`ConductorState.pending`);
 * - the first-run "trust this folder" question of the conductor's session;
 * - to-dos the conductor's session is blocked on (`state/tasks.ts`).
 *
 * Pure: answering goes through the existing paths (decision options,
 * `conductor_decide`, opening the session), so the safety rules stay where
 * they are. A later server (`cc_approvals`) replaces the source, not the
 * view model.
 */
import type { HumanTask } from '../protocol/tasks';
import type {
  ConductorEscalation,
  ConductorRequest,
  ConductorState,
  SessionId,
  SessionInfo,
  UnixMillis,
} from '../protocol/types';
import type { ApprovalAlert } from './approvals';
import { conductorSessionWait } from './conductor';
import { blockingOpen, type TaskBook } from './tasks';

export type InboxKind =
  'escalation' | 'decision' | 'question' | 'input' | 'conductor_send' | 'conductor_trust' | 'task';

export interface InboxItem {
  /**
   * Stable and unique: the alert key, `esc:<id>`, `cr:<id>`, `trust:<session>`
   * or `task:<id>`.
   */
  key: string;
  kind: InboxKind;
  /** The session (or, later, department) it is about; `null`: none. */
  sessionId: SessionId | null;
  /** Its name as shown ("Orbit 3D"); empty when unknown. */
  name: string;
  /** What is asked (first line); may be empty. */
  text: string;
  at: UnixMillis;
  /** About the conductor's own session ("Conductor session"). */
  conductor?: boolean;
  /** A decision inferred without agent hooks: answered in the terminal only. */
  inferred?: boolean;
  /** The source record, for the actions. */
  alert?: ApprovalAlert;
  request?: ConductorRequest;
  task?: HumanTask;
  escalation?: ConductorEscalation;
}

export interface InboxSources {
  alerts: readonly ApprovalAlert[];
  conductor: ConductorState | null;
  tasks: TaskBook | null;
  sessions: Readonly<Record<SessionId, SessionInfo | undefined>>;
  now?: UnixMillis;
}

const RANK: Record<InboxKind, number> = {
  // Conductor sends expire on their own: first.
  conductor_send: 0,
  conductor_trust: 1,
  escalation: 2,
  decision: 2,
  question: 3,
  input: 4,
  task: 5,
};

/**
 * An escalation still about the decision its session waits on. The server
 * drops stale ones too; this hides one at once when the decision was just
 * answered here. Unknown sessions keep it (the list may still be loading).
 */
function escalationCurrent(
  escalation: ConductorEscalation,
  sessions: Readonly<Record<SessionId, SessionInfo | undefined>>,
): boolean {
  const session = sessions[escalation.session_id];
  if (!session) return true;
  return session.pending_decision?.decision.id === escalation.decision_id;
}

export function buildInbox({
  alerts,
  conductor,
  tasks,
  sessions,
  now = Date.now(),
}: InboxSources): InboxItem[] {
  const items = new Map<string, InboxItem>();
  const add = (item: InboxItem) => {
    if (!items.has(item.key)) items.set(item.key, item);
  };
  const conductorId = conductor?.session_id ?? null;
  for (const alert of alerts) {
    const isConductor =
      !!alert.conductor ||
      alert.sessionId === conductorId ||
      !!sessions[alert.sessionId]?.conductor;
    // Only what the conductor raised: other sessions' alerts stay under
    // Alerts. The conductor's input waits are turn-ends, not questions.
    if (!isConductor || alert.kind !== 'decision') continue;
    add({
      key: alert.key,
      kind: alert.kind,
      sessionId: alert.sessionId,
      name: alert.name,
      text: alert.request,
      at: alert.at,
      ...(isConductor ? { conductor: true } : {}),
      ...(alert.inferred ? { inferred: true } : {}),
      alert,
    });
  }
  // Escalations exist only while the conductor is on (the server sends none
  // otherwise); a disabled conductor's leftovers are not shown either.
  const conductorOn = !!conductor?.settings.enabled;
  for (const escalation of conductorOn ? (conductor.escalations ?? []) : []) {
    if (!escalationCurrent(escalation, sessions)) continue;
    add({
      key: `esc:${escalation.id}`,
      kind: 'escalation',
      sessionId: escalation.session_id,
      name: sessions[escalation.session_id]?.display_name ?? escalation.session_name,
      text: escalation.summary || escalation.request.split('\n')[0] || '',
      at: escalation.created_at,
      escalation,
    });
  }
  for (const request of conductor?.pending ?? []) {
    add({
      key: `cr:${request.id}`,
      kind: 'conductor_send',
      sessionId: request.session_id,
      name: request.session_name,
      text: request.text,
      at: request.created_at,
      request,
    });
  }
  const wait = conductorSessionWait(conductor, sessions);
  if (wait?.kind === 'trust') {
    add({
      key: `trust:${wait.session.id}`,
      kind: 'conductor_trust',
      sessionId: wait.session.id,
      name: wait.session.display_name,
      text: '',
      at: wait.session.last_activity,
      conductor: true,
    });
  }
  for (const task of blockingOpen(tasks, now)) {
    // Tasks found in other sessions' messages belong to Alerts · To-do.
    if (!conductorId || task.session_id !== conductorId) continue;
    add({
      key: `task:${task.id}`,
      kind: 'task',
      sessionId: task.session_id,
      name: task.session_name ?? '',
      text: task.title,
      at: task.updated_at,
      task,
    });
  }
  return [...items.values()].sort((a, b) => RANK[a.kind] - RANK[b.kind] || b.at - a.at);
}

/**
 * Items the person put off ("Later") on this device: they move to the
 * bottom of the inbox and leave the counts until brought back. In memory
 * only; an item that leaves the inbox (answered anywhere) is forgotten.
 */
export interface DeferredStore {
  get(): ReadonlySet<string>;
  set(keys: ReadonlySet<string>): void;
  subscribe(listener: () => void): () => void;
}

export function createDeferredStore(): DeferredStore {
  let keys: ReadonlySet<string> = new Set();
  const listeners = new Set<() => void>();
  return {
    get: () => keys,
    set(next) {
      keys = next;
      for (const l of listeners) l();
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

export const deferredItems = createDeferredStore();

export function deferItem(key: string, later: boolean, store: DeferredStore = deferredItems): void {
  const next = new Set(store.get());
  if (later) next.add(key);
  else next.delete(key);
  store.set(next);
}
