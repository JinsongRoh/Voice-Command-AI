import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type DragEvent,
  type FormEvent,
} from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { agentLabel } from '../protocol/helpers';
import type {
  ArrangeItem,
  ArrangeItemStatus,
  ArrangeLayout,
  ArrangeMonitorPlan,
  ArrangeResult,
  ArrangeWindow,
  RestoreMonitor,
} from '../protocol/types';
import {
  GRID_PRESETS,
  MAX_COLS,
  MAX_ROWS,
  arrangeable,
  autoFill,
  cellsTooSmall,
  clearMonitor,
  fromPlans,
  gridOf,
  locate,
  monitorMap,
  monitorName,
  place,
  placedCount,
  prune,
  setGrid,
  toPlans,
  unplace,
  type ArrangeDraft,
  type GroupBy,
} from '../state/arrange';
import { RequestError } from '../state/core';
import { useActions, useAppState } from '../state/store';
import styles from './ArrangePanel.module.css';
import { Icon } from './Icon';
import { ConfirmDialog } from './Modal';
import { NameDialog } from './NameDialog';
import { useDeviceScope, useShell } from './ShellProvider';
import shared from './WindowsPanel.module.css';

/**
 * How often the window list is refreshed. Slower than the restore tab: it
 * reads the process table to find each window's sessions.
 */
const REFRESH_MS = 15000;

const REASON: Record<Exclude<ArrangeItemStatus, 'moved'>, MessageKey> = {
  skipped_not_open: 'arrange.reason.notOpen',
  skipped_monitor: 'arrange.reason.monitor',
  skipped_duplicate: 'arrange.reason.duplicate',
  failed: 'arrange.reason.failed',
};

const GROUPS: { id: GroupBy; label: MessageKey }[] = [
  { id: 'none', label: 'arrange.group.none' },
  { id: 'folder', label: 'arrange.group.folder' },
  { id: 'agent', label: 'arrange.group.agent' },
];

function windowLabel(window: ArrangeWindow): string {
  return window.title.trim() || window.sessions[0]?.title || '—';
}

function WindowFacts({ window }: { window: ArrangeWindow }) {
  const { t } = useI18n();
  const lead = window.sessions[0];
  return (
    <span className={shared.chips}>
      {lead && <span className={shared.chip}>{agentLabel(lead.agent)}</span>}
      {window.sessions.length > 1 && (
        <span className={styles.tabs}>
          {t('arrange.tabsChip', { count: window.sessions.length })}
        </span>
      )}
      {window.maximized && <span className={shared.chip}>{t('windows.maximized')}</span>}
      {window.minimized && <span className={shared.chip}>{t('arrange.minimized')}</span>}
    </span>
  );
}

function MonitorMap({
  monitors,
  draft,
  selected,
  onSelect,
}: {
  monitors: RestoreMonitor[];
  draft: ArrangeDraft;
  selected: string | null;
  /** Absent for a device that only views. */
  onSelect?: (id: string) => void;
}) {
  const { t } = useI18n();
  const map = monitorMap(monitors);
  return (
    <div className={styles.map} style={{ '--aspect': map.aspect } as CSSProperties}>
      {map.boxes.map(({ monitor, left, top, width, height }) => {
        const grid = gridOf(draft, monitor.id);
        const filled = Object.keys(grid.cells).length;
        const name = monitorName(monitor.id);
        const label = t('arrange.monitorLabel', {
          name,
          width: monitor.rect.w,
          height: monitor.rect.h,
          filled,
          cells: grid.rows * grid.cols,
        });
        const style = {
          left: `${left}%`,
          top: `${top}%`,
          width: `${width}%`,
          height: `${height}%`,
        };
        return onSelect ? (
          <button
            key={monitor.id}
            type="button"
            className={styles.monitor}
            style={style}
            aria-pressed={selected === monitor.id}
            aria-label={label}
            title={label}
            onClick={() => onSelect(monitor.id)}
          >
            <span className={styles.monitorName}>{name}</span>
            <span className={styles.monitorFill}>
              {grid.rows}×{grid.cols}
            </span>
          </button>
        ) : (
          <div key={monitor.id} className={styles.monitor} style={style} title={label}>
            <span className={styles.monitorName}>{name}</span>
          </div>
        );
      })}
    </div>
  );
}

