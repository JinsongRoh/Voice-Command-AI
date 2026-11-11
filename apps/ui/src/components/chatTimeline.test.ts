import { describe, expect, it } from 'vitest';

import type { SessionEvent, SessionState } from '../protocol/types';
import type { SentPrompt } from '../state/reducer';
import { buildTimeline, MAX_OUTPUT_CHARS, outputText } from './chatTimeline';

const enc = new TextEncoder();
const out = (seq: number, text: string, at = seq * 1000): SessionEvent => ({
  type: 'output',
  seq,
  at,
  data: enc.encode(text),
});
/**
 * A state line the chat shows. The names are kept from when every state was
 * a line; routine turn states are now hidden, so markers use shown states.
 */
const state = (seq: number, type: 'running' | 'awaiting_input'): SessionEvent => ({
  type: 'state_changed',
  seq,
  at: seq * 1000,
  state: { type: type === 'running' ? 'starting' : 'paused' },
});
const prompt = (id: string, text: string, afterSeq: number | null, at = 0): SentPrompt => ({
  id,
  text,
  at,
  afterSeq,
  status: 'delivered',
});

describe('buildTimeline', () => {
  it('leaves routine turn states out of the chat, keeps the ones that mark it', () => {
    const at = (seq: number, st: SessionState) =>
      ({ type: 'state_changed', seq, at: seq, state: st }) as SessionEvent;
    const items = buildTimeline(
      [
        at(1, { type: 'running' }),
        at(2, { type: 'idle' }),
        at(3, { type: 'awaiting_input' }),
        at(4, { type: 'running' }),
        at(5, { type: 'exited', code: 0 }),
      ],
      [],
      { includeOutput: false },
    );
    expect(items.map((i) => i.key)).toEqual(['ev-5']);
  });

  it('echoes sent prompts after the event seen when they were sent', () => {
    const events = [state(1, 'awaiting_input'), state(2, 'running'), state(3, 'awaiting_input')];
    // The device clock is far ahead of the server: placement must not depend on it.
    const items = buildTimeline(events, [prompt('p1', 'hello', 1, 9e12)], {
      includeOutput: false,
    });
    expect(items.map((i) => i.key)).toEqual(['ev-1', 'sent-p1', 'ev-2', 'ev-3']);
  });

  it('appends prompts sent after the last event', () => {
    const items = buildTimeline(
      [state(1, 'running')],
      [prompt('p1', 'a', 1), prompt('p2', 'b', 1)],
      {
        includeOutput: false,
      },
    );
    expect(items.map((i) => i.key)).toEqual(['ev-1', 'sent-p1', 'sent-p2']);
  });

  it('drops the local echo once the transcript shows the same user message', () => {
    const sent = [prompt('p1', 'run  the tests', 2, 10_000)];
    const events: SessionEvent[] = [
      state(2, 'running'),
      { type: 'message', seq: 3, at: 12_000, role: 'user', text: 'run the tests\n' },
      { type: 'message', seq: 4, at: 13_000, role: 'assistant', text: 'ok' },
    ];
    const items = buildTimeline(events, sent, { includeOutput: false });
    expect(items.filter((i) => i.kind === 'sent')).toEqual([]);
    expect(items).toHaveLength(3);
  });

  it('keeps the echo when the matching message is old or different', () => {
    const sent = [prompt('p1', 'continue', 5, 1_000_000)];
    const events: SessionEvent[] = [
      // Same text, but before the prompt was sent (an earlier turn).
      { type: 'message', seq: 3, at: 999_000, role: 'user', text: 'continue' },
      // After it, but different text.
      { type: 'message', seq: 6, at: 1_001_000, role: 'user', text: 'something else' },
      // Same text, after it, but far outside the window.
      { type: 'message', seq: 7, at: 9_000_000, role: 'user', text: 'continue' },
    ];
    const items = buildTimeline(events, sent, { includeOutput: false });
    expect(items.filter((i) => i.kind === 'sent')).toHaveLength(1);
  });

  it('groups agent output between prompts when output is included', () => {
    const events = [
      out(1, '\x1b[32mready\x1b[0m\r\n> '),
      out(2, 'hello\r\n'),
      state(3, 'running'),
      out(4, 'Hi! You said '),
      out(5, 'hello.\r\n'),
      state(6, 'awaiting_input'),
    ];
    const items = buildTimeline(events, [prompt('p1', 'hello', 1)], { includeOutput: true });
    expect(items.map((i) => i.kind)).toEqual(['output', 'sent', 'event', 'output', 'event']);
    expect(items[0]).toMatchObject({ kind: 'output', text: 'ready\n>' });
    // The echoed prompt line ("hello") is removed from the reply.
    expect(items[3]).toMatchObject({ kind: 'output', text: 'Hi! You said hello.' });
  });

  it('ignores output for transcript agents', () => {
    const items = buildTimeline([out(1, 'tui noise')], [], { includeOutput: false });
    expect(items).toEqual([]);
  });

  it('truncates very long output from the start', () => {
    const items = buildTimeline([out(1, 'x'.repeat(MAX_OUTPUT_CHARS + 10))], [], {
      includeOutput: true,
    });
    expect(items[0]).toMatchObject({ kind: 'output', truncated: true });
    expect(items[0]?.kind === 'output' && items[0].text.length).toBe(MAX_OUTPUT_CHARS);
  });
});

