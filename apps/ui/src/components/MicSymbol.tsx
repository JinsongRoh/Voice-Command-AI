import { useEffect, useId, useRef, useState, type CSSProperties } from 'react';

import { useMediaQuery } from '../hooks/useMediaQuery';
import {
  ASSEMBLY_MS,
  FLIGHTS,
  micAnimationKind,
  usesLevel,
  type MicAnimation,
} from '../state/micAnimation';
import { SYMBOL_PATHS, SYMBOL_VIEWBOX } from './brandArt';
import { REDUCED_MOTION_QUERY } from './VoiceWave';
import styles from './MicSymbol.module.css';

export type MicSymbolPhase = 'idle' | 'recording' | 'transcribing';

export interface MicSymbolProps {
  animation: MicAnimation;
  phase: MicSymbolPhase;
  /** The send motion is playing (stroke styles converge and flare). */
  sent?: boolean;
  /** A change to a non-zero value replays the light assembly (the "sent" moment, a preview tap). */
  replay?: number;
  /**
   * The live input level (RMS, 0 to 1), read once per frame while recording;
   * `null` (or no function) uses a gentle synthetic level.
   */
  level?: () => number | null;
  /** Sizes the symbol (the plate around it is the caller's). */
  logoClassName?: string;
}

/** Relative weight of each stroke for "voice-driven": the center reacts most. */
const WEIGHT = [0.62, 0.82, 1, 0.82, 0.62];
/** Bars in the "ring of bars" style. */
const RING_BARS = 20;
/** RMS of loud speech close to the mic maps to about 1. */
const LEVEL_GAIN = 7;
/** How long the particle flight runs (G), and when the canvas is cleared. */
const PARTICLE_MS = 780;
const PARTICLE_END = 1.25;

/**
 * The brand symbol inside the dictation button, moving in the chosen style
 * (`state/micAnimation.ts`). Decorative: the button carries the name and the
 * state. The plate, ring and glow belong to the caller; this draws only the
 * symbol and its effects. Light effects stay inside an inner circular clip,
 * so the button's own focus ring is never clipped.
 *
 * Costs nothing at rest: the per-frame level loop runs only while recording
 * (and only for styles that use it); the particle canvas only during its
 * flight. With `prefers-reduced-motion: reduce` the symbol is static.
 */
