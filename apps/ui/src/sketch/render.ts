/**
 * Canvas drawing for sketches: backgrounds, smoothed strokes (quadratic
 * curves through the midpoints of the sampled points), shapes and text.
 * Used for the live canvas and for the exported PNG alike.
 */
import {
  COLORS,
  HIGHLIGHTER_ALPHA,
  type Background,
  type ColorName,
  type ShapeItem,
  type SketchItem,
  type StrokeItem,
  type TextItem,
} from './model';

export const BACKGROUND_FILL: Record<Background, string> = {
  white: '#ffffff',
  dark: '#101b35',
  grid: '#ffffff',
};
const GRID_STEP = 24;
const GRID_MINOR = '#e3ebf8';
const GRID_MAJOR = '#c4d4ef';

/** Canvas text cannot read CSS variables: a plain stack matching `--font`. */
export const SKETCH_FONT =
  "'Inter Variable', 'Pretendard', system-ui, -apple-system, 'Segoe UI', 'Apple SD Gothic Neo', 'Malgun Gothic', 'Hiragino Sans', 'Noto Sans KR', 'Noto Sans JP', sans-serif";

/** What the ink sits on: black and white swap so they never vanish. */
export type PageTone = 'light' | 'dark' | 'photo';

export function pageTone(background: Background, annotate: boolean): PageTone {
  if (annotate) return 'photo';
  return background === 'dark' ? 'dark' : 'light';
}

/** Ink color on a page: white on a light page draws black, black on a dark page white. */
export function inkColor(color: ColorName, tone: PageTone): string {
  if (tone === 'light' && color === 'white') return COLORS.black;
  if (tone === 'dark' && color === 'black') return COLORS.white;
  return COLORS[color];
}

/** The outline behind text labels: the page itself, or a contrast on photos. */
export function textHalo(color: ColorName, tone: PageTone): string {
  if (tone === 'light') return '#ffffff';
  if (tone === 'dark') return BACKGROUND_FILL.dark;
  return color === 'white' || color === 'yellow' ? '#0b1220' : '#ffffff';
}

/** Draws the page (or the photo) over `width` x `height` document units. */
export function drawBackground(
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  background: Background,
  image?: CanvasImageSource | null,
) {
  if (image) {
    ctx.drawImage(image, 0, 0, width, height);
    return;
  }
  ctx.fillStyle = BACKGROUND_FILL[background];
  ctx.fillRect(0, 0, width, height);
  if (background !== 'grid') return;
  ctx.lineWidth = 1;
  for (const major of [false, true]) {
    ctx.strokeStyle = major ? GRID_MAJOR : GRID_MINOR;
    ctx.beginPath();
    for (let i = 1, x = GRID_STEP; x < width; i++, x += GRID_STEP) {
      if ((i % 5 === 0) !== major) continue;
      ctx.moveTo(Math.round(x) + 0.5, 0);
      ctx.lineTo(Math.round(x) + 0.5, height);
    }
    for (let i = 1, y = GRID_STEP; y < height; i++, y += GRID_STEP) {
      if ((i % 5 === 0) !== major) continue;
      ctx.moveTo(0, Math.round(y) + 0.5);
      ctx.lineTo(width, Math.round(y) + 0.5);
    }
    ctx.stroke();
  }
}

/** Width at a sampled point: pressure 0.5 (the default) draws the base width. */
function pressureWidth(base: number, pressure: number): number {
  return base * (0.35 + 1.3 * Math.min(1, Math.max(0, pressure)));
}

function strokeStyleFor(ctx: CanvasRenderingContext2D, item: StrokeItem, tone: PageTone) {
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  if (item.tool === 'eraser') {
    ctx.globalCompositeOperation = 'destination-out';
    ctx.strokeStyle = '#000';
    ctx.fillStyle = '#000';
    ctx.globalAlpha = 1;
  } else {
    ctx.globalCompositeOperation = 'source-over';
    ctx.strokeStyle = inkColor(item.color, tone);
    ctx.fillStyle = inkColor(item.color, tone);
    ctx.globalAlpha = item.tool === 'highlighter' ? HIGHLIGHTER_ALPHA : 1;
  }
}

