/**
 * What an ended session offers, wherever it shows (session view, card menu,
 * phone carousel): Reopen and Remove from list. Also the warning before a
 * remote shell closes for being idle, with "Keep open".
 */
import { useState } from 'react';

import { useNow } from '../hooks/useNow';
import { useI18n } from '../i18n/context';
import { isCloud, isShellSession, sessionCapabilities } from '../protocol/helpers';
import { formatCountdown, secondsLeft } from '../protocol/shell';
import type { SessionInfo } from '../protocol/types';
import { describeRequestError } from '../state/requestErrors';
import { useActions, useAppState } from '../state/store';
import { Icon } from './Icon';
import { useShell } from './ShellProvider';
import { describeShellError } from './shellErrors';
import styles from './SessionEnded.module.css';

/**
 * How an ended session comes back: a new remote shell in the same folder
 * (same shell, through the usual code and step-up), the restart action of a
 * session VCA started, or not at all (a session that ran in a window on the
 * PC, a cloud session).
 */
export type ReopenKind = 'shell' | 'restart' | 'observed' | 'none';

// eslint-disable-next-line react-refresh/only-export-components
export function reopenKind(session: SessionInfo, shellAllowed: boolean): ReopenKind {
  if (isShellSession(session)) return shellAllowed ? 'shell' : 'none';
  if (isCloud(session.origin)) return 'none';
  if (sessionCapabilities(session).restart) return 'restart';
  return 'observed';
}

export interface EndedCommands {
  kindOf(session: SessionInfo): ReopenKind;
  /** Reopens; resolves with an error text, or `null` (done or cancelled). */
  reopen(session: SessionInfo): Promise<string | null>;
  /** Removes from the list; resolves with an error text, or `null`. */
  remove(session: SessionInfo): Promise<string | null>;
}

/** Reopen and Remove from list, for any ended session (cards, carousel, view). */
// eslint-disable-next-line react-refresh/only-export-components
export function useEndedCommands(onReopened?: (session: SessionInfo) => void): EndedCommands {
  const { t } = useI18n();
  const actions = useActions();
  const shell = useShell();
  const kindOf = (session: SessionInfo) => reopenKind(session, !!shell?.allowed);
  return {
    kindOf,
    async reopen(session) {
      const kind = kindOf(session);
      try {
        if (kind === 'shell' && shell && session.origin.type === 'local') {
          const name =
            session.origin.agent.type === 'shell' ? session.origin.agent.name : 'default';
          // Biometrics (phones), the authenticator code and the server's checks.
          const opened = await shell.openShell({
            shell: name || 'default',
            project_dir: session.origin.project_dir,
            display_name: session.custom_name ? session.display_name : null,
          });
          if (opened) {
            if (onReopened) onReopened(opened);
            else actions.select(opened.id);
          }
        } else if (kind === 'restart') {
          await actions.control(session.id, 'restart');
        }
        return null;
      } catch (err) {
        return kind === 'shell'
          ? describeShellError(t, err)
          : describeRequestError(t, err as Error).message;
      }
    },
    async remove(session) {
      try {
        await actions.dismissSession(session.id);
        return null;
      } catch (err) {
        return describeRequestError(t, err as Error).message;
      }
    },
  };
}

export interface EndedActions {
  kind: ReopenKind;
  busy: boolean;
  error: string | null;
  reopen(): Promise<void>;
  remove(): Promise<void>;
}

/** Reopen and Remove from list for one ended session, with busy and error state. */
// eslint-disable-next-line react-refresh/only-export-components
export function useEndedActions(session: SessionInfo): EndedActions {
  const commands = useEndedCommands();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (what: (s: SessionInfo) => Promise<string | null>) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    const problem = await what(session);
    setBusy(false);
    setError(problem);
  };
  return {
    kind: commands.kindOf(session),
    busy,
    error,
    reopen: () => run(commands.reopen),
    remove: () => run(commands.remove),
  };
}

/**
 * The bar of an ended session in its view: what happened, Reopen (when the
 * session can come back) and Remove from list.
 */
