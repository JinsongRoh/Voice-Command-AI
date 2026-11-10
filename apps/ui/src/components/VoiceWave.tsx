import { useEffect, useId, useRef } from 'react';

import { useMediaQuery } from '../hooks/useMediaQuery';
import styles from './VoiceWave.module.css';

export const REDUCED_MOTION_QUERY = '(prefers-reduced-motion: reduce)';

/** How long the one-shot converging motion runs (keep in sync with the CSS). */
export const CONVERGE_MS = 520;

/**
 * Resting heights of the five strokes, echoing the symbol's rhythm: the outer
 * strokes reach furthest, the center converges.
 */
const STROKES = [
  { x: 1, h: 12 },
  { x: 7, h: 16 },
  { x: 13, h: 20 },
  { x: 19, h: 16 },
  { x: 25, h: 12 },
];
const WIDTH = 30;
const HEIGHT = 22;
const STROKE_W = 4;

export interface VoiceWaveProps {
  /**
   * `live`: recording, the stroke amplitudes move smoothly.
   * `converge`: played once after sending; the strokes gather to the center.
   */
  mode: 'live' | 'converge';
  /** Called when the converging motion has finished (or was skipped). */
  onDone?: () => void;
  className?: string;
}

/**
 * The five-stroke voice waveform (cyan to blue, like the symbol). Decorative:
 * the recording state is always announced by text and the timer next to it.
 *
 * With `prefers-reduced-motion: reduce` the amplitudes do not move (a static
 * waveform marks the recording) and the converging motion is skipped.
 */
export function VoiceWave({ mode, onDone, className }: VoiceWaveProps) {
  const reduced = useMediaQuery(REDUCED_MOTION_QUERY);
  const gradientId = `vca-wave-${useId().replace(/[^a-zA-Z0-9_-]/g, '')}`;
  const doneRef = useRef(onDone);
  useEffect(() => {
    doneRef.current = onDone;
  });

  // The converging motion ends on its own; a timer backs up `animationend`
  // (which never fires when animations are disabled or the view is hidden).
  useEffect(() => {
    if (mode !== 'converge') return;
    if (reduced) {
      doneRef.current?.();
      return;
    }
    const timer = setTimeout(() => doneRef.current?.(), CONVERGE_MS + 120);
    return () => clearTimeout(timer);
  }, [mode, reduced]);

  if (mode === 'converge' && reduced) return null;

  const animated = !reduced;
  const classes = [styles.wave, styles[mode], animated ? styles.animated : styles.still, className]
    .filter(Boolean)
    .join(' ');

  return (
    <svg
      className={classes}
      viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
      aria-hidden="true"
      focusable="false"
      data-testid="voice-wave"
      data-mode={mode}
      data-motion={reduced ? 'reduced' : 'full'}
    >
      <defs>
        <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" style={{ stopColor: 'var(--brand-cyan)' }} />
          <stop offset="1" style={{ stopColor: 'var(--brand-blue)' }} />
        </linearGradient>
      </defs>
      <g fill={`url(#${gradientId})`}>
        {STROKES.map((s, i) => (
          <rect
            key={i}
            className={styles.stroke}
            x={s.x}
            y={(HEIGHT - s.h) / 2}
            width={STROKE_W}
            height={s.h}
            rx={STROKE_W / 2}
            style={{ ['--i' as string]: i, ['--dx' as string]: `${13 - s.x}px` }}
          />
        ))}
      </g>
    </svg>
  );
}
