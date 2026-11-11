/**
 * "Remove from VCA": the one way to take a session off VCA, offered in the
 * card menu, the session view and each row of the session order. What it
 * does depends on the session:
 *
 * - ended: removed at once, as before;
 * - running, started by VCA: one confirmation (the agent process stops; the
 *   conversation history and the project folder stay), then stopped and
 *   removed in one go;
 * - running in a terminal window on the PC, or at a cloud provider: VCA
 *   cannot stop it, so a dialog says plainly what to do instead.
 */
import { useState, type ReactNode } from 'react';

import { useI18n } from '../i18n/context';
import { isCloud, isShellSession, providerLabel } from '../protocol/helpers';
import type { SessionInfo } from '../protocol/types';
import { describeRequestError } from '../state/requestErrors';
import { useActions, useAppState } from '../state/store';
import { Modal } from './Modal';
import { useDeviceScope } from './ShellProvider';

/** What "Remove from VCA" does for a session (see the module comment). */
export type RemoveKind = 'ended' | 'stop' | 'pc_window' | 'cloud' | 'conductor';

export function removeKind(session: SessionInfo): RemoveKind {
  if (session.state.type === 'exited') return 'ended';
  if (session.conductor) return 'conductor';
  if (isCloud(session.origin) || session.control === 'cloud') return 'cloud';
  if (session.control === 'managed_pty') return 'stop';
  return 'pc_window';
}

export interface RemoveFromVca {
  /**
   * Whether this device may offer the action for `session`: not on view-only
   * devices, and not for an open remote shell (it has its own Close).
   */
  offered(session: SessionInfo): boolean;
  /** Starts the action: removes, asks first, or explains (see `removeKind`). */
  ask(session: SessionInfo): void;
  /** The dialog to render (`null` while none is open). */
  dialog: ReactNode;
}

/**
 * "Remove from VCA" for any session. `onError` reports a failed removal of an
 * ended session (which has no dialog); a running one shows its error in the
 * dialog.
 */
export function useRemoveFromVca(
  onError: (session: SessionInfo, message: string) => void,
): RemoveFromVca {
  const { t } = useI18n();
  const actions = useActions();
  const scope = useDeviceScope();
  const connected = useAppState((s) => s.connection === 'connected');
  const sessions = useAppState((s) => s.sessions);
  const [askedId, setAskedId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The latest state of the session asked about; it may end meanwhile.
  const asked = askedId ? sessions[askedId] : undefined;

  const close = () => {
    setAskedId(null);
    setBusy(false);
    setError(null);
  };

  const offered = (session: SessionInfo) =>
    connected && scope !== 'view' && !(isShellSession(session) && session.state.type !== 'exited');

  const ask = (session: SessionInfo) => {
    if (removeKind(session) === 'ended') {
      actions
        .removeFromVca(session.id)
        .catch((err: Error) => onError(session, describeRequestError(t, err).message));
      return;
    }
    setError(null);
    setBusy(false);
    setAskedId(session.id);
  };

  const confirm = async (session: SessionInfo) => {
    setBusy(true);
    setError(null);
    try {
      await actions.removeFromVca(session.id);
      close();
    } catch (err) {
      setBusy(false);
      setError(describeRequestError(t, err as Error).message);
    }
  };

  // No dialog once the session is gone (removed here or elsewhere).
  let dialog: ReactNode = null;
  if (asked) {
    const kind = removeKind(asked);
    const name = asked.display_name;
    if (kind === 'stop' || kind === 'ended') {
      dialog = (
        <Modal
          title={t('remove.title', { name })}
          onClose={busy ? () => {} : close}
          footer={
            <>
              <button type="button" className="btn" onClick={close} disabled={busy}>
                {t('common.cancel')}
              </button>
              <button
                type="button"
                className="btn btn-danger"
                disabled={busy}
                onClick={() => void confirm(asked)}
                data-testid="remove-confirm"
              >
                {kind === 'ended' ? t('remove.action') : t('remove.stopConfirm')}
              </button>
            </>
          }
        >
          <p>{kind === 'ended' ? t('remove.endedBody') : t('remove.stopBody')}</p>
          {busy && (
            <p role="status" className="muted">
              {t('remove.busy')}
            </p>
          )}
          {error && <p role="alert">{error}</p>}
        </Modal>
      );
    } else {
      const body =
        kind === 'cloud'
          ? t('remove.cloudBody', {
              name,
              provider:
                asked.origin.type === 'cloud' ? providerLabel(asked.origin.provider) : 'cloud',
            })
          : kind === 'conductor'
            ? t('remove.conductorBody')
            : t('remove.pcWindowBody', { name });
      dialog = (
        <Modal
          title={kind === 'pc_window' ? t('remove.pcWindowTitle') : t('remove.stopFirstTitle')}
          onClose={close}
          footer={
            <button type="button" className="btn btn-primary" onClick={close}>
              {t('remove.ok')}
            </button>
          }
        >
          <p>{body}</p>
        </Modal>
      );
    }
  }

  return { offered, ask, dialog };
}
