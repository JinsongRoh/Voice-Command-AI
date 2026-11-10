import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type ChangeEvent,
  type FormEvent,
} from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { useNative } from '../native/context';
import { isLocalMachine } from '../native/host';
import { agentLabel } from '../protocol/helpers';
import {
  FEATURE_WINDOW_ARRANGE,
  type AgentKind,
  type RestoreEntry,
  type RestoreEntryDraft,
  type RestoreItem,
  type RestoreItemStatus,
  type RestoreJob,
  type RestoreProfile,
  type RestoreRunRequest,
  type SessionId,
} from '../protocol/types';
import { RequestError } from '../state/core';
import {
  useActions,
  useAppState,
  useMachines,
  type RestoreLocalServices,
  type RestoreLocalStatus,
} from '../state/store';
import { ArrangePanel } from './ArrangePanel';
import { Icon } from './Icon';
import { ConfirmDialog, Modal } from './Modal';
import { NameDialog } from './NameDialog';
import { SessionOrderPanel } from './SessionOrderPanel';
import { useDeviceScope, useShell } from './ShellProvider';
import styles from './WindowsPanel.module.css';

/** How often the open / closed marks are refreshed. */
const REFRESH_MS = 5000;

const STATUS: Record<RestoreItemStatus, MessageKey> = {
  waiting: 'windows.status.waiting',
  opening: 'windows.status.opening',
  placed: 'windows.status.placed',
  opened: 'windows.status.opened',
  skipped_open: 'windows.status.skippedOpen',
  skipped_missing: 'windows.status.skippedMissing',
  skipped_outside: 'windows.status.skippedOutside',
  failed: 'windows.status.failed',
};

const AGENTS: AgentKind[] = [
  { type: 'claude_code' },
  { type: 'codex' },
  { type: 'gemini_cli' },
  { type: 'antigravity' },
  { type: 'grok' },
  { type: 'cursor_cli' },
  { type: 'copilot_cli' },
  { type: 'aider' },
  { type: 'opencode' },
  { type: 'qwen_code' },
];

/** The shortcut and sign-in commands, on the computer's own desktop app only. */
function useRestoreLocal(): RestoreLocalServices | null {
  const native = useNative();
  const { machine } = useMachines();
  if (machine.simulate?.restoreLocal) return machine.simulate.restoreLocal;
  if (native?.host?.restoreLocal && isLocalMachine(machine.id)) return native.host.restoreLocal;
  return null;
}

/** Brings a session's terminal window to the front. */
export type BringToFront = (sessionId: SessionId, name: string) => Promise<void>;

/**
 * "Bring to front" for a session's terminal window: only in this computer's
 * own desktop app on Windows (the server refuses paired devices anyway), and
 * only ever called from a click. When Windows keeps the focus where it is,
 * a toast says so; nothing tries to force it.
 */
// eslint-disable-next-line react-refresh/only-export-components
export function useBringToFront(): BringToFront | null {
  const { t } = useI18n();
  const actions = useActions();
  const native = useNative();
  const { machine } = useMachines();
  const simulated = Boolean(machine.simulate?.restoreLocal);
  const here = Boolean(native?.host) && isLocalMachine(machine.id);
  const [windows, setWindows] = useState(false);

  useEffect(() => {
    if (!here || !native) return;
    let active = true;
    native.platform().then(
      (platform) => {
        if (active) setWindows(platform === 'windows');
      },
      () => {},
    );
    return () => {
      active = false;
    };
  }, [here, native]);

  const bring = useCallback<BringToFront>(
    async (sessionId, name) => {
      try {
        await actions.focusTerminal(sessionId);
      } catch (err) {
        const code = err instanceof RequestError ? err.code : null;
        actions.showToast({
          id: `front:${sessionId}`,
          category: 'error',
          sessionId,
          title: t('windows.frontFailed', { name }),
          body:
            code === 'unavailable'
              ? t('windows.frontRefused')
              : code === 'not_found'
                ? t('windows.frontNotFound')
                : (err as Error).message,
        });
      }
    },
    [actions, t],
  );

  return simulated || (here && windows) ? bring : null;
}

function draftOf(entry: RestoreEntry): RestoreEntryDraft {
  return {
    title: entry.title,
    agent: entry.agent,
    folder: entry.folder,
    session_id: entry.session_id,
    rect: entry.rect,
    maximized: entry.maximized,
    enabled: entry.enabled,
  };
}

