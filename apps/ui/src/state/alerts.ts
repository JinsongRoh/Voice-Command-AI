/**
 * Derives user-facing alerts (NTF-01/02/04) from server messages.
 * Pure: takes the state before the message is applied.
 */
import type { ServerMsg, SessionInfo, SessionState } from '../protocol/types';
import type { MessageKey, TranslateFn } from '../i18n/i18n';
import type { AppState, Toast } from './reducer';
import type { AlertCategory } from './settings';

/** Why a remote shell closed (`shell_notice.reason`). */
const SHELL_CLOSE_REASONS: Record<string, MessageKey> = {
  user: 'shell.closeReason.user',
  idle: 'shell.closeReason.idle',
  lifetime: 'shell.closeReason.lifetime',
  kill_switch: 'shell.closeReason.kill_switch',
  exit: 'shell.closeReason.exit',
  shutdown: 'shell.closeReason.shutdown',
};

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
      // No exit code: a terminal VCA only watched was closed; nothing
      // says it failed (spoken alerts skip it too).
      if (next.code == null) return null;
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
      return session.pending_decision?.decision.questions?.length
        ? t('alert.question', { name })
        : session.pending_decision?.decision.inferred
          ? t('alert.decisionInferred', { name })
          : t('alert.decision', { name });
    case 'question':
      return t('alert.agentQuestion', { name });
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
    // The conductor's own session alerts only for decisions and errors: its
    // turn-ends and input waits are its replies, which reach the Command
    // Center (`conductor_reply`).
    if (
      (msg.session.conductor || before.conductor) &&
      category !== 'decision' &&
      category !== 'error'
    )
      return [];
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
  if (msg.type === 'shell_notice') {
    const name = prev.sessions[msg.session_id]?.display_name ?? msg.session_id;
    if (msg.event === 'joined') {
      return [
        {
          id: `${msg.session_id}:shell:joined:${msg.device_name}`,
          category: 'activity',
          sessionId: msg.session_id,
          title: t('shell.notice.joined', { device: msg.device_name, name }),
          body: '',
          security: true,
        },
      ];
    }
    const opened = msg.event === 'opened';
    const reason = msg.reason ?? null;
    const reasonKey = reason ? SHELL_CLOSE_REASONS[reason] : undefined;
    return [
      {
        id: `${msg.session_id}:shell:${msg.event}`,
        category: 'activity',
        sessionId: opened ? msg.session_id : null,
        title: opened
          ? t('shell.notice.opened', { device: msg.device_name })
          : t('shell.notice.closed', { name }),
        body: opened ? name : reasonKey ? t(reasonKey) : (reason ?? ''),
        security: true,
      },
    ];
  }
  if (msg.type === 'event' && msg.event.type === 'notification') {
    const view = prev.views[msg.session_id];
    if (view?.lastSeq != null && msg.event.seq <= view.lastSeq) return [];
    if (now - msg.event.at > LIVE_WINDOW_MS) return [];
    // The conductor's notifications are not alerts (older servers send them).
    if (prev.sessions[msg.session_id]?.conductor) return [];
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
