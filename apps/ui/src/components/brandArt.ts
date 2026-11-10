import horizontalSvg from '../assets/brand/VCA_logo_horizontal_light.svg?raw';
import verticalSvg from '../assets/brand/VCA_logo_vertical_light.svg?raw';

/**
 * Path data of the official logo, read from the bundled SVG package
 * (docs/brand/logo, copied to src/assets/brand) so the files stay the single
 * source of truth. Used by `BrandLogo` and the mic button's `MicSymbol`.
 */
export interface LogoArt {
  viewBox: string;
  symbol: string[];
  wordmark: string;
}

export function parseLogo(svg: string): LogoArt {
  const viewBox = /viewBox="([^"]+)"/.exec(svg)?.[1] ?? '0 0 0 0';
  const group = /<g id="symbol"[^>]*>([\s\S]*?)<\/g>/.exec(svg)?.[1] ?? '';
  const symbol = [...group.matchAll(/\sd="([^"]+)"/g)].map((m) => m[1] ?? '');
  const wordmark = /<path id="wordmark"[^>]*\sd="([^"]+)"/.exec(svg)?.[1] ?? '';
  return { viewBox, symbol, wordmark };
}

export const HORIZONTAL = parseLogo(horizontalSvg);
export const VERTICAL = parseLogo(verticalSvg);
/** The symbol occupies the left 1178 × 1073 square of the horizontal lockup. */
export const SYMBOL_VIEWBOX = '0 0 1178 1073';
/** The symbol's five strokes, left to right. */
export const SYMBOL_PATHS: readonly string[] = HORIZONTAL.symbol;
