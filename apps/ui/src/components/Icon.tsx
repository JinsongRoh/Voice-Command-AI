/** Minimal inline SVG icon set (24x24, stroke-based, decorative by default). */
const PATHS = {
  mic: 'M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3zM5 11a7 7 0 0 0 14 0M12 18v3M8 21h8',
  send: 'M4 12l16-8-6 16-2.5-6.5L4 12z',
  plus: 'M12 5v14M5 12h14',
  minus: 'M5 12h14',
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
  /** The user manual. */
  book: 'M4 19V5a2 2 0 0 1 2-2h14v14H6a2 2 0 0 0-2 2zm0 0a2 2 0 0 0 2 2h14M8 7h8M8 11h6',
  // Device and machine management.
  phone: 'M8 3h8a1 1 0 0 1 1 1v16a1 1 0 0 1-1 1H8a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zM11 18h2',
  monitor: 'M3 5h18v11H3zM8 20h8M12 16v4',
  windows: 'M4 5h16v14H4zM4 9h16M12 9v10',
  upload: 'M12 16V5M7 10l5-5 5 5M4 19h16',
  more: 'M12 5h.01M12 12h.01M12 19h.01',
  star: 'M12 3l2.8 5.7 6.2.9-4.5 4.4 1 6.2L12 17.3 6.5 20.2l1-6.2L3 9.6l6.2-.9L12 3z',
  edit: 'M4 20h4L19 9l-4-4L4 16v4zM14 6l4 4',
  // Chat.
  copy: 'M9 9h11v11H9zM5 15H4V4h11v1',
  check: 'M5 12.5l4.5 4.5L19 7.5',
  agent:
    'M12 3v3M7 6h10a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2zM9.5 11h.01M14.5 11h.01M9.5 15h5',
  chevron: 'M9 6l6 6-6 6',
  search: 'M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4-4',
  // Remote shells.
  clock: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7v5l3 2',
  paste: 'M9 3h6v3H9zM9 4.5H6V21h12V4.5h-3',
  bookmark: 'M7 4h10v17l-5-4-5 4z',
  shield: 'M12 3l7 3v6c0 4.5-3 7.5-7 9-4-1.5-7-4.5-7-9V6l7-3z',
  /** The conductor (Command Center): one node directing three. */
  conductor:
    'M12 9a2 2 0 1 0 0 4 2 2 0 0 0 0-4zM5 3a2 2 0 1 0 0 4 2 2 0 0 0 0-4zM19 3a2 2 0 1 0 0 4 2 2 0 0 0 0-4zM12 18a2 2 0 1 0 0 4 2 2 0 0 0 0-4zM6.7 6.3l3.6 3.2M17.3 6.3l-3.6 3.2M12 13v5',
  // Spoken briefing.
  speaker: 'M4 9h4l5-4v14l-5-4H4zM16.5 8.5a5 5 0 0 1 0 7M19 6a8.5 8.5 0 0 1 0 12',
  speakerOff: 'M4 9h4l5-4v14l-5-4H4zM16 9l5 6M21 9l-5 6',
  /** "Listen to status": headphones, unlike the alerts speaker. */
  headphones:
    'M4 15v-3a8 8 0 0 1 16 0v3M4 14h3v6H5a1 1 0 0 1-1-1v-5zM17 14h3v5a1 1 0 0 1-1 1h-2v-6z',
  /** Hands-free voice control: a headset with a microphone boom. */
  headset:
    'M4 15v-3a8 8 0 0 1 16 0v3M4 14h3v6H5a1 1 0 0 1-1-1v-5zM17 14h3v4M20 18c0 2-2.2 3-5 3h-2.5M10.5 21h2',
  /** "Listen to status": a status list with a play mark (spoken summary). */
  briefing: 'M4 6h11M4 10h11M4 14h6M14 13.5l7 4-7 4v-8z',
  // Attachments summary.
  paperclip: 'M20 11.5l-8 8a5 5 0 0 1-7-7l8.5-8.5a3.5 3.5 0 0 1 5 5L10 17.5a2 2 0 0 1-3-3l7.5-7.5',
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
