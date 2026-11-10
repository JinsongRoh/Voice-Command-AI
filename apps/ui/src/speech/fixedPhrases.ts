/**
 * The fixed spoken prompts of hands-free mode and Voco, most urgent first:
 * the "네" after the wake word, then the short results, then the rest.
 * They are synthesized ahead in the PC voice (`alertVoice.warm`), so they
 * play at once and in the same voice as everything else.
 */
import { PHRASES, type VoiceLang } from './handsFree';
import { VOCO_PHRASES } from './vocoFlow';

export function fixedPhrases(lang: VoiceLang): string[] {
  const p = PHRASES[lang];
  const v = VOCO_PHRASES[lang];
  const list = [
    v.ack,
    p.sent,
    p.cancelled,
    p.approved,
    p.denied,
    p.checkScreen,
    v.recalled,
    v.whichSession,
    v.tooLate,
    v.noSession,
    p.kept,
    p.sendRetry,
    p.sendChoices,
    p.decisionRetry,
    p.changed,
    p.keptAttachments,
    p.keptChanged,
    p.keptFailed,
    v.locked,
  ];
  return [...new Set(list)];
}
