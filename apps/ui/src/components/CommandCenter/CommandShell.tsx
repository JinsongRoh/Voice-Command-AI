import { useEffect, useMemo, useState } from 'react';

import { useMediaQuery } from '../../hooks/useMediaQuery';
import { useI18n } from '../../i18n/context';
import { useActions, useAppState, useMachines } from '../../state/store';
import { Composer } from '../Composer';
import { FolderPicker, type FolderSelection } from '../FolderPicker';
import { Icon } from '../Icon';
import { Modal } from '../Modal';
import { FolderShell, openShellIn } from '../ShellTab';
import { useShell } from '../ShellProvider';
import { TerminalInput } from '../TerminalInput';
import {
  commandShellName,
  conductorFolders,
  defaultShellFolder,
  isExcludedFolder,
} from './shellFolders';
import styles from './CommandCenter.module.css';

const PHONE_QUERY = '(max-width: 899px)';

/**
 * The shell tab of the Command Center's conversation (next to Chat and Terminal): a
 * remote shell (PowerShell on Windows) in a project folder, through the
 * same flow as a session's Shell tab (biometrics on phones, the
 * authenticator code, the server's step-up and time limits).
 * It starts in the most recent project folder, with a control to change
 * it, and never in the conductor's own folder. An open shell of the folder
 * is reused; the input row below types into it.
 */
export function CommandShell({
  conductorSessionId,
  folder: chosen,
  onFolder,
}: {
  conductorSessionId: string | null | undefined;
  /** The folder picked here (kept by the Command Center across tabs and views); `null`: the default. */
  folder: string | null;
  onFolder: (folder: string) => void;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const shell = useShell();
  const { machine } = useMachines();
  const phone = useMediaQuery(PHONE_QUERY);
  const sessions = useAppState((s) => s.sessions);
  const excluded = useMemo(
    () => conductorFolders(sessions, conductorSessionId),
    [sessions, conductorSessionId],
  );
  /** The server's recent project folders (`null` while asking). */
  const [recent, setRecent] = useState<string[] | null>(null);
  const [picking, setPicking] = useState(false);

  useEffect(() => {
    let live = true;
    actions.projectFolders().then(
      (folders) => live && setRecent(folders.recent.map((f) => f.path)),
      // Older servers or no folder list: the sessions' folders still tell.
      () => live && setRecent([]),
    );
    return () => {
      live = false;
    };
  }, [actions]);

  const fallback = recent === null ? null : defaultShellFolder(recent, sessions, excluded);
  const folder = chosen && !isExcludedFolder(chosen, excluded) ? chosen : fallback;
  const shellName = commandShellName(shell?.os ?? null);
  const open = useAppState((s) => (folder ? openShellIn(s.sessions, folder) : undefined));

  /** Phones: the input row, or the full message box a tap away. */
  const [composerOpen, setComposerOpen] = useState(false);

  return (
    <div className={styles.shellView} data-testid="cc-shell">
      <div className={styles.shellFolder}>
        <Icon name="terminal" size={16} />
        <span className={styles.shellFolderLabel}>{t('cc.shell.folder')}</span>
        <span
          className={styles.shellFolderPath}
          title={folder ?? undefined}
          data-testid="cc-shell-folder"
        >
          {folder ?? (recent === null ? t('folderPicker.loading') : t('cc.shell.noFolder'))}
        </span>
        <button
          type="button"
          className="btn btn-sm"
          disabled={!shell}
          onClick={() => setPicking(true)}
        >
          <Icon name="edit" size={14} />
          {t('cc.shell.changeFolder')}
        </button>
      </div>
      <div className={styles.shellPane}>
        {folder ? (
          <FolderShell key={folder} folder={folder} shellName={shellName} fitWidth={phone} />
        ) : (
          recent !== null && (
            <p className={`${styles.help} ${styles.shellEmpty}`}>{t('cc.shell.pickFirst')}</p>
          )
        )}
      </div>
      {open &&
        (phone && !composerOpen ? (
          <TerminalInput
            key={open.id}
            session={open}
            onOpenComposer={() => setComposerOpen(true)}
          />
        ) : (
          <>
            {phone && (
              <button type="button" className="btn btn-sm" onClick={() => setComposerOpen(false)}>
                <Icon name="terminal" size={14} />
                {t('terminal.backToInput')}
              </button>
            )}
            <Composer key={open.id} session={open} layout={phone ? 'phone' : undefined} />
          </>
        ))}
      {picking && (
        <FolderDialog
          machineId={machine.id}
          excluded={excluded}
          onClose={() => setPicking(false)}
          onPick={(path) => {
            setPicking(false);
            onFolder(path);
          }}
        />
      )}
    </div>
  );
}

/** "Change folder": the folder picker (existing folders only). */
function FolderDialog({
  machineId,
  excluded,
  onClose,
  onPick,
}: {
  machineId: string;
  excluded: readonly string[];
  onClose: () => void;
  onPick: (path: string) => void;
}) {
  const { t } = useI18n();
  const [picked, setPicked] = useState<FolderSelection | null>(null);
  const [unavailable, setUnavailable] = useState<string | null | undefined>(undefined);
  const blocked = !!picked && isExcludedFolder(picked.path, excluded);
  return (
    <Modal
      title={t('cc.shell.changeFolder')}
      onClose={onClose}
      fullScreen
      wide
      footer={
        <>
          <button type="button" className="btn" onClick={onClose}>
            {t('common.cancel')}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={!picked || blocked}
            onClick={() => picked && onPick(picked.path)}
          >
            {t('cc.shell.useFolder')}
          </button>
        </>
      }
    >
      {unavailable !== undefined ? (
        <p className="muted">
          {unavailable === null
            ? t('cc.shell.pickerUnsupported')
            : t('cc.shell.pickerError', { error: unavailable })}
        </p>
      ) : (
        <FolderPicker
          machineId={machineId}
          allowCreate={false}
          onChange={setPicked}
          onUnavailable={setUnavailable}
        />
      )}
      {blocked && (
        <p role="alert" className={styles.error}>
          {t('cc.shell.conductorFolder')}
        </p>
      )}
    </Modal>
  );
}
