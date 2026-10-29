import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { BrandLogo } from './BrandLogo';

describe('BrandLogo', () => {
  it('draws the horizontal lockup: five symbol strokes and the wordmark', () => {
    const { container } = render(<BrandLogo />);
    const svg = container.querySelector('svg')!;
    expect(svg).toHaveAttribute('data-logo', 'horizontal');
    expect(svg).toHaveAttribute('viewBox', '0 0 4498 1073');
    expect(svg).toHaveAttribute('aria-hidden', 'true');
    const strokes = svg.querySelectorAll('[data-part="symbol"] path');
    expect(strokes).toHaveLength(5);
    for (const path of strokes) expect(path.getAttribute('d')).toMatch(/^M[\d.]+ [\d.]+C/);
    expect(svg.querySelector('[data-part="wordmark"]')?.getAttribute('d')).toBeTruthy();
  });

  it('draws the symbol alone without the wordmark', () => {
    const { container } = render(<BrandLogo variant="symbol" />);
    const svg = container.querySelector('svg')!;
    expect(svg).toHaveAttribute('viewBox', '0 0 1178 1073');
    expect(svg.querySelectorAll('[data-part="symbol"] path')).toHaveLength(5);
    expect(svg.querySelector('[data-part="wordmark"]')).toBeNull();
  });

  it('follows the theme tokens, or the signature gradient for heroes', () => {
    const { container, rerender } = render(<BrandLogo />);
    const stops = () => [...container.querySelectorAll('stop')].map((s) => s.style.stopColor);
    expect(stops()).toEqual(['var(--logo-grad-top)', 'var(--logo-grad-bottom)']);
    expect(container.querySelector<SVGPathElement>('[data-part="wordmark"]')!.style.fill).toBe(
      'var(--logo-wordmark)',
    );
    rerender(<BrandLogo tone="gradient" />);
    expect(stops()).toEqual(['var(--brand-cyan)', 'var(--brand-blue)']);
  });

  it('is an image with an accessible name when titled', () => {
    render(<BrandLogo variant="vertical" title="Voice Command AI" />);
    const img = screen.getByRole('img', { name: 'Voice Command AI' });
    expect(img).toHaveAttribute('viewBox', '0 0 2760 1416');
  });

  it('gives each instance its own gradient id', () => {
    const { container } = render(
      <>
        <BrandLogo />
        <BrandLogo variant="symbol" />
      </>,
    );
    const ids = [...container.querySelectorAll('linearGradient')].map((g) => g.id);
    expect(new Set(ids).size).toBe(2);
    for (const g of container.querySelectorAll('[data-part="symbol"]')) {
      expect(ids.map((id) => `url(#${id})`)).toContain(g.getAttribute('fill'));
    }
  });
});
