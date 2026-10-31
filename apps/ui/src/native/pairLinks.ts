import { getCurrent, onOpenUrl } from '@tauri-apps/plugin-deep-link';

import type { Unsubscribe } from '../protocol/transport';

export function isPairingUri(value: string): boolean {
  return /^vca:\/\/pair(\?|\/\?)/i.test(value.trim());
}

type Listener = (uri: string) => void;

/**
 * Pairing links that opened the app (`vca://pair?...`, e.g. the server's QR
 * code read by the phone camera). A link only pre-fills the pairing form;
 * pairing still needs the user's tap, so a link from elsewhere cannot pair
 * the device silently.
 */
export class PairLinks {
  private pending: string | null = null;
  private readonly listeners = new Set<Listener>();

  /** Records a link (non-pairing URLs are ignored) and notifies listeners. */
  push(uri: string): void {
    if (!isPairingUri(uri)) return;
    const link = uri.trim();
    this.pending = link;
    for (const l of [...this.listeners]) l(link);
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

/** Feeds the launch link and later links from the deep-link plugin into `links`. */
export async function listenForPairLinks(links: PairLinks): Promise<Unsubscribe> {
  const push = (urls: string[] | null) => urls?.forEach((u) => links.push(u));
  try {
    push(await getCurrent());
  } catch {
    // No launch link support on this platform.
  }
  try {
    return await onOpenUrl(push);
  } catch {
    return () => {};
  }
}
