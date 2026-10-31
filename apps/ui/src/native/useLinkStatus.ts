import { useCallback, useSyncExternalStore } from 'react';

import type { Machine } from '../state/store';
import type { LinkStatus } from './bridge';

const noop = () => {};

/** Native link status of a machine (null for non-native machines or before the first status). */
export function useLinkStatus(machine: Machine): LinkStatus | null {
  const native = machine.native;
  const subscribe = useCallback(
    (onChange: () => void) => (native ? native.onLinkStatus(onChange) : noop),
    [native],
  );
  return useSyncExternalStore(subscribe, () => native?.linkStatus() ?? null);
}
