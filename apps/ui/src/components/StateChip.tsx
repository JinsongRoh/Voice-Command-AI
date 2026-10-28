import { useI18n } from '../i18n/context';
import type { SessionState } from '../protocol/types';
import { stateKey } from './stateKey';
import styles from './StateChip.module.css';

/** Colored chip with text (color is never the only signal). */
export function StateChip({ state }: { state: SessionState }) {
  const { t } = useI18n();
  const label =
    state.type === 'exited' && state.code !== null
      ? t('state.exitedCode', { code: state.code })
      : t(stateKey(state));
  return (
    <span className={`${styles.chip} ${styles[state.type] ?? ''}`} data-state={state.type}>
      <span className={styles.dot} aria-hidden="true" />
      {label}
    </span>
  );
}
