import { getCurrent, onOpenUrl } from '@tauri-apps/plugin-deep-link';

import type { Unsubscribe } from '../protocol/transport';
import { bridge } from './bridge';

export function isPairingUri(value: string): boolean {
  return /^vca:\/\/pair(\?|\/\?)/i.test(value.trim());
}

/** The only console link the app accepts: https, exact host and path. */
const CONSOLE_ENROLL_PREFIX = 'https://dns.dnspilot.kr/enroll#';
const INVITE_CODE = /^dpt_inv_[A-Za-z0-9_-]{43}$/;
const TUNNEL_ID = /^[1-9][0-9]*$/;
const HOSTNAME = /^[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+$/;

/**
 * The DNSPilot console's "phone pairing QR":
 * `https://dns.dnspilot.kr/enroll#v=1&h=<hostname>&t=<tunnel_id>&c=<code>`.
 * Rust validates the link again and only ever posts to the configured
 * console; this check decides what the screen does with the text.
 */
export function isConsoleEnrollUrl(value: string): boolean {
  const text = value.trim();
  if (!text.startsWith(CONSOLE_ENROLL_PREFIX)) return false;
  const params = new URLSearchParams(text.slice(CONSOLE_ENROLL_PREFIX.length));
  const one = (key: string) => {
    const all = params.getAll(key);
    return all.length === 1 ? all[0] : undefined;
  };
  return (
    one('v') === '1' &&
    HOSTNAME.test(one('h') ?? '') &&
    TUNNEL_ID.test(one('t') ?? '') &&
    INVITE_CODE.test(one('c') ?? '')
  );
}

/** A `vca://pair` link that also carries a DNSPilot invite (`h`, `t`, `c`). */
export function hasInvite(uri: string): boolean {
  const text = uri.trim();
  if (!isPairingUri(text)) return false;
  const params = new URLSearchParams(text.slice(text.indexOf('?') + 1));
  return params.has('c') || params.has('h') || params.has('t');
}

/**
 * True when a `vca://pair` link says the PC asks for a 2-step verification
 * code (`tf=1`). Only a hint for the form: it carries no secret, and an old
 * link without it may still need a code.
 */
export function linkRequiresTotp(uri: string): boolean {
  const text = uri.trim();
  if (!isPairingUri(text)) return false;
  const params = new URLSearchParams(text.slice(text.indexOf('?') + 1));
  return params.get('tf') === '1';
}

/** What a link does: pair, enroll then pair, or enroll only. */
export type LinkKind = 'pair' | 'pair_invite' | 'enroll';

/** The kind of a link, or `null` when the app does not accept it. */
export function classifyLink(value: string): LinkKind | null {
  if (hasInvite(value)) return 'pair_invite';
  if (isPairingUri(value)) return 'pair';
  return isConsoleEnrollUrl(value) ? 'enroll' : null;
}

type Listener = (uri: string) => void;

/**
 * Pairing links that opened the app (`vca://pair?...`, e.g. the server's QR
 * code read by the phone camera). A link only pre-fills the pairing form;
 * pairing still needs the user's tap, so a link from elsewhere cannot pair
 * the device silently.
 *
 * Links are buffered until the pairing form takes them, so a link that
 * arrives before React renders (cold start) is not lost.
 */
export class PairLinks {
  private pending: string | null = null;
  private readonly seen = new Set<string>();
  private readonly listeners = new Set<Listener>();

  /**
   * Records a link that was just opened (other URLs are ignored) and
   * notifies listeners. Opening the same link again fills the form again.
   */
  push(uri: string): void {
    if (classifyLink(uri) === null) return;
    const link = uri.trim();
    this.seen.add(link);
    this.pending = link;
    for (const l of [...this.listeners]) l(link);
  }

  /**
   * Records the link that launched the app. The launch link is re-read on
   * several occasions (start, after a delay, on returning to the app), so a
   * link already seen is ignored: it must not refill a form the user cleared
   * or already paired with.
   */
  pushLaunch(uri: string): void {
    if (classifyLink(uri) === null || this.seen.has(uri.trim())) return;
    this.push(uri);
  }

  get hasPending(): boolean {
    return this.pending !== null;
  }

  /** Returns the pending link and clears it. */
  take(): string | null {
    const uri = this.pending;
    this.pending = null;
    return uri;
  }

  subscribe(listener: Listener): Unsubscribe {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }
}

/** Where pairing links come from (injectable for tests). */
export interface PairLinkSources {
  /** The deep-link plugin's launch URL(s). */
  getCurrent(): Promise<string[] | null>;
  /** Links opened while the app runs. */
  onOpenUrl(handler: (urls: string[]) => void): Promise<Unsubscribe>;
  /** Launch links captured natively (iOS cold start; see `vca_launch_links`). */
  launchLinks(): Promise<string[]>;
}

const pluginSources: PairLinkSources = {
  getCurrent,
  onOpenUrl,
  launchLinks: () => bridge.launchLinks(),
};

/**
 * Delays (ms) after start at which the launch link is read again: on a cold
 * start the plugin may only learn the URL after the webview has loaded.
 */
export const LAUNCH_RECHECK_MS = [500, 2000];

/**
 * Feeds pairing links into `links`: links opened while the app runs, and the
 * link that launched it. Call as early as possible (before React renders):
 * the event listener is registered first, then the launch link is read now,
 * again after short delays and whenever the app comes back to the
 * foreground.
 */
export async function listenForPairLinks(
  links: PairLinks,
  {
    sources = pluginSources,
    recheckMs = LAUNCH_RECHECK_MS,
  }: { sources?: PairLinkSources; recheckMs?: readonly number[] } = {},
): Promise<Unsubscribe> {
  let stopped = false;
  let unlisten: Unsubscribe = () => {};
  try {
    // Android may report `[null]` for an intent without a URL.
    unlisten = await sources.onOpenUrl((urls) =>
      (urls as (string | null)[] | null)?.forEach((u) => {
        if (typeof u === 'string') links.push(u);
      }),
    );
  } catch {
    // No deep-link events on this platform.
  }

  const readLaunchLinks = async () => {
    if (stopped) return;
    const [current, captured] = await Promise.all([
      sources.getCurrent().catch(() => null),
      sources.launchLinks().catch(() => null),
    ]);
    // Pushed even if stopped meanwhile: native launch links are handed out
    // only once, and `links` buffers them.
    for (const u of [...(current ?? []), ...(captured ?? [])]) {
      if (typeof u === 'string') links.pushLaunch(u);
    }
  };

  const timers = recheckMs.map((ms) => setTimeout(() => void readLaunchLinks(), ms));
  const onVisible = () => {
    if (document.visibilityState === 'visible') void readLaunchLinks();
  };
  const onFocus = () => void readLaunchLinks();
  document.addEventListener('visibilitychange', onVisible);
  window.addEventListener('focus', onFocus);

  await readLaunchLinks();

  return () => {
    stopped = true;
    timers.forEach(clearTimeout);
    document.removeEventListener('visibilitychange', onVisible);
    window.removeEventListener('focus', onFocus);
    unlisten();
  };
}
