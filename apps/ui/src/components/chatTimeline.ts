/**
 * Builds the chat view timeline from a session's event stream plus the
 * prompts this client sent (local echo). Pure, so it is easy to unit-test.
 */
import { stripAnsi } from '../protocol/helpers';
import type { SessionEvent } from '../protocol/types';
import type { SentAttachment, SentPrompt } from '../state/reducer';
import {
  type Activity,
  type ActivityStatus,
  classifyActivity,
  isMachineText,
} from './chatActivity';

/** A transcript `message` (role user) within this window replaces the local echo. */
export const ECHO_DEDUPE_WINDOW_MS = 120_000;

/** Longest agent output bubble; older text in a group is elided. */
export const MAX_OUTPUT_CHARS = 4000;

/** Tool names that start a sub-agent (Claude Code `Task` / `Agent`). */
export const SUBAGENT_TOOLS = new Set(['Task', 'Agent']);

/** Result of an asynchronous sub-agent launch: it keeps running in the background. */
const ASYNC_LAUNCH = /^Async agent launched/;

export type ChatItem =
  | {
      kind: 'event';
      key: string;
      event: Exclude<SessionEvent, { type: 'output' }>;
      /** Files sent with this prompt (the transcript echo of a local prompt). */
      attachments?: SentAttachment[];
    }
  | { kind: 'sent'; key: string; prompt: SentPrompt }
  | { kind: 'output'; key: string; text: string; truncated: boolean }
  /** System or agent activity the person did not type (collapsible). */
  | { kind: 'activity'; key: string; activity: Activity }
  /** A sub-agent started by a Task/Agent tool call, updated with its outcome. */
  | {
      kind: 'subagent';
      key: string;
      tool: string;
      description: string;
      status: ActivityStatus;
      /** Final report, once known. */
      report: string;
    }
  /** Output of a tool call (collapsible). */
  | { kind: 'tool_result'; key: string; tool: string | null; text: string; isError: boolean };

type SubagentItem = Extract<ChatItem, { kind: 'subagent' }>;

export interface TimelineOptions {
  /**
   * Render terminal output as plain-text agent bubbles. Use it for agents
   * without a chat transcript; transcript agents redraw a full-screen TUI
   * whose raw output is not readable as text.
   */
  includeOutput: boolean;
}

function normalize(text: string): string {
  return text.trim().replace(/\s+/g, ' ');
}

/** Claude Code's wrapper around long pasted text: `<pasted_content id="1">...`. */
const PASTED_BLOCK =
  /<pasted[_-]content\b[^>]*>\r?\n?([\s\S]*?)\r?\n?<\/pasted[_-]content\b[^>]*>/g;

/** What the server puts ahead of a prompt for its files (`batch_mention`). */
const ATTACHMENT_MENTION = /^Attached (file|folder|\d+ files)/;

/** A user message as the person wrote it, without the agent's paste wrapper. */
function unwrapPasted(text: string): string {
  return text.replace(PASTED_BLOCK, '$1');
}

/** Whether `message` (a transcript user message) is what `prompt` became. */
function echoesPrompt(message: string, prompt: string): boolean {
  const received = normalize(unwrapPasted(message));
  const sent = normalize(prompt);
  return received === sent || (ATTACHMENT_MENTION.test(received) && received.endsWith(sent));
}

/** The transcript message that echoes a local prompt, if any. */
function transcriptEcho(prompt: SentPrompt, events: SessionEvent[]): SessionEvent | undefined {
  return events.find(
    (e) =>
      e.type === 'message' &&
      e.role === 'user' &&
      (prompt.afterSeq === null || e.seq > prompt.afterSeq) &&
      Math.abs(e.at - prompt.at) <= ECHO_DEDUPE_WINDOW_MS &&
      echoesPrompt(e.text, prompt.text),
  );
}

const decoder = new TextDecoder();

function concat(chunks: Uint8Array[]): Uint8Array {
  if (chunks.length === 1) return chunks[0]!;
  const out = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
  let offset = 0;
  for (const c of chunks) {
    out.set(c, offset);
    offset += c.length;
  }
  return out;
}

/**
 * Plain text of a run of terminal output: ANSI stripped, blank runs collapsed
 * and, right after a prompt, the terminal's echo of that prompt removed.
 */
export function outputText(chunks: Uint8Array[], echoOf: string | null): string {
  // Decode the run as a whole so multi-byte characters split across chunks survive.
  const raw = decoder.decode(concat(chunks)).replace(/\r\n?/g, '\n');
  const lines = stripAnsi(raw)
    .split('\n')
    .map((l) => l.trimEnd());
  if (echoOf !== null) {
    const echo = normalize(echoOf);
    const first = lines.findIndex((l) => l.trim() !== '');
    if (first >= 0 && echo !== '' && normalize(lines[first]!).endsWith(echo)) {
      lines.splice(0, first + 1);
    }
  }
  return lines
    .join('\n')
    .replace(/\n{3,}/g, '\n\n')
    .replace(/^\n+|\n+$/g, '');
}

