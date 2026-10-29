# Development Handoff (M0)

> Snapshot of branch `dev/m0-foundation` when development moved from a cloud session to a local machine. Read this first, then `docs/development-plan.md`, `docs/architecture.md` and `docs/requirements.md`.

## 1. Branches and pull requests

| Branch | PR | Content |
|---|---|---|
| `docs/planning` | #1 (draft) | Planning documents: requirements, architecture, development plan, DNS Pilot integration. |
| `dev/m0-foundation` | #2 (draft, stacked on #1) | Cargo workspace, all crates below and the shared UI. **Continue here.** |

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `cargo clippy --workspace --all-targets` and `cargo test --workspace` on Linux, macOS and Windows with `RUSTFLAGS=-D warnings`. A separate UI job runs `pnpm check`.

## 2. Status by component

| Component | Status | Tests (Linux) | Notes |
|---|---|---|---|
| `vca-core` | Done | 3 | Session model, events, inputs, resources and the client/server protocol. Includes the UI-requested additions (cloud launch, devices, kill switch, `preview`, `Acknowledged`). |
| `vca-pty` + `tools/vca-fake-agent` | Done | 62 | Managed PTY, focus-free input, sequence-numbered replay, screen snapshot, attach protocol, shell integration snippets. See §3.1. |
| `vca-adapters` | Done | 72 | Claude Code and Codex adapters (transcripts, hooks/notify install, hook payloads, profiles), generic adapters, output heuristics, process tables, tier-3 injection (tmux verified; WezTerm, kitty, iTerm2, Windows console unverified). |
| `vca-broker` | Done | 75 | Classifier, `policies.toml`, admission controller with leased slots and queue, IPC for hooks (fail open), containment, monitor, threshold watcher. GPU sampling, cgroups v2 and history (RES-08) not done. |
| `vca-net` | Done (agent stopped during final review) | 95 | Keys, Noise XX with chunking, QR pairing, device registry, TOTP, rotating tokens, WebSocket transport with heartbeat, replay buffers, audit log, kill switch. Re-review it against the new `ClientMsg` variants before wiring. |
| `vca-speech` + `lexicon/` | Done (agent stopped during final review) | 68 | STT trait with OpenAI-compatible, Google and optional local Whisper engines (`whisper-local` feature not build-verified), VAD, layered developer vocabulary, multilingual intent router, filler cleanup. |
| `vca-cloud` | Done | 38 | Cloud adapters. Copilot, Claude Managed Agents, Claude Code routines and Codex cloud were checked against official sources. **Jules, Cursor and Devin field names are unverified** (docs were blocked in the cloud session). |
| `vca-providers` | Done | 33 | LAN, Tailscale, Cloudflare Tunnel, DNS Pilot (local Agent API, OAuth device flow, `app`-mode tunnels, `boot_id` event tracking) and custom providers. DNS Pilot JSON fields are placeholders in `dnspilot/contract.rs`. |
| `apps/ui` | Done (protocol sync just finished) | 65 | React + TypeScript shared UI with a mock transport. Verify the "subscribe only to the selected session" change end to end. Native mode (Tauri bridge, pairing, TOTP login, forget, server transcription) added in task 5. |
| `vca-server` (`vcad`) | §4 steps 3 and 4 done | 63 (Windows) | Session manager, API, local control channel, hooks with admission, discovery, Job Objects (Windows), `vca-net` remote transport (pairing, TOTP, scopes, revoke, kill switch, audit), cloud sessions as cards (`[cloud]`, `vca cloud`), connectivity providers for pairing endpoints (`[remote.provider]`), server-side transcription (`[speech]`, `vca transcribe`). |
| `vca-client` | Done | 13 | Device-side connection manager for the apps: pairing from `vca://pair` links, keys and rotating refresh tokens in a `KeyStore`, supervised reconnects with backoff, `LoginRequired`/`revoked`/`kill_switch` states. |
| `apps/shell`, `apps/desktop`, `apps/mobile` | Desktop verified on Windows | 3 | Tauri 2 shells (separate Cargo workspace `apps/Cargo.toml`). See §3.6. |
| `vca-cli` (`vca`) | Done | 28 | All §6 commands. Hooks fail open (checked against a missing vcad). |

## 3. Known issues

### 3.1 Windows: ConPTY cursor query blocks managed sessions (fixed)

