/**
 * What protects the person's input from an automatic session switch: text
 * in the composer, dictation, recent typing, an open dialog.
 */
import { hasOpenModal } from '../components/Modal';

/** Keys pressed this recently count as typing. */
export const TYPING_WINDOW_MS = 4000;

export interface InputActivity {
  hasText: boolean;
  dictating: boolean;
  lastInputAt: number;
  modalOpen: boolean;
}

/** Pure: whether switching now could take the screen from the person. */
export function isBusy(activity: InputActivity, now: number): boolean {
  return (
    activity.hasText ||
    activity.dictating ||
    activity.modalOpen ||
    now - activity.lastInputAt < TYPING_WINDOW_MS
  );
}

let composer = { hasText: false, dictating: false };
let lastInputAt = Number.NEGATIVE_INFINITY;

export function noteComposer(state: { hasText: boolean; dictating: boolean }): void {
  composer = state;
}

export function noteTyping(now: number = Date.now()): void {
  lastInputAt = now;
}

/** Follows the keyboard for the lifetime of the app; returns the cleanup. */
export function trackTyping(): () => void {
  const onKey = () => noteTyping();
  document.addEventListener('keydown', onKey, true);
  document.addEventListener('input', onKey, true);
  return () => {
    document.removeEventListener('keydown', onKey, true);
    document.removeEventListener('input', onKey, true);
  };
}

export function inputBusy(now: number = Date.now()): boolean {
  return isBusy({ ...composer, lastInputAt, modalOpen: hasOpenModal() }, now);
}

/** Forgets everything (tests). */
export function resetInputGuard(): void {
  composer = { hasText: false, dictating: false };
  lastInputAt = Number.NEGATIVE_INFINITY;
}
