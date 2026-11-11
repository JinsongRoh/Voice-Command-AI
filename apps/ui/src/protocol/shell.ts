/**
 * Pure helpers for remote shell sessions: shell choices per machine OS,
 * classification spans (UTF-8 byte offsets) mapped onto JS strings, and
 * commands found in agent decisions.
 */
import type { Decision, DangerLevel, SessionInfo } from './types';

export type MachineOs = 'windows' | 'unix';

/** Shell names the server accepts, per OS (`default` is always allowed). */
export const SHELLS: Record<MachineOs, readonly string[]> = {
  windows: ['powershell', 'pwsh', 'cmd'],
  unix: ['bash', 'zsh', 'sh'],
};

/** Largest command `shell_run` accepts (8 KiB of UTF-8). */
export const MAX_COMMAND_BYTES = 8 * 1024;

/** Stable reason codes of `command_danger` (each has an i18n key `shell.reason.<code>`). */
export const DANGER_REASONS = [
  'recursive_delete',
  'delete',
  'disk_format',
  'permissions',
  'registry_write',
  'service_or_process_kill',
  'shutdown',
  'remote_code',
  'encoded_command',
  'secret_read',
  'git_destructive',
  'elevation',
] as const;

export type DangerReason = (typeof DANGER_REASONS)[number];

export function isKnownReason(code: string): code is DangerReason {
  return (DANGER_REASONS as readonly string[]).includes(code);
}

/** Ordered: safe < caution < dangerous. */
export const DANGER_RANK: Record<DangerLevel, number> = { safe: 0, caution: 1, dangerous: 2 };

const WINDOWS_PATH = /^[a-z]:[\\/]|^\\\\/i;
const UNIX_PATH = /^(\/|~(\/|$))/;

/**
 * Guesses the OS of a machine from folder paths it reported (session project
 * folders, folder suggestions). `null` when nothing tells.
 */
export function guessMachineOs(paths: readonly string[]): MachineOs | null {
  let windows = 0;
  let unix = 0;
  for (const p of paths) {
    if (WINDOWS_PATH.test(p)) windows += 1;
    else if (UNIX_PATH.test(p)) unix += 1;
  }
  if (windows === 0 && unix === 0) return null;
  return windows >= unix ? 'windows' : 'unix';
}

/** Shell choices for the picker: `default` first, then the OS's shells (all when unknown). */
export function shellChoices(os: MachineOs | null): string[] {
  if (os) return ['default', ...SHELLS[os]];
  return ['default', ...SHELLS.windows, ...SHELLS.unix];
}

function utf8Length(codePoint: number): number {
  if (codePoint < 0x80) return 1;
  if (codePoint < 0x800) return 2;
  if (codePoint < 0x10000) return 3;
  return 4;
}

/** UTF-8 byte length of a string. */
export function utf8ByteLength(text: string): number {
  let bytes = 0;
  for (const ch of text) bytes += utf8Length(ch.codePointAt(0)!);
  return bytes;
}

/**
 * Converts a UTF-8 byte offset into a JS (UTF-16) string index. An offset in
 * the middle of a character snaps to the character's start (`round: 'down'`)
 * or end (`round: 'up'`); offsets past the end give `text.length`.
 */
export function byteToIndex(text: string, byteOffset: number, round: 'down' | 'up'): number {
  if (byteOffset <= 0) return 0;
  let bytes = 0;
  let index = 0;
  for (const ch of text) {
    const width = utf8Length(ch.codePointAt(0)!);
    // The offset falls inside this character.
    if (bytes + width > byteOffset) return round === 'down' ? index : index + ch.length;
    bytes += width;
    index += ch.length;
    if (bytes === byteOffset) return index;
  }
  return text.length;
}

export interface HighlightSegment {
  text: string;
  risky: boolean;
}

/**
 * Splits `text` into plain and risky segments from `[start, end)` UTF-8 byte
 * spans (as sent by the server). Spans may overlap, be unsorted or run past
 * the end; risky parts always cover whole characters.
 */
