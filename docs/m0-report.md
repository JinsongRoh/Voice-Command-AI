# M0 Report: Proofs of Concept

Status of the seven M0 proofs of concept from the development plan (§4), what was measured on real machines, and what changes for M1. Date: 2026-10-29. Branch `dev/m0-foundation`.

**Machines**
- Windows 11 Pro workstation (shared development PC, 128 GiB RAM).
- MacBook Pro, Apple silicon, macOS 27.0.1, Xcode 27.1.
- iPhone 16 Pro Max, iOS 26.7.1.
- WSL2 Ubuntu 24.04 for Linux.
- GitHub Actions runners (ubuntu, macos, windows) for CI.

**Rules that limited what could run**
- No paid APIs: no real speech, cloud agent or tunnel accounts.
- No user-scope agent hooks on the shared PC.
- No real-process termination tests outside disposable CI VMs.

## Summary

| PoC | Result | Evidence |
|---|---|---|
| PoC-1 Managed PTY and shell hook | **Pass** on Windows and macOS. Linux runs in CI and WSL2. GNOME Terminal and iTerm2 not run by hand. | `vca run` with Claude Code 2.1.312 in Windows Terminal. Fake agent in a managed PTY on macOS with injection from another process. |
| PoC-2 Existing-terminal injection | **Partial** | tmux works (WSL2, CI). WezTerm, kitty, iTerm2 and Windows console are implemented but unverified. Windows Terminal has no focus-free path. |
| PoC-3 Agent signals | **Pass** for Claude Code and Codex input. **Partial** for Codex `notify`. | Real Claude Code hooks reached vcad (project-local install). Codex 0.153.4 launched and accepted input. Gemini CLI, Antigravity and Grok use the generic adapter with output heuristics. |
| PoC-4 Admission control | **Pass** | Live vcad with the real `vca hook claude` CLI and real `cargo build` runs. 1 slot: two agents serialized. 2 slots: the third agent waits for a free slot. |
| PoC-5 Connectivity | **Pass** for LAN and Tailscale (2 h each, with a server restart). Cloudflare and DNS Pilot not run. | 7199/7199 requests answered per path, reconnect 1.3–1.4 s. See §PoC-5. |
| PoC-6 Mobile audio | **Pass on iOS**, Android not built | iPhone push-to-talk → vcad → a self-hosted faster-whisper large-v3-turbo GPU server: Korean transcribed correctly, 9–14 s clips in about 0.3 s. |
| PoC-7 Cloud APIs | **Not run** | Needs the user's real accounts and spends their quota. Adapters are tested against mock servers only. |

## PoC-1 Managed PTY and shell hook

- **Windows 11, Windows Terminal (task 2):** `vca run -- claude` (Claude Code 2.1.312) and `vca run -- vca-fake-agent`.
  - Output renders cleanly. ConPTY's cursor query (`ESC[6n`) is answered by `vca-pty`, which was the hang fixed in task 1.
  - `vca send` types into the session with no window focused, and Claude Code answered.
  - Decisions are detected and answered, and exit codes reach the terminal.
- **macOS 27 (this report):** a loopback-only vcad launched `vca-fake-agent` in a managed PTY through the local API. `vca send` from another process delivered `hello from another process`, and the screen read back through `subscribe`:
  ```
  Fake Agent (vca-fake-agent) ready. | > hello from another process | Echo: hello from another process | Done. | >
  ```
  This was run over SSH, so a GUI terminal (Terminal.app or iTerm2) was not attached.
- **Linux:** the PTY and attach tests run in CI (ubuntu-latest) and in WSL2. GNOME Terminal was not run by hand.
- **Not verified:** the shell integration snippet (`vca shell-init`) that makes typing `claude` start a managed session, in iTerm2 and GNOME Terminal. Resize and colors were checked only in Windows Terminal.

## PoC-2 Existing-terminal injection

| Terminal | Result | How |
|---|---|---|
| tmux | Works | `send-keys`, tested in WSL2 and CI |
| WezTerm | Implemented, unverified | `wezterm cli send-text` |
| kitty | Implemented, unverified | `kitty @ send-text` (needs remote control enabled) |
| iTerm2 | Implemented, unverified | AppleScript |
| Windows Terminal or conhost | No focus-free path | Managed PTY (PoC-1) is the answer on Windows |

