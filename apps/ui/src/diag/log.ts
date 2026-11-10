/**
 * Diagnostics log: a bounded ring buffer of events (dictation, microphone,
 * speech requests, Voco, the app lock, app lifecycle, the link) that
 * Settings -> About shows and can copy. It stays on this device (local
 * storage of the web view) and is never uploaded.
 *
 * Two levels:
 * - Every build keeps the speech events (`RELEASE_PREFIXES`), the last
 *   `DIAG_CAPACITY` entries of at most `DIAG_MAX_AGE_MS`, so "speech got
 *   unstable after a long idle" can be traced on a release build. Chatty
 *   per-poll events are left out and every event name is rate limited.
 * - Development builds and builds made with `VITE_VCA_DIAG=1` keep every
 *   event, unthrottled (uploads, per-poll counters, ...). A production build
 *   with that flag fails unless it is marked internal (`buildGuard.ts`).
 *
 * The log can be turned off in Settings -> About (`Settings.diagnostics`):
 * that stops recording and clears the stored log. `DIAG_DEFAULT_ON` (off) is
 * the default: nothing is recorded until the person turns the log on.
 *
 * Privacy: callers pass only counters, sizes, timings and fixed labels; never
 * audio, transcript text, prompts, paths or secrets. As a second line of
 * defence every entry is redacted when recorded and again when copied: values
 * of content-like keys are dropped and URLs, tokens, keys, e-mail and IP
 * addresses, host names and file paths inside strings are masked. Strings are
 * truncated. Outside verbose builds error text is reduced to a class
 * (`errorClass`) and an iOS audio route to its kind of output (`routeClass`).
 */

export type DiagValue = string | number | boolean | null | undefined;
export type DiagData = Record<string, DiagValue>;

export interface DiagEntry {
  /** Wall-clock time, Unix ms. */
  at: number;
  event: string;
  data: DiagData;
}

/**
 * Whether the log records until the person decides otherwise: off, the log
 * is opt-in (`Settings.diagnostics`).
 */
export const DIAG_DEFAULT_ON = false;

/** Entries kept; the oldest are dropped first. */
export const DIAG_CAPACITY = 2000;
/** Entries older than this are dropped (7 days). */
export const DIAG_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000;
/** Most entries of one event name per minute outside verbose builds. */
export const DIAG_RATE_PER_MIN = 30;
const MAX_STRING = 200;

/** Event families kept by every build. */
export const RELEASE_PREFIXES = [
  'app.',
  'mic.',
  'stt.',
  'dictation.',
  'composer.dictate.',
  'composer.hidden.',
  'voco.',
  'handsfree.',
  'voice.',
  'lock.',
  'lifecycle.',
  'link.',
  'snapshot',
] as const;

/** Per-poll events that would push everything else out of a release log. */
const CHATTY = new Set(['mic.peek', 'mic.split', 'mic.native.stats', 'dictation.cut', 'voco.vad']);

const STORE_KEY = 'vca.diag.v1';

let enabled = DIAG_DEFAULT_ON;
let verbose = import.meta.env.DEV || import.meta.env.VITE_VCA_DIAG === '1';
/** Loaded below, once the redaction rules exist. */
let entries: DiagEntry[] = [];
const listeners = new Set<() => void>();
let saveTimer: ReturnType<typeof setTimeout> | null = null;
/** Rate limit: per event name, the current minute's count and drops. */
const windows = new Map<string, { start: number; count: number; dropped: number }>();

/** Drops entries beyond the capacity or older than the age limit. */
function bound(list: DiagEntry[], now = Date.now()): DiagEntry[] {
  let start = Math.max(0, list.length - DIAG_CAPACITY);
  while (start < list.length && now - list[start]!.at > DIAG_MAX_AGE_MS) start += 1;
  return start === 0 ? list : list.slice(start);
}

