/**
 * Spoken briefing requests: a dictation that is exactly one of these
 * phrases ("현황 알려줘", "status", "what's going on", ...) asks for the
 * status briefing instead of being sent to the agent.
 *
 * Only an exact match after normalization counts (case, accents, spaces
 * and punctuation are ignored), so a real prompt that merely contains a
 * phrase ("status of the build?") is never taken.
 */

const PHRASES = [
  // Korean
  '현황 알려줘',
  '현황 알려 주세요',
  '현황 말해줘',
  '현황 브리핑',
  '상태 알려줘',
  '상태 알려 주세요',
  '상태 말해줘',
  '지금 현황',
  '전체 현황',
  '세션 현황',
  // English
  'status',
  'status update',
  'status report',
  'give me a status',
  'give me the status',
  "what's going on",
  'what is going on',
  'whats up with the sessions',
  'briefing',
  // Japanese
  '状況を教えて',
  '状況教えて',
  '現状を教えて',
  'ステータスを教えて',
  // Spanish
  'estado',
  'dame el estado',
  'informe de estado',
  'qué está pasando',
];

/** Lower case, no accents, no spaces or punctuation. */
export function normalizePhrase(text: string): string {
  return text
    .normalize('NFKD')
    .replace(/\p{M}/gu, '')
    .toLowerCase()
    .replace(/[\s\p{P}\p{S}]/gu, '')
    .normalize('NFC');
}

const NORMALIZED = new Set(PHRASES.map(normalizePhrase));

/** True when the whole `text` is a briefing request. */
export function isBriefingPhrase(text: string): boolean {
  const n = normalizePhrase(text);
  return n !== '' && NORMALIZED.has(n);
}
