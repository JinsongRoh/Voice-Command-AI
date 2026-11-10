/**
 * "View source conversation" of a to-do: the session view opens on its
 * chat, pages in earlier history until the source message is loaded, then
 * scrolls to it and highlights it. The request lives outside the store (like
 * the Command Center's terminal wish) so that it reaches a session view that is already
 * open as well as one that mounts afterwards.
 */
import { useSyncExternalStore } from 'react';

import type { SessionId } from '../protocol/types';

export interface ChatFocus {
  sessionId: SessionId;
  /** The source message's sequence number. */
  seq: number;
  /** Shown when the message cannot be found any more. */
  excerpt: string | null;
  /** Each request is new, even for the same message. */
  token: number;
  /**
   * `pending`: the view is looking for the message; `found`: it is shown
   * and highlighted; `missing`: it is gone (the excerpt is shown instead).
   */
  status: 'pending' | 'found' | 'missing';
}

let focus: ChatFocus | null = null;
let counter = 0;
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

/** Asks the session view of `sessionId` to show message `seq`. */
export function requestChatFocus(
  sessionId: SessionId,
  seq: number,
  excerpt: string | null = null,
): ChatFocus {
  counter += 1;
  focus = { sessionId, seq, excerpt, token: counter, status: 'pending' };
  emit();
  return focus;
}

/** The view found the message, or gave up looking for it. */
export function resolveChatFocus(token: number, status: 'found' | 'missing'): void {
  if (focus?.token !== token || focus.status === status) return;
  focus = { ...focus, status };
  emit();
}

/** Done with the request (the excerpt was closed, or the view left). */
export function clearChatFocus(token: number): void {
  if (focus?.token !== token) return;
  focus = null;
  emit();
}

export function currentChatFocus(): ChatFocus | null {
  return focus;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The request for this session, if any. */
export function useChatFocus(sessionId: SessionId): ChatFocus | null {
  const f = useSyncExternalStore(subscribe, currentChatFocus, currentChatFocus);
  return f?.sessionId === sessionId ? f : null;
}