export function MicSymbol({
  animation,
  phase,
  sent = false,
  replay = 0,
  level,
  logoClassName,
}: MicSymbolProps) {
  const reduced = useMediaQuery(REDUCED_MOTION_QUERY);
  const kind = micAnimationKind(animation);
  const uid = useId().replace(/[^a-zA-Z0-9_-]/g, '');
  const gradientId = `vca-logo-${uid}`;
  const rootRef = useRef<HTMLSpanElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const logoRef = useRef<SVGSVGElement>(null);
  const levelRef = useRef(level);
  useEffect(() => {
    levelRef.current = level;
  });

  // The assembly plays when recording starts and on every new `replay`.
  const [seen, setSeen] = useState({ phase, replay });
  const [assembly, setAssembly] = useState({ id: 0, on: false });
  if (seen.phase !== phase || seen.replay !== replay) {
    const started = phase === 'recording' && seen.phase !== 'recording';
    const replayed = replay !== seen.replay && replay > 0;
    setSeen({ phase, replay });
    if ((started || replayed) && kind === 'light' && !reduced) {
      setAssembly((a) => ({ id: a.id + 1, on: true }));
    }
  }
  const assembling = assembly.on && kind === 'light' && !reduced;

  useEffect(() => {
    if (!assembly.on) return;
    const id = assembly.id;
    const timer = setTimeout(
      () => setAssembly((a) => (a.id === id ? { ...a, on: false } : a)),
      ASSEMBLY_MS,
    );
    return () => clearTimeout(timer);
  }, [assembly]);

  // G: particles fly from the rim into the strokes (canvas, only during the flight).
  useEffect(() => {
    if (!assembling || animation !== 'light-particles') return;
    const canvas = canvasRef.current;
    const logo = logoRef.current;
    if (!canvas || !logo) return;
    return flyParticles(canvas, logo);
  }, [assembling, animation, assembly.id]);

  // Voice level, once per frame, only while recording.
  const live = phase === 'recording' && !reduced && usesLevel(animation);
  useEffect(() => {
    const root = rootRef.current;
    if (!live || !root || typeof requestAnimationFrame !== 'function') return;
    const synthetic = syntheticLevel();
    const size = root.offsetWidth || 64;
    root.style.setProperty('--ring-r', `${(size / 2 + Math.max(5, size * 0.09)).toFixed(1)}px`);
    let value = 0.2;
    let raf = 0;
    const t0 = performance.now();
    const frame = (now: number) => {
      const raw = levelRef.current?.() ?? null;
      const target = raw === null ? synthetic() : Math.min(1, raw * LEVEL_GAIN);
      // Fast attack, slower release: speech peaks show, silence settles softly.
      value += (target - value) * (target > value ? 0.35 : 0.15);
      root.style.setProperty('--level', value.toFixed(3));
      const t = (now - t0) / 1000;
      if (animation === 'stroke-voice') {
        root.querySelectorAll<SVGPathElement>('[data-part="symbol"] path').forEach((p, k) => {
          const jitter = 0.8 + 0.2 * Math.sin(t * 11 + k * 1.7);
          const s = 0.45 + value * 0.75 * (WEIGHT[k] ?? 1) * jitter;
          p.style.transform = `scaleY(${s.toFixed(3)})`;
        });
      } else if (animation === 'stroke-ring') {
        root.querySelectorAll<HTMLSpanElement>('[data-ring-bar]').forEach((bar, k) => {
          const h = 0.08 + value * 0.22 * (0.4 + 0.6 * Math.abs(Math.sin(t * 6 + k * 0.9)));
          bar.style.height = `${(h * size).toFixed(1)}px`;
        });
      }
      raf = requestAnimationFrame(frame);
    };
    raf = requestAnimationFrame(frame);
    return () => {
      cancelAnimationFrame(raf);
      root.style.removeProperty('--level');
      root
        .querySelectorAll<SVGPathElement>('[data-part="symbol"] path')
        .forEach((p) => p.style.removeProperty('transform'));
      root
        .querySelectorAll<HTMLSpanElement>('[data-ring-bar]')
        .forEach((bar) => bar.style.removeProperty('height'));
    };
  }, [live, animation]);

  const flight = kind === 'light' ? FLIGHTS[animation] : undefined;
  const rootStyle = flight
    ? ({
        '--stagger': flight.stagger ? `${flight.stagger}ms` : undefined,
        '--ease': flight.ease,
        '--dur': flight.dur,
      } as CSSProperties)
    : undefined;
  const pathStyle = (i: number): CSSProperties | undefined => {
    if (!flight) return kind === 'light' ? ({ '--i': i } as CSSProperties) : undefined;
    const [fx, fy] = flight.from[i] ?? [0, 0];
    const [sx, sy] = flight.stretch?.[i] ?? [1, 1];
    return {
      '--i': flight.order.indexOf(i),
      '--fx': `${fx}px`,
      '--fy': `${fy}px`,
      '--fr': `${flight.rotate?.[i] ?? 0}deg`,
      '--sx': sx,
      '--sy': sy,
    } as CSSProperties;
  };

  const flowing = animation === 'stroke-flow' && phase === 'recording' && !reduced;
  const fill = flowing ? `url(#${gradientId}-flow)` : `url(#${gradientId})`;
  // The main layer carries the gradient; ghost trails are white (CSS).
  const layer = (part: 'symbol' | 'ghost', className: string | undefined, withFill = false) => (
    <g data-part={part} className={className} fill={withFill ? fill : undefined}>
      {SYMBOL_PATHS.map((d, i) => (
        <path key={i} d={d} style={pathStyle(i)} />
      ))}
    </g>
  );

  return (
    <span
      ref={rootRef}
      className={[
        styles.root,
        phase === 'recording' && !reduced ? styles.rec : '',
        assembling ? styles.play : '',
      ]
        .filter(Boolean)
        .join(' ')}
      data-testid="mic-symbol"
      data-anim={animation}
      data-kind={kind}
      data-phase={phase}
      data-sent={(sent && kind === 'stroke' && !reduced) || undefined}
      data-assembling={assembling || undefined}
      data-motion={reduced ? 'reduced' : 'full'}
      style={rootStyle}
      aria-hidden="true"
    >
      {animation === 'stroke-ring' && (
        <span className={styles.ring}>
          {Array.from({ length: RING_BARS }, (_, k) => (
            <span
              key={k}
              data-ring-bar=""
              className={styles.bar}
              style={{ '--a': `${(k * 360) / RING_BARS}deg` } as CSSProperties}
            />
          ))}
        </span>
      )}
      {animation === 'stroke-echo' &&
        [0, 1, 2].map((k) => (
          <span key={k} className={styles.echo} style={{ '--k': k } as CSSProperties}>
            <svg
              viewBox={SYMBOL_VIEWBOX}
              className={[styles.logo, logoClassName].filter(Boolean).join(' ')}
              focusable="false"
            >
              <g fill="var(--brand-cyan)">
                {SYMBOL_PATHS.map((d, i) => (
                  <path key={i} d={d} />
                ))}
              </g>
            </svg>
          </span>
        ))}
      {animation === 'stroke-glow' && <span className={styles.halo} />}
      <span className={styles.clip}>
        {kind === 'light' && <span key={`b${assembly.id}`} className={styles.burst} />}
        {animation === 'light-scan' && <span key={`s${assembly.id}`} className={styles.scan} />}
        {animation === 'light-particles' && <canvas ref={canvasRef} className={styles.canvas} />}
        <svg
          // A new assembly restarts its CSS animations on fresh nodes.
          key={kind === 'light' ? `l${assembly.id}` : 'logo'}
          ref={logoRef}
          xmlns="http://www.w3.org/2000/svg"
          viewBox={SYMBOL_VIEWBOX}
          preserveAspectRatio="xMidYMid meet"
          className={[styles.logo, logoClassName].filter(Boolean).join(' ')}
          data-logo="symbol"
          focusable="false"
          aria-hidden="true"
        >
          <defs>
            {/* Same geometry as the package's `vcaGrad`: top to bottom per stroke. */}
            <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0" style={{ stopColor: 'var(--brand-cyan)' }} />
              <stop offset="1" style={{ stopColor: 'var(--brand-blue)' }} />
            </linearGradient>
            {flowing && (
              <linearGradient id={`${gradientId}-flow`} x1="0" y1="0" x2="0" y2="1">
                <stop offset="0" style={{ stopColor: 'var(--brand-blue)' }} />
                <stop offset="0.42" style={{ stopColor: 'var(--brand-blue)' }} />
                <stop offset="0.5" stopColor="#7ff3ff" />
                <stop offset="0.58" style={{ stopColor: 'var(--brand-blue)' }} />
                <stop offset="1" style={{ stopColor: 'var(--brand-blue)' }} />
                <animateTransform
                  attributeName="gradientTransform"
                  type="translate"
                  from="0 1"
                  to="0 -1"
                  dur="1.6s"
                  repeatCount="indefinite"
                />
              </linearGradient>
            )}
          </defs>
          {kind === 'light' && layer('ghost', `${styles.ghost} ${styles.g2}`)}
          {kind === 'light' && layer('ghost', `${styles.ghost} ${styles.g1}`)}
          {layer('symbol', styles.main, true)}
        </svg>
      </span>
    </span>
  );
}

