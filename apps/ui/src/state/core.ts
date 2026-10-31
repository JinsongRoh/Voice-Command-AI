import type { ErrorCode } from '../protocol/types';
import { initialState, reducer, type Action, type AppState } from './reducer';

export const APP_VERSION = '0.1.0';

export class RequestError extends Error {
  constructor(
    readonly code: ErrorCode | 'transport',
    message: string,
  ) {
    super(message);
    this.name = 'RequestError';
  }
}

export interface Store {
  getState(): AppState;
  dispatch(action: Action): void;
  subscribe(listener: () => void): () => void;
}

export function createStore(initial: AppState = initialState): Store {
  let state = initial;
  const listeners = new Set<() => void>();
  return {
    getState: () => state,
    dispatch(action) {
      const next = reducer(state, action);
      if (next === state) return;
      state = next;
      for (const l of listeners) l();
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}
