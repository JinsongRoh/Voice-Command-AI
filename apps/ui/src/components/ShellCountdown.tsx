import { useNow } from '../hooks/useNow';
import { useI18n } from '../i18n/context';
import { formatCountdown, nextDeadline, secondsLeft } from '../protocol/shell';
import type { SessionInfo } from '../protocol/types';
import { Icon } from './Icon';
import styles from './Shell.module.css';

/** Below this many seconds the countdown turns into a warning. */
const WARN_SECONDS = 120;

/** "Shell" badge for remote shell cards and headers. */
export function ShellBadge({ compact = false }: { compact?: boolean }) {
  const { t } = useI18n();
  return (
    <span className={`${styles.shellBadge} ${compact ? styles.compact : ''}`}>
      <Icon name="terminal" size={compact ? 11 : 12} />
      {t('shell.badge')}
    </span>
  );
}

/**
 * Time left before the server closes a remote shell: the idle deadline
 * (moves with input) or the absolute lifetime, whichever comes first.
 */
export function ShellCountdown({
  session,
  compact = false,
}: {
  session: SessionInfo;
  compact?: boolean;
}) {
  const { t } = useI18n();
  const now = useNow();
  const next = nextDeadline(session);
  if (!next || session.state.type === 'exited') return null;
  const left = secondsLeft(next.at, now);
  const time = formatCountdown(left);
  const label =
    next.kind === 'idle' ? t('shell.closesIdle', { time }) : t('shell.closesHard', { time });
  return (
    <span
      className={`${styles.countdown} ${compact ? styles.compact : ''}`}
      data-warn={left <= WARN_SECONDS || undefined}
      data-testid="shell-countdown"
      title={label}
    >
      <Icon name="clock" size={compact ? 11 : 13} />
      {compact ? <span aria-label={label}>{time}</span> : label}
    </span>
  );
}
