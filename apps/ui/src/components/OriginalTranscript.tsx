import { useId, useMemo, useState } from 'react';

import { useI18n } from '../i18n/context';
import { correctionSpans, type TranscriptCorrection } from '../speech/transcribe';
import { Icon } from './Icon';
import styles from './OriginalTranscript.module.css';

/**
 * "Original" toggle for the last dictation: shows the raw recognizer output
 * (before filler removal and corrections) read-only, with every corrected
 * term highlighted and tagged with the layer that changed it (vocabulary
 * layers today, a contextual LLM pass later). "Use original" swaps the
 * dictated text in the composer for the raw text.
 */
export function OriginalTranscript({
  raw,
  corrections,
  onUseOriginal,
}: {
  raw: string;
  corrections: TranscriptCorrection[];
  /** Absent when the dictated text can no longer be found in the composer. */
  onUseOriginal?: () => void;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const panelId = useId();

  const parts = useMemo(() => {
    const out: { text: string; correction?: TranscriptCorrection }[] = [];
    let pos = 0;
    for (const s of correctionSpans(raw, corrections, 'from')) {
      if (s.start > pos) out.push({ text: raw.slice(pos, s.start) });
      out.push({ text: raw.slice(s.start, s.end), correction: s.correction });
      pos = s.end;
    }
    if (pos < raw.length) out.push({ text: raw.slice(pos) });
    return out;
  }, [raw, corrections]);

  return (
    <div className={styles.original}>
      <button
        type="button"
        className={styles.toggle}
        aria-expanded={open}
        aria-controls={panelId}
        onClick={() => setOpen((v) => !v)}
      >
        <Icon name={open ? 'up' : 'down'} size={14} />
        {t(open ? 'composer.originalHide' : 'composer.originalShow')}
      </button>
      {open && (
        <div
          id={panelId}
          className={styles.panel}
          role="region"
          aria-label={t('composer.originalTitle')}
        >
          <p className={styles.raw} tabIndex={0}>
            {parts.map((p, i) =>
              p.correction ? (
                <mark
                  key={i}
                  className={styles.term}
                  data-layer={p.correction.layer}
                  tabIndex={0}
                  title={t('composer.correctedTo', {
                    to: p.correction.to,
                    layer: t(`correction.layer.${p.correction.layer}`),
                  })}
                >
                  {p.text}
                  <span className={styles.tag}>
                    {' → '}
                    {p.correction.to} · {t(`correction.layer.${p.correction.layer}`)}
                  </span>
                </mark>
              ) : (
                <span key={i}>{p.text}</span>
              ),
            )}
          </p>
          {onUseOriginal && (
            <button type="button" className="btn btn-sm" onClick={onUseOriginal}>
              {t('composer.useOriginal')}
            </button>
          )}
        </div>
      )}
    </div>
  );
}
