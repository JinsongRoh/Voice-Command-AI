import { describe, expect, it } from 'vitest';

import {
  MAX_HISTORY,
  canRedo,
  canUndo,
  commit,
  emptyHistory,
  exportScale,
  hasContent,
  imageDocSize,
  initialSketch,
  sketchFileName,
  sketchReducer,
  toolWidth,
  type SketchItem,
  type SketchState,
} from './model';

const dot = (x: number): SketchItem => ({
  kind: 'stroke',
  tool: 'pen',
  color: 'black',
  width: 5,
  points: [x, x, 0.5],
  pressure: false,
});

function apply(state: SketchState, ...actions: Parameters<typeof sketchReducer>[1][]) {
  return actions.reduce(sketchReducer, state);
}

describe('sketch tool state', () => {
  it('starts with a pen in the ink that contrasts with the theme page', () => {
    expect(initialSketch('light')).toMatchObject({
      tool: 'pen',
      color: 'black',
      background: 'white',
      width: 1,
    });
    expect(initialSketch('dark')).toMatchObject({ color: 'white', background: 'dark' });
  });

  it('switches tools, colors and widths', () => {
    const s = apply(
      initialSketch('light'),
      { type: 'tool', tool: 'arrow' },
      { type: 'color', color: 'red' },
      { type: 'width', width: 2 },
    );
    expect(s).toMatchObject({ tool: 'arrow', color: 'red', width: 2 });
  });

  it('picking a color while erasing goes back to the pen', () => {
    const s = apply(
      initialSketch('light'),
      { type: 'tool', tool: 'eraser' },
      { type: 'color', color: 'blue' },
    );
    expect(s.tool).toBe('pen');
    expect(apply(s, { type: 'tool', tool: 'rect' }, { type: 'color', color: 'green' }).tool).toBe(
      'rect',
    );
  });

  it('keeps the default ink readable when the page changes', () => {
    const s = apply(initialSketch('light'), { type: 'background', background: 'dark' });
    expect(s.color).toBe('white');
    // A color the user chose stays.
    const red = apply(
      initialSketch('light'),
      { type: 'color', color: 'red' },
      { type: 'background', background: 'dark' },
    );
    expect(red.color).toBe('red');
    expect(apply(s, { type: 'background', background: 'grid' }).color).toBe('black');
  });

  it('takes the canvas size once for a blank page', () => {
    const s = apply(
      initialSketch('light'),
      { type: 'size', width: 390, height: 600 },
      { type: 'size', width: 844, height: 300 },
    );
    expect([s.docWidth, s.docHeight]).toEqual([390, 600]);
  });

  it('a photo starts annotate mode at the photo size, in red, with a fresh history', () => {
    const s = apply(
      initialSketch('light'),
      { type: 'add', item: dot(1) },
      { type: 'image', width: 1600, height: 1200 },
    );
    expect(s).toMatchObject({ annotate: true, docWidth: 1600, docHeight: 1200, color: 'red' });
    expect(s.history).toEqual(emptyHistory);
    expect(hasContent(s)).toBe(true);
  });

  it('widths depend on the tool', () => {
    expect(toolWidth('pen', 1)).toBe(5);
    expect(toolWidth('highlighter', 1)).toBeGreaterThan(toolWidth('pen', 1));
    expect(toolWidth('eraser', 0)).toBeGreaterThan(toolWidth('pen', 0));
  });
});

describe('sketch undo and redo', () => {
  it('undoes and redoes in order, and a new item drops the redo stack', () => {
    let s = apply(
      initialSketch('light'),
      { type: 'add', item: dot(1) },
      { type: 'add', item: dot(2) },
      { type: 'add', item: dot(3) },
    );
    expect(s.history.present).toHaveLength(3);
    s = apply(s, { type: 'undo' }, { type: 'undo' });
    expect(s.history.present).toEqual([dot(1)]);
    expect(canRedo(s.history)).toBe(true);
    s = apply(s, { type: 'redo' });
    expect(s.history.present).toEqual([dot(1), dot(2)]);
    s = apply(s, { type: 'add', item: dot(9) });
    expect(s.history.present).toEqual([dot(1), dot(2), dot(9)]);
    expect(canRedo(s.history)).toBe(false);
  });

  it('undo and redo at the ends do nothing', () => {
    const s = initialSketch('light');
    expect(apply(s, { type: 'undo' })).toEqual(s);
    expect(apply(s, { type: 'redo' })).toEqual(s);
    expect(canUndo(s.history)).toBe(false);
  });

  it('clear is one undoable step (and a no-op on an empty page)', () => {
    const empty = initialSketch('light');
    expect(apply(empty, { type: 'clear' }).history).toBe(empty.history);
    let s = apply(empty, { type: 'add', item: dot(1) }, { type: 'add', item: dot(2) });
    s = apply(s, { type: 'clear' });
    expect(s.history.present).toEqual([]);
    expect(hasContent(s)).toBe(false);
    s = apply(s, { type: 'undo' });
    expect(s.history.present).toEqual([dot(1), dot(2)]);
  });

  it('keeps a bounded history', () => {
    let h = emptyHistory;
    for (let i = 0; i < MAX_HISTORY + 20; i++) h = commit(h, [...h.present, dot(i)]);
    expect(h.past).toHaveLength(MAX_HISTORY);
    expect(h.present).toHaveLength(MAX_HISTORY + 20);
  });
});

describe('sketch export sizing', () => {
  it('caps photos at 2048 px on the long edge', () => {
    expect(imageDocSize(4032, 3024)).toEqual({ width: 2048, height: 1536 });
    expect(imageDocSize(800, 600)).toEqual({ width: 800, height: 600 });
  });

  it('exports blank pages at screen density, never above 2048 px', () => {
    expect(exportScale(390, 700, false, 3)).toBe(2);
    expect(exportScale(1280, 700, false, 2)).toBeCloseTo(2048 / 1280);
    expect(exportScale(1280, 700, false, 1)).toBe(1);
    expect(exportScale(2048, 1536, true, 3)).toBe(1);
  });

  it('names files by local time', () => {
    expect(sketchFileName(new Date(2026, 9, 31, 9, 5, 7))).toBe('sketch-20261031-090507.png');
    expect(sketchFileName(new Date(2026, 0, 2, 23, 59, 0), 'jpg')).toBe(
      'sketch-20260102-235900.jpg',
    );
  });
});
