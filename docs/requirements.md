# Product Requirements

> **Status:** Draft v0.1. Under review; not final until development is approved.

## 1. Overview

### 1.1 Problem

Developers increasingly run several AI coding agent sessions in parallel on one workstation (Claude Code, Codex, Gemini CLI, Antigravity, Grok and others). Each session runs its own sub-agents, builds, tests and helper processes. This causes three problems:

| # | Problem | Effect |
|---|---|---|
| P1 | **Focus theft.** Newly spawned windows grab focus. | Prompts and keystrokes land in the wrong window, and the developer cannot issue commands. |
| P2 | **Uncoordinated resource use.** Every session acts as if it owned the machine. | CPU and memory saturate and the machine stalls or crashes. |
| P3 | **Navigation overhead.** Existing remote apps are organized per project. | Switching between sessions takes several screens for every command. |

### 1.2 Vision

A phone becomes the control surface for every agent on every machine you own. You see all sessions at a glance, speak a command to any of them, and let VCA keep the machine healthy.

### 1.3 Goals

- **G1.** Deliver commands to any agent session without needing window focus.
- **G2.** Show all sessions as one flat list of cards with live status.
- **G3.** Prevent resource exhaustion through admission control and active management.
- **G4.** Work securely from anywhere, including behind NAT and CGNAT.
- **G5.** Support any AI coding agent through adapters.
- **G6.** Be global from day one: multilingual UI and mixed-language speech.

### 1.4 Non-goals (v1.0)

- Replacing the agents themselves or their native UIs.
- Circumventing agent providers' usage limits or terms of service.
- Hosting a cloud backend that sees user code or prompts.

### 1.5 Target users

- Individual developers running several agent sessions locally.
- Developers who want to supervise long-running agent work while away from the desk.
- Small teams that share a build or dev server (one owner, several paired devices).

## 2. System components

| Component | Description |
|---|---|
| **VCA Server** | Long-running Rust daemon on the developer machine. It manages sessions, adapters, the resource broker, the speech pipeline, the API, authentication and the audit log. |
| **VCA Desktop** | Tauri 2 tray app and local dashboard for VCA Server: settings, pairing QR code, session overview and kill switch. |
| **VCA Mobile** | Tauri 2 app for iOS and Android: session cards, voice input, live view, notifications and machine switching. |
| **`vca` CLI** | Command-line entry point. It runs agents under VCA management, attaches terminals and administers the server. |
| **Connectivity providers** | Interchangeable plugins: DNS Pilot, Tailscale, Cloudflare Tunnel, direct LAN and custom. |
| **Push relay** | Relays end-to-end encrypted notifications to APNs and FCM (provided by DNS Pilot). |

## 3. Functional requirements

Priority levels: **M** = must-have for v1.0, **S** = should-have for v1.0. All items below are in scope for v1.0.

### 3.1 Session discovery and management (SES)

| ID | Requirement | Pri |
|---|---|---|
| SES-01 | Automatically detect running agent sessions of every supported agent (process inspection, session-store files and hook events). | M |
| SES-02 | Show each session as a card with project name, folder, agent type, account profile, state (running / idle / waiting for input / waiting for decision / error / paused), elapsed time and resource usage. | M |
| SES-03 | Let the user rename or alias a session, and pin or reorder cards. | S |
| SES-04 | Start a new session from mobile or desktop: pick an existing project folder or create a new one, pick an agent and an account profile, and launch. | M |
| SES-05 | Open a desktop terminal window attached to any managed session on request. | M |
| SES-06 | Stop, pause, resume and restart a session. | M |
| SES-07 | Resume a past session from the agent's history, where the agent supports it. | S |
| SES-08 | **Shell integration (default on, can be turned off):** sessions started by typing the plain agent command (for example `claude`) in any terminal are transparently run under VCA management. | M |

### 3.2 Command delivery (CMD)

| ID | Requirement | Pri |
|---|---|---|
| CMD-01 | Deliver a text command to the selected session without requiring window focus. | M |
| CMD-02 | Pick the best available delivery tier per session (see architecture §4): managed PTY, then the agent's native channel, then the terminal API. Simulated keystrokes are disabled by default. | M |
| CMD-03 | Show delivery confirmation: queued, delivered, acknowledged by the agent. | M |
| CMD-04 | Queue commands while the session is busy or the connection is down, and deliver them when possible. | S |
| CMD-05 | Command templates for frequent instructions (for example "run tests", "commit and open a PR"). | S |
| CMD-06 | Chained commands ("when this finishes, run …"). | S |
| CMD-07 | Send control keys (interrupt, escape, confirm) and pick options from agent menus. | M |

### 3.3 Voice input (VOC)

