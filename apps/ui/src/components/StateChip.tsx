import { useI18n } from '../i18n/context';
import type { SessionState, TerminalPrompt } from '../protocol/types';
import { stateKey } from './stateKey';
import styles from './StateChip.module.css';

/**
 * Colored chip with text (color is never the only signal). A menu only the
 * terminal answers reads "Choose in the terminal" instead of a bare "Waiting for input".
 */
export function StateChip({
  state,
  terminalPrompt,
}: {
  state: SessionState;
  terminalPrompt?: TerminalPrompt | null;
}) {
  const { t } = useI18n();
  const choose = !!terminalPrompt && state.type !== 'exited';
  const label = choose
    ? t('state.terminalChoice')
    : state.type === 'exited' && state.code !== null
      ? t('state.exitedCode', { code: state.code })
      : t(stateKey(state));
  return (
    <span
      className={`${styles.chip} ${styles[choose ? 'awaiting_input' : state.type] ?? ''}`}
      data-state={state.type}
      data-terminal-prompt={choose ? terminalPrompt : undefined}
    >
      <span className={styles.dot} aria-hidden="true" />
      {label}
    </span>
  );
}