export function drawStroke(
  ctx: CanvasRenderingContext2D,
  item: StrokeItem,
  tone: PageTone = 'photo',
) {
  const p = item.points;
  const n = p.length / 3;
  if (n === 0) return;
  ctx.save();
  strokeStyleFor(ctx, item, tone);
  if (n === 1) {
    // A tap leaves a dot.
    const w = item.pressure ? pressureWidth(item.width, p[2]!) : item.width;
    ctx.beginPath();
    ctx.arc(p[0]!, p[1]!, w / 2, 0, Math.PI * 2);
    ctx.fill();
    ctx.restore();
    return;
  }
  if (!item.pressure) {
    ctx.lineWidth = item.width;
    ctx.beginPath();
    ctx.moveTo(p[0]!, p[1]!);
    for (let i = 1; i < n - 1; i++) {
      const x = p[i * 3]!;
      const y = p[i * 3 + 1]!;
      ctx.quadraticCurveTo(x, y, (x + p[i * 3 + 3]!) / 2, (y + p[i * 3 + 4]!) / 2);
    }
    ctx.lineTo(p[(n - 1) * 3]!, p[(n - 1) * 3 + 1]!);
    ctx.stroke();
    ctx.restore();
    return;
  }
  // Stylus: one curve segment per sample, each with its own width.
  let sx = p[0]!;
  let sy = p[1]!;
  for (let i = 1; i < n; i++) {
    const x = p[i * 3]!;
    const y = p[i * 3 + 1]!;
    const last = i === n - 1;
    const ex = last ? x : (x + p[i * 3 + 3]!) / 2;
    const ey = last ? y : (y + p[i * 3 + 4]!) / 2;
    ctx.lineWidth = pressureWidth(item.width, (p[i * 3 - 1]! + p[i * 3 + 2]!) / 2);
    ctx.beginPath();
    ctx.moveTo(sx, sy);
    if (last) ctx.lineTo(ex, ey);
    else ctx.quadraticCurveTo(x, y, ex, ey);
    ctx.stroke();
    sx = ex;
    sy = ey;
  }
  ctx.restore();
}

export function drawShape(
  ctx: CanvasRenderingContext2D,
  item: ShapeItem,
  tone: PageTone = 'photo',
) {
  ctx.save();
  ctx.globalCompositeOperation = 'source-over';
  ctx.globalAlpha = 1;
  ctx.strokeStyle = inkColor(item.color, tone);
  ctx.lineWidth = item.width;
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  ctx.beginPath();
  if (item.shape === 'rect') {
    ctx.rect(
      Math.min(item.x1, item.x2),
      Math.min(item.y1, item.y2),
      Math.abs(item.x2 - item.x1),
      Math.abs(item.y2 - item.y1),
    );
  } else {
    ctx.moveTo(item.x1, item.y1);
    ctx.lineTo(item.x2, item.y2);
    if (item.shape === 'arrow') {
      const angle = Math.atan2(item.y2 - item.y1, item.x2 - item.x1);
      const length = Math.hypot(item.x2 - item.x1, item.y2 - item.y1);
      const head = Math.min(length * 0.5, Math.max(12, item.width * 4));
      for (const side of [-1, 1]) {
        const a = angle + Math.PI + side * 0.48;
        ctx.moveTo(item.x2, item.y2);
        ctx.lineTo(item.x2 + Math.cos(a) * head, item.y2 + Math.sin(a) * head);
      }
    }
  }
  ctx.stroke();
  ctx.restore();
}

export function textFont(size: number): string {
  return `600 ${size}px ${SKETCH_FONT}`;
}

