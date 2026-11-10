/**
 * Session restore on the simulated server: the same list, edits and run
 * states as vcad's restore module, with invented windows. A run advances one
 * step per simulation tick. Nothing here touches a real window.
 */
import type { MockProfile } from './mockData';
import type {
  ClientMsg,
  ErrorCode,
  RestoreEdit,
  RestoreEntry,
  RestoreEntryDraft,
  RestoreItem,
  RestoreItemStatus,
  RestoreJob,
  RestoreMonitor,
  RestoreOverview,
  RestoreProfile,
  RestoreRunRequest,
  ServerMsg,
  UnixMillis,
  WindowRect,
} from './types';

const SNAPSHOT_SECS = 300;

function monitor(id: string, x: number, y: number): RestoreMonitor {
  const rect = { x, y, w: 1920, h: 1080 };
  return { id: `\\\\.\\${id}`, rect, work: { ...rect, h: 1032 }, dpi: 144 };
}

export const MOCK_MONITORS = [
  monitor('DISPLAY1', 0, 0),
  monitor('DISPLAY2', 1920, 0),
  monitor('DISPLAY3', -1920, 0),
  monitor('DISPLAY4', 1920, -1080),
];

function resumeCommand(entry: RestoreEntry): string {
  if (entry.agent.type === 'codex') {
    return entry.session_id ? `codex resume ${entry.session_id}` : 'codex resume --last';
  }
  return entry.session_id ? `claude --resume ${entry.session_id}` : 'claude --continue';
}

function sampleEntries(now: UnixMillis): RestoreEntry[] {
  const entry = (
    order: number,
    title: string,
    folder: string,
    session: string | null,
    rect: WindowRect,
    display: RestoreMonitor,
    extra: Partial<RestoreEntry> = {},
  ): RestoreEntry => ({
    id: `entry-${order}`,
    title,
    agent: { type: 'claude_code' },
    folder,
    session_id: session,
    rect,
    monitor: display,
    maximized: false,
    enabled: true,
    order,
    args: [],
    transcript: null,
    note: null,
    source: 'auto',
    updated_at: now,
    ...extra,
  });
  const [m1, m2, m3, m4] = MOCK_MONITORS as [
    RestoreMonitor,
    RestoreMonitor,
    RestoreMonitor,
    RestoreMonitor,
  ];
  return [
    entry(0, 'payments-api', 'E:\\code\\payments-api', 'a1f3c2d4', rect(-9, 0), m1),
    entry(1, 'web-dashboard', 'E:\\code\\web-dashboard', 'b7e91c02', rect(1911, 0), m2, {
      agent: { type: 'codex' },
    }),
    entry(2, 'infra (release)', 'E:\\code\\infra', 'c44d10ee', rect(-1929, 0), m3),
    entry(3, 'docs site', 'E:\\code\\docs', null, rect(1911, -1080), m4, {
      source: 'imported',
    }),
    entry(4, 'old prototype', 'E:\\code\\archive\\prototype', 'd9021fab', rect(-1929, 540), m3, {
      enabled: false,
      note: 'a permission-bypass flag of the running session was dropped: the restored session asks as usual',
    }),
  ];
}

function rect(x: number, y: number): WindowRect {
  return { x, y, w: 1938, h: 1053 };
}

/** Folders of the sample that "no longer exist". */
const MISSING = 'E:\\code\\archive\\prototype';

export class MockRestore {
  private profiles: RestoreProfile[];
  private active = 'default';
  private counter = 100;
  private job: RestoreJob | null = null;
  private lastSnapshot: UnixMillis;
  /** Entries whose window is open. */
  private readonly open = new Set<string>(['entry-0', 'entry-1']);

  constructor(
    private readonly profile: MockProfile,
    private readonly now: () => UnixMillis,
    private readonly push: (msg: ServerMsg) => void,
  ) {
    const at = now();
    this.lastSnapshot = at - 120_000;
    this.profiles = [
      {
        id: 'default',
        name: 'Work',
        delay_secs: 6,
        auto_prompt: null,
        entries: profile === 'workstation' ? sampleEntries(at) : [],
      },
    ];
  }

  private get supported(): boolean {
    return this.profile === 'workstation';
  }

