/**
 * The human task inbox on the client: the PC's list as the server pushes it
 * (`tasks`, `task_saved`, `task_result_sent`), filters, the combined badge of
 * the "알림 · 할 일" menu and the generic spoken reminder lines.
 *
 * Reminders are read aloud with generic lines only (the session number or
 * a count): task titles come from agents and may hold commands or paths, so
 * they are shown but never spoken.
 */
import type { MessageKey, Vars } from '../i18n/i18n';
import type {
  HumanTask,
  TaskReminder,
  TaskServerMsg,
  TaskSettings,
  TaskStatus,
} from '../protocol/tasks';
import type { SessionId } from '../protocol/types';

export interface TaskBook {
  list: HumanTask[];
  settings: TaskSettings;
  /** The newest reminder pushed by the server (spoken and shown once). */
  reminder: TaskReminder | null;
}

export function isActive(status: TaskStatus): boolean {
  return status === 'open' || status === 'in_progress' || status === 'waiting';
}

const PRIORITY_RANK = { high: 0, normal: 1, low: 2 } as const;

/** Active first (priority, manual order, age); closed ones newest first. */
export function sortTasks(list: readonly HumanTask[]): HumanTask[] {
  return [...list].sort((a, b) => {
    const aa = isActive(a.status);
    const ba = isActive(b.status);
    if (aa !== ba) return aa ? -1 : 1;
    if (!aa) return b.updated_at - a.updated_at;
    return (
      PRIORITY_RANK[a.priority] - PRIORITY_RANK[b.priority] ||
      a.order - b.order ||
      a.created_at - b.created_at
    );
  });
}

function upsert(book: TaskBook | null, task: HumanTask): TaskBook | null {
  // Before the first list there is nothing to merge into: the list follows.
  if (!book) return book;
  const index = book.list.findIndex((t) => t.id === task.id);
  const list = [...book.list];
  if (index >= 0) list[index] = task;
  else list.push(task);
  return { ...book, list: sortTasks(list) };
}

/** Applies a task message to the book (`null` until the first list). */
export function applyTaskMessage(book: TaskBook | null, msg: TaskServerMsg): TaskBook | null {
  switch (msg.type) {
    case 'tasks':
      return {
        list: sortTasks(msg.tasks),
        settings: msg.settings,
        reminder: book?.reminder ?? null,
      };
    case 'task_saved':
    case 'task_result_sent':
      return upsert(book, msg.task);
    case 'task_reminder':
      return book ? { ...book, reminder: msg.reminder } : book;
    case 'task_exported':
      return book;
  }
}

/** Snoozed until later than `now`. */
export function isSnoozed(task: HumanTask, now: number): boolean {
  return task.snoozed_until !== null && task.snoozed_until > now;
}

/** Open tasks an agent waits on (not snoozed): part of the menu badge. */
export function blockingOpen(book: TaskBook | null, now: number = Date.now()): HumanTask[] {
  return (book?.list ?? []).filter((t) => isActive(t.status) && t.blocking && !isSnoozed(t, now));
}

/** The "agent is waiting on you" count of one session card. */
export function waitingOnYou(book: TaskBook | null, sessionId: SessionId): number {
  return blockingOpen(book).filter((t) => t.session_id === sessionId).length;
}

export type StatusFilter = 'active' | 'done' | 'all';

export interface TaskFilter {
  status: StatusFilter;
  /** `null`: every session; `''`: tasks without a session. */
  sessionId: SessionId | null;
  adminOnly: boolean;
  query: string;
}

export const DEFAULT_FILTER: TaskFilter = {
  status: 'active',
  sessionId: null,
  adminOnly: false,
  query: '',
};

export function filterTasks(list: readonly HumanTask[], f: TaskFilter): HumanTask[] {
  const q = f.query.trim().toLowerCase();
  return list.filter((t) => {
    if (f.status === 'active' && !isActive(t.status)) return false;
    if (f.status === 'done' && isActive(t.status)) return false;
    if (f.sessionId !== null && (t.session_id ?? '') !== f.sessionId) return false;
    if (f.adminOnly && !t.command?.needs_admin) return false;
    if (q) {
      const hay = [t.title, t.steps, t.command?.text ?? '', t.session_name ?? '']
        .join('\n')
        .toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });
}

/** The spoken line of a reminder: generic, never the task's text. */
export function reminderLine(
  t: (key: MessageKey, vars?: Vars) => string,
  reminder: TaskReminder,
  number: number | null,
): string {
  const who = number !== null ? t('tasks.speak.session', { n: number }) : t('tasks.speak.agent');
  switch (reminder.kind) {
    case 'blocking':
      return t('tasks.speak.blocking', { who });
    case 'due':
      return t('tasks.speak.due', { who });
    case 'stale':
      return t('tasks.speak.stale', { count: reminder.open });
    case 'agent_moved_on':
      return t('tasks.speak.movedOn', { who });
    case 'snooze_ended':
      return t('tasks.speak.snoozeEnded', { who });
  }
}

/** The next local `hour`:00 after `now` ("tomorrow morning" snooze). */
export function nextMorning(now: Date, hour = 9): number {
  const d = new Date(now);
  d.setHours(hour, 0, 0, 0);
  if (d.getTime() <= now.getTime()) d.setDate(d.getDate() + 1);
  return d.getTime();
}

/** Local `YYYY-MM-DD`. */
export function localDay(now: Date): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${now.getFullYear()}-${p(now.getMonth() + 1)}-${p(now.getDate())}`;
}

const DIGEST_KEY = 'vca.tasks.digestDay';

/** The daily digest is due: past the hour and not read today on this device. */
export function digestDue(
  settings: TaskSettings | undefined,
  now: Date,
  storage: Pick<Storage, 'getItem'> | null = typeof localStorage === 'undefined'
    ? null
    : localStorage,
): boolean {
  if (!settings || settings.digest_hour === null) return false;
  if (now.getHours() < settings.digest_hour) return false;
  return storage?.getItem(DIGEST_KEY) !== localDay(now);
}

export function markDigestRead(
  now: Date,
  storage: Pick<Storage, 'setItem'> | null = typeof localStorage === 'undefined'
    ? null
    : localStorage,
): void {
  try {
    storage?.setItem(DIGEST_KEY, localDay(now));
  } catch {
    // Private mode: the digest may come again.
  }
}

/** A short code block-safe excerpt of a chat message for "할 일로 추가". */
export function titleFromMessage(text: string, max = 120): string {
  const line =
    text
      .split('\n')
      .map((l) => l.replace(/^[#>*\-\s\d.)]+/, '').trim())
      .find((l) => l.length > 0 && !l.startsWith('```')) ?? '';
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/** The first fenced code block of a message, for the task's command. */
export function commandFromMessage(text: string): string | null {
  const match = /```[^\n]*\n([\s\S]*?)```/.exec(text);
  const body = match?.[1]?.trim();
  return body ? body : null;
}
