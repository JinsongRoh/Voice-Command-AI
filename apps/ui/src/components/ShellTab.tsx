import { lazy, Suspense, useEffect, useState } from 'react';

import { useI18n } from '../i18n/context';
import { isOpenShell, isShellSession } from '../protocol/helpers';
import type { SessionInfo } from '../protocol/types';
import { useActions, useAppState } from '../state/store';
import { Icon } from './Icon';
import { ConfirmDialog } from './Modal';
import { ShellBadge, ShellCountdown } from './ShellCountdown';
import { ShellClosingBanner, ShellJoinBanner } from './SessionEnded';
import { useShell } from './ShellProvider';
import { describeShellError } from './shellErrors';
import styles from './ShellTab.module.css';

const TerminalView = lazy(() => import('./TerminalView'));

/** Same folder, whatever the slashes or (on Windows) the letter case. */
// eslint-disable-next-line react-refresh/only-export-components
export function sameFolder(a: string, b: string): boolean {
  const norm = (p: string) => {
    const slashed = p.replace(/\\/g, '/').replace(/\/+$/, '');
    return /^[a-z]:/i.test(slashed) ? slashed.toLowerCase() : slashed;
  };
  return norm(a) === norm(b);
}

/** The newest open remote shell in `folder`, if any. */
// eslint-disable-next-line react-refresh/only-export-components
export function openShellIn(
  sessions: Record<string, SessionInfo>,
  folder: string,
): SessionInfo | undefined {
  let best: SessionInfo | undefined;
  for (const s of Object.values(sessions)) {
    if (!isOpenShell(s) || s.origin.type !== 'local') continue;
    if (!sameFolder(s.origin.project_dir, folder)) continue;
    if (!best || s.created_at > best.created_at) best = s;
  }
  return best;
}

/**
 * Whether a session gets a Shell tab: a local agent session (not itself a
 * shell), and remote shells are available to this device (the shell flows
 * are present and the device has admin permission).
 */
// eslint-disable-next-line react-refresh/only-export-components
export function useShellTabAvailable(session: SessionInfo): boolean {
  const shell = useShell();
  return (
    !!shell?.allowed &&
    session.origin.type === 'local' &&
    !isShellSession(session) &&
    !session.conductor
  );
}

/** The open shell of the session's project folder (reused by the Shell tab). */
// eslint-disable-next-line react-refresh/only-export-components
export function useProjectShell(session: SessionInfo): SessionInfo | undefined {
  const folder = session.origin.type === 'local' ? session.origin.project_dir : null;
  return useAppState((s) => (folder ? openShellIn(s.sessions, folder) : undefined));
}

/**
 * A session's Shell tab: a remote shell in the session's project folder,
 * right inside the session's screen. It reuses the shell already open for
 * that folder, or opens one through the usual flow (biometrics on phones,
 * the authenticator code, the server's step-up and time limits). The
 * output shows here; the input row below the tab types into the shell.
 */
export function ShellTab({ session, fitWidth }: { session: SessionInfo; fitWidth: boolean }) {
  const folder = session.origin.type === 'local' ? session.origin.project_dir : '';
  return <FolderShell folder={folder} fitWidth={fitWidth} />;
}

/**
 * A remote shell in `folder` (the session Shell tab, 지휘소's shell tab):
 * the folder's open shell, or a start screen that opens `shellName` there
 * through the usual flow.
 */
export function FolderShell({
  folder,
  shellName = 'default',
  fitWidth,
}: {
  folder: string;
  shellName?: string;
  fitWidth: boolean;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const shell = useShell();
  const project = useAppState((s) => (folder ? openShellIn(s.sessions, folder) : undefined));
  const view = useAppState((s) => (project ? s.views[project.id] : undefined));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmClose, setConfirmClose] = useState(false);

  // Stream the shell next to the agent session while the tab is open.
  const projectId = project?.id ?? null;
  useEffect(() => {
    actions.watch(projectId);
    return () => actions.watch(null);
  }, [actions, projectId]);

  const open = async () => {
    if (!shell || busy) return;
    setBusy(true);
    setError(null);
    try {
      await shell.openShell({ shell: shellName, project_dir: folder, display_name: null });
    } catch (err) {
      setError(describeShellError(t, err));
    } finally {
      setBusy(false);
    }
  };

  if (!project) {
    return (
      <div className={styles.start} data-testid="shell-tab-start">
        <Icon name="terminal" size={28} />
        <p className={styles.lead}>{t('shellTab.intro', { folder })}</p>
        <p className="muted">{t('shellTab.limits')}</p>
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy || !shell}
          onClick={() => void open()}
        >
          <Icon name="terminal" size={16} />
          {busy ? t('newSession.launching') : t('shellTab.open')}
        </button>
        {error && (
          <p role="alert" className={styles.error}>
            {error}
          </p>
        )}
      </div>
    );
  }

  return (
    <div className={styles.tab} data-testid="shell-tab">
      <div className={styles.bar}>
        <ShellBadge compact />
        <span className={styles.name} title={project.display_name}>
          {project.display_name}
        </span>
        <ShellCountdown session={project} compact />
        <button
          type="button"
          className="btn btn-sm"
          onClick={() => setConfirmClose(true)}
          aria-label={t('shell.close')}
          title={t('shell.close')}
        >
          <Icon name="close" size={14} />
          <span className={styles.btnLabel}>{t('shell.close')}</span>
        </button>
      </div>
      <ShellClosingBanner session={project} />
      <ShellJoinBanner session={project} />
      <div className={styles.screen}>
        <Suspense fallback={<p className="muted">…</p>}>
          <TerminalView
            sessionId={project.id}
            events={view?.events ?? []}
            fitWidth={fitWidth}
            label={t('shellTab.screen', { name: project.display_name })}
          />
        </Suspense>
      </div>
      {confirmClose && (
        <ConfirmDialog
          title={t('shell.close')}
          message={t('shell.confirmClose', { name: project.display_name })}
          confirmLabel={t('shell.close')}
          danger
          onCancel={() => setConfirmClose(false)}
          onConfirm={() => {
            setConfirmClose(false);
            actions.control(project.id, 'stop').catch(() => {});
          }}
        />
      )}
    </div>
  );
}
