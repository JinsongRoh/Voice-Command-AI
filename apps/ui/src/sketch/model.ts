/**
 * Sketch document model: tools, styles, drawn items and the undo/redo
 * history. Pure data and a reducer, so it is easy to test and to keep as an
 * in-memory draft while the sheet is closed.
 *
 * Coordinates are document units: the pixels of the annotated photo (capped
 * at `MAX_SKETCH_EDGE`), or the CSS pixels of the canvas when the sketch was
 * started on a blank page. The view maps them to the screen.
 */

export const MAX_SKETCH_EDGE = 2048;

export type Tool = 'pen' | 'highlighter' | 'eraser' | 'line' | 'arrow' | 'rect' | 'text';
export type ShapeKind = 'line' | 'arrow' | 'rect';
export type ColorName = 'black' | 'white' | 'red' | 'blue' | 'green' | 'yellow';
export type Background = 'white' | 'dark' | 'grid';
/** Index into `WIDTHS`: thin, medium, thick. */
export type WidthIndex = 0 | 1 | 2;

export const TOOLS: readonly Tool[] = [
  'pen',
  'highlighter',
  'eraser',
  'line',
  'arrow',
  'rect',
  'text',
];
export const COLOR_NAMES: readonly ColorName[] = [
  'black',
  'white',
  'red',
  'blue',
  'green',
  'yellow',
];
export const BACKGROUNDS: readonly Background[] = ['white', 'dark', 'grid'];

/** Ink colors, tuned to read on white, navy and photos alike. */
export const COLORS: Record<ColorName, string> = {
  black: '#0b1220',
  white: '#ffffff',
  red: '#ef3b3b',
  blue: '#1e6bff',
  green: '#16a34a',
  yellow: '#facc15',
};

/** Pen widths in CSS pixels at the time of drawing. */
export const WIDTHS: readonly [number, number, number] = [2.5, 5, 10];
/** Text sizes for the same three steps. */
export const TEXT_SIZES: readonly [number, number, number] = [16, 22, 32];
export const HIGHLIGHTER_FACTOR = 3.5;
export const ERASER_FACTOR = 4;
export const HIGHLIGHTER_ALPHA = 0.35;

export interface StrokeItem {
  kind: 'stroke';
  tool: 'pen' | 'highlighter' | 'eraser';
  color: ColorName;
  /** Base width in document units. */
  width: number;
  /** Flat `x, y, pressure` triplets in document units. */
  points: number[];
  /** Width follows the pressure of a stylus. */
  pressure: boolean;
}

