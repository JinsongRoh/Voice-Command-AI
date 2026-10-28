# Architecture

> **Status:** Draft v0.1. Companion to [requirements.md](requirements.md).

## 1. High-level view

```
                         ┌──────────────────────── Developer machine ───────────────────────┐
                         │                                                                  │
┌──────────────┐  E2E    │  ┌────────────┐   ┌───────────────────────────────────────────┐ │
│ VCA Mobile   │◄───────►│  │ Connectivity│◄─►│ VCA Server (vcad)                         │ │
│ (Tauri 2)    │ channel │  │ provider    │   │                                           │ │
└──────────────┘         │  │ DNS Pilot / │   │  api ──── auth ──── audit                 │ │
       ▲                 │  │ Tailscale / │   │   │                                       │ │
       │ push (E2E)      │  │ Cloudflare /│   │  session manager ── adapters ── hooks     │ │
┌──────┴───────┐         │  │ LAN         │   │   │        │                              │ │
│ Push relay   │◄────────┼──┤             │   │  pty host  resource broker   speech       │ │
│ (DNS Pilot)  │         │  └────────────┘   └───┬───────────────┬───────────────────────┘ │
└──────────────┘         │                       │               │                          │
                         │        ┌──────────────┴──┐   ┌────────┴────────┐                 │
                         │        │ Agent processes │   │ Terminal windows│ (attached views)│
                         │        │ claude · codex ·│   │ WT · iTerm2 ·   │                 │
                         │        │ gemini · grok … │   │ tmux · …        │                 │
                         │        └─────────────────┘   └─────────────────┘                 │
                         │  ┌──────────────┐                                                 │
                         │  │ VCA Desktop  │ tray · dashboard · pairing QR · kill switch     │
                         │  └──────────────┘                                                 │
                         └──────────────────────────────────────────────────────────────────┘
```

## 2. Repository layout (planned)

```
crates/
  vca-core/        Shared types, protocol messages, configuration
  vca-server/      Daemon (vcad): API, session manager, broker, audit
  vca-pty/         Cross-platform PTY host (ConPTY on Windows, openpty on Unix)
  vca-adapters/    Agent adapter trait and built-in adapters
  vca-broker/      Resource monitoring, admission control, policies
  vca-speech/      STT/TTS engine trait, Whisper and cloud engines, vocabulary engine
  vca-net/         Connectivity providers, E2E channel, pairing
  vca-cli/         `vca` command
apps/
  desktop/         Tauri 2 desktop app
  mobile/          Tauri 2 mobile app (iOS / Android)
  ui/              Shared front-end used by both apps
lexicon/           Open-source developer vocabulary, one folder per language
docs/
```

## 3. Session model

```rust
pub struct Session {
    pub id: SessionId,
    pub agent: AgentKind,          // ClaudeCode, Codex, GeminiCli, Antigravity, Grok, Generic(...)
    pub project_dir: PathBuf,
    pub display_name: String,
    pub profile: Option<ProfileId>,
    pub state: SessionState,       // Running, Idle, AwaitingInput, AwaitingDecision, Paused, Error
    pub control: ControlTier,      // ManagedPty, NativeChannel, TerminalApi, ObserveOnly
    pub process_tree: ProcessTreeHandle,
}
```

Each session has exactly one **control tier**, the strongest mechanism VCA can use to deliver input to it.

## 4. Command delivery tiers

| Tier | Mechanism | Needs focus | When used |
|---|---|---|---|
| 1. **Managed PTY** | The agent runs inside a pseudo-terminal owned by `vcad`. Terminal windows are attached views of that PTY. | No | Sessions started via shell integration, `vca run`, or from the app. **Default.** |
| 2. **Native channel** | The agent's own interface: hooks, inter-session messaging, headless/resume modes, app-server protocols. | No | Agents that expose one. Also used for background jobs. |
| 3. **Terminal API** | The terminal emulator's scripting interface: `tmux send-keys`, `wezterm cli send-text`, `kitty @ send-text`, the iTerm2 API, and Windows console input buffers. | No | Sessions started outside VCA in a scriptable terminal. |
| 4. **Simulated keystrokes** | OS-level key injection. | Yes | Disabled by default, last resort only. |

### 4.1 Shell integration (transparent management)

The setup wizard installs a small shell hook (bash, zsh, fish, PowerShell). When the user types a supported agent command such as `claude`, the hook runs it through `vca run claude` instead:

1. `vcad` spawns the agent in a managed PTY with the user's environment and working directory.
2. The current terminal becomes an attached view. It relays I/O and resizing, so the experience is identical to running the agent directly.
3. If `vcad` is not running, the hook falls back to launching the agent directly, so the user is never blocked.

The hook can be turned off globally or bypassed for a single run (for example with `command claude`).

### 4.2 Why not `--print` / headless for everything

Headless and resume modes (for example `claude -p --resume <id>`) start a **separate** process on the same conversation. They do not drive an interactive session that is already open, and running both at once can make the history diverge. VCA therefore uses them for background and orchestrated tasks, and uses tiers 1 and 3 for live sessions.

## 5. Agent adapters

