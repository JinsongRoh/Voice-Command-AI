import { useEffect, useState, type DragEvent } from 'react';

import { useI18n } from '../i18n/context';
import type { QueueEntry } from '../protocol/types';
import { useActions, useAppState } from '../state/store';
import { Gauge } from './Gauge';
import { Icon } from './Icon';
import styles from './ResourcesPanel.module.css';

/** Throughput that maps to a full gauge. */
const DISK_SCALE = 200 * 1024 * 1024;
const NET_SCALE = 25 * 1024 * 1024;

export function ResourcesPanel() {
  const { t, bytes, rate, relative } = useI18n();
  const actions = useActions();
  const snapshot = useAppState((s) => s.resources);
  const sessions = useAppState((s) => s.sessions);
  const [dragId, setDragId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const timer = setInterval(() => actions.refreshResources().catch(() => {}), 3000);
    return () => clearInterval(timer);
  }, [actions]);

  if (!snapshot) return <p className="muted">{t('resources.loading')}</p>;

  const run = (p: Promise<void>) => {
    setError(null);
    p.catch((err: Error) => setError(t('error.request', { error: err.message })));
  };
  const nameOf = (q: QueueEntry) =>
    (q.session_id && sessions[q.session_id]?.display_name) || q.session_id || '—';

  const mem = snapshot.memory_used_bytes / snapshot.memory_total_bytes;
  const disk = snapshot.disk_read_bytes_per_sec + snapshot.disk_write_bytes_per_sec;
  const net = snapshot.net_rx_bytes_per_sec + snapshot.net_tx_bytes_per_sec;
  const maxCpu = Math.max(100, ...snapshot.sessions.map((s) => s.cpu_percent));

  const onDrop = (e: DragEvent, target: QueueEntry) => {
    e.preventDefault();
    if (dragId && dragId !== target.job_id) run(actions.reorderQueue(dragId, target.position));
    setDragId(null);
  };

  return (
    <section className={styles.panel} aria-labelledby="resources-title">
      <h2 id="resources-title" className={styles.title}>
        {t('resources.title')}
      </h2>
      <div className={`panel ${styles.gauges}`}>
        <Gauge
          label={t('resources.cpu')}
          value={snapshot.cpu_percent / 100}
          valueText={`${Math.round(snapshot.cpu_percent)}%`}
        />
        <Gauge
          label={t('resources.memory')}
          value={mem}
          valueText={`${Math.round(mem * 100)}%`}
          detail={`${bytes(snapshot.memory_used_bytes)} / ${bytes(snapshot.memory_total_bytes)}`}
        />
        <Gauge
          label={t('resources.disk')}
          value={disk / DISK_SCALE}
          valueText={rate(disk)}
          detail={`${t('resources.read')} ${rate(snapshot.disk_read_bytes_per_sec)} · ${t('resources.write')} ${rate(snapshot.disk_write_bytes_per_sec)}`}
        />
        <Gauge
          label={t('resources.network')}
          value={net / NET_SCALE}
          valueText={rate(net)}
          detail={`${t('resources.rx')} ${rate(snapshot.net_rx_bytes_per_sec)} · ${t('resources.tx')} ${rate(snapshot.net_tx_bytes_per_sec)}`}
        />
      </div>

      <h3 className={styles.subtitle}>{t('resources.perSession')}</h3>
      {snapshot.sessions.length === 0 ? (
        <p className="muted">{t('resources.noSessions')}</p>
      ) : (
        <ul className={`panel ${styles.usage}`}>
          {snapshot.sessions.map((u) => (
            <li key={u.session_id} className={styles.usageRow}>
              <span className={styles.usageName}>
                {sessions[u.session_id]?.display_name ?? u.session_id}
              </span>
              <span className={styles.usageBar} aria-hidden="true">
                <span style={{ width: `${(u.cpu_percent / maxCpu) * 100}%` }} />
              </span>
              <span className={styles.usageNums}>
                {Math.round(u.cpu_percent)}% · {bytes(u.memory_bytes)} ·{' '}
                {t('resources.processes', { count: u.process_count })}
              </span>
            </li>
          ))}
        </ul>
      )}

      <h3 className={styles.subtitle}>{t('resources.queue')}</h3>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      {snapshot.queue.length === 0 ? (
        <p className="muted">{t('resources.queueEmpty')}</p>
      ) : (
        <>
          <p className={styles.hint}>{t('resources.dragHint')}</p>
          <ol className={`panel ${styles.queue}`}>
            {snapshot.queue.map((q, i) => (
              <li
                key={q.job_id}
                className={`${styles.job} ${dragId === q.job_id ? styles.dragging : ''}`}
                draggable
                onDragStart={(e) => {
                  setDragId(q.job_id);
                  e.dataTransfer.effectAllowed = 'move';
                  e.dataTransfer.setData('text/plain', q.job_id);
                }}
                onDragEnd={() => setDragId(null)}
                onDragOver={(e) => e.preventDefault()}
                onDrop={(e) => onDrop(e, q)}
                data-testid={`queue-${q.job_id}`}
              >
                <span className={styles.grip} aria-hidden="true">
                  <Icon name="grip" size={18} />
                </span>
                <span className={styles.pos}>{i + 1}</span>
                <span className={styles.jobMain}>
                  <code className={styles.command}>{q.command}</code>
                  <span className={styles.jobMeta}>
                    <span className={`${styles.cls} ${styles[q.class]}`}>
                      {t(q.class === 'heavy' ? 'resources.heavy' : 'resources.light')}
                    </span>
                    {t('resources.queuedFor', { name: nameOf(q), time: relative(q.queued_at) })}
                  </span>
                </span>
                <span className={styles.jobActions}>
                  <button
                    type="button"
                    className="icon-btn"
                    disabled={i === 0}
                    aria-label={t('resources.moveUp', { command: q.command })}
                    onClick={() => run(actions.reorderQueue(q.job_id, q.position - 1))}
                  >
                    <Icon name="up" size={18} />
                  </button>
                  <button
                    type="button"
                    className="icon-btn"
                    disabled={i === snapshot.queue.length - 1}
                    aria-label={t('resources.moveDown', { command: q.command })}
                    onClick={() => run(actions.reorderQueue(q.job_id, q.position + 1))}
                  >
                    <Icon name="down" size={18} />
                  </button>
                  <button
                    type="button"
                    className="icon-btn"
                    aria-label={t('resources.cancelJob', { command: q.command })}
                    onClick={() => run(actions.cancelQueued(q.job_id))}
                  >
                    <Icon name="close" size={18} />
                  </button>
                </span>
              </li>
            ))}
          </ol>
        </>
      )}
    </section>
  );
}
