import { listen } from '@tauri-apps/api/event';

import { HOST_CHANGED_EVENT } from './host';

/**
 * Calls `onChange` when the VCA server on this PC comes up or goes away
 * (desktop shell). Returns the unsubscribe function.
 */
export function listenForHostChanges(onChange: () => void): () => void {
  let stop: (() => void) | null = null;
  let active = true;
  listen<boolean>(HOST_CHANGED_EVENT, () => onChange())
    .then((unsubscribe) => {
      if (active) stop = unsubscribe;
      else unsubscribe();
    })
    .catch(() => {});
  return () => {
    active = false;
    stop?.();
  };
}
