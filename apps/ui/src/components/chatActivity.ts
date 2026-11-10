/**
 * Classifies transcript text that the person did not type: background task
 * notifications, sub-agent hand-back reports, slash command wrappers,
 * compaction summaries and other harness messages. The server already marks
 * these as `system` messages; the same rules are applied to `user` messages
 * here so that older servers and other agents cannot attribute them to the
 * person either. Pure, so it is easy to unit-test.
 */

/** Tags the harness wraps around user records the person did not type. */
const MACHINE_TAGS = [
  '<task-notification',
  '<command-name',
  '<command-message',
  '<command-args',
  '<local-command-',
  '<bash-input',
  '<bash-stdout',
  '<bash-stderr',
  '<system-reminder',
  '<cross-session-message',
  '<agent-message',
  '<user-prompt-submit-hook',
  '<artifact-content',
  '<tool_use_error',
];

/** Plain-text openings of harness-written user records. */
const MACHINE_PREFIXES = [
  'Caveat: The messages below were generated',
  'Another Claude session sent a message',
  '[Subagent hand-back]',
  'This session is being continued from a previous conversation',
  '[Request interrupted',
  '[Cross-session idle notice]',
  'Stop hook feedback:',
  'Base directory for this skill:',
  '[Image: original ',
  '[Image: source:',
];

const AGENT_MESSAGE = /<agent-message\b|<cross-session-message\b|\[Subagent hand-back\]/;

/** Whether a "user" message was written by the harness or another agent. */
export function isMachineText(text: string): boolean {
  const t = text.trimStart();
  return (
    MACHINE_TAGS.some((p) => t.startsWith(p)) ||
    MACHINE_PREFIXES.some((p) => t.startsWith(p)) ||
    AGENT_MESSAGE.test(t)
  );
}

export type ActivityKind =
  /** A background agent finished (task notification for a sub-agent). */
  | 'subagent'
  /** A background shell command or monitor event. */
  | 'task'
  /** A sub-agent's final report handed back to this session. */
  | 'handback'
  /** A message from another agent session. */
  | 'peer'
  /** The summary written when the conversation was compacted. */
  | 'summary'
  /** Slash commands, `!` shell commands and their output. */
  | 'command'
  /** "[Request interrupted by user]" and similar one-line markers. */
  | 'marker'
  | 'system';

export type ActivityStatus = 'started' | 'running' | 'completed' | 'failed' | 'stopped';

export interface Activity {
  kind: ActivityKind;
  /** One-line summary shown while collapsed. */
  title: string;
  /** Full text shown when expanded; empty when there is nothing more. */
  body: string;
  status: ActivityStatus | null;
  /** Sub-agent description, used to match a notification to its tool call. */
  agent: string | null;
}

/** Inner text of the first `<name>…</name>` element, or null. */
export function tagText(text: string, name: string): string | null {
  const m = new RegExp(`<${name}(?:\\s[^>]*)?>([\\s\\S]*?)(?:</${name}>|$)`).exec(text);
  return m ? m[1]!.trim() : null;
}

function stripTags(text: string): string {
  return text
    .replace(/<\/?[a-zA-Z][\w-]*(?:\s[^>]*)?>/g, '')
    .replace(/\n{3,}/g, '\n\n')
    .trim();
}

function firstLine(text: string): string {
  return (
    text
      .split('\n')
      .map((l) => l.trim())
      .find((l) => l !== '') ?? ''
  );
}

/** Maps a harness status word (`completed`, `killed`, ...) to ours. */
export function normalizeStatus(raw: string | null): ActivityStatus | null {
  if (!raw) return null;
  const s = raw.trim().toLowerCase();
  if (s === 'completed' || s === 'success' || s === 'succeeded' || s === 'done') return 'completed';
  if (s === 'failed' || s === 'error' || s === 'errored') return 'failed';
  if (s === 'killed' || s === 'stopped' || s === 'cancelled' || s === 'canceled') return 'stopped';
  if (s === 'running' || s === 'in_progress' || s === 'pending') return 'running';
  return null;
}

/** `Agent "Build the site" finished` -> `Build the site`. */
function agentDescription(summary: string): string | null {
  const m = /^Agent\s+["“](.+?)["”]/.exec(summary);
  return m ? m[1]! : null;
}

function taskNotification(text: string): Activity {
  const summary = tagText(text, 'summary') ?? '';
  const agent = agentDescription(summary);
  const result = tagText(text, 'result');
  const event = tagText(text, 'event');
  const title = agent ?? (summary || firstLine(stripTags(text)));
  const body = result ?? event ?? (agent ? summary : '');
  return {
    kind: agent !== null ? 'subagent' : 'task',
    title,
    body: body === title ? '' : body,
    status: normalizeStatus(tagText(text, 'status')),
    agent,
  };
}

/** Body of a hand-back report: the text after the frame, de-indented. */
function handBackBody(inner: string): string {
  const marker = /The report follows:\s*\n/.exec(inner);
  const report = marker ? inner.slice(marker.index + marker[0].length) : inner;
  const lines = report.split('\n');
  const indent = Math.min(
    ...lines.filter((l) => l.trim() !== '').map((l) => /^ */.exec(l)![0].length),
  );
  return lines
    .map((l) => l.slice(Number.isFinite(indent) ? indent : 0))
    .join('\n')
    .trim();
}

function agentMessage(text: string): Activity {
  const inner = tagText(text, 'agent-message') ?? tagText(text, 'cross-session-message') ?? text;
  const handback = text.includes('[Subagent hand-back]');
  const body = handback
    ? handBackBody(inner.replace(/^[\s\S]*?\[Subagent hand-back\]/, ''))
    : stripTags(inner.replace(/^Another Claude session sent a message:?/, ''));
  return {
    kind: handback ? 'handback' : 'peer',
    title: firstLine(body.replace(/^[#>*\s-]+/, '')),
    body,
    status: handback ? 'completed' : null,
    agent: null,
  };
}

function command(text: string): Activity {
  const name = tagText(text, 'command-name');
  if (name !== null) {
    const args = tagText(text, 'command-args') ?? '';
    return {
      kind: 'command',
      title: `${name} ${args}`.trim(),
      body: '',
      status: null,
      agent: null,
    };
  }
  const input = tagText(text, 'bash-input');
  if (input !== null) {
    return { kind: 'command', title: `! ${input}`, body: '', status: null, agent: null };
  }
  const body = stripTags(text);
  const title = firstLine(body);
  return { kind: 'command', title, body: body === title ? '' : body, status: null, agent: null };
}

/** Structured view of a system message (or a misattributed user message). */
export function classifyActivity(raw: string): Activity {
  const text = raw.trim();
  if (text.startsWith('<task-notification')) return taskNotification(text);
  if (AGENT_MESSAGE.test(text) || text.startsWith('Another Claude session sent a message')) {
    return agentMessage(text);
  }
  if (text.startsWith('This session is being continued from a previous conversation')) {
    return { kind: 'summary', title: firstLine(text), body: text, status: null, agent: null };
  }
  if (/^<(command-|local-command-|bash-)/.test(text)) return command(text);
  if (text.startsWith('[') && !text.includes('\n') && text.length <= 200) {
    return { kind: 'marker', title: text, body: '', status: null, agent: null };
  }
  const body = stripTags(text);
  const title = firstLine(body);
  return { kind: 'system', title, body: body === title ? '' : body, status: null, agent: null };
}
