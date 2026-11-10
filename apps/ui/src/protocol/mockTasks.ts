/**
 * The to-do list of the mock server: the same rules as vcad (commands are
 * stored and shown, never run; the administrator shell only from the PC's
 * own app; results go to the asking session and check the task off) with a
 * couple of seeded requests.
 */
import type { HumanTask, TaskChange, TaskClientMsg, TaskResultDraft, TaskSettings } from './tasks';
import type { ErrorCode, ServerMsg, SessionInfo, UnixMillis } from './types';

export interface MockTasksOptions {
  /** This client is the desktop app of the PC. Default true. */
  local?: boolean;
  /** Initial tasks (default: two seeded requests). */
  seed?: HumanTask[];
}

export function defaultTaskSettings(): TaskSettings {
  return {
    extract: true,
    extract_llm: false,
    stale_hours: 8,
    remind_blocking_minutes: 30,
    digest_hour: null,
  };
}

/** A task with defaults, for seeds and tests. */
export function mockTask(over: Partial<HumanTask> & { id: string; title: string }): HumanTask {
  return {
    session_id: null,
    session_name: null,
    machine: 'mock',
    steps: '',
    command: null,
    source: 'manual',
    status: 'open',
    priority: 'normal',
    blocking: false,
    due: null,
    snoozed_until: null,
    created_at: 0,
    updated_at: 0,
    order: 0,
    result: null,
    sent_to_session: false,
    sent_at: null,
    message_seq: null,
    message_at: null,
    excerpt: null,
    agent_moved_on: false,
    stale: false,
    ...over,
  };
}

const ACTIVE = new Set(['open', 'in_progress', 'waiting']);

export class MockTasks {
  private seeded: HumanTask[] | null;
  private settings = defaultTaskSettings();
  private counter = 0;
  private readonly local: boolean;

  constructor(
    private readonly now: () => UnixMillis,
    private readonly push: (body: ServerMsg) => void,
    private readonly sessions: () => SessionInfo[],
    private readonly deliver: (sessionId: string, text: string) => boolean,
    options: MockTasksOptions = {},
  ) {
    this.local = options.local ?? true;
    this.seeded = options.seed ?? null;
  }

  /** Seeded on first use, when the mock sessions exist. */
  private get tasks(): HumanTask[] {
    if (this.seeded === null) this.seeded = this.seed();
    return this.seeded;
  }

  private set tasks(list: HumanTask[]) {
    this.seeded = list;
  }

  private seed(): HumanTask[] {
    const at = this.now();
    const s = this.sessions()[0];
    return [
      mockTask({
        id: 'task_seed_admin',
        title: 'Run the driver installer in an administrator PowerShell',
        session_id: s?.id ?? null,
        session_name: s?.display_name ?? null,
        source: 'extracted',
        // Not blocking: the mock's menu badge stays the decisions' count.
        priority: 'high',
        steps: '- The agent cannot raise its own rights.',
        command: {
          text: 'pnputil /add-driver .\\driver\\device.inf /install',
          shell: 'powershell',
          needs_admin: true,
          danger: 'caution',
          reasons: [],
          spans: [],
        },
        created_at: at - 20 * 60_000,
        updated_at: at - 20 * 60_000,
      }),
      mockTask({
        id: 'task_seed_login',
        title: 'Sign in to the cloud console and approve the deploy key',
        session_id: s?.id ?? null,
        session_name: s?.display_name ?? null,
        source: 'explicit',
        steps: '1. Open <https://console.example.com>\n2. Approve the pending key',
        order: 1,
        created_at: at - 3 * 3_600_000,
        updated_at: at - 3 * 3_600_000,
      }),
    ];
  }

  private error(code: ErrorCode, message: string): ServerMsg {
    return { type: 'error', code, message };
  }

  private list(): ServerMsg {
    return { type: 'tasks', tasks: [...this.tasks], settings: { ...this.settings } };
  }

  private changed(): void {
    this.push(this.list());
  }

  private find(id: string): HumanTask | undefined {
    return this.tasks.find((t) => t.id === id);
  }

  private replace(task: HumanTask): HumanTask {
    this.tasks = this.tasks.map((t) => (t.id === task.id ? task : t));
    this.changed();
    return task;
  }

  private apply(task: HumanTask, change: TaskChange): HumanTask | string {
    const at = this.now();
    switch (change.op) {
      case 'status':
        return { ...task, status: change.status, updated_at: at };
      case 'snooze':
        if (change.until !== null && change.until <= at) return 'a snooze must end in the future';
        return { ...task, snoozed_until: change.until, updated_at: at };
      case 'edit': {
        const command =
          change.command === undefined || change.command === null
            ? task.command
              ? { ...task.command, needs_admin: change.needs_admin ?? task.command.needs_admin }
              : null
            : change.command.trim()
              ? {
                  text: change.command.trim(),
                  shell: change.shell ?? task.command?.shell ?? 'other',
                  needs_admin: change.needs_admin ?? task.command?.needs_admin ?? false,
                  danger: /rm\s+-rf|Remove-Item.*-Recurse|format\s/i.test(change.command)
                    ? ('dangerous' as const)
                    : ('safe' as const),
                  reasons: [],
                  spans: [],
                }
              : null;
        return {
          ...task,
          title: change.title?.trim() || task.title,
          steps: change.steps ?? task.steps,
          command,
          priority: change.priority ?? task.priority,
          due: change.clear_due ? null : (change.due ?? task.due),
          updated_at: at,
        };
      }
      case 'move': {
        const active = this.tasks
          .filter((t) => ACTIVE.has(t.status) && t.id !== task.id)
          .sort((a, b) => a.order - b.order);
        active.splice(Math.min(change.position, active.length), 0, task);
        active.forEach((t, i) => {
          if (t.id !== task.id)
            this.tasks = this.tasks.map((x) => (x.id === t.id ? { ...x, order: i } : x));
        });
        return { ...task, order: active.indexOf(task), updated_at: at };
      }
      case 'output':
        return {
          ...task,
          result: {
            text: task.result?.text ?? '',
            attachments: task.result?.attachments ?? [],
            output: change.output.trim() ? change.output : null,
            at,
          },
          updated_at: at,
        };
      case 'still_needed':
        return { ...task, agent_moved_on: false, stale: false, created_at: at, updated_at: at };
    }
  }

