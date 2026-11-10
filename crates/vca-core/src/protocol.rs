//! Application messages exchanged between VCA Server and paired clients.
//!
//! Messages travel inside the Noise channel as MessagePack-encoded
//! [`Envelope`]s. Requests carry a client-chosen `id`; the server echoes it
//! in the response. Server-initiated messages use `id = 0`.

use serde::{Deserialize, Serialize};

use crate::UnixMillis;
use crate::conductor::{ConductorSettings, ConductorState};
use crate::error::ErrorCode;
use crate::event::{Input, SessionEvent};
use crate::resource::ResourceSnapshot;
use crate::restore::{
    ArrangeEdit, ArrangeMonitorPlan, ArrangeOverview, ArrangeResult, RestoreEdit, RestoreJob,
    RestoreOverview, RestoreRunRequest,
};
use crate::session::{
    CloudLaunchRequest, CloudProvider, LaunchRequest, SessionAction, SessionId, SessionInfo,
    ShellRequest,
};
use crate::task::{
    HumanTask, TaskChange, TaskDraft, TaskExportFormat, TaskReminder, TaskResultDraft, TaskSettings,
};

/// Bumped on incompatible protocol changes.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub id: u64,
    pub body: T,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        protocol_version: u32,
        app_version: String,
    },
    Ping {
        at: UnixMillis,
    },
    ListSessions,
    /// Start streaming events of a session. With `from_seq`, events after
    /// that sequence number are replayed first (when still buffered).
    Subscribe {
        session_id: SessionId,
        from_seq: Option<u64>,
    },
    Unsubscribe {
        session_id: SessionId,
    },
    /// One page of a session's earlier events, newest last. `before_seq`
    /// is the oldest seq the client already has (`None`: the latest page).
    /// Answered with [`ServerMsg::History`].
    History {
        session_id: SessionId,
        before_seq: Option<u64>,
        /// Wanted events; the server may return fewer.
        limit: u32,
    },
    SendInput {
        session_id: SessionId,
        input: Input,
    },
    Control {
        session_id: SessionId,
        action: SessionAction,
    },
    Launch {
        request: LaunchRequest,
    },
    /// Answered with [`ServerMsg::Launched`].
    LaunchCloud {
        request: CloudLaunchRequest,
    },
    GetResources,
    /// Move a queued job to a new position (0 = next).
    ReorderQueue {
        job_id: String,
        position: u32,
    },
    CancelQueued {
        job_id: String,
    },
    MarkRead {
        session_id: SessionId,
    },
    /// Sets the name shown on a session card. `None` or a blank name goes
    /// back to the automatic name. Answered with [`ServerMsg::Ok`]; the new
    /// name reaches every client as [`ServerMsg::SessionUpdated`].
    RenameSession {
        session_id: SessionId,
        #[serde(default)]
        name: Option<String>,
    },
    /// Removes an ended session's card right away instead of after the
    /// server's retention time, releasing its buffers. Only for sessions in
    /// the `exited` state: a running one answers `conflict`. Command scope.
    /// Answered with [`ServerMsg::Ok`]; every client gets
    /// [`ServerMsg::SessionRemoved`]. Only sent when the server's `Welcome`
    /// lists [`FEATURE_DISMISS_SESSION`].
    ///
    /// `stop` ("Remove from VCA"): a running session VCA started is stopped
    /// first, and removed once it has exited, all on the server (it
    /// completes even if the asking client goes away). Sessions VCA did not
    /// start (a terminal window on the PC, a cloud session) answer
    /// `unsupported`, the conductor's own session `conflict`. Only sent when
    /// the server's `Welcome` lists [`FEATURE_DISMISS_STOP`]; older servers
    /// ignore the field and refuse a running session as before.
    DismissSession {
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "is_false")]
        stop: bool,
    },
    // --- Session card order. Only sent when the server's `Welcome` lists
    // [`FEATURE_SESSION_ORDER`]. ---
    /// The card order the user chose on this computer. View scope.
    /// Answered with [`ServerMsg::SessionOrder`].
    GetSessionOrder,
    /// Puts the cards in this order (first card first). Sessions not listed
    /// keep their place after the listed ones; unknown ids are ignored. The
    /// server keeps it by stable keys (folder, agent, name, conversation),
    /// so it outlives restarts and window restores. Command scope. Answered
    /// with [`ServerMsg::SessionOrder`], which is also pushed to every
    /// client.
    SetSessionOrder {
        session_ids: Vec<SessionId>,
    },
    /// Back to the default order (oldest first). Command scope. Answered
    /// and pushed like [`ClientMsg::SetSessionOrder`].
    ResetSessionOrder,
    ListDevices,
    RevokeDevice {
        device_id: String,
    },
    // --- Paired-device management (Admin scope). Answered with the updated
    // [`ServerMsg::Devices`] list. ---
    /// Renames a paired device: trimmed, 1-64 characters, no control
    /// characters.
    RenameDevice {
        device_id: String,
        name: String,
    },
    /// Changes a device's permission. A device cannot change its own scope;
    /// the change applies to its live connections from the next request on.
    SetDeviceScope {
        device_id: String,
        scope: DeviceScope,
    },
    /// Drop every remote connection and close the tunnel (SEC-09).
    KillSwitch,
    // --- Approval alerts (push). The phone is always the authenticated
    // device of the connection, never an id from the message. ---
    /// A paired phone asks for approval alerts: its APNs or FCM token and
    /// the public key payloads are sealed to. Re-sending replaces the
    /// phone's registration. Paired devices only; answered with
    /// [`ServerMsg::Ok`].
    PushRegister {
        /// The machine id the phone knows this computer by.
        machine: String,
        platform: PushPlatform,
        token: PushToken,
        /// X25519 public key, base64url.
        e2e_pubkey: String,
        /// Language of the fixed notification text.
        #[serde(default)]
        locale: Option<String>,
    },
    /// The phone stops approval alerts. Answered with [`ServerMsg::Ok`].
    PushUnregister,
    /// The phones registered for alerts, tokens included, for the desktop
    /// app of this computer (the only sender). Local clients only; answered
    /// with [`ServerMsg::PushPhones`].
    ListPushPhones,
    /// Cloud providers the server can launch and follow. Answered with
    /// [`ServerMsg::CloudProviders`].
    ListCloudProviders,
    /// Recognize one utterance with the server's speech pipeline (VOC-01).
    /// Answered with [`ServerMsg::Transcript`]. The transcript is only
    /// shown for review; nothing is sent to a session (VOC-03).
    Transcribe {
        /// Encoded audio (WebM/Opus, Ogg, M4A, WAV) or raw 16-bit PCM.
        #[serde(with = "crate::event::serde_bytes_compat")]
        audio: Vec<u8>,
        format: AudioEncoding,
        /// Sample rate; required for `pcm16`, informational otherwise.
        sample_rate: Option<u32>,
        /// BCP-47 primary language; `None` uses the server's setting.
        language: Option<String>,
        /// The selected session, for project vocabulary and intent routing.
        session_id: Option<SessionId>,
        /// Text dictated just before this audio (live dictation sends audio
        /// in chunks), so the recognizer continues it consistently.
        #[serde(default)]
        context: Option<String>,
        /// True for a FINAL chunk of live dictation (or a whole utterance);
        /// false for an interim preview that will be transcribed again. Only
        /// final chunks get the server's contextual (LLM) correction and
        /// update the dictation topic. Sent as `final`.
        #[serde(default, rename = "final")]
        is_final: bool,
    },
    /// The speech engine and transcript-correction LLM profiles the server
    /// offers, and this device's choice. Answered with
    /// [`ServerMsg::SpeechOptions`]. Never carries secrets.
    ListSpeechOptions,
    /// Chooses this device's speech engine and correction LLM by profile
    /// id. `None` follows the server default; `llm: Some("none")` turns the
    /// correction off. Answered with [`ServerMsg::SpeechOptions`].
    SetSpeechPreferences {
        engine: Option<String>,
        llm: Option<String>,
    },
    /// Speaks a short alert title with a TTS engine configured on the PC
    /// (`[[tts.engines]]`). At most [`TTS_MAX_TEXT_CHARS`] characters; the
    /// server synthesizes only this text and logs neither it nor the
    /// audio. Answered with [`ServerMsg::Audio`].
    Synthesize {
        text: String,
        /// BCP-47 language, which picks the engine's voice for it.
        #[serde(default)]
        language: Option<String>,
        /// Engine name; the server default when `None`.
        #[serde(default)]
        engine: Option<String>,
        /// Speaker id from the engine's [`TtsEngineInfo::speakers`]; the
        /// engine's configured voice when `None`. Anything else is refused.
        #[serde(default)]
        voice: Option<String>,
    },
    /// The TTS engines the server offers (names and languages only).
    /// Answered with [`ServerMsg::TtsEngines`].
    ListTtsEngines,
    /// A short spoken status of this computer's sessions (counts by state,
    /// the session waiting longest, errors), built from session state with
    /// fixed templates: no LLM. Answered with [`ServerMsg::Briefing`].
    Briefing {
        /// BCP-47 language of the listener (`en`, `ko`, `ja`, `es`; others
        /// get English). `None`: English.
        #[serde(default)]
        language: Option<String>,
        /// The client's card order, so session numbers match what it shows
        /// (1-based). Sessions it does not list are numbered after them.
        #[serde(default)]
        order: Vec<SessionId>,
    },
    /// This device's opt-in to spoken result summaries. Off until sent;
    /// `Summarize` is refused without it. Answered with [`ServerMsg::Ok`].
    SetVoiceSummary {
        enabled: bool,
        /// Commands, paths and code may be read out.
        #[serde(default)]
        details: bool,
    },
    /// A 1-2 sentence summary of a session's last assistant message, in the
    /// listener's language, with the server's LLM (`[[speech.llms]]`), else
    /// a fixed "session N finished" line. Needs the `SetVoiceSummary`
    /// opt-in; secrets are redacted before the LLM sees the text. Answered
    /// with [`ServerMsg::Summary`].
    Summarize {
        session_id: SessionId,
        #[serde(default)]
        language: Option<String>,
        /// The session's card number on the client, for the spoken prefix.
        #[serde(default)]
        number: Option<u32>,
    },
    /// Send one small file to a local session in a single message (up to
    /// [`ATTACH_CHUNK_BYTES`] of data). Answered with
    /// [`ServerMsg::Attached`]. Larger files use the chunked
    /// `AttachBegin` / `AttachChunk` / `AttachEnd` sequence.
    Attach {
        session_id: SessionId,
        /// The file name on the client; the server sanitizes it.
        name: String,
        /// MIME type reported by the client (may be empty).
        mime: String,
        #[serde(with = "crate::event::serde_bytes_compat")]
        data: Vec<u8>,
        /// Text added after the mention typed into the session.
        #[serde(default)]
        note: Option<String>,
        /// Press Enter after typing the mention (default: leave it for the
        /// user to complete).
        #[serde(default)]
        submit: bool,
    },
    /// Start a chunked upload of `size` bytes. Answered with
    /// [`ServerMsg::AttachReady`]; the client then sends the data in order
    /// with `AttachChunk` and finishes with `AttachEnd`.
    AttachBegin {
        session_id: SessionId,
        name: String,
        mime: String,
        size: u64,
        #[serde(default)]
        note: Option<String>,
        #[serde(default)]
        submit: bool,
        /// Set for the files of a folder upload (`AttachFolderBegin`).
        #[serde(default)]
        folder: Option<AttachFolderEntry>,
        /// Batch id chosen by the client for several files sent together
        /// (a multi-photo pick). Files of a batch are saved into one new
        /// folder and are not mentioned one by one: `AttachBatchEnd` types
        /// one compact mention for all of them. Ignored with `folder`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batch: Option<String>,
    },
    /// The next bytes of an upload. `offset` must equal the bytes already
    /// received. Answered with [`ServerMsg::Ok`].
    AttachChunk {
        upload_id: String,
        offset: u64,
        #[serde(with = "crate::event::serde_bytes_compat")]
        data: Vec<u8>,
    },
    /// Completes an upload. Answered with [`ServerMsg::Attached`].
    AttachEnd {
        upload_id: String,
    },
    /// Abandons an upload or a folder upload (by its id) and removes what
    /// was written. Answered with [`ServerMsg::Ok`].
    AttachCancel {
        upload_id: String,
    },
    /// Start uploading a folder of `files` files and `total_bytes` bytes.
    /// Answered with [`ServerMsg::AttachFolderReady`]. Each file is then
    /// sent with `AttachBegin { folder: Some(..) }`, and `AttachFolderEnd`
    /// tells the agent about the folder once.
    AttachFolderBegin {
        session_id: SessionId,
        /// The folder name on the client; the server sanitizes it.
        name: String,
        files: u32,
        total_bytes: u64,
        #[serde(default)]
        note: Option<String>,
        #[serde(default)]
        submit: bool,
    },
    /// Completes a folder upload. Answered with [`ServerMsg::Attached`]
    /// for the folder.
    AttachFolderEnd {
        folder_id: String,
    },
    /// Completes a batch (`AttachBegin { batch: Some(..) }`): one mention of
    /// every file of the batch that was uploaded is typed into the session
    /// (or returned for the client to show). Answered with
    /// [`ServerMsg::Attached`], whose `path` is the batch folder.
    AttachBatchEnd {
        batch_id: String,
        /// Text added after the mention.
        #[serde(default)]
        note: Option<String>,
        /// Press Enter after the mention.
        #[serde(default)]
        submit: bool,
    },
    // --- Remote shells (Admin scope, except `ClassifyCommand`). ---
    /// Opens a plain shell on this computer. Needs a fresh TOTP code, which
    /// also starts the device's step-up window. Answered with
    /// [`ServerMsg::Launched`]; the session's agent is
    /// [`crate::AgentKind::Shell`].
    OpenShell {
        request: ShellRequest,
    },
    /// Types `command` followed by Enter into a shell session. A
    /// `dangerous` command needs the device to be inside its step-up window,
    /// else the server answers `totp_required` and the client resends with
    /// `totp`. Answered with [`ServerMsg::ShellRan`].
    ///
    /// A `command` of two or more lines (one trailing line break does not
    /// count) is not typed until the person saw all of it: without
    /// `confirm` the server answers [`ServerMsg::ShellConfirm`] with the
    /// full text, and the client resends with `confirm` after an explicit
    /// confirmation. Every line is still classified; the most dangerous
    /// one decides about the step-up. Multi-line commands only from servers
    /// whose `Welcome` lists [`FEATURE_SHELL_CONFIRM_LINES`].
    ShellRun {
        session_id: SessionId,
        command: String,
        #[serde(default)]
        totp: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        confirm: bool,
    },
    /// Lets this device watch and type into a shell another device opened.
    /// Needs a fresh TOTP code (always, even inside the step-up window);
    /// the grant lasts for this shell and this device until the shell
    /// closes or the owner turns remote shells off. The device that opened
    /// the shell never needs it. Until then the other requests for that
    /// shell are answered `totp_required` with a message that mentions
    /// joining. Answered with [`ServerMsg::Ok`]. Only sent when the
    /// server's `Welcome` lists [`FEATURE_SHELL_JOIN`].
    ShellJoin {
        session_id: SessionId,
        totp: String,
    },
    /// Keeps an open shell open: counts as activity, so the idle deadline
    /// moves forward by the idle timeout, never past the shell's absolute
    /// lifetime (`ShellInfo::hard_deadline`). The answer to a
    /// [`ServerMsg::ShellClosingSoon`]. Answered with
    /// [`ServerMsg::Ok`]; the new deadline reaches every client as
    /// [`ServerMsg::SessionUpdated`] and a `kept_open` notice. Only sent when
    /// the server's `Welcome` lists [`FEATURE_SHELL_KEEP_ALIVE`].
    ShellKeepAlive {
        session_id: SessionId,
    },
    /// Classifies a command line without running it. Answered with
    /// [`ServerMsg::CommandDanger`].
    ClassifyCommand {
        command: String,
    },
    /// The newest entries of the remote shell audit log. `0` means the
    /// default (100); at most 1000. Answered with [`ServerMsg::Audit`].
    ListAudit {
        #[serde(default)]
        limit: u32,
    },
    // --- Session restore. ---
    /// The managed window list of this computer. Answered with
    /// [`ServerMsg::RestoreList`].
    RestoreList,
    /// Measures the open AI CLI windows into the profile (the active one by
    /// default), as the timed snapshot does. Admin scope. Answered with
    /// [`ServerMsg::RestoreList`].
    RestoreCapture {
        #[serde(default)]
        profile: Option<String>,
    },
    /// Changes the list. Admin scope. Answered with
    /// [`ServerMsg::RestoreList`].
    RestoreEdit {
        edit: RestoreEdit,
    },
    /// Imports a `layout.json` (its text). Admin scope. Answered with
    /// [`ServerMsg::RestoreList`].
    RestoreImport {
        layout: String,
        #[serde(default)]
        profile: Option<String>,
    },
    /// Reopens windows. Admin scope; a remote run also needs the step-up
    /// window or a fresh TOTP code (`totp_required` otherwise). Only folders
    /// inside the workspace roots open remotely. Answered with
    /// [`ServerMsg::RestoreProgress`]; the same message is pushed to every
    /// client as the run advances.
    RestoreRun {
        request: RestoreRunRequest,
    },
    /// Brings the terminal window of a session to the front, after the
    /// person clicked for it. Local clients only (the desktop app of the
    /// same computer); Windows only. Windows may refuse to hand over the
    /// focus: then the answer is `unavailable`. Answered with
    /// [`ServerMsg::Ok`].
    FocusTerminal {
        session_id: SessionId,
    },
    // --- Screen layouts: arrange open terminal windows in a grid. Only when
    // the server's `Welcome` lists [`FEATURE_WINDOW_ARRANGE`]. ---
    /// The monitors, the open terminal windows and the saved layouts.
    /// Answered with [`ServerMsg::ArrangeList`].
    ArrangeList,
    /// Puts open terminal windows in grid cells. The server looks every
    /// window up in the live list and works out the rectangles; a window it
    /// does not list is reported as skipped. Admin scope. Answered with
    /// [`ServerMsg::ArrangeDone`].
    ///
    /// Moving windows from a paired device needs the step-up window or a
    /// fresh TOTP code in `totp`, like [`ClientMsg::RestoreRun`]; without
    /// one the answer is `totp_required` and nothing moves. The owner's
    /// local clients need none. The same holds for `ArrangeApplyLayout`
    /// and `ArrangeUndo`.
    ArrangeApply {
        monitors: Vec<ArrangeMonitorPlan>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        totp: Option<String>,
    },
    /// Applies a saved layout to the windows open now. Admin scope; a code
    /// from a paired device (see `ArrangeApply`). Answered with
    /// [`ServerMsg::ArrangeDone`].
    ArrangeApplyLayout {
        layout: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        totp: Option<String>,
    },
    /// Puts the windows of the last arrangement back where they were. Admin
    /// scope; a code from a paired device (see `ArrangeApply`). Answered
    /// with [`ServerMsg::ArrangeDone`]; `conflict` when there is nothing to
    /// undo.
    ArrangeUndo {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        totp: Option<String>,
    },
    /// Saves, renames or deletes a layout. Admin scope. Answered with
    /// [`ServerMsg::ArrangeList`].
    ArrangeEdit {
        edit: ArrangeEdit,
    },
    // --- Project folder picker (Command scope, like `Launch`). Only sent
    // when the server's `Welcome` lists [`FEATURE_PROJECT_FOLDERS`]. ---
    /// The workspace roots and the recent project folders. Answered with
    /// [`ServerMsg::ProjectFolders`].
    ProjectFolders,
    /// The sub-folders of a folder inside the workspace roots (never
    /// files). Answered with [`ServerMsg::Folder`]; `forbidden` outside the
    /// roots.
    ListFolder {
        path: String,
        /// Also list hidden folders (dot folders, `node_modules`, `target`,
        /// hidden or system folders on Windows).
        #[serde(default)]
        show_hidden: bool,
    },
    /// Where a new folder `name` inside `parent` would be, checked against
    /// the workspace roots without creating anything, so clients never
    /// join paths themselves. Answered with [`ServerMsg::FolderPath`].
    FolderPath {
        parent: String,
        name: String,
    },
    // --- Session discovery. Only sent when the server's `Welcome` lists
    // [`FEATURE_SCAN_SESSIONS`]. Discovery is read-only: none of these
    // touches a process. ---
    /// Looks for AI agent sessions running on the computer now (at most one
    /// pass every few seconds; a request within that time gets the last
    /// result). View scope. Answered with [`ServerMsg::ScanReport`].
    ScanSessions,
    /// Takes an observed session off the list (and keeps it off while its
    /// process runs, across server restarts). Give the session or the
    /// process id of a [`ServerMsg::ScanReport`] row. Command scope.
    /// Answered with [`ServerMsg::ScanReport`].
    HideObserved {
        #[serde(default)]
        session_id: Option<SessionId>,
        #[serde(default)]
        pid: Option<u32>,
    },
    /// Lists a hidden session again (`all`: every hidden one). Command
    /// scope. Answered with [`ServerMsg::ScanReport`] after a new pass.
    UnhideObserved {
        #[serde(default)]
        session_id: Option<SessionId>,
        #[serde(default)]
        pid: Option<u32>,
        #[serde(default)]
        all: bool,
    },
    // --- The conductor (see [`crate::conductor`]). ---
    /// The conductor's state. View scope. Answered with
    /// [`ServerMsg::Conductor`]; changes are pushed the same way.
    ConductorGet,
    /// Changes the conductor settings (the opt-in included). The desktop
    /// app of this PC only: paired devices get `forbidden`. Answered with
    /// [`ServerMsg::Conductor`].
    ConductorConfigure {
        settings: ConductorSettings,
    },
    /// Starts the conductor ("지휘 세션 시작"). This PC only. Answered with
    /// [`ServerMsg::Conductor`].
    ConductorStart,
    /// Stops the conductor; with `kill`, also revokes its tool access, denies
    /// every pending request and clears the allowlist until it is started
    /// again. Command scope (stopping only reduces what it can do). Answered
    /// with [`ServerMsg::Conductor`].
    ///
    /// The kill switch (`kill`) needs a step-up: on this PC's local channel
    /// a local verification (step-up token), from a paired device the
    /// owner's authenticator code in `code` (refused with `totp_required`
    /// without one, `totp_invalid` for a wrong one). A plain stop needs
    /// neither.
    ConductorStop {
        #[serde(default)]
        kill: bool,
        /// TOTP (or backup) code for the kill switch from a paired device.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    /// Pauses (`paused: true`) or resumes the Command Center: while paused
    /// the conductor gets no wake-ups, but its session keeps running.
    /// Command scope: pausing is reversible and grants nothing. Answered
    /// with [`ServerMsg::Conductor`]. Announced with
    /// [`FEATURE_COMMAND_CENTER`].
    CcPause {
        paused: bool,
    },
    /// The person's answer to a [`crate::ConductorRequest`]. Command scope.
    /// Answered with [`ServerMsg::Conductor`].
    ConductorDecide {
        request_id: String,
        allow: bool,
    },
    /// Lets sends to `session_id` through without confirmation for
    /// `minutes` (`0` removes the entry). This PC only. Answered with
    /// [`ServerMsg::Conductor`].
    ConductorAllowSession {
        session_id: SessionId,
        minutes: u32,
    },
    /// An instruction for the conductor, typed into its session (CLI
    /// agents) or given to the built-in loop. Command scope. Answered with
    /// [`ServerMsg::Conductor`]; the reply arrives as
    /// [`ServerMsg::ConductorReply`].
    ConductorAsk {
        text: String,
    },
    /// Answers "yes" to the first-run "trust this folder" question on the
    /// conductor session's terminal ([`crate::TerminalPrompt::TrustFolder`])
    /// by pressing Enter, only while that question is on screen with the
    /// cursor on its "Yes" item. Command scope (the same as pressing Enter
    /// in the terminal). Answered with [`ServerMsg::Conductor`].
    ConductorTrustFolder,
    // --- The human task inbox (see [`crate::task`]). Only sent when the
    // server's `Welcome` lists [`crate::task::FEATURE_HUMAN_TASKS`]. ---
    /// Every task and the task settings. View scope. Answered with
    /// [`ServerMsg::Tasks`], which is also pushed (`id = 0`) on changes.
    TaskList,
    /// A manual task ("새 할 일", "할 일로 추가"). Command scope. Answered
    /// with [`ServerMsg::TaskSaved`].
    TaskCreate {
        draft: TaskDraft,
    },
    /// Checks off, snoozes, edits or moves a task. Command scope. Answered
    /// with [`ServerMsg::TaskSaved`].
    TaskUpdate {
        task_id: String,
        change: TaskChange,
    },
    /// "결과 보내기": types the result into the requesting session like a
    /// message and marks the task done; with `preview`, only composes it.
    /// Command scope. Answered with [`ServerMsg::TaskResultSent`].
    TaskSendResult {
        task_id: String,
        result: TaskResultDraft,
        #[serde(default)]
        preview: bool,
    },
    /// Opens an administrator PowerShell on this PC through the normal UAC
    /// prompt, showing the task's command (never running it). The desktop
    /// app of this PC only. Answered with [`ServerMsg::Ok`].
    TaskOpenAdminShell {
        task_id: String,
    },
    /// Markdown or CSV of the tasks (of one session). View scope. Answered
    /// with [`ServerMsg::TaskExported`].
    TaskExport {
        format: TaskExportFormat,
        #[serde(default)]
        session_id: Option<SessionId>,
    },
    /// A short spoken "today's tasks" line (counts only, fixed templates).
    /// View scope. Answered with [`ServerMsg::Briefing`].
    TaskDigest {
        #[serde(default)]
        language: Option<String>,
    },
    /// Changes the task settings (the LLM opt-in included). This PC only.
    /// Answered with [`ServerMsg::Tasks`].
    TaskConfigure {
        settings: TaskSettings,
    },
    // --- Pronunciation dictionary of spoken alerts. Only sent when the
    // server's `Welcome` lists [`crate::pronunciation::FEATURE_PRONUNCIATIONS`]. ---
    /// The entries (shipped, the user's and suggested ones), of one
    /// language or (`None`) all. View scope. Answered with
    /// [`ServerMsg::Pronunciations`].
    ListPronunciations {
        #[serde(default)]
        language: Option<String>,
    },
    /// Adds or changes an entry: `term` is said `spoken` by a voice of
    /// `language` (default `ko`). Command scope. Answered with
    /// [`ServerMsg::Pronunciations`] (all languages), which is also pushed
    /// to every client.
    SetPronunciation {
        term: String,
        spoken: String,
        #[serde(default)]
        language: Option<String>,
    },
    /// Deletes an entry (a shipped one stays off). Command scope. Answered
    /// like [`ClientMsg::SetPronunciation`].
    DeletePronunciation {
        term: String,
        #[serde(default)]
        language: Option<String>,
    },
    // --- Commonly misheard words of server-side speech. Only sent when the
    // server's `Welcome` lists
    // [`crate::personal_terms::FEATURE_PERSONAL_TERMS`]. ---
    /// The list. View scope. Answered with [`ServerMsg::PersonalTerms`].
    ListPersonalTerms,
    /// Replaces the whole list; it applies to the next dictation without a
    /// restart. Admin scope, audited. Answered with
    /// [`ServerMsg::PersonalTerms`], or `invalid_request` when the list
    /// breaks the rules in `vca_core::personal_terms`.
    SavePersonalTerms {
        terms: Vec<crate::personal_terms::PersonalTerm>,
    },
}

