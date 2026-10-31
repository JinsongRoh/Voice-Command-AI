import '@xterm/xterm/css/xterm.css';

import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import { useEffect, useRef } from 'react';

import { useI18n } from '../i18n/context';
import type { SessionEvent } from '../protocol/types';
import styles from './TerminalView.module.css';

/**
 * Raw terminal rendering of `output` events with xterm.js (MON-03).
 * Read-only: input goes through the composer and control keys.
 */
export default function TerminalView({
  sessionId,
  events,
}: {
  sessionId: string;
  events: SessionEvent[];
}) {
  const { t } = useI18n();
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const writtenSeq = useRef(-1);

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
      fontSize: 13,
      scrollback: 5000,
      theme: {
        background: cssVar('--term-bg') || '#0b1430',
        foreground: cssVar('--term-fg') || '#e6ebf5',
      },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    const refit = () => {
      try {
        fit.fit();
      } catch {
        // Not measurable yet (hidden); retried on the next resize.
      }
    };
    refit();
    // xterm caches the cell size: measure again once the bundled JetBrains
    // Mono has loaded (it is fetched lazily, on first use).
    let disposed = false;
    document.fonts
      ?.load(`13px ${fontFamily}`)
      .then(() => {
        if (disposed) return;
        term.options.fontFamily = 'monospace';
        term.options.fontFamily = fontFamily;
        refit();
      })
      .catch(() => {});
    const observer = new ResizeObserver(refit);
    observer.observe(host);
    termRef.current = term;
    writtenSeq.current = -1;
    return () => {
      disposed = true;
      observer.disconnect();
      term.dispose();
      termRef.current = null;
    };
  }, [sessionId]);

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
    for (const e of outputs) {
      if (e.seq > writtenSeq.current) {
        term.write(e.data);
        writtenSeq.current = e.seq;
      }
    }
  }, [events, sessionId]);

  return (
    <div ref={hostRef} className={styles.terminal} role="region" aria-label={t('live.terminal')} />
  );
}