ConPTY writes a cursor position query (`ESC[6n`) at startup and waits for an answer; portable-pty 0.9 hard-codes the inherit-cursor flag, so the query cannot be turned off without forking it. `vca-pty` now answers every `ESC[6n` itself (also when split across chunks) from the vt100 cursor at the point of the query, or `1;1` with screen tracking off (`SpawnOptions::answer_cursor_queries`, on by default). `attach_stdio` counts the queries it writes to the local terminal and drops that terminal's replies (`CprReplyFilter`, with a 2 s expiry so Ctrl+F3 is not swallowed later).

Found on the way, also fixed: portable-pty's Windows killer inverts `TerminateProcess`'s result (`PtySession::kill` ignores it on Windows); ConPTY drops bare line feeds inside bracketed paste (newlines are now sent as `
`, as real terminals do); removing a paste marker could join its neighbors into a new marker (now stripped to a fixed point).

### 3.2 Process containment on Windows reached unrelated processes (fixed)

Windows keeps a process's parent pid after the parent exits and reuses pids, and `vca-broker` built process trees from parent pids alone. While the end-to-end tests ran on a workstation, stopping a session called `terminate_session(ChildrenOnly)`, and the call terminated unrelated agent sessions on that machine. Fixed in `a78246f`:
- On Windows, parent links are verified with creation times; links that cannot be verified are dropped.
- `Stop` kills only the PTY child VCA started.
- Observed sessions are neither stoppable nor registered with the broker.
- Tests use fake process controls.

**Rule:** tests that terminate or suspend real processes run only with `VCA_PROCESS_CONTROL_TESTS=1`, which is set in CI (disposable VMs). Never set it on a workstation. Done in `cca8d75`: on Windows each managed session has its own Job Object, and `Stop` terminates that job. CI checks this (`stop_ends_processes_the_agent_started`).

### 3.3 Other items
- `vca-adapters`: WezTerm, kitty, iTerm2 and Windows console injection are unverified on real machines.
- `vca-cloud`: confirm Jules, Cursor and Devin request/response shapes against their official docs (PoC-7).
- `vca-providers`: align `dnspilot/contract.rs` with the DNS Pilot VCA contract (`VCA-contract.md` in the DNS Pilot repository).
- macOS and Windows behavior of every crate has only been compile-checked or CI-tested; run the M0 PoCs on real machines.

### 3.4 Verified on a real Windows 11 machine (task 2)

- `vca run` in Windows Terminal, with `vca-fake-agent` and with Claude Code 2.1.312:
  - output renders cleanly, with no stray cursor-report characters;
  - `vca send` delivers input without window focus (Claude Code answered);
  - decisions are detected and answered;
  - the exit code reaches the terminal (`/exit 5` showed code 5).
- `ping`, `ls`, `resources` and `stop` against a live vcad.

Verified later on the same machine (task 3 session):
- **Claude Code hooks, project-local only** (installed in a temporary project; a user-scope install would affect every session on a shared machine).
  - The real SessionStart hook reached vcad. A/B test against a project without hooks: heuristics stopped for the hooked session.
  - A Notification hook run through Git Bash produced a notification event.
  - Found and fixed: Git Bash rewrote `VCA_CONTROL=\\.\pipe\...`, so the hook could not reach vcad.
- **`codex` 0.153.4 through API `Launch`**: it starts and accepts focus-free input.
  - Found and fixed: portable-pty picked npm's extensionless `codex` script instead of `codex.cmd`.
  - The trust dialog was not answered, because answering writes to the global `~/.codex/config.toml`.
- **Remote transport against a live vcad**: `vca remote`, `vca pair` (terminal QR), `vca devices` and `vca kill-switch [--reset]` work; the listener is bound to 127.0.0.1 only.
- **Linux tests** run in WSL2 Ubuntu 24.04, isolated from Windows processes. They found Windows-only lookup tests running on Linux (fixed).

### 3.5 Task 4: cloud, providers and speech in the server

