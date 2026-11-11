import { useCallback, useEffect, useRef, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { agentLabel, folderName } from '../protocol/helpers';
import type { FoundAgent, FoundStatus } from '../protocol/types';
import { describeRequestError } from '../state/requestErrors';
import { useActions, useAppState } from '../state/store';
import { AgentIcon } from './AgentIcon';
import { Icon } from './Icon';
import { Modal } from './Modal';
import styles from './ImportSessionsSheet.module.css';

const STATUS_KEYS: Record<FoundStatus, MessageKey> = {
  imported: 'scan.status.imported',
  hidden_by_user: 'scan.status.hidden_by_user',
  no_transcript_yet: 'scan.status.no_transcript_yet',
  unsupported_agent: 'scan.status.unsupported_agent',
  not_in_a_terminal: 'scan.status.not_in_a_terminal',
};

const REASON_KEYS: Partial<Record<FoundStatus, MessageKey>> = {
  no_transcript_yet: 'scan.reason.no_transcript_yet',
  unsupported_agent: 'scan.reason.unsupported_agent',
  not_in_a_terminal: 'scan.reason.not_in_a_terminal',
};

/** Rows whose listing the person can switch on and off. */
function canToggle(status: FoundStatus): boolean {
  return status === 'imported' || status === 'no_transcript_yet' || status === 'hidden_by_user';
}

/** True when the row is on the session list. */
function isListed(status: FoundStatus): boolean {
  return status === 'imported' || status === 'no_transcript_yet';
}

/**
 * "Bring in AI sessions": what discovery found on the selected computer, why
 * something is not listed, and a switch per session to show or hide it.
 * Opening the sheet runs a scan. Plain terminal windows never appear.
 */
export function ImportSessionsSheet({ onClose }: { onClose: () => void }) {
  const { t } = useI18n();
  const actions = useActions();
  const serverName = useAppState((s) => s.serverName);
  const [found, setFound] = useState<FoundAgent[] | null>(null);
  const [scanning, setScanning] = useState(true);
  const [busyPid, setBusyPid] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const run = useCallback(
    async (work: () => Promise<{ found: FoundAgent[] }>) => {
      setError(null);
      try {
        const report = await work();
        if (alive.current) setFound(report.found);
      } catch (err) {
        if (alive.current) setError(describeRequestError(t, err).message);
      }
    },
    [t],
  );

  const scan = useCallback(async () => {
    setScanning(true);
    await run(() => actions.scanSessions());
    if (alive.current) setScanning(false);
  }, [actions, run]);

  // The first scan when the sheet opens (`scanning` starts true).
  useEffect(() => {
    actions
      .scanSessions()
      .then(
        (report) => {
          if (alive.current) setFound(report.found);
        },
        (err: unknown) => {
          if (alive.current) setError(describeRequestError(t, err).message);
        },
      )
      .finally(() => {
        if (alive.current) setScanning(false);
      });
  }, [actions, t]);

  const toggle = async (row: FoundAgent, show: boolean) => {
    setBusyPid(row.pid);
    await run(() =>
      show ? actions.unhideObserved({ pid: row.pid }) : actions.hideObserved({ pid: row.pid }),
    );
    if (alive.current) setBusyPid(null);
  };

  const importAll = async () => {
    setScanning(true);
    await run(() => actions.unhideObserved({ all: true }));
    if (alive.current) setScanning(false);
  };

  const hidden = found?.filter((f) => f.status === 'hidden_by_user').length ?? 0;
  const listed = found?.filter((f) => isListed(f.status)).length ?? 0;

  return (
    <Modal
      title={t('scan.title')}
      onClose={onClose}
      fullScreen
      footer={
        <>
          <button
            type="button"
            className="btn"
            onClick={() => void scan()}
            disabled={scanning}
            data-testid="scan-again"
          >
            <Icon name="restart" size={16} />
            {scanning ? t('scan.scanning') : t('scan.again')}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => void importAll()}
            disabled={scanning || found === null}
          >
            {t('scan.importAll')}
          </button>
        </>
      }
    >
      <p className={`muted ${styles.lead}`}>
        {serverName ? t('scan.leadOn', { name: serverName }) : t('scan.lead')}
      </p>
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
      <div aria-live="polite" className="visually-hidden">
        {found && !scanning ? t('scan.summary', { count: found.length, listed, hidden }) : ''}
      </div>
      {found === null ? (
        scanning && <p className="muted">{t('scan.scanning')}</p>
      ) : found.length === 0 ? (
        <p className="muted">{t('scan.empty')}</p>
      ) : (
        <ul className={styles.list} aria-busy={scanning || undefined}>
          {found.map((row) => (
            <FoundRow
              key={row.pid}
              row={row}
              busy={busyPid === row.pid}
              onToggle={(show) => void toggle(row, show)}
            />
          ))}
        </ul>
      )}
      <div className={styles.notes}>
        <p className="muted">{t('scan.noteShells')}</p>
        <p className="muted">{t('scan.noteChatgpt')}</p>
      </div>
    </Modal>
  );
}

function FoundRow({
  row,
  busy,
  onToggle,
}: {
  row: FoundAgent;
  busy: boolean;
  onToggle: (show: boolean) => void;
}) {
  const { t } = useI18n();
  const label = agentLabel(row.agent);
  const folder = row.project_dir ? folderName(row.project_dir) : '';
  const name = row.title || folder || label;
  const reason = REASON_KEYS[row.status];
  const where = [row.terminal, folder && folder !== name ? folder : null]
    .filter(Boolean)
    .join(' · ');
  return (
    <li className={styles.row} data-testid={`found-${row.pid}`} data-status={row.status}>
      <AgentIcon
        origin={{
          type: 'local',
          agent: row.agent,
          project_dir: row.project_dir ?? '',
          pid: row.pid,
        }}
        size={32}
      />
      <div className={styles.body}>
        <div className={styles.top}>
          <span className={styles.name} title={row.project_dir ?? undefined}>
            {name}
          </span>
          <span className={styles.badge} data-status={row.status}>
            {t(STATUS_KEYS[row.status])}
          </span>
        </div>
        <div className={`muted ${styles.meta}`}>
          {label}
          {where && ` · ${where}`}
        </div>
        {reason && <div className={`muted ${styles.reason}`}>{t(reason)}</div>}
      </div>
      {canToggle(row.status) && (
        <label className={`check ${styles.toggle}`}>
          <input
            type="checkbox"
            role="switch"
            checked={isListed(row.status)}
            disabled={busy}
            aria-label={t('scan.showLabel', { name })}
            onChange={(e) => onToggle(e.target.checked)}
          />
          <span aria-hidden="true">{t('scan.show')}</span>
        </label>
      )}
    </li>
  );
}
