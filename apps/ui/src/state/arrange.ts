/**
 * The screen layout being drawn: which open terminal window goes in which
 * cell of each monitor. Pure and local to the tab; the server gets it as a
 * plan and works out the rectangles itself.
 */
import type {
  ArrangeMonitorPlan,
  ArrangeWindow,
  RestoreMonitor,
  SessionId,
} from '../protocol/types';

/** The same bounds as `MAX_ARRANGE_ROWS` and `MAX_ARRANGE_COLS` on the server. */
export const MAX_ROWS = 6;
export const MAX_COLS = 8;

/** Below this many physical pixels the server keeps windows usable: they overlap. */
const MIN_CELL_W = 320;
const MIN_CELL_H = 200;

export const GRID_PRESETS = [
  { count: 1, rows: 1, cols: 1 },
  { count: 2, rows: 1, cols: 2 },
  { count: 4, rows: 2, cols: 2 },
  { count: 6, rows: 2, cols: 3 },
  { count: 8, rows: 2, cols: 4 },
  { count: 10, rows: 2, cols: 5 },
] as const;

const DEFAULT_GRID = { rows: 2, cols: 2 };

export type GroupBy = 'none' | 'folder' | 'agent';

export interface MonitorGrid {
  rows: number;
  cols: number;
  /** Cell number (row by row from the top left) to window id. */
  cells: Record<number, string>;
}

/** By monitor id. */
export type ArrangeDraft = Record<string, MonitorGrid>;

export function gridOf(draft: ArrangeDraft, monitor: string): MonitorGrid {
  return draft[monitor] ?? { ...DEFAULT_GRID, cells: {} };
}

function windowsIn(grid: MonitorGrid): string[] {
  return Object.keys(grid.cells)
    .map(Number)
    .sort((a, b) => a - b)
    .map((cell) => grid.cells[cell]!);
}

/** Changes the grid; windows keep their order, those that no longer fit leave. */
export function setGrid(
  draft: ArrangeDraft,
  monitor: string,
  rows: number,
  cols: number,
): ArrangeDraft {
  const cells: Record<number, string> = {};
  windowsIn(gridOf(draft, monitor))
    .slice(0, rows * cols)
    .forEach((id, cell) => {
      cells[cell] = id;
    });
  return { ...draft, [monitor]: { rows, cols, cells } };
}

/** Where a window is now. */
export function locate(
  draft: ArrangeDraft,
  window: string,
): { monitor: string; cell: number } | null {
  for (const [monitor, grid] of Object.entries(draft)) {
    for (const [cell, id] of Object.entries(grid.cells)) {
      if (id === window) return { monitor, cell: Number(cell) };
    }
  }
  return null;
}

/** Takes a window out of its cell. */
export function unplace(draft: ArrangeDraft, window: string): ArrangeDraft {
  const at = locate(draft, window);
  if (!at) return draft;
  const grid = gridOf(draft, at.monitor);
  const cells = { ...grid.cells };
  delete cells[at.cell];
  return { ...draft, [at.monitor]: { ...grid, cells } };
}

/**
 * Puts a window in a cell. A window already there swaps places with it when
 * it came from a cell, and otherwise goes back to the list.
 */
export function place(
  draft: ArrangeDraft,
  monitor: string,
  cell: number,
  window: string,
): ArrangeDraft {
  const from = locate(draft, window);
  const next = unplace(draft, window);
  const grid = gridOf(next, monitor);
  const occupant = grid.cells[cell];
  const placed = {
    ...next,
    [monitor]: { ...grid, cells: { ...grid.cells, [cell]: window } },
  };
  if (occupant === undefined || !from) return placed;
  const home = gridOf(placed, from.monitor);
  return {
    ...placed,
    [from.monitor]: { ...home, cells: { ...home.cells, [from.cell]: occupant } },
  };
}

export function clearMonitor(draft: ArrangeDraft, monitor: string): ArrangeDraft {
  return { ...draft, [monitor]: { ...gridOf(draft, monitor), cells: {} } };
}

/** Drops windows that closed. The same object when none did. */
export function prune(draft: ArrangeDraft, open: ReadonlySet<string>): ArrangeDraft {
  let next = draft;
  for (const [monitor, grid] of Object.entries(draft)) {
    const kept = Object.entries(grid.cells).filter(([, id]) => open.has(id));
    if (kept.length !== Object.keys(grid.cells).length) {
      next = { ...next, [monitor]: { ...grid, cells: Object.fromEntries(kept) } };
    }
  }
  return next;
}

/**
 * Whether the tab offers a window: it hosts at least one AI agent session,
 * whoever started it. Plain terminals (no session) and windows of plain
 * shells only are left out. The server's `arrangeable` applies the same rule;
 * this one covers servers from before it.
 */
export function arrangeable(window: ArrangeWindow): boolean {
  return window.sessions.some((s) => s.agent.type !== 'shell');
}

/**
 * A saved layout's grids as a draft, replacing the whole draft. `open`: the
 * windows offered now; a cell of any other window stays empty.
 */
export function fromPlans(
  plans: readonly ArrangeMonitorPlan[],
  open: ReadonlySet<string>,
): ArrangeDraft {
  const draft: ArrangeDraft = {};
  for (const plan of plans) {
    const cells: Record<number, string> = {};
    for (const { cell, window } of plan.cells) {
      if (open.has(window) && cell < plan.rows * plan.cols) cells[cell] = window;
    }
    draft[plan.monitor] = { rows: plan.rows, cols: plan.cols, cells };
  }
  return draft;
}