This matches the risk register: managed sessions through shell integration are the default, and injection into foreign terminals is a fallback.

## PoC-3 Agent signals

- **Claude Code:** real hooks, installed in a temporary project rather than user scope because the PC is shared.
  - SessionStart reached vcad and switched the session from heuristics to hooks (A/B checked against a project without hooks).
  - A Notification hook run through Git Bash produced a notification event.
  - Found and fixed: Git Bash rewrote `VCA_CONTROL=\\.\pipe\...`.
- **Codex 0.153.4:** launched through the API and accepted focus-free input.
  - Found and fixed: npm's extensionless `codex` script was picked instead of `codex.cmd`.
  - `notify` installation was not run, because Codex only reads a global `~/.codex/config.toml` and changing it would affect every Codex session on the machine.
- **Gemini CLI, Antigravity, Grok, Cursor CLI, Copilot CLI and others:** handled by the generic adapter (process detection plus output heuristics for prompts and waiting states). None of them has an integration hook beyond that yet.

## PoC-4 Admission control

**Setup**
- Live `vcad` on Windows, with `policies.toml` set to `max_heavy_jobs = 1`, then `2`.
- Each simulated agent sends Claude Code's PreToolUse payload through the real hook CLI (`vca hook claude PreToolUse`), runs a real `cargo clean && cargo build` on its own crate, then sends PostToolUse.
- No real agent was started.

One slot, two agents (ms from start):
```
  88 agent-A pre-hook sent
 547 agent-B pre-hook sent
 759 agent-A pre-hook answered: <no opinion>      (granted)
4442 agent-A cargo build finished
4579 agent-A post-hook sent
4598 agent-B pre-hook answered: <no opinion>      (granted 19 ms after A released)
7131 agent-B cargo build finished
```

Two slots, three agents:
```
 102 agent-A pre-hook sent        258 answered
 480 agent-B pre-hook sent        650 answered
 939 agent-C pre-hook sent
3158 agent-A post-hook sent      3158 agent-C answered
```

**Finding:** the hook fast path reads `--endpoint` only before `hook` (`vca --endpoint X hook claude PreToolUse`). Placed after, the hook falls back to the default endpoint and fails open silently. The installed hooks don't pass `--endpoint`, so this only affects hand-written hook commands. M1 should accept both positions, or say so in `vca hook --help`.

## PoC-5 Connectivity

**Setup**
- **Client:** Windows workstation running `crates/vca-client/examples/netprobe.rs`. It pairs once, then sends one `list_sessions` request per second over the real Noise channel and records round-trip times, requests lost in flight, and reconnect durations.
- **Servers:** two vcads on the Mac, one bound to its Tailscale address and one to its LAN address. Both were restarted at the 60-minute mark to simulate a relay or server restart.
- **Transport:** heartbeat 20 s (client) and the server default; WebSocket over HTTP/1.1; Noise XX.

| Path | Requests | Lost | RTT p50 | p95 | p99 | max | Reconnect after server restart |
|---|---|---|---|---|---|---|---|
| Tailscale (direct, same LAN) | 7199 | 0 | 10.0 ms | 284 ms | 454 ms | 796 ms | 1.26 s |
| LAN | 7199 | 0 | 5.5 ms | 258 ms | 436 ms | 781 ms | 1.36 s |

- Both 2-hour streams stayed up apart from the planned restart at 60 minutes. The client reported `Reconnecting`, then `Connected` again after one backoff step, using the rotated refresh token with no re-pairing. Two one-second ticks fell inside the outage; no request in flight was lost.
- The tail latency (p95 around 250–280 ms) is not network jitter alone. During the window the Mac built the iOS app several times and served the iPhone tests, and this PC ran workspace builds. A quiet rerun should give a cleaner p95; p50 shows the link itself adds only a few milliseconds.
- One-minute smoke runs before the soak: Tailscale p50 6.8 ms, LAN p50 51 ms. The LAN run was cold; it settled to 5.5 ms over two hours.

