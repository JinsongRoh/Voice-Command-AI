/**
 * Approval alerts: sessions waiting for an answer, as listed in the Alerts
 * menu. Pure, like the reducer that keeps them.
 *
 * Rules: one alert per session; the same decision never alerts twice, also
 * after a reconnect or a change of machine; an alert leaves on its own when
 * the session moves on, whoever answered.
 */
import type { MessageKey, TranslateFn } from '../i18n/i18n';
import type { SessionId, SessionInfo, UnixMillis } from '../protocol/types';

/** `question`: the agent ended its turn asking the person something. */
export type ApprovalKind = 'decision' | 'input' | 'question';

export interface ApprovalAlert {
  key: string;
  sessionId: SessionId;
  kind: ApprovalKind;
  name: string;
  /** What the agent asks (first line); may be empty. */
  request: string;
  at: UnixMillis;
  /** Inferred without agent hooks: answered only in the agent's terminal. */
  inferred?: boolean;
  /** The conductor's own session ("Conductor session"). */
  conductor?: boolean;
}

export interface AlertBook {
  /** One per session, in the order they came. */
  active: ApprovalAlert[];
  /** Keys that already alerted. */
  seen: string[];
  /** Keys the person dealt with. */
  handled: string[];
}

export const emptyAlertBook: AlertBook = { active: [], seen: [], handled: [] };

/** Remembered keys per list; older ones are dropped. */
const MAX_KEYS = 500;

/** Keys of waits without a decision id last only while the session waits. */
function isWaitKey(key: string): boolean {
  return key.startsWith('w:');
}

function waitPrefix(sessionId: SessionId): string {
  return `w:${sessionId}:`;
}

function alertFor(session: SessionInfo, previous?: ApprovalAlert): ApprovalAlert | null {
  const pending = session.pending_decision;
  const kind: ApprovalKind | null =
    pending || session.state.type === 'awaiting_decision'
      ? 'decision'
      : session.question
        ? 'question'
        : session.state.type === 'awaiting_input'
          ? 'input'
          : null;
  if (!kind) return null;
  // The conductor's own session: decisions only; its input waits are turn-ends.
  if (session.conductor && kind !== 'decision') return null;
  if (kind === 'question' && session.question) {
    return {
      key: `q:${session.id}:${session.question.id}`,
      sessionId: session.id,
      kind,
      name: session.display_name,
      request: session.question.text,
      at: session.question.at,
    };
  }
  const key = pending
    ? `d:${session.id}:${pending.decision.id}`
    : `${waitPrefix(session.id)}${kind}`;
  const headline = pending?.decision.prompt.split('\n')[0] ?? '';
  return {
    key,
    sessionId: session.id,
    kind,
    name: session.display_name,
    request: headline || session.preview || '',
    at: pending?.at ?? (previous?.kind === kind ? previous.at : session.last_activity),
    ...(pending?.decision.inferred ? { inferred: true } : {}),
    ...(session.conductor ? { conductor: true } : {}),
  };
}

function remember(list: string[], key: string): string[] {
  return list.includes(key) ? list : [...list, key].slice(-MAX_KEYS);
}

/** Applies one session's new state. Returns `book` itself when nothing changed. */
export function updateAlerts(book: AlertBook, session: SessionInfo): AlertBook {
  const existing = book.active.find((a) => a.sessionId === session.id);
  const next = alertFor(session, existing);
  if (!next) {
    const prefix = waitPrefix(session.id);
    const clear = (keys: string[]) => keys.filter((k) => !k.startsWith(prefix));
    const seen = clear(book.seen);
    const handled = clear(book.handled);
    if (!existing && seen.length === book.seen.length && handled.length === book.handled.length) {
      return book;
    }
    return { active: book.active.filter((a) => a !== existing), seen, handled };
  }
  // The decision details of a wait already on the list arrived: same alert.
  const refined =
    existing && existing.kind === next.kind && isWaitKey(existing.key) && !isWaitKey(next.key);
  if (book.handled.includes(next.key) || (refined && book.handled.includes(existing.key))) {
    return existing ? { ...book, active: book.active.filter((a) => a !== existing) } : book;
  }
  const same =
    existing &&
    existing.key === next.key &&
    existing.name === next.name &&
    existing.request === next.request &&
    existing.at === next.at &&
    existing.inferred === next.inferred &&
    existing.conductor === next.conductor;
  if (same) return book;
  return {
    ...book,
    active: existing ? book.active.map((a) => (a === existing ? next : a)) : [...book.active, next],
    seen: remember(book.seen, next.key),
  };
}

export function dropSession(book: AlertBook, id: SessionId): AlertBook {
  if (!book.active.some((a) => a.sessionId === id)) return book;
  return { ...book, active: book.active.filter((a) => a.sessionId !== id) };
}

/** Applies a whole session list (start, reconnect). */
export function syncSessions(book: AlertBook, sessions: SessionInfo[]): AlertBook {
  const known = new Set(sessions.map((s) => s.id));
  let next = book.active.every((a) => known.has(a.sessionId))
    ? book
    : { ...book, active: book.active.filter((a) => known.has(a.sessionId)) };
  for (const s of sessions) next = updateAlerts(next, s);
  return next;
}

/** The alert is dealt with: it leaves and does not come back. */
export function closeAlert(book: AlertBook, key: string): AlertBook {
  if (!book.active.some((a) => a.key === key)) return book;
  return {
    ...book,
    active: book.active.filter((a) => a.key !== key),
    handled: remember(book.handled, key),
  };
}

/**
 * Undo of "Mark handled": the alert comes back if its session still waits
 * for the same thing. It is not announced again (its key stays seen).
 */
export function reopenAlert(
  book: AlertBook,
  key: string,
  session: SessionInfo | undefined,
): AlertBook {
  if (!book.handled.includes(key)) return book;
  const next = { ...book, handled: book.handled.filter((k) => k !== key) };
  return session ? updateAlerts(next, session) : next;
}

/** Alerts in `after` the person has not been told about. */
export function freshAlerts(before: AlertBook, after: AlertBook): ApprovalAlert[] {
  if (before === after) return [];
  return after.active.filter(
    (a) => !before.seen.includes(a.key) && !before.active.some((b) => b.sessionId === a.sessionId),
  );
}

/** Newest first, as the Alerts menu lists them. */
export function newestFirst(book: AlertBook): ApprovalAlert[] {
  return [...book.active].sort((a, b) => b.at - a.at);
}

export function approvalTitle(alert: ApprovalAlert, t: TranslateFn): string {
  const key: MessageKey =
    alert.kind === 'input'
      ? 'alert.input'
      : alert.kind === 'question'
        ? 'alert.agentQuestion'
        : alert.inferred
          ? 'alert.decisionInferred'
          : 'alert.decision';
  return t(key, { name: alert.name });
}
