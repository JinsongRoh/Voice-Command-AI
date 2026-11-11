# Changelog

All notable changes to Voice Command AI are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-11-11

The desktop app now also runs on Linux. Windows and macOS get the same version with the fixes below; installed 0.1.0 apps offer the update in Settings.

**Linux desktop app**

- The desktop app for Linux, x86_64 and ARM64, as a `.deb` (Debian, Ubuntu) and an `.rpm` (Fedora). Like the Windows and macOS apps it includes the VCA server (`vcad`) and the `vca` command, and updates itself from GitHub Releases. The packages need Ubuntu 22.04, Debian 12, Fedora 36 or newer.
- A manual GitHub Actions workflow (`release-linux.yml`) that builds the unsigned Linux packages for both architectures; the updater signatures are added afterwards by the release team. See [Releasing](docs/releasing.md).

**Fixed**

- macOS: pairing a phone from the `vca` command could not finish, because the server could not tell which local program was asking (macOS reports no process id in the socket credentials). It now reads it the macOS way.
- Closing the chat search right after starting **Search earlier messages** now always stops loading earlier pages.
- Linux: the folder that holds the app's keys when no Secret Service is available is now readable only by your user (it was created with the default permissions).
- Linux: session discovery now also finds agents whose executable name is longer than 15 characters (Linux reports only the first 15).
- Linux packages: the `.deb` and `.rpm` declare the ALSA library the microphone needs, and the app ships 64×64 and 512×512 icons.

## [0.1.0] - 2026-11-11

The first public release. The desktop app for Windows 10/11 x64 (NSIS installer signed by MetaVu Co., Ltd.) and macOS (Apple silicon and Intel, Developer ID signed and notarized) is on GitHub Releases, with in-app updates; Linux packages come in a later version. The iPhone/iPad app and the Android app are in App Store and Google Play review. Walkthrough video: https://youtu.be/y1nxSgDrQi8.

### Added

**Server, CLI and sessions**

