/**
 * The demo PC's commonly misheard words (same rules as
 * crates/vca-server/src/personal_terms.rs).
 */
import {
  MAX_PERSONAL_TERMS,
  wordProblem,
  type PersonalTerm,
  type PersonalTermsClientMsg,
} from './personalTerms';
import type { ServerMsg } from './types';

export class MockPersonalTerms {
  private terms: PersonalTerm[] = [{ term: 'SkyHarbor', variants: ['sky harbour', '스카이 하버'] }];

  handle(msg: PersonalTermsClientMsg): ServerMsg {
    if (msg.type === 'list_personal_terms') return { type: 'personal_terms', terms: this.terms };
    const terms = msg.terms.map((t) => ({
      term: t.term.trim(),
      variants: t.variants.map((v) => v.trim()),
    }));
    const refused =
      terms.length > MAX_PERSONAL_TERMS ||
      terms.some((t, i) => wordProblem(t, terms.slice(0, i)) !== null);
    if (refused) return { type: 'error', code: 'invalid_request', message: 'invalid word list' };
    this.terms = terms;
    return { type: 'personal_terms', terms };
  }
}
