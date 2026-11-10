import type {
  AgentKind,
  CloudProvider,
  SessionEvent,
  SessionInfo,
  SessionOrigin,
  SessionState,
} from './types';
import { MAX_SESSION_NAME_CHARS } from './types';

/** Mirrors `SessionState::needs_attention` in vca-core. */
export function needsAttention(state: SessionState): boolean {
  return (
    state.type === 'awaiting_input' || state.type === 'awaiting_decision' || state.type === 'error'
  );
}

/** The session waits on a decision (its state or a pending decision). */
export function awaitsDecision(session: SessionInfo): boolean {
  return session.state.type === 'awaiting_decision' || !!session.pending_decision;
}

/**
 * Whether the session needs the person ("확인 필요"). The conductor's own
 * session only for a decision: its turn-ends and input waits live in 지휘소.
 */
export function sessionNeedsAttention(session: SessionInfo): boolean {
  return session.conductor ? awaitsDecision(session) : needsAttention(session.state);
}

/** Unread notifications that count (never the conductor's own session). */
export function unreadCount(session: SessionInfo): number {
  return session.conductor ? 0 : session.unread;
}

/** Mirrors `SessionEvent::seq` in vca-core. */
export function eventSeq(event: SessionEvent): number {
  return event.seq;
}

export function isCloud(
  origin: SessionOrigin,
): origin is Extract<SessionOrigin, { type: 'cloud' }> {
  return origin.type === 'cloud';
}

const AGENT_LABELS: Record<Exclude<AgentKind['type'], 'generic' | 'shell'>, string> = {
  claude_code: 'Claude Code',
  codex: 'Codex',
  gemini_cli: 'Gemini CLI',
  antigravity: 'Antigravity',
  grok: 'Grok',
  cursor_cli: 'Cursor CLI',
  copilot_cli: 'Copilot CLI',
  aider: 'Aider',
  opencode: 'opencode',
  qwen_code: 'Qwen Code',
};

/**
 * Display name of a generic agent: generic agents are often launched by path
 * (`/…/target/debug/vca-fake-agent`, `E:\…\tool.exe`), so show the
 * executable's basename without its extension.
 */
export function executableName(command: string): string {
  const base = folderName(command.trim());
  const stem = base.replace(/\.(exe|cmd|bat|com|ps1|sh|py|js|mjs|cjs)$/i, '');
  return stem || base;
}

const SHELL_LABELS: Record<string, string> = {
  powershell: 'PowerShell',
  pwsh: 'PowerShell 7',
  cmd: 'Command Prompt',
  bash: 'bash',
  zsh: 'zsh',
  sh: 'sh',
};

/** Display name of an allow-listed shell (`default` and unknown names pass through). */
export function shellLabel(name: string): string {
  return SHELL_LABELS[name] ?? name;
}

/** Product names are not translated. */
export function agentLabel(agent: AgentKind): string {
  if (agent.type === 'generic') return executableName(agent.name);
  if (agent.type === 'shell') return shellLabel(agent.name);
  return AGENT_LABELS[agent.type];
}

/** Whether a session is a remote shell (agent `shell`). */
export function isShellSession(session: SessionInfo): boolean {
  return session.origin.type === 'local' && session.origin.agent.type === 'shell';
}

/** Which session actions the server accepts for a session. */
export interface SessionCapabilities {
  pause: boolean;
  restart: boolean;
  stop: boolean;
  /** The session streams terminal output: only sessions VCA started do. */
  terminal: boolean;
}

/**
 * Mirrors `SessionManager::control` in crates/vca-server/src/sessions.rs:
 * only sessions VCA started itself (managed PTY) are registered with the
 * resource broker (pause / resume) and own their process (stop, restart).
 * Cloud sessions can only be stopped. Sessions found running in the user's
 * own terminal (terminal API, observe only) cannot be controlled at all:
 * VCA never suspends or ends processes it did not start.
 */
export function sessionCapabilities(session: SessionInfo): SessionCapabilities {
  if (isCloud(session.origin) || session.control === 'cloud') {
    return { pause: false, restart: false, stop: true, terminal: false };
  }
  const managed = session.control === 'managed_pty';
  return { pause: managed, restart: managed, stop: managed, terminal: managed };
}

/** An open remote shell accepts commands (not exited, not failed). */
export function isOpenShell(session: SessionInfo): boolean {
  return (
    isShellSession(session) && session.state.type !== 'exited' && session.state.type !== 'error'
  );
}

/** Full agent command or provider id for tooltips (generic agents keep their path). */
export function originTitle(origin: SessionOrigin): string {
  if (origin.type === 'local' && origin.agent.type === 'generic') return origin.agent.name;
  return originLabel(origin);
}

/**
 * Whether the server derives chat messages for this origin (Claude Code and
 * Codex transcripts, cloud provider APIs). Other agents only stream terminal output.
 */
export function hasChatTranscript(origin: SessionOrigin): boolean {
  if (origin.type === 'cloud') return true;
  return origin.agent.type === 'claude_code' || origin.agent.type === 'codex';
}

