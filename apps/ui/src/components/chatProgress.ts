/**
 * The chat's progress line while the agent works on a prompt: "Working ·
 * 42s" with its current step ("Reading main.rs", "Thinking…"), or "Waiting
 * for the agent" between sending a prompt and the agent taking it.
 *
 * The server reports the turn (`SessionInfo.progress`, from the agent's
 * hooks and its transcript); this client adds the moment it sent its own
 * prompt, so the line shows at once and its timer counts from the send.
 */
import type { MessageKey } from '../i18n/i18n';
import type { SessionInfo, TurnActivity } from '../protocol/types';
import type { SentPrompt } from '../state/reducer';

/** How long a sent prompt the agent has not taken yet shows "Waiting" (ms). */
export const UNSEEN_PROMPT_MS = 60_000;
/** A prompt sent this long before the turn's start started it (ms). */
export const SUBMIT_SLACK_MS = 30_000;

export interface ChatProgressInfo {
  /** `working`: a turn runs. `waiting`: a sent prompt the agent has not taken yet. */
  phase: 'working' | 'waiting';
  /** Start of the timer (ms). */
  since: number;
  activity: TurnActivity | null;
  /** The line goes away at this time (ms) unless new information comes; null: no limit. */
  until: number | null;
}

/** The newest prompt this client sent, unless it failed. */
function lastSent(sent: readonly SentPrompt[] | undefined): SentPrompt | null {
  const last = sent && sent.length > 0 ? sent[sent.length - 1] : undefined;
  return last && last.status !== 'failed' ? last : null;
}

/**
 * The progress line for `session`, or null when the agent is not at work.
 * Without `now` an expired "waiting" line is still returned: the line hides
 * itself at `until` (render stays pure).
 */
export function chatProgress(
  session: SessionInfo,
  sent: readonly SentPrompt[] | undefined,
  now: number | null = null,
): ChatProgressInfo | null {
  const state = session.state.type;
  const turn = session.progress ?? null;
  const last = lastSent(sent);
  if (turn && !turn.ended_at && state === 'running') {
    let since = turn.started_at;
    // The timer counts from the press of Send when that started the turn.
    if (last && last.at <= since && since - last.at <= SUBMIT_SLACK_MS) since = last.at;
    return { phase: 'working', since, activity: turn.activity ?? null, until: null };
  }
  // Seen by the agent (or failed): the turn above tells the rest.
  if (!last || last.status === 'acknowledged') return null;
  if (
    state !== 'running' &&
    state !== 'idle' &&
    state !== 'awaiting_input' &&
    state !== 'starting'
  ) {
    return null;
  }
  const until = last.at + UNSEEN_PROMPT_MS;
  if (now !== null && now >= until) return null;
  return { phase: 'waiting', since: last.at, activity: null, until };
}

/** "42s", "3m 05s" or "1h 04m" (the units are translated). */
export function elapsedParts(ms: number): { key: MessageKey; vars: Record<string, string> } {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => String(n).padStart(2, '0');
  if (h > 0) return { key: 'chat.progress.hours', vars: { h: String(h), m: pad(m) } };
  if (m > 0) return { key: 'chat.progress.minutes', vars: { m: String(m), s: pad(s) } };
  return { key: 'chat.progress.seconds', vars: { s: String(s) } };
}

/** The last segment of a file path. */
function fileName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, '');
  const at = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'));
  return at >= 0 ? trimmed.slice(at + 1) : trimmed;
}

/** One line, at most `max` characters. */
function short(text: string, max = 60): string {
  const line = (text.split(/\r?\n/)[0] ?? '').trim();
  const chars = [...line];
  return chars.length <= max ? line : `${chars.slice(0, max - 1).join('')}…`;
}

const READ_TOOLS = new Set(['Read', 'NotebookRead']);
const EDIT_TOOLS = new Set(['Edit', 'MultiEdit', 'NotebookEdit', 'apply_patch']);
const WRITE_TOOLS = new Set(['Write']);
const RUN_TOOLS = new Set(['Bash', 'PowerShell', 'shell', 'exec_command', 'local_shell']);
const SEARCH_TOOLS = new Set(['Grep', 'Glob', 'LS']);
const SUBAGENT_TOOLS = new Set(['Task', 'Agent']);
const TODO_TOOLS = new Set(['TodoWrite', 'update_plan']);

/** The activity as a translatable line: "Reading main.rs", "Running cargo test", ... */
export function activityLine(activity: TurnActivity): {
  key: MessageKey;
  vars: Record<string, string>;
} {
  if (activity.type === 'thinking') return { key: 'chat.progress.thinking', vars: {} };
  const { name, summary } = activity;
  const target = short(summary);
  const file = short(fileName(summary), 48);
  if (TODO_TOOLS.has(name)) return { key: 'chat.progress.todo', vars: {} };
  if (target) {
    if (READ_TOOLS.has(name)) return { key: 'chat.progress.read', vars: { target: file } };
    if (EDIT_TOOLS.has(name)) return { key: 'chat.progress.edit', vars: { target: file } };
    if (WRITE_TOOLS.has(name)) return { key: 'chat.progress.write', vars: { target: file } };
    if (RUN_TOOLS.has(name)) return { key: 'chat.progress.run', vars: { target } };
    if (SEARCH_TOOLS.has(name)) return { key: 'chat.progress.search', vars: { target } };
    if (name === 'WebFetch') return { key: 'chat.progress.fetch', vars: { target } };
    if (name === 'WebSearch') return { key: 'chat.progress.webSearch', vars: { target } };
    if (SUBAGENT_TOOLS.has(name)) return { key: 'chat.progress.delegate', vars: { target } };
  }
  // MCP tools read better without their prefix: `mcp__github__search` → `github search`.
  const tool = name.startsWith('mcp__') ? name.slice(5).replace(/__/g, ' ') : name;
  return { key: 'chat.progress.tool', vars: { tool } };
}
