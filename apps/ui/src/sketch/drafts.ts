/**
 * Unsent sketches, kept in memory per session while the sheet is closed
 * (Escape, switching sessions) until they are sent or discarded. Nothing is
 * written to storage: a reload drops them.
 */
import type { SketchState } from './model';

export interface SketchDraft {
  state: SketchState;
  /** The annotated photo, already scaled to the document size. */
  image: CanvasImageSource | null;
  /** Frees the decoded photo (an `ImageBitmap`) when the draft is dropped. */
  releaseImage?: () => void;
}

const drafts = new Map<string, SketchDraft>();

export function getDraft(key: string): SketchDraft | undefined {
  return drafts.get(key);
}

export function hasDraft(key: string): boolean {
  return drafts.has(key);
}

export function saveDraft(key: string, draft: SketchDraft) {
  const previous = drafts.get(key);
  if (previous && previous.image !== draft.image) previous.releaseImage?.();
  drafts.set(key, draft);
}

/** Drops a draft; `keepImage` when the photo is still on screen. */
export function deleteDraft(key: string, keepImage?: CanvasImageSource | null) {
  const previous = drafts.get(key);
  if (previous && previous.image !== keepImage) previous.releaseImage?.();
  drafts.delete(key);
}

/** Tests only. */
export function clearDrafts() {
  for (const d of drafts.values()) d.releaseImage?.();
  drafts.clear();
}
