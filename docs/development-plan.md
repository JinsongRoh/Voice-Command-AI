# Development Plan

> **Status:** Draft v0.1. To be finalized after the DNS Pilot questionnaire is answered. Companion to [requirements.md](requirements.md) and [architecture.md](architecture.md).

## 1. Scope and assumptions

- **Scope:** everything in the requirements document ships in **v1.0**, built through internal milestones M0–M4.
- **Team assumption:** a small core team working with AI coding agents. The durations below are planning estimates for that setup and will be recalibrated after M0.
- **Rule for DNS Pilot:** no milestone is blocked by DNS Pilot work. Each milestone validates connectivity through the provider abstraction with Tailscale, Cloudflare Tunnel and direct LAN, and DNS Pilot features are integrated as they land.

## 2. Technology baseline

| Area | Choice |
|---|---|
| Language | Rust (stable, edition 2024), with a minimum supported Rust version pinned in `rust-toolchain.toml` |
| Async runtime | Tokio |
| Server API | axum with WebSocket support, and MessagePack via `rmp-serde` |
| End-to-end encryption | Noise `XX` via `snow` |
| Pseudo-terminals | `portable-pty` (ConPTY on Windows, openpty on Unix), with a terminal-state parser for the raw view |
| Process and resource metrics | `sysinfo`, plus platform-specific code (Job Objects, cgroups v2, `taskpolicy`) |
| Local storage | SQLite via `sqlx` (sessions, audit log, settings) |
| Secrets | `keyring` (OS keychain) |
| Speech | `whisper-rs` (local), plus HTTP clients for cloud STT providers |
| Apps | Tauri 2 (desktop and mobile) with a shared TypeScript UI (framework chosen during M0) |
| Testing | `cargo test`, `insta` snapshot tests, integration tests with fake agents, Playwright for the UI |
| CI | GitHub Actions running a matrix of Windows, macOS and Linux, plus Android and iOS builds |

## 3. Workstreams

| Workstream | Crates and apps | Covers |
|---|---|---|
| **W1 Core and sessions** | `vca-core`, `vca-server`, `vca-pty`, `vca-cli` | Daemon, session model, managed PTY, shell integration, command delivery tiers |
| **W2 Agent adapters** | `vca-adapters` | Local: Claude Code, Codex, Gemini CLI, Antigravity, Grok, Cursor CLI, Copilot CLI, Aider, opencode, Qwen Code, generic. Cloud: Jules, Cursor Cloud Agents, Devin, Copilot cloud agent, Claude Managed Agents, Claude Code routines, Codex cloud |
| **W3 Resource broker** | `vca-broker` | Metrics, admission control, policies, containment |
| **W4 Speech** | `vca-speech`, `lexicon/` | STT engines, vocabulary engine, intent routing, TTS |
| **W5 Network and security** | `vca-net` | Providers, Noise channel, pairing, TOTP, tokens, audit, kill switch, push |
| **W6 Apps** | `apps/desktop`, `apps/mobile`, `apps/ui` | Cards, voice input, live view, notifications, resource panel, settings, wizard |
| **W7 Release engineering** | `.github/`, `packaging/` | CI, signing, installers, stores, DNS Pilot module, documentation site |

## 4. Milestones

### M0 – Proofs of concept (about 3 weeks)

Goal: retire the highest technical risks before building the product.