/// `Welcome::features` entry of servers that answer
/// [`ClientMsg::ProjectFolders`], [`ClientMsg::ListFolder`] and
/// [`ClientMsg::FolderPath`].
pub const FEATURE_PROJECT_FOLDERS: &str = "project_folders";
/// `Welcome::features`: the Command Center (Phase A): [`ClientMsg::CcPause`],
/// `ConductorStatus::Paused`, `ConductorState::problem`, and the kill
/// switch behind a step-up.
pub const FEATURE_COMMAND_CENTER: &str = "command_center";

/// `Welcome::features` entry of servers that answer
/// [`ClientMsg::ScanSessions`], [`ClientMsg::HideObserved`] and
/// [`ClientMsg::UnhideObserved`].
pub const FEATURE_SCAN_SESSIONS: &str = "scan_sessions";

/// `Welcome::features` entry of servers that answer
/// [`ClientMsg::GetSessionOrder`], [`ClientMsg::SetSessionOrder`] and
/// [`ClientMsg::ResetSessionOrder`].
pub const FEATURE_SESSION_ORDER: &str = "session_order";

/// `Welcome::features` entry of servers that answer the `Arrange*` messages.
pub const FEATURE_WINDOW_ARRANGE: &str = "window_arrange";

/// `Welcome::features` entry of servers that answer
/// [`ClientMsg::DismissSession`].
pub const FEATURE_DISMISS_SESSION: &str = "dismiss_session";

