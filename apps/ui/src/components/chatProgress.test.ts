import { describe, expect, it } from 'vitest';

import type { SentPrompt } from '../state/reducer';
import { localSession } from '../test/fixtures';
import {
  activityLine,
  chatProgress,
  elapsedParts,
  SUBMIT_SLACK_MS,
  UNSEEN_PROMPT_MS,
} from './chatProgress';

const NOW = 1_800_000_000_000;

const prompt = (overrides: Partial<SentPrompt> = {}): SentPrompt => ({
  id: 'p1',
  text: 'fix the build',
  at: NOW - 2_000,
  afterSeq: null,
  status: 'delivered',
  ...overrides,
});

describe('chatProgress', () => {
  it('shows a running turn with its activity, timed from the press of Send', () => {
    const session = localSession({
      state: { type: 'running' },
      progress: {
        started_at: NOW - 1_500,
        activity: { type: 'tool', name: 'Read', summary: '/repo/src/main.rs' },
      },
    });
    expect(chatProgress(session, [prompt()], NOW)).toEqual({
      phase: 'working',
      since: NOW - 2_000,
      activity: { type: 'tool', name: 'Read', summary: '/repo/src/main.rs' },
      until: null,
    });
    // A prompt sent long before the turn did not start it.
    const old = prompt({ at: NOW - 1_500 - SUBMIT_SLACK_MS - 1 });
    expect(chatProgress(session, [old], NOW)?.since).toBe(NOW - 1_500);
    // Turns started at the PC show without a prompt from this device.
    expect(chatProgress(session, undefined, NOW)?.phase).toBe('working');
  });

  it('waits for the agent after a send, for a while', () => {
    const session = localSession({ state: { type: 'idle' } });
    const sending = prompt({ status: 'sending', at: NOW - 100 });
    expect(chatProgress(session, [sending], NOW)).toEqual({
      phase: 'waiting',
      since: NOW - 100,
      activity: null,
      until: NOW - 100 + UNSEEN_PROMPT_MS,
    });
    expect(chatProgress(session, [prompt({ at: NOW - UNSEEN_PROMPT_MS })], NOW)).toBeNull();
    expect(chatProgress(session, [prompt({ status: 'failed' })], NOW)).toBeNull();
  });

  it('shows nothing once the turn is over, or while a decision waits', () => {
    const ended = localSession({
      state: { type: 'idle' },
      progress: { started_at: NOW - 9_000, ended_at: NOW - 1_000 },
    });
    expect(chatProgress(ended, [prompt({ status: 'acknowledged' })], NOW)).toBeNull();
    expect(chatProgress(ended, [], NOW)).toBeNull();
    const deciding = localSession({
      state: { type: 'awaiting_decision' },
      progress: { started_at: NOW - 9_000 },
    });
    expect(chatProgress(deciding, [prompt()], NOW)).toBeNull();
  });
});

describe('elapsedParts', () => {
  it('uses seconds, minutes and hours', () => {
    expect(elapsedParts(42_400)).toEqual({ key: 'chat.progress.seconds', vars: { s: '42' } });
    expect(elapsedParts(185_000)).toEqual({
      key: 'chat.progress.minutes',
      vars: { m: '3', s: '05' },
    });
    expect(elapsedParts(3_840_000)).toEqual({
      key: 'chat.progress.hours',
      vars: { h: '1', m: '04' },
    });
    expect(elapsedParts(-5)).toEqual({ key: 'chat.progress.seconds', vars: { s: '0' } });
  });
});

describe('activityLine', () => {
  it('names the step', () => {
    expect(activityLine({ type: 'thinking' }).key).toBe('chat.progress.thinking');
    expect(activityLine({ type: 'tool', name: 'Read', summary: 'C:\\repo\\src\\main.rs' })).toEqual(
      { key: 'chat.progress.read', vars: { target: 'main.rs' } },
    );
    expect(activityLine({ type: 'tool', name: 'Bash', summary: 'cargo test\necho done' })).toEqual({
      key: 'chat.progress.run',
      vars: { target: 'cargo test' },
    });
    expect(activityLine({ type: 'tool', name: 'TodoWrite', summary: '{}' }).key).toBe(
      'chat.progress.todo',
    );
    expect(activityLine({ type: 'tool', name: 'mcp__github__search', summary: '{"q":1}' })).toEqual(
      { key: 'chat.progress.tool', vars: { tool: 'github search' } },
    );
  });
});
