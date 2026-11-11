import { useId } from 'react';

import { HORIZONTAL, SYMBOL_VIEWBOX, VERTICAL } from './brandArt';

/**
 * The Voice Command AI logo, drawn inline from the official SVG package
 * (docs/brand/logo, copied to src/assets/brand). The path data is read from
 * the bundled SVG files so they stay the single source of truth.
 *
 * Inline drawing lets one component follow the theme without recoloring the
 * artwork beyond the official variants:
 * - light theme: the "light" lockup (single-color Command Blue symbol, Deep
 *   Navy wordmark);
 * - dark theme: the "dark-gradient" lockup without its navy plate (cyan-to-blue
 *   symbol, white wordmark).
 * The colors come from the `--logo-*` tokens in theme.css. The logo is never
 * recolored by session state, never stretched (fixed aspect ratio) and gets no
 * outline or shadow. Each lockup's viewBox already contains the 0.20S clear
 * space around the symbol.
 */

export type LogoVariant = 'horizontal' | 'vertical' | 'symbol';

export interface BrandLogoProps {
  variant?: LogoVariant;
  /**
   * `theme` follows the light/dark logo variants; `gradient` always uses the
   * signature cyan-to-blue symbol (for hero placements); `mono` draws the
   * official single-color variant in `currentColor` (white on Command Blue,
   * Deep Navy on the voice gradient), e.g. inside the microphone button.
   */
  tone?: 'theme' | 'gradient' | 'mono';
  /** Accessible name. Without it the logo is decorative (aria-hidden). */
  title?: string;
  className?: string;
}

export function BrandLogo({
  variant = 'horizontal',
  tone = 'theme',
  title,
  className,
}: BrandLogoProps) {
  const gradientId = `vca-logo-${useId().replace(/[^a-zA-Z0-9_-]/g, '')}`;
  const art = variant === 'vertical' ? VERTICAL : HORIZONTAL;
  const viewBox = variant === 'symbol' ? SYMBOL_VIEWBOX : art.viewBox;
  const top = tone === 'gradient' ? 'var(--brand-cyan)' : 'var(--logo-grad-top)';
  const bottom = tone === 'gradient' ? 'var(--brand-blue)' : 'var(--logo-grad-bottom)';
  const a11y = title ? { role: 'img', 'aria-label': title } : { 'aria-hidden': true as const };

  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox={viewBox}
      preserveAspectRatio="xMidYMid meet"
      className={className}
      data-logo={variant}
      focusable="false"
      {...a11y}
    >
      {tone !== 'mono' && (
        <defs>
          {/* Same geometry as the package's `vcaGrad`: top to bottom per stroke. */}
          <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" style={{ stopColor: top }} />
            <stop offset="1" style={{ stopColor: bottom }} />
          </linearGradient>
        </defs>
      )}
      <g data-part="symbol" fill={tone === 'mono' ? 'currentColor' : `url(#${gradientId})`}>
        {art.symbol.map((d, i) => (
          <path key={i} d={d} />
        ))}
      </g>
      {variant !== 'symbol' && (
        <path data-part="wordmark" d={art.wordmark} style={{ fill: 'var(--logo-wordmark)' }} />
      )}
    </svg>
  );
}