/// `Welcome::features` entry of servers that take `stop` on
/// [`ClientMsg::DismissSession`] (stop a running session VCA started, then
/// remove it).
pub const FEATURE_DISMISS_STOP: &str = "dismiss_stop";

/// `Welcome::features` entry of servers that send
/// [`ServerMsg::ShellClosingSoon`] and answer [`ClientMsg::ShellKeepAlive`].
pub const FEATURE_SHELL_KEEP_ALIVE: &str = "shell_keep_alive";

/// `Welcome::features` entry of servers that answer [`ClientMsg::ShellJoin`]
/// and keep a shell to its opener until another admin device joins it.
pub const FEATURE_SHELL_JOIN: &str = "shell_join";

/// `Welcome::features` entry of servers that accept multi-line
/// [`ClientMsg::ShellRun`] commands after a [`ServerMsg::ShellConfirm`].
pub const FEATURE_SHELL_CONFIRM_LINES: &str = "shell_confirm_lines";

fn is_false(b: &bool) -> bool {
    !*b
}

/// Why a found agent process is (or is not) on the session list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FoundStatus {
    /// Listed as an observed session (`session_id` is set).
    Imported,
    /// The person hid it from the list.
    HiddenByUser,
    /// Listed (`session_id` is set), but the agent has not written its
    /// conversation file yet, so the chat stays empty until the first
    /// message.
    NoTranscriptYet,
    /// The agent is recognized, but it is not a terminal program that can
    /// be followed (for example an IDE).
    UnsupportedAgent,
    /// The agent runs inside a desktop app without a terminal window (for
    /// example Codex inside the ChatGPT app).
    NotInATerminal,
}

