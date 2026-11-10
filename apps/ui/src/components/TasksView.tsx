import { useEffect, useMemo, useRef, useState } from 'react';

import { useNow } from '../hooks/useNow';
import { useI18n } from '../i18n/context';
import type { MessageKey, TranslateFn } from '../i18n/i18n';
import { isLocalMachine } from '../native/host';
import { useNative } from '../native/context';
import { describeSaveResult, saveTextFile, type SaveTextResult } from '../native/saveFile';
import { isStepUpCanceled } from '../native/security';
import { useStepUp } from './StepUpPrompt';
import type {
  HumanTask,
  TaskChange,
  TaskClientMsg,
  TaskExportFormat,
  TaskSettings,
} from '../protocol/tasks';
import { FEATURE_HUMAN_TASKS } from '../protocol/tasks';
import { RequestError } from '../state/core';
import { workOrder } from '../state/reducer';
import { shouldSpeak, shouldToast } from '../state/settings';
import { useActions, useAppState, useMachines, useSettings } from '../state/store';
import {
  DEFAULT_FILTER,
  digestDue,
  filterTasks,
  isActive,
  isSnoozed,
  markDigestRead,
  nextMorning,
  reminderLine,
  type StatusFilter,
  type TaskFilter,
} from '../state/tasks';
import { copyText } from './clipboard';
import { CommandPreview, DangerBadge, DangerReasons } from './CommandPreview';
import { Icon } from './Icon';
import { TaskSourceActions, useCopyWithToast } from './TaskSource';
import { TaskEditorDialog } from './TasksEditor';
import { SendResultDialog } from './TasksSendResult';
import styles from './TasksView.module.css';

const STATUS_FILTERS: { value: StatusFilter; label: MessageKey }[] = [
  { value: 'active', label: 'tasks.filter.active' },
  { value: 'done', label: 'tasks.filter.done' },
  { value: 'all', label: 'tasks.filter.all' },
];

const HOUR = 3_600_000;

/**
 * The second line of the export toast, whose title already says whether the
 * text was copied: only what happened besides ("Shared vca-tasks.csv.",
 * "Saved to …" or the error). A dismissed share sheet adds nothing, and
 * "this device can't save files" is left out while the text is on the
 * clipboard anyway.
 */
function exportNote(t: TranslateFn, saved: SaveTextResult, copied: boolean): string {
  if (saved.status === 'cancelled') return '';
  if (copied && saved.status === 'error' && saved.error.code === 'save_unsupported') return '';
  return describeSaveResult(t, saved);
}

/**
 * True in the app running on the PC itself (the desktop app connected to its
 * own server, or the simulated PC): only there may an administrator shell open.
 */
// eslint-disable-next-line react-refresh/only-export-components
export function useOnThisPc(): boolean {
  const native = useNative();
  const { machine } = useMachines();
  return (
    (Boolean(native?.host) && isLocalMachine(machine.id)) || Boolean(machine.simulate?.restoreLocal)
  );
}

/** Sends a task request; failures become a toast. */
function useTaskAction() {
  const actions = useActions();
  const stepUp = useStepUp();
  const { t } = useI18n();
  return async (body: TaskClientMsg): Promise<boolean> => {
    try {
      // The settings need the person at this PC; the rest does not.
      if (body.type === 'task_configure') {
        await stepUp.run('task_settings', () => actions.task(body));
      } else {
        await actions.task(body);
      }
      return true;
    } catch (err) {
      if (isStepUpCanceled(err)) return false;
      const message =
        err instanceof RequestError && err.code === 'forbidden'
          ? t('tasks.forbidden')
          : (err as Error).message;
      actions.showToast({
        id: `task-error:${Date.now()}`,
        category: 'error',
        sessionId: null,
        title: t('tasks.error'),
        body: message,
        security: true,
      });
      return false;
    }
  };
}

