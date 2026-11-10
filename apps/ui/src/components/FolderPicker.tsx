import {
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from 'react';

import { useI18n } from '../i18n/context';
import type { FolderEntry, FolderListing, ProjectFolders } from '../protocol/types';
import { RequestError } from '../state/core';
import { useActions } from '../state/store';
import { Icon } from './Icon';
import styles from './FolderPicker.module.css';

/** The chosen project folder; `create` when the server will create it. */
export interface FolderSelection {
  path: string;
  create: boolean;
}

type Tab = 'recent' | 'browse' | 'create';

const LAST_KEY = 'vca.folderPicker.last.';

function loadLast(machineId: string): string | null {
  try {
    return localStorage.getItem(LAST_KEY + machineId);
  } catch {
    return null;
  }
}

function saveLast(machineId: string, path: string) {
  try {
    localStorage.setItem(LAST_KEY + machineId, path);
  } catch {
    // Private mode or storage full: the picker just starts at the roots.
  }
}

/**
 * A shorter form of a server path for display only: the drive or root, an
 * ellipsis and the last two folders (`C:\…\code\payments-api`). Paths sent
 * to the server are always the server's own strings.
 */
// eslint-disable-next-line react-refresh/only-export-components
export function shortenPath(path: string, separator: string, keep = 2): string {
  const parts = path.split(separator).filter(Boolean);
  if (parts.length <= keep + 1) return path;
  const head = path.startsWith(separator) ? separator + parts[0] : parts[0];
  const tail = parts.slice(-keep).join(separator);
  return `${head}${separator}…${separator}${tail}`;
}

/** Arrow keys, Home and End move the focus between the rows of a list. */
function onListKeys(e: KeyboardEvent<HTMLUListElement>) {
  const rows = Array.from(e.currentTarget.querySelectorAll<HTMLElement>('[data-row]'));
  if (rows.length === 0) return;
  const at = rows.indexOf(document.activeElement as HTMLElement);
  let next: number | null = null;
  if (e.key === 'ArrowDown') next = at < 0 ? 0 : Math.min(rows.length - 1, at + 1);
  else if (e.key === 'ArrowUp') next = at < 0 ? 0 : Math.max(0, at - 1);
  else if (e.key === 'Home') next = 0;
  else if (e.key === 'End') next = rows.length - 1;
  if (next === null) return;
  e.preventDefault();
  rows[next]?.focus();
}

/**
 * Chooses a project folder on the server: recent projects, browsing inside
 * the workspace roots, or a new folder inside the browsed one. Calls
 * `onUnavailable` when the server has no folder picker (older servers) or
 * cannot list folders, so the caller can offer a typed path instead.
 */
export function FolderPicker({
  machineId,
  allowCreate,
  onChange,
  onUnavailable,
}: {
  machineId: string;
  /** Offer "New folder" (not for shells, which need an existing folder). */
  allowCreate: boolean;
  onChange: (selection: FolderSelection | null) => void;
  onUnavailable: (reason: string | null) => void;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const id = useId();
  const [info, setInfo] = useState<ProjectFolders | null>(null);
  const [chosenTab, setTab] = useState<Tab>('recent');
  // Shells cannot create folders: their "New folder" tab falls back to Browse.
  const tab: Tab = chosenTab === 'create' && !allowCreate ? 'browse' : chosenTab;
  /** `null`: the list of locations (workspace roots). */
  const [listing, setListing] = useState<FolderListing | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState('');
  const [picked, setPicked] = useState<FolderEntry | null>(null);
  const [newName, setNewName] = useState('');
  /** The server's answer for a new folder name (`key`: parent and name). */
  const [resolved, setResolved] = useState<{
    key: string;
    path?: string;
    exists?: boolean;
    error?: string;
  } | null>(null);
  const browseSeq = useRef(0);
  const onUnavailableRef = useRef(onUnavailable);
  const onChangeRef = useRef(onChange);
  useEffect(() => {
    onUnavailableRef.current = onUnavailable;
    onChangeRef.current = onChange;
  });

  const sep = info?.separator ?? '/';

  const browse = useCallback(
    async (path: string | null, remember = true): Promise<boolean> => {
      const seq = ++browseSeq.current;
      setFilter('');
      if (path === null) {
        setListing(null);
        setError(null);
        return true;
      }
      setLoading(true);
      try {
        const next = await actions.listFolder(path);
        if (seq !== browseSeq.current) return false;
        setListing(next);
        setError(null);
        // Opening a folder makes it the choice until a subfolder is picked.
        setPicked(null);
        if (remember) saveLast(machineId, next.path);
        return true;
      } catch (err) {
        if (seq === browseSeq.current) setError((err as Error).message);
        return false;
      } finally {
        if (seq === browseSeq.current) setLoading(false);
      }
    },
    [actions, machineId],
  );

  // First load: roots and recent projects, then the last browsed folder.
  useEffect(() => {
    let live = true;
    actions.projectFolders().then(
      async (folders) => {
        if (!live) return;
        setInfo(folders);
        if (folders.recent.length === 0) setTab('browse');
        const last = loadLast(machineId);
        const opened = last !== null && (await browse(last, false));
        if (!live || opened) return;
        setError(null);
        if (folders.roots.length === 1) await browse(folders.roots[0]!.path, false);
      },
      (err: unknown) => {
        if (!live) return;
        const unsupported = err instanceof RequestError && err.code === 'unsupported';
        onUnavailableRef.current(unsupported ? null : (err as Error).message);
      },
    );
    return () => {
      live = false;
    };
  }, [actions, machineId, browse]);

  // The parent of a new folder: the folder being browsed, else the first root.
  const parent = listing?.path ?? info?.roots[0]?.path ?? null;

  // Resolve the new folder's path on the server as the name is typed.
  const name = newName.trim();
  const nameKey =
    tab === 'create' && name && parent !== null
      ? `${parent}
${name}`
      : null;
  useEffect(() => {
    if (nameKey === null || parent === null) return;
    let live = true;
    const timer = setTimeout(() => {
      actions.folderPath(parent, name).then(
        (res) => {
          if (live) setResolved({ key: nameKey, ...res });
        },
        (err: unknown) => {
          if (live) setResolved({ key: nameKey, error: (err as Error).message });
        },
      );
    }, 200);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [actions, nameKey, parent, name]);
  const current = resolved !== null && resolved.key === nameKey ? resolved : null;
  const created =
    current?.path !== undefined ? { path: current.path, exists: !!current.exists } : null;
  const nameError = current?.error ?? null;

  const selection: FolderSelection | null =
    tab === 'create'
      ? created && { path: created.path, create: !created.exists }
      : picked
        ? { path: picked.path, create: false }
        : tab === 'browse' && listing
          ? { path: listing.path, create: false }
          : null;
  const selPath = selection?.path ?? null;
  const selCreate = selection?.create ?? false;
  useEffect(() => {
    onChangeRef.current(selPath === null ? null : { path: selPath, create: selCreate });
  }, [selPath, selCreate]);

  const shown = useMemo(() => {
    const entries = listing?.entries ?? [];
    const q = filter.trim().toLowerCase();
    return q ? entries.filter((e) => e.name.toLowerCase().includes(q)) : entries;
  }, [listing, filter]);

  if (!info) {
    return (
      <p className="muted" role="status">
        {t('folderPicker.loading')}
      </p>
    );
  }

  const tabs: Tab[] = allowCreate ? ['recent', 'browse', 'create'] : ['recent', 'browse'];
  const tabLabel = (k: Tab) =>
    k === 'recent'
      ? t('folderPicker.recent')
      : k === 'browse'
        ? t('folderPicker.browse')
        : t('folderPicker.create');

  const badges = (e: FolderEntry) => (
    <>
      {e.is_git_repo && (
        <span className={styles.tag} title={t('folderPicker.gitRepo')}>
          {t('folderPicker.git')}
          <span className="visually-hidden"> ({t('folderPicker.gitRepo')})</span>
        </span>
      )}
      {e.has_agent_history && (
        <span className={styles.dot} title={t('folderPicker.history')}>
          <span className="visually-hidden">{t('folderPicker.history')}</span>
        </span>
      )}
    </>
  );

  const multiRoot = info.roots.length > 1;

  return (
    <div className={styles.picker}>
      <div className="segmented" role="tablist" aria-label={t('folderPicker.tabs')}>
        {tabs.map((k) => (
          <button
            key={k}
            type="button"
            role="tab"
            id={`${id}-tab-${k}`}
            aria-selected={tab === k}
            aria-controls={`${id}-panel`}
            onClick={() => setTab(k)}
          >
            {tabLabel(k)}
          </button>
        ))}
      </div>

      <div
        id={`${id}-panel`}
        role="tabpanel"
        aria-labelledby={`${id}-tab-${tab}`}
        className={styles.panel}
      >
        {tab === 'recent' &&
          (info.recent.length === 0 ? (
            <p className="muted">{t('folderPicker.recentEmpty')}</p>
          ) : (
            <ul
              className={styles.list}
              aria-label={t('folderPicker.recentList')}
              onKeyDown={onListKeys}
            >
              {info.recent.map((e) => (
                <li key={e.path} className={styles.row}>
                  <button
                    type="button"
                    data-row
                    className={styles.main}
                    aria-pressed={picked?.path === e.path}
                    onClick={() => setPicked(e)}
                    title={e.path}
                  >
                    <span className={styles.name}>
                      {e.name}
                      {badges(e)}
                    </span>
                    <span className={styles.path}>{shortenPath(e.path, sep)}</span>
                  </button>
                  {picked?.path === e.path && (
                    <Icon name="check" className={styles.check} label={t('folderPicker.chosen')} />
                  )}
                </li>
              ))}
            </ul>
          ))}

        {tab !== 'recent' && (
          <nav aria-label={t('folderPicker.breadcrumbs')} className={styles.crumbs}>
            <ol>
              {multiRoot && (
                <li>
                  <button
                    type="button"
                    onClick={() => void browse(null)}
                    aria-current={listing === null ? 'location' : undefined}
                  >
                    {t('folderPicker.locations')}
                  </button>
                </li>
              )}
              {listing?.breadcrumbs.map((c, i) => (
                <li key={c.path}>
                  <button
                    type="button"
                    onClick={() => void browse(c.path)}
                    aria-current={i === listing.breadcrumbs.length - 1 ? 'location' : undefined}
                    title={c.path}
                  >
                    {i === 0 ? shortenPath(c.name, sep, 1) : c.name}
                  </button>
                </li>
              ))}
            </ol>
          </nav>
        )}

        {tab === 'browse' && (
          <>
            {listing === null ? (
              <ul
                className={styles.list}
                aria-label={t('folderPicker.locations')}
                onKeyDown={onListKeys}
              >
                {info.roots.map((r) => (
                  <li key={r.path} className={styles.row}>
                    <button
                      type="button"
                      data-row
                      className={styles.main}
                      onClick={() => void browse(r.path)}
                      aria-label={t('folderPicker.open', { name: r.path })}
                    >
                      <span className={styles.name}>{r.path}</span>
                    </button>
                    <Icon name="chevron" className={styles.chev} />
                  </li>
                ))}
              </ul>
            ) : (
              <>
                <div className={styles.toolbar}>
                  <label className={styles.filter}>
                    <Icon name="search" size={16} />
                    <span className="visually-hidden">{t('folderPicker.filter')}</span>
                    <input
                      type="search"
                      value={filter}
                      placeholder={t('folderPicker.filterPlaceholder')}
                      onChange={(e) => setFilter(e.target.value)}
                      autoCapitalize="off"
                      spellCheck={false}
                    />
                  </label>
                  {listing.parent !== null && (
                    <button
                      type="button"
                      className="btn"
                      onClick={() => void browse(listing.parent)}
                    >
                      <Icon name="up" size={16} />
                      {t('folderPicker.up')}
                    </button>
                  )}
                </div>
                <button
                  type="button"
                  className={`btn ${selPath === listing.path ? 'btn-primary' : ''} ${styles.useThis}`}
                  aria-pressed={selPath === listing.path}
                  onClick={() =>
                    setPicked({
                      name: listing.name,
                      path: listing.path,
                      is_git_repo: false,
                      has_agent_history: false,
                    })
                  }
                >
                  {t('folderPicker.useThis', { name: listing.name })}
                </button>
                {selPath !== null && (
                  <p className={styles.chosen} role="status">
                    {selPath === listing.path
                      ? t('folderPicker.chosenHere', { path: selPath })
                      : t('folderPicker.chosen', { path: selPath })}
                  </p>
                )}
                {shown.length === 0 ? (
                  <p className="muted">
                    {filter.trim()
                      ? t('folderPicker.noMatch', { query: filter.trim() })
                      : t('folderPicker.empty')}
                  </p>
                ) : (
                  <ul
                    className={styles.list}
                    aria-label={t('folderPicker.folderList', { name: listing.name })}
                    aria-describedby={`${id}-keys`}
                    onKeyDown={onListKeys}
                  >
                    {shown.map((e) => (
                      <li key={e.path} className={styles.row}>
                        <button
                          type="button"
                          data-row
                          className={styles.main}
                          onClick={() => void browse(e.path)}
                          aria-label={t('folderPicker.open', { name: e.name })}
                        >
                          <span className={styles.name}>
                            {e.name}
                            {badges(e)}
                          </span>
                        </button>
                        <button
                          type="button"
                          className={`btn ${styles.select} ${picked?.path === e.path ? 'btn-primary' : ''}`}
                          aria-pressed={picked?.path === e.path}
                          aria-label={t('folderPicker.selectNamed', { name: e.name })}
                          onClick={() => setPicked(e)}
                        >
                          {t('folderPicker.select')}
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
                <p id={`${id}-keys`} className="visually-hidden">
                  {t('folderPicker.keyboardHint')}
                </p>
                {listing.truncated && (
                  <p className="muted">
                    {t('folderPicker.truncated', { count: listing.entries.length })}
                  </p>
                )}
              </>
            )}
            {loading && (
              <p className="muted" role="status">
                {t('folderPicker.loading')}
              </p>
            )}
            {error && (
              <p role="alert" className={styles.error}>
                {t('folderPicker.error', { error })}
              </p>
            )}
          </>
        )}

        {tab === 'create' && parent !== null && (
          <div className={styles.create}>
            <p className={styles.createIn}>
              <span>{t('folderPicker.createIn', { path: shortenPath(parent, sep) })}</span>
              <button type="button" className="btn" onClick={() => setTab('browse')}>
                {t('folderPicker.changeParent')}
              </button>
            </p>
            <div className="field">
              <label htmlFor={`${id}-name`}>{t('folderPicker.newName')}</label>
              <input
                id={`${id}-name`}
                type="text"
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                autoCapitalize="off"
                spellCheck={false}
                aria-invalid={nameError !== null}
                aria-describedby={`${id}-name-status`}
              />
            </div>
            <p id={`${id}-name-status`} className="muted" aria-live="polite">
              {nameError
                ? t('folderPicker.invalidName', { error: nameError })
                : created
                  ? created.exists
                    ? t('folderPicker.exists', { path: created.path })
                    : t('folderPicker.willCreate', { path: created.path })
                  : ''}
            </p>
          </div>
        )}
      </div>

      <p className={styles.selected} aria-live="polite">
        {selection ? t('folderPicker.selected', { path: selection.path }) : t('folderPicker.none')}
      </p>
    </div>
  );
}
