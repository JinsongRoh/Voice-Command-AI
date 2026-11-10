/**
 * One voice per decision. A pending decision can be announced by the voice
 * queue (the spoken alert, `alert:<key>`) and read out by hands-free mode
 * (the decision card, with its choices). Hands-free mode claims the
 * decision when it starts reading: the queued alert for it is dropped, and
 * an alert that comes later is not spoken.
 */
import { voiceQueue } from './voiceQueue';

/** How long a claim lasts (a decision is answered well within this). */
const CLAIM_MS = 30 * 60_000;
const MAX_CLAIMS = 200;

const claims = new Map<string, number>();

/** Queue key of the spoken alert of a pending decision (`approvals.ts` keys). */
export function decisionVoiceKey(sessionId: string, decisionId: string): string {
  return `alert:d:${sessionId}:${decisionId}`;
}

function prune(now: number) {
  for (const [key, at] of claims) {
    if (now - at > CLAIM_MS || claims.size > MAX_CLAIMS) claims.delete(key);
  }
}

/** Hands-free mode reads this decision: the queue does not announce it as well. */
export function claimDecisionVoice(key: string, now: number = Date.now()): void {
  claims.set(key, now);
  prune(now);
  voiceQueue.drop(key);
}

/** True when hands-free mode has claimed (is reading or has read) this decision. */
export function decisionVoiceClaimed(key: string, now: number = Date.now()): boolean {
  const at = claims.get(key);
  return at !== undefined && now - at <= CLAIM_MS;
}

/** Forgets every claim (tests). */
export function resetDecisionVoice(): void {
  claims.clear();
}