/// One AI agent process in a [`ServerMsg::ScanReport`]. Plain shells and
/// other programs never appear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundAgent {
    pub pid: u32,
    pub agent: crate::session::AgentKind,
    #[serde(default)]
    pub project_dir: Option<String>,
    /// The terminal or app that hosts it (for example `Windows Terminal`,
    /// `tmux` or `ChatGPT`).
    #[serde(default)]
    pub terminal: Option<String>,
    /// The session name or the agent's own conversation title.
    #[serde(default)]
    pub title: Option<String>,
    pub status: FoundStatus,
    /// The observed session, when listed.
    #[serde(default)]
    pub session_id: Option<SessionId>,
}

/// Operating system family of the server, so clients show its paths right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostOs {
    Windows,
    Macos,
    /// Linux and other Unix systems.
    Linux,
}

impl HostOs {
    /// The family this binary runs on.
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }
}

/// A folder in [`ServerMsg::ProjectFolders`] or [`ServerMsg::Folder`].
/// `path` is absolute in the server's own form; clients send it back as is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderEntry {
    pub name: String,
    pub path: String,
    /// The folder has a `.git` entry.
    #[serde(default)]
    pub is_git_repo: bool,
    /// An agent (Claude Code, Codex, a session of this server) worked here.
    #[serde(default)]
    pub has_agent_history: bool,
    /// Last agent activity in the folder (recent projects only).
    #[serde(default)]
    pub last_used: Option<UnixMillis>,
}

/// One step of [`ServerMsg::Folder::breadcrumbs`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderCrumb {
    pub name: String,
    pub path: String,
}

/// How risky a shell command is, as classified by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DangerLevel {
    Safe,
    Caution,
    Dangerous,
}

/// Push service of a phone's device token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushPlatform {
    Ios,
    Android,
}

/// An APNs or FCM device token. `Debug` hides it; only the wire carries it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PushToken(pub String);

impl PushToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PushToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PushToken([REDACTED])")
    }
}

/// A phone registered for approval alerts (see [`ClientMsg::PushRegister`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushPhone {
    /// The paired device id of the phone.
    pub phone: String,
    /// The machine id the phone knows this computer by.
    pub machine: String,
    pub platform: PushPlatform,
    pub token: PushToken,
    pub e2e_pubkey: String,
    #[serde(default)]
    pub locale: Option<String>,
}

/// One line of the remote shell audit log (`audit.jsonl`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Unix milliseconds.
    pub ts: UnixMillis,
    pub device_id: String,
    pub device_name: String,
    /// `shell_open`, `shell_close`, `shell_run`, `step_up_ok`,
    /// `step_up_failed`, `restore_run`, `restore_edit`, `window_arrange` or
    /// `personal_terms_save`.
    pub event: String,
    #[serde(default)]
    pub session_id: Option<SessionId>,
    /// Never contains TOTP codes; command text is redacted.
    pub detail: String,
}

/// Largest data per `Attach` / `AttachChunk` message the server accepts.
/// Well below the transport's 16 MiB message limit, so uploads never need
/// a larger limit and progress can be reported per chunk.
pub const ATTACH_CHUNK_BYTES: usize = 1024 * 1024;

/// The place of one file inside a folder upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachFolderEntry {
    pub folder_id: String,
    /// Path relative to the folder, with `/` separators.
    pub path: String,
}

/// The `llm` preference value that turns the correction off.
pub const SPEECH_LLM_NONE: &str = "none";

/// A speech engine or correction LLM profile (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeechProfileInfo {
    pub id: String,
    pub label: String,
    /// `openai_compatible`, `openai`, `google` (engines) or
    /// `openai_compatible`, `anthropic` (LLMs).
    pub kind: String,
    /// A credential is available (or none is needed). Unconfigured profiles
    /// can be chosen only after `vca speech login <id>` on the server.
    pub configured: bool,
    /// The server default.
    pub default: bool,
}

/// Longest text a [`ClientMsg::Synthesize`] request may carry, in characters.
pub const TTS_MAX_TEXT_CHARS: usize = 300;

/// Longest LLM summary in a [`ServerMsg::Summary`], in characters (the
/// session prefix comes on top, within [`TTS_MAX_TEXT_CHARS`]).
pub const SUMMARY_MAX_CHARS: usize = 200;

/// A TTS engine of the server: never its URL, model or key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TtsEngineInfo {
    pub name: String,
    /// Languages with a configured voice (BCP-47); empty means the engine's
    /// default voice is used for every language.
    pub languages: Vec<String>,
    /// The server default.
    pub default: bool,
    /// Speakers a client may choose; empty means the configured voice only.
    #[serde(default)]
    pub speakers: Vec<TtsSpeaker>,
    /// Text a client should show where it offers this engine (for engines
    /// whose licence requires telling people the audio is machine-made).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disclosure: Option<String>,
}

/// Gender of a [`TtsSpeaker`], used by clients to group speakers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsGender {
    Female,
    Male,
    #[default]
    Other,
}

/// A speaker (voice) a TTS engine offers: an id to send in
/// [`ClientMsg::Synthesize`], a label and a gender. Never a URL or a key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TtsSpeaker {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub gender: TtsGender,
}

/// Audio container of a [`ClientMsg::Transcribe`] request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioEncoding {
    Wav,
    /// Raw little-endian signed 16-bit mono PCM.
    Pcm16,
    Webm,
    Ogg,
    M4a,
}

/// A term the developer vocabulary engine replaced (VOC-05).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptCorrection {
    /// What the recognizer heard.
    pub from: String,
    /// The vocabulary term that replaced it (appears in the text).
    pub to: String,
    /// `base`, `project` or `personal` (vocabulary layers), or `context`
    /// (the optional LLM-based contextual correction).
    pub layer: String,
}

/// How the server routed a transcript (VOC-06). The client confirms
/// control commands and ambiguous results before acting on them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptIntent {
    /// Text meant for the agent.
    Prompt,
    /// A command for VCA. `action` is the router's action name (`pause`,
    /// `stop`, `approve`, ...); `session_action` is set for lifecycle
    /// commands. `session_id: None` means the selected session.
    Control {
        action: String,
        session_action: Option<SessionAction>,
        session_id: Option<SessionId>,
        confidence: f32,
    },
    /// Could be a command or a prompt; ask the user.
    Ambiguous,
}

/// A cloud provider as the server offers it (CLD-07): the client hides
/// what `capabilities` does not allow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudProviderInfo {
    pub provider: CloudProvider,
    /// Account label from the server configuration.
    pub account: String,
    /// A credential is available for the account.
    pub configured: bool,
    pub create: bool,
    pub follow_up: bool,
    pub stop: bool,
    pub approve_plan: bool,
    /// Progress is limited to coarse status (no conversation).
    pub status_only: bool,
    /// Endpoint shapes could not be verified against public docs.
    pub unverified: bool,
}

