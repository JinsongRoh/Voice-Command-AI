/** Minimal inline SVG icon set (24x24, stroke-based, decorative by default). */
const PATHS = {
  mic: 'M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3zM5 11a7 7 0 0 0 14 0M12 18v3M8 21h8',
  send: 'M4 12l16-8-6 16-2.5-6.5L4 12z',
  plus: 'M12 5v14M5 12h14',
  settings:
    'M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z',
  gauge: 'M12 14l4-4M3.5 18a9 9 0 1 1 17 0',
  list: 'M4 6h16M4 12h16M4 18h10',
  back: 'M15 18l-6-6 6-6',
  up: 'M18 15l-6-6-6 6',
  down: 'M6 9l6 6 6-6',
  close: 'M6 6l12 12M18 6L6 18',
  expand: 'M14 4h6v6M20 4l-6 6M10 20H4v-6M4 20l6-6',
  collapse: 'M20 10h-6V4M14 10l6-6M4 14h6v6M10 14l-6 6',
  cloud: 'M7 18a4 4 0 0 1-.6-8A6 6 0 0 1 18 9a4.5 4.5 0 0 1-.5 9H7z',
  terminal: 'M4 5h16v14H4zM7 9l3 3-3 3M12 15h5',
  chat: 'M4 5h16v11H9l-5 4V5z',
  grip: 'M9 6h.01M15 6h.01M9 12h.01M15 12h.01M9 18h.01M15 18h.01',
  power: 'M12 3v9M6.3 6.3a8 8 0 1 0 11.4 0',
  qr: 'M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h2v2h-2zM18 18h2v2h-2zM14 18h2M18 14h2',
  pause: 'M8 5v14M16 5v14',
  play: 'M7 5l12 7-12 7V5z',
  stop: 'M6 6h12v12H6z',
  restart: 'M4 12a8 8 0 1 0 2.3-5.7M4 4v5h5',
  external: 'M14 4h6v6M20 4l-9 9M18 14v6H4V6h6',
  heart: 'M12 20s-7-4.4-7-10a4 4 0 0 1 7-2.6A4 4 0 0 1 19 10c0 5.6-7 10-7 10z',
  laptop: 'M5 6h14v9H5zM3 18h18',
  bell: 'M6 16V11a6 6 0 1 1 12 0v5l2 2H4l2-2zM10 21h4',
  info: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 11v6M12 7.5h.01',
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({
  name,
  size = 20,
  label,
  className,
}: {
  name: IconName;
  size?: number;
  /** When set, the icon is announced; otherwise it is hidden from assistive tech. */
  label?: string;
  className?: string;
}) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      focusable="false"
    >
      <path d={PATHS[name]} />
    </svg>
  );
}