describe('outputText', () => {
  it('decodes multi-byte characters split across chunks', () => {
    const bytes = enc.encode('안녕');
    expect(outputText([bytes.slice(0, 2), bytes.slice(2)], null)).toBe('안녕');
  });

  it('collapses blank runs and strips the prompt echo only at the start', () => {
    expect(outputText([enc.encode('\r\n$ go\r\n\r\n\r\n\r\ndone go\r\n')], 'go')).toBe('done go');
  });
});

const msg = (
  seq: number,
  role: 'user' | 'assistant' | 'system' | 'tool',
  text: string,
): SessionEvent => ({ type: 'message', seq, at: seq * 1000, role, text });
const call = (seq: number, name: string, summary: string): SessionEvent => ({
  type: 'tool_call',
  seq,
  at: seq * 1000,
  name,
  summary,
});

describe('buildTimeline activity', () => {
  it('never shows machine text as a user message', () => {
    const items = buildTimeline(
      [
        msg(1, 'user', 'Fix the login page'),
        // An older server still labels these as user messages.
        msg(
          2,
          'user',
          '<task-notification>\n<summary>Background command "x" completed</summary>\n</task-notification>',
        ),
        msg(
          3,
          'user',
          'Another Claude session sent a message:\n<agent-message from="a">\n[Subagent hand-back] The report follows:\n  Done.\n</agent-message>',
        ),
        msg(4, 'system', 'This session is being continued from a previous conversation.'),
      ],
      [],
      { includeOutput: false },
    );
    expect(items.map((i) => i.kind)).toEqual(['event', 'activity', 'activity', 'activity']);
    const users = items.filter((i) => i.kind === 'event' && i.event.type === 'message');
    expect(users).toHaveLength(1);
    expect(items.map((i) => (i.kind === 'activity' ? i.activity.kind : null))).toEqual([
      null,
      'task',
      'handback',
      'summary',
    ]);
  });

  it('folds a synchronous sub-agent result into its row', () => {
    const items = buildTimeline(
      [
        call(1, 'Bash', 'ls'),
        call(2, 'Agent', 'Review the plan'),
        msg(3, 'tool', 'a.txt'),
        msg(4, 'tool', 'Plan looks good.'),
      ],
      [],
      { includeOutput: false },
    );
    expect(items.map((i) => i.kind)).toEqual(['event', 'subagent', 'tool_result']);
    expect(items[1]).toMatchObject({
      kind: 'subagent',
      description: 'Review the plan',
      status: 'completed',
      report: 'Plan looks good.',
    });
    expect(items[2]).toMatchObject({ kind: 'tool_result', tool: 'Bash', text: 'a.txt' });
  });

  it('tracks a background sub-agent until its notification', () => {
    const items = buildTimeline(
      [
        call(1, 'Agent', 'Build env'),
        msg(2, 'tool', 'Async agent launched successfully.\nagentId: a1'),
        msg(3, 'assistant', 'Started it.'),
        msg(
          4,
          'system',
          '<task-notification>\n<status>completed</status>\n<summary>Agent "Build env" finished</summary>\n<result>All green.</result>\n</task-notification>',
        ),
      ],
      [],
      { includeOutput: false },
    );
    expect(items.map((i) => i.kind)).toEqual(['subagent', 'event', 'activity']);
    expect(items[0]).toMatchObject({ status: 'completed', report: 'All green.' });
    expect(items[2]).toMatchObject({
      kind: 'activity',
      activity: { kind: 'subagent', title: 'Build env', status: 'completed', body: 'All green.' },
    });
  });

  it('marks failed sub-agents and tool errors', () => {
    const items = buildTimeline(
      [
        call(1, 'Task', 'Fix'),
        msg(2, 'tool', 'error: boom'),
        call(3, 'Read', 'x'),
        msg(4, 'tool', 'error: missing'),
      ],
      [],
      { includeOutput: false },
    );
    expect(items[0]).toMatchObject({ kind: 'subagent', status: 'failed' });
    expect(items[2]).toMatchObject({ kind: 'tool_result', isError: true, tool: 'Read' });
  });

  it('a new prompt drops calls that never got a result', () => {
    const items = buildTimeline(
      [
        call(1, 'Agent', 'Old'),
        msg(2, 'user', 'stop that'),
        call(3, 'Bash', 'ls'),
        msg(4, 'tool', 'ok'),
      ],
      [],
      { includeOutput: false },
    );
    expect(items[0]).toMatchObject({ kind: 'subagent', status: 'started' });
    expect(items[3]).toMatchObject({ kind: 'tool_result', tool: 'Bash' });
  });
});

