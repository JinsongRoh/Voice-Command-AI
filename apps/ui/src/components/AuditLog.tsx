import { useCallback, useEffect, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import type { AuditEntry } from '../protocol/types';
import { useActions, useAppState } from '../state/store';
import { Icon } from './Icon';
import { describeShellError } from './shellErrors';
import styles from './AuditLog.module.css';

const EVENTS: Record<string, MessageKey> = {
  shell_open: 'audit.event.shell_open',
  shell_close: 'audit.event.shell_close',
  shell_run: 'audit.event.shell_run',
  step_up_ok: 'audit.event.step_up_ok',
  step_up_failed: 'audit.event.step_up_failed',
  conductor_send: 'audit.event.conductor_send',
  conductor_interrupt: 'audit.event.conductor_interrupt',
  conductor_focus: 'audit.event.conductor_focus',
  conductor_restore: 'audit.event.conductor_restore',
  conductor_new_session: 'audit.event.conductor_new_session',
  conductor_refused: 'audit.event.conductor_refused',
  conductor_wake: 'audit.event.conductor_wake',
  conductor_start: 'audit.event.conductor_start',
  conductor_stop: 'audit.event.conductor_stop',
  conductor_kill: 'audit.event.conductor_kill',
  conductor_settings: 'audit.event.conductor_settings',
  conductor_decide: 'audit.event.conductor_decide',
  conductor_allow: 'audit.event.conductor_allow',
  conductor_no_reply: 'audit.event.conductor_no_reply',
  cc_pause: 'audit.event.cc_pause',
  task_create: 'audit.event.task_create',
  task_update: 'audit.event.task_update',
  task_send_result: 'audit.event.task_send_result',
  task_admin_shell: 'audit.event.task_admin_shell',
  task_configure: 'audit.event.task_configure',
};

/** The audit view filter: everything, or the conductor's entries only. */
type AuditFilter = 'all' | 'conductor';
const FILTERS: AuditFilter[] = ['all', 'conductor'];

/** Events that deserve attention in the list. */
const ALERT_EVENTS = new Set(['step_up_failed']);

/** Settings → Activity log (admin): remote shell audit entries, newest first. */
export function AuditLog() {
  const { t, locale, relative } = useI18n();
  const actions = useActions();
  const sessions = useAppState((s) => s.sessions);
  const connected = useAppState((s) => s.connection === 'connected');
  const [entries, setEntries] = useState<AuditEntry[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<AuditFilter>('all');
  const shown =
    entries && filter === 'conductor'
      ? entries.filter((e) => e.event.startsWith('conductor_') || e.event.startsWith('cc_'))
      : entries;

  const refresh = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setEntries(await actions.listAudit());
    } catch (err) {
      setError(describeShellError(t, err));
    } finally {
      setBusy(false);
    }
  }, [actions, t]);

  // First load once connected (results arrive asynchronously).
  useEffect(() => {
    if (!connected) return;
    let live = true;
    actions.listAudit().then(
      (list) => live && setEntries(list),
      (err: unknown) => live && setError(describeShellError(t, err)),
    );
    return () => {
      live = false;
    };
  }, [connected, actions, t]);

  const time = new Intl.DateTimeFormat(locale, { dateStyle: 'short', timeStyle: 'medium' });

  return (
    <section className={`panel ${styles.section}`} aria-labelledby="set-audit">
      <div className={styles.head}>
        <h3 id="set-audit">{t('audit.title')}</h3>
        <button
          type="button"
          className="btn btn-sm"
          onClick={() => void refresh()}
          disabled={busy || !connected}
        >
          <Icon name="restart" size={14} />
          {busy ? t('audit.loading') : t('audit.refresh')}
        </button>
      </div>
      <p className={styles.help}>{t('audit.help')}</p>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      <div className="segmented" role="group" aria-label={t('audit.filter')}>
        {FILTERS.map((f) => (
          <button key={f} type="button" aria-pressed={filter === f} onClick={() => setFilter(f)}>
            {t(`audit.filter.${f}`)}
          </button>
        ))}
      </div>
      {shown && shown.length === 0 && <p className="muted">{t('audit.empty')}</p>}
      {shown && shown.length > 0 && (
        <div className={styles.scroll}>
          <table className={styles.table}>
            <thead>
              <tr>
                <th scope="col">{t('audit.time')}</th>
                <th scope="col">{t('audit.device')}</th>
                <th scope="col">{t('audit.event')}</th>
                <th scope="col">{t('audit.session')}</th>
                <th scope="col">{t('audit.detail')}</th>
              </tr>
            </thead>
            <tbody>
              {shown.map((e, i) => {
                const key = EVENTS[e.event];
                const session = e.session_id ? sessions[e.session_id] : undefined;
                return (
                  <tr key={`${e.ts}-${i}`} data-alert={ALERT_EVENTS.has(e.event) || undefined}>
                    <td data-label={t('audit.time')}>
                      <time dateTime={new Date(e.ts).toISOString()} title={time.format(e.ts)}>
                        {relative(e.ts)}
                      </time>
                    </td>
                    <td data-label={t('audit.device')}>{e.device_name || e.device_id}</td>
                    <td data-label={t('audit.event')}>
                      <span className={styles.event} data-event={e.event}>
                        {key ? t(key) : e.event}
                      </span>
                    </td>
                    <td data-label={t('audit.session')}>
                      {session?.display_name ?? e.session_id ?? '—'}
                    </td>
                    <td data-label={t('audit.detail')} className={styles.detail}>
                      {e.detail || '—'}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
