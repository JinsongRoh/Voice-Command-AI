import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  type KeyboardEvent,
} from 'react';

import { subscribeViewport } from '../hooks/visualViewport';
import type { TranscriptCorrection } from '../speech/transcribe';
import { autoGrow, caretOffsetTop, mirrorParts } from './transcriptMirror';
import styles from './TranscriptEditor.module.css';

/** Distance from the bottom (px) within which live text keeps the view pinned. */
const STICK_PX = 24;

/** Space (px) kept between the caret line and the box's edge when revealing it. */
const CARET_MARGIN_PX = 8;

/**
 * Scrolls the box (never the page) so the caret's line is inside it. The
 * mirror renders the same text with the same wrapping, so the caret's
 * position is measured there.
 */
function revealCaret(el: HTMLTextAreaElement, mirror: HTMLElement | null) {
  const caret = el.selectionEnd ?? el.value.length;
  if (caret >= el.value.length) {
    el.scrollTop = el.scrollHeight;
    if (mirror) mirror.scrollTop = el.scrollTop;
    return;
  }
  const top = mirror ? caretOffsetTop(mirror, caret) : null;
  if (top === null) return;
  const line = parseFloat(getComputedStyle(el).lineHeight) || 22;
  if (top - CARET_MARGIN_PX < el.scrollTop) {
    el.scrollTop = Math.max(0, top - CARET_MARGIN_PX);
  } else if (top + line + CARET_MARGIN_PX > el.scrollTop + el.clientHeight) {
    el.scrollTop = top + line + CARET_MARGIN_PX - el.clientHeight;
  }
  if (mirror) mirror.scrollTop = el.scrollTop;
}

/**
 * The composer's text box, also used to review and edit transcripts.
 *
 * A mirror element behind the transparent textarea renders `<mark>`s for
 * vocabulary corrections at the same positions. While `live` (dictation in
 * progress) the mirror also draws the text itself so the interim tail can be
 * dimmed, and the view follows new text unless the user scrolled up. The
 * textarea grows with its content (see `autoGrow`).
 */
/**
 * Whether plain Enter sends: true on desktops, where the primary pointer is a
 * mouse or trackpad that can hover; false on phones and tablets.
 */
function enterSends(): boolean {
  try {
    return globalThis.matchMedia?.('(hover: hover) and (pointer: fine)').matches ?? false;
  } catch {
    return false;
  }
}

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
  compact = false,
  onKeyCapture,
  combobox,
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
  /**
   * Typing with the on-screen keyboard open: the box takes the room left
   * above the keyboard (and scrolls inside), keeping the caret in view.
   */
  compact?: boolean;
  /** Sees keys first (a suggestion list); `true`: handled, nothing else happens. */
  onKeyCapture?: (e: KeyboardEvent<HTMLTextAreaElement>) => boolean;
  /** Suggestion list attributes while one is open (`@` mentions). */
  combobox?: { listId: string; expanded: boolean; activeId?: string };
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

  /** Keeps the caret visible while the person edits (not during dictation). */
  const keepCaret = useCallback(() => {
    const el = inputRef.current;
    if (!el || live || document.activeElement !== el) return;
    revealCaret(el, mirrorRef.current);
  }, [live]);

  // Re-measure whenever the text or size mode changes; follow live text.
  useLayoutEffect(() => {
    resize();
    const el = inputRef.current;
    if (el && live && atBottomRef.current) {
      el.scrollTop = el.scrollHeight;
      if (mirrorRef.current) mirrorRef.current.scrollTop = el.scrollTop;
    } else {
      // Editing, or the box changed size around the caret (keyboard,
      // compact layout): keep the caret's line in view.
      keepCaret();
    }
  }, [value, expanded, compact, live, resize, keepCaret]);

  // Width and viewport changes (rotation, on-screen keyboard) change line
  // wrapping and the room the box has. iOS reports the keyboard only through
  // the visual viewport, so follow that as well as the window.
  useEffect(() => {
    let frame = 0;
    const onChange = () => {
      resize();
      cancelAnimationFrame(frame);
      // After the layout settled at the new size.
      frame = requestAnimationFrame(keepCaret);
    };
    window.addEventListener('resize', onChange);
    const unsubscribe = subscribeViewport(onChange);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener('resize', onChange);
      unsubscribe();
    };
  }, [resize, keepCaret]);

  const parts = useMemo(
    () => mirrorParts(value, corrections, live ? tailLength : 0),
    [value, corrections, live, tailLength],
  );

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (onKeyCapture?.(e)) return;
    if (e.key !== 'Enter') return;
    // Never act while an IME (Korean, Japanese, ...) is still composing:
    // that Enter only commits the composed characters.
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    // Ctrl/Cmd+Enter always sends. On desktops (a mouse or trackpad) plain
    // Enter sends and Shift+Enter inserts a newline; on touch devices plain
    // Enter inserts a newline, which is safer with on-screen keyboards.
    const send = e.ctrlKey || e.metaKey || (!e.shiftKey && !e.altKey && enterSends());
    if (send) {
      e.preventDefault();
      onSubmit();
    }
  };

  const wrapClass = [
    styles.wrap,
    live ? styles.live : '',
    expanded ? styles.expanded : '',
    compact ? styles.compact : '',
  ]
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
        aria-autocomplete={combobox ? 'list' : undefined}
        aria-controls={combobox?.expanded ? combobox.listId : undefined}
        aria-activedescendant={combobox?.expanded ? combobox.activeId : undefined}
        disabled={disabled}
        rows={2}
        onChange={(e) => onChange(e.target.value)}
        onFocus={() => requestAnimationFrame(keepCaret)}
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
