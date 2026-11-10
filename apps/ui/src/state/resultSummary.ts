/**
 * Spoken result summaries (opt-in, per device): when a session's turn ends
 * (it goes from running to idle or to waiting for input), the PC summarizes
 * the agent's last answer in one or two sentences in the listener's
 * language, and the summary is read aloud on the voice queue instead of the
 * plain "finished" alert.
 *
 * Nothing is requested while the setting (or spoken alerts) is off. When
 * the PC gives no LLM summary (no model, an older server, an error, a
 * limit), the local "session N, <topic>, finished" line is spoken instead.
 */
import type { ClientMsg, ServerMsg, SessionId, SessionState } from '../protocol/types';
import { normalizeKoreanNumbers } from '../speech/koreanNumbers';
import { inQuietHours, type Settings } from './settings';

/** A turn ended: running before, idle or waiting for input now. */
export function turnEnded(prev: SessionState, next: SessionState): boolean {
  return prev.type === 'running' && (next.type === 'idle' || next.type === 'awaiting_input');
}

/** Whether this state change should be summarized aloud for this device. */
export function shouldSummarize(
  settings: Settings,
  prev: SessionState,
  next: SessionState,
  now: Date,
): boolean {
  return (
    settings.spokenAlerts &&
    settings.resultSummary &&
    turnEnded(prev, next) &&
    !inQuietHours(settings.quietHours, now)
  );
}

export interface SummaryDeps {
  request(body: ClientMsg): Promise<ServerMsg>;
  /** Queues the text on the voice queue. */
  speak(text: string): void;
  language: string;
}

/**
 * Asks the PC for the summary of `sessionId` and speaks it, else
 * `fallback`. Resolves with the spoken text. Never rejects.
 */
export async function speakResultSummary(
  deps: SummaryDeps,
  sessionId: SessionId,
  number: number | null,
  fallback: string,
): Promise<string> {
  let text = fallback;
  try {
    const res = await deps.request({
      type: 'summarize',
      session_id: sessionId,
      language: deps.language,
      number,
    });
    if (res.type === 'summary' && !res.fallback && res.text.trim()) text = res.text;
  } catch {
    // The local line is spoken.
  }
  const spoken = normalizeKoreanNumbers(text, deps.language);
  deps.speak(spoken);
  return spoken;
}