function TaskRow({
  task,
  here,
  sessionLabel,
  first,
  last,
  position,
}: {
  task: HumanTask;
  here: boolean;
  sessionLabel: string | null;
  first: boolean;
  last: boolean;
  position: number;
}) {
  const { t, relative } = useI18n();
  const actions = useActions();
  const run = useTaskAction();
  const [open, setOpen] = useState(false);
  const [editing, setEditing] = useState(false);
  const [sending, setSending] = useState(false);
  const [output, setOutput] = useState(task.result?.output ?? '');
  const [copied, setCopied] = useState(false);
  const copyWithToast = useCopyWithToast();
  const active = isActive(task.status);
  const now = useNow(60_000);
  const snoozed = isSnoozed(task, now);
  const admin = task.command?.needs_admin ?? false;
  const detailsId = `task-${task.id}`;

  const change = (c: TaskChange) => run({ type: 'task_update', task_id: task.id, change: c });

  const toggleDone = () =>
    void change({ op: 'status', status: task.status === 'done' ? 'open' : 'done' });

  return (
    <li
      className={`panel ${styles.item}`}
      data-blocking={task.blocking && active ? 'true' : undefined}
      data-status={task.status}
      data-testid={`task-${task.id}`}
    >
      <div className={styles.row}>
        <input
          type="checkbox"
          className={styles.check}
          checked={task.status === 'done'}
          disabled={task.status === 'dismissed'}
          aria-label={t(task.status === 'done' ? 'tasks.reopen' : 'tasks.check', {
            title: task.title,
          })}
          onChange={toggleDone}
        />
        <button
          type="button"
          className={styles.open}
          aria-expanded={open}
          aria-controls={detailsId}
          onClick={() => setOpen((v) => !v)}
        >
          <span className={styles.name}>{task.title}</span>
          <span className={styles.meta}>
            {task.blocking && active && (
              <span className={styles.tag} data-kind="blocking">
                {t('tasks.blocking')}
              </span>
            )}
            {admin && (
              <span className={styles.tag} data-kind="admin">
                <Icon name="shield" size={12} /> {t('tasks.admin')}
              </span>
            )}
            {task.command && task.command.danger !== 'safe' && (
              <DangerBadge level={task.command.danger} />
            )}
            {task.priority !== 'normal' && (
              <span className={styles.tag}>{t(`tasks.priority.${task.priority}`)}</span>
            )}
            {task.status !== 'open' && (
              <span className={styles.tag}>{t(`tasks.status.${task.status}`)}</span>
            )}
            <span>{t(`tasks.source.${task.source}`)}</span>
            <span aria-hidden="true">·</span>
            <span>{sessionLabel ?? t('tasks.noSession')}</span>
            <span aria-hidden="true">·</span>
            <time dateTime={new Date(task.created_at).toISOString()}>
              {relative(task.created_at)}
            </time>
            {snoozed && task.snoozed_until !== null && (
              <span className={styles.tag} data-kind="snoozed">
                <Icon name="clock" size={12} />{' '}
                {t('tasks.snoozedUntil', { time: new Date(task.snoozed_until).toLocaleString() })}
              </span>
            )}
          </span>
        </button>
        <Icon name={open ? 'collapse' : 'expand'} size={16} className={styles.chev} />
      </div>
      {active && (task.agent_moved_on || task.stale) && (
        <div className={styles.notice} role="note">
          <span>{t(task.agent_moved_on ? 'tasks.movedOn' : 'tasks.stale')}</span>
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => void change({ op: 'still_needed' })}
          >
            {t('tasks.stillNeeded')}
          </button>
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => void change({ op: 'status', status: 'dismissed' })}
          >
            {t('tasks.dismiss')}
          </button>
        </div>
      )}
      {open && (
        <div className={styles.details} id={detailsId}>
          <TaskSourceActions task={task} />
          {task.steps && (
            <div>
              <span className="label">{t('tasks.steps')}</span>
              <p className={styles.steps}>{task.steps}</p>
            </div>
          )}
          {task.command && (
            <div className={styles.command}>
              <span className="label">{t('tasks.command')}</span>
              <CommandPreview
                command={task.command.text}
                spans={task.command.spans}
                label={t('tasks.command')}
              />
              <DangerReasons reasons={task.command.reasons} />
              <div className={styles.actions}>
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() =>
                    void copyWithToast(task.command!.text).then((ok) => {
                      setCopied(ok);
                      if (ok) setTimeout(() => setCopied(false), 1500);
                    })
                  }
                >
                  <Icon name={copied ? 'check' : 'copy'} size={14} />{' '}
                  {copied ? t('chat.copied') : t('tasks.copy')}
                </button>
                {admin && here && (
                  <button
                    type="button"
                    className="btn btn-sm"
                    onClick={async () => {
                      // The command goes on the clipboard here too.
                      void copyText(task.command!.text);
                      const ok = await run({ type: 'task_open_admin_shell', task_id: task.id });
                      if (ok)
                        actions.showToast({
                          id: `task-admin:${task.id}`,
                          category: 'input',
                          sessionId: null,
                          title: t('tasks.openedAdmin'),
                          body: '',
                          security: true,
                        });
                    }}
                  >
                    <Icon name="shield" size={14} /> {t('tasks.openAdmin')}
                  </button>
                )}
              </div>
              {admin && (
                <p className={styles.hint}>
                  {here ? t('tasks.openAdminHint') : t('tasks.runOnPc')}
                </p>
              )}
              {active && (
                <div className="field">
                  <label htmlFor={`${detailsId}-out`}>{t('tasks.output')}</label>
                  <textarea
                    id={`${detailsId}-out`}
                    rows={3}
                    value={output}
                    onChange={(e) => setOutput(e.target.value)}
                  />
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={output === (task.result?.output ?? '')}
                    onClick={() => void change({ op: 'output', output })}
                  >
                    {t('tasks.saveOutput')}
                  </button>
                </div>
              )}
            </div>
          )}
          {task.result && task.result.text && (
            <p className={styles.result}>
              <span className="label">{t('tasks.send.text')}</span> {task.result.text}
            </p>
          )}
          <div className={styles.actions}>
            {active && task.session_id && (
              <button
                type="button"
                className="btn btn-primary btn-sm"
                onClick={() => setSending(true)}
              >
                <Icon name="send" size={14} /> {t('tasks.sendResult')}
              </button>
            )}
            {active && task.status !== 'in_progress' && (
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => void change({ op: 'status', status: 'in_progress' })}
              >
                {t('tasks.inProgress')}
              </button>
            )}
            {active && (
              <label className={styles.snooze}>
                <Icon name="clock" size={14} />
                <span className="visually-hidden">{t('tasks.snooze')}</span>
                <select
                  aria-label={t('tasks.snooze')}
                  value=""
                  onChange={(e) => {
                    const v = e.target.value;
                    const base = new Date();
                    const until =
                      v === '1h'
                        ? base.getTime() + HOUR
                        : v === '4h'
                          ? base.getTime() + 4 * HOUR
                          : v === 'tomorrow'
                            ? nextMorning(base)
                            : null;
                    void change({ op: 'snooze', until });
                  }}
                >
                  <option value="" disabled>
                    {t('tasks.snooze')}
                  </option>
                  <option value="1h">{t('tasks.snooze1h')}</option>
                  <option value="4h">{t('tasks.snooze4h')}</option>
                  <option value="tomorrow">{t('tasks.snoozeTomorrow')}</option>
                  {snoozed && <option value="none">{t('tasks.unsnooze')}</option>}
                </select>
              </label>
            )}
            {active && (
              <>
                <button
                  type="button"
                  className="icon-btn"
                  disabled={first}
                  aria-label={t('tasks.moveUp')}
                  title={t('tasks.moveUp')}
                  onClick={() => void change({ op: 'move', position: Math.max(0, position - 1) })}
                >
                  <Icon name="up" size={16} />
                </button>
                <button
                  type="button"
                  className="icon-btn"
                  disabled={last}
                  aria-label={t('tasks.moveDown')}
                  title={t('tasks.moveDown')}
                  onClick={() => void change({ op: 'move', position: position + 1 })}
                >
                  <Icon name="down" size={16} />
                </button>
              </>
            )}
            <button type="button" className="btn btn-sm" onClick={() => setEditing(true)}>
              <Icon name="edit" size={14} /> {t('tasks.edit')}
            </button>
            {active && (
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => void change({ op: 'status', status: 'dismissed' })}
              >
                {t('tasks.dismiss')}
              </button>
            )}
          </div>
        </div>
      )}
      {editing && <TaskEditorDialog task={task} onClose={() => setEditing(false)} />}
      {sending && <SendResultDialog task={task} onClose={() => setSending(false)} />}
    </li>
  );
}

