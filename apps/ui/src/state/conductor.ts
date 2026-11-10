/**
 * The conductor (conductor session) as the app sees it: the server's state,
 * kept in a tiny external store fed by `conductor` pushes, and a request bus
 * that lets the composer's "Conductor mode" shortcut open the Command Center
 * view.
 */
import {
  CONDUCTOR_PROBLEM_HOOKS_MISSING,
  CONDUCTOR_PROBLEM_TURN_STALLED,
  type ConductorState,
  type ConductorStatus,
  type ConductorTurn,
  type ServerMsg,
  type SessionEvent,
  type SessionInfo,
} from '../protocol/types';

export interface ConductorStore {
  get(): ConductorState | null;
  set(state: ConductorState | null): void;
  subscribe(listener: () => void): () => void;
  /** Feeds a server message; calls `onReply` for a new conductor reply. */
  handle(msg: ServerMsg, onReply?: (text: string, at: number) => void): void;
}

export function createConductorStore(): ConductorStore {
  let state: ConductorState | null = null;
  const listeners = new Set<() => void>();
  let lastReply = 0;
  const set = (next: ConductorState | null) => {
    state = next;
    for (const l of listeners) l();
  };
  return {
    get: () => state,
    set,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    handle(msg, onReply) {
      if (msg.type === 'conductor') set(msg.state);
      else if (msg.type === 'conductor_reply' && msg.at > lastReply) {
        lastReply = msg.at;
        onReply?.(msg.text, msg.at);
      }
    },
  };
}

/** Conductor requests waiting for the person (the Command Center badge). */
export function pendingCount(state: ConductorState | null): number {
  return state?.pending.length ?? 0;
}

/**
 * The conductor's replies cannot be read because the agent's VCA hooks are
 * missing. Only what the server reports counts (`problem`); older servers
 * report nothing and the app does not guess.
 */
export function hooksMissing(state: ConductorState | null): boolean {
  return state?.problem === CONDUCTOR_PROBLEM_HOOKS_MISSING;
}

/**
 * The conductor's last turn has shown no end for 15 minutes. The server
 * reports missing hooks instead when both apply.
 */
export function turnStalled(state: ConductorState | null): boolean {
  return state?.problem === CONDUCTOR_PROBLEM_TURN_STALLED;
}

/**
 * The Command Center is paused (`cc_pause`): on, but nothing wakes the
 * conductor. A request waiting for the person shows as `waiting_for_you`
 * even then, so the setting counts too.
 */
export function conductorPaused(state: ConductorState | null): boolean {
  return (
    !!state &&
    !conductorStopped(state) &&
    (state.status === 'paused' || state.settings.paused === true)
  );
}

/** The conductor is stopped (by the person, or by the kill switch). */
export function conductorStopped(state: ConductorState | null): boolean {
  return !state || state.status === 'off' || state.status === 'revoked';
}

/** The conductor is on and can take instructions. */
export function conductorUsable(state: ConductorState | null): boolean {
  return (
    !!state &&
    state.settings.enabled &&
    (state.status === 'ready' ||
      state.status === 'running' ||
      state.status === 'waiting_for_you' ||
      state.status === 'starting' ||
      // Paused: nothing wakes it, but it still takes instructions.
      state.status === 'paused')
  );
}

export const STATUS_KEYS: Record<
  ConductorStatus,
  | 'conductor.status.off'
  | 'conductor.status.starting'
  | 'conductor.status.running'
  | 'conductor.status.ready'
  | 'conductor.status.waiting'
  | 'conductor.status.revoked'
  | 'conductor.status.paused'
> = {
  off: 'conductor.status.off',
  starting: 'conductor.status.starting',
  running: 'conductor.status.running',
  ready: 'conductor.status.ready',
  waiting_for_you: 'conductor.status.waiting',
  revoked: 'conductor.status.revoked',
  paused: 'conductor.status.paused',
};

/**
 * What the conductor's own session waits on, for the cards at the top of
 * the Command Center: Claude Code's first-run "trust this folder" question,
 * or a permission decision (also in Alerts as "Conductor session"). `null`: nothing.
 */
export function conductorSessionWait(
  state: ConductorState | null,
  sessions: Record<string, SessionInfo | undefined>,
): { kind: 'trust' | 'decision'; session: SessionInfo } | null {
  const id = state?.session_id;
  const session = id ? sessions[id] : undefined;
  if (!session || session.state.type === 'exited') return null;
  if (session.terminal_prompt === 'trust_folder') return { kind: 'trust', session };
  if (session.pending_decision || session.state.type === 'awaiting_decision')
    return { kind: 'decision', session };
  return null;
}

let terminalWish: string | null = null;

/** The session view opens `sessionId` on its terminal tab (the Command Center's "Show terminal"). */
export function requestTerminal(sessionId: string): void {
  terminalWish = sessionId;
}

export function wantsTerminal(sessionId: string): boolean {
  return terminalWish === sessionId;
}

export function clearTerminalWish(sessionId: string): void {
  if (terminalWish === sessionId) terminalWish = null;
}

const viewListeners = new Set<() => void>();

/** Asks the app to show the Command Center view (the composer's "Conductor mode"). */
export function requestConductorView(): void {
  for (const l of viewListeners) l();
}

export function onConductorViewRequest(listener: () => void): () => void {
  viewListeners.add(listener);
  return () => {
    viewListeners.delete(listener);
  };
}

/**
 * Draft key of the conductor's chat when it has no session to type into
 * (the built-in agent, or a session the app does not list yet).
 */
export const CONDUCTOR_CHAT_DRAFT = 'conductor:chat';

/**
 * The built-in conductor's chat (`ConductorState.chat`) as chat events, so
 * the normal chat view shows it: the person's lines as prompts, replies as
 * agent messages and wake-up events as system notes.
 */
export function turnsToEvents(turns: readonly ConductorTurn[]): SessionEvent[] {
  return turns.map((turn, i) => ({
    type: 'message',
    seq: i + 1,
    at: turn.at,
    role: turn.role === 'user' ? 'user' : turn.role === 'assistant' ? 'assistant' : 'system',
    text: turn.text,
  }));
}
