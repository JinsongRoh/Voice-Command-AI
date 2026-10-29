import { useId } from 'react';

import horizontalSvg from '../assets/brand/VCA_logo_horizontal_light.svg?raw';
import verticalSvg from '../assets/brand/VCA_logo_vertical_light.svg?raw';

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

interface LogoArt {
  viewBox: string;
  symbol: string[];
  wordmark: string;
}

function parseLogo(svg: string): LogoArt {
  const viewBox = /viewBox="([^"]+)"/.exec(svg)?.[1] ?? '0 0 0 0';
  const group = /<g id="symbol"[^>]*>([\s\S]*?)<\/g>/.exec(svg)?.[1] ?? '';
  const symbol = [...group.matchAll(/\sd="([^"]+)"/g)].map((m) => m[1] ?? '');
  const wordmark = /<path id="wordmark"[^>]*\sd="([^"]+)"/.exec(svg)?.[1] ?? '';
  return { viewBox, symbol, wordmark };
}

const HORIZONTAL = parseLogo(horizontalSvg);
const VERTICAL = parseLogo(verticalSvg);
/** The symbol occupies the left 1178 × 1073 square of the horizontal lockup. */
const SYMBOL_VIEWBOX = '0 0 1178 1073';

export type LogoVariant = 'horizontal' | 'vertical' | 'symbol';

export interface BrandLogoProps {
  variant?: LogoVariant;
  /**
   * `theme` follows the light/dark logo variants; `gradient` always uses the
   * signature cyan-to-blue symbol (for hero placements).
   */
  tone?: 'theme' | 'gradient';
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
      <defs>
        {/* Same geometry as the package's `vcaGrad`: top to bottom per stroke. */}
        <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" style={{ stopColor: top }} />
          <stop offset="1" style={{ stopColor: bottom }} />
        </linearGradient>
      </defs>
      <g data-part="symbol" fill={`url(#${gradientId})`}>
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
