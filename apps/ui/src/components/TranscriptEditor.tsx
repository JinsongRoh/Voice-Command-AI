import { useMemo, useRef, type KeyboardEvent } from 'react';

import { correctionRanges, type TranscriptCorrection } from '../speech/transcribe';
import styles from './TranscriptEditor.module.css';

/**
 * Textarea with highlighted vocabulary corrections. A mirror element behind
 * the transparent textarea renders `<mark>`s at the same positions.
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
}: {
  id: string;
  value: string;
  onChange: (value: string) => void;
  corrections: TranscriptCorrection[];
  placeholder: string;
  describedBy?: string;
  disabled?: boolean;
  onSubmit: () => void;
}) {
  const mirrorRef = useRef<HTMLDivElement>(null);
  const parts = useMemo(() => {
    const ranges = correctionRanges(value, corrections);
    const out: { text: string; mark: boolean }[] = [];
    let pos = 0;
    for (const r of ranges) {
      if (r.start > pos) out.push({ text: value.slice(pos, r.start), mark: false });
      out.push({ text: value.slice(r.start, r.end), mark: true });
      pos = r.end;
    }
    out.push({ text: value.slice(pos), mark: false });
    return out;
  }, [value, corrections]);

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    // Ctrl/Cmd+Enter sends; plain Enter inserts a newline (safer on mobile).
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      onSubmit();
    }
  };

  return (
    <div className={styles.wrap}>
      <div ref={mirrorRef} className={styles.mirror} aria-hidden="true">
        {parts.map((p, i) =>
          p.mark ? <mark key={i}>{p.text}</mark> : <span key={i}>{p.text}</span>,
        )}
        {'​'}
      </div>
      <textarea
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
          if (mirrorRef.current) mirrorRef.current.scrollTop = e.currentTarget.scrollTop;
        }}
      />
    </div>
  );
}
