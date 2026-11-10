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
import type { PersonalTermsClientMsg, PersonalTermsServerMsg } from './personalTerms';
import type { PronunciationClientMsg, PronunciationServerMsg } from './pronunciations';
import type { TaskClientMsg, TaskServerMsg } from './tasks';

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
  | { type: 'generic'; name: string }
  /**
   * A plain remote shell opened with `open_shell`; `name` is the allow-listed
   * shell (`powershell`, `pwsh`, `cmd`, `bash`, `zsh` or `sh`).
   */
  | { type: 'shell'; name: string };

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
  /**
   * The name VCA picked on its own; `display_name` differs from it after a
   * rename. `#[serde(default)]`: missing or `null` from older servers.
   */
  auto_name?: string | null;
  /** True when `display_name` is a name the user chose (`#[serde(default)]`). */
  custom_name?: boolean;
  /**
   * Timeouts and owner of a remote shell session (agent `shell`);
   * `#[serde(default)]`: absent or `null` for every other session.
   */
  shell?: ShellInfo | null; /** The decision the agent waits on; absent from older servers. */
  pending_decision?: PendingDecisionInfo | null;
  /**
   * Sub-agents the session's agent is running now. Left out when 0
   * (`skip_serializing_if`), and by older servers.
   */
  subagents?: number;
  /**
   * True for the conductor's own session: its turn-ends, unread counts and
   * notifications are not alerts (its replies reach 지휘소); its permission
   * decisions still are. Left out when false, and by older servers.
   */
  conductor?: boolean;
  /**
   * A selection menu only the terminal answers (a managed session without
   * a transcript yet, or Claude Code's "trust this folder" question). Left
   * out when none, and by older servers.
   */
  terminal_prompt?: TerminalPrompt | null;
  /**
   * The agent ended its turn with a plain question to the person (not a
   * form or a permission prompt); cleared once the conversation moves on.
   * Left out when none, and by older servers.
   */
  question?: AgentQuestion | null;
  /**
   * The agent's current (or last) turn, from its hooks and its transcript:
   * the chat's "Working · 42s" line. Left out when unknown, and by older
   * servers.
   */
  progress?: TurnProgress | null;
  /**
   * Something keeps VCA from following the session well, for example
   * `hooks_missing` (`SESSION_PROBLEM_HOOKS_MISSING`). Left out when none,
   * and by older servers.
   */
  problem?: string | null;
}

/** `SessionInfo.problem`: the agent's VCA hooks are missing; progress shows late. */
export const SESSION_PROBLEM_HOOKS_MISSING = 'hooks_missing';

/** An agent's turn (`SessionInfo.progress`). Times are the server's clock. */
export interface TurnProgress {
  started_at: UnixMillis;
  /** Absent while the turn runs. */
  ended_at?: UnixMillis | null;
  /** What the agent does now, when known. */
  activity?: TurnActivity | null;
}

/** `thinking`: a thinking block (its text is never sent). `tool`: its newest tool call. */
export type TurnActivity = { type: 'thinking' } | { type: 'tool'; name: string; summary: string };

export type TerminalPrompt = 'trust_folder' | 'menu';

/** A question in an agent's final message of a turn (`SessionInfo.question`). */
export interface AgentQuestion {
  /** One per turn; alert bookkeeping keys on it. */
  id: string;
  /** The question sentence, shortened. */
  text: string;
  at: UnixMillis;
}

export interface PendingDecisionInfo {
  decision: Decision;
  at: UnixMillis;
}

/**
 * Timeouts of a remote shell. The server stops the shell at `idle_deadline`
 * (no input from any client for `idle_timeout_secs`) or at `hard_deadline`,
 * whichever comes first. `idle_deadline` moves with input and never passes
 * `hard_deadline`.
 */
export interface ShellInfo {
  idle_timeout_secs: number;
  idle_deadline: UnixMillis;
  hard_deadline: UnixMillis;
  /** Name of the device that opened the shell (`local` for the computer itself). */
  opened_by: string;
}

/** Request to open a remote shell (`open_shell`, admin scope). */
export interface ShellRequest {
  /** `powershell`, `pwsh`, `cmd`, `bash`, `zsh`, `sh` or `default`; never a path. */
  shell: string;
  /** Must be inside the server's workspace roots. */
  project_dir: string;
  display_name: string | null;
  /** A fresh, single-use TOTP code. */
  totp: string;
}

/** Must equal `vca_core::MAX_SESSION_NAME_CHARS`. */
export const MAX_SESSION_NAME_CHARS = 80;

export interface LaunchRequest {
  agent: AgentKind;
  project_dir: string;
  /** `#[serde(default)]` on the Rust side; always sent by this client. */
  create_dir: boolean;
  profile: string | null;
  /** `#[serde(default)]` on the Rust side; always sent by this client. */
  args: string[];
  display_name: string | null;
  /** Start a new conversation instead of continuing the folder's latest
   * Claude Code conversation. Omitted means continue. */
  fresh?: boolean;
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
  /**
   * The exact shell command from the agent's hook. `prompt` is display text
   * and never decides anything; a spoken approval needs this.
   */
  command?: string;
  /**
   * Inferred without agent hooks (an unanswered tool call while the agent
   * idles): probably waiting, answered only in the agent's own terminal.
   * Has no options.
   */
  inferred?: boolean;
  /**
   * A question form (Claude Code's `AskUserQuestion`): answered with an
   * `answer` input, one answer per question. Such a decision has no
   * options. Left out when empty, and by older servers.
   */
  questions?: Question[];
}