/// A device paired with the server (SEC-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub paired_at: UnixMillis,
    pub last_seen: UnixMillis,
    /// True for the device that sent the request.
    pub current: bool,
    // Added later; all default so older peers stay compatible.
    /// Permission scope; `None` from servers that do not report it.
    #[serde(default)]
    pub scope: Option<DeviceScope>,
    /// True while at least one remote connection of the device is open.
    #[serde(default)]
    pub online: bool,
    /// Number of live remote connections.
    #[serde(default)]
    pub connections: u32,
    /// Remote address of the most recent connection (shown to the owner).
    #[serde(default)]
    pub last_ip: Option<String>,
    /// App version the device reported in `hello` since the server started.
    #[serde(default)]
    pub app_version: Option<String>,
}

/// Permission scope of a paired device as it travels in the protocol
/// (`vca_net::Scope` converts to and from it). `View < Command < Admin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceScope {
    View,
    Command,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Queued,
    Delivered,
    /// The agent visibly reacted to the input.
    Acknowledged,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome {
        protocol_version: u32,
        server_version: String,
        server_name: String,
        /// Optional requests this server answers (for example
        /// [`FEATURE_PROJECT_FOLDERS`]); empty from older servers.
        #[serde(default)]
        features: Vec<String>,
    },
    Pong {
        at: UnixMillis,
    },
    Ok,
    Error {
        code: ErrorCode,
        message: String,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    SessionUpdated {
        session: SessionInfo,
    },
    SessionRemoved {
        session_id: SessionId,
    },
    /// The card order: every listed session, first card first (the order
    /// of [`ServerMsg::Sessions`] too). The answer to the session order
    /// requests, and pushed (`id = 0`) when it changes or a new session
    /// takes a stored place.
    SessionOrder {
        session_ids: Vec<SessionId>,
        /// The user chose the order (else it is the default order).
        custom: bool,
    },
    Event {
        session_id: SessionId,
        event: SessionEvent,
    },
    /// Sent after a resubscribe when requested events are no longer buffered;
    /// the client should reload the session view.
    StreamGap {
        session_id: SessionId,
        first_available_seq: u64,
    },
    /// Reply to [`ClientMsg::History`].
    History {
        session_id: SessionId,
        /// Ascending by seq.
        events: Vec<SessionEvent>,
        /// Older events than the first one returned are still available.
        has_more: bool,
    },
    InputAck {
        session_id: SessionId,
        status: DeliveryStatus,
    },
    Launched {
        session: SessionInfo,
    },
    Resources {
        snapshot: ResourceSnapshot,
    },
    Devices {
        devices: Vec<PairedDevice>,
    },
    /// Answer to [`ClientMsg::ListPushPhones`].
    PushPhones {
        phones: Vec<PushPhone>,
    },
    CloudProviders {
        providers: Vec<CloudProviderInfo>,
    },
    Transcript {
        /// Cleaned-up and vocabulary-corrected text, for review (VOC-03).
        text: String,
        /// The recognizer's text before corrections.
        raw_text: String,
        language: Option<String>,
        corrections: Vec<TranscriptCorrection>,
        intent: TranscriptIntent,
        /// Name of the engine that recognized the audio.
        engine: String,
        /// Topic of the ongoing dictation detected by the server's optional
        /// LLM pass (for example `frontend/React`), when known.
        #[serde(default)]
        topic: Option<String>,
    },
    SpeechOptions {
        engines: Vec<SpeechProfileInfo>,
        llms: Vec<SpeechProfileInfo>,
        /// The engine this device uses (its preference, else the default).
        current_engine: Option<String>,
        /// The correction LLM this device uses; `None` when off.
        current_llm: Option<String>,
    },
    /// Answer to [`ClientMsg::Synthesize`]: the encoded audio.
    Audio {
        /// `mp3`, `opus` (Ogg), `aac`, `flac` or `wav`.
        format: String,
        #[serde(with = "crate::event::serde_bytes_compat")]
        data: Vec<u8>,
    },
    /// Answer to [`ClientMsg::ListTtsEngines`]; empty when TTS is off.
    TtsEngines {
        engines: Vec<TtsEngineInfo>,
    },
    /// Answer to [`ClientMsg::Briefing`]: at most three short sentences.
    Briefing {
        text: String,
    },
    /// Answer to [`ClientMsg::Summarize`]: at most [`SUMMARY_MAX_CHARS`]
    /// characters of summary after a short session prefix.
    Summary {
        session_id: SessionId,
        text: String,
        /// The fixed "finished" line instead of an LLM summary (no LLM, no
        /// assistant message, or the turn was already summarized).
        #[serde(default)]
        fallback: bool,
    },
    /// A chunked upload may start.
    AttachReady {
        upload_id: String,
        /// Preferred data size per `AttachChunk`.
        chunk_size: u32,
    },
    /// A folder upload may start.
    AttachFolderReady {
        folder_id: String,
        max_files: u32,
        max_total_bytes: u64,
    },
    /// A file (or folder) was saved in the session's project folder.
    Attached {
        session_id: SessionId,
        /// Path relative to the project folder, with `/` separators.
        path: String,
        size: u64,
        /// The line that tells the agent about the file, e.g.
        /// `Attached file: @.vca/attachments/20261030-101500-photo.jpg`.
        /// Empty for the individual files of a folder upload.
        mention: String,
        /// The server typed `mention` into the session (managed sessions).
        /// When false, the client offers the mention to the user instead.
        typed: bool,
    },
    /// The command was typed into the shell (followed by Enter).
    ShellRan {
        session_id: SessionId,
        danger: DangerLevel,
    },
    /// Nothing was typed: the input has `lines` lines (two or more). Show
    /// all of `command` and, only on an explicit confirmation, resend it
    /// with [`ClientMsg::ShellRun`] and `confirm`. The answer to a
    /// multi-line `ShellRun` without `confirm` and to multi-line text sent
    /// to a shell with [`ClientMsg::SendInput`].
    ShellConfirm {
        session_id: SessionId,
        command: String,
        lines: u32,
    },
    /// Classification of a command line. `spans` are `[start, end)` UTF-8
    /// byte offsets of the risky parts; `reasons` are stable codes (see the
    /// user guide).
    CommandDanger {
        level: DangerLevel,
        reasons: Vec<String>,
        spans: Vec<[u32; 2]>,
        /// A spoken approval may run this: it is on the server's short list
        /// of plain read-only commands. Absent from older servers: no.
        #[serde(default)]
        voice_ok: bool,
    },
    /// Audit log entries, newest first.
    Audit {
        entries: Vec<AuditEntry>,
    },
    /// Pushed to every connected client (`id = 0`) about a remote shell.
    /// `event`:
    /// - `opened`;
    /// - `closed`, with `reason` `user`, `idle`, `lifetime`, `kill_switch`,
    ///   `exit` or `shutdown`;
    /// - `kept_open`: a client kept it open after a
    ///   [`ServerMsg::ShellClosingSoon`].
    ShellNotice {
        session_id: SessionId,
        event: String,
        device_name: String,
        #[serde(default)]
        reason: Option<String>,
    },
    /// Pushed to every connected client (`id = 0`), once per deadline,
    /// about a minute before a remote shell closes by itself. `reason`:
    /// `idle` (a [`ClientMsg::ShellKeepAlive`] or any input keeps it open)
    /// or `lifetime` (the absolute cap, `ShellInfo::hard_deadline`: nothing
    /// extends it). Only from servers whose `Welcome` lists
    /// [`FEATURE_SHELL_KEEP_ALIVE`].
    ShellClosingSoon {
        session_id: SessionId,
        seconds_left: u32,
        #[serde(default)]
        reason: String,
    },
    RestoreList {
        overview: RestoreOverview,
    },
    /// The state of a restore run; pushed to every client (`id = 0`) while
    /// it advances. A `dry_run` job is only sent to the asking client.
    RestoreProgress {
        job: RestoreJob,
    },
    /// Answer to [`ClientMsg::ArrangeList`] and [`ClientMsg::ArrangeEdit`].
    ArrangeList {
        overview: ArrangeOverview,
    },
    /// The report of an arrangement, with the state after it.
    ArrangeDone {
        result: ArrangeResult,
        overview: ArrangeOverview,
    },
    /// Answer to [`ClientMsg::ProjectFolders`].
    ProjectFolders {
        /// The workspace roots that exist; folders can be browsed below them.
        roots: Vec<FolderEntry>,
        /// Project folders of sessions and agent transcripts, most recent
        /// first, at most 30, all existing and inside the roots.
        recent: Vec<FolderEntry>,
        os: HostOs,
        /// The path separator of the server (`\` or `/`).
        separator: String,
    },
    /// Answer to [`ClientMsg::ListFolder`]: sub-folders sorted by name.
    Folder {
        /// The canonical folder.
        path: String,
        name: String,
        /// `None` at a workspace root (browsing never leaves the roots).
        parent: Option<String>,
        /// The workspace root that contains the folder.
        root: String,
        /// From the root down to this folder (both included).
        breadcrumbs: Vec<FolderCrumb>,
        entries: Vec<FolderEntry>,
        /// More folders exist than were listed.
        truncated: bool,
    },
    /// Answer to [`ClientMsg::FolderPath`].
    FolderPath {
        path: String,
        /// A folder exists there already.
        exists: bool,
    },
    /// Answer to [`ClientMsg::ScanSessions`], [`ClientMsg::HideObserved`]
    /// and [`ClientMsg::UnhideObserved`]: the AI agent processes found on
    /// the computer, ordered by agent and folder.
    ScanReport {
        found: Vec<FoundAgent>,
        /// When the pass ran.
        #[serde(default)]
        scanned_at: UnixMillis,
    },
    /// The conductor's state: the answer to every conductor request, and
    /// pushed to every client (`id = 0`) when it changes.
    Conductor {
        state: Box<ConductorState>,
    },
    /// A reply of the conductor (pushed, `id = 0`), for the 지휘소 chat and
    /// spoken replies. Redacted and at most a few hundred characters.
    ConductorReply {
        text: String,
        at: UnixMillis,
    },
    /// Answer to [`ClientMsg::TaskList`] and [`ClientMsg::TaskConfigure`],
    /// pushed to every client (`id = 0`) when a task changes. Active tasks
    /// first.
    Tasks {
        tasks: Vec<HumanTask>,
        settings: TaskSettings,
    },
    /// Answer to [`ClientMsg::TaskCreate`] and [`ClientMsg::TaskUpdate`].
    TaskSaved {
        task: HumanTask,
    },
    /// Answer to [`ClientMsg::TaskSendResult`]: the message (`sent` false
    /// for a preview).
    TaskResultSent {
        task: HumanTask,
        text: String,
        sent: bool,
    },
    /// Answer to [`ClientMsg::TaskExport`].
    TaskExported {
        format: TaskExportFormat,
        text: String,
    },
    /// A reminder (pushed, `id = 0`).
    TaskReminder {
        reminder: TaskReminder,
    },
    /// Answer to the pronunciation messages, pushed to every client
    /// (`id = 0`) when an entry changes. Sorted by term.
    Pronunciations {
        entries: Vec<crate::pronunciation::Pronunciation>,
    },
    /// Answer to [`ClientMsg::ListPersonalTerms`] and
    /// [`ClientMsg::SavePersonalTerms`].
    PersonalTerms {
        terms: Vec<crate::personal_terms::PersonalTerm>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ControlKey;

    #[test]
    fn json_shape_is_tagged() {
        let msg = Envelope {
            id: 7,
            body: ClientMsg::SendInput {
                session_id: "s1".into(),
                input: Input::Key {
                    key: ControlKey::Interrupt,
                },
            },
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["body"]["type"], "send_input");
        assert_eq!(json["body"]["input"]["type"], "key");
        assert_eq!(json["body"]["input"]["key"], "interrupt");
    }

    #[test]
    fn rename_session_shape() {
        let msg = ClientMsg::RenameSession {
            session_id: "s1".into(),
            name: Some("API work".into()),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "rename_session");
        assert_eq!(json["name"], "API work");
        // A missing name resets.
        let reset: ClientMsg =
            serde_json::from_str(r#"{"type":"rename_session","session_id":"s1"}"#).unwrap();
        assert_eq!(
            reset,
            ClientMsg::RenameSession {
                session_id: "s1".into(),
                name: None
            }
        );
        let bytes = rmp_serde::to_vec_named(&msg).unwrap();
        let back: ClientMsg = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn msgpack_roundtrip_with_bytes() {
        let msg = Envelope {
            id: 0,
            body: ServerMsg::Event {
                session_id: "s1".into(),
                event: SessionEvent::Output {
                    seq: 3,
                    at: 1,
                    data: b"hello\x1b[0m".to_vec(),
                },
            },
        };
        let bytes = rmp_serde::to_vec_named(&msg).unwrap();
        let back: Envelope<ServerMsg> = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn transcribe_final_defaults_to_false_and_uses_the_wire_name() {
        let json = r#"{"type":"transcribe","audio":[1,2],"format":"wav","sample_rate":null,"language":null,"session_id":null}"#;
        let msg: ClientMsg = serde_json::from_str(json).unwrap();
        assert!(matches!(
            msg,
            ClientMsg::Transcribe {
                is_final: false,
                ..
            }
        ));
        let msg = ClientMsg::Transcribe {
            audio: vec![1],
            format: AudioEncoding::Wav,
            sample_rate: None,
            language: None,
            session_id: None,
            context: None,
            is_final: true,
        };
        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["final"], true);
        let bytes = rmp_serde::to_vec_named(&msg).unwrap();
        assert_eq!(rmp_serde::from_slice::<ClientMsg>(&bytes).unwrap(), msg);
    }

    /// Audio and file bytes travel as MessagePack `bin` (one byte per byte),
    /// not as an array of numbers: a 5 s dictation clip stays ~160 KB on the
    /// wire. Only the JSON renderings (local IPC, the audit log) use arrays.
    #[test]
    fn audio_and_file_bytes_are_compact_on_the_wire_and_arrays_still_decode() {
        let audio: Vec<u8> = (0..160_044u32).map(|i| (i * 37 % 256) as u8).collect();
        let msg = Envelope {
            id: 9,
            body: ClientMsg::Transcribe {
                audio: audio.clone(),
                format: AudioEncoding::Wav,
                sample_rate: None,
                language: Some("ko".into()),
                session_id: None,
                context: None,
                is_final: true,
            },
        };
        let wire = rmp_serde::to_vec_named(&msg).unwrap();
        assert!(wire.len() < audio.len() + 200, "{} bytes", wire.len());
        assert_eq!(
            rmp_serde::from_slice::<Envelope<ClientMsg>>(&wire).unwrap(),
            msg
        );
        let chunk = ClientMsg::AttachChunk {
            upload_id: "u1".into(),
            offset: 0,
            data: audio.clone(),
        };
        let wire = rmp_serde::to_vec_named(&chunk).unwrap();
        assert!(wire.len() < audio.len() + 100, "{} bytes", wire.len());

        // An encoder that writes the bytes as an array (e.g. a plain serde
        // `Vec<u8>`) is still understood.
        #[derive(Serialize)]
        struct ArrayTranscribe<'a> {
            #[serde(rename = "type")]
            kind: &'a str,
            audio: Vec<u8>,
            format: &'a str,
            sample_rate: Option<u32>,
            language: Option<String>,
            session_id: Option<String>,
        }
        let old = rmp_serde::to_vec_named(&ArrayTranscribe {
            kind: "transcribe",
            audio: vec![82, 73, 70, 70, 200],
            format: "wav",
            sample_rate: None,
            language: None,
            session_id: None,
        })
        .unwrap();
        match rmp_serde::from_slice::<ClientMsg>(&old).unwrap() {
            ClientMsg::Transcribe { audio, .. } => assert_eq!(audio, vec![82, 73, 70, 70, 200]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn attach_messages_roundtrip() {
        let chunk = Envelope {
            id: 3,
            body: ClientMsg::AttachChunk {
                upload_id: "u1".into(),
                offset: 0,
                data: vec![1, 2, 3],
            },
        };
        let bytes = rmp_serde::to_vec_named(&chunk).unwrap();
        let back: Envelope<ClientMsg> = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(back, chunk);
        // Optional fields may be omitted by clients.
        let begin: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "attach_begin", "session_id": "s1", "name": "a.png",
            "mime": "image/png", "size": 10
        }))
        .unwrap();
        assert!(matches!(
            begin,
            ClientMsg::AttachBegin {
                submit: false,
                note: None,
                folder: None,
                batch: None,
                ..
            }
        ));
        let end: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "attach_batch_end", "batch_id": "b1"
        }))
        .unwrap();
        assert_eq!(
            end,
            ClientMsg::AttachBatchEnd {
                batch_id: "b1".into(),
                note: None,
                submit: false,
            }
        );
        let done = ServerMsg::Attached {
            session_id: "s1".into(),
            path: ".vca/attachments/x.png".into(),
            size: 10,
            mention: "Attached file: @.vca/attachments/x.png".into(),
            typed: true,
        };
        assert_eq!(serde_json::to_value(&done).unwrap()["type"], "attached");
    }

    #[test]
    fn shell_messages_shapes() {
        let open: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "open_shell",
            "request": {"shell": "powershell", "project_dir": "C:/code/app",
                        "display_name": null, "totp": "123456"}
        }))
        .unwrap();
        assert!(matches!(open, ClientMsg::OpenShell { .. }));
        let run: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "shell_run", "session_id": "s1", "command": "ls"
        }))
        .unwrap();
        assert_eq!(
            run,
            ClientMsg::ShellRun {
                session_id: "s1".into(),
                command: "ls".into(),
                totp: None,
                confirm: false,
            }
        );
        let confirmed: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "shell_run", "session_id": "s1", "command": "a\nb", "confirm": true
        }))
        .unwrap();
        assert!(matches!(
            confirmed,
            ClientMsg::ShellRun { confirm: true, .. }
        ));
        let join: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "shell_join", "session_id": "s1", "totp": "123456"
        }))
        .unwrap();
        assert!(matches!(join, ClientMsg::ShellJoin { .. }));
        let ask = serde_json::to_value(ServerMsg::ShellConfirm {
            session_id: "s1".into(),
            command: "a\nb".into(),
            lines: 2,
        })
        .unwrap();
        assert_eq!(
            ask,
            serde_json::json!({"type": "shell_confirm", "session_id": "s1",
                               "command": "a\nb", "lines": 2})
        );
        let list: ClientMsg = serde_json::from_str(r#"{"type":"list_audit","limit":5}"#).unwrap();
        assert_eq!(list, ClientMsg::ListAudit { limit: 5 });
        let danger = ServerMsg::CommandDanger {
            level: DangerLevel::Dangerous,
            reasons: vec!["recursive_delete".into()],
            spans: vec![[0, 6]],
            voice_ok: false,
        };
        let json = serde_json::to_value(&danger).unwrap();
        assert_eq!(json["type"], "command_danger");
        assert_eq!(json["level"], "dangerous");
        assert_eq!(json["spans"], serde_json::json!([[0, 6]]));
        let notice = ServerMsg::ShellNotice {
            session_id: "s1".into(),
            event: "closed".into(),
            device_name: "phone".into(),
            reason: Some("idle".into()),
        };
        let json = serde_json::to_value(&notice).unwrap();
        assert_eq!(json["type"], "shell_notice");
        let soon = ServerMsg::ShellClosingSoon {
            session_id: "s1".into(),
            seconds_left: 60,
            reason: "idle".into(),
        };
        assert_eq!(
            serde_json::to_value(&soon).unwrap(),
            serde_json::json!({
                "type": "shell_closing_soon", "session_id": "s1",
                "seconds_left": 60, "reason": "idle"
            })
        );
        // The shape the app reads.
        let soon: ServerMsg = serde_json::from_value(serde_json::json!({
            "type": "shell_closing_soon", "session_id": "s1", "seconds_left": 60
        }))
        .unwrap();
        assert!(matches!(
            soon,
            ServerMsg::ShellClosingSoon {
                seconds_left: 60,
                ..
            }
        ));
        let dismiss: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "dismiss_session", "session_id": "s1"
        }))
        .unwrap();
        assert!(
            matches!(dismiss, ClientMsg::DismissSession { ref session_id, stop: false } if session_id.0 == "s1")
        );
        // "Remove from VCA": `stop` is sent only when set, so older servers
        // see the message they know.
        let remove: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "dismiss_session", "session_id": "s1", "stop": true
        }))
        .unwrap();
        assert!(matches!(
            remove,
            ClientMsg::DismissSession { stop: true, .. }
        ));
        let plain = ClientMsg::DismissSession {
            session_id: "s1".into(),
            stop: false,
        };
        assert_eq!(
            serde_json::to_value(&plain).unwrap(),
            serde_json::json!({"type": "dismiss_session", "session_id": "s1"})
        );
        let keep: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "shell_keep_alive", "session_id": "s1"
        }))
        .unwrap();
        assert!(matches!(keep, ClientMsg::ShellKeepAlive { .. }));
        let ran = ServerMsg::ShellRan {
            session_id: "s1".into(),
            danger: DangerLevel::Safe,
        };
        assert_eq!(serde_json::to_value(&ran).unwrap()["danger"], "safe");
        let audit = ServerMsg::Audit {
            entries: vec![AuditEntry {
                ts: 1,
                device_id: "dev_1".into(),
                device_name: "phone".into(),
                event: "shell_open".into(),
                session_id: None,
                detail: "shell=cmd".into(),
            }],
        };
        for msg in [danger, notice, ran, audit] {
            let bytes = rmp_serde::to_vec_named(&msg).unwrap();
            assert_eq!(rmp_serde::from_slice::<ServerMsg>(&bytes).unwrap(), msg);
        }
        let bytes = rmp_serde::to_vec_named(&open).unwrap();
        assert_eq!(rmp_serde::from_slice::<ClientMsg>(&bytes).unwrap(), open);
        let code = serde_json::to_value(crate::error::ErrorCode::TotpRequired).unwrap();
        assert_eq!(code, "totp_required");
    }

    #[test]
    fn arrange_messages_shapes() {
        let apply: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "arrange_apply",
            "monitors": [{
                "monitor": "A", "rows": 2, "cols": 3,
                "cells": [{"cell": 4, "window": "65552"}]
            }]
        }))
        .unwrap();
        let ClientMsg::ArrangeApply { monitors, totp } = &apply else {
            panic!("{apply:?}");
        };
        assert_eq!(monitors[0].cells[0].window, "65552");
        // Older clients send no code; none is written when there is none.
        assert_eq!(*totp, None);
        let undo: ClientMsg = serde_json::from_str(r#"{"type":"arrange_undo"}"#).unwrap();
        assert_eq!(undo, ClientMsg::ArrangeUndo { totp: None });
        assert_eq!(
            serde_json::to_value(&undo).unwrap(),
            serde_json::json!({"type": "arrange_undo"})
        );
        let layout: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "arrange_apply_layout", "layout": "l1", "totp": "123456"
        }))
        .unwrap();
        assert_eq!(
            layout,
            ClientMsg::ArrangeApplyLayout {
                layout: "l1".into(),
                totp: Some("123456".into()),
            }
        );
        let undo = ClientMsg::ArrangeUndo {
            totp: Some("123456".into()),
        };
        let bytes = rmp_serde::to_vec_named(&undo).unwrap();
        assert_eq!(rmp_serde::from_slice::<ClientMsg>(&bytes).unwrap(), undo);
        let bytes = rmp_serde::to_vec_named(&ClientMsg::ArrangeUndo { totp: None }).unwrap();
        assert_eq!(
            rmp_serde::from_slice::<ClientMsg>(&bytes).unwrap(),
            ClientMsg::ArrangeUndo { totp: None }
        );
        let edit: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "arrange_edit",
            "edit": {"op": "layout_delete", "layout": "l1"}
        }))
        .unwrap();
        assert!(matches!(edit, ClientMsg::ArrangeEdit { .. }));
    }

    #[test]
    fn restore_messages_shapes() {
        let run: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "restore_run", "request": {"only": ["acme-pdf"], "dry_run": true}
        }))
        .unwrap();
        let ClientMsg::RestoreRun { request } = &run else {
            panic!("{run:?}");
        };
        assert_eq!(request.only, ["acme-pdf"]);
        assert!(request.dry_run);
        let list: ClientMsg = serde_json::from_str(r#"{"type":"restore_list"}"#).unwrap();
        assert_eq!(list, ClientMsg::RestoreList);
        let focus: ClientMsg =
            serde_json::from_str(r#"{"type":"focus_terminal","session_id":"s1"}"#).unwrap();
        assert_eq!(
            focus,
            ClientMsg::FocusTerminal {
                session_id: SessionId::from("s1")
            }
        );
        let import = ClientMsg::RestoreImport {
            layout: "{}".into(),
            profile: None,
        };
        let job = ServerMsg::RestoreProgress {
            job: RestoreJob {
                id: "r1".into(),
                profile: "default".into(),
                by: "local".into(),
                dry_run: false,
                started_at: 1,
                finished_at: None,
                items: Vec::new(),
            },
        };
        assert_eq!(
            serde_json::to_value(&job).unwrap()["type"],
            "restore_progress"
        );
        let bytes = rmp_serde::to_vec_named(&job).unwrap();
        assert_eq!(rmp_serde::from_slice::<ServerMsg>(&bytes).unwrap(), job);
        for msg in [run, import] {
            let bytes = rmp_serde::to_vec_named(&msg).unwrap();
            assert_eq!(rmp_serde::from_slice::<ClientMsg>(&bytes).unwrap(), msg);
        }
    }

    #[test]
    fn push_messages_shapes_and_hidden_tokens() {
        // The shape crates/vca-client/src/push.rs sends.
        let reg: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "push_register", "machine": "m1", "platform": "ios",
            "token": "apns-secret", "e2e_pubkey": "key", "locale": null
        }))
        .unwrap();
        let ClientMsg::PushRegister {
            platform, token, ..
        } = &reg
        else {
            panic!("{reg:?}");
        };
        assert_eq!(*platform, PushPlatform::Ios);
        assert_eq!(token.expose(), "apns-secret");
        assert!(!format!("{reg:?}").contains("apns-secret"));
        let unreg: ClientMsg = serde_json::from_str(r#"{"type":"push_unregister"}"#).unwrap();
        assert_eq!(unreg, ClientMsg::PushUnregister);
        let list: ClientMsg = serde_json::from_str(r#"{"type":"list_push_phones"}"#).unwrap();
        assert_eq!(list, ClientMsg::ListPushPhones);
        let phones = ServerMsg::PushPhones {
            phones: vec![PushPhone {
                phone: "dev_1".into(),
                machine: "m1".into(),
                platform: PushPlatform::Android,
                token: PushToken("fcm-secret".into()),
                e2e_pubkey: "key".into(),
                locale: Some("ko".into()),
            }],
        };
        let json = serde_json::to_value(&phones).unwrap();
        assert_eq!(json["type"], "push_phones");
        assert_eq!(json["phones"][0]["token"], "fcm-secret");
        assert_eq!(json["phones"][0]["platform"], "android");
        assert!(!format!("{phones:?}").contains("fcm-secret"));
        let bytes = rmp_serde::to_vec_named(&phones).unwrap();
        assert_eq!(rmp_serde::from_slice::<ServerMsg>(&bytes).unwrap(), phones);
        for msg in [reg, unreg, list] {
            let bytes = rmp_serde::to_vec_named(&msg).unwrap();
            assert_eq!(rmp_serde::from_slice::<ClientMsg>(&bytes).unwrap(), msg);
        }
    }

    #[test]
    fn json_roundtrip_with_bytes() {
        let ev = SessionEvent::Output {
            seq: 1,
            at: 2,
            data: vec![0, 255, 10],
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: SessionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn paired_device_new_fields_default() {
        // A device entry from an older server.
        let old = r#"{"id":"dev_1","name":"Phone","platform":"ios","paired_at":1,"last_seen":2,"current":false}"#;
        let d: PairedDevice = serde_json::from_str(old).unwrap();
        assert_eq!(d.scope, None);
        assert!(!d.online);
        assert_eq!(d.connections, 0);
        assert_eq!(d.last_ip, None);
        let msg = ClientMsg::SetDeviceScope {
            device_id: "dev_1".into(),
            scope: DeviceScope::Command,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "set_device_scope");
        assert_eq!(json["scope"], "command");
        let msg = ClientMsg::RenameDevice {
            device_id: "dev_1".into(),
            name: "Tablet".into(),
        };
        assert_eq!(serde_json::to_value(&msg).unwrap()["type"], "rename_device");
    }

    #[test]
    fn synthesize_shapes() {
        // Language and engine are optional on the wire.
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"synthesize","text":"Hi"}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::Synthesize {
                text: "Hi".into(),
                language: None,
                engine: None,
                voice: None,
            }
        );
        let with_voice: ClientMsg =
            serde_json::from_str(r#"{"type":"synthesize","text":"Hi","voice":"F1"}"#).unwrap();
        assert!(matches!(
            with_voice,
            ClientMsg::Synthesize { voice: Some(ref v), .. } if v == "F1"
        ));
        let json = serde_json::to_value(ClientMsg::ListTtsEngines).unwrap();
        assert_eq!(json["type"], "list_tts_engines");
        let audio = ServerMsg::Audio {
            format: "mp3".into(),
            data: vec![0, 1, 255],
        };
        assert_eq!(serde_json::to_value(&audio).unwrap()["type"], "audio");
        let bytes = rmp_serde::to_vec_named(&audio).unwrap();
        assert_eq!(rmp_serde::from_slice::<ServerMsg>(&bytes).unwrap(), audio);
        let list = ServerMsg::TtsEngines {
            engines: vec![TtsEngineInfo {
                name: "local".into(),
                languages: vec!["en".into()],
                default: true,
                speakers: vec![TtsSpeaker {
                    id: "F1".into(),
                    label: "Female 1".into(),
                    gender: TtsGender::Female,
                }],
                disclosure: Some("Machine-generated voice".into()),
            }],
        };
        let json = serde_json::to_value(&list).unwrap();
        assert_eq!(json["type"], "tts_engines");
        assert_eq!(json["engines"][0]["name"], "local");
        assert_eq!(json["engines"][0]["speakers"][0]["gender"], "female");
        let bytes = rmp_serde::to_vec_named(&list).unwrap();
        assert_eq!(rmp_serde::from_slice::<ServerMsg>(&bytes).unwrap(), list);
        // Older servers send neither field.
        let old: ServerMsg = serde_json::from_str(
            r#"{"type":"tts_engines","engines":[{"name":"a","languages":[],"default":false}]}"#,
        )
        .unwrap();
        let ServerMsg::TtsEngines { engines } = old else {
            panic!("wrong variant");
        };
        assert!(engines[0].speakers.is_empty() && engines[0].disclosure.is_none());
    }

    #[test]
    fn briefing_and_summary_shapes() {
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"briefing"}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::Briefing {
                language: None,
                order: Vec::new(),
            }
        );
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"summarize","session_id":"s1","language":"ko"}"#)
                .unwrap();
        assert!(matches!(
            msg,
            ClientMsg::Summarize { ref session_id, number: None, .. } if session_id.0 == "s1"
        ));
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"set_voice_summary","enabled":true}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::SetVoiceSummary {
                enabled: true,
                details: false,
            }
        );
        let reply = ServerMsg::Summary {
            session_id: SessionId("s1".into()),
            text: "Session 1: done.".into(),
            fallback: false,
        };
        assert_eq!(serde_json::to_value(&reply).unwrap()["type"], "summary");
        let bytes = rmp_serde::to_vec_named(&reply).unwrap();
        assert_eq!(rmp_serde::from_slice::<ServerMsg>(&bytes).unwrap(), reply);
        let brief = ServerMsg::Briefing { text: "x".into() };
        assert_eq!(serde_json::to_value(&brief).unwrap()["type"], "briefing");
    }

    #[test]
    fn project_folder_shapes() {
        let list: ClientMsg =
            serde_json::from_str(r#"{"type":"list_folder","path":"C:\\code"}"#).unwrap();
        assert_eq!(
            list,
            ClientMsg::ListFolder {
                path: r"C:\code".into(),
                show_hidden: false
            }
        );
        let folders: ClientMsg = serde_json::from_str(r#"{"type":"project_folders"}"#).unwrap();
        assert_eq!(folders, ClientMsg::ProjectFolders);
        let entry = FolderEntry {
            name: "app".into(),
            path: "/home/me/app".into(),
            is_git_repo: true,
            has_agent_history: false,
            last_used: Some(5),
        };
        let reply = ServerMsg::ProjectFolders {
            roots: vec![entry.clone()],
            recent: vec![entry.clone()],
            os: HostOs::Macos,
            separator: "/".into(),
        };
        let json = serde_json::to_value(&reply).unwrap();
        assert_eq!(json["type"], "project_folders");
        assert_eq!(json["os"], "macos");
        let folder = ServerMsg::Folder {
            path: "/home/me".into(),
            name: "me".into(),
            parent: None,
            root: "/home/me".into(),
            breadcrumbs: vec![FolderCrumb {
                name: "/home/me".into(),
                path: "/home/me".into(),
            }],
            entries: vec![entry],
            truncated: false,
        };
        assert_eq!(serde_json::to_value(&folder).unwrap()["type"], "folder");
        for msg in [reply, folder] {
            let bytes = rmp_serde::to_vec_named(&msg).unwrap();
            assert_eq!(rmp_serde::from_slice::<ServerMsg>(&bytes).unwrap(), msg);
        }
        // Older servers send no features.
        let old: ServerMsg = serde_json::from_str(
            r#"{"type":"welcome","protocol_version":1,"server_version":"0","server_name":"x"}"#,
        )
        .unwrap();
        assert!(matches!(old, ServerMsg::Welcome { ref features, .. } if features.is_empty()));
    }

    #[test]
    fn session_order_messages_shape() {
        let get: ClientMsg = serde_json::from_str(r#"{"type":"get_session_order"}"#).unwrap();
        assert_eq!(get, ClientMsg::GetSessionOrder);
        let set: ClientMsg =
            serde_json::from_str(r#"{"type":"set_session_order","session_ids":["b","a"]}"#)
                .unwrap();
        assert_eq!(
            set,
            ClientMsg::SetSessionOrder {
                session_ids: vec![SessionId("b".into()), SessionId("a".into())]
            }
        );
        let reset: ClientMsg = serde_json::from_str(r#"{"type":"reset_session_order"}"#).unwrap();
        assert_eq!(reset, ClientMsg::ResetSessionOrder);
        let order = ServerMsg::SessionOrder {
            session_ids: vec![SessionId("b".into())],
            custom: true,
        };
        let json = serde_json::to_value(&order).unwrap();
        assert_eq!(json["type"], "session_order");
        assert_eq!(json["session_ids"][0], "b");
        assert_eq!(json["custom"], true);
        let bytes = rmp_serde::to_vec_named(&order).unwrap();
        assert_eq!(rmp_serde::from_slice::<ServerMsg>(&bytes).unwrap(), order);
    }

    #[test]
    fn scan_messages_shape() {
        let scan: ClientMsg = serde_json::from_str(r#"{"type":"scan_sessions"}"#).unwrap();
        assert_eq!(scan, ClientMsg::ScanSessions);
        let hide: ClientMsg = serde_json::from_str(r#"{"type":"hide_observed","pid":42}"#).unwrap();
        assert_eq!(
            hide,
            ClientMsg::HideObserved {
                session_id: None,
                pid: Some(42)
            }
        );
        let all: ClientMsg =
            serde_json::from_str(r#"{"type":"unhide_observed","all":true}"#).unwrap();
        assert_eq!(
            all,
            ClientMsg::UnhideObserved {
                session_id: None,
                pid: None,
                all: true
            }
        );
        let report = ServerMsg::ScanReport {
            found: vec![FoundAgent {
                pid: 7,
                agent: crate::session::AgentKind::Codex,
                project_dir: Some("/src/app".into()),
                terminal: Some("ChatGPT".into()),
                title: None,
                status: FoundStatus::NotInATerminal,
                session_id: None,
            }],
            scanned_at: 5,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["type"], "scan_report");
        assert_eq!(json["found"][0]["status"], "not_in_a_terminal");
        assert_eq!(json["found"][0]["agent"]["type"], "codex");
        for (status, wire) in [
            (FoundStatus::Imported, "imported"),
            (FoundStatus::HiddenByUser, "hidden_by_user"),
            (FoundStatus::NoTranscriptYet, "no_transcript_yet"),
            (FoundStatus::UnsupportedAgent, "unsupported_agent"),
        ] {
            assert_eq!(serde_json::to_value(status).unwrap(), wire);
        }
        let bytes = rmp_serde::to_vec_named(&report).unwrap();
        assert_eq!(rmp_serde::from_slice::<ServerMsg>(&bytes).unwrap(), report);
    }
}
