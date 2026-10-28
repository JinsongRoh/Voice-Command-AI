import type { Locale } from '../i18n/i18n';
import type { SttEngine } from '../state/settings';

/** A term replaced by the developer vocabulary engine (VOC-05). */
export interface TranscriptCorrection {
  /** What the recognizer heard. */
  from: string;
  /** The vocabulary term it was replaced with (appears in `text`). */
  to: string;
  layer: 'base' | 'project' | 'personal';
}

export interface Transcript {
  text: string;
  corrections: TranscriptCorrection[];
}

export interface TranscribeOptions {
  engine: SttEngine;
  locale: Locale;
  sessionId: string | null;
}

/**
 * Speech-to-text entry point injected into the UI. The real implementation
 * will send the audio to the server's speech pipeline (vca-speech) or run an
 * on-device engine; the mock returns canned transcripts.
 */
export type TranscribeFn = (audio: Blob, options: TranscribeOptions) => Promise<Transcript>;

const CANNED: Transcript[] = [
  {
    text: 'Run pnpm test and fix the useEffect warning in Settings.tsx',
    corrections: [
      { from: 'p n p m', to: 'pnpm', layer: 'base' },
      { from: 'use effect', to: 'useEffect', layer: 'base' },
      { from: 'settings dot tsx', to: 'Settings.tsx', layer: 'project' },
    ],
  },
  {
    text: 'Check the kubectl rollout status for payments-api and summarize it',
    corrections: [
      { from: 'kube control', to: 'kubectl', layer: 'base' },
      { from: 'payments API', to: 'payments-api', layer: 'project' },
    ],
  },
  {
    text: 'Commit the changes and open a PR against main',
    corrections: [{ from: 'P are', to: 'PR', layer: 'personal' }],
  },
];

export function createMockTranscriber(delayMs = 900): TranscribeFn {
  let i = 0;
  return (_audio, _options) =>
    new Promise((resolve) => {
      const result = CANNED[i % CANNED.length]!;
      i += 1;
      setTimeout(() => resolve(result), delayMs);
    });
}

/** Character ranges of correction terms in `text`, for highlighting. */
export function correctionRanges(
  text: string,
  corrections: TranscriptCorrection[],
): { start: number; end: number }[] {
  const ranges: { start: number; end: number }[] = [];
  for (const c of corrections) {
    if (!c.to) continue;
    let from = 0;
    for (;;) {
      const idx = text.indexOf(c.to, from);
      if (idx < 0) break;
      ranges.push({ start: idx, end: idx + c.to.length });
      from = idx + c.to.length;
    }
  }
  ranges.sort((a, b) => a.start - b.start);
  // Drop overlaps.
  const merged: { start: number; end: number }[] = [];
  for (const r of ranges) {
    const last = merged[merged.length - 1];
    if (last && r.start < last.end) continue;
    merged.push(r);
  }
  return merged;
}