describe('buildTimeline attachments', () => {
  it('moves the files of an echoed prompt onto the transcript message', () => {
    const files = [{ name: 'shot.png', kind: 'file' as const, thumbUrl: 'blob:1' }];
    const sentPrompt = { ...prompt('p1', 'look at this', 1, 1000), attachments: files };
    const echo: SessionEvent = {
      type: 'message',
      seq: 2,
      at: 1200,
      role: 'user',
      text: 'look at this',
    };
    const items = buildTimeline([echo], [sentPrompt], { includeOutput: false });
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: 'event', attachments: files });
    // Not echoed (yet): the local bubble carries them.
    const local = buildTimeline([], [sentPrompt], { includeOutput: false });
    expect(local[0]).toMatchObject({ kind: 'sent', prompt: { attachments: files } });
  });

  const userMessage = (text: string): SessionEvent => ({
    type: 'message',
    seq: 2,
    at: 1200,
    role: 'user',
    text,
  });
  const files = [{ name: 'a.pdf', kind: 'file' as const, thumbUrl: null }];

  it('recognises a long prompt the agent received as a pasted block with its files', () => {
    const body = 'first line\nsecond line';
    const sentPrompt = { ...prompt('p1', body, 1, 1000), attachments: files };
    const echo = userMessage(
      `Attached files (2): @.vca/a.pdf @.vca/b.md\n\n<pasted_content id="55fd">\n${body}\n</pasted_content id="55fd">\n`,
    );
    const items = buildTimeline([echo], [sentPrompt], { includeOutput: false });
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      kind: 'event',
      attachments: files,
      event: { text: body },
    });
  });

  it('recognises a long prompt without files', () => {
    const echo = userMessage('<pasted_content id="1">\nlong text\n</pasted_content id="1">');
    const items = buildTimeline([echo], [prompt('p1', 'long text', 1, 1000)], {
      includeOutput: false,
    });
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: 'event', event: { text: 'long text' } });
  });

  it('recognises a short prompt behind its attachment mention', () => {
    const sentPrompt = { ...prompt('p1', 'look at this', 1, 1000), attachments: files };
    const items = buildTimeline(
      [userMessage('Attached 23 files: @.vca/b/ (list: @.vca/b/list.txt) look at this')],
      [sentPrompt],
      { includeOutput: false },
    );
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: 'event', attachments: files });
  });

  it('recognises a prompt of files only', () => {
    const sentPrompt = { ...prompt('p1', '', 1, 1000), attachments: files };
    const items = buildTimeline([userMessage('Attached file: @.vca/a.pdf')], [sentPrompt], {
      includeOutput: false,
    });
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: 'event', attachments: files });
  });

  it('keeps a prompt that only ends like another message', () => {
    const items = buildTimeline(
      [userMessage('please run the tests')],
      [prompt('p1', 'run the tests', 1, 1000)],
      { includeOutput: false },
    );
    expect(items.map((i) => i.kind)).toEqual(['sent', 'event']);
  });

  it('shows a pasted prompt from history without its wrapper', () => {
    const items = buildTimeline(
      [userMessage('Attached file: @a\n\n<pasted_content id="9">\nbody\n</pasted_content>')],
      [],
      { includeOutput: false },
    );
    expect(items[0]).toMatchObject({
      kind: 'event',
      event: { text: 'Attached file: @a\n\nbody' },
    });
  });
});
