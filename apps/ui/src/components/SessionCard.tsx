import { useId } from 'react';

import { useI18n } from '../i18n/context';
import {
  folderName,
  isCloud,
  needsAttention,
  originLabel,
  originTitle,
  providerLabel,
} from '../protocol/helpers';
import type { SessionInfo } from '../protocol/types';
import { AgentIcon } from './AgentIcon';
import { StateChip } from './StateChip';
import styles from './SessionCard.module.css';

export interface SessionCardProps {
  session: SessionInfo;
  preview: string;
  selected: boolean;
  /** 1-based card number (used for voice addressing, VOC-08). */
  index: number;
  onSelect: (id: string) => void;
}

export function SessionCard({ session, preview, selected, index, onSelect }: SessionCardProps) {
  const { t, relative } = useI18n();
  const cloud = isCloud(session.origin);
  const location =
    session.origin.type === 'local'
      ? folderName(session.origin.project_dir)
      : providerLabel(session.origin.provider);
  const attention = needsAttention(session.state);
  const nameId = useId();
  const detailsId = useId();
  const unreadId = useId();
  const stateId = useId();
  const previewId = useId();

  return (
    <li className={styles.item}>
      <button
        type="button"
        className={`${styles.card} ${selected ? styles.selected : ''} ${attention ? styles.attention : ''}`}
        aria-pressed={selected}
        aria-labelledby={nameId}
        aria-describedby={`${detailsId} ${unreadId} ${stateId} ${previewId}`}
        onClick={() => onSelect(session.id)}
        data-testid={`session-card-${session.id}`}
      >
        <div className={styles.top}>
          <AgentIcon origin={session.origin} />
          <div className={styles.titles}>
            <span className={styles.name} id={nameId}>
              <span className={styles.number} aria-hidden="true">
                {index}
              </span>
              {session.display_name}
            </span>
            <span className={styles.meta} id={detailsId}>
              <span className={styles.agent} title={originTitle(session.origin)}>
                {originLabel(session.origin)}
              </span>
              <span aria-hidden="true">·</span>
              <span
                className={styles.location}
                title={
                  session.origin.type === 'local'
                    ? session.origin.project_dir
                    : session.origin.remote_id
                }
              >
                {location}
              </span>
              {cloud && <span className={styles.cloud}>{t('card.cloud')}</span>}
              {session.profile && <span className={styles.profile}>{session.profile}</span>}
            </span>
          </div>
          {session.unread > 0 && (
            <span className="badge" id={unreadId}>
              <span aria-hidden="true">{session.unread}</span>
              <span className="visually-hidden">{t('card.unread', { count: session.unread })}</span>
            </span>
          )}
        </div>
        <p className={styles.preview} id={previewId}>
          {preview || t('card.noOutput')}
        </p>
        <div className={styles.bottom} id={stateId}>
          <StateChip state={session.state} />
          <time className={styles.time} dateTime={new Date(session.last_activity).toISOString()}>
            {t('card.lastActivity', { time: relative(session.last_activity) })}
          </time>
        </div>
      </button>
    </li>
  );
}