  private overview(): RestoreOverview {
    return {
      supported: this.supported,
      platform: this.supported ? 'windows' : 'linux',
      message: this.supported
        ? null
        : 'session restore is available on Windows only for now (this computer runs linux)',
      snapshot_interval_secs: SNAPSHOT_SECS,
      last_snapshot_at: this.supported ? this.lastSnapshot : null,
      active_profile: this.active,
      profiles: this.profiles,
      live: this.profiles.flatMap((p) =>
        p.entries.map((e) => ({ entry_id: e.id, open: this.open.has(e.id), session_id: null })),
      ),
      monitors: this.supported ? MOCK_MONITORS : [],
      job: this.job,
    };
  }

  list(): ServerMsg {
    return { type: 'restore_list', overview: this.overview() };
  }

  private error(code: ErrorCode, message: string): ServerMsg {
    return { type: 'error', code, message };
  }

  private find(key: string | null | undefined): RestoreProfile | undefined {
    const wanted = key ?? this.active;
    return this.profiles.find(
      (p) => p.id === wanted || p.name.toLowerCase() === wanted.toLowerCase(),
    );
  }

  handle(msg: Extract<ClientMsg, { type: 'restore_capture' | 'restore_edit' | 'restore_import' }>) {
    switch (msg.type) {
      case 'restore_capture':
        this.lastSnapshot = this.now();
        return this.list();
      case 'restore_edit':
        return this.edit(msg.edit);
      case 'restore_import':
        return this.importLayout(msg.layout, msg.profile);
    }
  }

  private importLayout(text: string, profile: string | null): ServerMsg {
    const target = this.find(profile);
    if (!target) return this.error('not_found', `no profile ${profile}`);
    let layout: {
      windows?: { title: string; dir: string; x: number; y: number; w: number; h: number }[];
    };
    try {
      layout = JSON.parse(text.replace(/^\uFEFF/, '')) as typeof layout;
    } catch {
      return this.error('invalid_request', 'not a layout file');
    }
    if (!layout.windows?.length)
      return this.error('invalid_request', 'the layout file has no windows');
    for (const w of layout.windows) {
      if (target.entries.some((e) => e.title === w.title)) continue;
      this.counter += 1;
      target.entries = [
        ...target.entries,
        {
          id: `entry-${this.counter}`,
          title: w.title,
          agent: { type: 'claude_code' },
          folder: w.dir,
          session_id: null,
          rect: { x: w.x, y: w.y, w: w.w, h: w.h },
          monitor: null,
          maximized: false,
          enabled: true,
          order: target.entries.length,
          args: [],
          transcript: null,
          note: null,
          source: 'imported',
          updated_at: this.now(),
        },
      ];
    }
    return this.list();
  }

  private fromDraft(id: string, order: number, draft: RestoreEntryDraft): RestoreEntry {
    return {
      ...draft,
      id,
      order,
      monitor: null,
      args: [],
      transcript: null,
      note: null,
      source: 'manual',
      updated_at: this.now(),
    };
  }

  private edit(edit: RestoreEdit): ServerMsg {
    if (edit.op === 'profile_add') {
      this.counter += 1;
      this.profiles = [
        ...this.profiles,
        {
          id: `profile-${this.counter}`,
          name: edit.name.trim(),
          delay_secs: 6,
          auto_prompt: null,
          entries: [],
        },
      ];
      return this.list();
    }
    const target = this.find(edit.profile);
    if (!target) return this.error('not_found', `no profile ${edit.profile}`);
    switch (edit.op) {
      case 'profile_rename':
        target.name = edit.name.trim();
        break;
      case 'profile_delete':
        if (this.profiles.length === 1)
          return this.error('conflict', 'the last profile cannot be deleted');
        this.profiles = this.profiles.filter((p) => p !== target);
        if (this.active === target.id) this.active = this.profiles[0]!.id;
        break;
      case 'profile_activate':
        this.active = target.id;
        break;
      case 'profile_settings':
        target.delay_secs = edit.delay_secs;
        target.auto_prompt = edit.auto_prompt;
        break;
      case 'entry_add': {
        if (edit.entry.agent.type === 'generic' || edit.entry.agent.type === 'shell')
          return this.error('invalid_request', 'only known agents can be restored');
        if (target.entries.some((e) => e.title === edit.entry.title))
          return this.error('conflict', `another window is already named ${edit.entry.title}`);
        this.counter += 1;
        target.entries = [
          ...target.entries,
          this.fromDraft(`entry-${this.counter}`, target.entries.length, edit.entry),
        ];
        break;
      }
      case 'entry_update':
        target.entries = target.entries.map((e) =>
          e.id === edit.entry_id ? { ...e, ...edit.entry, updated_at: this.now() } : e,
        );
        break;
      case 'entry_delete':
        // Only the list changes; an open window stays open.
        target.entries = target.entries.filter((e) => e.id !== edit.entry_id);
        break;
      case 'entry_set_enabled':
        target.entries = target.entries.map((e) =>
          e.id === edit.entry_id ? { ...e, enabled: edit.enabled } : e,
        );
        break;
      case 'entry_reorder':
        target.entries = edit.entry_ids
          .map((id, order) => {
            const e = target.entries.find((x) => x.id === id);
            return e ? { ...e, order } : null;
          })
          .filter((e): e is RestoreEntry => e !== null);
        break;
    }
    return this.list();
  }

