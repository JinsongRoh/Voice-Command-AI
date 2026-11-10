/**
 * Unsent composer text per session. The composer is remounted for every
 * session (`key={session.id}`), so a draft typed in one session would be lost
 * when the user switches to another and back. Kept in memory only: drafts
 * never reach storage and are gone when the app restarts.
 */
import type { TranscriptCorrection } from '../speech/transcribe';

export interface ComposerDraft {
  text: string;
  corrections: TranscriptCorrection[];
}

const drafts = new Map<string, ComposerDraft>();

export function loadDraft(sessionId: string): ComposerDraft | undefined {
  return drafts.get(sessionId);
}

/** Stores the draft, or forgets it when the box is empty. */
export function saveDraft(sessionId: string, draft: ComposerDraft): void {
  if (draft.text.trim() === '') drafts.delete(sessionId);
  else drafts.set(sessionId, { text: draft.text, corrections: [...draft.corrections] });
}

export function clearDraft(sessionId: string): void {
  drafts.delete(sessionId);
}

/** Forgets every draft (tests). */
export function clearAllDrafts(): void {
  drafts.clear();
}

const draftListeners = new Map<string, Set<() => void>>();

/**
 * Puts `text` after the draft of `sessionId` and tells an open composer of
 * that session to show it (the header's "talk to the conductor": the
 * transcript lands in the conductor's box for review, never sent).
 */
export function appendToDraft(sessionId: string, text: string): void {
  const value = text.trim();
  if (!value) return;
  const draft = drafts.get(sessionId);
  const before = draft?.text.trim() ?? '';
  saveDraft(sessionId, {
    text: before ? `${before} ${value}` : value,
    corrections: draft?.corrections ?? [],
  });
  for (const l of draftListeners.get(sessionId) ?? []) l();
}

/**
 * Puts `text` into the draft of `sessionId` on its own lines ("Send to
 * session…": a message or terminal output copied from another session). It
 * is only shown in that session's box, never sent.
 */
export function insertIntoDraft(sessionId: string, text: string): void {
  const value = text.replace(/\s+$/, '');
  if (!value.trim()) return;
  const draft = drafts.get(sessionId);
  const before = draft?.text.replace(/\s+$/, '') ?? '';
  saveDraft(sessionId, {
    text: before ? `${before}\n\n${value}` : value,
    corrections: draft?.corrections ?? [],
  });
  for (const l of draftListeners.get(sessionId) ?? []) l();
}

/** Called when `appendToDraft` or `insertIntoDraft` changed the draft of `sessionId`. */
export function onDraftAppended(sessionId: string, listener: () => void): () => void {
  let set = draftListeners.get(sessionId);
  if (!set) draftListeners.set(sessionId, (set = new Set()));
  set.add(listener);
  return () => {
    set.delete(listener);
    if (set.size === 0) draftListeners.delete(sessionId);
  };
}