**Not run**
- **Cloudflare Tunnel:** a quick tunnel would publish this PC on the internet. That needs an explicit decision by the owner, who can run the same probe against a named tunnel.
- **DNS Pilot:** its API contract is still unpublished, and undocumented APIs are not used.

## PoC-6 Mobile audio

- **Capture:** `vca-shell` records with cpal on a dedicated thread, downmixes and resamples to 16 kHz mono 16-bit WAV, and stops itself at 240 s.
  - Verified in the Windows app: 1.38 s of audio with signal (peak 833).
  - Push-to-talk through the UI sent `Transcribe` over Noise, and the server's "speech is off" message showed in the composer.
- **iOS (iPhone 16 Pro Max, iOS 26.7.1):**
  - The app builds for the device (team signing, `--archive-only`), installs and pairs with a vcad on the Mac over Tailscale. The pairing link reached the app through `vca://` while it was running.
  - Text typed on the phone reached a managed session, and the agent's reply came back.
  - Push-to-talk recordings went through vcad to a self-hosted faster-whisper server (`large-v3-turbo`, fp16, CUDA, token-protected). Korean speech came back correct; 9–14 s clips took 0.27–0.32 s on the GPU.
  - cpal needs AudioToolbox and CoreAudio in `bundle.iOS.frameworks`, and `NSMicrophoneUsageDescription` is set.
- **Feedback from the phone test**, now implemented (`54fcc8b`, `6273347`):
  - Holding the button was tiring, so tap to start and tap to stop is now the default.
  - The user wanted to watch the transcript appear live. Dictation now streams: `vca_mic_peek` refreshes the text in progress about once a second, and `vca_mic_split` hands over a chunk at a pause every ~15 s for a final transcript. Each request carries the preceding text as `context`.
  - The input box was too small, so it now auto-grows and has an expand toggle.
  - The original transcript can be shown and restored next to the corrected one.
  - Still to do: LLM-based contextual correction, and automatic topic or domain detection that switches vocabulary hints.
- **Android:** not built (no SDK on the PC). CI type-checks the Android library. Runtime microphone permission (`RECORD_AUDIO`) still needs a small Kotlin plugin.
- **Engines:** paid cloud engines (OpenAI, Google) were not called. The OpenAI-compatible engine was used against a self-hosted Whisper server instead. Mixed Korean/English speech has not been measured yet.

## PoC-7 Cloud APIs

Not run. `vca-cloud` has adapters for Copilot, Claude Managed Agents, Claude Code routines, Codex cloud, Jules, Cursor and Devin. They are tested against mock servers built from the official documentation. Jules, Cursor and Devin field names are still unverified. Running the PoC needs the owner's accounts and quota: one create, list, follow-up and progress call per provider, recorded with the capability flags.

## Changes for the architecture and M1

1. **Remote stack:** keep Noise XX over WebSocket with refresh-token rotation. PoC-5 measured reconnects of 1.3–1.4 s after a server restart, with no manual re-pairing and no lost requests over 2 hours.
2. **Client in Rust:** keep the client in `vca-client` and expose it to the webview only through Tauri commands. Keys and tokens never reach JavaScript, and the CSP allows only IPC.
3. **Mobile:**
   - Add a camera QR scanner (a Tauri barcode plugin) and fix the cold-start `vca://` link on iOS (the warm path works).
   - Add an Android microphone-permission plugin and an Android Keystore secret store.
4. **Windows injection:** treat focus-free injection into foreign Windows terminals as out of scope. Managed sessions cover it.
5. **Hooks:**
   - Make the `vca hook` endpoint flag position-independent.
   - Offer a project-local hook install as the default on shared machines.
6. **Still open:**
   - PoC-7 with real accounts.
   - Android build and microphone permission (PoC-6).
   - Cloudflare and DNS Pilot runs of PoC-5.

## Estimates

M0 took longer than planned in calendar terms, but most of M1's core already exists:
- Session manager, API, local control, remote transport, pairing and TOTP.
- Shared UI and Tauri shells.
- Cloud, provider and speech wiring.

Remaining M1 work is mostly hardening and the gaps above, not new subsystems. The M1 estimate (about 8 weeks) can shrink by roughly 2–3 weeks. The mobile items in point 3 and PoC-7 are the largest unknowns.