function TaskSettingsPanel({ settings }: { settings: TaskSettings }) {
  const { t } = useI18n();
  const run = useTaskAction();
  const save = (patch: Partial<TaskSettings>) =>
    void run({ type: 'task_configure', settings: { ...settings, ...patch } });
  const num = (v: string, max: number) => Math.max(0, Math.min(max, Math.round(Number(v) || 0)));
  return (
    <details className={`panel ${styles.settings}`}>
      <summary>{t('tasks.settings.title')}</summary>
      <label className={styles.toggle}>
        <input
          type="checkbox"
          checked={settings.extract}
          onChange={(e) => save({ extract: e.target.checked })}
        />
        {t('tasks.settings.extract')}
      </label>
      <label className={styles.toggle}>
        <input
          type="checkbox"
          checked={settings.extract_llm}
          disabled={!settings.extract}
          onChange={(e) => save({ extract_llm: e.target.checked })}
        />
        {t('tasks.settings.extractLlm')}
      </label>
      <div className="field">
        <label htmlFor="task-stale">{t('tasks.settings.staleHours')}</label>
        <input
          id="task-stale"
          type="number"
          min={0}
          max={720}
          defaultValue={settings.stale_hours}
          onBlur={(e) => save({ stale_hours: num(e.target.value, 720) })}
        />
      </div>
      <div className="field">
        <label htmlFor="task-blocking">{t('tasks.settings.blockingMinutes')}</label>
        <input
          id="task-blocking"
          type="number"
          min={0}
          max={1440}
          defaultValue={settings.remind_blocking_minutes}
          onBlur={(e) => save({ remind_blocking_minutes: num(e.target.value, 1440) })}
        />
      </div>
      <div className="field">
        <label htmlFor="task-digest">{t('tasks.settings.digestHour')}</label>
        <input
          id="task-digest"
          type="number"
          min={0}
          max={23}
          defaultValue={settings.digest_hour ?? ''}
          onBlur={(e) =>
            save({
              digest_hour: e.target.value.trim() === '' ? null : num(e.target.value, 23),
            })
          }
        />
      </div>
    </details>
  );
}