| PoC | Question to answer | Exit criteria |
|---|---|---|
| PoC-1 Managed PTY and shell hook | Can `claude` and `codex` run transparently under a VCA-owned PTY on all three OSes, with an attached terminal that feels native? | Typing `claude` in Windows Terminal, iTerm2 and GNOME Terminal starts a managed session. Text can be injected from another process with no window focused. Resize and colors work. |
| PoC-2 Existing-terminal injection | Can text be delivered to sessions **not** started by VCA? | Results table per terminal (tmux, WezTerm, kitty, iTerm2, Windows Terminal or conhost), each marked works, partial or no. |
| PoC-3 Agent signals | Can completion, waiting-for-input and permission requests be detected reliably? | Claude Code hooks and Codex `notify` produce events into the PoC daemon. The integration paths for Gemini CLI, Antigravity and Grok are documented. |
| PoC-4 Admission control | Can a PreToolUse hook hold a heavy command until the broker grants a slot? | Two agents running `cargo build` concurrently are serialized by the broker with a 2-slot policy. |
| PoC-5 Connectivity | Does a WebSocket stream (HTTP/1.1, 30 s heartbeat, resumable) survive each provider, including relay restarts? | 2-hour stream with p95 latency and reconnection time measured over Tailscale, Cloudflare Tunnel and LAN. Also measured over DNS Pilot P2P, via its existing Android binding, and over a DNS Pilot tunnel once `app` mode is available. |
| PoC-6 Mobile audio | Can a Tauri 2 mobile app capture audio with VAD and stream it? | Android and iOS builds capture and transcribe one utterance (local Whisper on the server and one cloud engine), including mixed-language speech. |
| PoC-7 Cloud APIs | Do the official cloud agent APIs behave as documented? | For each provider in the architecture matrix: create, list, follow-up and progress verified with a real account, with the capability flags confirmed. |

**Deliverable:** an M0 report that updates the architecture document and confirms or revises the estimates.

### M1 – Core (about 8 weeks)

- **W1:**
  - Production daemon with persistent session registry.
  - Shell integration installer (bash, zsh, fish, PowerShell).
  - Delivery tiers 1–3, with delivery acknowledgements.
- **W2:**
  - Claude Code and Codex local adapters (discovery, events, launch, hooks).
  - Cloud adapter trait, plus the Jules, Cursor Cloud Agents and Devin adapters (full capability APIs).
- **W4:** STT engine trait with local Whisper and one cloud engine, editable transcript, base lexicon for English plus 3 additional languages.
- **W5:**
  - LAN, Tailscale and Cloudflare Tunnel providers.
  - QR pairing, TOTP, short-lived tokens, Noise channel.
  - Audit log and kill switch.
- **W6:**
  - Mobile session cards, voice → edit → send, chat-style live view and raw terminal view.
  - In-app alerts and one-tap decisions.
  - Desktop tray, dashboard and setup wizard.
- **W7:** CI matrix, nightly unsigned builds.

**Exit criteria:** from a phone on a cellular network, the user can see all Claude Code and Codex sessions on a paired machine, send a spoken command to any of them, watch the result live, and answer a permission prompt.

### M2 – Resources and projects (about 6 weeks)

- **W3:**
  - Monitoring and admission control (autonomous mode plus user policies).
  - Pause and resume of child processes and whole sessions.
  - Mobile resource panel with a reorderable queue.
- **W1:** New project creation and session launch from mobile. Resume of past sessions.
- **W5:**
  - DNS Pilot provider (tunnel and/or P2P, depending on DNS Pilot progress).
  - Push notifications through the DNS Pilot relay.
  - Multiple machines.
- **W6:** Multi-machine switcher, notification settings and quiet hours.

**Exit criteria:** with four agents running heavy builds on an 8-core / 16 GB machine, the machine stays responsive (memory never above the policy limit) and the queue is visible and controllable from mobile.

### M3 – Intelligence and breadth (about 6 weeks)

- **W2:**
  - Gemini CLI, Antigravity and Grok adapters, plus the generic adapter.
  - Cloud adapters for the Copilot cloud agent, Claude Managed Agents, Claude Code routines and Codex cloud (CLI).
  - Direct mode (cloud adapters running in the mobile app).
  - Adapter SDK documentation for community contributions.
- **W4:**
  - Project and personal lexicon layers, learning from transcript corrections, conflict handling.
  - Intent routing (control commands vs. prompts) and optional LLM clean-up.
- **W1 / ORC:** Orchestrator mode.
- **W5:** Account profiles (isolation, usage display, user-confirmed switching).

**Exit criteria:**
- Mixed-language commands containing project-specific identifiers are transcribed correctly in at least 95% of a 200-utterance test set.
- One instruction can coordinate two sessions through orchestrator mode.