| ID | Requirement | Pri |
|---|---|---|
| VOC-01 | Selectable speech-to-text engine: local Whisper (on the server or on the device), OpenAI, Google Cloud and other providers through a plugin interface. | M |
| VOC-02 | Mixed-language recognition, with a configurable primary language and secondary languages. | M |
| VOC-03 | Editable transcript: the user reviews and corrects the text before sending. | M |
| VOC-04 | Listening modes: auto-listen while the app is in the foreground (voice activity detection ends the utterance), push-to-talk, and wake word (S). The microphone is released whenever the app leaves the foreground. | M |
| VOC-05 | **Developer vocabulary engine** (see §3.3.1). | M |
| VOC-06 | Intent routing: distinguish **control commands** ("pause session three") from **agent prompts** ("fix the login bug"), and ask for confirmation when the intent is ambiguous. | M |
| VOC-07 | Optional LLM clean-up pass (remove fillers, fix grammar) before showing the transcript. | S |
| VOC-08 | Address sessions by voice using their name, alias, folder or card number, in addition to the selected card. | S |

#### 3.3.1 Developer vocabulary engine

- **Layer 1, base lexicon:** a curated, open-source dictionary of technical terms per language. It includes spoken-form variants, for example "use effect" → `useEffect` and "kube control" → `kubectl`, plus localized pronunciations.
- **Layer 2, project lexicon:** automatically extracted from the active project (folder and package names, branches, frequently used identifiers and file names).
- **Layer 3, personal lexicon:** user-defined terms, plus terms learned from the user's own transcript corrections.
- **Conflict handling:** detect homophones and near-duplicates across layers and rank them by layer priority and context.
- Terms are applied in two places: as hints to the STT engine and as a post-recognition correction pass.

### 3.4 Live view (MON)

| ID | Requirement | Pri |
|---|---|---|
| MON-01 | Stream a session in real time. | M |
| MON-02 | Chat view (structured messages, tool calls, diffs) built from the agent's session data. | M |
| MON-03 | Raw terminal view (faithful terminal rendering). | M |
| MON-04 | Continue the conversation from the live view, by voice or text. | M |
| MON-05 | Automatically mask secrets (API keys, tokens, passwords) in streamed output. | M |
| MON-06 | Search and scroll back through session history. | S |

### 3.5 Notifications and decisions (NTF)

| ID | Requirement | Pri |
|---|---|---|
| NTF-01 | Detect task completion, waiting for input, permission requests and errors, using agent hooks where available and output heuristics otherwise. | M |
| NTF-02 | Show a badge on the session card and an in-app alert. | M |
| NTF-03 | Push notifications while the app is in the background or closed, through the push relay, end-to-end encrypted. | M |
| NTF-04 | Spoken notifications (text-to-speech) while the app is in the foreground, can be turned off. | M |
| NTF-05 | One-tap decisions: show the agent's options (approve / deny / choose) and answer directly from the notification or card. | M |
| NTF-06 | Per-session and per-event notification settings, plus quiet hours. | S |

### 3.6 Resource broker (RES)

| ID | Requirement | Pri |
|---|---|---|
| RES-01 | Monitor CPU, memory, disk I/O, network and (where available) GPU, both system-wide and per session process tree. | M |
| RES-02 | **Admission control:** before a heavy job (build, test, container start) runs, the agent hook asks the broker for a slot. The broker grants the slot, queues the job, or denies it with a reason. | M |
| RES-03 | Active management when thresholds are crossed: lower priority, throttle, pause child processes, or propose terminating them. | M |
| RES-04 | **Autonomous mode (default):** VCA decides automatically, using built-in policies and current load. | M |
| RES-05 | **User policies:** configurable limits and rules, for example "at most 2 concurrent heavy builds", "hold new sessions above 85% memory", "foreground session first". | M |
| RES-06 | Mobile resource panel: live graphs, queue of waiting jobs, and the ability to reorder, approve or cancel jobs (by voice too). | M |
| RES-07 | Pause and resume entire sessions, or child processes only (child-only is the default, to avoid agent API timeouts). | M |
| RES-08 | Alert before critical exhaustion and record history for later analysis. | S |

### 3.7 Orchestration (ORC)

| ID | Requirement | Pri |
|---|---|---|
| ORC-01 | Direct mode (default): commands go straight to the selected session. | M |
| ORC-02 | Orchestrator mode: an optional orchestrator agent session receives high-level instructions spanning several sessions and delegates work through VCA's session API. | S |
| ORC-03 | Show orchestration activity (which session asked which session to do what). | S |

### 3.8 Connectivity (CON)

| ID | Requirement | Pri |
|---|---|---|
| CON-01 | DNS Pilot provider: app-token tunnel and embedded P2P, developed in parallel. See [dnspilot-requirements.md](dnspilot-requirements.md). | M |
| CON-02 | Tailscale, Cloudflare Tunnel and direct LAN providers, supported on equal footing with DNS Pilot. The user chooses; the setup wizard recommends one based on diagnostics. | M |
| CON-05 | Custom provider: any user-supplied tunnel or VPN that forwards to the server's local port. The user is responsible for securing it; VCA's own authentication and end-to-end encryption still apply. | S |
| CON-03 | Connectivity diagnostics in the setup wizard (NAT type, reachability, recommended provider). | M |
| CON-04 | Automatic reconnection, with the stream resuming without data loss. | M |

### 3.9 Security (SEC)