/** A gentle stand-in for the voice when the recorder has no level. */
function syntheticLevel() {
  let target = 0.35;
  return () => {
    if (Math.random() < 0.08) target = Math.random() < 0.15 ? 0.1 : 0.25 + Math.random() * 0.5;
    return target;
  };
}

/** Sampled stroke points per layout (CSS px inside the canvas). */
const pointCache = new Map<string, [number, number][]>();

function strokePoints(
  w: number,
  h: number,
  box: { x: number; y: number; width: number },
): [number, number][] {
  const key = `${w}x${h}:${box.x.toFixed(1)},${box.y.toFixed(1)},${box.width.toFixed(1)}`;
  const cached = pointCache.get(key);
  if (cached) return cached;
  const probe = document.createElement('canvas').getContext('2d');
  if (!probe) return [];
  const s = box.width / 1178;
  probe.setTransform(s, 0, 0, s, box.x, box.y);
  const paths = SYMBOL_PATHS.map((d) => new Path2D(d));
  const points: [number, number][] = [];
  const step = 1.5;
  for (let y = 0; y < h; y += step) {
    for (let x = 0; x < w; x += step) {
      const jx = x + Math.random() * 1;
      const jy = y + Math.random() * 1;
      if (paths.some((p) => probe.isPointInPath(p, jx, jy))) points.push([jx, jy]);
    }
  }
  pointCache.set(key, points);
  return points;
}

