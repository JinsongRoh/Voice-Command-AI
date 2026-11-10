/**
 * How the brand symbol on the dictation (mic) button moves. Two families:
 *
 * - `light-*` ("light assembly"): each of the symbol's five strokes is a light
 *   that flies in from outside the circle and snaps into the logo, with ghost
 *   trails, a flash and a ring burst. Plays when recording starts and again
 *   at the "sent" moment; while recording the symbol shimmers with the voice.
 * - `stroke-*` ("stroke motion"): the strokes move while recording; "sent"
 *   plays the converge/flare motion.
 *
 * The list order is the picker order (the CEO's ranking first).
 */
export const MIC_ANIMATIONS = [
  'light-drop',
  'light-particles',
  'light-sides',
  'light-rise',
  'light-swoosh',
  'light-spiral',
  'light-burst',
  'light-scan',
  'stroke-amplitude',
  'stroke-voice',
  'stroke-wave',
  'stroke-center',
  'stroke-flow',
  'stroke-glow',
  'stroke-writing',
  'stroke-echo',
  'stroke-spread',
  'stroke-ring',
] as const;

export type MicAnimation = (typeof MIC_ANIMATIONS)[number];
export type MicAnimationKind = 'light' | 'stroke';

/** Default for new users and for stored values that are missing or unknown. */
export const DEFAULT_MIC_ANIMATION: MicAnimation = 'light-drop';

export function isMicAnimation(value: unknown): value is MicAnimation {
  return typeof value === 'string' && (MIC_ANIMATIONS as readonly string[]).includes(value);
}

export function micAnimationKind(id: MicAnimation): MicAnimationKind {
  return id.startsWith('light-') ? 'light' : 'stroke';
}

export const MIC_ANIMATION_GROUPS: { kind: MicAnimationKind; items: MicAnimation[] }[] = [
  { kind: 'light', items: MIC_ANIMATIONS.filter((id) => micAnimationKind(id) === 'light') },
  { kind: 'stroke', items: MIC_ANIMATIONS.filter((id) => micAnimationKind(id) === 'stroke') },
];

/** The prototype's label for each style (A–H, 1–10), shown as a small code. */
export const MIC_ANIMATION_CODES: Record<MicAnimation, string> = {
  'light-drop': 'E',
  'light-particles': 'G',
  'light-sides': 'A',
  'light-rise': 'B',
  'light-swoosh': 'C',
  'light-spiral': 'D',
  'light-burst': 'F',
  'light-scan': 'H',
  'stroke-amplitude': '1',
  'stroke-voice': '2',
  'stroke-wave': '3',
  'stroke-center': '4',
  'stroke-flow': '5',
  'stroke-glow': '6',
  'stroke-writing': '7',
  'stroke-echo': '8',
  'stroke-spread': '9',
  'stroke-ring': '10',
};

/** Styles that follow the live input level while recording (plus every light style's shimmer). */
const VOICE_REACTIVE = new Set<MicAnimation>([
  'stroke-voice',
  'stroke-glow',
  'stroke-spread',
  'stroke-ring',
]);

/** Whether the style reads the input level each frame while recording. */
export function usesLevel(id: MicAnimation): boolean {
  return micAnimationKind(id) === 'light' || VOICE_REACTIVE.has(id);
}

/** Start distance of a flying stroke, in symbol units (just outside the circle). */
const U = 1800;

/** Per-stroke start of a light-assembly flight (CSS custom properties). */
export interface Flight {
  /** Order in which the strokes arrive, per stroke index. */
  order: number[];
  /** Start offset `[x, y]` per stroke, in symbol units. */
  from: [number, number][];
  /** Start rotation per stroke, degrees. */
  rotate?: number[];
  /** Start stretch per stroke `[sx, sy]`. */
  stretch?: [number, number][];
  stagger?: number;
  ease?: string;
  dur?: string;
}

const same = <T>(v: T): T[] => [v, v, v, v, v];
const IDENTITY_ORDER = [0, 1, 2, 3, 4];

/**
 * The flights of the CSS light styles, from the prototype (light-assemble
 * template, variants A–F). G (particles) and H (scan) do not fly.
 */
export const FLIGHTS: Partial<Record<MicAnimation, Flight>> = {
  // A: from all sides, stretched along the direction of travel.
  'light-sides': (() => {
    const from: [number, number][] = [
      [-U, -U * 0.55],
      [-U * 0.5, U],
      [0, -U],
      [U * 0.5, U],
      [U, -U * 0.55],
    ];
    return {
      order: IDENTITY_ORDER,
      from,
      stretch: from.map(([x, y]): [number, number] =>
        Math.abs(x) > Math.abs(y) ? [1.6, 1] : [1, 1.6],
      ),
    };
  })(),
  // B: rise from below.
  'light-rise': {
    order: IDENTITY_ORDER,
    from: same<[number, number]>([0, U]),
    stretch: same<[number, number]>([1, 2.4]),
    stagger: 55,
  },
  // C: sideways whoosh.
  'light-swoosh': {
    order: IDENTITY_ORDER,
    from: same<[number, number]>([-U * 1.3, 0]),
    stretch: same<[number, number]>([3, 1]),
    stagger: 45,
    ease: 'cubic-bezier(.12,.85,.2,1.06)',
  },
  // D: spiral in along the circle.
  'light-spiral': {
    order: IDENTITY_ORDER,
    from: IDENTITY_ORDER.map((i): [number, number] => {
      const a = ((i * 72 + 200) * Math.PI) / 180;
      return [Math.round(Math.cos(a) * U), Math.round(Math.sin(a) * U)];
    }),
    rotate: IDENTITY_ORDER.map((i) => -160 + i * 20),
  },
  // E: drop from above with a bounce, in a mixed order.
  'light-drop': {
    order: [2, 1, 3, 0, 4],
    from: same<[number, number]>([0, -U]),
    stretch: same<[number, number]>([1, 1.8]),
    ease: 'cubic-bezier(.3,1.5,.5,1)',
    dur: '.7s',
  },
  // F: burst from the center, then reassemble.
  'light-burst': {
    order: IDENTITY_ORDER,
    from: [
      [-U * 0.8, -U * 0.5],
      [-U * 0.5, U * 0.7],
      [0, -U * 0.8],
      [U * 0.5, U * 0.7],
      [U * 0.8, -U * 0.5],
    ],
  },
};

/** How long the assembly class stays on (the longest flight plus its flash). */
export const ASSEMBLY_MS = 1300;