  private planItem(entry: RestoreEntry, delay: number): RestoreItem {
    let status: RestoreItemStatus = 'waiting';
    if (this.open.has(entry.id)) status = 'skipped_open';
    else if (entry.folder === MISSING) status = 'skipped_missing';
    return {
      entry_id: entry.id,
      title: entry.title,
      folder: entry.folder,
      status,
      resume: entry.session_id ? 'exact' : 'latest',
      command: resumeCommand(entry),
      rect: entry.rect,
      delay_after_secs: status === 'waiting' ? delay : 0,
      message: null,
    };
  }

  run(request: RestoreRunRequest): ServerMsg {
    if (!this.supported) {
      return this.error('unsupported', 'session restore is available on Windows only for now');
    }
    const target = this.find(request.profile);
    if (!target) return this.error('not_found', `no profile ${request.profile}`);
    if (!request.dry_run && this.job && this.job.finished_at === null) {
      return this.error('conflict', 'a restore is already running');
    }
    const only = (request.only ?? []).map((o) => o.toLowerCase());
    const ids = request.entry_ids ?? [];
    const picked = [...target.entries]
      .sort((a, b) => a.order - b.order)
      .filter((e) => {
        if (ids.length > 0) return ids.includes(e.id);
        if (!e.enabled) return false;
        return only.length === 0 || only.some((o) => e.title.toLowerCase().includes(o));
      });
    const now = this.now();
    const job: RestoreJob = {
      id: `job-${now}`,
      profile: target.name,
      by: 'this device',
      dry_run: !!request.dry_run,
      started_at: now,
      finished_at: request.dry_run ? now : null,
      items: picked.map((e) => this.planItem(e, target.delay_secs)),
    };
    if (!request.dry_run) {
      this.job = job;
      setTimeout(() => this.publish(), 0);
    }
    return { type: 'restore_progress', job };
  }

  private publish(): void {
    if (this.job) this.push({ type: 'restore_progress', job: this.job });
  }

  /** One step of the running job. */
  tick(): void {
    const job = this.job;
    if (!job || job.finished_at !== null) return;
    const items = [...job.items];
    const opening = items.findIndex((i) => i.status === 'opening');
    if (opening >= 0) {
      this.open.add(items[opening]!.entry_id);
      // As on vcad: a window with no saved position opens where it opens.
      items[opening] = items[opening]!.rect
        ? { ...items[opening]!, status: 'placed' }
        : {
            ...items[opening]!,
            status: 'opened',
            message: 'opened, but not placed: no position is saved for it',
          };
    }
    const next = items.findIndex((i) => i.status === 'waiting');
    if (next >= 0) items[next] = { ...items[next]!, status: 'opening' };
    const done = !items.some((i) => i.status === 'waiting' || i.status === 'opening');
    this.job = { ...job, items, finished_at: done ? this.now() : null };
    this.publish();
  }
}

/** Stands in for the desktop app's shortcut and sign-in commands. */
export function mockRestoreLocal() {
  let signIn = false;
  let delay = 60;
  const status = () => ({ supported: true, sign_in: signIn, sign_in_delay_secs: delay });
  return {
    status: async () => status(),
    createShortcut: async () => status(),
    setSignIn: async (enabled: boolean, delaySecs: number) => {
      signIn = enabled;
      delay = delaySecs;
      return status();
    },
  };
}
