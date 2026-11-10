import { useI18n } from '../i18n/context';
import { Icon } from './Icon';
import styles from './SubagentBadge.module.css';

/**
 * How many sub-agents the session's agent runs now: an agent icon and the
 * number, with a gentle pulse while they work. Nothing when there are none.
 */
export function SubagentBadge({ count, compact = false }: { count?: number; compact?: boolean }) {
  const { t } = useI18n();
  if (!count || count <= 0) return null;
  const label = t('card.subagents', { count });
  return (
    <span
      className={`${styles.badge} ${compact ? styles.compact : ''}`}
      role="img"
      aria-label={label}
      title={label}
      data-testid="subagent-badge"
    >
      <Icon name="agent" size={compact ? 12 : 14} className={styles.icon} />
      <span className={styles.count} aria-hidden="true">
        {count}
      </span>
    </span>
  );
}