/** The "할 일" tab: the PC's to-do list from agents. */
export function TasksView() {
  const { t, locale } = useI18n();
  const actions = useActions();
  const book = useAppState((s) => s.tasks);
  const features = useAppState((s) => s.serverFeatures);
  const sessions = useAppState((s) => s.sessions);
  const order = useAppState(workOrder);
  const here = useOnThisPc();
  const [filter, setFilter] = useState<TaskFilter>(DEFAULT_FILTER);
  const [creating, setCreating] = useState(false);
  const [digest, setDigest] = useState<string | null>(null);
  const supported = features.includes(FEATURE_HUMAN_TASKS);

  const list = useMemo(() => filterTasks(book?.list ?? [], filter), [book, filter]);
  const activeIds = useMemo(
    () => (book?.list ?? []).filter((x) => isActive(x.status)).map((x) => x.id),
    [book],
  );
  const sessionIds = useMemo(() => {
    const ids = new Set<string>();
    for (const task of book?.list ?? []) ids.add(task.session_id ?? '');
    return [...ids];
  }, [book]);

  const labelOf = (task: HumanTask): string | null => {
    if (!task.session_id) return null;
    const s = sessions[task.session_id];
    const index = order.indexOf(task.session_id);
    const name = s?.display_name ?? task.session_name ?? task.session_id;
    return index >= 0 ? `${index + 1} · ${name}` : name;
  };

  const exportAs = async (format: TaskExportFormat) => {
    try {
      const res = await actions.task({
        type: 'task_export',
        format,
        session_id: filter.sessionId || null,
      });
      if (res.type !== 'task_exported') return;
      const copying = copyText(res.text);
      const saved = await saveTextFile(
        format === 'csv' ? 'vca-tasks.csv' : 'vca-tasks.md',
        res.text,
      );
      const copied = await copying;
      actions.showToast({
        id: `task-export:${Date.now()}`,
        category:
          copied || saved.status === 'saved' || saved.status === 'shared' ? 'completed' : 'error',
        sessionId: null,
        title: t(copied ? 'tasks.exportCopied' : 'tasks.exportNotCopied'),
        body: exportNote(t, saved, copied),
        security: true,
      });
    } catch (err) {
      actions.showToast({
        id: `task-error:${Date.now()}`,
        category: 'error',
        sessionId: null,
        title: t('tasks.error'),
        body: (err as Error).message,
        security: true,
      });
    }
  };

  const readDigest = async () => {
    try {
      const res = await actions.task({ type: 'task_digest', language: locale });
      if (res.type !== 'briefing') return;
      setDigest(res.text);
      actions.speak(res.text);
    } catch {
      // Shown by the toast of the next action, if any.
    }
  };

  if (!supported) {
    return (
      <section className={styles.view} aria-labelledby="tasks-title">
        <h2 id="tasks-title" className={styles.title}>
          {t('tasks.title')}
        </h2>
        <p className="muted">{t('tasks.unsupported')}</p>
      </section>
    );
  }

  return (
    <section className={styles.view} aria-labelledby="tasks-title">
      <div className={styles.head}>
        <h2 id="tasks-title" className={styles.title}>
          {t('tasks.title')}
        </h2>
        <button type="button" className="btn btn-primary btn-sm" onClick={() => setCreating(true)}>
          <Icon name="plus" size={14} /> {t('tasks.new')}
        </button>
      </div>
      <div className={`panel ${styles.toolbar}`}>
        <div className="segmented" role="group" aria-label={t('tasks.filter.label')}>
          {STATUS_FILTERS.map((f) => (
            <button
              key={f.value}
              type="button"
              aria-pressed={filter.status === f.value}
              onClick={() => setFilter((x) => ({ ...x, status: f.value }))}
            >
              {t(f.label)}
            </button>
          ))}
        </div>
        <select
          aria-label={t('tasks.filter.session')}
          value={filter.sessionId ?? '*'}
          onChange={(e) =>
            setFilter((x) => ({ ...x, sessionId: e.target.value === '*' ? null : e.target.value }))
          }
        >
          <option value="*">{t('tasks.filter.allSessions')}</option>
          {sessionIds.map((id) => (
            <option key={id || 'none'} value={id}>
              {id
                ? (sessions[id]?.display_name ??
                  book?.list.find((x) => x.session_id === id)?.session_name ??
                  id)
                : t('tasks.filter.noSession')}
            </option>
          ))}
        </select>
        <label className={styles.toggle}>
          <input
            type="checkbox"
            checked={filter.adminOnly}
            onChange={(e) => setFilter((x) => ({ ...x, adminOnly: e.target.checked }))}
          />
          {t('tasks.filter.admin')}
        </label>
        <input
          type="search"
          className={styles.search}
          placeholder={t('tasks.search')}
          aria-label={t('tasks.search')}
          value={filter.query}
          onChange={(e) => setFilter((x) => ({ ...x, query: e.target.value }))}
        />
        <span className={styles.spacer} />
        <button type="button" className="btn btn-sm" onClick={() => void readDigest()}>
          <Icon name="speaker" size={14} /> {t('tasks.digest')}
        </button>
        <button type="button" className="btn btn-sm" onClick={() => void exportAs('markdown')}>
          {t('tasks.exportMd')}
        </button>
        <button type="button" className="btn btn-sm" onClick={() => void exportAs('csv')}>
          {t('tasks.exportCsv')}
        </button>
      </div>
      {digest && (
        <p className={styles.digest} role="status">
          {digest}
        </p>
      )}
      {book === null ? (
        <p className="muted">…</p>
      ) : list.length === 0 ? (
        <p className={`muted ${styles.empty}`}>
          {book.list.length === 0 ? t('tasks.empty') : t('tasks.emptyFiltered')}
        </p>
      ) : (
        <ul className={styles.list} aria-label={t('tasks.title')}>
          {list.map((task) => {
            const position = activeIds.indexOf(task.id);
            return (
              <TaskRow
                key={task.id}
                task={task}
                here={here}
                sessionLabel={labelOf(task)}
                position={Math.max(0, position)}
                first={position <= 0}
                last={position < 0 || position === activeIds.length - 1}
              />
            );
          })}
        </ul>
      )}
      {here && book && (
        <TaskSettingsPanel key={JSON.stringify(book.settings)} settings={book.settings} />
      )}
      {creating && <TaskEditorDialog onClose={() => setCreating(false)} />}
    </section>
  );
}

