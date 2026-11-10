import { act, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { ASSEMBLY_MS } from '../state/micAnimation';
import { MicSymbol, type MicSymbolProps } from './MicSymbol';
import { REDUCED_MOTION_QUERY } from './VoiceWave';
import styles from './MicSymbol.module.css';

/** Installs a matchMedia stub that reports the given reduced-motion preference. */
function mockReducedMotion(reduce: boolean) {
  vi.stubGlobal(
    'matchMedia',
    vi.fn((query: string) => ({
      matches: query === REDUCED_MOTION_QUERY ? reduce : false,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    })),
  );
}

/** Counts animation frames without running them. */
function mockFrames() {
  let next = 1;
  const pending = new Map<number, FrameRequestCallback>();
  vi.stubGlobal(
    'requestAnimationFrame',
    vi.fn((cb: FrameRequestCallback) => {
      pending.set(next, cb);
      return next++;
    }),
  );
  vi.stubGlobal(
    'cancelAnimationFrame',
    vi.fn((id: number) => pending.delete(id)),
  );
  return {
    pending,
    /** Runs the frames requested so far. */
    tick(now = 16) {
      const due = [...pending.entries()];
      pending.clear();
      for (const [, cb] of due) cb(now);
    },
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

const base: MicSymbolProps = { animation: 'light-drop', phase: 'idle' };

function setup(props: Partial<MicSymbolProps> = {}) {
  const view = render(<MicSymbol {...base} {...props} />);
  const root = () => screen.getByTestId('mic-symbol');
  const update = (next: Partial<MicSymbolProps>) =>
    view.rerender(<MicSymbol {...base} {...props} {...next} />);
  return { ...view, root, update };
}

describe('MicSymbol', () => {
  it('draws the five-stroke symbol with the brand gradient, decorative', () => {
    mockReducedMotion(false);
    const { root } = setup();
    expect(root()).toHaveAttribute('aria-hidden', 'true');
    const logo = root().querySelector('svg[data-logo="symbol"]');
    expect(logo?.querySelector('[data-part="symbol"]')?.getAttribute('fill')).toMatch(
      /^url\(#vca-logo-/,
    );
    expect(logo?.querySelectorAll('[data-part="symbol"] path')).toHaveLength(5);
    // Light styles carry two ghost trail layers.
    expect(logo?.querySelectorAll('[data-part="ghost"]')).toHaveLength(2);
    expect(root()).toHaveAttribute('data-kind', 'light');
    expect(root()).not.toHaveAttribute('data-assembling');
  });

  it('plays the light assembly when recording starts and again at the send', () => {
    vi.useFakeTimers();
    mockReducedMotion(false);
    const { root, update } = setup();
    update({ phase: 'recording' });
    expect(root()).toHaveAttribute('data-assembling', 'true');
    expect(root()).toHaveClass(styles.play!);
    act(() => vi.advanceTimersByTime(ASSEMBLY_MS));
    expect(root()).not.toHaveAttribute('data-assembling');
    // Stopping (transcribing) does not replay it; the send does.
    update({ phase: 'transcribing' });
    update({ phase: 'idle' });
    expect(root()).not.toHaveAttribute('data-assembling');
    update({ phase: 'idle', sent: true, replay: 1 });
    expect(root()).toHaveAttribute('data-assembling', 'true');
    // Light styles never use the strokes' converge motion.
    expect(root()).not.toHaveAttribute('data-sent');
    act(() => vi.advanceTimersByTime(ASSEMBLY_MS));
    expect(root()).not.toHaveAttribute('data-assembling');
  });

  it('gives each flying stroke its start and arrival order (E: middle first)', () => {
    mockReducedMotion(false);
    const { root } = setup();
    const paths = [...root().querySelectorAll<SVGPathElement>('[data-part="symbol"] path')];
    expect(paths.map((p) => p.style.getPropertyValue('--i'))).toEqual(['3', '1', '0', '2', '4']);
    expect(paths[0]!.style.getPropertyValue('--fy')).toBe('-1800px');
  });

  it('stroke styles move while recording and converge on send, without an assembly', () => {
    mockReducedMotion(false);
    const { root, update } = setup({ animation: 'stroke-amplitude' });
    expect(root()).toHaveAttribute('data-kind', 'stroke');
    expect(root().querySelectorAll('[data-part="ghost"]')).toHaveLength(0);
    update({ phase: 'recording' });
    expect(root()).not.toHaveAttribute('data-assembling');
    expect(root()).toHaveClass(styles.rec!);
    update({ phase: 'idle', sent: true, replay: 1 });
    expect(root()).toHaveAttribute('data-sent', 'true');
    expect(root()).not.toHaveAttribute('data-assembling');
  });

  it('reads the live level only while recording, and stops when idle', () => {
    mockReducedMotion(false);
    const frames = mockFrames();
    const level = vi.fn(() => 0.1);
    const { root, update } = setup({ animation: 'stroke-voice', level });
    expect(frames.pending.size).toBe(0);
    update({ phase: 'recording' });
    expect(frames.pending.size).toBe(1);
    frames.tick();
    expect(level).toHaveBeenCalled();
    expect(Number(root().style.getPropertyValue('--level'))).toBeGreaterThan(0);
    // Voice-driven: each stroke's height follows the level.
    const paths = root().querySelectorAll<SVGPathElement>('[data-part="symbol"] path');
    expect(paths[2]!.style.transform).toMatch(/^scaleY\(/);
    update({ phase: 'transcribing' });
    expect(frames.pending.size).toBe(0);
    expect(root().style.getPropertyValue('--level')).toBe('');
    expect(paths[2]!.style.transform).toBe('');
  });

  it('falls back to a synthetic level when the recorder has none', () => {
    mockReducedMotion(false);
    const frames = mockFrames();
    const { root } = setup({ animation: 'stroke-glow', phase: 'recording', level: () => null });
    frames.tick();
    frames.tick(32);
    expect(Number(root().style.getPropertyValue('--level'))).toBeGreaterThan(0);
  });

  it('needs no frames for styles that ignore the level', () => {
    mockReducedMotion(false);
    const frames = mockFrames();
    setup({ animation: 'stroke-wave', phase: 'recording' });
    expect(frames.pending.size).toBe(0);
  });

  it('stays static with reduced motion: no assembly, no level loop, no converge', () => {
    mockReducedMotion(true);
    const frames = mockFrames();
    const { root, update } = setup({ animation: 'light-particles' });
    expect(root()).toHaveAttribute('data-motion', 'reduced');
    update({ phase: 'recording' });
    expect(root()).not.toHaveAttribute('data-assembling');
    expect(root()).not.toHaveClass(styles.rec!);
    expect(frames.pending.size).toBe(0);
    update({ phase: 'idle', sent: true, replay: 1 });
    expect(root()).not.toHaveAttribute('data-assembling');

    update({ animation: 'stroke-ring', phase: 'idle', sent: true, replay: 2 });
    expect(root()).not.toHaveAttribute('data-sent');
  });
});