/** Runs one particle flight on `canvas`; returns its cleanup. */
function flyParticles(canvas: HTMLCanvasElement, logo: SVGSVGElement): () => void {
  if (typeof Path2D === 'undefined' || typeof requestAnimationFrame !== 'function') {
    return () => {};
  }
  const rect = canvas.getBoundingClientRect();
  const w = rect.width;
  const h = rect.height;
  const g = w > 0 && h > 0 ? canvas.getContext('2d') : null;
  if (!g) return () => {};
  const dpr = Math.min(2, window.devicePixelRatio || 1);
  canvas.width = Math.round(w * dpr);
  canvas.height = Math.round(h * dpr);
  // The logo box at its final, untransformed place inside the canvas.
  const lr = logo.getBoundingClientRect();
  const box = { x: lr.left - rect.left, y: lr.top - rect.top, width: lr.width };
  const targets = strokePoints(w, h, box);
  const cx = w / 2;
  const cy = h / 2;
  const r0 = Math.min(w, h) / 2;
  const ps = targets.map(([x, y]) => {
    const a = Math.random() * Math.PI * 2;
    const r = r0 + Math.random() * r0 * 0.2;
    return { x, y, sx: cx + Math.cos(a) * r, sy: cy + Math.sin(a) * r, d: Math.random() * 0.25 };
  });
  const t0 = performance.now();
  let raf = 0;
  const clear = () => {
    g.setTransform(1, 0, 0, 1, 0, 0);
    g.clearRect(0, 0, canvas.width, canvas.height);
  };
  const step = (now: number) => {
    const t = (now - t0) / PARTICLE_MS;
    clear();
    if (t >= PARTICLE_END) return;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    for (const p of ps) {
      const k = Math.min(1, Math.max(0, (t - p.d) / (1 - p.d)));
      const e = 1 - Math.pow(1 - k, 3);
      g.fillStyle = k < 1 ? `rgba(233,253,255,${0.35 + 0.65 * k})` : 'rgba(0,207,232,.9)';
      g.beginPath();
      g.arc(p.sx + (p.x - p.sx) * e, p.sy + (p.y - p.sy) * e, k < 1 ? 0.8 : 0.65, 0, Math.PI * 2);
      g.fill();
    }
    raf = requestAnimationFrame(step);
  };
  raf = requestAnimationFrame(step);
  return () => {
    cancelAnimationFrame(raf);
    clear();
  };
}
