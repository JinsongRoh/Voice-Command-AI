import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useReducer,
  useRef,
  useState,
  type ChangeEvent,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from 'react';
import { createPortal } from 'react-dom';

import { browserCodec, type ImageCodec } from '../attachments/resize';
import { useI18n } from '../i18n/context';
import { deleteDraft, getDraft, saveDraft } from '../sketch/drafts';
import {
  BACKGROUNDS,
  COLOR_NAMES,
  COLORS,
  TEXT_SIZES,
  TOOLS,
  WIDTHS,
  canRedo,
  canUndo,
  exportScale,
  hasContent,
  imageDocSize,
  initialSketch,
  sketchFileName,
  sketchReducer,
  toolWidth,
  type Background,
  type ColorName,
  type ShapeItem,
  type SketchState,
  type StrokeItem,
  type Tool,
  type WidthIndex,
} from '../sketch/model';
import {
  BACKGROUND_FILL,
  drawBackground,
  drawItem,
  drawItems,
  exportSketch,
  fitView,
  inkColor,
  pageTone,
  textFont,
  textHalo,
  type View,
} from '../sketch/render';
import { Icon } from './Icon';
import { ConfirmDialog, openModalLayer } from './Modal';
import styles from './SketchSheet.module.css';

const ICONS: Record<Tool | 'undo' | 'redo' | 'clear' | 'photo', string> = {
  pen: 'M4 20l1.2-4.4L15.6 5.2a2 2 0 0 1 2.8 0l.4.4a2 2 0 0 1 0 2.8L8.4 18.8 4 20zM14 7l3 3',
  highlighter: 'M14.5 4.5l5 5-7.5 7.5H7v-5l7.5-7.5zM7 17l-3 3h6M10 9.5l4.5 4.5',
  eraser:
    'M9 20h11M5.2 15.2l8.6-8.6a2 2 0 0 1 2.8 0l2.8 2.8a2 2 0 0 1 0 2.8L12 19.6H9.6l-4.4-4.4zM9.5 11l4.5 4.5',
  line: 'M5 19L19 5',
  arrow: 'M5 19L19 5M10 5h9v9',
  rect: 'M4 6h16v12H4z',
  text: 'M5 7V5h14v2M12 5v14M9 19h6',
  undo: 'M9 14L4 9l5-5M4 9h10.5a5.5 5.5 0 0 1 0 11H11',
  redo: 'M15 14l5-5-5-5M20 9H9.5a5.5 5.5 0 0 0 0 11H13',
  clear: 'M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3',
  photo: 'M4 5h16v14H4zM4 16l5-5 4 4 2-2 5 5M15.5 9h.01',
};

function Glyph({ name, size = 22 }: { name: keyof typeof ICONS; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.9}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d={ICONS[name]} />
    </svg>
  );
}

