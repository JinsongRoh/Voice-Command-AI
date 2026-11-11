import { toNativeError } from './bridge';

/** Delay between connection attempts while the Agent learns a new key. */
export const ENROLL_RETRY_MS = 4000;

/**
 * After an invite the Agent picks the new key up on its poll, and until then
 * the tunnel refuses this app during the TLS handshake. That looks like an
 * unreachable server, and nothing has reached the server yet, so the
 * one-time pairing secret is still good.
 */
function isRefusedConnection(err: unknown): boolean {
  const { code } = toNativeError(err);
  return code === 'unreachable' || code === 'timeout';
}

/** Whole seconds left until `untilMs`, never negative. */
export function secondsUntil(untilMs: number, now: () => number = Date.now): number {
  return Math.max(0, Math.ceil((untilMs - now()) / 1000));
}

/**
 * Runs `attempt`, and while `until` (ms) has not passed repeats it after every
 * refused connection, so the person never sees "server unreachable" during
 * the window. Other errors surface at once. Once the window is over the error
 * of the last attempt is the real one.
 */
export async function retryWhileEnrolling<T>(
  attempt: () => Promise<T>,
  {
    until,
    retryMs = ENROLL_RETRY_MS,
    now = Date.now,
    sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  }: {
    until: number;
    retryMs?: number;
    now?: () => number;
    sleep?: (ms: number) => Promise<void>;
  },
): Promise<T> {
  for (;;) {
    try {
      return await attempt();
    } catch (err) {
      const left = until - now();
      if (left <= 0 || !isRefusedConnection(err)) throw err;
      await sleep(Math.min(retryMs, left));
    }
  }
}
