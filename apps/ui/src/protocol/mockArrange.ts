/**
 * Screen layouts on the simulated server: the same answers as vcad's arrange
 * module, with invented windows. Nothing here touches a real window.
 */
import { arrangeable } from '../state/arrange';
import type { MockProfile } from './mockData';
import { MOCK_MONITORS } from './mockRestore';
import type {
  ArrangeEdit,
  ArrangeItem,
  ArrangeLayout,
  ArrangeMonitorPlan,
  ArrangeOverview,
  ArrangeResult,
  ArrangeSavedCell,
  ArrangeWindow,
  ErrorCode,
  ServerMsg,
  UnixMillis,
} from './types';

function sampleWindows(): ArrangeWindow[] {
  const window = (
    id: string,
    title: string,
    monitor: string,
    sessions: ArrangeWindow['sessions'],
  ): ArrangeWindow => ({ id, title, monitor, maximized: false, minimized: false, sessions });
  return [
    window('101', 'payments-api', '\\\\.\\DISPLAY1', [
      {
        id: 'wk-claude-api',
        title: 'payments-api',
        agent: { type: 'claude_code' },
        folder: 'E:\\code\\payments-api',
      },
      {
        id: 'wk-shell-api',
        title: 'api shell',
        agent: { type: 'claude_code' },
        folder: 'E:\\code\\payments-api',
      },
    ]),
    window('102', 'web-dashboard', '\\\\.\\DISPLAY2', [
      {
        id: 'wk-codex-web',
        title: 'web-dashboard',
        agent: { type: 'codex' },
        folder: 'E:\\code\\web-dashboard',
      },
    ]),
    window('103', 'docs site', '\\\\.\\DISPLAY3', [
      {
        id: 'wk-gemini-docs',
        title: 'docs site',
        agent: { type: 'gemini_cli' },
        folder: 'E:\\code\\docs',
      },
    ]),
    // Neither is offered: a plain terminal, and a plain shell opened from the app.
    window('104', 'Windows PowerShell', '\\\\.\\DISPLAY1', []),
    window('105', 'remote pwsh', '\\\\.\\DISPLAY2', [
      {
        id: 'wk-remote-shell',
        title: 'remote pwsh',
        agent: { type: 'shell', name: 'pwsh' },
        folder: 'E:\\code',
      },
    ]),
  ];
}

const UNSUPPORTED =
  'session restore is available on Windows only for now (this computer runs linux)';

export class MockArrange {
  private windows: ArrangeWindow[];
  private layouts: ArrangeLayout[] = [];
  private undoMonitors: { id: string; monitor: string | null }[] = [];
  private counter = 0;

  constructor(
    private readonly profile: MockProfile,
    private readonly now: () => UnixMillis,
  ) {
    this.windows = this.supported ? sampleWindows() : [];
  }

  private get supported(): boolean {
    return this.profile === 'workstation';
  }

  /** The window of a saved cell: by its sessions, then by its title. */
  private findWindow(cell: ArrangeSavedCell): ArrangeWindow | undefined {
    const ids = new Set(cell.sessions.map((s) => s.session_id));
    return (
      this.windows.find((w) => w.sessions.some((s) => ids.has(s.id))) ??
      this.windows.find((w) => w.title === cell.title)
    );
  }

  private overview(): ArrangeOverview {
    // As the server: plain terminals and plain shells are not offered.
    const windows = this.windows.filter(arrangeable);
    const offered = new Set(windows.map((w) => w.id));
    const layoutPlans: Record<string, ArrangeMonitorPlan[]> = {};
    for (const layout of this.layouts) {
      layoutPlans[layout.id] = layout.monitors
        .filter((saved) => MOCK_MONITORS.some((m) => m.id === saved.monitor))
        .map((saved) => ({
          monitor: saved.monitor,
          rows: saved.rows,
          cols: saved.cols,
          cells: saved.cells.flatMap((cell) => {
            const window = this.findWindow(cell);
            return window && offered.has(window.id) ? [{ cell: cell.cell, window: window.id }] : [];
          }),
        }));
    }
    return {
      supported: this.supported,
      message: this.supported ? null : UNSUPPORTED,
      monitors: this.supported ? MOCK_MONITORS : [],
      windows,
      layouts: this.layouts,
      undo_windows: this.undoMonitors.length,
      layout_plans: layoutPlans,
    };
  }

