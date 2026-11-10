import { describe, expect, it } from 'vitest';

import type { ArrangeWindow, RestoreMonitor } from '../protocol/types';
import {
  GRID_PRESETS,
  arrangeable,
  autoFill,
  cellsTooSmall,
  fillOrder,
  fromPlans,
  gridOf,
  monitorMap,
  monitorName,
  orderWindows,
  place,
  placedCount,
  prune,
  setGrid,
  toPlans,
  unplace,
  type ArrangeDraft,
} from './arrange';

function win(id: string, session: string | null, folder = 'E:\\a', agent = 'claude_code') {
  return {
    id,
    title: id,
    monitor: null,
    maximized: false,
    minimized: false,
    sessions: session ? [{ id: session, title: session, agent: { type: agent }, folder }] : [],
  } as ArrangeWindow;
}

const M = 'M';

describe('the draft', () => {
  it('offers the agreed presets', () => {
    expect(GRID_PRESETS.map((p) => `${p.count}=${p.rows}x${p.cols}`)).toEqual([
      '1=1x1',
      '2=1x2',
      '4=2x2',
      '6=2x3',
      '8=2x4',
      '10=2x5',
    ]);
    for (const p of GRID_PRESETS) expect(p.rows * p.cols).toBe(p.count);
  });

  it('puts a window in a cell and moves it, one place at a time', () => {
    let draft = place({}, M, 1, 'a');
    expect(gridOf(draft, M).cells).toEqual({ 1: 'a' });
    draft = place(draft, 'N', 0, 'a');
    expect(gridOf(draft, M).cells).toEqual({});
    expect(gridOf(draft, 'N').cells).toEqual({ 0: 'a' });
  });

  it('swaps two windows that are both in cells', () => {
    let draft = place(place({}, M, 0, 'a'), M, 3, 'b');
    draft = place(draft, M, 0, 'b');
    expect(gridOf(draft, M).cells).toEqual({ 0: 'b', 3: 'a' });
  });

  it('sends a window back to the list when one from the list takes its cell', () => {
    const draft = place(place({}, M, 0, 'a'), M, 0, 'b');
    expect(gridOf(draft, M).cells).toEqual({ 0: 'b' });
  });

  it('keeps the order of the windows that still fit when the grid changes', () => {
    let draft = place(place(place({}, M, 3, 'c'), M, 0, 'a'), M, 2, 'b');
    draft = setGrid(draft, M, 1, 2);
    expect(gridOf(draft, M)).toEqual({ rows: 1, cols: 2, cells: { 0: 'a', 1: 'b' } });
  });

  it('forgets windows that closed and takes a window out', () => {
    const draft = place(place({}, M, 0, 'a'), M, 1, 'b');
    expect(prune(draft, new Set(['a', 'b']))).toBe(draft);
    expect(gridOf(prune(draft, new Set(['b'])), M).cells).toEqual({ 1: 'b' });
    expect(gridOf(unplace(draft, 'a'), M).cells).toEqual({ 1: 'b' });
  });

  it('becomes a plan of the monitors that hold a window', () => {
    const draft = setGrid(place({}, M, 2, 'a'), 'N', 1, 1);
    expect(toPlans(draft)).toEqual([
      { monitor: M, rows: 2, cols: 2, cells: [{ cell: 2, window: 'a' }] },
    ]);
  });
});

