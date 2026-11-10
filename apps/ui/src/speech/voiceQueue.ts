/**
 * One queue for everything the app says on its own: approval and error
 * alerts, completions, result summaries and briefings. Items play one after
 * another (never cutting each other off), most urgent first:
 *
 * - priority: decisions and input requests, then errors, then briefings the
 *   person asked for, then completions and summaries;
 * - an item whose key is already queued or playing is dropped (the same
 *   alert never plays twice);
 * - items that waited longer than `maxAgeMs` are dropped unheard;
 * - at most `cap` items wait; beyond that the least urgent (and, among
 *   those, the newest) is dropped;
 * - a short gap separates items;
 * - every item has a time limit, so a player that never reports its end
 *   cannot block the queue;
 * - `stopAll` silences the current item and empties the queue;
 * - `exclusive` (hands-free read-back and listening, a running dictation)
 *   pauses the queue: the current item is stopped and played again afterwards,
 *   then the queue resumes. Paused time does not count towards `maxAgeMs`.
 *
 * Device-voice and PC-voice items are the same here: each item's `play`
 * resolves once its audio has ended.
 */

export type VoicePriority = 'decision' | 'error' | 'briefing' | 'completion';

const RANK: Record<VoicePriority, number> = {
  decision: 0,
  error: 1,
  briefing: 2,
  completion: 3,
};

export interface VoiceItem {
  /** Same key queued or playing: the new item is dropped. */
  key?: string;
  priority: VoicePriority;
  /** Plays the item; resolves when it has finished (or failed). */
  play(): Promise<unknown>;
  /** Stops the item while it plays. */
  stop(): void;
  /** Upper bound of `play`; the default depends on nothing but the queue. */
  timeoutMs?: number;
}

interface Entry extends VoiceItem {
  at: number;
  seq: number;
}

export interface VoiceQueueOptions {
  gapMs?: number;
  maxAgeMs?: number;
  cap?: number;
  itemTimeoutMs?: number;
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
}

export const GAP_MS = 400;
export const MAX_AGE_MS = 60_000;
export const QUEUE_CAP = 5;
export const ITEM_TIMEOUT_MS = 45_000;

export interface VoiceQueue {
  /** Adds an item. Returns `false` when it was dropped (duplicate or over the cap). */
  enqueue(item: VoiceItem): boolean;
  /** Stops the current item and drops everything waiting. */
  stopAll(): void;
  /** Runs `fn` with the queue paused (the current item is stopped and replayed later). */
  exclusive<T>(fn: () => Promise<T>): Promise<T>;
  /** Something is playing or waiting (and the queue is not paused). */
  readonly speaking: boolean;
  /** Keys waiting, in play order (tests and diagnostics). */
  pending(): string[];
  /** Called whenever `speaking` may have changed. Returns an unsubscribe. */
  subscribe(listener: () => void): () => void;
}

const defaultSleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

export function createVoiceQueue(options: VoiceQueueOptions = {}): VoiceQueue {
  const gapMs = options.gapMs ?? GAP_MS;
  const maxAgeMs = options.maxAgeMs ?? MAX_AGE_MS;
  const cap = options.cap ?? QUEUE_CAP;
  const itemTimeoutMs = options.itemTimeoutMs ?? ITEM_TIMEOUT_MS;
  const now = options.now ?? Date.now;
  const sleep = options.sleep ?? defaultSleep;

  let waiting: Entry[] = [];
  let current: Entry | null = null;
  /** Bumped by `stopAll` and `exclusive`: a running item no longer counts. */
  let generation = 0;
  let paused = 0;
  let pausedAt = 0;
  let running = false;
  let seq = 0;
  const listeners = new Set<() => void>();
  const notify = () => {
    for (const l of listeners) l();
  };

  const order = (a: Entry, b: Entry) => RANK[a.priority] - RANK[b.priority] || a.seq - b.seq;

  /** Ends the wait for the current item (stop all, exclusive use). */
  let release: (() => void) | null = null;

  const playOne = async (entry: Entry): Promise<void> => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const limit = new Promise<'timeout'>((resolve) => {
      timer = setTimeout(() => resolve('timeout'), entry.timeoutMs ?? itemTimeoutMs);
    });
    const released = new Promise<'released'>((resolve) => {
      release = () => resolve('released');
    });
    try {
      const outcome = await Promise.race([
        entry.play().then(() => 'done' as const),
        limit,
        released,
      ]);
      if (outcome === 'timeout') entry.stop();
    } catch {
      // A failed item is skipped.
    } finally {
      clearTimeout(timer);
      release = null;
    }
  };

  const pump = async () => {
    if (running) return;
    running = true;
    try {
      let first = true;
      for (;;) {
        if (paused > 0) return;
        waiting = waiting.filter((e) => now() - e.at <= maxAgeMs);
        const next = waiting.sort(order).shift();
        if (!next) return;
        if (!first) await sleep(gapMs);
        first = false;
        if (paused > 0) {
          waiting.push(next);
          return;
        }
        if (now() - next.at > maxAgeMs) continue;
        current = next;
        const mine = generation;
        notify();
        await playOne(next);
        if (current === next) current = null;
        if (mine !== generation) {
          // Stopped or paused meanwhile; `exclusive` re-queued it if needed.
          continue;
        }
      }
    } finally {
      running = false;
      notify();
    }
  };

  const queue: VoiceQueue = {
    enqueue(item) {
      if (item.key && (current?.key === item.key || waiting.some((e) => e.key === item.key))) {
        return false;
      }
      seq += 1;
      const entry: Entry = { ...item, at: now(), seq };
      waiting.push(entry);
      let kept = true;
      while (waiting.length > cap) {
        // The least urgent, newest first, is dropped.
        const victim = [...waiting].sort(order).pop();
        if (!victim) break;
        waiting = waiting.filter((e) => e !== victim);
        if (victim === entry) kept = false;
      }
      notify();
      void pump();
      return kept;
    },
    stopAll() {
      generation += 1;
      waiting = [];
      const playing = current;
      current = null;
      playing?.stop();
      release?.();
      notify();
    },
    async exclusive(fn) {
      if (paused === 0) pausedAt = now();
      paused += 1;
      generation += 1;
      const playing = current;
      current = null;
      if (playing) {
        playing.stop();
        // Heard again after the exclusive use, if still fresh.
        waiting.push({ ...playing, seq: -1 });
      }
      release?.();
      notify();
      try {
        return await fn();
      } finally {
        paused -= 1;
        if (paused === 0) {
          const t = now();
          for (const e of waiting) e.at = Math.min(t, e.at + t - pausedAt);
        }
        notify();
        if (paused === 0) {
          // Let a pump that is just winding down finish first.
          await Promise.resolve();
          void pump();
        }
      }
    },
    get speaking() {
      return paused === 0 && (current !== null || waiting.length > 0);
    },
    pending() {
      return [...waiting].sort(order).map((e) => e.key ?? '');
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
  return queue;
}

/** The app's one voice queue. */
export const voiceQueue: VoiceQueue = createVoiceQueue();
