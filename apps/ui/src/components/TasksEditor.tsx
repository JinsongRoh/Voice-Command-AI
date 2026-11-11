import { useCallback, useId, useState, type FormEvent, type ReactNode } from 'react';

import { useI18n } from '../i18n/context';
import {
  FEATURE_HUMAN_TASKS,
  TASK_MAX_TITLE_CHARS,
  type HumanTask,
  type TaskPriority,
} from '../protocol/tasks';
import type { SessionId } from '../protocol/types';
import { useActions, useAppState } from '../state/store';
import { commandFromMessage, titleFromMessage } from '../state/tasks';
import { Modal } from './Modal';
import styles from './TasksView.module.css';

const PRIORITIES: TaskPriority[] = ['high', 'normal', 'low'];

/** `datetime-local` value of a timestamp, in local time. */
function toLocalInput(at: number | null): string {
  if (at === null) return '';
  const d = new Date(at);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

export interface TaskPrefill {
  sessionId: SessionId | null;
  title: string;
  steps: string;
  command: string | null;
  messageSeq: number | null;
  /** Point the task at the session's newest agent message (from an alert). */
  anchorLatest?: boolean;
}

/**
 * "New task", "Add to to-do" (with `prefill`) or editing a task (`task`).
 * Nothing in it runs: a command is only stored, classified and shown.
 */
export function TaskEditorDialog({
  task,
  prefill,
  onClose,
}: {
  task?: HumanTask;
  prefill?: TaskPrefill;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const id = useId();
  const [title, setTitle] = useState(task?.title ?? prefill?.title ?? '');
  const [steps, setSteps] = useState(task?.steps ?? prefill?.steps ?? '');
  const [command, setCommand] = useState(task?.command?.text ?? prefill?.command ?? '');
  const [needsAdmin, setNeedsAdmin] = useState(task?.command?.needs_admin ?? false);
  const [blocking, setBlocking] = useState(task?.blocking ?? false);
  const [priority, setPriority] = useState<TaskPriority>(task?.priority ?? 'normal');
  const [due, setDue] = useState(toLocalInput(task?.due ?? null));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e?: FormEvent) => {
    e?.preventDefault();
    if (!title.trim()) {
      setError(t('tasks.editor.needTitle'));
      return;
    }
    const dueAt = due ? new Date(due).getTime() : null;
    setBusy(true);
    try {
      if (task) {
        await actions.task({
          type: 'task_update',
          task_id: task.id,
          change: {
            op: 'edit',
            title,
            steps,
            command: command.trim() === (task.command?.text ?? '') ? null : command,
            needs_admin: needsAdmin,
            priority,
            due: dueAt,
            clear_due: dueAt === null,
          },
        });
      } else {
        await actions.task({
          type: 'task_create',
          draft: {
            session_id: prefill?.sessionId ?? null,
            title,
            steps,
            command: command.trim() ? command : null,
            needs_admin: needsAdmin,
            blocking,
            priority,
            due: dueAt,
            message_seq: prefill?.messageSeq ?? null,
            ...(prefill?.anchorLatest ? { anchor_latest: true } : {}),
          },
        });
        if (prefill)
          actions.showToast({
            id: `task-added:${Date.now()}`,
            category: 'completed',
            sessionId: null,
            title: t('tasks.added'),
            body: '',
            security: true,
          });
      }
      onClose();
    } catch (err) {
      setError((err as Error).message);
      setBusy(false);
    }
  };

  return (
    <Modal
      title={t(task ? 'tasks.editor.editTitle' : 'tasks.editor.newTitle')}
      onClose={onClose}
      dismissOnBackdrop={false}
      fullScreen
      footer={
        <>
          <button type="button" className="btn" onClick={onClose}>
            {t('tasks.editor.cancel')}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy}
            onClick={() => void submit()}
          >
            {t('tasks.editor.save')}
          </button>
        </>
      }
    >
      <form onSubmit={(e) => void submit(e)}>
        <div className="field">
          <label htmlFor={`${id}-title`}>{t('tasks.editor.title')}</label>
          <input
            id={`${id}-title`}
            type="text"
            maxLength={TASK_MAX_TITLE_CHARS}
            value={title}
            onChange={(e) => setTitle(e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor={`${id}-steps`}>{t('tasks.editor.steps')}</label>
          <textarea
            id={`${id}-steps`}
            rows={4}
            value={steps}
            onChange={(e) => setSteps(e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor={`${id}-command`}>{t('tasks.editor.command')}</label>
          <textarea
            id={`${id}-command`}
            rows={2}
            spellCheck={false}
            value={command}
            onChange={(e) => setCommand(e.target.value)}
          />
        </div>
        <label>
          <input
            type="checkbox"
            checked={needsAdmin}
            onChange={(e) => setNeedsAdmin(e.target.checked)}
          />{' '}
          {t('tasks.editor.needsAdmin')}
        </label>
        {!task && (
          <label>
            {' '}
            <input
              type="checkbox"
              checked={blocking}
              onChange={(e) => setBlocking(e.target.checked)}
            />{' '}
            {t('tasks.editor.blocking')}
          </label>
        )}
        <div className="field">
          <label htmlFor={`${id}-priority`}>{t('tasks.priority.label')}</label>
          <select
            id={`${id}-priority`}
            value={priority}
            onChange={(e) => setPriority(e.target.value as TaskPriority)}
          >
            {PRIORITIES.map((p) => (
              <option key={p} value={p}>
                {t(`tasks.priority.${p}`)}
              </option>
            ))}
          </select>
        </div>
        <div className="field">
          <label htmlFor={`${id}-due`}>{t('tasks.editor.due')}</label>
          <input
            id={`${id}-due`}
            type="datetime-local"
            value={due}
            onChange={(e) => setDue(e.target.value)}
          />
        </div>
        {error && (
          <p className={styles.error} role="alert">
            {error}
          </p>
        )}
      </form>
    </Modal>
  );
}

/**
 * "Add to to-do" on a chat message: `open(text, seq)` shows the editor filled
 * from the message. `open` is undefined when the PC's server has no task list.
 */
// eslint-disable-next-line react-refresh/only-export-components
export function useAddTaskFromMessage(sessionId: SessionId): {
  open: ((text: string, seq: number) => void) | undefined;
  dialog: ReactNode;
} {
  const supported = useAppState((s) => s.serverFeatures.includes(FEATURE_HUMAN_TASKS));
  const [prefill, setPrefill] = useState<TaskPrefill | null>(null);
  const open = useCallback(
    (text: string, seq: number) =>
      setPrefill({
        sessionId,
        title: titleFromMessage(text),
        steps: text.length > 2000 ? `${text.slice(0, 1999)}…` : text,
        command: commandFromMessage(text),
        messageSeq: seq,
      }),
    [sessionId],
  );
  return {
    open: supported ? open : undefined,
    dialog: prefill ? (
      <TaskEditorDialog prefill={prefill} onClose={() => setPrefill(null)} />
    ) : null,
  };
}
