import { useEffect, useState } from 'react';

import { useI18n } from '../i18n/context';
import { activityLine, elapsedParts, type ChatProgressInfo } from './chatProgress';
import styles from './ChatProgressLine.module.css';

/** How often the timer moves (ms). */
const TICK_MS = 1000;

/**
 * The line under the chat while the agent works: a pulsing dot, "Working ·
 * 42s" and its current step. Screen readers hear the phase and the step
 * when they change, not every second of the timer.
 */
export function ChatProgressLine({ progress }: { progress: ChatProgressInfo }) {
  const { t } = useI18n();
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), TICK_MS);
    return () => clearInterval(timer);
  }, []);
  if (progress.until !== null && now >= progress.until) return null;
  const elapsed = elapsedParts(now - progress.since);
  const time = t(elapsed.key, elapsed.vars);
  const phaseKey = progress.phase === 'working' ? 'chat.progress.working' : 'chat.progress.waiting';
  const step = progress.activity ? activityLine(progress.activity) : null;
  const stepText = step ? t(step.key, step.vars) : '';
  const spoken = [
    t(progress.phase === 'working' ? 'chat.progress.workingSr' : 'chat.progress.waitingSr'),
    stepText,
  ]
    .filter(Boolean)
    .join(', ');
  return (
    <div
      className={styles.progress}
      data-phase={progress.phase}
      data-thinking={progress.activity?.type === 'thinking' || undefined}
      data-testid="chat-progress"
    >
      <span className={styles.dot} aria-hidden="true" />
      <span className={styles.phase} aria-hidden="true">
        {t(phaseKey, { time })}
      </span>
      {stepText && (
        <span className={styles.step} aria-hidden="true" title={stepText}>
          {stepText}
        </span>
      )}
      <span className="visually-hidden" role="status">
        {spoken}
      </span>
    </div>
  );
}
