/** Pure helpers of a to-do's source context (see `components/TaskSource`). */
import type { HumanTask } from '../protocol/tasks';
import type { MessageRole, SessionEvent } from '../protocol/types';

/** Messages shown before and after the source message in the sheet. */
export const CONTEXT_AROUND = 3;

export interface ContextMessage {
  seq: number;
  at: number;
  role: MessageRole;
  text: string;
}

/** The task has a source to show (old tasks and plain manual ones do not). */
export function hasSource(task: HumanTask): boolean {
  return (task.session_id !== null && task.message_seq !== null) || !!task.excerpt;
}

/** The task as text: title, steps and command. */
export function taskText(task: HumanTask): string {
  return [task.title, task.steps.trim(), task.command?.text ?? '']
    .filter((part) => part.length > 0)
    .join('\n\n');
}

/** The source message and up to `CONTEXT_AROUND` messages on each side. */
export function messagesAround(events: SessionEvent[], seq: number): ContextMessage[] | null {
  const messages: ContextMessage[] = [];
  for (const e of events) {
    if (e.type === 'message') messages.push({ seq: e.seq, at: e.at, role: e.role, text: e.text });
  }
  const at = messages.findIndex((m) => m.seq === seq);
  if (at < 0) return null;
  return messages.slice(Math.max(0, at - CONTEXT_AROUND), at + CONTEXT_AROUND + 1);
}
