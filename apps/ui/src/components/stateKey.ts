import type { SessionState } from '../protocol/types';

/** i18n key for a session state label. */
export function stateKey(state: SessionState) {
  return `state.${state.type}` as const;
}