### M4 – Release (about 5 weeks)

- Wake word (foreground only).
- UI localization framework with initial translations.
- Signed installers (MSI, DMG/PKG, deb, rpm, AppImage), Homebrew, winget, crates.io and the DNS Pilot Agent module.
- App Store and Google Play submissions.
- Documentation site, security policy (`SECURITY.md`), contribution guide, code of conduct.
- External security review of pairing, authentication and the Noise channel.
- Public beta, then **v1.0**.

**Indicative total:** about 28 weeks from the start of M0.

## 5. Engineering conventions

- **English only:** all code, comments, commit messages, issues and documentation.
- **Commits and branches:** Conventional Commits; one feature branch per change; pull requests with required CI.
- **Code quality:** `rustfmt`, `clippy -D warnings` and `cargo deny` (licenses and advisories) in CI.
- **Security-sensitive crates** (`vca-net`, the authentication parts of `vca-server`) need a second reviewer and a dedicated test suite.
- **Releases:** semantic versioning. Releases are built by CI only, signed and reproducible where the platform allows it.
- **Privacy:** no telemetry by default. Any optional diagnostics are opt-in and documented.

## 6. Testing strategy

| Level | Approach |
|---|---|
| Unit | Per crate, run on all three OSes in CI. |
| Fake agents | A scriptable fake agent binary that mimics Claude Code and Codex output, hooks and prompts, so session, notification and broker logic can be tested without real accounts. |
| Integration | Daemon and fake agents and a headless client over every provider available in CI (LAN, and a Tailscale or Cloudflare test tunnel where secrets are available). |
| UI | Playwright against the shared UI, plus device testing on real Android and iOS hardware before each milestone exit. |
| Speech | Versioned multilingual utterance corpus with word-error-rate tracking per engine and language. |
| Resource | Load scenarios (parallel builds, memory hogs) with pass/fail criteria for responsiveness. |
| Security | Fuzzing of protocol parsers, pairing and token tests, external review before v1.0. |

## 7. Dependencies on DNS Pilot

| DNS Pilot item | Needed by | If late |
|---|---|---|
| `app` access mode, OAuth device flow, local REST API, 24-hour proxy timeout | M2 | Ship the DNS Pilot provider later; the other providers are unaffected. |
| Redistributable P2P mobile binding (Android and iOS) | M2 | DNS Pilot P2P stays out of release builds; tunnel and other providers remain. |
| Push relay | M2 | In-app notifications only, or a temporary self-hosted push option. |
| Delegated P2P grants | M3 | The phone signs in to the user's DNS Pilot organization instead. |
| Code-signed and notarized Agent, Agent module support, international payments, API usage policy | M4 | Standalone installers remain the primary distribution; the DNS Pilot provider is labeled preview. |

See [dnspilot-requirements.md](dnspilot-requirements.md) for the full gap list.

**Build rule:** the DNS Pilot P2P client is a separately licensed binary. It is included only behind a build feature flag, and the default open-source build compiles and runs without it.

## 8. Risk register

| Risk | Likelihood | Impact | Response |
|---|---|---|---|
| Focus-free injection into existing Windows Terminal sessions is not possible | Medium | Medium | Managed PTY via shell integration is the default anyway. Document the limitation. |
| An agent changes its hooks, CLI or session format | High | Medium | Version-detected adapters, fake-agent tests, community maintainers per adapter. |
| Tauri 2 mobile limitations (audio, background, push) | Medium | High | PoC-6 in M0. Native plugins where required. |
| App store review rejects remote-control functionality | Low | High | Clear privacy disclosures. The app controls only the user's own paired machines. |
| Resource pausing breaks agent requests | Medium | Medium | Pause child processes by default. Pause whole sessions only on explicit request. |
| Scope of v1.0 is large | High | High | Milestone exit criteria, plus re-planning after M0 with real velocity data. |

## 9. Next steps

1. Fold in the DNS Pilot answers and finalize the requirements, architecture and this plan.
2. Approve the plan.
3. Start M0: create the Cargo workspace skeleton, CI, and the seven PoCs.
