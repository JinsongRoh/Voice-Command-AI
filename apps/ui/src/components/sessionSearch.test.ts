import { describe, expect, it } from 'vitest';

import { reducer, initialState } from '../state/reducer';
import { localSession } from '../test/fixtures';
import { groupSessions, leadingMention, mentionQuery, searchSessions } from './sessionSearch';
import { fitFontSize, MAX_FONT_PX, MIN_FONT_PX, pinchZoom, stepZoom } from './terminalFit';
import { crossSendBlock } from './crossSend';
import { openShellIn, sameFolder } from './ShellTab';

const a = localSession({ id: 'a', display_name: 'payments-api', last_activity: 10 });
const b = localSession({
  id: 'b',
  display_name: 'Web Frontend',
  last_activity: 30,
  state: { type: 'idle' },
  origin: { type: 'local', agent: { type: 'codex' }, project_dir: 'E:\\work\\site', pid: 1 },
});
const c = localSession({
  id: 'c',
  display_name: 'api',
  last_activity: 20,
  state: { type: 'awaiting_decision' },
});

describe('session search', () => {
  it('matches names and folders, names first', () => {
    expect(searchSessions([a, b, c], 'api').map((s) => s.id)).toEqual(['c', 'a']);
    expect(searchSessions([a, b, c], 'site').map((s) => s.id)).toEqual(['b']);
    expect(searchSessions([a, b, c], 'WEB front').map((s) => s.id)).toEqual(['b']);
  });

  it('groups attention, pinned, recent and all, each session once', () => {
    const groups = groupSessions([a, b, c], ['a'], '');
    expect(groups.map((g) => [g.section, g.sessions.map((s) => s.id)])).toEqual([
      ['attention', ['c']],
      ['pinned', ['a']],
      ['recent', ['b']],
    ]);
    expect(groupSessions([a, b, c], [], 'zzz')).toEqual([]);
  });

  it('finds the @ word at the start and the leading mention', () => {
    expect(mentionQuery('@pay', 4)).toBe('pay');
    expect(mentionQuery('  @', 3)).toBe('');
    expect(mentionQuery('hi @pay', 7)).toBeNull();
    expect(leadingMention('@Web Frontend run it', [a, b, c])).toEqual({
      session: b,
      rest: 'run it',
    });
    // Whole names only, longest first.
    expect(leadingMention('@api-gateway x', [c])).toBeNull();
    expect(leadingMention('@payments-api go', [a, c])?.session.id).toBe('a');
  });
});

describe('cross-session send guards', () => {
  it('refuses ended, watched-only and waiting targets; shells need one line', () => {
    expect(crossSendBlock(undefined, 'x', true)).toBe('gone');
    expect(crossSendBlock(a, '  ', true)).toBe('empty');
    expect(crossSendBlock({ ...a, state: { type: 'exited', code: 0 } }, 'x', true)).toBe('exited');
    expect(crossSendBlock({ ...a, control: 'observe_only' }, 'x', true)).toBe('observe');
    expect(crossSendBlock(c, 'x', true)).toBe('decision');
    expect(crossSendBlock(a, 'x', true)).toBeNull();
    const shell = localSession({
      origin: { type: 'local', agent: { type: 'shell', name: 'bash' }, project_dir: '/p', pid: 1 },
      state: { type: 'idle' },
    });
    expect(crossSendBlock(shell, 'ls', true)).toBeNull();
    expect(crossSendBlock(shell, 'ls\nrm x', true)).toBe('shell');
    expect(crossSendBlock(shell, 'ls', false)).toBe('shell');
  });
});

describe('project shell lookup', () => {
  it('compares folders across slashes and Windows case', () => {
    expect(sameFolder('E:\\Work\\Site\\', 'e:/work/site')).toBe(true);
    expect(sameFolder('/home/Dev', '/home/dev')).toBe(false);
    const open = localSession({
      id: 's1',
      origin: {
        type: 'local',
        agent: { type: 'shell', name: 'cmd' },
        project_dir: 'E:/work/site',
        pid: 1,
      },
      created_at: 5,
    });
    const closed = {
      ...open,
      id: 's0',
      state: { type: 'exited' as const, code: 0 },
      created_at: 9,
    };
    expect(openShellIn({ s1: open, s0: closed }, 'E:\\work\\site')?.id).toBe('s1');
  });
});

describe('phone terminal sizing', () => {
  it('scales the font so the PC columns fit the width, within limits', () => {
    expect(fitFontSize(720, 120, 0.6)).toBe(10);
    expect(fitFontSize(720, 120, 0.6, 2)).toBe(20);
    expect(fitFontSize(300, 120, 0.6)).toBe(MIN_FONT_PX);
    expect(fitFontSize(5000, 120, 0.6)).toBe(MAX_FONT_PX);
    expect(fitFontSize(0)).toBe(MIN_FONT_PX);
  });

  it('pinches and steps the zoom', () => {
    expect(pinchZoom(1, 100, 200)).toBe(2);
    expect(pinchZoom(1, 100, 1)).toBe(0.5);
    expect(stepZoom(1, 1)).toBe(1.25);
    expect(stepZoom(1.25, -1)).toBe(1);
  });
});

describe('ended and closing sessions in the store', () => {
  const ended = { ...a, state: { type: 'exited' as const, code: 0 } };
  const loaded = reducer(initialState, {
    type: 'server',
    msg: { type: 'sessions', sessions: [ended, b] },
  });

  it('hides a removed session until the server drops it or it runs again', () => {
    const hidden = reducer(loaded, { type: 'hide_session', id: 'a' });
    expect(hidden.order).toEqual(['b']);
    const relisted = reducer(hidden, {
      type: 'server',
      msg: { type: 'sessions', sessions: [ended, b] },
    });
    expect(relisted.order).toEqual(['b']);
    const running = reducer(relisted, {
      type: 'server',
      msg: { type: 'session_updated', session: { ...a, state: { type: 'running' } } },
    });
    expect(running.order).toContain('a');
    expect(running.hidden).toEqual([]);
    // Only ended sessions can be hidden.
    expect(reducer(loaded, { type: 'hide_session', id: 'b' })).toBe(loaded);
  });

  it('tracks the idle-close notice until the shell gets input or ends', () => {
    const shell = localSession({
      id: 'sh',
      origin: { type: 'local', agent: { type: 'shell', name: 'bash' }, project_dir: '/p', pid: 1 },
      shell: {
        idle_timeout_secs: 60,
        idle_deadline: Date.now() + 30_000,
        hard_deadline: Date.now() + 9e6,
        opened_by: 'x',
      },
    });
    let s = reducer(initialState, { type: 'server', msg: { type: 'sessions', sessions: [shell] } });
    s = reducer(s, {
      type: 'server',
      msg: { type: 'shell_closing_soon', session_id: 'sh', seconds_left: 30 },
    });
    expect(s.closingSoon.sh).toBeGreaterThan(Date.now());
    expect(s.closingFinal).not.toContain('sh');
    // A lifetime closing cannot be extended: Keep open is not offered.
    const capped = reducer(s, {
      type: 'server',
      msg: { type: 'shell_closing_soon', session_id: 'sh', seconds_left: 20, reason: 'lifetime' },
    });
    expect(capped.closingFinal).toContain('sh');
    s = reducer(s, {
      type: 'server',
      msg: {
        type: 'session_updated',
        session: { ...shell, shell: { ...shell.shell!, idle_deadline: Date.now() + 600_000 } },
      },
    });
    expect(s.closingSoon.sh).toBeUndefined();
  });
});