- **Protocol:** `Transcribe`/`Transcript` (command scope, because server engines spend the owner's speech quota) and `ListCloudProviders`/`CloudProviders` (view scope). The UI types and mock transport mirror them.
- **Speech (`[speech]`, off by default):** OpenAI-compatible or Google engine → filler clean-up → vocabulary correction (built-in base lexicon embedded at build time, the selected session's project terms, `data_dir/speech/personal.json`) → intent router. Results go back for review only. API keys come only from environment variables named in the config, and HTTP errors drop the URL (Google keys travel in the query string).
- **Providers (`[remote.provider]`, none by default):** LAN, Tailscale (serve or direct), Cloudflare (quick or named) and custom URLs. The provider's endpoint goes into pairing codes, `vca remote` shows it, and it is torn down at shutdown. LAN and Tailscale direct are refused while `remote.bind` is loopback, so exposure is always an explicit choice. **DNS Pilot is refused** until its API contract is public (no undocumented APIs).
- **Cloud (`[[cloud.accounts]]`):** `Kind::Cloud` sessions with an `ActivityPoller`. Supported actions: follow-up text, Stop, and the provider capability list. Credentials come from `credential_env` or a file store in `data_dir/cloud/`, written only through the local-only `CloudCredentialSet`/`CloudCredentialDelete` requests (`vca cloud login/logout`). Config rejects secret keys.
- **Tests:** wiremock only. No real speech, cloud or tunnel service was called, and no `tailscale` or `cloudflared` process was started on this machine.
- **Not done:**
  - Learning the personal lexicon from transcript edits (no protocol message yet).
  - Mapping plan approval to `Input::Choose`.
  - Persisting cloud sessions across restarts.
  - Whisper-local engine wiring.
  - Hidden-input `vca cloud login` on a real terminal.
  - Owner-only ACL of `data_dir/cloud/` on Windows (inherited from the per-user data directory, not checked).

### 3.6 Task 5: Tauri shells and the native transport

- **Layout:** `apps/Cargo.toml` is its own workspace (the main one excludes `apps`), so `cargo test --workspace` at the root needs no WebView libraries. `vca-shell` holds every command; `vca-desktop` and `vca-mobile` only call `vca_shell::run(generate_context!())`. Both embed `apps/ui/dist` (build the UI first; the `custom-protocol` feature serves it instead of the dev server).
- **Boundary:** the webview never sees keys, tokens or Noise. It calls `vca_pair`, `vca_connect` (two channels: link status and raw MessagePack server envelopes), `vca_send` (raw MessagePack body, `vca-machine` header; a JSON number array on Android), `vca_login`, `vca_forget`, `vca_status`, `vca_disconnect`, `vca_mic_start/stop/cancel` and `vca_platform`. `vca-client` owns pairing, the server key pin, refresh-token rotation (stored before `Connected` is reported) and reconnects. CSP allows only `'self'` and IPC.
- **Secrets:** OS keychain on desktop and iOS (probed at start; falls back to files in the app data directory), app-private files on Android.
- **Microphone:** cpal capture on a dedicated thread, downmixed and resampled to 16 kHz mono WAV, 240 s cap. `NSMicrophoneUsageDescription` and the macOS audio-input entitlement are set.
- **UI:** `TauriTransport`, a machine registry (dynamic machines, zero machines → pairing screen), pairing by pasted link, TOTP/backup-code login prompt, "Forget this machine", server transcription with the native recorder.
- **Verified on this Windows 11 PC** (release-like build driven through WebView2 DevTools, against a test vcad on 127.0.0.1): pairing through the form, `Connected` over Noise, live resources, a session card appearing with its preview and alert, text sent from the composer echoed by the fake agent, native mic capture (16 kHz mono WAV with signal), push-to-talk → `Transcribe` → the server's "speech is off" error shown in the composer, `Reconnecting…` → `Connected` after a vcad restart, revoke → `Disconnected` without retries, `vca_forget` removing the keychain entries.
- **Not done or unverified:**
  - Mobile builds: `gen/android` and `gen/apple` are not generated (`pnpm android:init` / `ios:init` need the SDKs). CI only runs clippy on the Android library.
  - Android microphone permission (`RECORD_AUDIO` runtime request) and an Android Keystore-backed secret store.
  - QR camera scanning (the button is disabled; links are pasted).
  - macOS/Linux desktop runs, signing and installers.
  - The UI shows `Disconnected` after a revoke without naming the reason.
  - Korean (and other) locale files translate only part of the UI; the rest falls back to English.

Still unverified:
- Codex `notify` installation: it only supports the global config.
- iTerm2 and macOS on real hardware.
- A real phone client on Android or iOS hardware.

## 4. Next steps (in order)

1. **Fix §3.1** and get CI green on all three platforms.
2. **Build `vca-server` and `vca-cli`** (spec in §6). Start from the drafts in `crates/vca-server/src/`.
3. **Wire `vca-net`** into the server as the remote transport (pairing, TOTP, Noise, resumable streams, kill switch, audit), replacing the dev-only WebSocket.
4. **Wire `vca-cloud`, `vca-providers` and `vca-speech`** into the server: cloud sessions as cards, provider selection and pairing QR endpoints, server-side transcription.
5. **Create the Tauri 2 shells** (`apps/desktop`, `apps/mobile`) around `apps/ui`. Implement the real `Transport` over the Noise channel and the microphone capture plugin.
6. **Run the M0 PoCs on real Windows and macOS machines** (development plan §4), including PoC-7 with real cloud accounts.

## 5. Local setup

```bash
git clone https://github.com/JinsongRoh/Voice-Command-AI.git
cd Voice-Command-AI
git checkout dev/m0-foundation

# Rust (stable, edition 2024)
cargo test --workspace

# UI
cd apps/ui
pnpm install --frozen-lockfile
pnpm check     # lint, format, typecheck, tests, build
pnpm dev       # mock-backed UI at http://localhost:5173

# Desktop shell (after `pnpm build` in apps/ui)
cd ../desktop
pnpm install
pnpm tauri dev  # or: cargo run --manifest-path ../Cargo.toml -p vca-desktop --features custom-protocol
```

Linux tests use `tmux` and `bash` when available; they skip themselves otherwise.

## 6. Spec for `vca-server` and `vca-cli`

**vcad (server)**
- **Config and logging:** load the config file (see `config.rs`) and set up logging with tracing.
- **Session manager:**
  - Managed sessions: adapter `launch_spec` plus a `vca-pty` session, an attach server per session, broker registration, and state from hook events, PTY exit and heuristics.
  - Observed sessions: from periodic `discover_all`.
  - Event log: server-assigned per-session `seq`, bounded replay, `StreamGap`.
  - Other: chat events from history tailing, unread counters, and `preview` maintained from the screen tail or the last message.
- **Command delivery:**
  - Tier 1 is `PtySession::send_input`; tier 3 is `terminal::inject`.
  - `Input::Choose` is mapped through the pending decision's option ids.
  - Reply with `InputAck{Queued}`, then push `InputAck{Delivered}`.
- **Control and launch:**
  - Pause, resume, stop and restart go through the broker and the PTY.
  - Launch only inside the configured workspace roots, otherwise `Forbidden`.
- **Resources:** `GetResources`, periodic `Resources` pushes, and reorder/cancel of queued jobs.
- **API core:** a transport-agnostic `Api` (`handle(ctx, Envelope<ClientMsg>)` plus a per-client outbound stream). It is exposed over:
  - a local control channel for the CLI (owner-only socket or named pipe, newline-delimited JSON);
  - an optional dev-only WebSocket on 127.0.0.1, off by default and protected by a printed random token.
- **Hook ingestion:** from `vca hook` over the local channel.
- **Graceful shutdown.**
- **Not yet wired:** answer the new `LaunchCloud`, `ListDevices`, `RevokeDevice` and `KillSwitch` messages with `Unsupported` until §4 step 3 and step 4.

**vca (CLI, clap)**
- **Daemon and sessions:** `ping [--quiet]`, `run -- <cmd> [args]` (launch, `attach_stdio`, exit with the agent's code), `attach`, `ls`, `send`, `key`, `stop`/`pause`/`resume`, `resources`.
- **Hooks:** `hook claude <event>` and `hook codex …`.
  - These **must fail open**: on any error, exit 0 with no output.
  - PreToolUse goes through broker admission; the slot is released on PostToolUse or Stop.
- **Setup and other commands:** `hooks install|uninstall|status`, `shell-init <shell>`, `daemon`, `version`.

**End-to-end test (Linux)**
1. Start vcad with temporary directories.
2. Launch a `vca-fake-agent` session through the API and send input without focus.
3. Receive output and message events, then answer a permission decision.
4. Simulate a `vca hook claude` call and check broker admission for a heavy command.
5. Stop the session.
6. Also test workspace-root rejection, and fail-open hooks while vcad is down.

## 7. Conventions

- **Language:** English only in code, comments, docs, commits and UI source strings. Translations live in `apps/ui/src/i18n/*.json` and `lexicon/<lang>/`.
- **Quality:** `cargo fmt`, clippy clean with `-D warnings`, tests for logic, no network in tests (use wiremock or local fakes).
- **Unsafe code:** denied at workspace level. Platform code that needs it lives in a small module with `#[allow(unsafe_code)]` and SAFETY comments.
- **Workspace hygiene:** do not run `cargo remove` (it can garbage-collect `[workspace.dependencies]`). When several agents work in parallel, give each its own crate and run cargo per package (`-p`).
- **Commits:** Conventional Commits.
