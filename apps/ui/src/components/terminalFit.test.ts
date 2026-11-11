import { describe, expect, it } from 'vitest';

import {
  firstSizeMarker,
  fitScreenFont,
  MAX_FONT_PX,
  MIN_FONT_PX,
  parseSizeMarker,
  SIZE_OSC,
} from './terminalFit';

const marker = (cols: number, rows: number) => `\x1b]${SIZE_OSC};${cols};${rows}\x07`;

describe('PTY size marker', () => {
  it('parses the payload', () => {
    expect(parseSizeMarker('120;40')).toEqual({ cols: 120, rows: 40 });
    expect(parseSizeMarker('0;40')).toBeNull();
    expect(parseSizeMarker('120')).toBeNull();
    expect(parseSizeMarker('a;b')).toBeNull();
  });

  it('finds the first marker in the output', () => {
    expect(firstSizeMarker(['plain', `x${marker(100, 30)}y`, marker(80, 24)])).toEqual({
      cols: 100,
      rows: 30,
    });
    expect(firstSizeMarker([`\x1b]${SIZE_OSC};90;20\x1b\\`])).toEqual({ cols: 90, rows: 20 });
    expect(firstSizeMarker(['\x1b]0;title\x07', 'no marker'])).toBeNull();
    expect(firstSizeMarker([])).toBeNull();
  });
});

describe('fitScreenFont', () => {
  it('fits the PTY columns to a wide pane, capped (the desktop case)', () => {
    // A wide desktop pane must not get more columns: the font stops at 13px.
    expect(fitScreenFont({ width: 1700, height: 1200, cols: 120, rows: 40, max: 13 })).toBe(13);
  });

  it('shrinks to whichever of width and height is tighter', () => {
    // Width allows 10px (720 / (120 * 0.6)); height allows 8px (400 / (40 * 1.25)).
    expect(fitScreenFont({ width: 720, height: 400, cols: 120, rows: 40 })).toBe(8);
    expect(fitScreenFont({ width: 720, height: 2000, cols: 120, rows: 40 })).toBe(10);
  });

  it('rounds down so the screen is never wider than the pane', () => {
    const size = fitScreenFont({ width: 719, height: 0, cols: 120, rows: 40 });
    expect(size * 120 * 0.6).toBeLessThanOrEqual(719);
  });

  it('applies zoom and the limits', () => {
    expect(fitScreenFont({ width: 720, height: 0, cols: 120, rows: 40, zoom: 2 })).toBe(20);
    expect(fitScreenFont({ width: 100, height: 0, cols: 120, rows: 40 })).toBe(MIN_FONT_PX);
    expect(fitScreenFont({ width: 9000, height: 0, cols: 120, rows: 40 })).toBe(MAX_FONT_PX);
    expect(fitScreenFont({ width: 0, height: 0, cols: 120, rows: 40, min: 8 })).toBe(8);
  });
});
