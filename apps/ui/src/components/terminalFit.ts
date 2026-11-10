/**
 * Terminal sizing. Agent sessions and remote shells run in a PTY of a size
 * set on the PC (120 x 40 unless `vca run` passed its terminal's size), and
 * programs draw for exactly that size; a viewer with other columns or rows
 * (a wide desktop pane, a narrow phone) garbles the screen. So every viewer
 * keeps the PTY's columns and rows, which the server sends in the output
 * (see SIZE_OSC), and scales the font to fit its pane instead; on phones
 * pinch zoom (or the zoom buttons) scales it further, and a screen larger
 * than the phone scrolls.
 */

/** Columns of the PTYs the server opens (vca-server `sessions.rs`, `shell/mod.rs`). */
export const PTY_COLS = 120;
/** Rows of the PTYs the server opens. */
export const PTY_ROWS = 40;
/**
 * OSC number of the server's PTY size marker, `ESC ] 7377 ; cols ; rows BEL`,
 * sent in front of output chunks (vca-server `term_size.rs`).
 */
export const SIZE_OSC = 7377;

export interface PtyGeometry {
  cols: number;
  rows: number;
}

export const DEFAULT_GEOMETRY: PtyGeometry = { cols: PTY_COLS, rows: PTY_ROWS };

/** The size in a marker's payload (`"120;40"`), or null when it is not one. */
export function parseSizeMarker(payload: string): PtyGeometry | null {
  const m = /^(\d{1,4});(\d{1,4})$/.exec(payload);
  if (!m) return null;
  const cols = Number(m[1]);
  const rows = Number(m[2]);
  return cols > 0 && rows > 0 ? { cols, rows } : null;
}

const MARKER_RE = new RegExp(`\\x1b\\]${SIZE_OSC};(\\d{1,4};\\d{1,4})(?:\\x07|\\x1b\\\\)`);

/**
 * The size of the first marker in the output, so a view that replays only
 * a page of history starts at the right size before it writes anything.
 */
export function firstSizeMarker(chunks: Iterable<string>): PtyGeometry | null {
  for (const chunk of chunks) {
    const m = MARKER_RE.exec(chunk);
    if (m) return parseSizeMarker(m[1]!);
  }
  return null;
}

/**
 * Font size (px, to a tenth) at which a `cols` x `rows` screen fits a
 * `width` x `height` pane, times `zoom`, within `[min, max]`. A screen that
 * does not fit at `min` scrolls.
 */
export function fitScreenFont({
  width,
  height,
  cols,
  rows,
  charRatio = DEFAULT_CHAR_RATIO,
  lineRatio = DEFAULT_LINE_RATIO,
  zoom = 1,
  min = MIN_FONT_PX,
  max = MAX_FONT_PX,
}: {
  width: number;
  height: number;
  cols: number;
  rows: number;
  charRatio?: number;
  lineRatio?: number;
  zoom?: number;
  min?: number;
  max?: number;
}): number {
  if (!(width > 0) || !(cols > 0) || !(charRatio > 0)) return min;
  const byWidth = width / (cols * charRatio);
  const byHeight = height > 0 && rows > 0 && lineRatio > 0 ? height / (rows * lineRatio) : Infinity;
  const auto = Math.min(byWidth, byHeight) * clamp(zoom, MIN_ZOOM, MAX_ZOOM);
  // Rounded down, so the screen is not a fraction wider than the pane.
  return Math.floor(clamp(auto, min, max) * 10) / 10;
}
/** Smallest automatic font size; narrower screens scroll sideways. */
export const MIN_FONT_PX = 5;
export const MAX_FONT_PX = 24;
export const MIN_ZOOM = 0.5;
export const MAX_ZOOM = 4;
/** Monospace advance width per pixel of font size when it cannot be measured. */
export const DEFAULT_CHAR_RATIO = 0.6;
/** Row height per pixel of font size when it cannot be measured. */
export const DEFAULT_LINE_RATIO = 1.25;

export const ZOOM_KEY = 'vca.terminal.zoom';

export function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/**
 * Font size (px, to a tenth) at which `cols` columns fill `width` pixels,
 * times the person's zoom, within the allowed range.
 */
export function fitFontSize(
  width: number,
  cols: number = PTY_COLS,
  charRatio: number = DEFAULT_CHAR_RATIO,
  zoom = 1,
): number {
  if (!(width > 0) || !(cols > 0) || !(charRatio > 0)) return MIN_FONT_PX;
  const auto = width / (cols * charRatio);
  return (
    Math.round(clamp(auto * clamp(zoom, MIN_ZOOM, MAX_ZOOM), MIN_FONT_PX, MAX_FONT_PX) * 10) / 10
  );
}

/** The zoom after a pinch from `startDistance` to `distance` (px between two fingers). */
export function pinchZoom(startZoom: number, startDistance: number, distance: number): number {
  if (!(startDistance > 0) || !(distance > 0)) return startZoom;
  return clamp(startZoom * (distance / startDistance), MIN_ZOOM, MAX_ZOOM);
}

/** Next zoom step for the zoom buttons. */
export function stepZoom(zoom: number, direction: 1 | -1): number {
  const next = direction > 0 ? zoom * 1.25 : zoom / 1.25;
  // Snap back to exactly 1 near it, so "fit" is reachable with the buttons.
  return Math.abs(next - 1) < 0.05 ? 1 : clamp(next, MIN_ZOOM, MAX_ZOOM);
}

export function loadZoom(): number {
  try {
    const v = Number(localStorage.getItem(ZOOM_KEY));
    return Number.isFinite(v) && v > 0 ? clamp(v, MIN_ZOOM, MAX_ZOOM) : 1;
  } catch {
    return 1;
  }
}

export function saveZoom(zoom: number): void {
  try {
    if (zoom === 1) localStorage.removeItem(ZOOM_KEY);
    else localStorage.setItem(ZOOM_KEY, String(zoom));
  } catch {
    // Not remembered; the zoom still applies now.
  }
}

/** Advance width of the monospace font per pixel of font size. */
export function measureCharRatio(fontFamily: string): number {
  try {
    const ctx = document.createElement('canvas').getContext('2d');
    if (!ctx) return DEFAULT_CHAR_RATIO;
    ctx.font = `100px ${fontFamily}`;
    const width = ctx.measureText('W'.repeat(10)).width / 10;
    return width > 0 ? width / 100 : DEFAULT_CHAR_RATIO;
  } catch {
    return DEFAULT_CHAR_RATIO;
  }
}

/** Row height of the terminal font per pixel of font size (xterm measures a line box). */
export function measureLineRatio(fontFamily: string): number {
  try {
    const span = document.createElement('span');
    span.textContent = 'W';
    span.style.cssText = `position:absolute;visibility:hidden;white-space:pre;line-height:normal;font-size:100px;font-family:${fontFamily}`;
    document.body.appendChild(span);
    const height = span.getBoundingClientRect().height;
    span.remove();
    return height > 0 ? height / 100 : DEFAULT_LINE_RATIO;
  } catch {
    return DEFAULT_LINE_RATIO;
  }
}