- `vcad`, a server on your PC that owns agent sessions in managed pseudo-terminals, and the `vca` command (`vca run`, `vca pair`, `vca daemon`, shell integration for bash, zsh, fish and PowerShell).
- Session cards for Claude Code, Codex and other terminal agents (Gemini CLI, Antigravity, Grok, Cursor CLI, Copilot CLI and more through a generic adapter), with status, alerts and sub-agent counts.
- Discovery of agent sessions that are already open, and **Bring in** to list the AI sessions running in other terminal windows (with the reason when one can't be brought in).
- New sessions from the app with a project folder picker: recent projects, browsing inside your workspace roots (Git repositories marked, filter, keyboard navigation) and new folders.
- Cloud agent sessions through official provider APIs: Jules, Cursor Cloud, Devin, Copilot (cloud), Claude Managed Agents, Claude Code Routines and Codex Cloud (`[[cloud.accounts]]`, `vca cloud`).
- **Remove from VCA** for every kind of session: ended sessions go at once, sessions VCA started are stopped and removed after one confirmation, and terminal-window and cloud sessions are ended first (a cloud session is stopped, then removed).
- Answerable question forms: an agent's multi-question form (Claude Code) answered from the app with one **Submit**, typed into the form only after it shows on screen, with **Send again** when delivery is not confirmed; a question is never answered twice.
- Live view as a readable chat or the raw terminal, with search and paged history.
- Resource broker: per-session CPU, memory, disk and network, with admission control for heavy builds and tests.
- Files, photos and folders sent to a session's project folder (`.vca/attachments/`).

**Voice**

- Live dictation with your own speech-to-text engine (any OpenAI-compatible server, OpenAI or Google Cloud), developer-vocabulary correction and an original-transcript view.
- Optional context correction and topic detection with your own LLM.
- Hands-free mode with the **"Voco"** wake word: say "Voco", dictate, end with "over" and cancel within 3 seconds; session switching and a status briefing by voice (off by default).
- Weak-network handling: a recording that doesn't reach the PC is kept and can be sent again with **Retry**.
- **Commonly misheard words**: your own corrections and hints for the speech engine, kept on the PC and used by every connected device.
- A pronunciation dictionary for spoken alerts (typed or taught by voice), shared by the PC's voice and every device.
- Speech engines (STT, correction LLM, TTS) set up and tested from the desktop app, with presets. See [Your own speech engines](docs/speech-engines.md), [Your own Whisper server](docs/whisper-server.md) and the [TTS engine benchmark](docs/tts-benchmark.md).

**Alerts**

- An Alerts menu with a count badge for every session waiting for a decision or input.
- Exact approval alerts through optional agent hooks for Claude Code and Codex, installed, repaired and removed from the desktop app; inferred approval alerts without hooks.
- Spoken alerts through the device voice or your own OpenAI-compatible TTS engine, a spoken status briefing, and opt-in spoken result summaries.
- One voice at a time, in the voice you chose: when a TTS engine on the PC is selected, everything spoken (alerts, briefings, summaries, hands-free read-backs) uses that voice; the device voice is only a fallback before any PC audio starts.
- Desktop system notifications while the app is in the background.

**Command Center (the conductor)**

- An opt-in AI session (Claude Code, Codex CLI, Gemini CLI or a built-in loop with your own LLM) that oversees the other sessions through VCA's MCP server (`vca mcp`). See [The conductor](docs/conductor.md).

**To-do list**

- Requests that agents need you for, collected from the chat, from agents through MCP (`request_human_action`) or added by hand; reminders, snooze, export and sending the result back to the session.
- Markdown and CSV exports on phones open the system share sheet and are also copied to the clipboard.

**Windows back after a restart**

- A saved list of AI CLI windows that VCA reopens at the same place and in the same conversation after a reboot, on Windows, macOS, Linux and tmux; import of `layout.json`.
- Screen layout: arrange open terminal windows in a grid on each monitor (1 to 10 cells or your own rows and columns, auto fill, undo), save layouts, apply them or load them into the grid, rename and delete them; a single saved layout loads into the grid by itself.

**Apps and connectivity**

- Desktop app for Windows, macOS and Linux with a bundled `vcad` and `vca`, and an in-app updater that checks GitHub Releases, verifies the signature and installs only when you confirm. See [Releasing](docs/releasing.md).
- **Start VCA when I sign in** on the desktop (off by default).
- iPhone/iPad and Android apps with a phone UI: session carousel, attachments, light, dark and black themes, for the App Store and Google Play. Phones show alerts while the app is open; notifications while it is closed are not part of this release.
- A demo mode in the phone app: **Try the demo** on the first screen shows sample computers and sessions without any network connection; **Exit demo** leaves it.
- Step-by-step phone pairing with one QR code: a step card shows scanning the PC's QR code, remote access registration (when the code carries a DNSPilot invite), the 2-step verification code (only if the PC asks for one) and connecting. One QR code from **Settings › Connection & security › Connect a phone** registers the phone for remote access and pairs it; **Reconnect DNSPilot** on the PC when the code can't carry an invite. DNSPilot sign-in and the console QR fallback are under **Advanced** on the phone.
- The Android app's native library is aligned for 16 KB memory pages.
- App lock: the apps open locked and unlock with Windows Hello, Touch ID, Face ID, fingerprint or the device passcode, and lock again after 1 to 60 minutes without use.
- A diagnostic log on each device (off by default, kept for 7 days, never uploaded) that you can copy and send yourself.
- Connectivity providers: direct LAN, Tailscale, Cloudflare Tunnel, DNSPilot (app-mode tunnel) or your own.
- Pairing several PCs with one app.
- App languages: English, Korean, Japanese and Spanish, following the device language (English otherwise); the iOS permission prompts are localized too.

### Security

- One-time pairing codes (valid for 60 seconds) with a QR code, and TOTP two-factor sign-in with backup codes.
- End-to-end Noise (`XX`) encryption between each paired device and `vcad`; connectivity providers are treated as untrusted transports.
- Per-device permissions (view, command, admin), short-lived tokens, device revocation and a kill switch.
- Workspace allow-list (`workspace_roots`) for new sessions, remote shells, window restore and attachments.
- Remote shell limited to admin devices, with a fresh authenticator code, a danger check for each command, step-up codes for dangerous commands and automatic time limits; no administrator rights.
- The conductor never answers permission decisions and sends nothing to another session without your confirmation; actions are capped per hour and audited.
- Agent hooks are opt-in, back up the files they change and fail open.
- Desktop updates are verified against the app's minisign public key before they are installed, and nothing is installed until you click.
- Audit logs on the PC: connection and sign-in attempts, and an activity log (`audit.jsonl`) for remote shells, the conductor, window restore and arrangement, and to-do changes. Messages sent to sessions are recorded by length only, never their text; authenticator codes are never written and secrets in commands are masked.
- Arranging windows from a paired device needs an authenticator code (or a recent step-up), like a remote window restore.
- No analytics, tracking or crash reporting; the diagnostic log is opt-in and stays on the device. Self-hosted use sends MetaVu no personal data; optional DNSPilot is the only VCA-team service that receives data. See the [privacy policy](https://vca.metavu.io/privacy).
- Device keys and tokens kept in the OS credential store in the desktop and iOS apps.
- A local verification approves one request, chosen by the server from that request; the grace window is 5 minutes at most and only covers flows of several requests. See [Security setup](docs/security-setup.md), including what this does not stop.
- New backup codes and a new place for mail need the authenticator as well as the approval. Recovery needs two separate proofs and opens one operation. On computers with no OS verifier a backup code can start recovery, and the first authenticator needs a person's confirmation.
- Hook events are tied to their session, and an event about one tool call no longer clears the decision of another.
- Text that goes out on its own (voice, the conductor, task results) cannot answer an open permission prompt, and a spoken approval only works for short read-only commands from the server's list.
- A confirm code on the computer and the phone shows a forged pairing link before any code is entered.
- Failed sign-ins from an address no longer lock out paired devices, and connections that have not signed in have their own slots.
- Stored engine keys only go to the address they were saved with, over https or on this computer, and never follow a redirect to another host.

[0.1.0]: https://github.com/JinsongRoh/Voice-Command-AI/releases/tag/v0.1.0