function currentTheme(): 'light' | 'dark' {
  const forced = document.documentElement.dataset.theme;
  if (forced === 'black') return 'dark';
  if (forced === 'dark' || forced === 'light') return forced;
  return globalThis.matchMedia?.('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

const FOCUSABLE =
  'button:not(:disabled), [href], input:not(:disabled):not([type="file"]), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])';

interface Live {
  pointerId: number;
  item: StrokeItem | ShapeItem | null;
  /** Text tool: where the pointer went down (opens the label on release). */
  textAt?: { x: number; y: number; clientX: number; clientY: number };
}

interface TextDraft {
  x: number;
  y: number;
  size: number;
  color: ColorName;
  value: string;
}

type Confirm = 'cancel' | 'clear' | 'photo';

export interface SketchSheetProps {
  /** Where the unsent drawing is kept while the sheet is closed (the session). */
  draftKey: string;
  /** The finished drawing as a PNG (or JPEG for a heavy photo) file. */
  onDone(file: File): void;
  /** The sheet closed; the drawing is kept as a draft unless it was discarded. */
  onClose(): void;
  codec?: ImageCodec;
}

/**
 * Full-screen drawing sheet: pen (stylus pressure), highlighter, eraser,
 * line, arrow, rectangle and text on a white, dark or grid page, or on top
 * of a photo. Done exports a PNG that the composer attaches like any photo.
 */
export function SketchSheet({ draftKey, onDone, onClose, codec = browserCodec }: SketchSheetProps) {
  const { t } = useI18n();
  const titleId = useId();
  const [draft] = useState(() => getDraft(draftKey));
  const [state, dispatch] = useReducer(
    sketchReducer,
    undefined,
    (): SketchState => draft?.state ?? initialSketch(currentTheme()),
  );
  const [image, setImage] = useState<{
    source: CanvasImageSource;
    release?: () => void;
  } | null>(() => (draft?.image ? { source: draft.image, release: draft.releaseImage } : null));
  const [stage, setStage] = useState({ width: 0, height: 0 });
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  const [text, setText] = useState<TextDraft | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const sheetRef = useRef<HTMLDivElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const baseRef = useRef<HTMLCanvasElement>(null);
  const inkRef = useRef<HTMLCanvasElement>(null);
  const liveRef = useRef<HTMLCanvasElement>(null);
  const photoRef = useRef<HTMLInputElement>(null);
  const textRef = useRef<HTMLInputElement>(null);
  const live = useRef<Live | null>(null);
  const frame = useRef(0);
  const finished = useRef(false);
  const latest = useRef({ state, image, text });
  // Layout effects, so the canvas effects below (also layout) see this render.
  useLayoutEffect(() => {
    latest.current = { state, image, text };
  });

  const dpr = Math.max(1, Math.min(3, globalThis.devicePixelRatio || 1));
  const view: View = fitView(
    state.docWidth,
    state.docHeight,
    stage.width,
    stage.height,
    state.annotate ? 12 : 0,
  );
  const viewRef = useRef(view);
  useLayoutEffect(() => {
    viewRef.current = view;
  });
  const items = state.history.present;

  // Keep the drawing as a draft when the sheet goes away without Done or
  // Discard (Escape, switching sessions, the composer unmounting).
  useEffect(
    () => () => {
      const { state: s, image: img } = latest.current;
      if (!finished.current && hasContent(s)) {
        saveDraft(draftKey, { state: s, image: img?.source ?? null, releaseImage: img?.release });
      } else {
        deleteDraft(draftKey, finished.current ? null : img?.source);
        if (finished.current) img?.release?.();
      }
    },
    [draftKey],
  );

  // ---- Stage size --------------------------------------------------------
  useLayoutEffect(() => {
    const node = stageRef.current;
    if (!node) return;
    const measure = () => {
      const r = node.getBoundingClientRect();
      const width = Math.round(r.width);
      const height = Math.round(r.height);
      setStage((s) => (s.width === width && s.height === height ? s : { width, height }));
    };
    measure();
    if (typeof ResizeObserver === 'function') {
      const ro = new ResizeObserver(measure);
      ro.observe(node);
      return () => ro.disconnect();
    }
    window.addEventListener('resize', measure);
    return () => window.removeEventListener('resize', measure);
  }, []);

  useEffect(() => {
    if (stage.width > 0 && stage.height > 0) {
      dispatch({ type: 'size', width: stage.width, height: stage.height });
    }
  }, [stage.width, stage.height]);

  // ---- Rendering ---------------------------------------------------------
  const prepare = useCallback(
    (canvas: HTMLCanvasElement | null, clip: boolean) => {
      if (!canvas) return null;
      const w = Math.max(1, Math.round(stage.width * dpr));
      const h = Math.max(1, Math.round(stage.height * dpr));
      if (canvas.width !== w) canvas.width = w;
      if (canvas.height !== h) canvas.height = h;
      const ctx = canvas.getContext('2d');
      if (!ctx) return null;
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.globalCompositeOperation = 'source-over';
      ctx.clearRect(0, 0, w, h);
      const v = viewRef.current;
      ctx.setTransform(dpr * v.scale, 0, 0, dpr * v.scale, dpr * v.offsetX, dpr * v.offsetY);
      if (clip) {
        ctx.beginPath();
        ctx.rect(0, 0, latest.current.state.docWidth, latest.current.state.docHeight);
        ctx.clip();
      }
      return ctx;
    },
    [stage.width, stage.height, dpr],
  );

  const renderInk = useCallback(() => {
    const ctx = prepare(inkRef.current, true);
    if (!ctx) return;
    const s = latest.current.state;
    const tone = pageTone(s.background, s.annotate);
    drawItems(ctx, s.history.present, tone);
    const item = live.current?.item;
    if (item?.kind === 'stroke' && item.tool === 'eraser') drawItem(ctx, item, tone);
  }, [prepare]);

  const renderLive = useCallback(() => {
    frame.current = 0;
    const item = live.current?.item;
    if (item?.kind === 'stroke' && item.tool === 'eraser') {
      renderInk();
      return;
    }
    const ctx = prepare(liveRef.current, true);
    const s = latest.current.state;
    if (ctx && item) drawItem(ctx, item, pageTone(s.background, s.annotate));
  }, [prepare, renderInk]);

  const schedule = () => {
    if (frame.current) return;
    if (typeof requestAnimationFrame === 'function') {
      frame.current = requestAnimationFrame(renderLive);
    } else {
      renderLive();
    }
  };
  useEffect(
    () => () => {
      if (frame.current && typeof cancelAnimationFrame === 'function') {
        cancelAnimationFrame(frame.current);
      }
    },
    [],
  );

  useLayoutEffect(() => {
    if (stage.width === 0 || state.docWidth === 0) return;
    const ctx = prepare(baseRef.current, false);
    if (ctx) drawBackground(ctx, state.docWidth, state.docHeight, state.background, image?.source);
  }, [
    prepare,
    stage.width,
    stage.height,
    state.docWidth,
    state.docHeight,
    state.background,
    image,
    view.scale,
  ]);

  useLayoutEffect(() => {
    if (stage.width === 0 || state.docWidth === 0) return;
    renderInk();
    prepare(liveRef.current, false);
  }, [
    renderInk,
    prepare,
    items,
    stage.width,
    stage.height,
    state.docWidth,
    state.background,
    state.annotate,
    view.scale,
  ]);

  // ---- Pointer input -----------------------------------------------------
  const toDoc = (e: { clientX: number; clientY: number }) => {
    const rect = liveRef.current!.getBoundingClientRect();
    const v = viewRef.current;
    return {
      x: (e.clientX - rect.left - v.offsetX) / v.scale,
      y: (e.clientY - rect.top - v.offsetY) / v.scale,
    };
  };

  const commitText = useCallback(() => {
    const current = latest.current.text;
    if (!current) return;
    latest.current.text = null;
    setText(null);
    const value = current.value.trim();
    if (value) {
      dispatch({
        type: 'add',
        item: {
          kind: 'text',
          color: current.color,
          size: current.size,
          x: current.x,
          y: current.y,
          text: value,
        },
      });
    }
  }, []);

  const onPointerDown = (e: ReactPointerEvent<HTMLCanvasElement>) => {
    if (e.pointerType === 'mouse' && e.button !== 0) return;
    if (live.current || state.docWidth === 0 || busy) return;
    e.preventDefault();
    if (latest.current.text) {
      // A tap outside the label finishes it.
      commitText();
      return;
    }
    const { x, y } = toDoc(e);
    e.currentTarget.setPointerCapture?.(e.pointerId);
    const tool = state.tool;
    if (tool === 'text') {
      live.current = {
        pointerId: e.pointerId,
        item: null,
        textAt: { x, y, clientX: e.clientX, clientY: e.clientY },
      };
      return;
    }
    const width = toolWidth(tool, state.width) / view.scale;
    if (tool === 'pen' || tool === 'highlighter' || tool === 'eraser') {
      const stylus = e.pointerType === 'pen' && tool !== 'highlighter';
      live.current = {
        pointerId: e.pointerId,
        item: {
          kind: 'stroke',
          tool,
          color: state.color,
          width,
          points: [x, y, stylus ? e.pressure || 0.5 : 0.5],
          pressure: stylus,
        },
      };
    } else {
      live.current = {
        pointerId: e.pointerId,
        item: { kind: 'shape', shape: tool, color: state.color, width, x1: x, y1: y, x2: x, y2: y },
      };
    }
    schedule();
  };

  const onPointerMove = (e: ReactPointerEvent<HTMLCanvasElement>) => {
    const current = live.current;
    if (!current || current.pointerId !== e.pointerId || !current.item) return;
    const item = current.item;
    const native = e.nativeEvent as PointerEvent;
    const samples: { clientX: number; clientY: number; pressure: number }[] =
      typeof native.getCoalescedEvents === 'function' ? native.getCoalescedEvents() : [];
    if (samples.length === 0) samples.push(e);
    const minStep = 0.75 / viewRef.current.scale;
    for (const s of samples) {
      const { x, y } = toDoc(s);
      if (item.kind === 'shape') {
        item.x2 = x;
        item.y2 = y;
        continue;
      }
      const p = item.points;
      const lx = p[p.length - 3]!;
      const ly = p[p.length - 2]!;
      if (Math.abs(x - lx) < minStep && Math.abs(y - ly) < minStep) continue;
      p.push(x, y, item.pressure ? s.pressure || 0.5 : 0.5);
    }
    schedule();
  };

  const endPointer = (e: ReactPointerEvent<HTMLCanvasElement>, cancelled: boolean) => {
    const current = live.current;
    if (!current || current.pointerId !== e.pointerId) return;
    live.current = null;
    if (frame.current && typeof cancelAnimationFrame === 'function') {
      cancelAnimationFrame(frame.current);
    }
    frame.current = 0;
    if (current.textAt) {
      if (cancelled) return;
      const v = viewRef.current;
      const size = TEXT_SIZES[state.width] / v.scale;
      const at = current.textAt;
      const next: TextDraft = {
        x: at.x,
        // Center the first line on the tap.
        y: at.y - (size * 1.25) / 2,
        size,
        color: state.color,
        value: '',
      };
      latest.current.text = next;
      setText(next);
      return;
    }
    const item = current.item;
    let keep = !cancelled && item !== null;
    if (item?.kind === 'shape') {
      const length = Math.hypot(item.x2 - item.x1, item.y2 - item.y1);
      keep = keep && length * viewRef.current.scale >= 4;
    }
    if (keep && item) {
      // Draw it right away so nothing blinks before React re-renders.
      const ctx = inkRef.current?.getContext('2d');
      if (ctx && !(item.kind === 'stroke' && item.tool === 'eraser')) {
        drawItem(ctx, item, pageTone(state.background, state.annotate));
      }
      prepare(liveRef.current, false);
      dispatch({ type: 'add', item });
    } else {
      prepare(liveRef.current, false);
      renderInk();
    }
  };

  // Focus the label as soon as it appears (on release, so iOS shows the keyboard).
  useLayoutEffect(() => {
    if (text) textRef.current?.focus();
  }, [text !== null]); // eslint-disable-line react-hooks/exhaustive-deps

  // ---- Actions -----------------------------------------------------------
  const finish = () => {
    finished.current = true;
  };

  const discard = () => {
    setConfirm(null);
    finish();
    onClose();
  };

  const requestCancel = () => {
    commitText();
    if (hasContent(latest.current.state)) setConfirm('cancel');
    else discard();
  };

  const done = async () => {
    // A label still being typed is part of the drawing.
    const pending = latest.current.text;
    const label = pending?.value.trim();
    commitText();
    const s = latest.current.state;
    if (busy || (!hasContent(s) && !label)) return;
    setBusy(true);
    setError(null);
    try {
      const all = label
        ? [
            ...s.history.present,
            {
              kind: 'text' as const,
              color: pending!.color,
              size: pending!.size,
              x: pending!.x,
              y: pending!.y,
              text: label,
            },
          ]
        : s.history.present;
      const blob = await exportSketch({
        docWidth: s.docWidth,
        docHeight: s.docHeight,
        scale: exportScale(s.docWidth, s.docHeight, s.annotate, dpr),
        background: s.background,
        image: latest.current.image?.source,
        items: all,
      });
      const type = blob.type || 'image/png';
      const name = sketchFileName(new Date(), type === 'image/jpeg' ? 'jpg' : 'png');
      finish();
      onDone(new File([blob], name, { type }));
    } catch (err) {
      setBusy(false);
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const pickPhoto = () => {
    commitText();
    if (latest.current.state.history.present.length > 0) setConfirm('photo');
    else photoRef.current?.click();
  };

  const onPhoto = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = '';
    if (!file) return;
    setError(null);
    try {
      const decoded = await codec.decode(file);
      const size = imageDocSize(decoded.width, decoded.height);
      let source: CanvasImageSource = decoded.source;
      let release = decoded.close;
      if (size.width !== decoded.width || size.height !== decoded.height) {
        // Keep at most 2048 px in memory (a 48 MP photo would hold ~190 MB).
        const canvas = document.createElement('canvas');
        canvas.width = size.width;
        canvas.height = size.height;
        const ctx = canvas.getContext('2d');
        if (ctx) {
          ctx.imageSmoothingQuality = 'high';
          ctx.drawImage(decoded.source, 0, 0, size.width, size.height);
          decoded.close?.();
          source = canvas;
          release = undefined;
        }
      }
      setImage((previous) => {
        previous?.release?.();
        return { source, release };
      });
      dispatch({ type: 'image', width: size.width, height: size.height });
    } catch {
      setError(t('sketch.photoError'));
    }
  };

  // ---- Keyboard and focus ------------------------------------------------
  const keyRefs = useRef({ requestCancel, onClose, dispatch });
  useEffect(() => {
    keyRefs.current = { requestCancel, onClose, dispatch };
  });

  useEffect(() => {
    const node = sheetRef.current;
    const previous = document.activeElement as HTMLElement | null;
    const first = node?.querySelector<HTMLElement>('[data-tool][aria-pressed="true"]');
    (first ?? node)?.focus();
    const layer = openModalLayer();
    const onKey = (e: KeyboardEvent) => {
      if (!layer.isTop()) return;
      const typing = (e.target as HTMLElement | null)?.tagName === 'INPUT';
      if (e.key === 'Escape') {
        e.stopPropagation();
        // Closing by Escape keeps the drawing as a draft.
        keyRefs.current.onClose();
      } else if (e.key === 'Tab') {
        const items = Array.from(node?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? []);
        if (items.length === 0) return;
        const firstItem = items[0]!;
        const last = items[items.length - 1]!;
        if (
          e.shiftKey &&
          (document.activeElement === firstItem || document.activeElement === node)
        ) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          firstItem.focus();
        }
      } else if ((e.ctrlKey || e.metaKey) && !typing) {
        const key = e.key.toLowerCase();
        if (key === 'z') {
          e.preventDefault();
          keyRefs.current.dispatch({ type: e.shiftKey ? 'redo' : 'undo' });
        } else if (key === 'y') {
          e.preventDefault();
          keyRefs.current.dispatch({ type: 'redo' });
        }
      }
    };
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('keydown', onKey);
      layer.close();
      previous?.focus?.();
    };
  }, []);

  const onTextKey = (e: ReactKeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      commitText();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      latest.current.text = null;
      setText(null);
    }
  };

  // ---- View --------------------------------------------------------------
  const showBackgrounds = !state.annotate;
  const empty = !hasContent(state) && !text?.value.trim();
  const toolLabel = (tool: Tool) => t(`sketch.tool.${tool}`);
  const tone = pageTone(state.background, state.annotate);
  const textStyle: CSSProperties | undefined = text
    ? {
        left: view.offsetX + text.x * view.scale,
        top: view.offsetY + text.y * view.scale,
        maxWidth: Math.max(120, stage.width - (view.offsetX + text.x * view.scale) - 8),
        font: textFont(text.size * view.scale),
        color: inkColor(text.color, tone),
        textShadow: `0 0 2px ${textHalo(text.color, tone)}, 0 0 1px ${textHalo(text.color, tone)}`,
      }
    : undefined;

  return createPortal(
    <div
      ref={sheetRef}
      className={styles.sheet}
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      tabIndex={-1}
      data-testid="sketch-sheet"
    >
      <header className={styles.top}>
        <button
          type="button"
          className={styles.iconButton}
          onClick={requestCancel}
          aria-label={t('sketch.cancel')}
          disabled={busy}
        >
          <Icon name="close" size={22} />
        </button>
        <h2 id={titleId} className={styles.title}>
          {state.annotate ? t('sketch.annotateTitle') : t('sketch.title')}
        </h2>
        <button
          type="button"
          className={styles.iconButton}
          onClick={() => dispatch({ type: 'undo' })}
          disabled={!canUndo(state.history) || busy}
          aria-label={t('sketch.undo')}
          title={t('sketch.undo')}
        >
          <Glyph name="undo" />
        </button>
        <button
          type="button"
          className={styles.iconButton}
          onClick={() => dispatch({ type: 'redo' })}
          disabled={!canRedo(state.history) || busy}
          aria-label={t('sketch.redo')}
          title={t('sketch.redo')}
        >
          <Glyph name="redo" />
        </button>
        <button
          type="button"
          className={`btn btn-primary ${styles.done}`}
          onClick={() => void done()}
          disabled={empty || busy}
        >
          {busy ? t('sketch.saving') : t('sketch.done')}
        </button>
      </header>

      <div
        ref={stageRef}
        className={styles.stage}
        data-annotate={state.annotate || undefined}
        data-tool={state.tool}
        style={
          {
            '--page': state.annotate ? undefined : BACKGROUND_FILL[state.background],
          } as CSSProperties
        }
      >
        <canvas ref={baseRef} className={styles.canvas} aria-hidden="true" />
        <canvas ref={inkRef} className={styles.canvas} aria-hidden="true" />
        <canvas
          ref={liveRef}
          className={styles.canvas}
          role="img"
          aria-label={t('sketch.canvas', { count: items.length })}
          data-testid="sketch-canvas"
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={(e) => endPointer(e, false)}
          onPointerCancel={(e) => endPointer(e, true)}
          onContextMenu={(e) => e.preventDefault()}
        />
        {empty && !text && (
          <p
            className={styles.hint}
            data-dark={(!state.annotate && state.background === 'dark') || undefined}
          >
            {t('sketch.hint')}
          </p>
        )}
        {text && (
          <input
            ref={textRef}
            className={styles.textInput}
            style={textStyle}
            value={text.value}
            aria-label={t('sketch.textLabel')}
            placeholder={t('sketch.textPlaceholder')}
            enterKeyHint="done"
            onChange={(e) => {
              const next = { ...text, value: e.target.value };
              latest.current.text = next;
              setText(next);
            }}
            onKeyDown={onTextKey}
            onBlur={commitText}
          />
        )}
        {error && (
          <p className={styles.error} role="alert">
            {error}
          </p>
        )}
      </div>

      <div className={styles.dock}>
        <div className={styles.group} role="group" aria-label={t('sketch.tools')}>
          {TOOLS.map((tool) => (
            <button
              key={tool}
              type="button"
              className={styles.tool}
              data-tool={tool}
              aria-pressed={state.tool === tool}
              aria-label={toolLabel(tool)}
              title={toolLabel(tool)}
              onClick={() => {
                commitText();
                dispatch({ type: 'tool', tool });
              }}
            >
              <Glyph name={tool} />
            </button>
          ))}
        </div>
        <div className={styles.group} role="group" aria-label={t('sketch.colors')}>
          {COLOR_NAMES.map((color) => (
            <button
              key={color}
              type="button"
              className={styles.swatch}
              aria-pressed={state.color === color}
              aria-label={t(`sketch.color.${color}`)}
              title={t(`sketch.color.${color}`)}
              onClick={() => {
                if (text) {
                  const next = { ...text, color };
                  latest.current.text = next;
                  setText(next);
                }
                dispatch({ type: 'color', color });
              }}
            >
              <span className={styles.dot} style={{ background: COLORS[color] }} />
            </button>
          ))}
          <span className={styles.divider} aria-hidden="true" />
          {([0, 1, 2] as WidthIndex[]).map((w) => (
            <button
              key={w}
              type="button"
              className={styles.swatch}
              aria-pressed={state.width === w}
              aria-label={t(`sketch.width.${w}`)}
              title={t(`sketch.width.${w}`)}
              onClick={() => dispatch({ type: 'width', width: w })}
            >
              <span
                className={styles.widthDot}
                style={{ width: 4 + WIDTHS[w] * 1.4, height: 4 + WIDTHS[w] * 1.4 }}
              />
            </button>
          ))}
        </div>
        <div className={styles.group} role="group" aria-label={t('sketch.page')}>
          {showBackgrounds &&
            BACKGROUNDS.map((bg: Background) => (
              <button
                key={bg}
                type="button"
                className={styles.swatch}
                aria-pressed={state.background === bg}
                aria-label={t(`sketch.background.${bg}`)}
                title={t(`sketch.background.${bg}`)}
                onClick={() => dispatch({ type: 'background', background: bg })}
              >
                <span className={styles.page} data-bg={bg} />
              </button>
            ))}
          <button
            type="button"
            className={styles.tool}
            onClick={pickPhoto}
            aria-label={t(state.annotate ? 'sketch.photoReplace' : 'sketch.photo')}
            title={t(state.annotate ? 'sketch.photoReplace' : 'sketch.photo')}
          >
            <Glyph name="photo" />
          </button>
          <button
            type="button"
            className={styles.tool}
            onClick={() => setConfirm('clear')}
            disabled={items.length === 0}
            aria-label={t('sketch.clear')}
            title={t('sketch.clear')}
          >
            <Glyph name="clear" />
          </button>
        </div>
      </div>

      <input
        ref={photoRef}
        type="file"
        accept="image/*"
        hidden
        data-testid="sketch-photo"
        onChange={(e) => void onPhoto(e)}
      />

      {confirm === 'cancel' && (
        <ConfirmDialog
          title={t('sketch.discardTitle')}
          message={t('sketch.discardMessage')}
          confirmLabel={t('sketch.discard')}
          danger
          onConfirm={discard}
          onCancel={() => setConfirm(null)}
        />
      )}
      {confirm === 'clear' && (
        <ConfirmDialog
          title={t('sketch.clearTitle')}
          message={t('sketch.clearMessage')}
          confirmLabel={t('sketch.clear')}
          danger
          onConfirm={() => {
            setConfirm(null);
            dispatch({ type: 'clear' });
          }}
          onCancel={() => setConfirm(null)}
        />
      )}
      {confirm === 'photo' && (
        <ConfirmDialog
          title={t('sketch.photoTitle')}
          message={t('sketch.photoMessage')}
          confirmLabel={t('sketch.photoConfirm')}
          onConfirm={() => {
            setConfirm(null);
            photoRef.current?.click();
          }}
          onCancel={() => setConfirm(null)}
        />
      )}
    </div>,
    document.body,
  );
}
