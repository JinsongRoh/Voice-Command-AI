# Voice Command AI: User Guide

This guide covers setting up Voice Command AI (VCA) on your own PC, pairing the phone or desktop app, talking to your agents, and reaching your PC from outside your network.

> **Status (M0, October 2026):** VCA is pre-release. There are no installers yet. You build the server and the apps from source. Items marked *coming soon* are not available yet.

## 1. What you get

| Part | Runs on | What it does |
|---|---|---|
| `vcad` (server) | your development PC | Owns your agent sessions (Claude Code, Codex, …), streams them, admits heavy builds, listens for paired devices. |
| `vca` (CLI) | the same PC | Starts sessions (`vca run`), pairs devices (`vca pair`), shows status. |
| VCA app | iPhone, Windows/macOS/Linux desktop (Android *coming soon*) | Session cards, live chat and terminal view, voice dictation, alerts. |
| Speech service | a server you choose | Speech-to-text for `vcad`: any OpenAI-compatible endpoint (a self-hosted Whisper server, OpenAI) or Google Cloud. |

Your phone talks only to your own PC, over an end-to-end encrypted (Noise) channel, after a one-time pairing. Nothing goes through a VCA cloud.

## 2. Install the server on your PC

Requirements: Git and the Rust toolchain (https://rustup.rs). Windows also needs the "Desktop development with C++" build tools.

```bash
git clone https://github.com/JinsongRoh/Voice-Command-AI.git
cd Voice-Command-AI
git checkout dev/m0-foundation
cargo install --path crates/vca-server   # installs vcad
cargo install --path crates/vca-cli      # installs vca
```

Start the server in the background and check it:

```bash
vca daemon
vca ping
```

### Configuration file

| OS | Path |
|---|---|
| Windows | `%APPDATA%\vca\vca\config\config.toml` |
| macOS | `~/Library/Application Support/dev.vca.vca/config.toml` |
| Linux | `~/.config/vca/config.toml` |

A minimal file:

```toml
server_name = "alice-desk"
# The app may only create sessions below these folders.
workspace_roots = ["~/code"]
```

Restart `vcad` after editing: end the `vcad` process and run `vca daemon` again.

## 3. Run your agents under VCA

Start agents through VCA so the app can see and type into them, without stealing window focus:

```bash
vca run -- claude
vca run -- codex
```

To make this automatic, add the shell integration to your profile. Typing `claude` then starts a managed session:

```bash
vca shell-init bash        # or: zsh, fish, powershell
```

Optional: install agent hooks so VCA gets exact "done", "needs input" and "permission" signals. On a shared machine, prefer the project-local scope:

```bash
vca hooks install --agent claude --scope project-local --dir ~/code/my-project
```

Agents started outside VCA are discovered automatically (`[discovery]`). They appear as *observed* cards: you can watch them, but typing into them only works in tmux, so use `vca run` for full control. On Windows this is the only way.

## 4. Pair the app

1. **Turn on two-factor sign-in once** (required by default): run `vca totp enroll` and scan the code with an authenticator app (Google Authenticator, 1Password, …). Keep the printed backup codes.
2. **Create a pairing code:** `vca pair` shows a QR code and a `vca://pair?...` link. It works once and expires after 5 minutes.
3. **In the app:** open the pairing screen, paste the link (or open it on the phone), check the device name, enter the 6-digit authenticator code, and tap **Pair**.

Manage devices with `vca devices` and `vca revoke <device-id>`. `vca kill-switch` drops every remote connection at once; `vca kill-switch --reset` undoes it.

### Install the app

- **iPhone:** build and install from `apps/mobile` with Xcode (developer signing). App Store/TestFlight is *coming soon*.
- **Windows/macOS/Linux desktop:** build it from the repository (`apps/ui`: `pnpm install && pnpm build`, then `apps/desktop`: `pnpm install && pnpm tauri build`), 
- **Android:** *coming soon*.

## 5. Reach your PC

By default `vcad` listens only on `127.0.0.1:47800`, so nothing outside your PC can reach it. Choose one way in:

**LAN.** Let devices on your local network reach your PC directly:

```toml
[remote]
bind = "0.0.0.0:47800"
[remote.provider]
kind = "lan"
```

**Tailscale.** If your PC and phone are on the same tailnet:

```toml
[remote.provider]
kind = "tailscale"
[remote.provider.tailscale]
mode = "serve"
```

**Outside the office: DNSPilot.** Install the DNSPilot Agent on your PC (section 7). Connecting the app through DNSPilot ("Connect DNSPilot" in the app, an app-mode tunnel) is *coming soon*. Until then, use Tailscale from outside. Never publish `vcad` through a public tunnel.

Create a new pairing code after changing the provider, so the QR code carries the new address.

## 6. Talk to your agents

### Turn on speech recognition

Point `vcad` at an OpenAI-compatible speech-to-text endpoint, put its API key in an environment variable, and add this to `config.toml`:

```toml
[speech]
engine = "openai"
language = "ko"

[speech.openai]
base_url = "https://stt.example.com/v1"   # your Whisper server, or https://api.openai.com/v1
model = "whisper-1"
api_key_env = "VCA_STT_TOKEN"
```

Set `VCA_STT_TOKEN` in the environment `vcad` starts from (Windows: System Properties → Environment Variables; macOS/Linux: your shell profile), restart `vcad`, and check with `vca transcribe sample.wav`. Never put the key in `config.toml` or share it.

### Dictate in the app

1. Open a session card.
2. Tap the microphone once and speak naturally, as long as you like. The text appears in the input box as you talk (it refreshes about once a second; the dimmed tail is still being recognized).
3. Tap again to finish, or **X** to discard this dictation.
4. Edit if needed, then **Send**.

Technical terms such as `pnpm` and `useEffect` are corrected automatically. **Show original** shows exactly what was heard, and **Use original** restores it. The expand button enlarges the input box for long dictation. Settings → Speech recognition switches to hold-to-talk if you prefer it.

The Chat tab shows your messages and the agent's replies; the Terminal tab shows the raw screen.

## 7. Install the DNSPilot Agent (for outside access)

Before you start: you need a DNSPilot console account (ask your organization's admin for an invite). Turn on two-factor authentication there (an authenticator app or a passkey). Docs: https://dns.dnspilot.kr/docs/en/agent (Korean: `/docs/ko/agent`).

**Windows 10/11 (x64, arm64)**
1. Sign in at https://dns.dnspilot.kr, open **Agent download**, and run the MSI. SmartScreen may warn until the code-signing certificate arrives; that is expected.
2. It installs the `DNSPilotAgent` service, a tray icon and the `dnspilot` command. Updates are automatic and signature-checked.
3. Choose **Sign in** in the tray icon (see "Sign in" below).

**macOS:** *coming soon*.

**Linux (Ubuntu, Debian, Raspberry Pi OS; amd64, arm64, armv7; systemd)**

```bash
curl -fsSL https://dns.dnspilot.kr/agent/install.sh | sudo sh
sudo dnspilot-agent login
systemctl status dnspilot-agent
```

To uninstall: `sudo sh install.sh --uninstall`.

**Sign in (device approval).** The Agent shows an 8-character code and opens https://dns.dnspilot.kr/agent/activate. Check that the code matches, sign in with your own account, name the device, and approve. You approve your own device; no admin is needed. You then get an email with a "block this device" link, valid for 7 days. Use it if you didn't do this yourself. Only approve codes you started; deny anything else.

**Tunnels.** Every tunnel that opens sends a "tunnel opened" email with a one-click off link. `dnspilot kill` on the PC turns all tunnels off immediately. Use it whenever something looks wrong. Exact command names: run `dnspilot --help`.

More: tunnels https://dns.dnspilot.kr/docs/en/tunnels, app mode https://dns.dnspilot.kr/docs/en/tunnel-app-mode.

## 8. Rules

- Never expose `vcad` or any internal service in a public tunnel.
- Never share pairing links, authenticator or backup codes, speech tokens, DNSPilot sign-in codes or app tokens.
- Only approve DNSPilot codes you started yourself.
- Don't copy or edit the DNSPilot Agent data folder (`/var/lib/dnspilot` or the Windows Agent data folder).
- Install DNSPilot only from dns.dnspilot.kr, never with `--skip-signature`.
- Don't create tunnels or records under customer domains.
- Lost or retired PC or phone: revoke the phone with `vca revoke`, and have the PC blocked in the DNSPilot console (DDNS devices).

## 9. Troubleshooting

| Symptom | Check |
|---|---|
| `vca ping` fails | Is `vcad` running? Start it with `vca daemon`. |
| App says "Login required" | Enter a fresh authenticator code (or a backup code). |
| App shows "Disconnected" and doesn't retry | The device was revoked or the kill switch is on (`vca devices`, `vca kill-switch --reset`). Pair again if revoked. |
| Pairing link "not valid" | Links expire after 5 minutes and work once. Run `vca pair` again. |
| "Speech recognition is turned off" | Section 6: `[speech]` in `config.toml`, and `VCA_STT_TOKEN` set where `vcad` starts. |
| Speech errors 401 | The token is wrong or expired; get a new one from whoever runs the speech server. |
| Speech errors 429 | Too many requests or failed attempts; wait a minute (10 minutes after repeated bad tokens). |

Help: open an issue at https://github.com/JinsongRoh/Voice-Command-AI/issues.