function monitorLabel(entry: RestoreEntry): string | null {
  if (!entry.rect) return null;
  const size = `${entry.rect.w}×${entry.rect.h}`;
  const name = entry.monitor?.id.replace(/^\\\\\.\\/, '');
  return name ? `${name} · ${size}` : size;
}

function EntryDialog({
  entry,
  onSave,
  onClose,
}: {
  entry: RestoreEntry | null;
  onSave: (draft: RestoreEntryDraft) => Promise<void>;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const id = useId();
  const [title, setTitle] = useState(entry?.title ?? '');
  const [folder, setFolder] = useState(entry?.folder ?? '');
  const [agent, setAgent] = useState<AgentKind['type']>(
    (entry?.agent.type as AgentKind['type']) ?? 'claude_code',
  );
  const [sessionId, setSessionId] = useState(entry?.session_id ?? '');
  const [error, setError] = useState<string | null>(null);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    onSave({
      title,
      folder,
      agent: AGENTS.find((a) => a.type === agent) ?? { type: 'claude_code' },
      session_id: sessionId.trim() || null,
      rect: entry?.rect ?? null,
      maximized: entry?.maximized ?? false,
      enabled: entry?.enabled ?? true,
    }).catch((err: Error) => setError(err.message));
  };

  return (
    <Modal
      title={t(entry ? 'windows.editTitle' : 'windows.addTitle')}
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn" onClick={onClose}>
            {t('common.cancel')}
          </button>
          <button type="submit" form={`${id}-form`} className="btn btn-primary">
            {t('windows.save')}
          </button>
        </>
      }
    >
      <form id={`${id}-form`} className={styles.form} onSubmit={submit}>
        <div className="field">
          <label htmlFor={`${id}-title`}>{t('windows.field.title')}</label>
          <input
            id={`${id}-title`}
            value={title}
            maxLength={80}
            required
            onChange={(e) => setTitle(e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor={`${id}-agent`}>{t('windows.field.agent')}</label>
          <select
            id={`${id}-agent`}
            value={agent}
            onChange={(e) => setAgent(e.target.value as AgentKind['type'])}
          >
            {AGENTS.map((a) => (
              <option key={a.type} value={a.type}>
                {agentLabel(a)}
              </option>
            ))}
          </select>
        </div>
        <div className="field">
          <label htmlFor={`${id}-folder`}>{t('windows.field.folder')}</label>
          <input
            id={`${id}-folder`}
            value={folder}
            required
            onChange={(e) => setFolder(e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor={`${id}-session`}>{t('windows.field.session')}</label>
          <input
            id={`${id}-session`}
            value={sessionId}
            onChange={(e) => setSessionId(e.target.value)}
          />
        </div>
        {error && (
          <p role="alert" className={styles.error}>
            {error}
          </p>
        )}
      </form>
    </Modal>
  );
}

function ItemRow({
  item,
  onRetry,
}: {
  item: RestoreItem;
  /** Offered for failed windows. */
  onRetry?: () => void;
}) {
  const { t } = useI18n();
  return (
    <li className={styles.item} data-status={item.status}>
      <span className={styles.itemMain}>
        <span className={styles.itemTitle}>{item.title}</span>
        {item.command && <code className={styles.command}>{item.command}</code>}
        {item.message && <span className={styles.note}>{item.message}</span>}
      </span>
      <span className={styles.status}>{t(STATUS[item.status])}</span>
      {onRetry && item.status === 'failed' && (
        <button type="button" className="btn btn-sm" onClick={onRetry}>
          {t('windows.retry')}
        </button>
      )}
    </li>
  );
}

function Progress({ job, onRetry }: { job: RestoreJob; onRetry?: (id: string) => void }) {
  const { t } = useI18n();
  const running = job.finished_at === null;
  return (
    <section className={`panel ${styles.block}`} aria-labelledby="restore-progress">
      <h3 id="restore-progress" className={styles.subtitle}>
        {t(running ? 'windows.running' : 'windows.finished', { profile: job.profile })}
      </h3>
      <ul className={styles.items} aria-live="polite">
        {job.items.map((item) => (
          <ItemRow
            key={item.entry_id}
            item={item}
            {...(onRetry ? { onRetry: () => onRetry(item.entry_id) } : {})}
          />
        ))}
      </ul>
    </section>
  );
}

function LocalCard({
  services,
  profile,
  onEdit,
}: {
  services: RestoreLocalServices;
  profile: RestoreProfile;
  onEdit: (prompt: string | null) => void;
}) {
  const { t } = useI18n();
  const [status, setStatus] = useState<RestoreLocalStatus | null>(null);
  const [done, setDone] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [typing, setTyping] = useState(profile.auto_prompt !== null);
  const [prompt, setPrompt] = useState(profile.auto_prompt ?? t('windows.promptDefault'));

  useEffect(() => {
    let active = true;
    services.status().then(
      (s) => {
        if (active) setStatus(s);
      },
      () => {},
    );
    return () => {
      active = false;
    };
  }, [services]);

  const run = (p: Promise<RestoreLocalStatus>) => {
    setError(null);
    return p.then(setStatus, (err: Error) => setError(err.message));
  };

  return (
    <section className={`panel ${styles.block}`} aria-labelledby="restore-local">
      <h3 id="restore-local" className={styles.subtitle}>
        {t('windows.local')}
      </h3>
      <div className={styles.row}>
        <button
          type="button"
          className="btn btn-sm"
          onClick={() =>
            void run(services.createShortcut(t('windows.shortcutName'))).then(() => setDone(true))
          }
        >
          <Icon name="external" size={14} /> {t('windows.shortcut')}
        </button>
        {done && <span role="status">{t('windows.shortcutDone')}</span>}
      </div>
      <label className="check">
        <input
          type="checkbox"
          checked={status?.sign_in ?? false}
          disabled={!status}
          onChange={(e) =>
            void run(services.setSignIn(e.target.checked, status?.sign_in_delay_secs ?? 60))
          }
        />
        {t('windows.signIn')}
      </label>
      <p className={styles.note}>{t('windows.signInHint')}</p>
      <label className="check">
        <input
          type="checkbox"
          checked={typing}
          onChange={(e) => {
            setTyping(e.target.checked);
            onEdit(e.target.checked ? prompt : null);
          }}
        />
        {t('windows.prompt')}
      </label>
      <p className={styles.note}>{t('windows.promptHint')}</p>
      {typing && (
        <div className="field">
          <label htmlFor="restore-prompt">{t('windows.promptField')}</label>
          <input
            id="restore-prompt"
            value={prompt}
            maxLength={200}
            onChange={(e) => setPrompt(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') e.currentTarget.blur();
            }}
            onBlur={() => onEdit(prompt)}
          />
        </div>
      )}
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
    </section>
  );
}

/** The managed list of AI CLI windows of the selected computer. */
type WindowsTab = 'restore' | 'order' | 'arrange';
const WINDOWS_TABS: WindowsTab[] = ['restore', 'order', 'arrange'];
const WINDOWS_TAB_KEY = 'vca.windowsTab';
const TAB_LABEL: Record<WindowsTab, MessageKey> = {
  restore: 'windows.tab.restore',
  order: 'windows.tab.order',
  arrange: 'windows.tab.arrange',
};
const TAB_HINT: Record<WindowsTab, MessageKey> = {
  restore: 'windows.tab.restoreHint',
  order: 'windows.tab.orderHint',
  arrange: 'windows.tab.arrangeHint',
};

/** The tab last open in this browser (a convenience; falls back to "restore"). */
function readWindowsTab(): WindowsTab {
  try {
    const saved = localStorage.getItem(WINDOWS_TAB_KEY);
    return WINDOWS_TABS.find((id) => id === saved) ?? 'restore';
  } catch {
    return 'restore';
  }
}

function writeWindowsTab(tab: WindowsTab) {
  try {
    localStorage.setItem(WINDOWS_TAB_KEY, tab);
  } catch {
    // Storage may be unavailable (private mode); the tab just is not remembered.
  }
}

export function WindowsPanel() {
  const { t, relative } = useI18n();
  const actions = useActions();
  const shell = useShell();
  const scope = useDeviceScope();
  const local = useRestoreLocal();
  const bringToFront = useBringToFront();
  const { machine } = useMachines();
  const overview = useAppState((s) => s.restore);
  const canArrange = useAppState((s) => s.serverFeatures.includes(FEATURE_WINDOW_ARRANGE));
  const canManage = scope === null || scope === 'admin';
  const [profileId, setProfileId] = useState<string | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [editing, setEditing] = useState<RestoreEntry | 'new' | null>(null);
  const [removing, setRemoving] = useState<RestoreEntry | null>(null);
  const [naming, setNaming] = useState<'new' | 'rename' | null>(null);
  const [removingProfile, setRemovingProfile] = useState(false);
  const [preview, setPreview] = useState<{
    request: RestoreRunRequest;
    job: RestoreJob;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [chosenTab, setTab] = useState<WindowsTab>(readWindowsTab);
  const tabs = WINDOWS_TABS.filter((id) => id !== 'arrange' || canArrange);
  const tab = tabs.includes(chosenTab) ? chosenTab : 'restore';
  const fileInput = useRef<HTMLInputElement>(null);
  const selectTab = (next: WindowsTab) => {
    setTab(next);
    writeWindowsTab(next);
    requestAnimationFrame(() => document.getElementById(`windows-tab-${next}`)?.focus());
  };

  useEffect(() => {
    const load = () => void actions.refreshRestore().catch(() => {});
    load();
    const timer = setInterval(load, REFRESH_MS);
    return () => clearInterval(timer);
  }, [actions]);

  const fail = (err: unknown) => setError(t('error.request', { error: (err as Error).message }));
  const guard = (p: Promise<unknown>) => {
    setError(null);
    p.catch(fail);
  };

  if (!overview) return <p className="muted">{t('windows.loading')}</p>;

  const profile =
    overview.profiles.find((p) => p.id === (profileId ?? overview.active_profile)) ??
    overview.profiles[0];
  if (!profile) return <p className="muted">{t('windows.loading')}</p>;
  const entries = [...profile.entries].sort((a, b) => a.order - b.order);
  const liveOf = (id: string) => overview.live.find((l) => l.entry_id === id);
  const running = overview.job !== null && overview.job.finished_at === null;

  const edit = actions.restoreEdit;
  const reorder = (index: number, offset: number) => {
    const ids = entries.map((e) => e.id);
    const [id] = ids.splice(index, 1);
    ids.splice(index + offset, 0, id!);
    guard(edit({ op: 'entry_reorder', profile: profile.id, entry_ids: ids }));
  };
  const saveSettings = (delay: number, prompt: string | null) =>
    guard(
      edit({
        op: 'profile_settings',
        profile: profile.id,
        delay_secs: delay,
        auto_prompt: prompt,
      }),
    );

  /** The dry run first: the person sees what will open before anything does. */
  const askToRun = (request: RestoreRunRequest) => {
    setError(null);
    actions
      .restoreRun({ ...request, dry_run: true })
      .then((job) => setPreview({ request, job }), fail);
  };

  const start = async (request: RestoreRunRequest) => {
    setPreview(null);
    setError(null);
    if (shell && !(await shell.confirmPresence(t('windows.presence')))) return;
    try {
      await actions.restoreRun(request);
    } catch (err) {
      if (!(err instanceof RequestError) || err.code !== 'totp_required' || !shell) throw err;
      await shell.withTotp(
        {
          title: t('windows.totpTitle'),
          description: t('windows.totpDescription', { profile: profile.name }),
          submitLabel: t('windows.restore'),
        },
        (code) => actions.restoreRun({ ...request, totp: code }),
      );
    }
  };

  const onImport = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = '';
    if (!file) return;
    setError(null);
    actions.restoreImport(await file.text(), profile.id).catch(fail);
  };

  const opening = preview?.job.items.filter((i) => i.status === 'waiting').length ?? 0;

  return (
    <section className={styles.panel} aria-labelledby="windows-title">
      <div className={styles.head}>
        <h2 id="windows-title" className={styles.title}>
          {t('windows.title')}
        </h2>
        <span className={styles.machine}>{machine.name}</span>
      </div>
      {overview.message && tab !== 'arrange' && (
        <p role="status" className={styles.notice}>
          <Icon name="info" size={16} />
          <span>{overview.message}</span>
        </p>
      )}
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      <div className={styles.tabs} role="tablist" aria-label={t('windows.tabs')}>
        {tabs.map((id, index) => (
          <button
            key={id}
            type="button"
            role="tab"
            id={`windows-tab-${id}`}
            aria-selected={tab === id}
            aria-controls={`windows-panel-${id}`}
            tabIndex={tab === id ? 0 : -1}
            className={styles.tab}
            onClick={() => selectTab(id)}
            onKeyDown={(e) => {
              if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
                e.preventDefault();
                const step = e.key === 'ArrowRight' ? 1 : tabs.length - 1;
                selectTab(tabs[(index + step) % tabs.length]!);
              }
            }}
          >
            {t(TAB_LABEL[id])}
          </button>
        ))}
      </div>

      <div
        role="tabpanel"
        id={`windows-panel-${tab}`}
        aria-labelledby={`windows-tab-${tab}`}
        className={styles.tabPanel}
      >
        <p className={styles.tabHint}>
          <Icon name="info" size={16} />
          <span>{t(TAB_HINT[tab])}</span>
        </p>
        {/* Sessions VCA started have no PC window: say where they go. */}
        {tab === 'restore' && (
          <p className={styles.tabHint} data-testid="restore-vca-note">
            <Icon name="info" size={16} />
            <span>{t('windows.restoreVcaNote')}</span>
          </p>
        )}
        {tab === 'order' ? <SessionOrderPanel /> : null}
        {tab === 'arrange' ? <ArrangePanel key={machine.id} /> : null}
      </div>

      {tab === 'restore' && !canManage && <p className={styles.note}>{t('windows.viewOnly')}</p>}

      {tab === 'restore' && (
        <>
          <div className={`panel ${styles.block}`}>
            <div className={styles.row}>
              <label className="visually-hidden" htmlFor="restore-profile">
                {t('windows.profile')}
              </label>
              <select
                id="restore-profile"
                className={styles.profile}
                value={profile.id}
                onChange={(e) => {
                  setProfileId(e.target.value);
                  setSelected([]);
                  if (canManage) {
                    guard(edit({ op: 'profile_activate', profile: e.target.value }));
                  }
                }}
              >
                {overview.profiles.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </select>
              {canManage && (
                <>
                  <button
                    type="button"
                    className="icon-btn"
                    aria-label={t('windows.addProfile')}
                    title={t('windows.addProfile')}
                    onClick={() => setNaming('new')}
                  >
                    <Icon name="plus" size={18} />
                  </button>
                  <button
                    type="button"
                    className="icon-btn"
                    aria-label={t('windows.renameProfile')}
                    title={t('windows.renameProfile')}
                    onClick={() => setNaming('rename')}
                  >
                    <Icon name="edit" size={18} />
                  </button>
                  <button
                    type="button"
                    className="icon-btn"
                    aria-label={t('windows.deleteProfile')}
                    title={t('windows.deleteProfile')}
                    disabled={overview.profiles.length < 2}
                    onClick={() => setRemovingProfile(true)}
                  >
                    <Icon name="close" size={18} />
                  </button>
                </>
              )}
            </div>
            <div className={styles.actions}>
              <button
                type="button"
                className="btn btn-primary"
                disabled={!canManage || !overview.supported || running || entries.length === 0}
                onClick={() => askToRun({ profile: profile.id })}
              >
                <Icon name="restart" size={16} /> {t('windows.restoreAll')}
              </button>
              <button
                type="button"
                className="btn"
                disabled={!canManage || !overview.supported || running || selected.length === 0}
                onClick={() => askToRun({ profile: profile.id, entry_ids: selected })}
              >
                {t('windows.restoreSelected', { count: selected.length })}
              </button>
              <button
                type="button"
                className="btn"
                disabled={!canManage || !overview.supported}
                onClick={() => guard(actions.restoreCapture(profile.id))}
              >
                <Icon name="windows" size={16} /> {t('windows.capture')}
              </button>
            </div>
            <p className={styles.note}>
              {overview.last_snapshot_at
                ? t('windows.lastSnapshot', {
                    time: relative(overview.last_snapshot_at),
                    minutes: Math.round(overview.snapshot_interval_secs / 60),
                  })
                : t('windows.noSnapshot', {
                    minutes: Math.round(overview.snapshot_interval_secs / 60),
                  })}
            </p>
          </div>

          {overview.job && !overview.job.dry_run && (
            <Progress
              job={overview.job}
              {...(canManage
                ? {
                    onRetry: (id: string) =>
                      void start({ profile: profile.id, entry_ids: [id] }).catch(fail),
                  }
                : {})}
            />
          )}

          <section className={`panel ${styles.block}`} aria-labelledby="restore-list">
            <div className={styles.listHead}>
              <h3 id="restore-list" className={styles.subtitle}>
                {t('windows.list', { count: entries.length })}
              </h3>
              {canManage && (
                <div className={styles.row}>
                  <button type="button" className="btn btn-sm" onClick={() => setEditing('new')}>
                    <Icon name="plus" size={14} /> {t('windows.add')}
                  </button>
                  <button
                    type="button"
                    className="btn btn-sm"
                    onClick={() => fileInput.current?.click()}
                  >
                    <Icon name="upload" size={14} /> {t('windows.import')}
                  </button>
                  <input
                    ref={fileInput}
                    type="file"
                    accept=".json,application/json"
                    hidden
                    aria-label={t('windows.import')}
                    onChange={(e) => void onImport(e)}
                  />
                </div>
              )}
            </div>
            {entries.length === 0 ? (
              <p className="muted">{t('windows.empty')}</p>
            ) : (
              <ul className={styles.entries}>
                {entries.map((entry, index) => {
                  const live = liveOf(entry.id);
                  const open = live?.open ?? false;
                  const sessionId = live?.session_id ?? null;
                  return (
                    <li key={entry.id} className={styles.entry} data-enabled={entry.enabled}>
                      <input
                        type="checkbox"
                        className={styles.pick}
                        aria-label={t('windows.select', { title: entry.title })}
                        checked={selected.includes(entry.id)}
                        onChange={(e) =>
                          setSelected((s) =>
                            e.target.checked ? [...s, entry.id] : s.filter((id) => id !== entry.id),
                          )
                        }
                      />
                      <div className={styles.entryMain}>
                        <span className={styles.entryTitle}>{entry.title}</span>
                        <span className={styles.folder} title={entry.folder}>
                          {agentLabel(entry.agent)} · {entry.folder}
                        </span>
                        <span className={styles.chips}>
                          <span className={styles.chip} data-open={open}>
                            {t(open ? 'windows.open' : 'windows.closed')}
                          </span>
                          {monitorLabel(entry) && (
                            <span className={styles.chip}>{monitorLabel(entry)}</span>
                          )}
                          {entry.maximized && (
                            <span className={styles.chip}>{t('windows.maximized')}</span>
                          )}
                          <span className={styles.chip}>
                            {t(entry.session_id ? 'windows.resumeExact' : 'windows.resumeLatest')}
                          </span>
                        </span>
                        {entry.note && <span className={styles.note}>{entry.note}</span>}
                      </div>
                      {bringToFront && open && sessionId && (
                        <button
                          type="button"
                          className="icon-btn"
                          aria-label={t('windows.frontLabel', { name: entry.title })}
                          title={t('windows.front')}
                          onClick={() => void bringToFront(sessionId, entry.title)}
                        >
                          <Icon name="windows" size={18} />
                        </button>
                      )}
                      {canManage && (
                        <div className={styles.entryActions}>
                          <label
                            className="check"
                            title={t('windows.enable', { title: entry.title })}
                          >
                            <input
                              type="checkbox"
                              checked={entry.enabled}
                              onChange={(e) =>
                                guard(
                                  edit({
                                    op: 'entry_set_enabled',
                                    profile: profile.id,
                                    entry_id: entry.id,
                                    enabled: e.target.checked,
                                  }),
                                )
                              }
                            />
                            <span className="visually-hidden">
                              {t('windows.enable', { title: entry.title })}
                            </span>
                          </label>
                          <button
                            type="button"
                            className="icon-btn"
                            disabled={index === 0}
                            aria-label={t('windows.moveUp', { title: entry.title })}
                            onClick={() => reorder(index, -1)}
                          >
                            <Icon name="up" size={18} />
                          </button>
                          <button
                            type="button"
                            className="icon-btn"
                            disabled={index === entries.length - 1}
                            aria-label={t('windows.moveDown', { title: entry.title })}
                            onClick={() => reorder(index, 1)}
                          >
                            <Icon name="down" size={18} />
                          </button>
                          <button
                            type="button"
                            className="icon-btn"
                            aria-label={t('windows.edit', { title: entry.title })}
                            onClick={() => setEditing(entry)}
                          >
                            <Icon name="edit" size={18} />
                          </button>
                          <button
                            type="button"
                            className="icon-btn"
                            aria-label={t('windows.delete', { title: entry.title })}
                            onClick={() => setRemoving(entry)}
                          >
                            <Icon name="close" size={18} />
                          </button>
                        </div>
                      )}
                    </li>
                  );
                })}
              </ul>
            )}
          </section>

          {canManage && (
            <section className={`panel ${styles.block}`} aria-labelledby="restore-settings">
              <h3 id="restore-settings" className={styles.subtitle}>
                {t('windows.settings')}
              </h3>
              <div className="field">
                <label htmlFor="restore-delay">{t('windows.delay')}</label>
                <input
                  key={profile.delay_secs}
                  id="restore-delay"
                  className={styles.delay}
                  type="number"
                  min={0}
                  max={120}
                  defaultValue={profile.delay_secs}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') e.currentTarget.blur();
                  }}
                  onBlur={(e) => {
                    // The min/max attributes only hint; clamp what is typed.
                    const typed = Number(e.target.value);
                    const delay = Number.isFinite(typed)
                      ? Math.min(120, Math.max(0, Math.round(typed)))
                      : profile.delay_secs;
                    e.target.value = String(delay);
                    if (delay !== profile.delay_secs) saveSettings(delay, profile.auto_prompt);
                  }}
                />
              </div>
            </section>
          )}

          {local && (
            <LocalCard
              key={profile.id}
              services={local}
              profile={profile}
              onEdit={(prompt) => saveSettings(profile.delay_secs, prompt)}
            />
          )}
        </>
      )}

      {editing && (
        <EntryDialog
          entry={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
          onSave={async (draft) => {
            await edit(
              editing === 'new'
                ? { op: 'entry_add', profile: profile.id, entry: draft }
                : {
                    op: 'entry_update',
                    profile: profile.id,
                    entry_id: editing.id,
                    entry: { ...draftOf(editing), ...draft },
                  },
            );
            setEditing(null);
          }}
        />
      )}
      {naming && (
        <NameDialog
          title={t(naming === 'new' ? 'windows.addProfile' : 'windows.renameProfile')}
          label={t('windows.profileName')}
          initial={naming === 'new' ? '' : profile.name}
          onClose={() => setNaming(null)}
          onSave={(name) =>
            edit(
              naming === 'new'
                ? { op: 'profile_add', name }
                : { op: 'profile_rename', profile: profile.id, name },
            )
          }
        />
      )}
      {removing && (
        <ConfirmDialog
          title={t('windows.deleteTitle')}
          message={t('windows.deleteMessage', { title: removing.title })}
          confirmLabel={t('windows.deleteConfirm')}
          danger
          onCancel={() => setRemoving(null)}
          onConfirm={() => {
            const entryId = removing.id;
            setRemoving(null);
            guard(edit({ op: 'entry_delete', profile: profile.id, entry_id: entryId }));
          }}
        />
      )}
      {removingProfile && (
        <ConfirmDialog
          title={t('windows.deleteProfile')}
          message={t('windows.deleteProfileMessage', { name: profile.name })}
          confirmLabel={t('windows.deleteConfirm')}
          danger
          onCancel={() => setRemovingProfile(false)}
          onConfirm={() => {
            setRemovingProfile(false);
            setProfileId(null);
            guard(edit({ op: 'profile_delete', profile: profile.id }));
          }}
        />
      )}
      {preview && (
        <Modal
          title={t('windows.previewTitle')}
          onClose={() => setPreview(null)}
          footer={
            <>
              <button type="button" className="btn" onClick={() => setPreview(null)}>
                {t('common.cancel')}
              </button>
              <button
                type="button"
                className="btn btn-primary"
                disabled={opening === 0}
                onClick={() => void start(preview.request).catch(fail)}
              >
                {t('windows.previewConfirm', { count: opening })}
              </button>
            </>
          }
        >
          <p className={styles.note}>{t('windows.previewHint')}</p>
          <ul className={styles.items}>
            {preview.job.items.map((item) => (
              <ItemRow key={item.entry_id} item={item} />
            ))}
          </ul>
        </Modal>
      )}
    </section>
  );
}