function CustomGrid({
  rows,
  cols,
  onSet,
}: {
  rows: number;
  cols: number;
  onSet: (rows: number, cols: number) => void;
}) {
  const { t } = useI18n();
  const [typedRows, setTypedRows] = useState(String(rows));
  const [typedCols, setTypedCols] = useState(String(cols));
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const clamp = (text: string, max: number, was: number) => {
      const value = Math.round(Number(text));
      return Number.isFinite(value) && value >= 1 ? Math.min(max, value) : was;
    };
    const nextRows = clamp(typedRows, MAX_ROWS, rows);
    const nextCols = clamp(typedCols, MAX_COLS, cols);
    setTypedRows(String(nextRows));
    setTypedCols(String(nextCols));
    onSet(nextRows, nextCols);
  };
  return (
    <form className={styles.custom} noValidate onSubmit={submit}>
      <label className={styles.customField}>
        <span>{t('arrange.rows')}</span>
        <input
          type="number"
          min={1}
          max={MAX_ROWS}
          value={typedRows}
          onChange={(e) => setTypedRows(e.target.value)}
        />
      </label>
      <span aria-hidden="true">×</span>
      <label className={styles.customField}>
        <span>{t('arrange.cols')}</span>
        <input
          type="number"
          min={1}
          max={MAX_COLS}
          value={typedCols}
          onChange={(e) => setTypedCols(e.target.value)}
        />
      </label>
      <button type="submit" className="btn btn-sm">
        {t('arrange.customSet')}
      </button>
    </form>
  );
}

function ReportRow({ item }: { item: ArrangeItem }) {
  const { t } = useI18n();
  if (item.status === 'moved') return null;
  return (
    <li className={shared.item} data-status={item.status === 'failed' ? 'failed' : 'skipped'}>
      <span className={shared.itemMain}>
        <span className={shared.itemTitle}>{item.title || t('arrange.unknownWindow')}</span>
        {item.message && <span className={shared.note}>{item.message}</span>}
      </span>
      <span className={shared.status}>{t(REASON[item.status])}</span>
    </li>
  );
}

function Report({ result }: { result: ArrangeResult }) {
  const { t } = useI18n();
  return (
    <section className={`panel ${shared.block}`} aria-live="polite">
      <p className={styles.summary}>
        {t(result.failed > 0 ? 'arrange.resultFailed' : 'arrange.result', {
          moved: result.moved,
          skipped: result.skipped,
          failed: result.failed,
        })}
      </p>
      {result.items.some((i) => i.status !== 'moved') && (
        <ul className={shared.items}>
          {result.items.map((item, index) => (
            <ReportRow key={index} item={item} />
          ))}
        </ul>
      )}
    </section>
  );
}

function SavedLayouts({
  layouts,
  canManage,
  busy,
  loaded,
  onLoad,
  onApply,
  onRename,
  onDelete,
}: {
  layouts: ArrangeLayout[];
  canManage: boolean;
  busy: boolean;
  /** The layout last loaded into the editor. */
  loaded: string | null;
  /** Absent when the server sends no grids to load. */
  onLoad?: (layout: ArrangeLayout) => void;
  onApply: (layout: ArrangeLayout) => void;
  onRename: (layout: ArrangeLayout) => void;
  onDelete: (layout: ArrangeLayout) => void;
}) {
  const { t } = useI18n();
  if (layouts.length === 0) return <p className="muted">{t('arrange.noLayouts')}</p>;
  const load = canManage ? onLoad : undefined;
  return (
    <ul className={shared.items}>
      {layouts.map((layout) => (
        <li key={layout.id} className={shared.item}>
          <span className={shared.itemMain}>
            {load ? (
              <button
                type="button"
                className={`${shared.itemTitle} ${styles.layoutName}`}
                aria-pressed={loaded === layout.id}
                title={t('arrange.loadLayout', { name: layout.name })}
                onClick={() => load(layout)}
              >
                {layout.name}
              </button>
            ) : (
              <span className={shared.itemTitle}>{layout.name}</span>
            )}
            <span className={shared.note}>
              {t('arrange.layoutInfo', {
                count: layout.monitors.reduce((sum, m) => sum + m.cells.length, 0),
              })}
            </span>
          </span>
          {canManage && (
            <span className={shared.row}>
              {load && (
                <button
                  type="button"
                  className="btn btn-sm"
                  aria-label={t('arrange.loadLayout', { name: layout.name })}
                  onClick={() => load(layout)}
                >
                  {t('arrange.load')}
                </button>
              )}
              <button
                type="button"
                className="btn btn-sm"
                disabled={busy}
                aria-label={t('arrange.applyLayout', { name: layout.name })}
                onClick={() => onApply(layout)}
              >
                {t('arrange.applyShort')}
              </button>
              <button
                type="button"
                className="icon-btn"
                aria-label={t('arrange.renameLayout', { name: layout.name })}
                title={t('arrange.renameLayout', { name: layout.name })}
                onClick={() => onRename(layout)}
              >
                <Icon name="edit" size={18} />
              </button>
              <button
                type="button"
                className="icon-btn"
                aria-label={t('arrange.deleteLayout', { name: layout.name })}
                title={t('arrange.deleteLayout', { name: layout.name })}
                onClick={() => onDelete(layout)}
              >
                <Icon name="close" size={18} />
              </button>
            </span>
          )}
        </li>
      ))}
    </ul>
  );
}

