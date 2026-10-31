import { describe, expect, it } from 'vitest';

import indexHtml from '../index.html?raw';

/** Bundled brand SVGs, keyed by their path relative to this file. */
const brandAssets = import.meta.glob<string>('./assets/brand/*.svg', {
  query: '?raw',
  import: 'default',
  eager: true,
});

describe('index.html', () => {
  const doc = new DOMParser().parseFromString(indexHtml, 'text/html');

  it('uses the bundled VCA favicon instead of an empty data URL', () => {
    const icon = doc.querySelector<HTMLLinkElement>('link[rel="icon"]');
    expect(icon).not.toBeNull();
    const href = icon!.getAttribute('href')!;
    expect(href).not.toMatch(/^data:/);
    // A root-relative source path: Vite bundles it (no network, CSP 'self').
    expect(href).toBe('/src/assets/brand/VCA_favicon_transparent.svg');
    expect(icon!.getAttribute('type')).toBe('image/svg+xml');

    const svg = brandAssets[`.${href.replace('/src', '')}`];
    expect(svg).toBeDefined();
    // The favicon is the five-stroke symbol in Command Blue.
    expect(svg).toContain('fill="#0361F7"');
    expect(svg!.match(/<path /g)).toHaveLength(5);
  });

  it('loads nothing from the network', () => {
    const external = [...doc.querySelectorAll('[href], [src]')]
      .map((el) => el.getAttribute('href') ?? el.getAttribute('src') ?? '')
      .filter((url) => /^(https?:)?\/\//.test(url));
    expect(external).toEqual([]);
  });
});