| ID | Requirement | Pri |
|---|---|---|
| SEC-01 | Device pairing: scan a QR code shown by VCA Desktop. Only paired devices can connect. | M |
| SEC-02 | Two-factor authentication with TOTP (any RFC 6238 authenticator app). | M |
| SEC-03 | TLS on every connection, plus an end-to-end encrypted application channel between paired device and server. | M |
| SEC-04 | Short-lived access tokens with refresh tokens bound to the device. | M |
| SEC-05 | Workspace allow-list: sessions and project creation are limited to approved root folders. | M |
| SEC-06 | Step-up confirmation for sensitive actions (kill session, delete, raw shell, policy changes). | M |
| SEC-07 | Audit log of every connection and command, viewable on desktop and mobile. | M |
| SEC-08 | Alerts for new devices and new locations, and for failed authentication bursts. | M |
| SEC-09 | Kill switch: drop all remote connections and close the tunnel instantly, from desktop, mobile or the DNS Pilot console. | M |
| SEC-10 | Revoke a device and list active connections. | M |

### 3.10 Account profiles (ACC)

| ID | Requirement | Pri |
|---|---|---|
| ACC-01 | Register several legitimately owned agent accounts or API keys per agent as profiles (for example personal, work, API pay-as-you-go). | S |
| ACC-02 | Choose the profile when starting a session. Credentials stay isolated per profile. | S |
| ACC-03 | Show usage and limit status per profile where the agent exposes it, and alert as a limit approaches. | S |
| ACC-04 | Suggest a switch when a limit is reached. The **user confirms** every switch. | S |
| ACC-05 | Store credentials only in the OS keychain or secure storage. | M |

> Automatic rotation across accounts to evade provider usage limits is explicitly out of scope, because it would violate provider terms.

### 3.11 Multiple machines (DEV)

| ID | Requirement | Pri |
|---|---|---|
| DEV-01 | Pair several machines with one mobile app and switch between them. | S |
| DEV-02 | Aggregated view: alerts from all machines in one place. | S |

### 3.12 Desktop app (DSK)

| ID | Requirement | Pri |
|---|---|---|
| DSK-01 | Tray icon showing server status, connection status and alert count. | M |
| DSK-02 | Local dashboard with sessions, resources, policies, devices and audit log. | M |
| DSK-03 | Setup wizard covering agent detection, shell integration, connectivity provider, STT engine and pairing. | M |
| DSK-04 | An unobtrusive "Support the project" link in the About and Settings screens (desktop and mobile). No nag screens, pop-ups or feature gating. | S |

### 3.13 Internationalization (I18N)

| ID | Requirement | Pri |
|---|---|---|
| I18N-01 | UI localized through resource files. English is the source language; community translations are welcome. | M |
| I18N-02 | All code, comments, documentation and repository content are in English. | M |

## 4. Non-functional requirements

| Area | Requirement |
|---|---|
| Platforms | Server and desktop on Windows 10+, macOS 13+ and Linux (x86_64, arm64). Mobile on iOS 16+ and Android 10+. |
| Terminals | Windows Terminal, conhost, iTerm2, Terminal.app, GNOME Terminal, Konsole, WezTerm, kitty, Alacritty, tmux and zellij. |
| Latency | Command delivery in under 300 ms end to end on a normal connection, excluding STT. Live view lag under 500 ms. |
| Footprint | Idle server uses under 1% CPU and under 100 MB RAM. The mobile app holds no microphone or background CPU while not in the foreground. |
| Reliability | The server restarts sessions' management state after a crash, and managed agents keep running if the server restarts. |
| Privacy | No telemetry by default. Code and prompts never leave the user's machine except to the user's own devices and chosen STT provider. |
| Extensibility | Agent adapters, STT engines and connectivity providers are plugins with documented interfaces. |
| Distribution | Signed installers (MSI, DMG/PKG, deb/rpm/AppImage), Homebrew, winget and Cargo. Also available as a DNS Pilot Agent module. Mobile through the App Store and Google Play. |

## 5. Release plan

All features above ship in **v1.0**. Internal milestones:

| Milestone | Scope |
|---|---|
| **M0 – Proofs of concept** | Focus-free input delivery on each OS and terminal, session discovery for Claude Code and Codex, DNS Pilot tunnel round trip, Tauri 2 mobile audio capture. |
| **M1 – Core** | VCA Server, session cards, voice → edit → send, live view, notifications, pairing and TOTP. |
| **M2 – Resources and projects** | Resource broker, project creation, multiple machines, alternative connectivity providers. |
| **M3 – Intelligence** | Orchestrator mode, vocabulary learning, intent routing and LLM clean-up, account profiles, additional agent adapters. |
| **M4 – Release** | Wake word, localization, installers and store submissions, documentation → **v1.0** |

## 6. Open questions

1. DNS Pilot gaps G1–G10, especially app-token tunnel access (G1) and the embeddable P2P SDK (G2). See [dnspilot-requirements.md](dnspilot-requirements.md).
2. Exact native channels per agent (Codex app-server, Gemini CLI, Antigravity, Grok CLI). To be confirmed during M0.
3. Feasibility of focus-free input into existing Windows Terminal sessions. To be confirmed during M0.
4. Which usage data each agent exposes for account profiles.
