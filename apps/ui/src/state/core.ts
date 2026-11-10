import type { ErrorCode } from '../protocol/types';
import { initialState, reducer, type Action, type AppState } from './reducer';

export const APP_VERSION = '0.1.0';
/** When the UI bundle was built (see vite.config.ts); shown next to the version. */
export const BUILD_STAMP: string = typeof __BUILD_STAMP__ === 'string' ? __BUILD_STAMP__ : 'dev';

/**
 * While a session's first page is on its way, the replayed events are kept
 * back from the view until they stop arriving this long, so the page appears
 * in one piece instead of streaming in.
 */
export const OPEN_SETTLE_MS = 40;
/** The page is shown at the latest this long after the first event. */
export const OPEN_MAX_MS = 300;
/** A session that never sends anything is shown (as empty) after this long. */
export const OPEN_EMPTY_MS = 1500;

export class RequestError extends Error {
  constructor(
    readonly code: ErrorCode | 'transport',
    message: string,
  ) {
    super(message);
    this.name = 'RequestError';
  }
}

/**
 * The shell did not type the input: it has `lines` lines and runs only after
 * the person confirmed all of `command` (`shell_confirm`).
 */
export class ShellConfirmNeeded extends Error {
  constructor(
    readonly sessionId: string,
    readonly command: string,
    readonly lines: number,
  ) {
    super(`these ${lines} lines need a confirmation`);
    this.name = 'ShellConfirmNeeded';
  }
}

export interface Store {
  getState(): AppState;
  dispatch(action: Action): void;
  subscribe(listener: () => void): () => void;
}

interface Opening {
  settle?: ReturnType<typeof setTimeout>;
  max?: ReturnType<typeof setTimeout>;
  empty?: ReturnType<typeof setTimeout>;
}

/** The session whose first page this action is part of, if any. */
function replayTarget(action: Action, state: AppState): string | null {
  if (action.type !== 'server') return null;
  const { msg } = action;
  if (msg.type !== 'event' && msg.type !== 'stream_gap') return null;
  return state.views[msg.session_id]?.loading ? msg.session_id : null;
}

export function createStore(initial: AppState = initialState): Store {
  let state = initial;
  const listeners = new Set<() => void>();
  const opening = new Map<string, Opening>();
  const notify = () => {
    for (const l of listeners) l();
  };
  const clear = (o: Opening) => {
    clearTimeout(o.settle);
    clearTimeout(o.max);
    clearTimeout(o.empty);
  };
  const store: Store = {
    getState: () => state,
    dispatch(action) {
      const hold = replayTarget(action, state);
      const next = reducer(state, action);
      if (next === state) return;
      state = next;
      if (action.type === 'view_opening') {
        // Even a session with nothing to say stops showing the placeholder.
        const o = opening.get(action.sessionId) ?? {};
        clearTimeout(o.empty);
        o.empty = setTimeout(
          () => store.dispatch({ type: 'view_opened', sessionId: action.sessionId }),
          OPEN_EMPTY_MS,
        );
        opening.set(action.sessionId, o);
      }
      if (action.type === 'view_opened') {
        const o = opening.get(action.sessionId);
        if (o) clear(o);
        opening.delete(action.sessionId);
      }
      if (hold !== null) {
        const o = opening.get(hold) ?? {};
        const open = () => store.dispatch({ type: 'view_opened', sessionId: hold });
        clearTimeout(o.settle);
        o.settle = setTimeout(open, OPEN_SETTLE_MS);
        o.max ??= setTimeout(open, OPEN_MAX_MS);
        opening.set(hold, o);
        return;
      }
      notify();
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
  return store;
}
