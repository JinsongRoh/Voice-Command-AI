import { useEffect, useRef } from 'react';

import { useI18n } from '../i18n/context';
import type { SessionEvent } from '../protocol/types';
import { Icon } from './Icon';
import { stateKey } from './stateKey';
import styles from './ChatView.module.css';

/** Structured view of a session: messages, tool calls and decisions (MON-02). */
export function ChatView({ events, gapFrom }: { events: SessionEvent[]; gapFrom: number | null }) {
  const { t } = useI18n();
  const ref = useRef<HTMLOListElement>(null);
  const stick = useRef(true);
  const items = events.filter((e) => e.type !== 'output');

  useEffect(() => {
    const el = ref.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [items.length]);

  return (
    <ol
      ref={ref}
      className={styles.chat}
      role="log"
      aria-label={t('live.log')}
      onScroll={(e) => {
        const el = e.currentTarget;
        stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
      }}
    >
      {gapFrom !== null && (
        <li className={styles.gap} role="note">
          {t('live.gap', { seq: gapFrom })}
        </li>
      )}
      {items.length === 0 && <li className="muted">{t('live.empty')}</li>}
      {items.map((e) => {
        switch (e.type) {
          case 'message':
            return (
              <li key={e.seq} className={`${styles.msg} ${styles[e.role]}`}>
                <span className={styles.role}>{t(`role.${e.role}`)}</span>
                <p className={styles.text}>{e.text}</p>
              </li>
            );
          case 'tool_call':
            return (
              <li key={e.seq} className={styles.tool}>
                <span className={styles.toolName}>{e.name}</span>
                <span className={styles.toolSummary}>{e.summary}</span>
              </li>
            );
          case 'state_changed':
            return (
              <li key={e.seq} className={styles.state}>
                {t(stateKey(e.state))}
              </li>
            );
          case 'decision_requested':
            return (
              <li key={e.seq} className={styles.decision}>
                <span className={styles.role}>{t('decision.title')}</span>
                <p className={styles.text}>{e.decision.prompt}</p>
                <ul className={styles.options}>
                  {e.decision.options.map((o) => (
                    <li key={o.id}>{o.label}</li>
                  ))}
                </ul>
              </li>
            );
          case 'notification':
            return (
              <li key={e.seq} className={styles.notification}>
                <Icon name="bell" size={16} />
                <span>
                  <strong>{e.title}</strong> {e.body}
                </span>
              </li>
            );
          default:
            return null;
        }
      })}
    </ol>
  );
}
