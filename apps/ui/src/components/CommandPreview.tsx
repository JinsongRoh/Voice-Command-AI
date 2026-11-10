import { useMemo } from 'react';

import { useI18n } from '../i18n/context';
import { highlightSegments, isKnownReason } from '../protocol/shell';
import type { DangerLevel } from '../protocol/types';
import styles from './Shell.module.css';

/** Level badge of a classified command (text, not only color). */
export function DangerBadge({ level }: { level: DangerLevel }) {
  const { t } = useI18n();
  return (
    <span className={styles.badge} data-level={level}>
      <span className={styles.badgeDot} aria-hidden="true" />
      {t(`shell.level.${level}`)}
    </span>
  );
}

/**
 * A command line with the risky parts the server found highlighted. `spans`
 * are UTF-8 byte offsets; they are mapped onto the string here.
 */
export function CommandPreview({
  command,
  spans = [],
  label,
}: {
  command: string;
  spans?: readonly (readonly [number, number])[];
  label: string;
}) {
  const segments = useMemo(() => highlightSegments(command, spans), [command, spans]);
  return (
    <pre className={styles.command} aria-label={label} data-testid="command-preview">
      <code>
        {segments.map((seg, i) =>
          seg.risky ? (
            <mark key={i} className={styles.risky}>
              {seg.text}
            </mark>
          ) : (
            <span key={i}>{seg.text}</span>
          ),
        )}
      </code>
    </pre>
  );
}

/** Translated reason codes (unknown codes are shown as they are). */
export function DangerReasons({ reasons }: { reasons: string[] }) {
  const { t } = useI18n();
  if (reasons.length === 0) return null;
  return (
    <ul className={styles.reasons}>
      {reasons.map((r) => (
        <li key={r}>{isKnownReason(r) ? t(`shell.reason.${r}`) : r}</li>
      ))}
    </ul>
  );
}