export interface ShapeItem {
  kind: 'shape';
  shape: ShapeKind;
  color: ColorName;
  width: number;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

export interface TextItem {
  kind: 'text';
  color: ColorName;
  /** Font size in document units. */
  size: number;
  x: number;
  y: number;
  text: string;
}

export type SketchItem = StrokeItem | ShapeItem | TextItem;

export interface History {
  past: SketchItem[][];
  present: SketchItem[];
  future: SketchItem[][];
}

export const emptyHistory: History = { past: [], present: [], future: [] };

/** Keeps memory bounded on long sessions; older steps cannot be undone. */
export const MAX_HISTORY = 100;

export function commit(h: History, present: SketchItem[]): History {
  const past = [...h.past, h.present];
  if (past.length > MAX_HISTORY) past.splice(0, past.length - MAX_HISTORY);
  return { past, present, future: [] };
}

export function undo(h: History): History {
  if (h.past.length === 0) return h;
  const previous = h.past[h.past.length - 1]!;
  return { past: h.past.slice(0, -1), present: previous, future: [h.present, ...h.future] };
}

export function redo(h: History): History {
  if (h.future.length === 0) return h;
  const [next, ...rest] = h.future;
  return { past: [...h.past, h.present], present: next!, future: rest };
}

export const canUndo = (h: History) => h.past.length > 0;
export const canRedo = (h: History) => h.future.length > 0;

export interface SketchState {
  tool: Tool;
  color: ColorName;
  width: WidthIndex;
  background: Background;
  history: History;
  /** Document size; 0 until the canvas is measured (blank page). */
  docWidth: number;
  docHeight: number;
  /** A photo is the background (annotate mode). */
  annotate: boolean;
}

export type SketchAction =
  | { type: 'tool'; tool: Tool }
  | { type: 'color'; color: ColorName }
  | { type: 'width'; width: WidthIndex }
  | { type: 'background'; background: Background }
  | { type: 'add'; item: SketchItem }
  | { type: 'undo' }
  | { type: 'redo' }
  | { type: 'clear' }
  | { type: 'size'; width: number; height: number }
  | { type: 'image'; width: number; height: number };

/** The ink that contrasts with a background. */
export function contrastInk(background: Background): ColorName {
  return background === 'dark' ? 'white' : 'black';
}

export function initialSketch(theme: 'light' | 'dark'): SketchState {
  const background: Background = theme === 'dark' ? 'dark' : 'white';
  return {
    tool: 'pen',
    color: contrastInk(background),
    width: 1,
    background,
    history: emptyHistory,
    docWidth: 0,
    docHeight: 0,
    annotate: false,
  };
}

export function sketchReducer(state: SketchState, action: SketchAction): SketchState {
  switch (action.type) {
    case 'tool':
      return { ...state, tool: action.tool };
    case 'color':
      // Picking a color while erasing means "draw again".
      return {
        ...state,
        color: action.color,
        tool: state.tool === 'eraser' ? 'pen' : state.tool,
      };
    case 'width':
      return { ...state, width: action.width };
    case 'background': {
      // Keep the default ink readable when it would vanish on the new page.
      const color =
        state.color === contrastInk(state.background)
          ? contrastInk(action.background)
          : state.color;
      return { ...state, background: action.background, color };
    }
    case 'add':
      return {
        ...state,
        history: commit(state.history, [...state.history.present, action.item]),
      };
    case 'undo':
      return { ...state, history: undo(state.history) };
    case 'redo':
      return { ...state, history: redo(state.history) };
    case 'clear':
      if (state.history.present.length === 0) return state;
      return { ...state, history: commit(state.history, []) };
    case 'size':
      // A blank page takes the size of the canvas once; later resizes fit it.
      if (state.docWidth > 0 || action.width <= 0 || action.height <= 0) return state;
      return { ...state, docWidth: action.width, docHeight: action.height };
    case 'image':
      return {
        ...state,
        annotate: true,
        docWidth: action.width,
        docHeight: action.height,
        history: emptyHistory,
        // Red reads on most photos.
        color: state.color === contrastInk(state.background) ? 'red' : state.color,
      };
  }
}

/** Something worth keeping (or confirming before it is thrown away). */
export function hasContent(state: SketchState): boolean {
  return state.history.present.length > 0 || state.annotate;
}

/** Document size for a photo: its own pixels, capped at `MAX_SKETCH_EDGE`. */
export function imageDocSize(width: number, height: number): { width: number; height: number } {
  const long = Math.max(width, height);
  if (long <= MAX_SKETCH_EDGE) return { width, height };
  const k = MAX_SKETCH_EDGE / long;
  return { width: Math.max(1, Math.round(width * k)), height: Math.max(1, Math.round(height * k)) };
}

/** Export scale: crisp on high-density screens, never above `MAX_SKETCH_EDGE`. */
export function exportScale(
  docWidth: number,
  docHeight: number,
  annotate: boolean,
  pixelRatio: number,
): number {
  const long = Math.max(docWidth, docHeight);
  if (long <= 0) return 1;
  const wanted = annotate ? 1 : Math.min(2, Math.max(1, pixelRatio));
  return Math.min(wanted, MAX_SKETCH_EDGE / long);
}

/** `sketch-20261031-142530.png` (local time). */
export function sketchFileName(now: Date, ext = 'png'): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `sketch-${now.getFullYear()}${p(now.getMonth() + 1)}${p(now.getDate())}-${p(
    now.getHours(),
  )}${p(now.getMinutes())}${p(now.getSeconds())}.${ext}`;
}

/** Stroke width for a tool, in CSS pixels. */
export function toolWidth(tool: Tool, width: WidthIndex): number {
  const base = WIDTHS[width];
  if (tool === 'highlighter') return base * HIGHLIGHTER_FACTOR;
  if (tool === 'eraser') return base * ERASER_FACTOR;
  return base;
}
