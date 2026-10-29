import { describe, expect, it } from 'vitest';

import type { SessionEvent } from '../protocol/types';
import type { SentPrompt } from '../state/reducer';
import { buildTimeline, MAX_OUTPUT_CHARS, outputText } from './chatTimeline';

const enc = new TextEncoder();
const out = (seq: number, text: string, at = seq * 1000): SessionEvent => ({
  type: 'output',
  seq,
  at,
  data: enc.encode(text),
});
const state = (seq: number, type: 'running' | 'awaiting_input'): SessionEvent => ({
  type: 'state_changed',
  seq,
  at: seq * 1000,
  state: { type },
});
const prompt = (id: string, text: string, afterSeq: number | null, at = 0): SentPrompt => ({
  id,
  text,
  at,
  afterSeq,
  status: 'delivered',
});

describe('buildTimeline', () => {
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
