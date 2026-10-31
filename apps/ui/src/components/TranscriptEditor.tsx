import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  type KeyboardEvent,
} from 'react';

import type { TranscriptCorrection } from '../speech/transcribe';
import { autoGrow, mirrorParts } from './transcriptMirror';
import styles from './TranscriptEditor.module.css';

/** Distance from the bottom (px) within which live text keeps the view pinned. */
const STICK_PX = 24;

/**
 * The composer's text box, also used to review and edit transcripts.
 *
 * A mirror element behind the transparent textarea renders `<mark>`s for
 * vocabulary corrections at the same positions. While `live` (dictation in
 * progress) the mirror also draws the text itself so the interim tail can be
 * dimmed, and the view follows new text unless the user scrolled up. The
 * textarea grows with its content (see `autoGrow`).
 */
export function TranscriptEditor({
  id,
  value,
  onChange,
  corrections,
  placeholder,
  describedBy,
  disabled,
  onSubmit,
  live = false,
  tailLength = 0,
  expanded = false,
}: {
  id: string;
  value: string;
  onChange: (value: string) => void;
  corrections: TranscriptCorrection[];
  placeholder: string;
  describedBy?: string;
  disabled?: boolean;
  onSubmit: () => void;
  /** Dictation is writing into the box. */
  live?: boolean;
  /** Trailing characters that are still an interim transcript. */
  tailLength?: number;
  /** Taller box (~65% of the viewport). */
  expanded?: boolean;
}) {
  const mirrorRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  /** Whether the user is at (or near) the bottom; live text then keeps it there. */
  const atBottomRef = useRef(true);

  const syncMirror = () => {
    if (mirrorRef.current && inputRef.current) {
      mirrorRef.current.scrollTop = inputRef.current.scrollTop;
    }
  };

  const resize = useCallback(() => {
    const el = inputRef.current;
    if (!el) return;
    autoGrow(el);
    if (mirrorRef.current) mirrorRef.current.scrollTop = el.scrollTop;
  }, []);

  // Re-measure whenever the text or size mode changes; follow live text.
  useLayoutEffect(() => {
    resize();
    const el = inputRef.current;
    if (el && live && atBottomRef.current) {
      el.scrollTop = el.scrollHeight;
      if (mirrorRef.current) mirrorRef.current.scrollTop = el.scrollTop;
    }
  }, [value, expanded, live, resize]);

  // Width and viewport changes (rotation, on-screen keyboard) change line wrapping.
  useEffect(() => {
    window.addEventListener('resize', resize);
    return () => window.removeEventListener('resize', resize);
  }, [resize]);

  const parts = useMemo(
    () => mirrorParts(value, corrections, live ? tailLength : 0),
    [value, corrections, live, tailLength],
  );

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    // Ctrl/Cmd+Enter sends; plain Enter inserts a newline (safer on mobile).
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      onSubmit();
    }
  };

  const wrapClass = [styles.wrap, live ? styles.live : '', expanded ? styles.expanded : '']
    .filter(Boolean)
    .join(' ');

  return (
    <div className={wrapClass}>
      <div ref={mirrorRef} className={styles.mirror} aria-hidden="true">
        {parts.map((p, i) => {
          const cls = p.tail ? styles.tail : undefined;
          return p.mark ? (
            <mark key={i} className={cls}>
              {p.text}
            </mark>
          ) : (
            <span key={i} className={cls}>
              {p.text}
            </span>
          );
        })}
        {'​'}
      </div>
      <textarea
        ref={inputRef}
        id={id}
        className={styles.input}
        value={value}
        placeholder={placeholder}
        aria-describedby={describedBy}
        disabled={disabled}
        rows={2}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={onKeyDown}
        onScroll={(e) => {
          const el = e.currentTarget;
          atBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight <= STICK_PX;
          syncMirror();
        }}
      />
    </div>
  );
}
