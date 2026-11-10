/** Drafts of question form answers (see `QuestionForm`). */
import type { Question, QuestionAnswer } from '../protocol/types';

/** Claude Code marks its suggestion in the label. */
const RECOMMENDED = /\s*\((recommended|추천|권장)\)\s*$/i;

/** The label without the "(Recommended)" suffix, and whether it had one. */
export function splitRecommended(label: string): { label: string; recommended: boolean } {
  const recommended = RECOMMENDED.test(label);
  return { label: recommended ? label.replace(RECOMMENDED, '') : label, recommended };
}

/** One question's draft: picked options and typed text. */
export interface Draft {
  options: number[];
  text: string;
}

export const EMPTY_DRAFT: Draft = { options: [], text: '' };

/** Whether a draft is a complete answer. */
export function answered(q: Question, d: Draft): boolean {
  return d.options.length > 0 || (q.free_text && d.text.trim() !== '');
}

/** The wire answer of a draft. Single choice: an option or the text. */
export function toAnswer(q: Question, d: Draft): QuestionAnswer {
  const text = q.free_text && d.text.trim() !== '' ? d.text.trim() : null;
  if (q.multi_select) return { options: [...d.options].sort((a, b) => a - b), text };
  return text !== null && d.options.length === 0
    ? { options: [], text }
    : { options: d.options.slice(0, 1), text: null };
}
