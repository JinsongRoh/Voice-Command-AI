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
| `apps/ui` | Done (protocol sync just finished) | 65 | React + TypeScript shared UI with a mock transport. Verify the "subscribe only to the selected session" change end to end. Tauri shells not created yet. |
| `vca-server` (`vcad`) | **Not started** beyond drafts | — | `config.rs`, `events.rs`, `workspace.rs`, `util.rs` are drafts that are **not yet in the module tree**. `main.rs` is still a stub. |
| `vca-cli` (`vca`) | **Not started** | — | Stub. |

## 3. Known issues

### 3.1 Windows: ConPTY cursor query blocks managed sessions (fixed)

ConPTY writes a cursor position query (`ESC[6n`) at startup and waits for an answer; portable-pty 0.9 hard-codes the inherit-cursor flag, so the query cannot be turned off without forking it. `vca-pty` now answers every `ESC[6n` itself (also when split across chunks) from the vt100 cursor at the point of the query, or `1;1` with screen tracking off (`SpawnOptions::answer_cursor_queries`, on by default). `attach_stdio` counts the queries it writes to the local terminal and drops that terminal's replies (`CprReplyFilter`, with a 2 s expiry so Ctrl+F3 is not swallowed later).

Found on the way, also fixed: portable-pty's Windows killer inverts `TerminateProcess`'s result (`PtySession::kill` ignores it on Windows); ConPTY drops bare line feeds inside bracketed paste (newlines are now sent as ``, as real terminals do); removing a paste marker could join its neighbors into a new marker (now stripped to a fixed point).

### 3.2 Other items
- `vca-adapters`: WezTerm, kitty, iTerm2 and Windows console injection are unverified on real machines.
- `vca-cloud`: confirm Jules, Cursor and Devin request/response shapes against their official docs (PoC-7).
- `vca-providers`: align `dnspilot/contract.rs` with the DNS Pilot VCA contract (`VCA-contract.md` in the DNS Pilot repository).
- macOS and Windows behavior of every crate has only been compile-checked or CI-tested; run the M0 PoCs on real machines.

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
