/**
 * TypeScript mirror of `vca-core/src/pronunciation.rs`: the pronunciation
 * dictionary of spoken alerts, kept by each PC's server and shared by every
 * device and the PC's own voice.
 */
import type { UnixMillis } from './types';

/** `welcome.features` entry of servers that answer the pronunciation messages. */
export const FEATURE_PRONUNCIATIONS = 'pronunciations';

/** `default`: shipped with VCA; `user`: taught by the user; `auto`: suggested for a session name. */
export type PronunciationSource = 'default' | 'user' | 'auto';

export interface Pronunciation {
  /** The written term (Latin letters and digits). */
  term: string;
  /** What the voice says instead. */
  spoken: string;
  /** ISO 639-1 language of the voice (`ko`, ...). */
  language: string;
  source: PronunciationSource;
  /** 0 for shipped entries. */
  updated_at: UnixMillis;
}

export type PronunciationClientMsg =
  /** View scope; answered with `pronunciations`. `null`: every language. */
  | { type: 'list_pronunciations'; language: string | null }
  /** Command scope; answered with `pronunciations` (also pushed to every client). */
  | { type: 'set_pronunciation'; term: string; spoken: string; language: string | null }
  /** Command scope; `not_found` when there was no entry. */
  | { type: 'delete_pronunciation'; term: string; language: string | null };

/** Every answer of the pronunciation messages, and the push after a change. */
export type PronunciationServerMsg = { type: 'pronunciations'; entries: Pronunciation[] };
