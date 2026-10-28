/**
 * TypeScript mirror of the `vca-core` wire protocol (crates/vca-core/src).
 *
 * Rules that follow from the serde attributes on the Rust side:
 * - Enums with `#[serde(tag = "type", rename_all = "snake_case")]` become
 *   discriminated unions on a `type` field with snake_case variant names.
 * - Plain unit enums with `rename_all = "snake_case"` become string unions.
 * - `AgentKind` uses `tag = "type", content = "name"`: unit variants are
 *   `{ type: "claude_code" }`, `Generic(String)` is `{ type: "generic", name }`.
 * - `Option<T>` is `T | null` (serde writes `null`, never omits the field).
 * - `PathBuf` is a string; `UnixMillis` (u64) is a number.
 * - `Output.data` (`Vec<u8>`) is a MessagePack `bin` on the wire, which
 *   decodes to `Uint8Array`; in JSON it is an array of numbers.
 *
 * Keep this file in sync with vca-core.
 */

/** Must equal `vca_core::PROTOCOL_VERSION`. */
export const PROTOCOL_VERSION = 1;

/** Milliseconds since the Unix epoch. */
export type UnixMillis = number;

/** `SessionId` is `#[serde(transparent)]` over a String. */
export type SessionId = string;

// ---------------------------------------------------------------- session.rs

export type AgentKind =
  | { type: 'claude_code' }
  | { type: 'codex' }
  | { type: 'gemini_cli' }
  | { type: 'antigravity' }
  | { type: 'grok' }
  | { type: 'cursor_cli' }
  | { type: 'copilot_cli' }
  | { type: 'aider' }
  | { type: 'opencode' }
  | { type: 'qwen_code' }
  | { type: 'generic'; name: string };

export type AgentKindType = AgentKind['type'];

export type CloudProvider =
  | 'jules'
  | 'cursor_cloud'
  | 'devin'
  | 'copilot_cloud'
  | 'claude_managed_agents'
  | 'claude_code_routines'
  | 'codex_cloud';

export type SessionOrigin =
  | { type: 'local'; agent: AgentKind; project_dir: string; pid: number | null }
  | { type: 'cloud'; provider: CloudProvider; remote_id: string; url: string | null };

export type SessionState =
  | { type: 'starting' }
  | { type: 'running' }
  | { type: 'idle' }
  | { type: 'awaiting_input' }
  | { type: 'awaiting_decision' }
  | { type: 'paused' }
  | { type: 'exited'; code: number | null }
  | { type: 'error'; message: string };

export type SessionStateType = SessionState['type'];

export type ControlTier =
  'managed_pty' | 'native_channel' | 'terminal_api' | 'observe_only' | 'cloud';

export interface SessionInfo {
  id: SessionId;
  origin: SessionOrigin;
  display_name: string;
  state: SessionState;
  control: ControlTier;
  profile: string | null;
  created_at: UnixMillis;
  last_activity: UnixMillis;
  /** Number of unread notifications for this session. */
  unread: number;
  /**
   * Short excerpt of recent output for the session card (`#[serde(default)]`:
   * older servers may omit it, which decodes as `undefined`).
   */
  preview: string | null;
}

export interface LaunchRequest {
  agent: AgentKind;
  project_dir: string;
  /** `#[serde(default)]` on the Rust side; always sent by this client. */
  create_dir: boolean;
  profile: string | null;
  /** `#[serde(default)]` on the Rust side; always sent by this client. */
  args: string[];
  display_name: string | null;
}

/** Request to start a new cloud session (CLD-03). */
export interface CloudLaunchRequest {
  provider: CloudProvider;
  repository: string;
  prompt: string;
  display_name: string | null;
}

export type SessionAction = 'pause' | 'resume' | 'stop' | 'restart';

// ------------------------------------------------------------------ event.rs

export type MessageRole = 'user' | 'assistant' | 'system' | 'tool';

export interface DecisionOption {
  id: string;
  label: string;
}

export interface Decision {
  id: string;
  prompt: string;
  options: DecisionOption[];
}

export type SessionEvent =
  | { type: 'output'; seq: number; at: UnixMillis; data: Uint8Array }
  | { type: 'message'; seq: number; at: UnixMillis; role: MessageRole; text: string }
  | { type: 'tool_call'; seq: number; at: UnixMillis; name: string; summary: string }
  | { type: 'state_changed'; seq: number; at: UnixMillis; state: SessionState }
  | { type: 'decision_requested'; seq: number; at: UnixMillis; decision: Decision }
  | { type: 'notification'; seq: number; at: UnixMillis; title: string; body: string };