  private compose(task: HumanTask, r: TaskResultDraft): string {
    const parts = [`Result of the task you asked me to do: "${task.title}"`];
    if (r.text.trim()) parts.push(r.text.trim());
    const output = r.output ?? task.result?.output ?? null;
    if (output?.trim()) parts.push(`Output:\n\`\`\`\n${output.trimEnd()}\n\`\`\``);
    parts.push(...r.mentions);
    return parts.join('\n\n');
  }

  handle(msg: TaskClientMsg): ServerMsg {
    switch (msg.type) {
      case 'task_list':
        return this.list();
      case 'task_create': {
        if (!msg.draft.title.trim()) return this.error('invalid_request', 'a task needs a title');
        const at = this.now();
        this.counter += 1;
        const session = this.sessions().find((s) => s.id === msg.draft.session_id);
        const task = mockTask({
          id: `task_mock_${this.counter}`,
          title: msg.draft.title.trim(),
          steps: msg.draft.steps ?? '',
          session_id: msg.draft.session_id ?? null,
          session_name: session?.display_name ?? null,
          blocking: msg.draft.blocking ?? false,
          priority: msg.draft.priority ?? (msg.draft.blocking ? 'high' : 'normal'),
          due: msg.draft.due ?? null,
          message_seq: msg.draft.message_seq ?? null,
          command: msg.draft.command?.trim()
            ? {
                text: msg.draft.command.trim(),
                shell: msg.draft.shell ?? 'other',
                needs_admin: msg.draft.needs_admin ?? false,
                danger: 'safe',
                reasons: [],
                spans: [],
              }
            : null,
          order: this.tasks.length,
          created_at: at,
          updated_at: at,
        });
        this.tasks = [...this.tasks, task];
        this.changed();
        return { type: 'task_saved', task };
      }
      case 'task_update': {
        const task = this.find(msg.task_id);
        if (!task) return this.error('not_found', `no task ${msg.task_id}`);
        const next = this.apply(task, msg.change);
        if (typeof next === 'string') return this.error('invalid_request', next);
        return { type: 'task_saved', task: this.replace(next) };
      }
      case 'task_send_result': {
        const task = this.find(msg.task_id);
        if (!task) return this.error('not_found', `no task ${msg.task_id}`);
        const r = msg.result;
        if (!r.text.trim() && !r.output?.trim() && r.attachments.length === 0)
          return this.error('invalid_request', 'write a result, paste output or attach a file');
        const text = this.compose(task, r);
        if (msg.preview) return { type: 'task_result_sent', task, text, sent: false };
        if (!task.session_id || !this.deliver(task.session_id, text))
          return this.error('invalid_request', 'this task has no session to send the result to');
        const at = this.now();
        const done = this.replace({
          ...task,
          status: 'done',
          sent_to_session: true,
          sent_at: at,
          updated_at: at,
          result: { text: r.text.trim(), attachments: r.attachments, output: r.output, at },
        });
        return { type: 'task_result_sent', task: done, text, sent: true };
      }
      case 'task_open_admin_shell': {
        if (!this.local)
          return this.error(
            'forbidden',
            'an administrator shell opens only on the PC itself; run it there',
          );
        const task = this.find(msg.task_id);
        if (!task?.command) return this.error('invalid_request', 'this task has no command');
        // The mock never opens anything.
        return { type: 'ok' };
      }
      case 'task_export': {
        const list = this.tasks.filter((t) => !msg.session_id || t.session_id === msg.session_id);
        const text =
          msg.format === 'csv'
            ? ['id,status,title', ...list.map((t) => `${t.id},${t.status},"${t.title}"`)].join('\n')
            : list.map((t) => `- [${t.status === 'done' ? 'x' : ' '}] ${t.title}`).join('\n');
        return { type: 'task_exported', format: msg.format, text };
      }
      case 'task_digest': {
        const open = this.tasks.filter((t) => ACTIVE.has(t.status)).length;
        return {
          type: 'briefing',
          text: open === 0 ? 'No open tasks.' : `You have ${open} open tasks.`,
        };
      }
      case 'task_configure':
        if (!this.local)
          return this.error(
            'forbidden',
            'the task settings change only in the app on the PC itself',
          );
        this.settings = { ...msg.settings };
        this.changed();
        return this.list();
    }
  }
}
