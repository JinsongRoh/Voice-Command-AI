import '@xterm/xterm/css/xterm.css';

import { Terminal } from '@xterm/xterm';
import { useEffect, useRef, useState } from 'react';

import { useI18n } from '../i18n/context';
import { decodeOutput } from '../protocol/helpers';
import type { SessionEvent } from '../protocol/types';
import {
  DEFAULT_GEOMETRY,
  firstSizeMarker,
  fitScreenFont,
  loadZoom,
  measureCharRatio,
  measureLineRatio,
  parseSizeMarker,
  pinchZoom,
  type PtyGeometry,
  saveZoom,
  SIZE_OSC,
  stepZoom,
} from './terminalFit';
import { TerminalToolbar } from './TerminalToolbar';
import styles from './TerminalView.module.css';

/** Largest font of the desktop terminal; it shrinks to fit a smaller pane. */
const DESKTOP_FONT_PX = 13;
/** Smallest font of the desktop terminal; a smaller pane scrolls. */
const DESKTOP_MIN_FONT_PX = 8;
/** Room xterm keeps for its scrollbar at the right edge. */
const SCROLLBAR_PX = 14;

function distance(a: Touch, b: Touch): number {
  return Math.hypot(a.clientX - b.clientX, a.clientY - b.clientY);
}

/**
 * Raw terminal rendering of `output` events with xterm.js (MON-03).
 * Read-only: input goes through the composer, the phone terminal's input
 * row and the control keys.
 *
 * The terminal always has the PTY's columns and rows, which the server sends
 * in the output (vca-server `term_size.rs`): programs such as Claude Code
 * redraw with relative cursor moves and rely on wrapping at exactly that
 * width, so a terminal fitted to the pane instead draws lines over each
 * other. The font scales so the screen fits the pane; viewers never resize
 * the PTY (a phone and the PC watching together cannot fight over it).
 *
 * `fitWidth` (phones): adds pinch zoom and sideways scrolling.
 * Text can be selected and sent to another session.
 */
