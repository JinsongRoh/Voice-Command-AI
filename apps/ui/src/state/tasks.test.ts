import { describe, expect, it } from 'vitest';

import { translate } from '../i18n/i18n';
import { mockTask } from '../protocol/mockTasks';
import type { TaskSettings } from '../protocol/tasks';
import { alertsAndTasksCount, initialState, reducer } from './reducer';
import {
  applyTaskMessage,
  blockingOpen,
  commandFromMessage,
  DEFAULT_FILTER,
  digestDue,
  filterTasks,
  localDay,
  reminderLine,
  titleFromMessage,
  waitingOnYou,
} from './tasks';

const SETTINGS: TaskSettings = {
  extract: true,
  extract_llm: false,
  stale_hours: 8,
  remind_blocking_minutes: 30,
  digest_hour: 9,
};

describe('task state', () => {
  it('keeps the list in order and merges saved tasks', () => {
    const low = mockTask({ id: 'a', title: 'A', priority: 'low' });
    const high = mockTask({ id: 'b', title: 'B', priority: 'high' });
    const done = mockTask({ id: 'c', title: 'C', status: 'done', updated_at: 5 });
    // A saved task before the first list waits for the list.
    expect(applyTaskMessage(null, { type: 'task_saved', task: low })).toBeNull();
    const book = applyTaskMessage(null, {
      type: 'tasks',
      tasks: [done, low, high],
      settings: SETTINGS,
    })!;
    expect(book.list.map((t) => t.id)).toEqual(['b', 'a', 'c']);
    const next = applyTaskMessage(book, {
      type: 'task_saved',
      task: { ...low, status: 'done', updated_at: 9 },
    })!;
    expect(next.list.map((t) => t.id)).toEqual(['b', 'a', 'c']);
    expect(next.list[1]!.status).toBe('done');
    const added = applyTaskMessage(next, {
      type: 'task_result_sent',
      task: mockTask({ id: 'd', title: 'D' }),
      text: '',
      sent: true,
    })!;
    expect(added.list).toHaveLength(4);
  });

  it('counts decisions and open blocking tasks on one badge', () => {
    const now = Date.now();
    let state = reducer(initialState, {
      type: 'server',
      msg: {
        type: 'tasks',
        settings: SETTINGS,
        tasks: [
          mockTask({ id: 'b', title: 'Blocking', blocking: true, session_id: 's1' }),
          mockTask({ id: 'z', title: 'Snoozed', blocking: true, snoozed_until: now + 60_000 }),
          mockTask({ id: 'd', title: 'Done', blocking: true, status: 'done' }),
          mockTask({ id: 'n', title: 'Plain' }),
        ],
      },
    });
    expect(blockingOpen(state.tasks).map((t) => t.id)).toEqual(['b']);
    expect(alertsAndTasksCount(state)).toBe(1);
    expect(waitingOnYou(state.tasks, 's1')).toBe(1);
    expect(waitingOnYou(state.tasks, 's2')).toBe(0);
    state = reducer(state, {
      type: 'server',
      msg: {
        type: 'task_reminder',
        reminder: {
          kind: 'due',
          task_id: 'b',
          title: 'Blocking',
          session_id: 's1',
          open: 3,
          blocking: 1,
          at: 1,
        },
      },
    });
    expect(state.tasks?.reminder?.kind).toBe('due');
    // A machine switch forgets the list.
    expect(reducer(state, { type: 'reset' }).tasks).toBeNull();
  });

  it('filters', () => {
    const list = [
      mockTask({ id: 'a', title: 'Run setup', session_id: 's1' }),
      mockTask({
        id: 'b',
        title: 'Admin',
        command: {
          text: 'net stop x',
          shell: 'cmd',
          needs_admin: true,
          danger: 'caution',
          reasons: [],
          spans: [],
        },
      }),
      mockTask({ id: 'c', title: 'Old', status: 'dismissed' }),
    ];
    const ids = (f: Partial<typeof DEFAULT_FILTER>) =>
      filterTasks(list, { ...DEFAULT_FILTER, ...f }).map((t) => t.id);
    expect(ids({})).toEqual(['a', 'b']);
    expect(ids({ status: 'done' })).toEqual(['c']);
    expect(ids({ status: 'all', sessionId: '' })).toEqual(['b', 'c']);
    expect(ids({ adminOnly: true })).toEqual(['b']);
    expect(ids({ query: 'NET STOP' })).toEqual(['b']);
  });

  it('speaks generic reminder lines, never the title', () => {
    const t = (k: Parameters<typeof translate>[1], v?: Record<string, string | number>) =>
      translate('ko', k, v);
    const reminder = {
      kind: 'blocking' as const,
      task_id: 'x',
      title: 'rm -rf C:\\secret',
      session_id: 's1',
      open: 2,
      blocking: 1,
      at: 0,
    };
    const line = reminderLine(t, reminder, 2);
    expect(line).toBe('2번 세션에서 할 일을 기다리고 있습니다.');
    expect(line).not.toContain('secret');
    expect(reminderLine(t, { ...reminder, kind: 'stale' }, null)).toContain('2개');
  });

  it('reads the digest once a day after its hour', () => {
    const store = new Map<string, string>();
    const storage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
    };
    const morning = new Date(2026, 10, 1, 8, 0);
    const later = new Date(2026, 10, 1, 9, 30);
    expect(digestDue(SETTINGS, morning, storage)).toBe(false);
    expect(digestDue(SETTINGS, later, storage)).toBe(true);
    storage.setItem('vca.tasks.digestDay', localDay(later));
    expect(digestDue(SETTINGS, later, storage)).toBe(false);
    expect(digestDue({ ...SETTINGS, digest_hour: null }, later, storage)).toBe(false);
  });

  it('fills a task from a chat message', () => {
    const text = '## Next step\n\nRun this in an admin PowerShell:\n```powershell\nSet-X 1\n```';
    expect(titleFromMessage(text)).toBe('Next step');
    expect(commandFromMessage(text)).toBe('Set-X 1');
    expect(commandFromMessage('no code')).toBeNull();
    expect(titleFromMessage('x'.repeat(200)).length).toBe(120);
  });
});