/** The previous runs' log survives a restart (a killed web view loses memory). */
function load(): DiagEntry[] {
  try {
    const raw = globalThis.localStorage?.getItem(STORE_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    if (!Array.isArray(parsed)) return [];
    const valid = (parsed as DiagEntry[]).filter(
      (e) => e && typeof e.at === 'number' && typeof e.event === 'string' && e.data,
    );
    return bound(valid.map((e) => ({ ...e, data: clean(e.data) })));
  } catch {
    return [];
  }
}

function save() {
  if (!enabled) return;
  try {
    globalThis.localStorage?.setItem(STORE_KEY, JSON.stringify(entries));
  } catch {
    // Storage full or unavailable: the in-memory log still works.
  }
}

function scheduleSave() {
  if (saveTimer) return;
  saveTimer = setTimeout(() => {
    saveTimer = null;
    save();
  }, 1000);
}

/** Writes the log to storage now (the app is going to the background). */
export function flushDiag(): void {
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = null;
  save();
}

/**
 * Whether every event is collected (development and `VITE_VCA_DIAG=1`
 * builds). Release builds keep the speech events only; see `diagRecords`.
 */
export function diagEnabled(): boolean {
  return enabled && verbose;
}

/** Whether `event` would be recorded in this build. */
export function diagRecords(event: string): boolean {
  if (!enabled) return false;
  if (verbose) return true;
  return !CHATTY.has(event) && RELEASE_PREFIXES.some((p) => event.startsWith(p));
}

/** Turns collection on or off. Off also clears the log and its stored copy. */
export function setDiagEnabled(value: boolean): void {
  enabled = value;
  if (value) return;
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = null;
  entries = [];
  windows.clear();
  try {
    globalThis.localStorage?.removeItem(STORE_KEY);
  } catch {
    // Storage unavailable: nothing was stored either.
  }
  for (const l of [...listeners]) l();
}

/** Test hook: verbose (debug build) or release collection. */
export function setDiagVerbose(value: boolean): void {
  verbose = value;
  windows.clear();
}

/** Keys whose string values may carry content: never kept. */
const CONTENT_KEY =
  /^(text|transcript|rawtext|prompt|content|body|audio|heard|rest|dictated|query|token|secret|password|key|apikey|url|uri|path|file|filename)$/i;

/** Keys holding error text: reduced to a class outside verbose builds. */
const ERROR_TEXT_KEY = /^(error|errorMessage|errorText|lastError|message|detail|cause)$/i;

/** First match wins. Every class also classifies as itself. */
const ERROR_CLASSES: [RegExp, string][] = [
  [/time[d -]?\s?out/i, 'timeout'],
  [/abort|cancel/i, 'canceled'],
  [/permission|not.?allowed|denied|forbidden|unauthori[sz]ed/i, 'permission'],
  [/network|connect|refused|unreachable|socket|fetch|offline|no route/i, 'network'],
  [/not[ _-]?found|no such/i, 'not_found'],
  [/busy|in use|taken/i, 'busy'],
  [/invalid|malformed|unexpected|parse|decode|unsupported/i, 'invalid'],
];

/** A fixed class for error text (or a code or name), never the text itself. */
export function errorClass(text: string): string {
  return ERROR_CLASSES.find(([pattern]) => pattern.test(text))?.[1] ?? 'other';
}

/** Port types of `AVAudioSession` by kind of output. */
const ROUTE_CLASSES: [RegExp, string][] = [
  [/^Speaker/, 'speaker'],
  [/^Receiver/, 'receiver'],
  [/^Bluetooth/, 'bluetooth'],
  [/^(Headphones|HeadsetMic|LineOut|LineIn|USBAudio)/, 'wired'],
];

/**
 * The kind of output of an iOS route description (`route=<AVAudioSession...`).
 * Port names can be a person's name ("Jane's AirPods").
 */
function routeClass(description: string): string {
  const type = /outputs\s*=[\s\S]*?type = (\w+)/.exec(description)?.[1] ?? '';
  return ROUTE_CLASSES.find(([pattern]) => pattern.test(type))?.[1] ?? 'other';
}

/**
 * A route ends with a `)>` that closes the output list (a port's `(null)>"`
 * is followed by a quote), before the next part of an activation note, or
 * with the text (a route cut by an older log's truncation).
 */
const ROUTE = /\broute=[\s\S]*?(?:\)>(?!\\?")|(?=\s\|\serrors:)|$)/g;

function maskRoutes(text: string): string {
  return text.replace(ROUTE, (route) => `route=${routeClass(route)}`);
}

/** Names that end a host name without a port; others would hit file names and labels. */
const HOST_ENDINGS =
  'com|net|org|io|ai|dev|app|co|kr|jp|cn|de|uk|us|eu|fr|cloud|xyz|info|biz|edu|gov|mil|internal|intranet|local|localdomain|localhost|lan|corp|home|test|example|invalid';

/**
 * Bare host names: dotted labels ending in `HOST_ENDINGS`, or any dotted name
 * with a port. Not when the dotted part is a version (`1.2.3.dev`), inside a
 * longer dotted word or part of a path.
 */
const BARE_HOST = new RegExp(
  String.raw`(?<![\w.@/\\-])(?!v?\d+(?:\.\d+)*[.:])(?:[a-z0-9_-]{1,63}\.)+` +
    String.raw`(?:(?:${HOST_ENDINGS})(?::\d{1,5})?(?![\w-]|\.[a-z0-9])|[a-z][a-z0-9-]{0,62}:\d{2,5}\b)`,
  'gi',
);

const REDACTIONS: [RegExp, string][] = [
  // URLs of any scheme (pairing links carry secrets, hosts are private).
  [/\b[a-z][a-z0-9+.-]*:\/\/[^\s"'<>]+/gi, '<url>'],
  [/\bbearer\s+[\w.~+/=-]+/gi, 'Bearer <redacted>'],
  [
    /\b(access_token|refresh_token|token|api[_-]?key|apikey|secret|password|passwd|pwd|auth|signature|sig|totp|otp)(\s*[=:]\s*)[^\s&"',;]+/gi,
    '$1$2<redacted>',
  ],
  // JSON Web Tokens and well-known key prefixes.
  [/\beyJ[\w-]{5,}\.[\w-]{5,}\.[\w-]*/g, '<redacted>'],
  [/\b(sk|pk|rk|ghp|gho|ghs|github_pat|xox[abprs]|AKIA)[-_A-Za-z0-9]{10,}/g, '<redacted>'],
  [/[\w.+-]+@[\w-]+(\.[\w-]+)+/g, '<email>'],
  // File paths (Windows drives, UNC, home folders).
  [/\b[A-Za-z]:\\[^\s"'<>|]*/g, '<path>'],
  [/\\\\[^\s"'<>|]+/g, '<path>'],
  [/(^|[\s"'=(])\/(Users|home|var|private|storage|data|sdcard)\/[^\s"'<>]*/g, '$1<path>'],
  [/\b\d{1,3}(\.\d{1,3}){3}(:\d+)?\b/g, '<ip>'],
  [BARE_HOST, '<host>'],
  // Long opaque strings: keys, hashes, base64 blobs.
  [/[A-Za-z0-9+/_-]{32,}={0,2}/g, '<redacted>'],
];

/** Masks URLs, tokens, keys, e-mail and IP addresses, host names and paths in `text`. */
export function redactText(text: string): string {
  let out = text;
  for (const [pattern, replacement] of REDACTIONS) out = out.replace(pattern, replacement);
  return out;
}

function clean(data: DiagData): DiagData {
  const out: DiagData = {};
  for (const [key, value] of Object.entries(data)) {
    if (typeof value !== 'string') {
      out[key] = value;
    } else if (CONTENT_KEY.test(key)) {
      out[key] = '[redacted]';
    } else if (!verbose && ERROR_TEXT_KEY.test(key)) {
      out[key] = errorClass(value);
    } else {
      const masked = redactText(verbose ? value : maskRoutes(value));
      out[key] = masked.length > MAX_STRING ? `${masked.slice(0, MAX_STRING)}...` : masked;
    }
  }
  return out;
}

entries = load();

/** Outside verbose builds: false when `event` exceeded its per-minute share. */
function admit(event: string, now: number): boolean {
  if (verbose) return true;
  const w = windows.get(event);
  if (!w || now - w.start >= 60_000) {
    windows.set(event, { start: now, count: 1, dropped: 0 });
    if (w?.dropped) push('diag.dropped', { event, count: w.dropped }, now);
    return true;
  }
  if (w.count >= DIAG_RATE_PER_MIN) {
    w.dropped += 1;
    return false;
  }
  w.count += 1;
  return true;
}

function push(event: string, data: DiagData, now: number) {
  entries.push({ at: now, event, data: clean(data) });
  entries = bound(entries, now);
}

/** Records one event (no-op for events this build does not keep). */
export function diag(event: string, data: DiagData = {}): void {
  if (!diagRecords(event)) return;
  const now = Date.now();
  if (!admit(event, now)) return;
  push(event, data, now);
  scheduleSave();
  for (const l of [...listeners]) l();
}

/** A code or error name as logged outside verbose builds. */
function errorLabel(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  return /^[\w.-]{1,40}$/.test(value) ? value : 'other';
}

/**
 * Describes an error without its stack. Outside verbose builds `error` is a
 * class and name and code are kept only when they look like identifiers.
 */
export function errorInfo(err: unknown): DiagData {
  const e = err as { name?: unknown; code?: unknown; message?: unknown } | null;
  const text = typeof e?.message === 'string' ? e.message : String(err);
  const name = typeof e?.name === 'string' ? e.name : undefined;
  const code = typeof e?.code === 'string' ? e.code : undefined;
  if (verbose) return { error: text, errorName: name, errorCode: code };
  const errorName = errorLabel(name);
  const errorCode = errorLabel(code);
  return {
    error: errorClass(
      `${errorCode ?? ''} ${errorName === 'Error' ? '' : (errorName ?? '')} ${text}`,
    ),
    errorName,
    errorCode,
  };
}

export function diagEntries(): readonly DiagEntry[] {
  return entries;
}

export function clearDiag(): void {
  entries = [];
  windows.clear();
  scheduleSave();
  for (const l of [...listeners]) l();
}

export function subscribeDiag(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function two(n: number, width = 2): string {
  return String(n).padStart(width, '0');
}

function day(d: Date): string {
  return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())}`;
}

/**
 * One line per entry, `HH:MM:SS.mmm event key=value ...`, under a
 * `# YYYY-MM-DD` line whenever the (local) day changes. Redacted again, so
 * entries stored by an older version cannot leak either.
 */
export function formatDiag(list: readonly DiagEntry[] = entries): string {
  const lines: string[] = [];
  let lastDay = '';
  for (const e of list) {
    const d = new Date(e.at);
    const today = day(d);
    if (today !== lastDay) {
      lines.push(`# ${today}`);
      lastDay = today;
    }
    const time = `${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}.${two(d.getMilliseconds(), 3)}`;
    const fields = Object.entries(clean(e.data))
      .filter(([, v]) => v !== undefined)
      .map(([k, v]) => `${k}=${typeof v === 'string' ? JSON.stringify(v) : String(v)}`)
      .join(' ');
    lines.push(redactText(fields ? `${time} ${e.event} ${fields}` : `${time} ${e.event}`));
  }
  return lines.join('\n');
}

/** What "Copy" puts on the clipboard: a short header and the redacted log. */
export function diagReport(header: Record<string, string | number>): string {
  const head = Object.entries(header)
    .map(([k, v]) => `${k}: ${redactText(String(v))}`)
    .join('\n');
  return `${head}\n\n${formatDiag()}`;
}

/** Logs app lifecycle events (visibility, focus, page show/hide, freeze/resume, network). */
export function installLifecycleDiag(): () => void {
  if (typeof document === 'undefined') return () => {};
  const state = () => ({
    visibility: document.visibilityState,
    focused: document.hasFocus(),
    online: typeof navigator === 'undefined' ? undefined : navigator.onLine,
  });
  const handlers: [EventTarget, string][] = [
    [document, 'visibilitychange'],
    [document, 'freeze'],
    [document, 'resume'],
    [window, 'pagehide'],
    [window, 'pageshow'],
    [window, 'focus'],
    [window, 'blur'],
    [window, 'online'],
    [window, 'offline'],
  ];
  const listeners = handlers.map(([target, name]) => {
    const fn = () => {
      diag(`lifecycle.${name}`, state());
      // The web view may be frozen or killed in the background: keep the log.
      if (name === 'pagehide' || document.visibilityState === 'hidden') flushDiag();
    };
    target.addEventListener(name, fn);
    return () => target.removeEventListener(name, fn);
  });
  diag('lifecycle.start', { ...state(), restoredEntries: entries.length });
  return () => listeners.forEach((off) => off());
}