/** How many windows a draft holds. */
export function placedCount(draft: ArrangeDraft): number {
  return Object.values(draft).reduce((sum, grid) => sum + Object.keys(grid.cells).length, 0);
}

/** A window's place in the session order: its first session; no session is last. */
function rankOf(window: ArrangeWindow, order: readonly SessionId[]): number {
  const places = window.sessions.map((s) => order.indexOf(s.id)).filter((index) => index >= 0);
  return places.length > 0 ? Math.min(...places) : Number.MAX_SAFE_INTEGER;
}

function groupOf(window: ArrangeWindow, groupBy: GroupBy, order: readonly SessionId[]): string {
  if (groupBy === 'none') return '';
  const lead = [...window.sessions].sort((a, b) => order.indexOf(a.id) - order.indexOf(b.id))[0];
  if (!lead) return '';
  if (groupBy === 'agent') {
    return lead.agent.type === 'generic' || lead.agent.type === 'shell'
      ? `${lead.agent.type}:${lead.agent.name}`
      : lead.agent.type;
  }
  return lead.folder.replace(/[\\/]+$/, '').toLowerCase();
}

/**
 * Windows in session order, those of one group next to each other, groups by
 * their first window. Windows with no session come last.
 */
export function orderWindows(
  windows: readonly ArrangeWindow[],
  order: readonly SessionId[],
  groupBy: GroupBy,
): ArrangeWindow[] {
  const byRank = [...windows].sort(
    (a, b) => rankOf(a, order) - rankOf(b, order) || a.title.localeCompare(b.title),
  );
  if (groupBy === 'none') return byRank;
  const groups = new Map<string, ArrangeWindow[]>();
  for (const window of byRank) {
    const key = groupOf(window, groupBy, order);
    groups.set(key, [...(groups.get(key) ?? []), window]);
  }
  // The windows without a session are no group: they stay at the end.
  const loose = groups.get('') ?? [];
  groups.delete('');
  return [...[...groups.values()].flat(), ...loose];
}

/**
 * The cells in filling order. In a grouped fill the rows run back and forth,
 * so one cell always touches the next, also from one row to the next.
 */
export function fillOrder(rows: number, cols: number, snake: boolean): number[] {
  const out: number[] = [];
  for (let row = 0; row < rows; row += 1) {
    for (let col = 0; col < cols; col += 1) {
      const across = snake && row % 2 === 1 ? cols - 1 - col : col;
      out.push(row * cols + across);
    }
  }
  return out;
}

/**
 * Fills a monitor from scratch: its windows and those in no cell yet, in
 * session order. Windows on another monitor stay there.
 */
export function autoFill(
  draft: ArrangeDraft,
  monitor: string,
  windows: readonly ArrangeWindow[],
  order: readonly SessionId[],
  groupBy: GroupBy,
): ArrangeDraft {
  const pool = windows.filter((w) => {
    const at = locate(draft, w.id);
    return !at || at.monitor === monitor;
  });
  const grid = gridOf(draft, monitor);
  const cells: Record<number, string> = {};
  const slots = fillOrder(grid.rows, grid.cols, groupBy !== 'none');
  orderWindows(pool, order, groupBy)
    .slice(0, slots.length)
    .forEach((window, index) => {
      cells[slots[index]!] = window.id;
    });
  return { ...draft, [monitor]: { ...grid, cells } };
}

/** The draft as the server takes it: monitors with at least one window. */
export function toPlans(draft: ArrangeDraft): ArrangeMonitorPlan[] {
  return Object.entries(draft)
    .filter(([, grid]) => Object.keys(grid.cells).length > 0)
    .map(([monitor, grid]) => ({
      monitor,
      rows: grid.rows,
      cols: grid.cols,
      cells: Object.entries(grid.cells).map(([cell, window]) => ({ cell: Number(cell), window })),
    }));
}

/** `\\.\DISPLAY1` as a person reads it. */
export function monitorName(id: string): string {
  return id.replace(/^\\\\\.\\/, '');
}

/** The monitors' rectangles as percentages of the desktop around them. */
export function monitorMap(monitors: readonly RestoreMonitor[]) {
  const left = Math.min(...monitors.map((m) => m.rect.x));
  const top = Math.min(...monitors.map((m) => m.rect.y));
  const right = Math.max(...monitors.map((m) => m.rect.x + m.rect.w));
  const bottom = Math.max(...monitors.map((m) => m.rect.y + m.rect.h));
  const width = right - left;
  const height = bottom - top;
  return {
    aspect: width / height,
    boxes: monitors.map((m) => ({
      monitor: m,
      left: ((m.rect.x - left) / width) * 100,
      top: ((m.rect.y - top) / height) * 100,
      width: (m.rect.w / width) * 100,
      height: (m.rect.h / height) * 100,
    })),
  };
}

/** The cells of this grid are smaller than a usable window. */
export function cellsTooSmall(monitor: RestoreMonitor, rows: number, cols: number): boolean {
  return monitor.work.w / cols < MIN_CELL_W || monitor.work.h / rows < MIN_CELL_H;
}
