import { describe, expect, it, vi } from 'vitest';

import { isPairingUri, PairLinks } from './pairLinks';

vi.mock('@tauri-apps/plugin-deep-link', () => ({
  getCurrent: async () => null,
  onOpenUrl: async () => () => {},
}));

describe('PairLinks', () => {
  it('keeps only pairing links and hands each out once', () => {
    const links = new PairLinks();
    const seen: string[] = [];
    const stop = links.subscribe((u) => seen.push(u));
    links.push('https://evil.example/pair');
    expect(links.hasPending).toBe(false);
    links.push(' vca://pair?v=1&k=x ');
    expect(seen).toEqual(['vca://pair?v=1&k=x']);
    expect(links.take()).toBe('vca://pair?v=1&k=x');
    expect(links.take()).toBeNull();
    stop();
    links.push('vca://pair?v=1&k=y');
    expect(seen).toHaveLength(1);
  });

  it('recognizes pairing URIs', () => {
    expect(isPairingUri('vca://pair?v=1')).toBe(true);
    expect(isPairingUri('VCA://pair/?v=1')).toBe(true);
    expect(isPairingUri('vca://other?v=1')).toBe(false);
  });
});
