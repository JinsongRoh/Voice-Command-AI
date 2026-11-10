/**
 * The Command Center's Log: audit entries of the conductor as sentences a
 * person reads ("Sent “keep going…” to Orbit 3D (auto-allowed).") instead
 * of the server's `key=value` details.
 *
 * Entries come from the server's audit log (`conductor_*`, later `cc_*`).
 * The server keeps no prompt text there: a send holds its length
 * (`chars=…`). Older entries still quote it as the last field (`text=…`,
 * which may hold spaces, as may a refusal's `reason=…`) and are shown so.
 */
import type { MessageKey, Vars } from '../../i18n/i18n';
import type { AuditEntry, SessionId } from '../../protocol/types';

type Translate = (key: MessageKey, vars?: Vars) => string;

/** Fields that run to the end of the detail (they may contain spaces). */
const TAIL_FIELDS = new Set(['text', 'reason', 'command', 'summary']);
/** Quoted text in a sentence is cut to this many characters. */
export const LOG_TEXT_CHARS = 60;

/** Audit events the Log shows: the conductor's and the Command Center's. */
export function isCommandEntry(entry: AuditEntry): boolean {
  return entry.event.startsWith('conductor_') || entry.event.startsWith('cc_');
}

/** `key=value` fields of an audit detail; `{}` for a plain sentence. */
export function parseDetail(detail: string): Record<string, string> {
  const fields: Record<string, string> = {};
  const text = detail.trim();
  if (!/^[a-z_]+=/.test(text)) return fields;
  const re = /(?:^|\s)([a-z_]+)=/g;
  const marks: { key: string; start: number; end: number }[] = [];
  for (let m = re.exec(text); m; m = re.exec(text)) {
    marks.push({ key: m[1]!, start: m.index, end: m.index + m[0].length });
  }
  for (let i = 0; i < marks.length; i++) {
    const mark = marks[i]!;
    if (TAIL_FIELDS.has(mark.key)) {
      fields[mark.key] = text.slice(mark.end).trim();
      break;
    }
    const next = marks[i + 1];
    fields[mark.key] = text.slice(mark.end, next ? next.start : undefined).trim();
  }
  return fields;
}

/** Sentences of `conductor_send`; each has a `…NoText` twin without the quote. */
type SendKey = 'sentAuto' | 'sentInstruction' | 'sentAllowed' | 'sent' | 'timedOut' | 'denied';

function clip(text: string, max = LOG_TEXT_CHARS): string {
  const chars = Array.from(text.replace(/\s+/g, ' ').trim());
  return chars.length > max ? `${chars.slice(0, max - 1).join('')}…` : chars.join('');
}

const AGENTS = new Set(['claude_code', 'codex', 'gemini_cli', 'builtin']);

function agentLabel(t: Translate, agent: string | undefined): string {
  if (agent && AGENTS.has(agent)) return t(`conductor.agent.${agent}` as MessageKey);
  return agent ?? '';
}

/**
 * One audit entry as a sentence. `nameOf` gives a session's current name
 * (`null`: unknown, e.g. it has ended).
 */
export function auditSentence(
  entry: AuditEntry,
  t: Translate,
  nameOf: (id: SessionId) => string | null,
): string {
  const f = parseDetail(entry.detail);
  const session = (entry.session_id ? nameOf(entry.session_id) : null) ?? t('cc.log.someSession');
  const text = clip(f.text ?? '');
  // Entries written before the audit stopped keeping prompt text quote it;
  // newer ones hold its length only (`chars=`) and say "a message".
  const quoted = f.text !== undefined;
  const send = (key: SendKey) =>
    quoted ? t(`cc.log.${key}`, { text, session }) : t(`cc.log.${key}NoText`, { session });
  switch (entry.event) {
    case 'conductor_send':
      if (f.outcome === 'sent') {
        if (f.confirmed_by === 'allowlist') return send('sentAuto');
        // The person's own instruction of that turn named the session.
        if (f.confirmed_by === 'instruction') return send('sentInstruction');
        if (f.confirmed_by === 'person') return send('sentAllowed');
        return send('sent');
      }
      if (f.outcome === 'timed_out') return send('timedOut');
      return send('denied');
    case 'conductor_interrupt':
      return t('cc.log.interrupt', { session });
    case 'conductor_focus':
      return t('cc.log.focus', { session });
    case 'conductor_restore':
      return t('cc.log.restore', { profile: f.profile ?? '', count: Number(f.windows ?? 0) });
    case 'conductor_new_session':
      return t('cc.log.newSession', { agent: agentLabel(t, f.agent), session });
    case 'conductor_refused':
      // A kill switch from a paired device without a valid code (no tool).
      if (!f.tool && /kill switch/i.test(entry.detail)) return t('cc.log.killRefused');
      return t('cc.log.refused', {
        tool: f.tool ?? '?',
        reason: clip(f.reason ?? entry.detail, 120),
      });
    case 'conductor_wake':
      // Newer entries: `events=N chars=M`, no text.
      if (f.events !== undefined) return t('cc.log.wakeNoText');
      return t('cc.log.wake', { text: clip(entry.detail, 120) });
    case 'conductor_start':
      return t('cc.log.start', { agent: agentLabel(t, f.agent) });
    case 'conductor_stop':
      return t('cc.log.stop');
    case 'conductor_kill':
      return t('cc.log.kill');
    case 'conductor_settings':
      return f.enabled === 'false'
        ? t('cc.log.settingsOff')
        : t('cc.log.settingsOn', {
            agent: agentLabel(t, f.agent),
            max: f.max_actions_per_hour ?? '?',
          });
    case 'conductor_decide':
      return t(f.allow === 'true' ? 'cc.log.decideAllow' : 'cc.log.decideDeny');
    case 'conductor_allow': {
      const minutes = Number(f.minutes ?? 0);
      return minutes > 0
        ? t('cc.log.allowAdd', { session, minutes })
        : t('cc.log.allowRemove', { session });
    }
    case 'conductor_trust':
      return t('cc.log.trust');
    case 'conductor_grant': {
      const minutes = Number(f.minutes ?? 10);
      if (f.all === 'true') return t('cc.log.grantAll', { minutes });
      const sessions = (f.sessions ?? '')
        .split(',')
        .filter(Boolean)
        .map((id) => nameOf(id) ?? t('cc.log.someSession'))
        .join(', ');
      return t('cc.log.grant', { sessions, minutes });
    }
    case 'conductor_no_reply':
      // The server's reason says when the session never reported a hook event.
      return t(/hook/i.test(entry.detail) ? 'cc.log.noReplyHooks' : 'cc.log.noReply');
    case 'conductor_answer':
      // Another session's decision, answered under the person's allow rule.
      return t(f.answer === 'approve' ? 'cc.log.answerApprove' : 'cc.log.answerDeny', {
        session,
        rule: f.rule ?? '?',
      });
    case 'conductor_escalate':
      if (f.source === 'fallback') return t('cc.log.escalatedFallback', { session });
      // Newer entries hold the summary's length only (`summary_chars=`).
      return f.summary === undefined
        ? t('cc.log.escalatedNoText', { session })
        : t('cc.log.escalated', { session, text: clip(f.summary) });
    case 'cc_pause':
      return t(f.paused === 'false' ? 'cc.log.resumed' : 'cc.log.paused');
    default: {
      const known = `audit.event.${entry.event}`;
      const label = t(known as MessageKey);
      const name = label === known ? entry.event : label;
      return entry.detail ? `${name} · ${clip(entry.detail, 120)}` : name;
    }
  }
}
