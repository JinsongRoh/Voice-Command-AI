import { describe, expect, it } from 'vitest';

import { COLORS } from './model';
import { fitView, inkColor, pageTone, textHalo } from './render';

describe('sketch rendering helpers', () => {
  it('black and white ink never vanish on the page', () => {
    expect(inkColor('white', 'light')).toBe(COLORS.black);
    expect(inkColor('black', 'dark')).toBe(COLORS.white);
    expect(inkColor('white', 'dark')).toBe(COLORS.white);
    // On a photo the chosen color is kept.
    expect(inkColor('white', 'photo')).toBe(COLORS.white);
    expect(inkColor('red', 'light')).toBe(COLORS.red);
  });

  it('derives the page tone and the text halo', () => {
    expect(pageTone('grid', false)).toBe('light');
    expect(pageTone('dark', false)).toBe('dark');
    expect(pageTone('dark', true)).toBe('photo');
    expect(textHalo('yellow', 'photo')).not.toBe(textHalo('red', 'photo'));
  });

  it('fits the document into the stage, centered', () => {
    expect(fitView(400, 300, 400, 300)).toEqual({ scale: 1, offsetX: 0, offsetY: 0 });
    const v = fitView(2000, 1000, 400, 400);
    expect(v.scale).toBeCloseTo(0.2);
    expect(v.offsetY).toBeCloseTo(100);
  });
});
