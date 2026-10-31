/**
 * Derives user-facing alerts (NTF-01/02/04) from server messages.
 * Pure: takes the state before the message is applied.
 */
import type { ServerMsg, SessionInfo, SessionState } from '../protocol/types';
import type { TranslateFn } from '../i18n/i18n';
import type { AppState, Toast } from './reducer';
import type { AlertCategory } from './settings';

/** Notification events older than this (e.g. history replays) do not alert. */
export const LIVE_WINDOW_MS = 15_000;

/** Exit code used for sessions stopped by the user (SIGINT convention). */
const USER_STOP_CODE = 130;

function categoryForTransition(prev: SessionState, next: SessionState): AlertCategory | null {
  if (prev.type === next.type) return null;
  switch (next.type) {
    case 'awaiting_decision':
      return 'decision';
    case 'awaiting_input':
      return 'input';
    case 'error':
      return 'error';
    case 'exited':
      if (next.code === 0) return 'completed';
      return next.code === USER_STOP_CODE ? null : 'error';
    case 'idle':
      return prev.type === 'running' ? 'completed' : null;
    default:
      return null;
  }
}

function titleFor(category: AlertCategory, session: SessionInfo, t: TranslateFn): string {
  const name = session.display_name;
  switch (category) {
    case 'decision':
      return t('alert.decision', { name });
    case 'input':
      return t('alert.input', { name });
    case 'error':
      return t('alert.error', { name });
    case 'completed':
      return t('alert.completed', { name });
    case 'activity':
      return name;
  }
}

export function alertsFor(
  prev: AppState,
  msg: ServerMsg,
  t: TranslateFn,
  now: number = Date.now(),
): Toast[] {
  if (msg.type === 'session_updated') {
    const before = prev.sessions[msg.session.id];
    if (!before) return [];
    const category = categoryForTransition(before.state, msg.session.state);
    if (!category) return [];
    const body =
      msg.session.state.type === 'error' ? msg.session.state.message : (msg.session.preview ?? '');
    return [
      {
        id: `${msg.session.id}:${category}:${msg.session.last_activity}`,
        category,
        sessionId: msg.session.id,
        title: titleFor(category, msg.session, t),
        body,
      },
    ];
  }
  if (msg.type === 'event' && msg.event.type === 'notification') {
    const view = prev.views[msg.session_id];
    if (view?.lastSeq != null && msg.event.seq <= view.lastSeq) return [];
    if (now - msg.event.at > LIVE_WINDOW_MS) return [];
    return [
      {
        id: `${msg.session_id}:activity:${msg.event.seq}`,
        category: 'activity',
        sessionId: msg.session_id,
        title: msg.event.title,
        body: msg.event.body,
      },
    ];
  }
  return [];
}
