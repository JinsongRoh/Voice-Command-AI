import { act, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { CONVERGE_MS, REDUCED_MOTION_QUERY, VoiceWave } from './VoiceWave';

/** Installs a matchMedia stub that reports the given reduced-motion preference. */
function mockReducedMotion(reduce: boolean) {
  const matchMedia = vi.fn((query: string) => ({
    matches: query === REDUCED_MOTION_QUERY ? reduce : false,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  }));
  vi.stubGlobal('matchMedia', matchMedia);
  return matchMedia;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe('VoiceWave', () => {
  it('animates five strokes while recording', () => {
    const matchMedia = mockReducedMotion(false);
    render(<VoiceWave mode="live" />);
    const wave = screen.getByTestId('voice-wave');
    expect(matchMedia).toHaveBeenCalledWith(REDUCED_MOTION_QUERY);
    expect(wave).toHaveAttribute('aria-hidden', 'true');
    expect(wave).toHaveAttribute('data-motion', 'full');
    expect(wave).toHaveClass('live', 'animated');
    expect(wave.querySelectorAll('rect')).toHaveLength(5);
  });

  it('shows a static waveform when reduced motion is requested', () => {
    mockReducedMotion(true);
    render(<VoiceWave mode="live" />);
    const wave = screen.getByTestId('voice-wave');
    expect(wave).toHaveAttribute('data-motion', 'reduced');
    expect(wave).toHaveClass('still');
    expect(wave).not.toHaveClass('animated');
    expect(wave.querySelectorAll('rect')).toHaveLength(5);
  });

  it('plays the converging motion once, then reports it is done', () => {
    vi.useFakeTimers();
    mockReducedMotion(false);
    const onDone = vi.fn();
    render(<VoiceWave mode="converge" onDone={onDone} />);
    expect(screen.getByTestId('voice-wave')).toHaveClass('converge', 'animated');
    expect(onDone).not.toHaveBeenCalled();
    act(() => {
      vi.advanceTimersByTime(CONVERGE_MS + 200);
    });
    expect(onDone).toHaveBeenCalledTimes(1);
  });

  it('skips the converging motion with reduced motion', () => {
    mockReducedMotion(true);
    const onDone = vi.fn();
    render(<VoiceWave mode="converge" onDone={onDone} />);
    expect(screen.queryByTestId('voice-wave')).not.toBeInTheDocument();
    expect(onDone).toHaveBeenCalledTimes(1);
  });
});