export const LOCAL_AGENT_TYPES = Object.keys(AGENT_LABELS) as Exclude<
  AgentKind['type'],
  'generic' | 'shell'
>[];

const PROVIDER_LABELS: Record<CloudProvider, string> = {
  jules: 'Jules',
  cursor_cloud: 'Cursor Cloud',
  devin: 'Devin',
  copilot_cloud: 'Copilot (cloud)',
  claude_managed_agents: 'Claude Managed Agents',
  claude_code_routines: 'Claude Code Routines',
  codex_cloud: 'Codex Cloud',
};

export const CLOUD_PROVIDERS = Object.keys(PROVIDER_LABELS) as CloudProvider[];

export function providerLabel(provider: CloudProvider): string {
  return PROVIDER_LABELS[provider];
}

/** Agent or provider name shown on a card. */
export function originLabel(origin: SessionOrigin): string {
  return origin.type === 'local' ? agentLabel(origin.agent) : providerLabel(origin.provider);
}

/** Short monogram used as a brand-neutral agent icon. */
export function originMonogram(origin: SessionOrigin): string {
  const label = originLabel(origin);
  // Split on separators and drop punctuation so names like `vca-fake-agent`
  // or `my_tool` give letters, never a stray `/` or `.`.
  const words = label
    .split(/[\s\-_.]+/)
    .map((w) => w.replace(/[^\p{L}\p{N}]/gu, ''))
    .filter(Boolean);
  const first = words[0] ?? '?';
  if (words.length >= 2) return (first[0]! + words[1]![0]!).toUpperCase();
  return first.slice(0, 2).toUpperCase();
}

/** Stable hue (0-359) per agent/provider so icons are distinguishable. */
export function originHue(origin: SessionOrigin): number {
  const key = origin.type === 'local' ? origin.agent.type : origin.provider;
  let h = 0;
  for (const ch of key) h = (h * 31 + ch.charCodeAt(0)) % 360;
  return h;
}

/** Last path component, handling both `/` and `\` separators. */
export function folderName(path: string): string {
  const parts = path.split(/[\\/]+/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

/**
 * Shortens a long path in the middle (`E:\\code\\…\\project`), keeping the
 * start (drive or home) and the end (the folder name), to `max` characters.
 */
export function middleEllipsis(path: string, max: number): string {
  const chars = [...path];
  if (chars.length <= max) return path;
  const tail = Math.ceil((max - 1) * 0.6);
  const head = max - 1 - tail;
  return `${chars.slice(0, head).join('')}…${chars.slice(chars.length - tail).join('')}`;
}

/** Project folder for local sessions, provider remote id for cloud sessions. */
export function sessionLocation(session: SessionInfo): string {
  return session.origin.type === 'local' ? session.origin.project_dir : session.origin.remote_id;
}

/** Strips ANSI escape sequences and control characters for previews. */
export function stripAnsi(text: string): string {
  return (
    text
      // CSI, OSC and two-character escape sequences
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[@-_]/g, '')
      // eslint-disable-next-line no-control-regex
      .replace(/[\x00-\x08\x0b-\x1f\x7f]/g, '')
  );
}

const decoder = new TextDecoder();

/** Decodes terminal output bytes as UTF-8 (lossy). */
export function decodeOutput(data: Uint8Array): string {
  return decoder.decode(data);
}

/** One-line excerpt of an event for card previews, or null when it has none. */
export function eventPreview(event: SessionEvent): string | null {
  switch (event.type) {
    case 'output': {
      const lines = stripAnsi(decodeOutput(event.data))
        .split(/\r?\n|\r/)
        .map((l) => l.trim())
        .filter(Boolean);
      return lines[lines.length - 1] ?? null;
    }
    case 'message':
      return event.text.split('\n')[0] ?? null;
    case 'tool_call':
      return `${event.name}: ${event.summary}`;
    case 'decision_requested':
      return event.decision.prompt.split('\n')[0] ?? null;
    case 'notification':
      return `${event.title}: ${event.body}`;
    case 'state_changed':
      return null;
  }
}

export type SessionNameProblem = 'too_long' | 'control_chars';

/**
 * Mirrors `vca_core::validate_session_name`: the trimmed name, or `null` for
 * a blank one (meaning the automatic name), and what makes it unacceptable.
 * Lengths count Unicode code points, like Rust's `chars()`.
 */
export function checkSessionName(input: string): {
  name: string | null;
  problem: SessionNameProblem | null;
} {
  const name = input.trim();
  if (!name) return { name: null, problem: null };
  // Unicode general category Cc, like `char::is_control`.
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f-\u009f]/.test(name)) return { name, problem: 'control_chars' };
  if ([...name].length > MAX_SESSION_NAME_CHARS) return { name, problem: 'too_long' };
  return { name, problem: null };
}

/** The automatic name of a session, when known (older servers omit it). */
export function automaticName(session: SessionInfo): string | null {
  return session.auto_name ?? (session.custom_name ? null : session.display_name);
}