/**
 * Says reminders (generic lines only) and shows them as toasts, and reads the
 * daily digest once after its hour. Mounted once in the app shell.
 */
export function TasksWatcher({
  onOpen,
}: {
  /** Opens the to-do tab. */
  onOpen?: () => void;
}) {
  const { t, locale } = useI18n();
  const actions = useActions();
  const { settings: prefs } = useSettings();
  const prefsRef = useRef(prefs);
  const reminder = useAppState((s) => s.tasks?.reminder ?? null);
  const settings = useAppState((s) => s.tasks?.settings);
  const order = useAppState(workOrder);
  const seen = useRef<string | null>(null);
  const onOpenRef = useRef(onOpen);
  useEffect(() => {
    onOpenRef.current = onOpen;
    prefsRef.current = prefs;
  });

  useEffect(() => {
    if (!reminder) return;
    const key = `${reminder.kind}:${reminder.task_id ?? ''}:${reminder.at}`;
    if (seen.current === key) return;
    seen.current = key;
    const index = reminder.session_id ? order.indexOf(reminder.session_id) : -1;
    const line = reminderLine(t, reminder, index >= 0 ? index + 1 : null);
    const current = prefsRef.current;
    if (shouldToast(current, 'input')) {
      actions.showToast({
        id: `task-reminder:${key}`,
        category: 'input',
        sessionId: null,
        title: line,
        body: reminder.title,
        ...(onOpenRef.current
          ? { action: { label: t('tasks.open'), run: () => onOpenRef.current?.() } }
          : {}),
      });
    }
    // Generic line only: the title may hold commands or paths.
    if (shouldSpeak(current, 'input', new Date())) actions.speak(line);
    // `order` only numbers the line; a new order alone is not a new reminder.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reminder]);

  useEffect(() => {
    if (!settings) return;
    const now = new Date();
    if (!prefsRef.current.spokenAlerts || !digestDue(settings, now)) return;
    markDigestRead(now);
    actions.task({ type: 'task_digest', language: locale }).then(
      (res) => {
        if (res.type === 'briefing') actions.speak(res.text);
      },
      () => {
        // Older server or offline: no digest today.
      },
    );
  }, [settings, actions, locale]);

  return null;
}
