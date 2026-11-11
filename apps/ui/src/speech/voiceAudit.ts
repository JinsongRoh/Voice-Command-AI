/**
 * Client-side record of decisions answered by voice. The `choose` message
 * has no origin field, so the server audits a voice answer exactly like a
 * tap; this device keeps its own note that the answer was spoken.
 */
import type { SessionId } from '../protocol/types';

export interface VoiceAnswerRecord {
  at: number;
  sessionId: SessionId;
  decisionId: string;
  optionId: string;
  /** What was recognized (normalized vocabulary word, e.g. "허용"). */
  heard: 'allow' | 'deny';
}

const STORAGE_KEY = 'vca.voiceAnswers.v1';
/** Newest entries kept on the device. */
export const VOICE_AUDIT_ENTRIES = 100;

export function loadVoiceAnswers(): VoiceAnswerRecord[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? (parsed as VoiceAnswerRecord[]) : [];
  } catch {
    return [];
  }
}

/** Appends one voice answer (oldest entries are dropped). */
export function recordVoiceAnswer(entry: VoiceAnswerRecord): void {
  const next = [...loadVoiceAnswers(), entry].slice(-VOICE_AUDIT_ENTRIES);
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Storage may be unavailable (private mode); the server audit still has the answer.
  }
}
