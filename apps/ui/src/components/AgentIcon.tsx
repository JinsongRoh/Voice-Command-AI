import type { CSSProperties } from 'react';

import { originHue, originLabel, originMonogram } from '../protocol/helpers';
import type { SessionOrigin } from '../protocol/types';
import styles from './AgentIcon.module.css';

/** Brand-neutral monogram tile for an agent or cloud provider. */
export function AgentIcon({ origin, size = 40 }: { origin: SessionOrigin; size?: number }) {
  const hue = originHue(origin);
  return (
    <span
      className={styles.icon}
      style={{ '--hue': hue, width: size, height: size } as CSSProperties}
      role="img"
      aria-label={originLabel(origin)}
    >
      {originMonogram(origin)}
    </span>
  );
}