/** One question of a question form (`vca_core::Question`). */
export interface Question {
  /** Short tab label; may be empty. */
  header: string;
  question: string;
  options: QuestionOption[];
  /** Several options may be picked (check boxes). */
  multi_select: boolean;
  /** The form takes a typed answer ("Type something."). */
  free_text: boolean;
}

export interface QuestionOption {
  label: string;
  description: string;
}

/** The answer to one question: picked option indices and/or typed text. */
export interface QuestionAnswer {
  options: number[];
  text: string | null;
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
  | {
      type: 'text';
      text: string;
      submit: boolean;
      /**
       * A person typed this by hand. While a decision is open the server
       * refuses submitted text without it: voice and automatic sends leave
       * it out.
       */
      at_prompt?: boolean;
    }
  | { type: 'key'; key: ControlKey }
  | { type: 'choose'; decision_id: string; option_id: string }
  | { type: 'answer'; decision_id: string; answers: QuestionAnswer[] };

// --------------------------------------------------------------- resource.rs

export interface SessionUsage {
  session_id: SessionId;
  cpu_percent: number;
  memory_bytes: number;
  process_count: number;
  /** A session VCA did not start: shown only, never paused or stopped. */
  monitor_only?: boolean;
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
  | 'internal'
  /** The action needs a TOTP code (remote shell step-up): resend with one. */
  | 'totp_required'
  /** The TOTP code was wrong or already used. */
  | 'totp_invalid';

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
  | { type: 'history'; session_id: SessionId; before_seq: number | null; limit: number }
  | { type: 'send_input'; session_id: SessionId; input: Input }
  | { type: 'control'; session_id: SessionId; action: SessionAction }
  | { type: 'launch'; request: LaunchRequest }
  | { type: 'launch_cloud'; request: CloudLaunchRequest }
  | { type: 'get_resources' }
  | { type: 'reorder_queue'; job_id: string; position: number }
  | { type: 'cancel_queued'; job_id: string }
  | { type: 'mark_read'; session_id: SessionId }
  /**
   * Sets the card name; `null` or a blank name restores the automatic one.
   * Answered with `ok`; the name arrives as `session_updated`.
   */
  | { type: 'rename_session'; session_id: SessionId; name: string | null }
  /**
   * Takes an ended session off every client's list (only when `welcome.features`
   * has `dismiss_session`, see FEATURE_DISMISS_SESSION). `stop` ("Remove from
   * VCA", only with FEATURE_DISMISS_STOP): a running session VCA started is
   * stopped first and removed once it exited, all on the server.
   */
  | { type: 'dismiss_session'; session_id: SessionId; stop?: boolean }
  /**
   * Resets a remote shell's idle timer after a `shell_closing_soon` notice
   * (servers with `shell_keep_alive`; never past the shell's hard deadline).
   */
  | { type: 'shell_keep_alive'; session_id: SessionId }
  /** The card order (only with `session_order`); answered with `session_order`. */
  | { type: 'get_session_order' }
  /**
   * Puts the cards in this order, first card first (command scope). Sessions
   * not listed follow; answered with `session_order`, also pushed to every
   * client.
   */
  | { type: 'set_session_order'; session_ids: SessionId[] }
  /** Back to the default order (oldest first); answered like `set_session_order`. */
  | { type: 'reset_session_order' }
  | { type: 'list_devices' }
  | { type: 'revoke_device'; device_id: string }
  /** Admin scope; answered with the updated `devices` list. */
  | { type: 'rename_device'; device_id: string; name: string }
  /** Admin scope; a device cannot change its own scope. Answered with `devices`. */
  | { type: 'set_device_scope'; device_id: string; scope: DeviceScope }
  /** Drop every remote connection and close the tunnel (SEC-09). */
  | { type: 'kill_switch' }
  | { type: 'list_cloud_providers' }
  /** Server-side speech recognition (VOC-01); answered with `transcript`. */
  | {
      type: 'transcribe';
      audio: Uint8Array;
      format: AudioEncoding;
      sample_rate: number | null;
      language: string | null;
      session_id: SessionId | null;
      /**
       * Text that precedes this audio (e.g. the last ~300 characters already
       * dictated), used as a recognition prompt for continuity. Omitted when
       * there is none.
       */
      context?: string | null;
      /**
       * True for a FINAL dictation chunk (or a whole recording): only these
       * get the server's contextual (LLM) correction. Omitted for interim
       * previews.
       */
      final?: boolean;
    }
  /** Speech engine and correction LLM profiles; answered with `speech_options`. */
  | { type: 'list_speech_options' }
  /**
   * This device's engine and LLM by profile id (`null`: server default;
   * `llm: 'none'`: no correction). Needs command scope; answered with
   * `speech_options`.
   */
  | { type: 'set_speech_preferences'; engine: string | null; llm: string | null }
  /**
   * Speaks a short alert title (at most `TTS_MAX_TEXT_CHARS`) with a TTS
   * engine on the PC (`engine: null`: the server default). View scope;
   * answered with `audio`.
   */
  | {
      type: 'synthesize';
      text: string;
      language: string | null;
      engine: string | null;
      /** A `speakers` id of that engine; `null` for the engine's configured voice. */
      voice?: string | null;
    }
  /** The PC's TTS engines (names and languages only); answered with `tts_engines`. */
  | { type: 'list_tts_engines' }
  /**
   * A short spoken status of the computer's sessions, from fixed templates
   * (no LLM). `order`: this client's card order, so numbers match. View
   * scope; answered with `briefing`.
   */
  | { type: 'briefing'; language: string | null; order?: SessionId[] }
  /** This device's opt-in to result summaries (off until sent). Answered with `ok`. */
  | { type: 'set_voice_summary'; enabled: boolean; details: boolean }
  /**
   * A 1-2 sentence summary of the session's last answer in `language`
   * (needs the opt-in). `number`: the card number for the spoken prefix.
   * Answered with `summary`.
   */
  | { type: 'summarize'; session_id: SessionId; language: string | null; number?: number | null }
  /** One small file in a single message (at most `ATTACH_CHUNK_BYTES`). */
  | {
      type: 'attach';
      session_id: SessionId;
      name: string;
      mime: string;
      data: Uint8Array;
      note: string | null;
      /** Press Enter after the mention (default false). */
      submit: boolean;
    }
  /** Starts a chunked upload; answered with `attach_ready`. */
  | {
      type: 'attach_begin';
      session_id: SessionId;
      name: string;
      mime: string;
      size: number;
      note: string | null;
      submit: boolean;
      /** Set for the files of a folder upload. */
      folder: AttachFolderEntry | null;
      /**
       * Client-chosen id of several files sent together: they are not
       * mentioned one by one; `attach_batch_end` mentions them once.
       */
      batch?: string | null;
    }
  /** The next bytes, in order (`offset` = bytes sent so far); answered with `ok`. */
  | { type: 'attach_chunk'; upload_id: string; offset: number; data: Uint8Array }
  /** Answered with `attached`. */
  | { type: 'attach_end'; upload_id: string }
  /** Cancels an upload or a folder upload by id. */
  | { type: 'attach_cancel'; upload_id: string }
  /** Answered with `attach_folder_ready`. */
  | {
      type: 'attach_folder_begin';
      session_id: SessionId;
      name: string;
      files: number;
      total_bytes: number;
      note: string | null;
      submit: boolean;
    }
  /** Answered with `attached` for the folder (one mention). */
  | { type: 'attach_folder_end'; folder_id: string }
  /** Answered with `attached` for the batch folder: one mention of all its files. */
  | { type: 'attach_batch_end'; batch_id: string; note: string | null; submit: boolean }
  // --- Remote shells (admin scope, except `classify_command`). ---
  /** Opens a plain shell; answered with `launched` (agent `shell`). */
  | { type: 'open_shell'; request: ShellRequest }
  /**
   * Types `command` and Enter into a shell session; answered with
   * `shell_ran`. A `dangerous` command outside the device's step-up window
   * is answered with `totp_required`; resend with `totp`. Two or more lines
   * (one trailing line break does not count) are answered with
   * `shell_confirm` and only typed when resent with `confirm` (servers with
   * `shell_confirm_lines`).
   */
  | {
      type: 'shell_run';
      session_id: SessionId;
      command: string;
      totp: string | null;
      confirm?: boolean;
    }
  /**
   * Lets this device watch and type into a shell another device opened
   * (servers with `shell_join`). Always needs a fresh code; answered with
   * `ok`. Until then that shell's requests fail with `totp_required` and a
   * message that mentions joining.
   */
  | { type: 'shell_join'; session_id: SessionId; totp: string }
  /** Classifies a command line without running it; answered with `command_danger`. */
  | { type: 'classify_command'; command: string }
  /** Newest audit entries (`0`: server default of 100, at most 1000); answered with `audit`. */
  | { type: 'list_audit'; limit: number }
  | { type: 'restore_list' }
  | { type: 'restore_capture'; profile: string | null }
  | { type: 'restore_edit'; edit: RestoreEdit }
  | { type: 'restore_import'; layout: string; profile: string | null }
  | { type: 'restore_run'; request: RestoreRunRequest }
  // Screen layouts (only when `welcome.features` has `window_arrange`).
  | { type: 'arrange_list' }
  /**
   * The server finds each window in its live list and works out the
   * rectangles. Moving windows from a paired device needs the step-up
   * window or a fresh `totp` code (`totp_required` otherwise), like
   * `restore_run`; so do `arrange_apply_layout` and `arrange_undo`.
   */
  | { type: 'arrange_apply'; monitors: ArrangeMonitorPlan[]; totp?: string }
  | { type: 'arrange_apply_layout'; layout: string; totp?: string }
  /** `conflict` when there is nothing to undo. */
  | { type: 'arrange_undo'; totp?: string }
  | { type: 'arrange_edit'; edit: ArrangeEdit }
  /**
   * Brings a session's terminal window to the front after a click; answered
   * with `ok`. Only this computer's own app may send it (`forbidden` from a
   * paired device), Windows only (`unsupported`), and Windows may refuse to
   * hand over the focus (`unavailable`).
   */
  | { type: 'focus_terminal'; session_id: SessionId }
  // Project folder picker: only when `welcome.features` has `project_folders`.
  | { type: 'project_folders' }
  | { type: 'list_folder'; path: string; show_hidden?: boolean }
  | { type: 'folder_path'; parent: string; name: string }
  // Session discovery: only when `welcome.features` has `scan_sessions`.
  /** Looks for AI sessions on the PC now (view scope); answered with `scan_report`. */
  | { type: 'scan_sessions' }
  /** Takes a found session off the list (command scope); the process is not touched. */
  | { type: 'hide_observed'; session_id?: SessionId | null; pid?: number | null }
  /** Lists a hidden session again, or every one with `all` (command scope). */
  | { type: 'unhide_observed'; session_id?: SessionId | null; pid?: number | null; all?: boolean }
  // The conductor (only when `welcome.features` has `conductor`).
  /** Its state (view scope); answered with `conductor`. */
  | { type: 'conductor_get' }
  /** Setup and the opt-in: the desktop app of the PC only (`forbidden` elsewhere). */
  | { type: 'conductor_configure'; settings: ConductorSettings }
  /** "지휘 세션 시작": the PC only. */
  | { type: 'conductor_start' }
  /**
   * Stops it; `kill` also revokes its tools (command scope). The kill switch
   * needs a step-up: the PC's local verification (`step_up_required` first,
   * action `conductor_kill`), or from a paired device the owner's
   * authenticator `code` (`totp_required` without one, `totp_invalid` for a
   * wrong or reused one, `forbidden` after too many wrong ones, `unsupported`
   * with no authenticator set up, `unavailable` with security services off).
   */
  | { type: 'conductor_stop'; kill?: boolean; code?: string }
  /**
   * Pauses or resumes the Command Center (command scope, phones too; only
   * when `welcome.features` has `command_center`): nothing wakes a paused
   * conductor, its session keeps running.
   */
  | { type: 'cc_pause'; paused: boolean }
  /** The person's answer to a conductor request (command scope). */
  | { type: 'conductor_decide'; request_id: string; allow: boolean }
  /** Sends to the session skip confirmation for `minutes` (`0` removes); the PC only. */
  | { type: 'conductor_allow_session'; session_id: SessionId; minutes: number }
  /** An instruction for the conductor (command scope); the reply comes as `conductor_reply`. */
  | { type: 'conductor_ask'; text: string }
  // "신뢰하고 시작": Enter on the conductor terminal's "trust this folder" question.
  | { type: 'conductor_trust_folder' }
  // The human task inbox (only when `welcome.features` has `human_tasks`); see tasks.ts.
  | TaskClientMsg
  // The pronunciation dictionary (only with `pronunciations`); see pronunciations.ts.
  | PronunciationClientMsg
  // The commonly misheard words (only with `personal_terms`); see personalTerms.ts.
  | PersonalTermsClientMsg;

/** `welcome.features` entry of servers with the project folder picker. */
export const FEATURE_PROJECT_FOLDERS = 'project_folders';

/** `welcome.features` entry of servers that answer `scan_sessions`. */
export const FEATURE_SCAN_SESSIONS = 'scan_sessions';

/** `welcome.features` entry of servers that keep a card order the user chose. */
export const FEATURE_SESSION_ORDER = 'session_order';

/** `welcome.features` entry of servers that answer the `arrange_*` messages. */
export const FEATURE_WINDOW_ARRANGE = 'window_arrange';

/** Why a found AI agent process is (or is not) on the session list. */
export type FoundStatus =
  'imported' | 'hidden_by_user' | 'no_transcript_yet' | 'unsupported_agent' | 'not_in_a_terminal';

/** One AI agent process found on the PC. Plain shells never appear. */
export interface FoundAgent {
  pid: number;
  agent: AgentKind;
  project_dir?: string | null;
  /** The terminal or app hosting it, e.g. `Windows Terminal`, `tmux`, `ChatGPT`. */
  terminal?: string | null;
  /** The session name or the agent's conversation title. */
  title?: string | null;
  status: FoundStatus;
  /** The listed session (`imported`, `no_transcript_yet`). */
  session_id?: SessionId | null;
}

/** Operating system family of the server. */
export type HostOs = 'windows' | 'macos' | 'linux';

/** A folder from the server; `path` is sent back as is, never rebuilt. */
export interface FolderEntry {
  name: string;
  path: string;
  is_git_repo: boolean;
  has_agent_history: boolean;
  last_used?: number | null;
}

export interface FolderCrumb {
  name: string;
  path: string;
}

/** Answer to `project_folders`. */
export interface ProjectFolders {
  roots: FolderEntry[];
  /** Most recent first, at most 30. */
  recent: FolderEntry[];
  os: HostOs;
  separator: string;
}

/** Answer to `list_folder`: sub-folders only. */
export interface FolderListing {
  path: string;
  name: string;
  /** `null` at a workspace root. */
  parent: string | null;
  root: string;
  breadcrumbs: FolderCrumb[];
  entries: FolderEntry[];
  truncated: boolean;
}

/** How risky a shell command is, as classified by the server. */
export type DangerLevel = 'safe' | 'caution' | 'dangerous';

/** One line of the remote shell audit log. */
// ---------------------------------------------------------------- restore.rs

export interface WindowRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface RestoreMonitor {
  id: string;
  rect: WindowRect;
  work: WindowRect;
  dpi: number;
}

export type RestoreEntrySource = 'auto' | 'manual' | 'imported';

export interface RestoreEntry {
  id: string;
  title: string;
  agent: AgentKind;
  folder: string;
  session_id: string | null;
  rect: WindowRect | null;
  monitor: RestoreMonitor | null;
  maximized: boolean;
  enabled: boolean;
  order: number;
  args: string[];
  transcript: string | null;
  note: string | null;
  source: RestoreEntrySource;
  updated_at: UnixMillis;
}

export interface RestoreProfile {
  id: string;
  name: string;
  delay_secs: number;
  /** Off by default; only the computer itself can set it. */
  auto_prompt: string | null;
  entries: RestoreEntry[];
}

export type RestoreItemStatus =
  | 'waiting'
  | 'opening'
  | 'placed'
  /** Opened, but not put in place (Wayland, tmux, no saved position). */
  | 'opened'
  | 'skipped_open'
  | 'skipped_missing'
  | 'skipped_outside'
  | 'failed';

export type RestoreResume = 'exact' | 'latest' | 'fresh';

export interface RestoreItem {
  entry_id: string;
  title: string;
  folder: string;
  status: RestoreItemStatus;
  resume: RestoreResume;
  command: string;
  rect: WindowRect | null;
  delay_after_secs: number;
  message: string | null;
}

export interface RestoreJob {
  id: string;
  profile: string;
  by: string;
  dry_run: boolean;
  started_at: UnixMillis;
  finished_at: UnixMillis | null;
  items: RestoreItem[];
}

export interface RestoreEntryLive {
  entry_id: string;
  open: boolean;
  session_id: SessionId | null;
}

export interface RestoreOverview {
  supported: boolean;
  platform: string;
  message: string | null;
  snapshot_interval_secs: number;
  last_snapshot_at: UnixMillis | null;
  active_profile: string;
  profiles: RestoreProfile[];
  live: RestoreEntryLive[];
  monitors: RestoreMonitor[];
  job: RestoreJob | null;
}

export interface RestoreRunRequest {
  profile?: string | null;
  only?: string[];
  entry_ids?: string[];
  dry_run?: boolean;
  totp?: string | null;
}

/** A session in an open terminal window. */
export interface ArrangeSession {
  id: SessionId;
  title: string;
  agent: AgentKind;
  folder: string;
}

/** An open terminal window; several sessions in it are tabs and move together. */
export interface ArrangeWindow {
  /** Valid until the window closes; only for requests, never stored. */
  id: string;
  title: string;
  monitor: string | null;
  maximized: boolean;
  minimized: boolean;
  sessions: ArrangeSession[];
}

export interface ArrangeCell {
  /** Row by row from the top left. */
  cell: number;
  window: string;
}

export interface ArrangeMonitorPlan {
  monitor: string;
  rows: number;
  cols: number;
  cells: ArrangeCell[];
}

/** How a saved layout finds a session again; never a window handle. */
export interface ArrangeWindowKey {
  agent: AgentKind;
  folder: string;
  session_id: string | null;
  title: string;
}

export interface ArrangeSavedCell {
  cell: number;
  title: string;
  sessions: ArrangeWindowKey[];
}

export interface ArrangeSavedMonitor {
  monitor: string;
  rows: number;
  cols: number;
  cells: ArrangeSavedCell[];
}

export interface ArrangeLayout {
  id: string;
  name: string;
  saved_at: UnixMillis;
  monitors: ArrangeSavedMonitor[];
}

export type ArrangeItemStatus =
  'moved' | 'skipped_not_open' | 'skipped_monitor' | 'skipped_duplicate' | 'failed';

export interface ArrangeItem {
  /** Empty for a window the server does not know. */
  title: string;
  status: ArrangeItemStatus;
  message: string | null;
}

export interface ArrangeResult {
  moved: number;
  skipped: number;
  failed: number;
  items: ArrangeItem[];
}

export interface ArrangeOverview {
  supported: boolean;
  message: string | null;
  monitors: RestoreMonitor[];
  windows: ArrangeWindow[];
  layouts: ArrangeLayout[];
  /** How many windows an undo puts back; 0: nothing to undo. */
  undo_windows: number;
  /**
   * By layout id: its grids with the windows of `windows` open now, for the
   * editor to load. Cells of windows not found and monitors not connected are
   * left out. Never saved; absent from servers before it.
   */
  layout_plans?: Record<string, ArrangeMonitorPlan[]>;
}

export type ArrangeEdit =
  | { op: 'layout_save'; name: string; monitors: ArrangeMonitorPlan[] }
  | { op: 'layout_rename'; layout: string; name: string }
  | { op: 'layout_delete'; layout: string };

export interface RestoreEntryDraft {
  title: string;
  agent: AgentKind;
  folder: string;
  session_id: string | null;
  rect: WindowRect | null;
  maximized: boolean;
  enabled: boolean;
}

export type RestoreEdit =
  | { op: 'profile_add'; name: string }
  | { op: 'profile_rename'; profile: string; name: string }
  | { op: 'profile_delete'; profile: string }
  | { op: 'profile_activate'; profile: string }
  | { op: 'profile_settings'; profile: string; delay_secs: number; auto_prompt: string | null }
  | { op: 'entry_add'; profile: string; entry: RestoreEntryDraft }
  | { op: 'entry_update'; profile: string; entry_id: string; entry: RestoreEntryDraft }
  | { op: 'entry_delete'; profile: string; entry_id: string }
  | { op: 'entry_set_enabled'; profile: string; entry_id: string; enabled: boolean }
  | { op: 'entry_reorder'; profile: string; entry_ids: string[] };

export interface AuditEntry {
  ts: UnixMillis;
  device_id: string;
  device_name: string;
  /** `shell_open`, `shell_close`, `shell_run`, `step_up_ok` or `step_up_failed`. */
  event: string;
  /** `#[serde(default)]`. */
  session_id?: SessionId | null;
  /** Never contains TOTP codes; command text is redacted by the server. */
  detail: string;
}

/** Must equal `vca_core::ATTACH_CHUNK_BYTES`: largest data per attach message. */
export const ATTACH_CHUNK_BYTES = 1024 * 1024;

/** The place of one file inside a folder upload. */
export interface AttachFolderEntry {
  folder_id: string;
  /** Path relative to the folder, `/`-separated. */
  path: string;
}

/** The `llm` preference that turns the correction off. */
export const SPEECH_LLM_NONE = 'none';

/** A speech engine or correction LLM profile of the server (no secrets). */
export interface SpeechProfileInfo {
  id: string;
  label: string;
  /** `openai_compatible`, `openai`, `google` or `anthropic`. */
  kind: string;
  /** A key is available on the server (or none is needed). */
  configured: boolean;
  /** The server default. */
  default: boolean;
}

/** Longest text of a `synthesize` request, in characters. */
export const TTS_MAX_TEXT_CHARS = 300;

/** A TTS engine on the PC: never its URL, model or key. */
export interface TtsEngineInfo {
  name: string;
  /** Languages with their own voice; empty: one voice for all. */
  languages: string[];
  /** The server default. */
  default: boolean;
  /** Speakers the user may choose; empty or missing: the configured voice only. */
  speakers?: TtsSpeaker[];
  /** Text to show where the engine is offered (licence: audio is machine-generated). */
  disclosure?: string | null;
}

export type TtsGender = 'female' | 'male' | 'other';

/** A speaker of a TTS engine; `id` goes into `synthesize.voice`. */
export interface TtsSpeaker {
  id: string;
  label: string;
  gender: TtsGender;
}

export type AudioEncoding = 'wav' | 'pcm16' | 'webm' | 'ogg' | 'm4a';

/**
 * A term replaced by the developer vocabulary engine (VOC-05), or by the
 * server's optional contextual (LLM-based) correction pass (`context`).
 */
export interface TranscriptCorrection {
  from: string;
  to: string;
  layer: 'base' | 'project' | 'personal' | 'context';
}

/** How the server routed a transcript (VOC-06). */
export type TranscriptIntent =
  | { type: 'prompt' }
  | {
      type: 'control';
      action: string;
      session_action: SessionAction | null;
      session_id: SessionId | null;
      confidence: number;
    }
  | { type: 'ambiguous' };

/** A cloud provider as the server offers it (CLD-07). */
export interface CloudProviderInfo {
  provider: CloudProvider;
  account: string;
  configured: boolean;
  create: boolean;
  follow_up: boolean;
  stop: boolean;
  approve_plan: boolean;
  status_only: boolean;
  unverified: boolean;
}

/** A device paired with the server (SEC-10). */
export interface PairedDevice {
  id: string;
  name: string;
  platform: string;
  paired_at: UnixMillis;
  last_seen: UnixMillis;
  /** True for the device that sent the request. */
  current: boolean;
  // Newer servers only (absent or null from older ones).
  scope?: DeviceScope | null;
  /** At least one live remote connection. */
  online?: boolean;
  /** Number of live remote connections. */
  connections?: number;
  /** Remote address of the most recent connection. */
  last_ip?: string | null;
  app_version?: string | null;
}

/** Permission of a paired device. Ordered: view < command < admin. */
export type DeviceScope = 'view' | 'command' | 'admin';

/** `acknowledged`: the agent visibly reacted to the input. */
export type DeliveryStatus = 'queued' | 'delivered' | 'acknowledged';

export type ServerMsg =
  | {
      type: 'welcome';
      protocol_version: number;
      server_version: string;
      server_name: string;
      /** Optional requests the server answers; absent from older servers. */
      features?: string[];
    }
  | { type: 'pong'; at: UnixMillis }
  | { type: 'ok' }
  | { type: 'error'; code: ErrorCode; message: string }
  | { type: 'sessions'; sessions: SessionInfo[] }
  | { type: 'session_updated'; session: SessionInfo }
  | { type: 'session_removed'; session_id: SessionId }
  /**
   * A remote shell is about to close (pushed once per deadline, about 60 s
   * ahead): `idle` can be extended with `shell_keep_alive`, `lifetime` cannot.
   */
  | {
      type: 'shell_closing_soon';
      session_id: SessionId;
      seconds_left: number;
      reason?: 'idle' | 'lifetime';
    }
  /**
   * The card order, first card first (also the order of `sessions`). Pushed
   * when it changes; `custom` when the user chose it.
   */
  | { type: 'session_order'; session_ids: SessionId[]; custom: boolean }
  | { type: 'event'; session_id: SessionId; event: SessionEvent }
  | { type: 'stream_gap'; session_id: SessionId; first_available_seq: number }
  | { type: 'history'; session_id: SessionId; events: SessionEvent[]; has_more: boolean }
  | { type: 'input_ack'; session_id: SessionId; status: DeliveryStatus }
  | { type: 'launched'; session: SessionInfo }
  | { type: 'resources'; snapshot: ResourceSnapshot }
  | { type: 'devices'; devices: PairedDevice[] }
  | { type: 'cloud_providers'; providers: CloudProviderInfo[] }
  | {
      type: 'transcript';
      text: string;
      raw_text: string;
      language: string | null;
      corrections: TranscriptCorrection[];
      intent: TranscriptIntent;
      engine: string;
      /** Topic of the ongoing dictation detected by the server, when known. */
      topic?: string | null;
    }
  | {
      type: 'speech_options';
      engines: SpeechProfileInfo[];
      llms: SpeechProfileInfo[];
      /** The engine this device uses. */
      current_engine: string | null;
      /** The correction LLM this device uses (`null`: off). */
      current_llm: string | null;
    }
  /** Answer to `synthesize`: `mp3`, `opus` (Ogg), `aac`, `flac` or `wav` bytes. */
  | { type: 'audio'; format: string; data: Uint8Array }
  | { type: 'tts_engines'; engines: TtsEngineInfo[] }
  /** Answer to `briefing`: at most three short sentences. */
  | { type: 'briefing'; text: string }
  /** Answer to `summarize`. `fallback`: the fixed "finished" line, not an LLM summary. */
  | { type: 'summary'; session_id: SessionId; text: string; fallback?: boolean }
  | { type: 'attach_ready'; upload_id: string; chunk_size: number }
  | { type: 'attach_folder_ready'; folder_id: string; max_files: number; max_total_bytes: number }
  | {
      type: 'attached';
      session_id: SessionId;
      /** Relative to the project folder, `/`-separated. */
      path: string;
      size: number;
      /** e.g. `Attached file: @.vca/attachments/…`; empty for files inside a folder. */
      mention: string;
      /** The server typed `mention` into the session (managed sessions). */
      typed: boolean;
    }
  /** The command was typed into the shell (followed by Enter). */
  | { type: 'shell_ran'; session_id: SessionId; danger: DangerLevel }
  /**
   * Nothing was typed: the input has `lines` (two or more) lines. Show all of
   * `command`; only an explicit confirmation resends it as `shell_run` with
   * `confirm`. Answers `shell_run` and `send_input` into a shell.
   */
  | { type: 'shell_confirm'; session_id: SessionId; command: string; lines: number }
  /**
   * Classification of a command line. `spans` are `[start, end)` UTF-8 byte
   * offsets of the risky parts; `reasons` are stable codes.
   */
  | {
      type: 'command_danger';
      level: DangerLevel;
      reasons: string[];
      spans: [number, number][];
      /** A spoken approval may run this. Absent from older servers: no. */
      voice_ok?: boolean;
    }
  /** Audit entries, newest first. */
  | { type: 'audit'; entries: AuditEntry[] }
  /**
   * Pushed to every client when a remote shell opens or closes. `reason`
   * (closed only): `user`, `idle`, `lifetime`, `kill_switch`, `exit` or `shutdown`.
   */
  | {
      type: 'shell_notice';
      session_id: SessionId;
      event: 'opened' | 'closed' | (string & {});
      device_name: string;
      /** `#[serde(default)]`. */
      reason?: string | null;
    }
  | { type: 'restore_list'; overview: RestoreOverview }
  | { type: 'restore_progress'; job: RestoreJob }
  | { type: 'arrange_list'; overview: ArrangeOverview }
  | { type: 'arrange_done'; result: ArrangeResult; overview: ArrangeOverview }
  | ({ type: 'project_folders' } & ProjectFolders)
  | ({ type: 'folder' } & FolderListing)
  | { type: 'folder_path'; path: string; exists: boolean }
  /** Answer to `scan_sessions`, `hide_observed` and `unhide_observed`. */
  | { type: 'scan_report'; found: FoundAgent[]; scanned_at: UnixMillis }
  /** The conductor's state: every conductor answer, and pushed when it changes. */
  | { type: 'conductor'; state: ConductorState }
  /** A conductor reply (pushed), redacted and short. */
  | { type: 'conductor_reply'; text: string; at: UnixMillis }
  /** Task list answers and pushes (see tasks.ts). */
  | TaskServerMsg
  /** Pronunciation dictionary answers and pushes (see pronunciations.ts). */
  | PronunciationServerMsg
  /** Commonly misheard words (see personalTerms.ts). */
  | PersonalTermsServerMsg;

// ------------------------------------------------------------- conductor.rs

/** `welcome.features` entry of servers that take `dismiss_session`. */
export const FEATURE_DISMISS_SESSION = 'dismiss_session';

/** `welcome.features` entry of servers that take `stop` on `dismiss_session`. */
export const FEATURE_DISMISS_STOP = 'dismiss_stop';

/** `welcome.features` entry of servers that take `shell_join`. */
export const FEATURE_SHELL_JOIN = 'shell_join';

/** `welcome.features` entry of servers that answer multi-line shell input with `shell_confirm`. */
export const FEATURE_SHELL_CONFIRM_LINES = 'shell_confirm_lines';

/** `welcome.features` entry of servers with the conductor. */
export const FEATURE_CONDUCTOR = 'conductor';

/**
 * `welcome.features` entry of servers with the Command Center's own pause
 * (`cc_pause`, status `paused`) and the kill switch behind a step-up.
 */
export const FEATURE_COMMAND_CENTER = 'command_center';

export type ConductorAgent = 'claude_code' | 'codex' | 'gemini_cli' | 'builtin';

export interface ConductorSettings {
  enabled: boolean;
  agent: ConductorAgent;
  builtin_llm: string | null;
  wake_events: boolean;
  max_actions_per_hour: number;
  max_wakes_per_hour: number;
  confirm_timeout_secs: number;
  builtin_max_calls_per_hour: number;
  builtin_max_tokens: number;
  /**
   * The Command Center is paused (changed only with `cc_pause`; a saved
   * settings form keeps the server's value). Absent from older servers.
   */
  paused?: boolean;
  /**
   * The person's allow rules for answering other sessions' decisions (set
   * only on the PC). Empty or absent: the conductor only escalates.
   */
  decision_rules?: ConductorDecisionRule[];
}

/** What a {@link ConductorDecisionRule} lets the conductor answer: the plain Yes or No. */
export type ConductorRuleAnswer = 'approve' | 'deny';

export interface ConductorDecisionRule {
  id: string;
  answer: ConductorRuleAnswer;
  /** Command prefixes on whole words; required for `approve`. */
  commands: string[];
  /** Session names or ids; empty: every session. */
  sessions: string[];
  expires_at?: UnixMillis | null;
}

/** `conductor`: raised with the conductor's summary; `fallback`: by VCA itself. */
export type ConductorEscalationSource = 'conductor' | 'fallback';

/**
 * Another session's decision raised to the person's Approvals inbox. It
 * lives as long as that decision (`decision_id`): answered anywhere, it goes.
 */
export interface ConductorEscalation {
  id: string;
  session_id: SessionId;
  session_name: string;
  decision_id: string;
  asked_at: UnixMillis;
  source: ConductorEscalationSource;
  /** The conductor's short summary; empty for a fallback. */
  summary: string;
  recommendation?: string | null;
  /** The original request (the decision's prompt). */
  request: string;
  command?: string | null;
  /**
   * Why a fallback was raised: `timeout`, `stopped`, `paused`, `revoked`,
   * `hooks_missing`, `turn_stalled`, `error` or `not_routed`.
   */
  reason?: string | null;
  created_at: UnixMillis;
}

/** `paused`: on and running, but nothing wakes it (`cc_pause`). */
export type ConductorStatus =
  'off' | 'starting' | 'running' | 'ready' | 'waiting_for_you' | 'revoked' | 'paused';

export interface ConductorRequest {
  id: string;
  session_id: SessionId;
  session_name: string;
  text: string;
  created_at: UnixMillis;
  expires_at: UnixMillis;
  /** Why the person is asked (`not_named`, `risky_command`, ...); absent from older servers. */
  reason?: string | null;
}

export interface ConductorAllow {
  session_id: SessionId;
  expires_at: UnixMillis;
}

export interface ConductorTurn {
  role: 'user' | 'assistant' | 'event' | (string & {});
  text: string;
  at: UnixMillis;
}

export interface ConductorAgentInfo {
  agent: ConductorAgent;
  available: boolean;
  experimental: boolean;
}

export interface ConductorLlm {
  id: string;
  label: string;
  model: string | null;
}

export interface ConductorState {
  settings: ConductorSettings;
  status: ConductorStatus;
  session_id: SessionId | null;
  model: string | null;
  kill_switch: boolean;
  pending: ConductorRequest[];
  allowlist: ConductorAllow[];
  chat: ConductorTurn[];
  actions_last_hour: number;
  wakes_last_hour: number;
  agents: ConductorAgentInfo[];
  llms: ConductorLlm[];
  /**
   * Why the conductor cannot work as it is, when the server knows:
   * {@link CONDUCTOR_PROBLEM_HOOKS_MISSING} (it wins when both apply) or
   * {@link CONDUCTOR_PROBLEM_TURN_STALLED}. Absent or `null` when nothing is
   * known to be wrong, and from older servers.
   */
  problem?: string | null;
  /**
   * Other sessions' decisions raised to the person (by the conductor or the
   * fallback). Empty while the conductor is off; absent from older servers.
   */
  escalations?: ConductorEscalation[];
}

/**
 * {@link ConductorState.problem}: the conductor's session started but its
 * agent never reported a hook event, so its replies cannot be read.
 */
export const CONDUCTOR_PROBLEM_HOOKS_MISSING = 'hooks_missing';

/**
 * {@link ConductorState.problem}: the conductor's current turn has shown no
 * end for 15 minutes (it may be stuck).
 */
export const CONDUCTOR_PROBLEM_TURN_STALLED = 'turn_stalled';