export function highlightSegments(
  text: string,
  spans: readonly (readonly [number, number])[],
): HighlightSegment[] {
  const ranges = spans
    .map(([s, e]) => [byteToIndex(text, s, 'down'), byteToIndex(text, e, 'up')] as const)
    .filter(([s, e]) => e > s)
    .sort((a, b) => a[0] - b[0]);
  const merged: [number, number][] = [];
  for (const [s, e] of ranges) {
    const last = merged[merged.length - 1];
    if (last && s <= last[1]) last[1] = Math.max(last[1], e);
    else merged.push([s, e]);
  }
  const out: HighlightSegment[] = [];
  let pos = 0;
  for (const [s, e] of merged) {
    if (s > pos) out.push({ text: text.slice(pos, s), risky: false });
    out.push({ text: text.slice(s, e), risky: true });
    pos = e;
  }
  if (pos < text.length || out.length === 0) out.push({ text: text.slice(pos), risky: false });
  return out;
}

/**
 * The lines of one submission, as the server counts them: one trailing line
 * break is not a line of its own; every other `\n`, `\r\n` or `\r` starts
 * one. Two or more lines are held back for "Run these N lines?".
 */
export function submissionLines(text: string): string[] {
  const body = text.replace(/(\r\n|\n|\r)$/, '');
  return body.split(/\r\n|\n|\r/);
}

export type CommandProblem = 'empty' | 'multiline' | 'too_long';

/** Mirrors the server's checks for `shell_run` (one non-empty line, at most 8 KiB). */
export function commandProblem(command: string): CommandProblem | null {
  if (!command.trim()) return 'empty';
  if (/[\r\n]/.test(command)) return 'multiline';
  if (utf8ByteLength(command) > MAX_COMMAND_BYTES) return 'too_long';
  return null;
}

const COMMAND_PROMPT = /\b(bash|shell|powershell|command|run|execute)\b|명령|コマンド|comando/i;

/**
 * The command of a permission decision such as "Allow Bash command?\n<cmd>":
 * the detail lines under a headline that asks about a command. `null` when
 * the decision does not carry one.
 */
export function commandFromDecision(decision: Decision | undefined): string | null {
  if (!decision) return null;
  const [headline = '', ...details] = decision.prompt.split('\n');
  if (!COMMAND_PROMPT.test(headline)) return null;
  const command = details.join('\n').trim();
  return command || null;
}

/** Remaining whole seconds until `deadline` (never negative). */
export function secondsLeft(deadline: number, now: number): number {
  return Math.max(0, Math.ceil((deadline - now) / 1000));
}

/** `m:ss`, or `h:mm:ss` from one hour on. */
export function formatCountdown(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, '0');
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
}

/** The deadline that stops the shell first, and which one it is. */
export function nextDeadline(session: SessionInfo): { at: number; kind: 'idle' | 'hard' } | null {
  const shell = session.shell;
  if (!shell) return null;
  return shell.idle_deadline < shell.hard_deadline
    ? { at: shell.idle_deadline, kind: 'idle' }
    : { at: shell.hard_deadline, kind: 'hard' };
}

/** Control character for Ctrl+<letter> (Ctrl+D = 0x04). */
export function ctrlChar(letter: string): string {
  return String.fromCharCode(letter.toUpperCase().charCodeAt(0) - 64);
}

/** A six-digit TOTP code (spaces allowed while typing are removed by the caller). */
export function isTotpCode(code: string): boolean {
  return /^\d{6}$/.test(code);
}

/**
 * Issuer of the authenticator entry that `vca totp enroll` and the desktop
 * app create (`otpauth://totp/Voice%20Command%20AI:owner?...`).
 */
export const TOTP_ISSUER = 'Voice Command AI';

/** A backup code as typed: upper case, spaces and dashes removed. */
export function normalizeBackupCode(code: string): string {
  return code.replace(/[\s-]/g, '').toUpperCase();
}

/** A backup code (16 base32 characters, shown as `XXXX-XXXX-XXXX-XXXX`). */
export function isBackupCode(code: string): boolean {
  return /^[A-Z2-7]{16}$/.test(normalizeBackupCode(code));
}