/** States that change with every turn (see `buildTimeline`). */
const ROUTINE_STATES: ReadonlySet<string> = new Set(['running', 'idle', 'awaiting_input']);

export function buildTimeline(
  events: SessionEvent[],
  sent: SentPrompt[],
  { includeOutput }: TimelineOptions,
): ChatItem[] {
  // Prompts the transcript echoes are shown once, as the transcript message
  // with the words and the files the person sent.
  const echoed = new Map<number, SentPrompt>();
  const prompts = sent.filter((p) => {
    const echo = transcriptEcho(p, events);
    if (echo) echoed.set(echo.seq, p);
    return !echo;
  });
  const items: ChatItem[] = [];
  let run: { chunks: Uint8Array[]; key: string; echoOf: string | null } | null = null;
  let lastPrompt: string | null = null;
  // Tool calls waiting for their result, oldest first.
  const pending: { name: string; subagent: SubagentItem | null }[] = [];
  const subagents: SubagentItem[] = [];

  const flush = () => {
    if (!run) return;
    let text = outputText(run.chunks, run.echoOf);
    const truncated = text.length > MAX_OUTPUT_CHARS;
    if (truncated) text = text.slice(text.length - MAX_OUTPUT_CHARS);
    if (text) items.push({ kind: 'output', key: run.key, text, truncated });
    run = null;
  };
  const pushPrompt = (prompt: SentPrompt) => {
    flush();
    items.push({ kind: 'sent', key: `sent-${prompt.id}`, prompt });
    lastPrompt = prompt.text;
  };

  // Prompts are anchored by the last seq seen when they were sent: that is
  // immune to clock skew between this device and the server.
  let p = 0;
  for (const event of events) {
    while (p < prompts.length && (prompts[p]!.afterSeq ?? -Infinity) < event.seq) {
      pushPrompt(prompts[p]!);
      p += 1;
    }
    if (event.type === 'output') {
      if (!includeOutput) continue;
      if (!run) {
        run = { chunks: [], key: `out-${event.seq}`, echoOf: lastPrompt };
        lastPrompt = null;
      }
      run.chunks.push(event.data);
      continue;
    }
    flush();
    // Only the first output run after a prompt can contain its echo.
    if (event.type === 'message' || event.type === 'decision_requested') lastPrompt = null;
    // Routine turn states are not chat lines: they repeat with every turn
    // (a conductor woken by events flips them all day) and the progress line
    // and the header already show them. Starting, paused, exited and the
    // like still mark the conversation.
    if (event.type === 'state_changed' && ROUTINE_STATES.has(event.state.type)) continue;
    const key = `ev-${event.seq}`;
    if (event.type === 'tool_call') {
      if (SUBAGENT_TOOLS.has(event.name)) {
        const item: SubagentItem = {
          kind: 'subagent',
          key,
          tool: event.name,
          description: event.summary,
          status: 'started',
          report: '',
        };
        items.push(item);
        pending.push({ name: event.name, subagent: item });
        subagents.push(item);
      } else {
        items.push({ kind: 'event', key, event });
        pending.push({ name: event.name, subagent: null });
      }
      continue;
    }
    if (event.type === 'message' && event.role === 'tool') {
      // Results arrive in call order, so the oldest open call owns this one.
      const call = pending.shift();
      const isError = event.text.startsWith('error:');
      if (call?.subagent) {
        const agent = call.subagent;
        if (isError) {
          agent.status = 'failed';
          agent.report = event.text;
        } else if (ASYNC_LAUNCH.test(event.text)) {
          agent.status = 'running';
        } else {
          agent.status = 'completed';
          agent.report = event.text;
        }
        continue;
      }
      items.push({ kind: 'tool_result', key, tool: call?.name ?? null, text: event.text, isError });
      continue;
    }
    if (
      event.type === 'message' &&
      (event.role === 'system' || (event.role === 'user' && isMachineText(event.text)))
    ) {
      const activity = classifyActivity(event.text);
      if (activity.kind === 'subagent' && activity.agent !== null) {
        // A background sub-agent finished: update the row of its launch too.
        let agent: SubagentItem | undefined;
        for (let i = subagents.length - 1; i >= 0 && !agent; i--) {
          if (subagents[i]!.description === activity.agent) agent = subagents[i];
        }
        if (agent) {
          agent.status = activity.status ?? 'completed';
          if (activity.body) agent.report = activity.body;
        }
      }
      items.push({ kind: 'activity', key, activity });
      continue;
    }
    // A new prompt starts a new turn: calls left open were interrupted.
    if (event.type === 'message' && event.role === 'user') pending.length = 0;
    if (event.type === 'message' && event.role === 'user') {
      const prompt = echoed.get(event.seq);
      const text = prompt ? prompt.text : unwrapPasted(event.text);
      const files = prompt?.attachments;
      items.push({
        kind: 'event',
        key,
        event: text === event.text ? event : { ...event, text },
        ...(files?.length ? { attachments: files } : {}),
      });
      continue;
    }
    items.push({ kind: 'event', key, event });
  }
  flush();
  for (; p < prompts.length; p++) pushPrompt(prompts[p]!);
  return items;
}