export default function TerminalView({
  sessionId,
  events,
  fitWidth = false,
  label,
}: {
  sessionId: string;
  events: SessionEvent[];
  fitWidth?: boolean;
  /** Accessible name (default: "Terminal"). */
  label?: string;
}) {
  const { t } = useI18n();
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const writtenSeq = useRef(-1);
  const [selection, setSelection] = useState('');
  const [zoom, setZoom] = useState(loadZoom);
  const zoomRef = useRef(zoom);
  const refitRef = useRef<() => void>(() => {});
  /** The PTY's size, from the server's size markers. */
  const geometryRef = useRef<PtyGeometry>(DEFAULT_GEOMETRY);

  useEffect(() => {
    zoomRef.current = zoom;
    refitRef.current();
  }, [zoom]);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const rootStyle = getComputedStyle(document.documentElement);
    const cssVar = (name: string) => rootStyle.getPropertyValue(name).trim();
    const fontFamily = cssVar('--mono') || 'monospace';
    const term = new Terminal({
      disableStdin: true,
      cursorBlink: false,
      convertEol: false,
      fontFamily,
      fontSize: DESKTOP_FONT_PX,
      scrollback: 5000,
      theme: {
        background: cssVar('--term-bg') || '#0b1430',
        foreground: cssVar('--term-fg') || '#e6ebf5',
      },
    });
    term.open(host);
    let ratio = measureCharRatio(fontFamily);
    let lineRatio = measureLineRatio(fontFamily);
    const refit = () => {
      try {
        const { cols, rows } = geometryRef.current;
        if (term.cols !== cols || term.rows !== rows) term.resize(cols, rows);
        const style = getComputedStyle(host);
        const width =
          host.clientWidth -
          (parseFloat(style.paddingLeft) || 0) -
          (parseFloat(style.paddingRight) || 0) -
          (fitWidth ? 0 : SCROLLBAR_PX);
        const height =
          host.clientHeight -
          (parseFloat(style.paddingTop) || 0) -
          (parseFloat(style.paddingBottom) || 0);
        if (!(width > 0)) return; // Hidden; refitted when it is shown.
        const size = fitScreenFont({
          width,
          height,
          cols,
          rows,
          charRatio: ratio,
          lineRatio,
          ...(fitWidth
            ? { zoom: zoomRef.current }
            : { min: DESKTOP_MIN_FONT_PX, max: DESKTOP_FONT_PX }),
        });
        if (term.options.fontSize !== size) term.options.fontSize = size;
      } catch {
        // Not measurable yet (hidden); retried on the next resize.
      }
    };
    refitRef.current = refit;
    refit();
    // The server's size marker: the PTY's columns and rows (see above).
    const sizeSub = term.parser.registerOscHandler(SIZE_OSC, (payload) => {
      const next = parseSizeMarker(payload);
      if (next) {
        const cur = geometryRef.current;
        if (cur.cols !== next.cols || cur.rows !== next.rows) {
          geometryRef.current = next;
          // Synchronous: the rest of the chunk is laid out for the new size.
          refit();
        }
      }
      return true;
    });
    // xterm caches the cell size: measure again once the bundled JetBrains
    // Mono has loaded (it is fetched lazily, on first use).
    let disposed = false;
    document.fonts
      ?.load(`${DESKTOP_FONT_PX}px ${fontFamily}`)
      .then(() => {
        if (disposed) return;
        ratio = measureCharRatio(fontFamily);
        lineRatio = measureLineRatio(fontFamily);
        term.options.fontFamily = 'monospace';
        term.options.fontFamily = fontFamily;
        refit();
      })
      .catch(() => {});
    const observer = new ResizeObserver(refit);
    observer.observe(host);
    const selectionSub = term.onSelectionChange(() => setSelection(term.getSelection()));

    // Pinch zoom (two fingers); one finger still scrolls.
    let pinch: { distance: number; zoom: number } | null = null;
    const onTouchStart = (e: TouchEvent) => {
      if (!fitWidth || e.touches.length !== 2) return;
      pinch = { distance: distance(e.touches[0]!, e.touches[1]!), zoom: zoomRef.current };
    };
    const onTouchMove = (e: TouchEvent) => {
      if (!pinch || e.touches.length !== 2) return;
      e.preventDefault();
      zoomRef.current = pinchZoom(
        pinch.zoom,
        pinch.distance,
        distance(e.touches[0]!, e.touches[1]!),
      );
      refit();
    };
    const onTouchEnd = () => {
      if (!pinch) return;
      pinch = null;
      setZoom(zoomRef.current);
      saveZoom(zoomRef.current);
    };
    host.addEventListener('touchstart', onTouchStart, { passive: true });
    host.addEventListener('touchmove', onTouchMove, { passive: false });
    host.addEventListener('touchend', onTouchEnd);
    host.addEventListener('touchcancel', onTouchEnd);

    termRef.current = term;
    writtenSeq.current = -1;
    return () => {
      disposed = true;
      refitRef.current = () => {};
      observer.disconnect();
      selectionSub.dispose();
      sizeSub.dispose();
      host.removeEventListener('touchstart', onTouchStart);
      host.removeEventListener('touchmove', onTouchMove);
      host.removeEventListener('touchend', onTouchEnd);
      host.removeEventListener('touchcancel', onTouchEnd);
      term.dispose();
      termRef.current = null;
      setSelection('');
    };
  }, [sessionId, fitWidth]);

  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    const outputs = events.filter((e) => e.type === 'output');
    const last = outputs[outputs.length - 1];
    // The view was reloaded (stream gap): start over.
    if (!last || last.seq < writtenSeq.current) {
      term.reset();
      writtenSeq.current = -1;
    }
    // Starting over (also mid-session, from a page of history): take the
    // PTY's size before writing, so the first lines are not laid out wrong.
    if (writtenSeq.current < 0) {
      const found = firstSizeMarker(
        (function* () {
          for (const e of outputs) yield decodeOutput(e.data);
        })(),
      );
      geometryRef.current = found ?? DEFAULT_GEOMETRY;
      refitRef.current();
    }
    for (const e of outputs) {
      if (e.seq > writtenSeq.current) {
        term.write(e.data);
        writtenSeq.current = e.seq;
      }
    }
  }, [events, sessionId, fitWidth]);

  const changeZoom = (next: number) => {
    setZoom(next);
    saveZoom(next);
  };

  return (
    <div className={styles.wrap}>
      <TerminalToolbar
        selection={selection}
        zoomable={fitWidth}
        zoom={zoom}
        onZoom={(direction) => changeZoom(stepZoom(zoom, direction))}
        onFit={() => changeZoom(1)}
      />
      <div
        ref={hostRef}
        className={styles.terminal}
        data-fit-width={fitWidth || undefined}
        role="region"
        aria-label={label ?? t('live.terminal')}
      />
    </div>
  );
}
