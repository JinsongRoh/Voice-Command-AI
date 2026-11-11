/**
 * What a spoken alert says. A completion names the session by the name the
 * user gave it (its card number only without one) and, when there is a safe
 * one, the topic of its work ("Orbit Platform, fix the login bug,
 * finished."); an error adds a short generic reason ("Orbit Platform hit an
 * error: permission problem."). Neither ever reads a
 * command, a path or code: the topic comes from the session's title and is
 * dropped when it looks technical, and error reasons are fixed words.
 */
import type { MessageKey, Vars } from '../i18n/i18n';
import type { SessionInfo, SessionState } from '../protocol/types';
import type { AlertCategory } from './settings';
import type { VoicePriority } from '../speech/voiceQueue';

export type Translate = (key: MessageKey, vars?: Vars) => string;

/** Longest topic read out. */
const MAX_TOPIC_CHARS = 40;

/** Signs of commands, paths, code or URLs. */
const TECHNICAL =
  /[/\\`$|<>{}[\]=;~^*#@]|&&|--|\.\w{1,5}\b|\b(?:sudo|rm|git|npm|pnpm|yarn|npx|cargo|pip|curl|wget|docker|kubectl|cd|ls|cat|echo)\b|https?:/i;

/**
 * The topic of a session's work, safe to read aloud: its automatic title
 * (the agent's title or the launch name), else a custom name. `null` when
 * it is the generic "<folder> · <agent>" name or looks technical.
 */
export function spokenTopic(
  session: Pick<SessionInfo, 'auto_name' | 'display_name'>,
): string | null {
  const raw = (session.auto_name ?? session.display_name ?? '').replace(/\s+/g, ' ').trim();
  if (!raw || raw.includes(' · ') || TECHNICAL.test(raw)) return null;
  if ([...raw].length > MAX_TOPIC_CHARS) return null;
  return raw;
}

/**
 * "Orbit Platform, fix the login bug, finished." in the listener's
 * language: by the session's spoken name (`name`, see speech/sessionNames.ts)
 * when there is one, else by its card number.
 */
export function completionLine(
  t: Translate,
  number: number | null,
  topic: string | null,
  name?: string | null,
): string {
  if (name) {
    const sameAsName = topic && topic.replace(/\s+/g, ' ').trim() === name.trim();
    return topic && !sameAsName
      ? t('voice.finishedNamedTopic', { name, topic })
      : t('summary.finishedNamed', { name });
  }
  if (number == null) {
    return topic ? t('voice.finishedTopicOnly', { topic }) : t('voice.finishedNoNumber');
  }
  return topic ? t('voice.finishedTopic', { number, topic }) : t('summary.finished', { number });
}

const REASONS: [MessageKey, RegExp][] = [
  ['voice.reason.timeout', /time(?:d)?\s*out|timeout/i],
  ['voice.reason.permission', /permission|denied|forbidden|unauthori[sz]ed|EACCES|EPERM/i],
  ['voice.reason.notFound', /not found|no such|ENOENT|404/i],
  ['voice.reason.network', /network|connect|ECONNRE|unreachable|DNS|socket/i],
  ['voice.reason.memory', /out of memory|OOM|ENOMEM|heap/i],
];

/** A short, generic reason for an error state (never its text). */
export function errorReason(t: Translate, state: SessionState): string | null {
  if (state.type === 'exited' && state.code != null && state.code !== 0) {
    return t('voice.reason.exitCode', { code: state.code });
  }
  if (state.type !== 'error') return null;
  const found = REASONS.find(([, re]) => re.test(state.message));
  return found ? t(found[0]) : null;
}

/** "Orbit Platform hit an error: permission problem." (by name, else by number). */
export function errorLine(
  t: Translate,
  number: number | null,
  state: SessionState,
  name?: string | null,
): string {
  const reason = errorReason(t, state);
  const who = name
    ? t('voice.sessionNamed', { name })
    : number != null
      ? t('voice.sessionNumber', { number })
      : t('voice.aSession');
  return reason ? t('voice.errorReason', { who, reason }) : t('voice.error', { who });
}

/** Queue priority of an alert category. */
export function priorityOf(category: AlertCategory): VoicePriority {
  switch (category) {
    case 'decision':
    case 'question':
    case 'input':
      return 'decision';
    case 'error':
      return 'error';
    default:
      return 'completion';
  }
}
