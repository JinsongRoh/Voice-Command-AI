# Prompt for the next Claude Code session

Paste the text below into Claude Code, started in the root of your local clone (branch `dev/m0-foundation`).

---

You are continuing development of Voice Command AI (VCA), an open-source, cross-platform tool for controlling AI coding agents (Claude Code, Codex, Gemini CLI, Antigravity, Grok, cloud agents) from a phone by voice. The project is written in Rust (Cargo workspace), with a shared React/TypeScript UI that will be wrapped in Tauri 2 for desktop and mobile.

**Read first, in this order:**
1. `docs/handoff.md`: current state, known issues and next steps.
2. `docs/development-plan.md`
3. `docs/architecture.md`
4. `docs/requirements.md`
5. `docs/dnspilot-requirements.md`

**Rules:**
- Work on branch `dev/m0-foundation`. Draft PR #2 tracks it; keep CI green on Linux, macOS and Windows.
- English only in code, comments, docs and commits. Global audience; nothing locale-specific outside translation files.
- Before every commit, run `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `pnpm check` in `apps/ui` if the UI changed.
- Do not run `cargo remove`. Keep `[workspace.dependencies]` intact.
- This machine is a real development PC. Use it to verify what the cloud session could not: real terminals (Windows Terminal, iTerm2 or others), real Claude Code and Codex installations, and platform-specific code paths. Report what you verified and what remains unverified.
- Never use undocumented provider APIs. Never weaken security defaults (pairing, TOTP, Noise end-to-end encryption, workspace allow-list, fail-open hooks).
- You may split independent work across parallel sub-agents. Give each one its own crate or directory and have it run cargo per package (`-p`).

**Tasks, in order** (details in `docs/handoff.md` §3, §4 and §6):
1. Fix the Windows ConPTY cursor-query hang in `crates/vca-pty` (handoff §3.1) and get CI green on all three platforms.
2. Implement `vca-server` (`vcad`) and `vca-cli` (`vca`) per handoff §6, including the Linux end-to-end test with `vca-fake-agent`. Then test `vca run -- claude` in a real terminal on this machine.
3. Wire `vca-net` into the server as the remote transport, replacing the dev-only WebSocket.
4. Wire `vca-cloud`, `vca-providers` and `vca-speech` into the server.
5. Create the Tauri 2 desktop and mobile shells around `apps/ui`, with a real `Transport` over the Noise channel.
6. Run the M0 proofs of concept on this machine (development plan §4) and write the M0 report.

Start with task 1. Before writing code, give me a short plan for tasks 1 and 2. After each task, commit, push and summarize what changed, how it was verified, and what is still unverified.