/** Text with a thin contrasting halo, so a label reads on photos too. */
export function drawText(ctx: CanvasRenderingContext2D, item: TextItem, tone: PageTone = 'photo') {
  ctx.save();
  ctx.globalCompositeOperation = 'source-over';
  ctx.globalAlpha = 1;
  ctx.font = textFont(item.size);
  ctx.textBaseline = 'top';
  const lines = item.text.split('\n');
  const lineHeight = item.size * 1.25;
  const halo = textHalo(item.color, tone);
  ctx.lineJoin = 'round';
  ctx.lineWidth = Math.max(2, item.size / 6);
  ctx.strokeStyle = halo;
  ctx.fillStyle = inkColor(item.color, tone);
  lines.forEach((line, i) => {
    ctx.strokeText(line, item.x, item.y + i * lineHeight);
    ctx.fillText(line, item.x, item.y + i * lineHeight);
  });
  ctx.restore();
}

export function drawItem(ctx: CanvasRenderingContext2D, item: SketchItem, tone: PageTone) {
  if (item.kind === 'stroke') drawStroke(ctx, item, tone);
  else if (item.kind === 'shape') drawShape(ctx, item, tone);
  else drawText(ctx, item, tone);
}

export function drawItems(
  ctx: CanvasRenderingContext2D,
  items: readonly SketchItem[],
  tone: PageTone,
) {
  for (const item of items) drawItem(ctx, item, tone);
}

/** Maps document units to the canvas: contain-fit, centered. */
export interface View {
  scale: number;
  offsetX: number;
  offsetY: number;
}

export function fitView(
  docWidth: number,
  docHeight: number,
  stageWidth: number,
  stageHeight: number,
  padding = 0,
): View {
  if (docWidth <= 0 || docHeight <= 0 || stageWidth <= 0 || stageHeight <= 0) {
    return { scale: 1, offsetX: 0, offsetY: 0 };
  }
  const w = Math.max(1, stageWidth - padding * 2);
  const h = Math.max(1, stageHeight - padding * 2);
  const scale = Math.min(w / docWidth, h / docHeight);
  return {
    scale,
    offsetX: (stageWidth - docWidth * scale) / 2,
    offsetY: (stageHeight - docHeight * scale) / 2,
  };
}

function encode(canvas: HTMLCanvasElement, type: string, quality?: number): Promise<Blob> {
  return new Promise((resolve, reject) => {
    canvas.toBlob(
      (blob) => (blob ? resolve(blob) : reject(new Error('Could not encode the sketch'))),
      type,
      quality,
    );
  });
}

/** A photo annotated as PNG can get heavy; past this, JPEG is used instead. */
export const PNG_BUDGET = 3 * 1024 * 1024;

export interface ExportInput {
  docWidth: number;
  docHeight: number;
  scale: number;
  background: Background;
  image?: CanvasImageSource | null;
  items: readonly SketchItem[];
}

/**
 * Renders the sketch off screen and encodes it: PNG (sharp lines, small for
 * drawings); an annotated photo that would exceed `PNG_BUDGET` becomes JPEG.
 */
export async function exportSketch(input: ExportInput): Promise<Blob> {
  const width = Math.max(1, Math.round(input.docWidth * input.scale));
  const height = Math.max(1, Math.round(input.docHeight * input.scale));
  const out = document.createElement('canvas');
  out.width = width;
  out.height = height;
  const ink = document.createElement('canvas');
  ink.width = width;
  ink.height = height;
  const ctx = out.getContext('2d');
  const inkCtx = ink.getContext('2d');
  if (ctx && inkCtx) {
    ctx.setTransform(input.scale, 0, 0, input.scale, 0, 0);
    drawBackground(ctx, input.docWidth, input.docHeight, input.background, input.image);
    inkCtx.setTransform(input.scale, 0, 0, input.scale, 0, 0);
    drawItems(inkCtx, input.items, pageTone(input.background, Boolean(input.image)));
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.drawImage(ink, 0, 0);
  }
  const png = await encode(out, 'image/png');
  if (!input.image || png.size <= PNG_BUDGET) return png;
  return encode(out, 'image/jpeg', 0.9);
}