export type ControlKey =
  'enter' | 'escape' | 'interrupt' | 'tab' | 'up' | 'down' | 'left' | 'right' | 'backspace';

export type Input =
  | { type: 'text'; text: string; submit: boolean }
  | { type: 'key'; key: ControlKey }
  | { type: 'choose'; decision_id: string; option_id: string };

// --------------------------------------------------------------- resource.rs

export interface SessionUsage {
  session_id: SessionId;
  cpu_percent: number;
  memory_bytes: number;
  process_count: number;
}

export type JobClass = 'light' | 'heavy';

export interface QueueEntry {
  job_id: string;
  session_id: SessionId | null;
  command: string;
  class: JobClass;
  queued_at: UnixMillis;
  /** 0 is the next job to be admitted. */
  position: number;
}

export interface ResourceSnapshot {
  at: UnixMillis;
  /** Average over all cores, 0-100. */
  cpu_percent: number;
  memory_used_bytes: number;
  memory_total_bytes: number;
  disk_read_bytes_per_sec: number;
  disk_write_bytes_per_sec: number;
  net_rx_bytes_per_sec: number;
  net_tx_bytes_per_sec: number;
  sessions: SessionUsage[];
  queue: QueueEntry[];
}

export type AdmissionVerdict =
  | { type: 'grant'; slot_id: string }
  | { type: 'queue'; job_id: string; position: number }
  | { type: 'deny'; reason: string };

// ------------------------------------------------------------------ error.rs

export type ErrorCode =
  | 'not_found'
  | 'invalid_request'
  | 'unauthorized'
  | 'forbidden'
  | 'conflict'
  | 'unsupported'
  | 'unavailable'
  | 'internal';

// --------------------------------------------------------------- protocol.rs

export interface Envelope<T> {
  /** Client-chosen request id, echoed in the response. Server-initiated messages use 0. */
  id: number;
  body: T;
}

export type ClientMsg =
  | { type: 'hello'; protocol_version: number; app_version: string }
  | { type: 'ping'; at: UnixMillis }
  | { type: 'list_sessions' }
  | { type: 'subscribe'; session_id: SessionId; from_seq: number | null }
  | { type: 'unsubscribe'; session_id: SessionId }
  | { type: 'send_input'; session_id: SessionId; input: Input }
  | { type: 'control'; session_id: SessionId; action: SessionAction }
  | { type: 'launch'; request: LaunchRequest }
  | { type: 'launch_cloud'; request: CloudLaunchRequest }
  | { type: 'get_resources' }
  | { type: 'reorder_queue'; job_id: string; position: number }
  | { type: 'cancel_queued'; job_id: string }
  | { type: 'mark_read'; session_id: SessionId }
  | { type: 'list_devices' }
  | { type: 'revoke_device'; device_id: string }
  /** Drop every remote connection and close the tunnel (SEC-09). */
  | { type: 'kill_switch' };

/** A device paired with the server (SEC-10). */
export interface PairedDevice {
  id: string;
  name: string;
  platform: string;
  paired_at: UnixMillis;
  last_seen: UnixMillis;
  /** True for the device that sent the request. */
  current: boolean;
}

/** `acknowledged`: the agent visibly reacted to the input. */
export type DeliveryStatus = 'queued' | 'delivered' | 'acknowledged';

export type ServerMsg =
  | { type: 'welcome'; protocol_version: number; server_version: string; server_name: string }
  | { type: 'pong'; at: UnixMillis }
  | { type: 'ok' }
  | { type: 'error'; code: ErrorCode; message: string }
  | { type: 'sessions'; sessions: SessionInfo[] }
  | { type: 'session_updated'; session: SessionInfo }
  | { type: 'session_removed'; session_id: SessionId }
  | { type: 'event'; session_id: SessionId; event: SessionEvent }
  | { type: 'stream_gap'; session_id: SessionId; first_available_seq: number }
  | { type: 'input_ack'; session_id: SessionId; status: DeliveryStatus }
  | { type: 'launched'; session: SessionInfo }
  | { type: 'resources'; snapshot: ResourceSnapshot }
  | { type: 'devices'; devices: PairedDevice[] };
