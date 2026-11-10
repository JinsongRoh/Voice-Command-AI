/**
 * TypeScript mirror of `vca-core/src/task.rs`: the human task inbox ("할 일")
 * kept by each PC's server and shared by every device.
 */
import type { DangerLevel, SessionId, UnixMillis } from './types';

/** `welcome.features` entry of servers that answer the task messages. */
export const FEATURE_HUMAN_TASKS = 'human_tasks';

export const TASK_MAX_TITLE_CHARS = 200;
export const TASK_MAX_RESULT_CHARS = 8000;
export const TASK_MAX_OUTPUT_CHARS = 16000;
export const TASK_MAX_ATTACHMENTS = 10;

export type TaskSource = 'explicit' | 'extracted' | 'manual';
export type TaskStatus = 'open' | 'in_progress' | 'waiting' | 'done' | 'dismissed';
export type TaskPriority = 'high' | 'normal' | 'low';
export type TaskShell = 'powershell' | 'cmd' | 'bash' | 'zsh' | 'sh' | 'other';

export interface TaskCommand {
  text: string;
  shell: TaskShell;
  needs_admin: boolean;
  danger: DangerLevel;
  reasons: string[];
  /** `[start, end)` UTF-8 byte offsets of the risky parts. */
  spans: [number, number][];
}

export interface TaskResult {
  text: string;
  attachments: string[];
  output: string | null;
  at: UnixMillis;
}

export interface HumanTask {
  id: string;
  session_id: SessionId | null;
  session_name: string | null;
  machine: string;
  title: string;
  /** Markdown. */
  steps: string;
  command: TaskCommand | null;
  source: TaskSource;
  status: TaskStatus;
  priority: TaskPriority;
  blocking: boolean;
  due: UnixMillis | null;
  snoozed_until: UnixMillis | null;
  created_at: UnixMillis;
  updated_at: UnixMillis;
  order: number;
  result: TaskResult | null;
  sent_to_session: boolean;
  sent_at: UnixMillis | null;
  message_seq: number | null;
  /** When the source message was written (absent from older servers). */
  message_at?: UnixMillis | null;
  /** Redacted excerpt of the source message (absent from older tasks). */
  excerpt?: string | null;
  agent_moved_on: boolean;
  stale: boolean;
}

export interface TaskDraft {
  session_id?: SessionId | null;
  title: string;
  steps?: string;
  command?: string | null;
  shell?: TaskShell | null;
  needs_admin?: boolean;
  blocking?: boolean;
  priority?: TaskPriority | null;
  due?: UnixMillis | null;
  message_seq?: number | null;
  /** Without `message_seq`: point at the session's newest agent message. */
  anchor_latest?: boolean;
}

export type TaskChange =
  | { op: 'status'; status: TaskStatus }
  | { op: 'snooze'; until: UnixMillis | null }
  | {
      op: 'edit';
      title?: string | null;
      steps?: string | null;
      command?: string | null;
      shell?: TaskShell | null;
      needs_admin?: boolean | null;
      priority?: TaskPriority | null;
      due?: UnixMillis | null;
      clear_due?: boolean;
    }
  | { op: 'move'; position: number }
  | { op: 'output'; output: string }
  | { op: 'still_needed' };

export interface TaskResultDraft {
  text: string;
  /** Project-relative paths from `attached` answers. */
  attachments: string[];
  /** Mentions the server did not type itself (`attached.typed === false`). */
  mentions: string[];
  /** Pasted output; the stored one when `null`. */
  output: string | null;
}

export type TaskExportFormat = 'markdown' | 'csv';

export type TaskReminderKind = 'due' | 'blocking' | 'stale' | 'agent_moved_on' | 'snooze_ended';

export interface TaskReminder {
  kind: TaskReminderKind;
  task_id: string | null;
  title: string;
  session_id: SessionId | null;
  open: number;
  blocking: number;
  at: UnixMillis;
}

export interface TaskSettings {
  extract: boolean;
  extract_llm: boolean;
  stale_hours: number;
  remind_blocking_minutes: number;
  /** Local hour after which the daily digest is read once; `null`: none. */
  digest_hour: number | null;
}

export type TaskClientMsg =
  /** Every task and the settings (view scope); answered with `tasks`. */
  | { type: 'task_list' }
  /** A manual task (command scope); answered with `task_saved`. */
  | { type: 'task_create'; draft: TaskDraft }
  /** Check off, snooze, edit, move (command scope); answered with `task_saved`. */
  | { type: 'task_update'; task_id: string; change: TaskChange }
  /** "결과 보내기" (command scope); `preview` only composes. Answered with `task_result_sent`. */
  | { type: 'task_send_result'; task_id: string; result: TaskResultDraft; preview: boolean }
  /** Administrator PowerShell on the PC: the PC's own app only. Answered with `ok`. */
  | { type: 'task_open_admin_shell'; task_id: string }
  /** Markdown or CSV (view scope); answered with `task_exported`. */
  | { type: 'task_export'; format: TaskExportFormat; session_id: SessionId | null }
  /** "오늘 할 일" line (view scope); answered with `briefing`. */
  | { type: 'task_digest'; language: string | null }
  /** Settings: the PC's own app only. Answered with `tasks`. */
  | { type: 'task_configure'; settings: TaskSettings };

export type TaskServerMsg =
  | { type: 'tasks'; tasks: HumanTask[]; settings: TaskSettings }
  | { type: 'task_saved'; task: HumanTask }
  | { type: 'task_result_sent'; task: HumanTask; text: string; sent: boolean }
  | { type: 'task_exported'; format: TaskExportFormat; text: string }
  | { type: 'task_reminder'; reminder: TaskReminder };
