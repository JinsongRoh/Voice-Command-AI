/**
 * TypeScript mirror of `vca-core/src/personal_terms.rs`: the "commonly
 * misheard words" of server-side speech, kept by each PC's server. The list
 * is saved as a whole.
 */

/** `welcome.features` entry of servers that answer the messages below. */
export const FEATURE_PERSONAL_TERMS = 'personal_terms';

/** Same limits as the server. */
export const MAX_PERSONAL_TERMS = 500;
export const MAX_PERSONAL_VARIANTS = 16;
export const MAX_PERSONAL_TERM_CHARS = 64;

export interface PersonalTerm {
  /** The correct spelling. */
  term: string;
  /** What the recognizer writes instead (may be empty). */
  variants: string[];
}

export type PersonalTermsClientMsg =
  /** View scope; answered with `personal_terms`. */
  | { type: 'list_personal_terms' }
  /** Admin scope, audited; replaces the list. Answered with `personal_terms`. */
  | { type: 'save_personal_terms'; terms: PersonalTerm[] };

export type PersonalTermsServerMsg = { type: 'personal_terms'; terms: PersonalTerm[] };

/** Two spellings with one key count as the same word (server: `text::normalize`). */
export function termKey(text: string): string {
  return text.normalize('NFKC').toLowerCase().replace(/\s+/g, ' ').trim();
}

/** The forms typed into one field: separated by commas or new lines. */
export function splitForms(text: string): string[] {
  return text
    .split(/[,，、\n]/u)
    .map((form) => form.trim())
    .filter(Boolean);
}

/** The first rule a word breaks, with the text it applies to. */
export type WordProblem =
  | { rule: 'empty' }
  | { rule: 'tooLong'; text: string }
  | { rule: 'tooManyForms' }
  | { rule: 'duplicateWord'; text: string }
  | { rule: 'formTooShort'; text: string }
  | { rule: 'formIsWord' }
  | { rule: 'duplicateForm'; text: string };

/** Checks one word against the rest of the list (same rules as the server). */
export function wordProblem(word: PersonalTerm, others: PersonalTerm[]): WordProblem | null {
  const term = word.term.trim();
  if (!term) return { rule: 'empty' };
  if ([...term].length > MAX_PERSONAL_TERM_CHARS) return { rule: 'tooLong', text: term };
  if (word.variants.length > MAX_PERSONAL_VARIANTS) return { rule: 'tooManyForms' };
  if (others.some((o) => termKey(o.term) === termKey(term)))
    return { rule: 'duplicateWord', text: term };
  const seen = new Set(others.flatMap((o) => o.variants.map(termKey)));
  for (const form of word.variants) {
    if ([...form].length > MAX_PERSONAL_TERM_CHARS) return { rule: 'tooLong', text: form };
    const key = termKey(form);
    if ([...key].length < 2) return { rule: 'formTooShort', text: form };
    if (form === term) return { rule: 'formIsWord' };
    if (seen.has(key)) return { rule: 'duplicateForm', text: form };
    seen.add(key);
  }
  return null;
}
