import { useSyncExternalStore } from 'react';

import { buildInbox, deferredItems, type InboxItem } from '../../state/inbox';
import { useAppState, useConductor } from '../../state/store';

/** Keys put off for later on this device. */
export function useDeferred(): ReadonlySet<string> {
  return useSyncExternalStore(deferredItems.subscribe, deferredItems.get);
}

/** The Approvals inbox, built from what the app already has. */
export function useInbox(): InboxItem[] {
  const alerts = useAppState((s) => s.alerts.active);
  const tasks = useAppState((s) => s.tasks);
  const sessions = useAppState((s) => s.sessions);
  const conductor = useConductor();
  return buildInbox({ alerts, conductor, tasks, sessions });
}

/** Everything waiting on the person, not put off for later. */
export function useInboxCount(): number {
  const items = useInbox();
  const deferred = useDeferred();
  return items.filter((i) => !deferred.has(i.key)).length;
}
