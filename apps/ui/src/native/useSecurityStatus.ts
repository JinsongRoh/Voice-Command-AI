import { useCallback, useEffect, useSyncExternalStore } from 'react';

import type { SecurityServices, SecurityStatus } from './security';

export interface SecurityStatusState {
  status: SecurityStatus | null;
  /** The last refresh failed (e.g. `unreachable`: the server is not running). */
  error: unknown;
  loading: boolean;
  /** At least one refresh finished. */
  loaded: boolean;
}

const INITIAL: SecurityStatusState = { status: null, error: null, loading: false, loaded: false };

/**
 * One shared status per services object, so the Settings sections, the
 * step-up prompt and the first-run wizard agree after any change.
 */
class StatusStore {
  state: SecurityStatusState = INITIAL;
  private listeners = new Set<() => void>();
  private inflight: Promise<void> | null = null;

  constructor(private readonly services: SecurityServices) {}

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  snapshot = () => this.state;

  private update(next: Partial<SecurityStatusState>) {
    this.state = { ...this.state, ...next };
    for (const l of this.listeners) l();
  }

  refresh = (): Promise<void> => {
    if (this.inflight) return this.inflight;
    this.update({ loading: true });
    this.inflight = this.services
      .status()
      .then(
        (status) => this.update({ status, error: null, loading: false, loaded: true }),
        (error: unknown) => this.update({ status: null, error, loading: false, loaded: true }),
      )
      .finally(() => {
        this.inflight = null;
      });
    return this.inflight;
  };

  set = (status: SecurityStatus) => this.update({ status, error: null, loaded: true });
}

const stores = new WeakMap<SecurityServices, StatusStore>();

function storeFor(services: SecurityServices): StatusStore {
  let store = stores.get(services);
  if (!store) {
    store = new StatusStore(services);
    stores.set(services, store);
  }
  return store;
}

/** Re-reads the shared status (after a change made elsewhere). */
export function refreshSecurityStatus(services: SecurityServices): Promise<void> {
  return storeFor(services).refresh();
}

/** The security status of this PC's server; loaded on first use. */
export function useSecurityStatus(services: SecurityServices) {
  const store = storeFor(services);
  const state = useSyncExternalStore(store.subscribe, store.snapshot, store.snapshot);
  useEffect(() => {
    if (!store.state.loaded && !store.state.loading) void store.refresh();
  }, [store]);
  const refresh = useCallback(() => store.refresh(), [store]);
  const set = useCallback((s: SecurityStatus) => store.set(s), [store]);
  return { ...state, refresh, set };
}
