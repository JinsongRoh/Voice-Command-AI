import { describe, expect, it } from 'vitest';

import {
  DEFAULT_MIC_ANIMATION,
  FLIGHTS,
  MIC_ANIMATION_CODES,
  MIC_ANIMATION_GROUPS,
  MIC_ANIMATIONS,
  isMicAnimation,
  micAnimationKind,
  usesLevel,
} from './micAnimation';
import { defaultSettings, parseSettings } from './settings';

describe('mic animation styles', () => {
  it('defaults to E (drop from above) for new users', () => {
    expect(DEFAULT_MIC_ANIMATION).toBe('light-drop');
    expect(defaultSettings().micAnimation).toBe('light-drop');
  });

  it('lists all 18 styles: light assembly E, G, A, B, C, D, F, H, then strokes 1 to 10', () => {
    expect(MIC_ANIMATIONS).toHaveLength(18);
    expect(MIC_ANIMATIONS.map((id) => MIC_ANIMATION_CODES[id])).toEqual([
      'E',
      'G',
      'A',
      'B',
      'C',
      'D',
      'F',
      'H',
      ...Array.from({ length: 10 }, (_, i) => String(i + 1)),
    ]);
    expect(MIC_ANIMATION_GROUPS.map((g) => [g.kind, g.items.length])).toEqual([
      ['light', 8],
      ['stroke', 10],
    ]);
    expect(MIC_ANIMATION_GROUPS.flatMap((g) => g.items)).toEqual([...MIC_ANIMATIONS]);
  });

  it('knows which styles fly and which follow the voice', () => {
    // The CSS flights (A–F); G and H draw their own entrance.
    expect(Object.keys(FLIGHTS).sort()).toEqual(
      [
        'light-burst',
        'light-drop',
        'light-rise',
        'light-sides',
        'light-spiral',
        'light-swoosh',
      ].sort(),
    );
    // E arrives in the prototype's mixed order: the middle stroke first.
    expect(FLIGHTS['light-drop']?.order).toEqual([2, 1, 3, 0, 4]);
    expect(
      MIC_ANIMATIONS.filter((id) => usesLevel(id) && micAnimationKind(id) === 'stroke'),
    ).toEqual(['stroke-voice', 'stroke-glow', 'stroke-spread', 'stroke-ring']);
    expect(MIC_ANIMATIONS.filter((id) => micAnimationKind(id) === 'light').every(usesLevel)).toBe(
      true,
    );
  });

  it('keeps a stored style and falls back to E when it is missing or unknown', () => {
    const defaults = defaultSettings();
    const stored = (v: unknown) => JSON.stringify({ micMode: 'hold', micAnimation: v });
    expect(parseSettings(stored('stroke-ring'), defaults).micAnimation).toBe('stroke-ring');
    expect(parseSettings(stored('light-particles'), defaults).micAnimation).toBe('light-particles');
    // Saved before the setting existed: existing users get E too.
    expect(parseSettings(JSON.stringify({ micMode: 'hold' }), defaults)).toMatchObject({
      micMode: 'hold',
      micAnimation: 'light-drop',
    });
    for (const bad of ['v3', 'amplitude', '', 7, null, { id: 'light-drop' }]) {
      expect(parseSettings(stored(bad), defaults).micAnimation).toBe('light-drop');
    }
    expect(isMicAnimation('stroke-amplitude')).toBe(true);
    expect(isMicAnimation('toString')).toBe(false);
  });
});
