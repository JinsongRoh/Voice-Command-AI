import { useCallback, useEffect, useState } from 'react';

import { useI18n } from '../../i18n/context';
import type { AuditEntry, ConductorState } from '../../protocol/types';
import { useActions, useAppState } from '../../state/store';
import { Icon } from '../Icon';
import { auditSentence, isCommandEntry } from './auditSentences';
import styles from './CommandCenter.module.css';

/** Audit entries fetched; the Log shows the conductor's among them. */
const FETCH = 200;

/**
 * The conductor's log: what it did, was refused and was told, newest
 * first, as sentences. Needs the audit log's access on this device.
 */
export function LogTab({ state }: { state: ConductorState }) {
  const { t, relative } = useI18n();
  const actions = useActions();
  const sessions = useAppState((s) => s.sessions);
  const [entries, setEntries] = useState<AuditEntry[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [loading, setLoading] = useState(false);
  const changes = state.actions_last_hour + state.pending.length + state.wakes_last_hour;

  /** Fetches the entries; every state change happens when the answer comes. */
  const load = useCallback(() => {
    let live = true;
    actions.listAudit(FETCH).then(
      (list) => {
        if (!live) return;
        setEntries(list.filter(isCommandEntry));
        setFailed(false);
        setLoading(false);
      },
      () => {
        if (!live) return;
        setFailed(true);
        setLoading(false);
      },
    );
    return () => {
      live = false;
    };
  }, [actions]);

  // Refreshed when the conductor acts, is asked or changes state.
  useEffect(() => load(), [load, changes, state.status]);

  const nameOf = (id: string) => sessions[id]?.display_name ?? null;

  return (
    <section className={styles.log} aria-labelledby="cc-log-title">
      <div className={styles.logHead}>
        <h3 id="cc-log-title" className={styles.subhead}>
          {t('cc.log.title')}
        </h3>
        <button
          type="button"
          className="btn btn-sm"
          disabled={loading}
          onClick={() => {
            setLoading(true);
            load();
          }}
        >
          <Icon name="restart" size={14} /> {t('audit.refresh')}
        </button>
      </div>
      {failed ? (
        <p className="muted">{t('cc.log.unavailable')}</p>
      ) : entries === null ? (
        <p className="muted">{t('audit.loading')}</p>
      ) : entries.length === 0 ? (
        <p className="muted">{t('conductor.actionsEmpty')}</p>
      ) : (
        <ol className={styles.logList} data-testid="cc-log">
          {entries.map((e, i) => (
            <li key={`${e.ts}-${i}`} className={styles.logItem} data-event={e.event}>
              <span className={styles.logText}>{auditSentence(e, t, nameOf)}</span>
              <span className={styles.logMeta}>
                <time dateTime={new Date(e.ts).toISOString()}>{relative(e.ts)}</time>
                {e.device_name && ` · ${e.device_name}`}
              </span>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}