```rust
#[async_trait]
pub trait AgentAdapter: Send + Sync {
    fn kind(&self) -> AgentKind;
    /// Find sessions of this agent that are already running or stored on disk.
    async fn discover(&self) -> Result<Vec<DiscoveredSession>>;
    /// Build the command line for launching a new managed session.
    fn launch_spec(&self, req: &LaunchRequest) -> Result<LaunchSpec>;
    /// Stream structured events (messages, tool calls, state changes) for the chat view.
    async fn events(&self, session: &Session) -> Result<EventStream>;
    /// Deliver input over a native channel, if the agent has one.
    async fn send_native(&self, session: &Session, input: &Input) -> Result<Delivery>;
    /// Install or verify agent-side hooks used for notifications and admission control.
    async fn install_hooks(&self, scope: HookScope) -> Result<()>;
    /// Profile isolation, for example a per-profile config directory.
    fn profile_env(&self, profile: &Profile) -> Vec<(String, String)>;
}
```

| Agent | Discovery source | Events | Native channel | Profile isolation |
|---|---|---|---|---|
| Claude Code | Process scan, session transcripts on disk | Hooks (Stop, Notification, permission, PreToolUse) | Hooks, local session messaging, headless resume | Config directory per profile |
| OpenAI Codex | Process scan, session files | `notify` configuration, session files | `exec resume`, app-server protocol | Home directory per profile |
| Gemini CLI | Process scan, checkpoints | To be validated in M0 | To be validated in M0 | To be validated in M0 |
| Google Antigravity | To be validated in M0 | To be validated in M0 | To be validated in M0 | To be validated in M0 |
| Grok (xAI) | To be validated in M0 | To be validated in M0 | To be validated in M0 | To be validated in M0 |
| Generic | Process name match | Output-pattern heuristics | None (tiers 1 and 3 only) | None |

## 6. Resource broker

```
 agent hook (PreToolUse etc.)          vcad broker                         OS
 ─────────────────────────────►  classify command (heavy / light)
   "about to run: cargo test"     check live load + policies
                                  ├─ grant  → hook returns allow ──────►  job runs (in session job object / cgroup)
                                  ├─ queue  → hook waits, card shows "queued"
                                  └─ deny   → hook returns reason to the agent
```

- **Monitoring:** system and per-process-tree sampling every second, with smoothed load indicators.
- **Containment:**
  - Windows: Job Objects.
  - Linux: cgroups v2, when available.
  - macOS: process groups plus priority (`nice`/`taskpolicy`).
- **Actions:** grant, queue, deprioritize, throttle, pause children (SIGSTOP / job suspend), propose termination.
- **Autonomous mode:** built-in policy that reserves headroom (for example 15% memory) and limits concurrent heavy jobs based on core count and memory.
- **User policies:** declarative rules in `policies.toml`, editable from desktop and mobile.

## 7. Protocol

- **Transport:** WebSocket over TLS through the connectivity provider. Messages are encoded with MessagePack.
- **Application-layer E2E encryption:** a Noise protocol (`XX` pattern) session between the paired device key and the server key. The tunnel operator only sees ciphertext.
- **Message families:** `session.*` (list, update, create, control), `cmd.*` (send, ack), `stream.*` (subscribe, chunk, resume from offset), `notify.*`, `res.*` (metrics, queue, policy), `auth.*` and `audit.*`.
- **Resumable streams:** every stream chunk has a sequence number, so clients resume from the last offset after reconnecting.

## 8. Security model

| Layer | Control |
|---|---|
| Pairing | QR code containing the server public key and a one-time pairing secret. The device generates its own key pair, stored in Secure Enclave or Android Keystore. |
| Authentication | Device key signature, plus TOTP at sign-in and for step-up actions. |
| Sessions | Access tokens valid for 15 minutes. Refresh tokens are bound to the device key and revocable. |
| Authorization | Workspace allow-list, plus per-device permission scopes (view only / command / admin). |
| Transport | Tunnel TLS, plus Noise end-to-end encryption. |
| Detection | Audit log, new-device and new-location alerts, and rate limiting with lockout. |
| Response | Kill switch (local, mobile, DNS Pilot console) and device revocation. |
| Secrets | OS keychain (Windows Credential Manager, macOS Keychain, Secret Service), plus output masking in streams. |

## 9. Speech pipeline

```
mic (device) ─► VAD ─► audio chunks ─► STT engine ─► vocabulary correction ─► intent router ─► editable transcript ─► send
                                       (device, server            (lexicon layers 1–3)   (control vs prompt)
                                        or cloud)
```

- Engines implement a common `SttEngine` trait. Each engine can run on the device, on the server (local Whisper using the developer machine's CPU or GPU), or in the cloud.
- Language configuration: a primary language plus secondary languages. Code-switching works because recognition is not forced to a single language.
- TTS for spoken notifications uses platform TTS on the device by default.

## 10. Key technical risks (to be resolved in M0)

| Risk | Mitigation |
|---|---|
| Focus-free input into existing Windows Terminal sessions | Default to managed PTY through shell integration. Prototype console input buffer injection. |
| Agents without hooks or native channels | Generic adapter using output heuristics. Contribute hook support upstream where possible. |
| Pausing an agent mid-request causes API timeouts | Pause child processes only by default. Pause whole sessions only on explicit request. |
| Tauri 2 mobile audio and background limits | Prototype microphone capture and VAD in M0. Fall back to a native plugin if needed. |
| DNS Pilot gaps (app-token access, P2P SDK, push relay, relay regions) | Track them in dnspilot-requirements.md. Use the provider abstraction. Tailscale and Cloudflare Tunnel act as fallbacks until the gaps are closed. |
