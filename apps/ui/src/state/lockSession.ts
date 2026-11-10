/**
 * The app lock's state for one app run, kept outside React.
 *
 * The lock provider sits deep in the tree (below the machine store, which
 * only renders once a machine is known). On the desktop the tree under it
 * can unmount and mount again while the app starts: no machine at first
 * (pairing screen), then this PC once its server answers, and the list may
 * briefly empty again while the shell settles. Component state would lock
 * again on every such mount and the person would have to verify twice. Here
 * the lock is decided once per run, the last unlock is shared by the lock
 * screen and the Command Center gate, and one verification runs at a time.
 *
 * One session per verifier object: the app builds its verifier once
 * (main.tsx), tests build a new one per test.
 */
import { diag } from '../diag/log';
import type { AppLockVerifier, UnlockRequest, UnlockResult } from '../native/appLock';

export interface LockSnapshot {
  /** `null`: not decided yet (no provider mounted so far in this run). */
  locked: boolean | null;
  /** When the person last proved it is them (Unix ms); `null`: not yet. */
  lastUnlockAt: number | null;
}

/** What a verification is for: `unlock` opens the app once verified. */
export type VerifyPurpose = 'unlock' | 'verify';

export class LockSession {
  private snapshot: LockSnapshot = { locked: null, lastUnlockAt: null };
  private readonly listeners = new Set<() => void>();
  private inFlight: Promise<UnlockResult> | null = null;
  /** The lock screen already tried by itself during this lock. */
  private autoTried = false;
  /**
   * The lock screen waits for the person to come back (a pointer, key or
   * focus event) before it tries by itself: the app locked because nobody
   * used it, so a prompt shown now would only run out (Windows answers a
   * prompt nobody sees with "canceled") and ask for a second sign-in later.
   */
  private awaitReturn = false;
  private activity = 0;

  constructor(private readonly verifier: AppLockVerifier) {}

  getSnapshot = (): LockSnapshot => this.snapshot;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  /**
   * Fixes the launch state once per run; later calls change nothing. Called
   * while the first provider renders, so it does not notify (nobody else
   * reads the value before that render commits).
   */
  decide(locked: boolean): void {
    if (this.snapshot.locked !== null) return;
    diag('lock.decide', { locked });
    this.snapshot = { ...this.snapshot, locked };
  }

  /**
   * `awaitReturn`: locked because nobody used the app (the idle timer), so
   * the lock screen's own try waits until the person is back.
   */
  setLocked(locked: boolean, { awaitReturn = false }: { awaitReturn?: boolean } = {}): void {
    if (this.snapshot.locked === locked) return;
    diag(locked ? 'lock.locked' : 'lock.unlocked', locked ? { idle: awaitReturn } : {});
    // Locked again (idle, "Lock now"): the lock screen may try by itself again.
    if (locked) this.autoTried = false;
    this.awaitReturn = locked && awaitReturn;
    this.update({ ...this.snapshot, locked });
  }

  /** True while the lock screen's own try waits for the person to come back. */
  get awaitingReturn(): boolean {
    return this.awaitReturn;
  }

  /** The person is back (a pointer, key or focus event on the lock screen). */
  noteReturn(): void {
    this.awaitReturn = false;
  }

  /**
   * The lock screen's own try ended "canceled" while nobody could answer
   * (the window was hidden or had lost focus): it tries once more when the
   * person is back, instead of leaving an error that asks for a second
   * sign-in.
   */
  rearmAutoUnlock(): void {
    diag('lock.rearm');
    this.autoTried = false;
    this.awaitReturn = true;
  }

  /** Last use of the app (idle lock), kept across remounts too. */
  get lastActivity(): number {
    return this.activity;
  }

  /** The person used the app (resets the idle time). */
  touch(now = Date.now()): void {
    this.activity = now;
  }

  /** Records a verification made now. */
  noteUnlocked(now = Date.now()): void {
    this.activity = now;
    this.update({ ...this.snapshot, lastUnlockAt: now });
  }

  /** True the first time per lock: the lock screen's own try. */
  claimAutoUnlock(): boolean {
    if (this.autoTried) return false;
    this.autoTried = true;
    return true;
  }

  /** True while a verification (the OS prompt, a code check) is open. */
  get verifying(): boolean {
    return this.inFlight !== null;
  }

  /**
   * Runs the verifier, one call at a time: a request made while one is open
   * (the lock screen's own try plus a click on Unlock, a lock screen mounted
   * again while the prompt is up) waits for that one instead of opening a
   * second prompt. A success is recorded here, so it counts even when the
   * screen that asked is gone by the time the person answers.
   */
  verify(request: UnlockRequest, purpose: VerifyPurpose): Promise<UnlockResult> {
    if (this.inFlight) return this.inFlight;
    const run = (async () => {
      try {
        const result = await this.verifier.verify(request);
        diag('lock.verify', { purpose, result });
        if (result === 'verified') {
          this.noteUnlocked();
          if (purpose === 'unlock') this.setLocked(false);
        }
        return result;
      } finally {
        this.inFlight = null;
      }
    })();
    this.inFlight = run;
    return run;
  }

  private update(next: LockSnapshot): void {
    this.snapshot = next;
    for (const l of [...this.listeners]) l();
  }
}

const sessions = new WeakMap<AppLockVerifier, LockSession>();

/** The lock session of this verifier (made on first use). */
export function lockSession(verifier: AppLockVerifier): LockSession {
  let session = sessions.get(verifier);
  if (!session) {
    session = new LockSession(verifier);
    sessions.set(verifier, session);
  }
  return session;
}
