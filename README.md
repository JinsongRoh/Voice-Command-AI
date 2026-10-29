<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/JinsongRoh/Voice-Command-AI/dev/m0-foundation/docs/brand/logo/02_lockups/horizontal/VCA_logo_horizontal_dark-transparent.svg">
    <img src="https://raw.githubusercontent.com/JinsongRoh/Voice-Command-AI/dev/m0-foundation/docs/brand/logo/02_lockups/horizontal/VCA_logo_horizontal_light.svg" alt="Voice Command AI" width="420">
  </picture>
</p>

<p align="center"><strong>Your voice. Every agent. One command center.</strong></p>

# Voice Command AI

**Control every AI coding agent on your machine from your phone, with your voice.**

Voice Command AI (VCA) is an open-source, cross-platform tool. It lets you watch, command and orchestrate AI coding agents running on your development machine. Supported agents include Claude Code, OpenAI Codex, Gemini CLI, Google Antigravity, Grok and more. You can reach them from anywhere, without fighting for keyboard focus and without letting runaway builds bring your machine down.

> Development happens on the [`dev/m0-foundation`](https://github.com/JinsongRoh/Voice-Command-AI/tree/dev/m0-foundation) branch until the first release is merged here.
>
> **Status:** Pre-release (milestone M0 complete, October 2026). The server, CLI, shared UI and Tauri desktop/mobile apps run end to end: pairing over Noise, live session cards, voice dictation, resource admission. There are no installers or store builds yet; see the [user guide](https://github.com/JinsongRoh/Voice-Command-AI/blob/dev/m0-foundation/docs/user-guide.md) to build and run it, and the [M0 report](https://github.com/JinsongRoh/Voice-Command-AI/blob/dev/m0-foundation/docs/m0-report.md) for what was verified.

---

## Why

Modern development often means running many agent sessions in parallel. Each session spawns its own sub-agents, compilers, test runners and helper windows. On a single workstation this quickly causes three problems:

- **Focus theft.** New windows constantly steal focus, so prompts and keystrokes land in the wrong terminal.
- **Resource exhaustion.** Agents launch builds and tests with no awareness of each other until CPU and memory are saturated and the machine stalls.
- **Navigation overhead.** Existing remote-control apps make you drill into one project, go back, and drill into another for every command.

VCA fixes this by putting a lightweight server on your machine. That server owns every agent session and exposes the sessions to a mobile app as a simple deck of cards.

## Highlights

- **Session cards.** Every running agent session on your machine is listed as a live card showing project, agent, status and alerts.
- **Voice first.** Pick a card, speak, review the transcript, send. Mixed-language speech (for example English mixed with Japanese, Spanish or Korean) is supported. A developer-vocabulary engine improves recognition of technical terms.
- **No focus required.** Commands are delivered straight to the target session through a virtual terminal or the agent's native channel, never through simulated keystrokes.
- **Live view.** Stream any session in real time as a readable chat view or the raw terminal, and keep the conversation going from there.
- **Actionable notifications.** Get text and spoken alerts when a session finishes, needs input or asks for a decision. Answer with one tap.
- **Resource broker.** CPU, memory, disk and network are monitored per session. Heavy jobs are admitted, queued, throttled or paused, automatically or by your own policies.
- **Projects anywhere.** Create a project folder and start a new agent session on your machine from your phone.
- **Multi-agent orchestration** *(coming soon)*. Coordinate several sessions on one task through an optional orchestrator session.
- **Multiple machines.** Pair your home, office and server machines with one app.
- **Secure remote access.** Device pairing via QR code, TOTP two-factor authentication, short-lived tokens, end-to-end encryption, audit log and a kill switch.
- **Your choice of connectivity.** Pluggable providers: Tailscale, Cloudflare Tunnel, direct LAN, or bring your own; DNSPilot (tunnel or P2P) *coming soon*.
- **Pluggable speech-to-text.** Use any OpenAI-compatible server (for example a self-hosted Whisper server), OpenAI or Google Cloud; live dictation streams as you speak.
- **Account profiles.** Keep several legitimately owned agent accounts or API keys side by side, see usage at a glance, and choose which profile a session uses.

## Architecture at a glance

```
┌────────────────┐     encrypted tunnel      ┌──────────────────────────────┐
│  VCA Mobile    │◄─────────────────────────►│  VCA Server (your machine)   │
│  (Tauri 2)     │   (DNSPilot / Tailscale  │  ├─ Session manager (PTY)    │
│  cards · voice │    / Cloudflare / LAN)    │  ├─ Agent adapters           │
│  live view     │                           │  ├─ Resource broker          │
└────────────────┘                           │  ├─ Speech pipeline          │
                                             │  └─ Auth · audit · API       │
                                             └──────────────┬───────────────┘
                                                            │
                                     Claude Code · Codex · Gemini CLI · Antigravity · Grok · …
```

The core is written in Rust. The desktop and mobile apps are built with [Tauri 2](https://tauri.app) and run on Windows, macOS, Linux, iOS and Android.

## Documentation

- [User guide](https://github.com/JinsongRoh/Voice-Command-AI/blob/dev/m0-foundation/docs/user-guide.md)
- [M0 report](https://github.com/JinsongRoh/Voice-Command-AI/blob/dev/m0-foundation/docs/m0-report.md)
- [Brand definition](https://github.com/JinsongRoh/Voice-Command-AI/blob/dev/m0-foundation/docs/brand/brand-definition.md) and [logo files](https://github.com/JinsongRoh/Voice-Command-AI/blob/dev/m0-foundation/docs/brand/logo/README.md)
- [Product requirements](https://github.com/JinsongRoh/Voice-Command-AI/blob/dev/m0-foundation/docs/requirements.md)
- [Architecture](https://github.com/JinsongRoh/Voice-Command-AI/blob/dev/m0-foundation/docs/architecture.md)
- [Development plan](https://github.com/JinsongRoh/Voice-Command-AI/blob/dev/m0-foundation/docs/development-plan.md)
- [DNSPilot integration requirements](https://github.com/JinsongRoh/Voice-Command-AI/blob/dev/m0-foundation/docs/dnspilot-requirements.md)

## Supporting the project

Voice Command AI is free and open source, and every feature works without paying anyone. If you want to support its development, you can:

- **Consider [DNSPilot](https://dnspilot.kr) for connectivity** once VCA's DNSPilot integration ships (console: [dns.dnspilot.kr](https://dns.dnspilot.kr)). DNSPilot is developed by the same team that maintains VCA; once its paid plans open, subscriptions will help fund VCA's development. It is entirely optional: Tailscale, Cloudflare Tunnel, direct LAN and custom providers are supported on equal footing.
- **Contribute** code, agent adapters, translations or vocabulary for your language.
- **Star the repository** and share it with other developers.

## License

[MIT](LICENSE)