export function SessionEndedBar({ session }: { session: SessionInfo }) {
  const { t } = useI18n();
  const ended = useEndedActions(session);
  return (
    <div className={styles.bar} role="region" aria-label={t('ended.title')} data-testid="ended-bar">
      <p className={styles.text}>
        <Icon name="info" size={14} />
        <span>
          {ended.kind === 'observed'
            ? t('ended.observed')
            : isShellSession(session)
              ? t('ended.shell')
              : t('ended.body')}
        </span>
      </p>
      <div className={styles.actions}>
        {(ended.kind === 'shell' || ended.kind === 'restart') && (
          <button
            type="button"
            className="btn btn-sm btn-primary"
            disabled={ended.busy}
            onClick={() => void ended.reopen()}
          >
            <Icon name="restart" size={14} />
            {t('ended.reopen')}
          </button>
        )}
        <button
          type="button"
          className="btn btn-sm"
          disabled={ended.busy}
          onClick={() => void ended.remove()}
        >
          <Icon name="close" size={14} />
          {t('ended.remove')}
        </button>
      </div>
      {ended.error && (
        <p role="alert" className={styles.error}>
          {ended.error}
        </p>
      )}
    </div>
  );
}

/**
 * Announces the closing warning once, when it appears. The ticking countdown
 * in the visible text is deliberately not a live region: an assertive alert
 * re-read every second would make Keep open impossible to reach.
 */
function ClosingAnnouncement({ message }: { message: string }) {
  const [first] = useState(message);
  return (
    <span className="visually-hidden" role="alert">
      {first}
    </span>
  );
}

/**
 * "This shell closes in 0:45 for being idle" with Keep open, after the
 * server's `shell_closing_soon` notice.
 */
export function ShellClosingBanner({ session }: { session: SessionInfo }) {
  const { t } = useI18n();
  const actions = useActions();
  const deadline = useAppState((s) => s.closingSoon[session.id]);
  const final = useAppState((s) => s.closingFinal.includes(session.id));
  const now = useNow();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  if (deadline === undefined || session.state.type === 'exited') return null;
  const left = secondsLeft(deadline, now);
  const keep = async () => {
    setBusy(true);
    setError(null);
    try {
      await actions.keepShellOpen(session.id);
    } catch (err) {
      setError(describeShellError(t, err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div
      className={styles.closing}
      role="group"
      aria-label={session.display_name}
      data-testid="shell-closing"
    >
      <Icon name="clock" size={16} />
      <ClosingAnnouncement
        key={final ? 'final' : 'soon'}
        message={t(final ? 'ended.closingFinal' : 'ended.closingSoon', {
          name: session.display_name,
          time: formatCountdown(left),
        })}
      />
      <span className={styles.text}>
        {t(final ? 'ended.closingFinal' : 'ended.closingSoon', {
          name: session.display_name,
          time: formatCountdown(left),
        })}
      </span>
      {!final && (
        <button
          type="button"
          className="btn btn-sm btn-primary"
          disabled={busy}
          onClick={() => void keep()}
        >
          {t('ended.keepOpen')}
        </button>
      )}
      {error && <span className={styles.error}>{error}</span>}
    </div>
  );
}

/**
 * A remote shell another device opened, which this device has not joined:
 * the server streams nothing and takes no input until an authenticator code
 * is entered once for it here.
 */
export function ShellJoinBanner({ session }: { session: SessionInfo }) {
  const { t } = useI18n();
  const shell = useShell();
  const needed = useAppState((s) => s.shellJoinNeeded.includes(session.id));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!needed || !shell || session.state.type === 'exited') return null;
  const join = async () => {
    setBusy(true);
    setError(null);
    try {
      await shell.joinShell(session.id);
    } catch (err) {
      setError(describeShellError(t, err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div
      className={styles.closing}
      role="group"
      aria-label={session.display_name}
      data-testid="shell-join"
    >
      <Icon name="shield" size={16} />
      <span className={styles.text}>{t('shell.join.banner')}</span>
      <button
        type="button"
        className="btn btn-sm btn-primary"
        disabled={busy}
        onClick={() => void join()}
      >
        {t('shell.join.button')}
      </button>
      {error && <span className={styles.error}>{error}</span>}
    </div>
  );
}