  list(): ServerMsg {
    return { type: 'arrange_list', overview: this.overview() };
  }

  private error(code: ErrorCode, message: string): ServerMsg {
    return { type: 'error', code, message };
  }

  private done(result: Omit<ArrangeResult, 'moved' | 'skipped' | 'failed'>): ServerMsg {
    const count = (moved: boolean) =>
      result.items.filter((i) => (i.status === 'moved') === moved).length;
    return {
      type: 'arrange_done',
      result: { ...result, moved: count(true), skipped: count(false), failed: 0 },
      overview: this.overview(),
    };
  }

  private move(targets: { title: string; window: string | null; monitor: string }[]): ServerMsg {
    const items: ArrangeItem[] = [];
    const undo: typeof this.undoMonitors = [];
    for (const target of targets) {
      const window = this.windows.find((w) => w.id === target.window);
      if (!window) {
        items.push({ title: target.title, status: 'skipped_not_open', message: null });
      } else if (!MOCK_MONITORS.some((m) => m.id === target.monitor)) {
        items.push({ title: window.title, status: 'skipped_monitor', message: null });
      } else {
        undo.push({ id: window.id, monitor: window.monitor });
        window.monitor = target.monitor;
        items.push({ title: window.title, status: 'moved', message: null });
      }
    }
    if (undo.length > 0) this.undoMonitors = undo;
    return this.done({ items });
  }

  apply(monitors: ArrangeMonitorPlan[]): ServerMsg {
    if (!this.supported) return this.error('unsupported', UNSUPPORTED);
    return this.move(
      monitors.flatMap((plan) =>
        plan.cells.map((cell) => ({ title: '', window: cell.window, monitor: plan.monitor })),
      ),
    );
  }

  applyLayout(id: string): ServerMsg {
    if (!this.supported) return this.error('unsupported', UNSUPPORTED);
    const layout = this.layouts.find((l) => l.id === id);
    if (!layout) return this.error('not_found', `no layout ${id}`);
    return this.move(
      layout.monitors.flatMap((saved) =>
        saved.cells.map((cell) => ({
          title: cell.title,
          window: this.findWindow(cell)?.id ?? null,
          monitor: saved.monitor,
        })),
      ),
    );
  }

  undo(): ServerMsg {
    if (!this.supported) return this.error('unsupported', UNSUPPORTED);
    if (this.undoMonitors.length === 0) return this.error('conflict', 'there is nothing to undo');
    const items: ArrangeItem[] = this.undoMonitors.map(({ id, monitor }) => {
      const window = this.windows.find((w) => w.id === id);
      if (window) window.monitor = monitor;
      return { title: window?.title ?? '', status: 'moved', message: null };
    });
    this.undoMonitors = [];
    return this.done({ items });
  }

  /** Closes a window, for tests of a saved layout with a window missing. */
  closeWindow(id: string): void {
    this.windows = this.windows.filter((w) => w.id !== id);
  }

  edit(edit: ArrangeEdit): ServerMsg {
    switch (edit.op) {
      case 'layout_save': {
        const name = edit.name.trim();
        if (this.layouts.some((l) => l.name.toLowerCase() === name.toLowerCase())) {
          return this.error('conflict', `another layout is already named ${name}`);
        }
        this.counter += 1;
        this.layouts = [
          ...this.layouts,
          {
            id: `layout-${this.counter}`,
            name,
            saved_at: this.now(),
            monitors: edit.monitors.map((plan) => ({
              monitor: plan.monitor,
              rows: plan.rows,
              cols: plan.cols,
              cells: plan.cells.map((cell) => {
                const window = this.windows.find((w) => w.id === cell.window);
                return {
                  cell: cell.cell,
                  title: window?.title ?? '',
                  sessions: (window?.sessions ?? []).map((s) => ({
                    agent: s.agent,
                    folder: s.folder,
                    session_id: s.id,
                    title: s.title,
                  })),
                };
              }),
            })),
          },
        ];
        return this.list();
      }
      case 'layout_rename':
        this.layouts = this.layouts.map((l) =>
          l.id === edit.layout ? { ...l, name: edit.name.trim() } : l,
        );
        return this.list();
      case 'layout_delete':
        this.layouts = this.layouts.filter((l) => l.id !== edit.layout);
        return this.list();
    }
  }
}