/**
 * "Screen layout" in the Windows menu: arranges open terminal windows in a
 * grid, one monitor at a time. A window goes in a cell by dragging it, or by
 * choosing it and then the cell, which works by keyboard and touch. The
 * layout stays on this page until "Arrange windows" sends it to the server,
 * which checks every window and reports what it did.
 */
export function ArrangePanel() {
  const { t } = useI18n();
  const actions = useActions();
  const scope = useDeviceScope();
  const shell = useShell();
  const overview = useAppState((s) => s.arrange);
  const order = useAppState((s) => s.order);
  const canManage = scope === null || scope === 'admin';
  const [rawDraft, setDraft] = useState<ArrangeDraft>({});
  const [monitorId, setMonitorId] = useState<string | null>(null);
  const [picked, setPicked] = useState<string | null>(null);
  const [groupBy, setGroupBy] = useState<GroupBy>('none');
  const [result, setResult] = useState<ArrangeResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [naming, setNaming] = useState<ArrangeLayout | 'new' | null>(null);
  const [removing, setRemoving] = useState<ArrangeLayout | null>(null);
  /** The saved layout last loaded into the draft, and what the load found. */
  const [loaded, setLoaded] = useState<string | null>(null);
  const [loadNote, setLoadNote] = useState<string | null>(null);
  /**
   * The person changed the draft (or loaded a layout): from then on nothing
   * replaces it by itself.
   */
  const touched = useRef(false);
  /** The window being dragged: only this app's own drags are dropped. */
  const dragged = useRef<string | null>(null);

  useEffect(() => {
    const load = () => void actions.refreshArrange().catch(() => {});
    load();
    const timer = setInterval(load, REFRESH_MS);
    return () => clearInterval(timer);
  }, [actions]);

  const windows = useMemo(() => (overview?.windows ?? []).filter(arrangeable), [overview]);
  const draft = useMemo(
    () => prune(rawDraft, new Set(windows.map((w) => w.id))),
    [rawDraft, windows],
  );

  /**
   * Replaces the draft with a saved layout's grids of the windows open now.
   * Moves nothing. Returns the monitor to show first.
   */
  const loadLayout = (layout: ArrangeLayout, plans: ArrangeMonitorPlan[]): string | null => {
    const next = fromPlans(plans, new Set(windows.map((w) => w.id)));
    const placed = placedCount(next);
    const total = layout.monitors.reduce((sum, m) => sum + m.cells.length, 0);
    setDraft(next);
    setLoaded(layout.id);
    setLoadNote(
      placed < total
        ? t('arrange.loadedPartial', { name: layout.name, placed, total })
        : t('arrange.loaded', { name: layout.name }),
    );
    return plans.find((plan) => plan.cells.length > 0)?.monitor ?? plans[0]?.monitor ?? null;
  };

  // The only saved layout is loaded by itself (into the draft: nothing
  // moves), until the person changes the draft; a refresh before that fills
  // in windows that opened since.
  // Deleting layouts down to one shows that one, even over earlier changes:
  // the tab often stays open while the person saves a new layout and then
  // deletes the old one. Saving the first layout loads nothing over the draft.
  const layoutCount = overview?.layouts.length;
  const lastCount = useRef(layoutCount);
  const only = layoutCount === 1 ? overview?.layouts[0] : undefined;
  const onlyPlans = only ? overview?.layout_plans?.[only.id] : undefined;
  useEffect(() => {
    if (layoutCount === undefined) return;
    if (layoutCount === 1 && (lastCount.current ?? 0) > 1) touched.current = false;
    lastCount.current = layoutCount;
  }, [layoutCount]);
  useEffect(() => {
    if (!canManage || !only || !onlyPlans || touched.current) return;
    const first = loadLayout(only, onlyPlans);
    setMonitorId((current) => current ?? first);
    // `loadLayout` changes on every render; the inputs that matter are these.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [canManage, only, onlyPlans, windows]);

  if (!overview) return <p className="muted">{t('arrange.loading')}</p>;
  if (!overview.supported) {
    return (
      <p role="status" className={shared.notice}>
        <Icon name="info" size={16} />
        <span>{overview.message}</span>
      </p>
    );
  }

  const monitors = overview.monitors;
  const monitor =
    monitors.find((m) => m.id === monitorId) ??
    monitors.find((m) => m.rect.x === 0 && m.rect.y === 0) ??
    monitors[0];
  const grid = monitor ? gridOf(draft, monitor.id) : null;
  const byId = new Map(windows.map((w) => [w.id, w]));
  const tray = windows.filter((w) => !locate(draft, w.id));
  const plans = toPlans(draft);
  const pickedWindow = picked ? (byId.get(picked) ?? null) : null;

  /** A change by the person: nothing loads over it any more. */
  const edit = (next: ArrangeDraft) => {
    touched.current = true;
    setDraft(next);
  };

  const put = (cell: number, window: string) => {
    if (!monitor) return;
    edit(place(draft, monitor.id, cell, window));
    setPicked(null);
  };

  const load = (layout: ArrangeLayout) => {
    const plans = overview.layout_plans?.[layout.id];
    if (!plans) return;
    touched.current = true;
    setPicked(null);
    const first = loadLayout(layout, plans);
    if (first) setMonitorId(first);
  };

  /**
   * Runs an arrangement and shows its report. From a paired device outside
   * its step-up window the server asks for a code (`totp_required`), as for
   * a restore: the code prompt opens and the same request is sent again
   * with the code. Cancelling it moves nothing.
   */
  const arrange = (run: (totp?: string) => Promise<ArrangeResult>) => {
    setError(null);
    setResult(null);
    setBusy(true);
    const withCode = async (): Promise<ArrangeResult | null> => {
      try {
        return await run();
      } catch (err) {
        if (!(err instanceof RequestError) || err.code !== 'totp_required' || !shell) throw err;
        return shell.withTotp(
          {
            title: t('arrange.totpTitle'),
            description: t('arrange.totpDescription'),
            submitLabel: t('arrange.totpSubmit'),
          },
          (code) => run(code),
        );
      }
    };
    withCode()
      .then(
        (done) => {
          if (done) setResult(done);
        },
        (err: Error) => {
          setError(t('error.request', { error: err.message }));
          void actions.refreshArrange().catch(() => {});
        },
      )
      .finally(() => setBusy(false));
  };

  const onDragStart = (e: DragEvent<HTMLElement>, window: string) => {
    dragged.current = window;
    setPicked(null);
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', window);
  };

  const onDrop = (e: DragEvent<HTMLElement>, cell: number) => {
    e.preventDefault();
    const window = dragged.current;
    dragged.current = null;
    if (window) put(cell, window);
  };

  const editor = canManage && monitor && grid && (
    <section className={`panel ${shared.block}`} aria-labelledby="arrange-grid">
      <h3 id="arrange-grid" className={shared.subtitle}>
        {t('arrange.gridTitle', {
          name: monitorName(monitor.id),
          width: monitor.work.w,
          height: monitor.work.h,
        })}
      </h3>
      {loadNote && (
        <p className={shared.note} role="status">
          {loadNote}
        </p>
      )}
      <div className={shared.row} role="group" aria-label={t('arrange.presets')}>
        {GRID_PRESETS.map((preset) => (
          <button
            key={preset.count}
            type="button"
            className="btn btn-sm"
            aria-label={t('arrange.preset', { rows: preset.rows, cols: preset.cols })}
            aria-pressed={grid.rows === preset.rows && grid.cols === preset.cols}
            onClick={() => edit(setGrid(draft, monitor.id, preset.rows, preset.cols))}
          >
            {preset.count}
          </button>
        ))}
      </div>
      <CustomGrid
        key={`${monitor.id}:${grid.rows}x${grid.cols}`}
        rows={grid.rows}
        cols={grid.cols}
        onSet={(rows, cols) => edit(setGrid(draft, monitor.id, rows, cols))}
      />
      <p className={shared.note}>{t('arrange.customHint', { rows: MAX_ROWS, cols: MAX_COLS })}</p>
      <div className={shared.row}>
        <label className="visually-hidden" htmlFor="arrange-group">
          {t('arrange.groupBy')}
        </label>
        <select
          id="arrange-group"
          className={shared.profile}
          value={groupBy}
          onChange={(e) => setGroupBy(e.target.value as GroupBy)}
        >
          {GROUPS.map((group) => (
            <option key={group.id} value={group.id}>
              {t(group.label)}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="btn btn-sm"
          disabled={windows.length === 0}
          onClick={() => edit(autoFill(draft, monitor.id, windows, order, groupBy))}
        >
          {t('arrange.autoFill')}
        </button>
        <button
          type="button"
          className="btn btn-sm"
          disabled={Object.keys(grid.cells).length === 0}
          onClick={() => edit(clearMonitor(draft, monitor.id))}
        >
          {t('arrange.clear')}
        </button>
      </div>
      <p className={shared.note} role="status">
        {pickedWindow
          ? t('arrange.picked', { title: windowLabel(pickedWindow) })
          : t('arrange.pickHint')}
      </p>
      <div
        className={styles.grid}
        role="group"
        aria-label={t('arrange.cells')}
        style={
          {
            gridTemplateColumns: `repeat(${grid.cols}, minmax(0, 1fr))`,
            '--aspect': monitor.work.w / monitor.work.h,
          } as CSSProperties
        }
      >
        {Array.from({ length: grid.rows * grid.cols }, (_, cell) => {
          const id = grid.cells[cell];
          const window = id === undefined ? null : byId.get(id);
          const position = {
            row: Math.floor(cell / grid.cols) + 1,
            col: (cell % grid.cols) + 1,
          };
          return (
            <div
              key={cell}
              className={styles.cell}
              data-filled={Boolean(window)}
              data-picked={Boolean(window) && picked === id}
              onDragOver={(e) => e.preventDefault()}
              onDrop={(e) => onDrop(e, cell)}
            >
              <button
                type="button"
                className={styles.cellButton}
                draggable={Boolean(window)}
                aria-label={
                  window
                    ? t('arrange.cell', { ...position, title: windowLabel(window) })
                    : t('arrange.cellEmpty', position)
                }
                onDragStart={(e) => {
                  if (id !== undefined) onDragStart(e, id);
                }}
                onClick={() => {
                  if (picked) put(cell, picked);
                  else if (id !== undefined) setPicked(id);
                }}
              >
                {window && (
                  <>
                    <span className={styles.cellTitle}>{windowLabel(window)}</span>
                    {window.sessions.length > 1 && (
                      <span className={styles.tabs}>
                        {t('arrange.tabsChip', { count: window.sessions.length })}
                      </span>
                    )}
                  </>
                )}
              </button>
              {window && (
                <button
                  type="button"
                  className={styles.cellRemove}
                  aria-label={t('arrange.remove', { title: windowLabel(window) })}
                  title={t('arrange.remove', { title: windowLabel(window) })}
                  onClick={() => {
                    edit(unplace(draft, window.id));
                    if (picked === window.id) setPicked(null);
                  }}
                >
                  <Icon name="close" size={14} />
                </button>
              )}
            </div>
          );
        })}
      </div>
      {cellsTooSmall(monitor, grid.rows, grid.cols) && (
        <p className={shared.note}>{t('arrange.smallCells')}</p>
      )}
    </section>
  );

  return (
    <div className={styles.panel}>
      {!canManage && <p className={shared.note}>{t('arrange.viewOnly')}</p>}
      {error && (
        <p role="alert" className={shared.error}>
          {error}
        </p>
      )}

      <section className={`panel ${shared.block}`} aria-labelledby="arrange-monitors">
        <h3 id="arrange-monitors" className={shared.subtitle}>
          {t('arrange.monitors')}
        </h3>
        {monitors.length === 0 ? (
          <p className="muted">{t('arrange.noMonitors')}</p>
        ) : (
          <MonitorMap
            monitors={monitors}
            draft={draft}
            selected={monitor?.id ?? null}
            {...(canManage
              ? {
                  onSelect: (id: string) => {
                    setMonitorId(id);
                  },
                }
              : {})}
          />
        )}
      </section>

      {editor}

      <section className={`panel ${shared.block}`} aria-labelledby="arrange-windows">
        <h3 id="arrange-windows" className={shared.subtitle}>
          {t('arrange.windows', { count: canManage ? tray.length : windows.length })}
        </h3>
        {windows.length === 0 ? (
          <p className="muted">{t('arrange.noWindows')}</p>
        ) : canManage && tray.length === 0 ? (
          <p className="muted">{t('arrange.allPlaced')}</p>
        ) : (
          <ul className={styles.tray}>
            {(canManage ? tray : windows).map((window) =>
              canManage ? (
                <li key={window.id}>
                  <button
                    type="button"
                    className={styles.trayItem}
                    draggable
                    aria-pressed={picked === window.id}
                    aria-label={t('arrange.pick', { title: windowLabel(window) })}
                    onDragStart={(e) => onDragStart(e, window.id)}
                    onClick={() => setPicked(picked === window.id ? null : window.id)}
                  >
                    <span className={shared.itemTitle}>{windowLabel(window)}</span>
                    <WindowFacts window={window} />
                  </button>
                </li>
              ) : (
                <li key={window.id} className={styles.trayItem}>
                  <span className={shared.itemTitle}>{windowLabel(window)}</span>
                  <WindowFacts window={window} />
                </li>
              ),
            )}
          </ul>
        )}
        {windows.some((w) => w.sessions.length > 1) && (
          <p className={shared.note}>{t('arrange.tabsHint')}</p>
        )}
      </section>

      {canManage && (
        <div className={shared.actions}>
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy || plans.length === 0}
            onClick={() => arrange((totp) => actions.arrangeApply(plans, totp))}
          >
            <Icon name="windows" size={16} /> {t('arrange.apply')}
          </button>
          <button
            type="button"
            className="btn"
            disabled={busy || overview.undo_windows === 0}
            onClick={() => arrange((totp) => actions.arrangeUndo(totp))}
          >
            <Icon name="restart" size={16} /> {t('arrange.undo')}
          </button>
          <button
            type="button"
            className="btn"
            disabled={plans.length === 0}
            onClick={() => setNaming('new')}
          >
            <Icon name="bookmark" size={16} /> {t('arrange.save')}
          </button>
        </div>
      )}

      {result && <Report result={result} />}

      <section className={`panel ${shared.block}`} aria-labelledby="arrange-layouts">
        <h3 id="arrange-layouts" className={shared.subtitle}>
          {t('arrange.layouts', { count: overview.layouts.length })}
        </h3>
        <SavedLayouts
          layouts={overview.layouts}
          canManage={canManage}
          busy={busy}
          loaded={loaded}
          {...(overview.layout_plans ? { onLoad: load } : {})}
          onApply={(layout) => arrange((totp) => actions.arrangeApplyLayout(layout.id, totp))}
          onRename={setNaming}
          onDelete={setRemoving}
        />
      </section>

      {naming && (
        <NameDialog
          title={t(naming === 'new' ? 'arrange.saveTitle' : 'arrange.renameTitle')}
          label={t('arrange.layoutName')}
          initial={naming === 'new' ? '' : naming.name}
          onClose={() => setNaming(null)}
          onSave={(name) =>
            actions.arrangeEdit(
              naming === 'new'
                ? { op: 'layout_save', name, monitors: plans }
                : { op: 'layout_rename', layout: naming.id, name },
            )
          }
        />
      )}
      {removing && (
        <ConfirmDialog
          title={t('arrange.deleteTitle')}
          message={t('arrange.deleteMessage', { name: removing.name })}
          confirmLabel={t('arrange.deleteConfirm')}
          danger
          onCancel={() => setRemoving(null)}
          onConfirm={() => {
            const layout = removing.id;
            setRemoving(null);
            setError(null);
            actions
              .arrangeEdit({ op: 'layout_delete', layout })
              .catch((err: Error) => setError(t('error.request', { error: err.message })));
          }}
        />
      )}
    </div>
  );
}
