/**
 * Builds the chat view timeline from a session's event stream plus the
 * prompts this client sent (local echo). Pure, so it is easy to unit-test.
 */
import { stripAnsi } from '../protocol/helpers';
import type { SessionEvent } from '../protocol/types';
import type { SentPrompt } from '../state/reducer';

/** A transcript `message` (role user) within this window replaces the local echo. */
export const ECHO_DEDUPE_WINDOW_MS = 120_000;

/** Longest agent output bubble; older text in a group is elided. */
export const MAX_OUTPUT_CHARS = 4000;

export type ChatItem =
  | { kind: 'event'; key: string; event: Exclude<SessionEvent, { type: 'output' }> }
  | { kind: 'sent'; key: string; prompt: SentPrompt }
  | { kind: 'output'; key: string; text: string; truncated: boolean };

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

/** Whether a transcript user message already shows this locally echoed prompt. */
function echoedByTranscript(prompt: SentPrompt, events: SessionEvent[]): boolean {
  const text = normalize(prompt.text);
  return events.some(
    (e) =>
      e.type === 'message' &&
      e.role === 'user' &&
      (prompt.afterSeq === null || e.seq > prompt.afterSeq) &&
      Math.abs(e.at - prompt.at) <= ECHO_DEDUPE_WINDOW_MS &&
      normalize(e.text) === text,
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

export function buildTimeline(
  events: SessionEvent[],
  sent: SentPrompt[],
  { includeOutput }: TimelineOptions,
): ChatItem[] {
  const prompts = sent.filter((p) => !echoedByTranscript(p, events));
  const items: ChatItem[] = [];
  let run: { chunks: Uint8Array[]; key: string; echoOf: string | null } | null = null;
  let lastPrompt: string | null = null;

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
    items.push({ kind: 'event', key: `ev-${event.seq}`, event });
  }
  flush();
  for (; p < prompts.length; p++) pushPrompt(prompts[p]!);
  return items;
}