describe('automatic filling', () => {
  const order = ['s1', 's2', 's3', 's4'];

  it('follows the session order, windows without a session last', () => {
    const windows = [win('c', 's3'), win('none', null), win('a', 's1'), win('b', 's2')];
    const draft = autoFill({}, M, windows, order, 'none');
    expect(gridOf(draft, M).cells).toEqual({ 0: 'a', 1: 'b', 2: 'c', 3: 'none' });
  });

  it('leaves windows on another monitor alone and stops at the last cell', () => {
    const windows = ['s1', 's2', 's3', 's4'].map((s, i) => win(`w${i}`, s));
    let draft: ArrangeDraft = place({}, 'N', 0, 'w0');
    draft = setGrid(draft, M, 1, 2);
    draft = autoFill(draft, M, windows, order, 'none');
    expect(gridOf(draft, M).cells).toEqual({ 0: 'w1', 1: 'w2' });
    expect(gridOf(draft, 'N').cells).toEqual({ 0: 'w0' });
  });

  it('keeps the windows of one folder together', () => {
    const windows = [
      win('a1', 's1', 'E:\\a'),
      win('b1', 's2', 'E:\\b'),
      win('a2', 's3', 'e:\\A\\'),
      win('b2', 's4', 'E:\\b'),
    ];
    expect(orderWindows(windows, order, 'folder').map((w) => w.id)).toEqual([
      'a1',
      'a2',
      'b1',
      'b2',
    ]);
  });

  it('keeps the windows of one agent together', () => {
    const windows = [
      win('a1', 's1', 'E:\\a', 'claude_code'),
      win('b1', 's2', 'E:\\b', 'codex'),
      win('none', null),
      win('a2', 's3', 'E:\\c', 'claude_code'),
    ];
    expect(orderWindows(windows, order, 'agent').map((w) => w.id)).toEqual([
      'a1',
      'a2',
      'b1',
      'none',
    ]);
  });

  it('never breaks a group at the end of a row', () => {
    // Three of a group in a 2×2 grid would otherwise end in the far corner.
    const windows = [
      win('a1', 's1', 'E:\\a'),
      win('a2', 's2', 'E:\\a'),
      win('a3', 's3', 'E:\\a'),
      win('b1', 's4', 'E:\\b'),
    ];
    const { cells } = gridOf(autoFill({}, M, windows, order, 'folder'), M);
    expect(cells).toEqual({ 0: 'a1', 1: 'a2', 3: 'a3', 2: 'b1' });
  });

  it('walks every grid through touching cells when grouped', () => {
    for (const [rows, cols] of [
      [2, 5],
      [3, 3],
      [6, 8],
    ] as const) {
      const cells = fillOrder(rows, cols, true);
      expect(new Set(cells).size).toBe(rows * cols);
      cells.slice(1).forEach((cell, i) => {
        const from = cells[i]!;
        const distance =
          Math.abs(Math.floor(cell / cols) - Math.floor(from / cols)) +
          Math.abs((cell % cols) - (from % cols));
        expect(distance).toBe(1);
      });
    }
    expect(fillOrder(2, 2, false)).toEqual([0, 1, 2, 3]);
  });
});

describe('the drawing', () => {
  const monitor = (x: number, y: number, w: number, h: number): RestoreMonitor => ({
    id: `\\\\.\\D${x}`,
    rect: { x, y, w, h },
    work: { x, y, w, h: h - 40 },
    dpi: 96,
  });

  it('draws monitors to scale around negative coordinates', () => {
    const map = monitorMap([monitor(0, 0, 1920, 1080), monitor(-1920, 0, 1920, 1080)]);
    expect(map.aspect).toBeCloseTo(3840 / 1080);
    expect(map.boxes[0]).toMatchObject({ left: 50, top: 0, width: 50, height: 100 });
    expect(map.boxes[1]).toMatchObject({ left: 0, top: 0, width: 50 });
  });

  it('names monitors and warns about tiny cells', () => {
    expect(monitorName('\\\\.\\DISPLAY2')).toBe('DISPLAY2');
    const small = monitor(0, 0, 1366, 768);
    expect(cellsTooSmall(small, 2, 2)).toBe(false);
    expect(cellsTooSmall(small, 2, 5)).toBe(true);
    expect(cellsTooSmall(small, 4, 2)).toBe(true);
  });
});

describe('what the tab offers and loads', () => {
  it('offers windows with an AI agent, not plain terminals or plain shells', () => {
    const shell = win('s', 'x', 'E:\\a', 'shell');
    expect(arrangeable(win('a', 'claude'))).toBe(true);
    expect(arrangeable(win('p', null))).toBe(false);
    expect(arrangeable(shell)).toBe(false);
    const mixed = { ...shell, sessions: [...shell.sessions, ...win('c', 'y').sessions] };
    expect(arrangeable(mixed)).toBe(true);
  });

  it('turns saved grids into a draft of the windows open now', () => {
    const draft = fromPlans(
      [
        {
          monitor: M,
          rows: 1,
          cols: 2,
          cells: [
            { cell: 1, window: 'a' },
            { cell: 0, window: 'gone' },
            { cell: 5, window: 'b' },
          ],
        },
        { monitor: 'N', rows: 2, cols: 2, cells: [] },
      ],
      new Set(['a', 'b']),
    );
    expect(draft).toEqual({
      [M]: { rows: 1, cols: 2, cells: { 1: 'a' } },
      N: { rows: 2, cols: 2, cells: {} },
    });
    expect(placedCount(draft)).toBe(1);
  });
});
