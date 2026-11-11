/**
 * App lock rules: the idle times offered in Settings, the freshness window
 * of the Command Center and a process-wide "locked" flag for code outside
 * React (spoken alerts must stay silent while the app is locked).
 */

/** Idle times offered in Settings (minutes). */
export const APP_LOCK_IDLE_CHOICES = [1, 5, 15, 30, 60] as const;
export type AppLockIdleMinutes = (typeof APP_LOCK_IDLE_CHOICES)[number];
export const DEFAULT_APP_LOCK_IDLE_MIN: AppLockIdleMinutes = 15;

/** Entering the Command Center needs an unlock no older than this. */
export const COMMAND_CENTER_FRESH_MS = 5 * 60_000;

/** How often the idle time is checked while the app is unlocked. */
export const IDLE_CHECK_MS = 5_000;

export function isIdleChoice(v: unknown): v is AppLockIdleMinutes {
  return (APP_LOCK_IDLE_CHOICES as readonly unknown[]).includes(v);
}

/** True once `idleMs` passed since the last activity. */
export function idleExpired(lastActivity: number, now: number, idleMs: number): boolean {
  return now - lastActivity >= idleMs;
}

/** True when the last unlock (Unix ms, `null`: none yet) is recent enough. */
export function unlockFresh(lastUnlockAt: number | null, now: number, maxAgeMs: number): boolean {
  return lastUnlockAt !== null && now - lastUnlockAt <= maxAgeMs && now >= lastUnlockAt;
}

let locked = false;
const listeners = new Set<(locked: boolean) => void>();

/** Whether the app lock is covering the app right now. */
export function isAppLocked(): boolean {
  return locked;
}

/** Set by the lock screen; listeners hear every change. */
export function setAppLocked(next: boolean): void {
  if (locked === next) return;
  locked = next;
  for (const l of listeners) l(next);
}

export function onAppLockChange(listener: (locked: boolean) => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
