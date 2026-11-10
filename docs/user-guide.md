# Voice Command AI: User Guide

This guide covers setting up Voice Command AI (VCA) on your own PC, pairing the phone or desktop app, talking to your agents, and reaching your PC from outside your network.

> **Status (version 0.1.0, November 2026):** This is VCA's first public release. The desktop app for Windows and macOS is on [GitHub Releases] (Linux comes in a later version; build it from source until then)(https://github.com/JinsongRoh/Voice-Command-AI/releases/latest); the iPhone and Android apps are coming to the App Store and Google Play. You can also build everything from source. Items marked *coming soon* are not available yet.

## 1. What you get

| Part | Runs on | What it does |
|---|---|---|
| `vcad` (server) | your development PC | Owns your agent sessions (Claude Code, Codex, …), streams them, admits heavy builds, listens for paired devices. |
| `vca` (CLI) | the same PC | Starts sessions (`vca run`), pairs devices (`vca pair`), shows status. |
| VCA app | iPhone, Android, Windows/macOS/Linux desktop | Session cards, live chat and terminal view, voice dictation, alerts. |
| Speech service | a server you choose | Speech-to-text for `vcad`: any OpenAI-compatible endpoint (a self-hosted Whisper server, OpenAI) or Google Cloud. |

Your phone talks only to your own PC, over an end-to-end encrypted (Noise) channel, after a one-time pairing. Nothing goes through a VCA cloud.

## 2. Install VCA

Install the **desktop app on the PC where your agents run**: it includes the server (`vcad`) and the `vca` command it uses, and starts the server when it opens (section 4). Then install the app on your phone and pair it with the PC (section 4). Desktop downloads are on [GitHub Releases](https://github.com/JinsongRoh/Voice-Command-AI/releases/latest).

### Windows

Download `Voice Command AI_<version>_x64-setup.exe` (on the release page the spaces show as dots) and run it. The installer and the app are signed by **MetaVu Co., Ltd.** For a new release, Microsoft Defender SmartScreen can still show "Windows protected your PC" until the file has built up reputation: choose **More info**, check that the publisher is MetaVu Co., Ltd., then **Run anyway**. Never run a VCA installer whose publisher is anyone else. The app then updates itself (section 14).

### macOS

Download the `.dmg` for your Mac (`aarch64` for Apple silicon, `x64` for Intel), open it and drag **Voice Command AI** to Applications. The app is signed with an Apple Developer ID and notarized by Apple, so it opens without a warning. macOS asks for the microphone the first time you dictate. The app then updates itself (section 14).

### Linux

Linux packages (AppImage, `.deb`, `.rpm`) come in a later version. Until then, build the app from source (section 4). Once they are released:

- **AppImage:** make it executable (`chmod +x Voice*.AppImage`) and run it. It updates itself.
- **`.deb`** (Debian, Ubuntu): `sudo apt install ./Voice*.deb`. **`.rpm`** (Fedora, openSUSE): `sudo dnf install ./Voice*.rpm`. The in-app updater installs a newer package of the same type and asks for your password.

The app keeps its keys in the Secret Service (GNOME Keyring or KWallet) when one runs.

### Android

**Google Play:** *coming soon* (the store listing is not live yet). Android 8.0 or later. Until then, build the app from source (below).

### iPhone

**App Store:** *coming soon* (the listing is not live yet). iOS 15 or later. Until then, build the app from source (below).

### Build from source

Requirements: Git and the Rust toolchain (https://rustup.rs). Windows also needs the "Desktop development with C++" build tools. To use the `vca` command in your own terminal (for example `vca run`, `vca pair` or the commands in this guide), install the server and the CLI this way even if you use the desktop app.

```bash
git clone https://github.com/JinsongRoh/Voice-Command-AI.git
cd Voice-Command-AI
cargo install --path crates/vca-server   # installs vcad
cargo install --path crates/vca-cli      # installs vca
```

Start the server in the background and check it:

```bash
vca daemon
vca ping
```

**The apps from source:**

- **iPhone:** build and install from `apps/mobile` with Xcode (developer signing).
- **Windows/macOS/Linux desktop:** build it from the repository (`apps/ui`: `pnpm install && pnpm build`, then `apps/desktop`: `pnpm install && pnpm build`). `pnpm build` first builds `vcad` and the `vca` CLI in release mode and copies them to `apps/desktop/src-tauri/binaries/vcad-<target-triple>[.exe]` and `vca-<target-triple>[.exe]` (`pnpm vcad`, or `node scripts/build-vcad.mjs [--debug] [--target <triple>] [--dry-run]`), then bundles both with the app as sidecars (`src-tauri/tauri.vcad.conf.json`). Agent hooks installed from the app run the bundled `vca`. `pnpm build:no-vcad` builds the app without a bundled server or CLI; `pnpm dev:vcad` runs the dev app with a debug `vcad`. The `binaries` folder is not committed. `pnpm build` also signs the files the in-app updater downloads, which needs the project's release key; without it, use `pnpm build:unsigned` (the same build, without those files). See [Updating VCA](#14-updating-vca).
- **Android:** build the APK from `apps/mobile` (`pnpm tauri android build`) and install it with `adb install`.

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

### Exact approval alerts (agent hooks) from the desktop app

Without hooks, VCA guesses from the screen when an agent waits for approval. With hooks, Claude Code and Codex tell VCA the moment they ask for approval or finish a turn. Hooks are off by default; nothing is installed until you click **Install**.

**Turn it on:** in the desktop app, open **Settings → Alerts → Agent connection (exact approval alerts)**, or press **Set up exact answer buttons** on a decision card that only seems to need confirmation. Each agent (Claude Code, Codex) shows its state: **Installed**, **Not installed**, **Outdated** (some hooks are missing or changed) or **Points to another vca** (the hooks run a different `vca`, for example one you installed by hand). Click **Install**, read the summary and confirm. **Repair** rewrites outdated hooks, or hooks that point elsewhere, so they run the `vca` bundled with the app. Phones and paired devices cannot install hooks; they only show a hint to turn this on in the PC app.

**What it changes:**

- **Claude Code:** adds VCA entries to the `hooks` of your user settings (`~/.claude/settings.json`, or `settings.json` in `CLAUDE_CONFIG_DIR`). Your own hooks and settings are kept.
- **Codex:** sets `notify` in `~/.codex/config.toml` (or in `CODEX_HOME`). If you already had a `notify` program, VCA chains it: VCA receives each event first and then runs your program with the same payload.
- A backup of the file (`<file>.vca-backup-<time>`) is written before every change; the app shows its path.
- The hooks run the `vca` CLI installed with the app (Windows: next to the app's executable; macOS: `Voice Command AI.app/Contents/MacOS/vca`; Linux: next to the app's executable). They fail open: if VCA is not running, the agent keeps working as usual.
- Agent sessions started afterwards use the hooks; sessions that are already running may need a restart.

The app installs for your user account (all projects). To use hooks in one project only, run `vca hooks install --scope project` (or `--scope project-local`) in that project's folder instead.

**Undo:** click **Uninstall** for the agent (or run `vca hooks uninstall`). It removes only VCA's entries, keeps your own hooks and restores a chained Codex `notify` program. You can also restore the backup file. Uninstall the hooks before you remove the app; otherwise the agents keep calling a `vca` that no longer exists (they still work, but may report a failed hook).

**Progress in the chat.** After you send a message, the chat shows a line under the last message: **Waiting for the agent · 3s** until the agent takes the message, then **Working · 42s** with the agent's current step (**Thinking…**, **Reading main.rs**, **Running cargo test**). Your message's status turns to **Seen by the agent** at the same moment. The line goes away when the turn ends. With hooks, the session flips to *Running* about 0.2 seconds after you press Enter. Without hooks, VCA reads the same from the agent's transcript a moment later: a new prompt means *Running*, the end of the turn means *Idle*. When both report, the hooks win and the transcript copy changes nothing. The text of the agent's thinking is never shown or sent, only that it is thinking. When a Claude Code agent in your own terminal starts a new conversation (`/clear`), the chat follows the new conversation, unless another Claude Code agent runs in the same folder (then VCA cannot tell which one started it).

**"The Claude connection is missing."** vcad checks the Claude Code settings for the VCA hooks when it starts, whenever the settings file changes, and every 5 minutes (it only reads the file). When they are missing (another tool may have rewritten `~/.claude/settings.json`), Claude Code sessions that never reported through a hook show a small banner: *The Claude connection is missing, so progress shows late*. In the desktop app, on this PC, **Repair** installs the hooks exactly like **Settings → Alerts → Agent connection**; nothing is written until you press it. Phones and paired devices show where to repair it instead. Sessions that were already running may need a restart to pick up the repaired hooks.

Agents started outside VCA are discovered automatically (`[discovery]`). They appear as *observed* cards: you can watch them, but typing into them only works in tmux, so use `vca run` for full control. On Windows this is the only way.

### Bring in sessions that are already open

The **Bring in** button of the session list (on a phone, pull the list down) looks for AI sessions running in other terminal windows of the selected computer. Each one found shows its state: **Listed** (it is on the session list now), **Hidden** (you hid it; **Show** puts it back), **No conversation yet** (listed; the chat appears after the first message), **Not supported** (the agent is not a terminal program) or **No terminal window** (it runs inside an app, for example Codex inside the ChatGPT app, and cannot be brought in). **Bring in all** lists every one that can be listed. Plain terminal windows without an AI agent (PowerShell, cmd, bash, zsh) are left out.

### Start a new session from the app

**New session** starts an agent on the selected computer. Choose **Local** or **Cloud** (below), the agent, and the **Project folder**:

- **Recent projects:** folders where you recently worked with an agent (marked **Agent history**).
- **Browse:** starts at your workspace roots (**Locations**). Open folders, go **Up one level** with the path at the top, filter long lists, and see which folders are **Git** repositories. On a computer, the arrow keys move between folders and Enter opens one. **Use this folder** picks the folder you are in.
- **New folder:** type a name; the folder is created inside the chosen folder when the session starts. An existing folder of that name is used as is.

Only folders inside `workspace_roots` are listed or created. You can also pick an **Account profile** and a **Display name**. For Claude Code, the session continues the last conversation in that folder (`claude -c`) unless you turn on **Start a new conversation**.

### Cloud agent sessions

VCA can also show sessions that run at a cloud agent provider: **Jules**, **Cursor Cloud**, **Devin**, **Copilot (cloud)**, **Claude Managed Agents**, **Claude Code Routines** and **Codex Cloud** (which drives the `codex` CLI on the PC). VCA uses each provider's official API with your own account.

1. **On the PC,** add the account to `config.toml` (non-secret settings only):

   ```toml
   [[cloud.accounts]]
   provider = "cursor_cloud"
   account = "work"                    # a label, "default" if left out
   credential_env = "CURSOR_API_KEY"   # or store the key with: vca cloud login cursor --account work
   ```

   `vca cloud login <provider>` reads the key from stdin and stores it on the PC (`cloud/` in the data folder); `vca cloud logout` deletes it. `vca cloud providers` lists the configured accounts and what each provider supports. Keys never leave the PC except to that provider.
2. **In the app,** choose **New session → Cloud**, the provider, the **Repository** (usually `owner/repo`) and the **Prompt**, then **Launch** (or `vca cloud launch` on the PC).

A cloud session appears as a card marked **Cloud**, and the session filter has a **Cloud** view. It has a chat view but no terminal, and it cannot receive files. Each provider shows only what it supports. To take a cloud session off the list, it must end first: **Stop** it in the session view (for providers that support stopping from VCA: Cursor Cloud, Devin, Claude Managed Agents; stop the others in the provider's own app), then use **Remove from VCA**.

## 4. Pair the app

1. **Turn on two-factor sign-in once** (required by default): run `vca totp enroll` and scan the code with an authenticator app (Google Authenticator, 1Password, …). Keep the printed backup codes.
2. **Create a pairing code:** `vca pair` shows a QR code and a `vca://pair?...` link. It works once and expires after 60 seconds (`remote.pairing_ttl_secs`). A missing or mistyped 2-step verification code does not use it up: retry with the same link until it expires (at most 5 wrong codes).
3. **In the app:** open the pairing screen, paste the link (or open it on the phone), check the device name, enter the 6-digit authenticator code, and tap **Pair**.

**From the desktop app instead:** open **Settings → Connect a phone** (or **Connect a phone** on the pairing screen). It shows whether the server on this PC runs (with a **Start server** button), how phones will reach this PC, a permission picker (**Command** by default) and a **Create QR code** button. No code is made until you press it, because making one asks you to verify on this PC and starts its 60-second clock; press it when the phone is ready. The QR code then shows with a countdown and a copyable link; after it expires, **New QR code** makes another. Changing the permission while a code is shown removes that code (it was made for the old permission) and waits for **New QR code**, so no new verification prompt opens on its own. The **Two-factor sign-in** part always shows whether an authenticator is set up (and since when), its entry in the authenticator app and how many backup codes are left. **Set up again** replaces the authenticator (the old one keeps working until the new code is confirmed), and **Make new backup codes** replaces the backup codes; new codes can be copied or saved to a file before you close them. If the server requires two-factor sign-in and none is set up, the screen enrolls an authenticator first. The secret is shown only on this PC, and these controls exist only in the desktop app: paired phones cannot change them. The desktop app also shows this PC's own sessions as **This PC** without pairing: it talks to `vcad` over the same owner-only local channel as the `vca` CLI.

The desktop app starts its bundled `vcad` when it opens and no server is running (setting **Run the VCA server on this PC**, on by default). A server you started yourself (`vca daemon`) is used as is; the app only ever stops a server it started. The bundled server keeps running when you close the app, so your agents and phones stay connected; stop it from the same screen.

**Start VCA when I sign in** (Settings → General → Startup, desktop app only, off by default) opens the app, and with it the VCA server on this PC, each time you sign in to the computer. The app opens normally; it does not start hidden. On Windows it is listed under Settings → Apps → Startup and in Task Manager (turning it off there shows here as off); on macOS it is a LaunchAgent in `~/Library/LaunchAgents`; on Linux an entry in `~/.config/autostart`. Turning it off removes the entry. This is separate from *Restore at sign-in* (section 10), which reopens your saved agent windows.

Manage devices with `vca devices` and `vca revoke <device-id>`. `vca kill-switch` drops every remote connection at once; `vca kill-switch --reset` undoes it.

### Install the app

Install the app on each phone or computer as in section 2. The desktop app on the PC itself needs no pairing: it shows this PC's sessions as **This PC**.

### App lock

The desktop and phone apps open locked, so a lost or stolen device cannot drive your sessions. Unlock with the device's own sign-in; there is no extra password:

- **Windows:** Windows Hello. **macOS:** Touch ID or your account password. The prompt comes from the VCA server on this PC (the same local verification as [security setup](security-setup.md#local-verification)), so the desktop app waits for its bundled server to start. If the server does not run, the lock screen offers **Start the server and try again**. A PC with no Windows Hello or Touch ID asks for the authenticator code instead.
- **iPhone:** Face ID or Touch ID, with the device passcode as fallback. **Android:** fingerprint or face unlock, with the screen lock PIN, pattern or password as fallback.
- **Nothing to verify with** (no Windows Hello, Touch ID or authenticator on the PC; no biometrics and no passcode on the phone): the app warns that anyone can open it, then opens. Set one of them up to make the lock work.

The lock screen shows only the app name and **Unlock**, and starts the verification once by itself. While the app is locked, no session content is shown and nothing can be sent, decided or dictated. Hands-free mode and Voco pause and spoken alerts stay silent. Alerts still arrive as system notifications, which show only the session name and a generic line, never the request.

The app locks again after a while without use, and when it comes back from the background after longer than that. Set the time under **Settings → Security → App lock** (1, 5, 15, 30 or 60 minutes; 15 by default). **Lock now** locks at once. Turning the lock off needs a verification first. The **Command Center** asks for a new unlock whenever the last one is more than 5 minutes old, even with the app lock off, because it can act on every session.

The browser build (`pnpm dev` in `apps/ui`) has no lock. Add `?applock=1` to the address to try the lock screens; there, a stand-in prompt succeeds after a short pause.

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

**Outside the office: DNSPilot.** The DNSPilot Agent on your PC (section 7) carries the app's connection through a DNSPilot **app-mode tunnel**: no port is opened on your router or PC, `vcad` stays on `127.0.0.1`, and the tunnel admits only the VCA app's own key (a client certificate checked during the TLS handshake). TLS ends at the Agent on your PC; VCA's pairing, authenticator code and end-to-end encryption still apply inside the tunnel. Tunnels need a paid DNSPilot plan.

*New in this build: tested against a local stand-in for the Agent and a mock console, not yet against the live DNSPilot service.*

1. **In the desktop app on your PC**, open Settings → DNSPilot (or the pairing screen) and choose **Connect DNSPilot**. The app shows an approval code and an **Open** button for https://dns.dnspilot.kr/oauth/activate.
2. **In the browser**, sign in to DNSPilot, check that the code matches, pick your organization and, under devices, **your PC**, then approve. Creating tunnels needs approval by an organization owner or admin. The app continues by itself.
3. **Create the tunnel** in the desktop app: pick your PC from the list (the desktop app preselects the PC it runs on; any other device must be chosen by hand) and the domain, press **Create tunnel on** *your PC*, and confirm the device and domain in the dialog. Phones don't create tunnels. If that PC already has a tunnel to `vcad`, the app uses it instead of creating another. The app creates an app-mode tunnel to `127.0.0.1:47800` with a random name such as `vca-k3x9q2ab4m.example.com` and registers its key on it. DNSPilot emails your organization that a tunnel opened, with a link to turn it off. Until DNSPilot offers its domain list to apps, you type the zone ID shown in the console instead of picking the domain. If DNSPilot answers that the PC wasn't selected when you approved, disconnect DNSPilot, connect again and select it. If you already have an app-mode tunnel to the PC, pick it under **Use an existing tunnel** instead. If the tunnel is created but the app's key can't be registered, the app says so and keeps the tunnel: press **Register this app on the tunnel** rather than creating another.
4. **On the PC**, set the provider in `config.toml` and restart `vcad`:

   ```toml
   [remote.provider]
   kind = "dnspilot"
   ```

   `vcad` finds the Agent's app-mode tunnel to its port by itself. If it can't read the Agent's tunnel list (on Linux, add the `vcad` user to the `dnspilot` group), or the PC has several such tunnels, add `[remote.provider.dnspilot]` with `hostname = "vca-k3x9q2ab4m.example.com"` (the app shows these lines). `vca remote` shows the provider, the Agent's state, and the tunnel's address and state (`serving`, `waiting`, `paused`, …).
5. **Pair** (section 4) with a new pairing code: it carries `wss://vca-k3x9q2ab4m.example.com/v1/ws`. A phone needs no DNSPilot login: see below.

**Phones: one QR code.** A tunnel admits only registered keys, so a new phone has to register its key before it can reach your PC. When the desktop app shows a pairing code for a tunnel it created, it adds a single-use DNSPilot invite to the same QR code. In the phone app, choose **Scan QR code** and scan it once: the phone registers its own key with the invite. Then enter your authenticator code and tap **Pair**. If you type the code before scanning, pairing continues by itself. The server uses up the pairing code before it checks the authenticator code, so the app never tries without one when the QR code came from the camera.

**The 30-second wait.** After a phone registers, the DNSPilot Agent on your PC can take up to about 30 seconds to learn the new key. The app shows "Registered. Connecting can take up to 30 seconds." and keeps retrying every few seconds; you only see an error if the wait ends without a connection. A key that was already registered connects at once.

If the invite can't be made, the desktop app still shows a plain pairing QR code with a short note saying why (DNSPilot isn't connected in this app, this app didn't create the tunnel, or too many invites). Register the phone with the console QR code, below.

**Console QR code.** In the DNSPilot console, open the tunnel, then **App clients → Phone pairing QR**. Scan it in the phone app, or paste the link into the pairing field. The app accepts only `https://dns.dnspilot.kr/enroll#...` links; this registers the phone but doesn't pair it, so the app then asks you to scan the VCA pairing QR code on your PC. Your phone's camera app can't do this for you: scan from inside the VCA app.

**Registered devices.** In the desktop app, Settings → DNSPilot → **Registered devices** lists the keys on a tunnel, with where each came from and any invites still pending. **Revoke** removes a key or cancels an invite; a revoked device is cut off within about a minute.

**Tunnel created in the console instead.** Create it there with Tunnels → New: device = your PC, local port 47800, http, access mode **App**. In the app, pick it under **Use an existing tunnel** (or, without connecting DNSPilot, enter its address under **Use a tunnel created in the DNSPilot console**). Because VCA did not create that tunnel, register the app's key yourself: open the tunnel's **App clients**, add the SPKI SHA-256 fingerprint the app shows, and confirm your identity when DNSPilot asks (re-authentication). On the PC, `dnspilot client add <tunnel-id> --spki <fingerprint>` does the same. Then tap **I added it** and continue with step 4.

**App tokens.** Settings → DNSPilot → Advanced can issue a `dpt_app_` app token for a tunnel VCA created. Keep this off for VCA: an app token is a fallback for clients that can't present a certificate, not a second factor, and while app tokens are allowed a token alone opens the tunnel. If you issue one anyway, it expires after 7 days; turn off **Allow app tokens** in the console when you're done.

The app's client key and the DNSPilot tokens stay in your phone's or computer's keychain; they never appear on screen except the public fingerprint. **Disconnect DNSPilot** revokes the app's access: DNSPilot turns off every tunnel the app created and revokes the keys and tokens it registered, so phones on those tunnels stop connecting. The tunnels aren't deleted, but connecting DNSPilot again doesn't bring them back, because a new approval can't manage them; you'd have to turn them on in the console. Never publish `vcad` through a public tunnel (password or fully public access modes).

Create a new pairing code after changing the provider, so the QR code carries the new address.

## 6. Talk to your agents

### Turn on speech recognition

The easiest way: in the desktop app on the PC, open **Settings > Speech engines on this PC**, add your speech-to-text engine (and, if you like, a correction LLM and a TTS engine) from a preset, enter a key if it needs one, and press **Test**. VCA ships no speech servers or models; [Your own speech engines](speech-engines.md) shows step by step how to run free local engines (faster-whisper-server, Ollama, MeloTTS or Supertonic) and connect them. Phones see the engines by name and pick them in their own settings; only the PC's desktop app can change them.

To configure by hand instead, point `vcad` at an OpenAI-compatible speech-to-text endpoint, put its API key in an environment variable, and add this to `config.toml`:

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

**On a weak network.** If the recording doesn't reach your PC, the app keeps it and says so: tap **Retry** to send the same recording again, or discard it. Nothing you said is lost to a dropped connection, and nothing is sent twice.

Technical terms such as `pnpm` and `useEffect` are corrected automatically. **Show original** shows exactly what was heard, and **Use original** restores it. The expand button enlarges the input box for long dictation. Settings → Voice → Speech recognition switches to hold-to-talk if you prefer it.

**Teach it the words it keeps getting wrong.** Settings → Voice → **Commonly misheard words** holds your own list: each correct word with what gets written by mistake, for example a product name and the two ways it comes out. Add, change or delete a word, and it applies from your next dictation; there is nothing to restart. Words without a mistaken form are still given to the speech engine as a hint. The list is kept on your PC (`speech/personal.json` in the data folder), so it works on every device connected to that PC. Viewing it is open to every paired device; changing it needs a device with admin access, and each change is written to the audit log (the number of words, not the words). The screen appears when speech recognition is on and `vcad` is new enough to offer it.

**Choose how the dictation button moves.** Settings → Voice → Speech recognition → **Dictation button animation** offers 18 styles in two groups. In the **Light assembly** styles, the five strokes of the logo fly in as light and snap together when you start dictating and again when you send. The default is **Drop from above**. While you speak, the logo shimmers with your voice. The **Stroke motion** styles move the strokes while you dictate. **Amplitude (classic)** is the original motion. Point at a style or select it to preview it. If your system's reduce-motion setting is on, the button always shows the still logo.

**Your keyboard's own dictation works too.** The input box is a normal text field, so the dictation key of your phone or computer keyboard can fill it as well: the microphone key on the iOS keyboard, Google voice typing on Android (Gboard), Samsung voice input, Windows voice typing (**Win+H**) or macOS Dictation (press **Fn** twice, or the key set in System Settings → Keyboard). Tap into the input box first, dictate with the keyboard, check the text, then send. This uses your device's speech recognition, so it works even when no speech engine is set up on the PC, and it follows the device's language and privacy settings. VCA's own microphone button adds what the keyboard can't: live text from your PC's engine, automatic correction of technical terms, the original-transcript view and hands-free mode.

### Hands-free mode

Hands-free mode lets you dictate and send messages, and answer simple requests, by voice. It is **off** by default. Turn it on with the **Hands-free** button above the input box or in Settings → Voice → Speech recognition. The first time, the app notes that **command text will be read aloud**: whatever you dictate, and the commands of pending requests, are spoken by the alert voice (the phone's voice or the TTS engine on your PC).

**Sending a message with the wake word (default).** Say **"Voco"** (보코, ボコ); the app answers with a short chime and "Yes?" and writes what you say into the input box. End the message with **"over"** or **"send it over"** (보내줘 오버, 送信、オーバー, cambio): the app says which session it sends to and sends the text after **3 seconds**. Saying **"cancel"** in those 3 seconds recalls it, and nothing has left the phone until then. A message that ends like a command ("…fix it", "…고쳐줘") followed by a short silence is sent the same way after a short countdown with soft ticks; speaking again or tapping the screen stops the countdown and keeps the text. Nothing is ever sent on a pause alone after an unfinished sentence. "Again" starts over, "Voco, session 3" (or the session's name) switches the session and "Voco, status" plays the briefing. Before the wake word, only a short piece of each utterance is checked for it; nothing else is kept or sent.

These are the defaults. In Settings → Voice → Speech recognition you can turn the wake word off, turn off **Schedule the send when a message ends like a command**, change the countdown, or set **When a message ends** to **Read it back briefly and send on "yes"**, which uses the read-back below instead of the 3-second window.

**Sending a dictation you started with the microphone button.** When you tap the microphone, dictate and finish, the app reads back only the target session and the beginning of the text (about the first 8 words, at most 40 characters, then "…"), for example "To Session 3: fix the login button… Send it?", and then listens for a few seconds. To hear the whole text instead, turn on **Read the whole text when confirming** in Settings → Voice → Speech recognition.

| You say | What happens |
|---|---|
| yes, send · 응, 네, 예, 보내, 보내줘, 전송, 그래, 좋아 · はい, 送信 · sí, enviar | The text is sent. |
| no, cancel · 취소, 아니, 아니요, 하지 마 · いいえ, キャンセル · no, cancelar | Nothing is sent; the text stays in the box. |
| again · 다시 · もう一度 · otra vez | The dictation is discarded and a new one starts (tap mode). |
| anything else, or nothing (silence) | The app asks again ("Send it? Say yes or cancel."), up to twice. If there is still no clear answer, nothing is sent and the text stays in the box. |

Hesitations, polite endings and punctuation around the answer are fine: "uh, yes", "send it please", "어 보내", "네 보내주세요" and "보내줘요." all send. Only a cancel word cancels; an unclear answer never sends and never discards the text. The state is always shown next to the microphone: **Waiting for confirmation**, **Listening**, **Sent**, **Cancelled** or **Not sent: the text is kept**. Tapping anywhere on the screen, or pressing a key, stops the voice at once and leaves the text unsent.

**Answering requests by voice.** While hands-free mode is on, a request that appears on the open session ("Session 5 asks to run pnpm build. Allow it?") is read out. A decision in another session is announced by the alert voice even when spoken alerts are off (quiet hours and the per-category alert switches still apply); open that session to answer it. The PC first classifies the command, as for the remote shell:

- **Safe** commands with plain **Yes** / **No** options can be answered by saying **allow** or **deny** (허용 / 거절, 許可 / 拒否, permitir / rechazar). "Yes" or "응" is not an approval: say the word.
- **Caution** and **dangerous** commands, requests without a command, requests the PC could not classify, and requests with other choices are never answered by voice. The app says "Please check the screen" and you answer on screen (with biometrics where you set them up).
- Options such as **Yes, and don't ask again** are never chosen by voice.
- An unclear answer is asked again once, then the app stops and leaves the request on screen. Silence or "cancel" also stops without answering.
- If the request changes or disappears while the app is listening (answered on another device, or replaced), the spoken answer is dropped.

A voice answer sends exactly the same message as a tap, so the PC records it like a tap. The app also keeps its own list of the last 100 voice answers on the device.

Hands-free mode uses whatever microphone and speaker the system uses, so a Bluetooth headset works like the built-in ones.

### Optional: context correction and topic detection

`vcad` can also ask a language model to fix words the recognizer misheard, using the surrounding text, and to keep track of what you are talking about. It works with any OpenAI-compatible chat endpoint (`POST {base_url}/chat/completions`) and is off by default:

```toml
[speech.llm]
enabled = true
base_url = "https://llm.example.com/v1"   # any OpenAI-compatible chat endpoint
model = "your-model"
api_key_env = "VCA_LLM_TOKEN"   # omit for servers without a key
timeout_ms = 4000               # after this, the text is kept as recognized
max_concurrent = 2
topic_every_n_finals = 3        # refresh the topic every 3 finished chunks
```

- Only finished chunks are checked (about every 15 seconds while you speak, and when you stop), never the live preview, so dictation stays fast.
- The model may only replace misheard words. Answers that rephrase, translate or change too much are ignored, and on any error or timeout you get the normal transcript.
- Fixes appear in **Show original** tagged **Context (AI)**; **Use original** still restores exactly what was heard.
- The detected topic (for example "Topic: Kubernetes deploy") is shown under the input box, and its terms help the recognizer with the next chunks.
- The transcribed text and the recent dictation are sent to the endpoint you configure. Use a server you trust.

### Several engines and correction models (choose in the app)

You can configure several speech engines and correction models as profiles. Each phone or tablet picks one of each in **Settings → Voice → Speech recognition** ("Speech engine (on the PC)" and "Transcript correction (LLM)"; a choice is saved as soon as you pick it); devices that never chose use the defaults. The single `[speech] engine` and `[speech.llm]` sections above keep working and appear as the profiles `openai`/`google` and `llm`.

Keys never go in `config.toml`. Give a profile an `api_key_env`, or store the key on the PC (it is kept only on that PC, and only the local `vca` command can set it; paired devices can never send keys):

```sh
vca speech login openai      # asks for the key without showing it
vca speech logout openai
vca speech options           # lists profiles and which ones still need a key
```

Profiles without a key are shown greyed out in the app with the `vca speech login …` command to run.

**Self-hosted Whisper and a local OpenAI-compatible LLM** (nothing leaves your network):

```toml
[speech]
language = "ko"
default_engine = "whisper-local"
default_llm = "local-llm"

[[speech.engines]]
id = "whisper-local"
label = "Whisper (this PC)"
kind = "openai_compatible"
base_url = "http://127.0.0.1:8000/v1"
model = "whisper-1"

[[speech.llms]]
id = "local-llm"
label = "Local LLM"
kind = "openai_compatible"
base_url = "http://127.0.0.1:8080/v1"
model = "qwen2.5-7b-instruct"
```

**OpenAI** (speech and correction; run `vca speech login openai` once, then both profiles share that key):

```toml
[[speech.engines]]
id = "openai"
label = "OpenAI"
kind = "openai"
model = "gpt-4o-transcribe"

[[speech.llms]]
id = "openai-mini"
label = "OpenAI (small model)"
kind = "openai_compatible"
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"
credential = "openai"      # reuse the key stored for the "openai" profile
```

**Anthropic** (correction only), with an API key:

```toml
[[speech.llms]]
id = "claude"
label = "Claude"
kind = "anthropic"
model = "claude-haiku-5-5"   # the default; fast enough for live dictation. claude-opus-5-5 also works.
# api_key_env = "ANTHROPIC_API_KEY", or run: vca speech login claude
```

or with your Anthropic account through the `ant` command-line tool: install it on the PC, run `ant auth login` once, and set `ant_profile`:

```toml
[[speech.llms]]
id = "claude-account"
label = "Claude (my Anthropic account)"
kind = "anthropic"
ant_profile = "default"      # or the name of another ant profile
```

`vcad` then asks `ant auth print-credentials --access-token` for a short-lived token when needed. This uses the credits of your Anthropic API (Console) account. A claude.ai subscription login cannot be used: Anthropic does not allow third-party apps to use it.

Other options: `timeout_ms`, `max_concurrent` and `topic_every_n_finals` under `[speech.llm]` apply to every correction profile (a profile can override `timeout_ms`). Profile ids use lowercase letters, digits, `-` and `_`, and must be unique across engines and models.

The Chat tab shows your messages and the agent's replies; the Terminal tab shows the raw screen.

### Send files and photos

The paperclip next to the microphone attaches files to the open session:

- **Photo library**: pick one or more photos.
- **Take photo** (phones and tablets): opens the camera.
- **Files**: the system file browser, with the device's own folders (On My iPhone, Downloads, internal storage, local folders on a PC) and cloud drives (iCloud Drive, Google Drive, Dropbox, OneDrive). Select several files at once if you like.
- **Folder** (desktop app only): sends a whole folder. Hidden and system entries (`.git`, `node_modules`, `.DS_Store`, `Thumbs.db`) are skipped. A folder upload can be up to 100 MB and 500 files.

On the desktop app you can also drag files and folders onto the input box.

Files are saved on the PC in the session's project folder under `.vca/attachments/` (named `<date>-<time>-<name>`, in UTC). VCA adds `/.vca/` to the project's `.git/info/exclude` (not `.gitignore`) so attachments are never committed by accident. In sessions started with `vca run`, VCA types `Attached file: @.vca/attachments/…` into the agent's input without pressing Enter: add your instructions by voice or text and send. Turn on **Send right after attaching** in the paperclip menu to send immediately. Sessions VCA only observes cannot take input, so the app shows where the file was saved; mention it in your next message.

Limits: 25 MB per file (photos over 4096 px are scaled down on the phone first). Images, PDF, text and code, Office documents and zip archives are accepted; programs and scripts that Windows runs directly (`.exe`, `.bat`, `.ps1`, ...) are refused. Files go only to local sessions inside your workspace roots; cloud sessions cannot receive files. Change the limits in `config.toml`:

```toml
[attachments]
max_bytes = 26214400          # 25 MB per file
max_folder_bytes = 104857600  # 100 MB per folder
max_folder_files = 500
git_exclude = true            # add /.vca/ to .git/info/exclude
# enabled = false             # turn attachments off
```

### Work inside a session

Everything about a session can be done from its own screen.

- **Switch sessions quickly.** Press **Ctrl+K** on a computer (Cmd+K on a Mac), or tap the session name at the top of the session on a phone. A searchable list opens with the sessions that need attention first, then the ones you pinned, the recent ones and all the rest. The star pins a session on this device only. The sheet also holds the **All / Needs attention / Local / Cloud** filter.
- **The session screen on phones.** An open session fills the screen under one slim bar: **Back** (to the session list, where the bottom menu is), the session name (tap it to switch), the **Chat / Terminal / Shell** menu, **Reading mode**, the connection dot and **⋯**. **⋯** holds the computer, the connection, the Command Center, spoken alerts, session actions (search, rename, pause, restart, stop) and **Alerts**; its badge counts pending alerts. The bar slides away while you scroll back through older messages and returns when you scroll down, but never while a decision or an alert waits. Below the chat, one row holds the microphone, the message box (it grows to about four lines), **⋯** and **Send**; **⋯** opens the quick prompts, the control keys, snippets, hands-free mode, conductor mode and the larger text box (the control keys keep it open for repeated presses). **Reading mode**, or a double tap on empty chat space, hides the bar and the message row so the chat fills the screen; a small microphone and an exit button stay. Decision cards always stay on screen. To get the swipeable session cards back, turn on **Settings → Appearance → Show the session card row on phones**.
- **Send text to another session.** Chat messages, code blocks and text selected in the Terminal tab have a **Send to session…** button. Choose the session, then either **Put in its message box** (the text waits there for you to edit and send) or **Send now…**, which first asks "Send to *name* now?". A session that is waiting for your answer to a question, that has ended, or that you can only watch never receives the text directly; put it in its message box instead.
- **Address another session with @.** Type `@` at the start of the message box and pick a session, or type its full name (`@web-frontend run the tests`). A **To: *name*** chip shows where the message will go, and the Send button says so; remove the chip to send to the current session instead. Messages to another session are sent by hand only (not by hands-free voice), without attachments, and follow that session's rules.
- **Shell tab.** On devices allowed to open remote shells (admin permission), a session started in a folder has a **Shell** tab next to **Chat** and **Terminal**. It opens a remote shell in the session's folder, with the same authenticator code, checks and time limits as below, and shows the time left. If a shell is already open for that folder, the tab uses it. The shell's screen is in the tab, and the input below it types into the shell.
- **Terminal tab on phones.** The terminal keeps the PC's width and scales the text to fit the screen; pinch with two fingers (or use **−**, **Fit**, **+**) to zoom, and swipe sideways when it is wider than the screen. Landscape gives the most room. Below the terminal, a one-line input and the keys **Ctrl**, **Ctrl+C**, **Esc**, **Enter**, **Tab** and the arrows let you type into the terminal without going back to Chat. The chat button next to the input opens the full message box (voice, attachments).
- **Sessions running in a terminal window on the PC.** Sessions you started yourself in a terminal, and sessions brought back by **Windows** restore, run in a real terminal window on the PC, so their screen is not mirrored in the Terminal tab; read them in Chat. On the PC's own desktop app, **Bring the PC window to front** shows that window. Sessions VCA opens itself show their screen in the tab.
- **Ended sessions.** An ended session shows **Reopen** and **Remove from VCA** in its screen, in its card menu and on the selected phone card. Reopen starts a remote shell again in the same folder with the same shell (it asks for the code again), or restarts a session VCA started. Sessions that ran in a terminal window on the PC can't be reopened from VCA; start them again on the PC.
- **Remove from VCA** (card menu, session view, and each row of **Session order**) takes a session off the list on every device. The conversation history and the project folder stay on the PC. What it does depends on the session:
  - **Ended:** removed at once.
  - **Running, started by VCA:** asks once, then stops the agent and removes the session (**Stop and remove**).
  - **Running in a terminal window on the PC:** VCA cannot stop it. Close that window (or exit the agent there), then use **Remove from VCA** again.
  - **Cloud:** two steps. **Stop** it in the session view first; once it has ended, use **Remove from VCA** again.
  - **The conductor's own session:** stop the conductor in the Command Center.

  View-only devices don't get the action, and an open remote shell has its own **Close**.

### Open a shell on your PC (remote shell)

When an agent is stuck on a command it isn't allowed to run, you can open a plain shell on the PC from the app and run the command yourself, without typing it again. Shells run with your normal user rights only: VCA never asks for administrator rights, and `sudo` or `runas` in a remote shell won't get them.

Who can open one: only devices paired with the **admin** permission (and the VCA app on the PC itself). View and command devices can't open shells, watch them or type into them; they can still stop one.

Before the first shell, set up an authenticator app on the PC: in the desktop app, **Settings → Security → Security status → 2-step verification** (also offered under **Settings → Connect a phone → Two-factor sign-in**), or `vca totp enroll`. Then:

1. In the app, start a new session and choose **Shell**, the shell (Windows: PowerShell, PowerShell 7 (`pwsh`), Command Prompt; macOS/Linux: bash, zsh, sh) and a folder inside your workspace roots.
2. Enter the 6-digit number shown for the **Voice Command AI** entry in your authenticator app. Each code works once; if it was just used (for example to sign in), wait for the next one. No access to the app? Tap **Use a backup code** and type one of the backup codes you saved (each works once). If the PC has no authenticator set up, the app says so instead of asking for a code.
3. The shell opens as a session card marked **Shell**, with a countdown. Every computer running the VCA app shows a notice when a remote shell opens or closes.

Running commands: type in the Terminal tab (or in a session's **Shell** tab, see above), or use **Run in shell** on a decision card or a code block. The PC checks each command first and marks it safe, caution or dangerous, highlighting the risky part. Dangerous commands (recursive deletes, formatting disks, permission and registry changes, stopping services, shutdown, `curl ... | sh`, reading `.ssh`/`.aws`/`.env` files, `git push --force`, `git reset --hard`, `sudo`/`runas`, ...) need a fresh authenticator code unless you entered one in the last 5 minutes on that device.

Shells close by themselves after 15 minutes without input or 60 minutes in total, when you trip the kill switch, and when `vcad` stops. Shortly before a shell closes for being idle, a banner counts down the seconds left; **Keep open** resets the idle timer (on PCs that send this notice). A closed shell offers **Reopen**, which opens a new one in the same folder. A minute before an idle close every app gets a "closing soon" notice; keeping the shell open (or typing in it) moves the idle limit forward again, but never past the 60-minute total. A closed shell stays as an ended card for 10 minutes (`[sessions] exited_retention_secs`); remove it sooner with **Dismiss** (needs command permission; written to the audit log as `session_dismiss`), or open a new shell with the same shell and folder, which needs a fresh code. Every open, close, command and code check is written to `audit.jsonl` in the VCA data folder (codes are never written; secrets in commands are masked). The log rotates at 5 MB, keeping one older file (`audit.jsonl.1`). Admin devices can read it in **Settings > Activity log**.

```toml
[shell]
enabled = true            # false: nobody can open a shell
idle_timeout_mins = 15
max_lifetime_mins = 60
step_up_window_mins = 5   # after a code, dangerous commands need no new code for this long
```

Arbitrary programs can only be started from the PC itself with `vca run`; a paired device cannot launch them (use a remote shell instead). For development and testing only, `[sessions] allow_generic_launch = true` lets clients launch arbitrary programs; leave it off otherwise.

## 7. Install the DNSPilot Agent (for outside access)

Before you start: you need a DNSPilot console account (ask your organization's admin for an invite). Turn on two-factor authentication there (an authenticator app or a passkey). Docs: https://dns.dnspilot.kr/docs/en/agent (Korean: `/docs/ko/agent`).

**Windows 10/11 (x64, arm64)**
1. Sign in at https://dns.dnspilot.kr, open **Agent download**, and run the MSI. SmartScreen may warn until the code-signing certificate arrives; that is expected.
2. It installs the `DNSPilotAgent` service, a tray icon and the `dnspilot` command. Updates are automatic and signature-checked.
3. Choose **Sign in** in the tray icon (see "Sign in" below).

**macOS 12 or later (Apple silicon or Intel)**

```bash
curl -fsSL https://dns.dnspilot.kr/agent/install-macos.sh | sudo sh
sudo dnspilot-agent login
sudo dnspilot-agent status
```

The Agent is signed with Developer ID and notarized by Apple. It runs as a LaunchDaemon (no menu-bar app yet); the CLI is `/usr/local/bin/dnspilot-agent`.

**Linux (Ubuntu, Debian, Raspberry Pi OS; amd64, arm64, armv7; systemd)**

```bash
curl -fsSL https://dns.dnspilot.kr/agent/install.sh | sudo sh
sudo dnspilot-agent login
systemctl status dnspilot-agent
```

To uninstall: `sudo sh install.sh --uninstall`.

**Sign in (device approval).** The Agent shows an 8-character code and opens https://dns.dnspilot.kr/agent/activate. Check that the code matches, sign in with your own account, name the device, and approve. You approve your own device; no admin is needed. You then get an email with a "block this device" link, valid for 7 days. Use it if you didn't do this yourself. Only approve codes you started; deny anything else.

**Tunnels.** VCA uses one app-mode tunnel to `127.0.0.1:47800` (section 5). Every tunnel that opens sends a "tunnel opened" email with a **Disable now** link (it asks you to confirm). `dnspilot kill` on the PC turns all tunnels off immediately and keeps them off until `dnspilot resume`; use it whenever something looks wrong. Read-only checks: `dnspilot status`, `dnspilot tunnel ls`, `dnspilot events`. On Linux, `vcad` reads the Agent's status through `/run/dnspilot/local.sock`, which needs membership in the `dnspilot` group (without it, `vcad` can't find the tunnel by itself or show its state, so set `remote.provider.dnspilot.hostname`).

More: tunnels https://dns.dnspilot.kr/docs/en/tunnels, app mode https://dns.dnspilot.kr/docs/en/tunnel-app-mode, app authorization https://dns.dnspilot.kr/docs/en/oauth-device-flow.

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
| Pairing link "not valid" | Links expire after 60 seconds and work once. In the desktop app, open Settings → Connect a phone and press **New QR code** (or **Create QR code**), or run `vca pair` again. |
| "Speech recognition is turned off" | Section 6: `[speech]` in `config.toml`, and `VCA_STT_TOKEN` set where `vcad` starts. |
| Support asks for a diagnostic log | The log is off by default. Turn on Settings → About → **Diagnostic log → Keep a diagnostic log on this device**, repeat the problem, then **Copy** and send it yourself. It stays on the device and is never uploaded: it holds timings, counts, status and error codes, not audio, transcripts, prompts, file paths, keys or addresses. GitHub issues are public. Turning it off clears it. |
| Speech errors 401 | The token is wrong or expired; get a new one from whoever runs the speech server. |
| Speech errors 429 | Too many requests or failed attempts; wait a minute (10 minutes after repeated bad tokens). |
| Pairing says "DNSPilot is not ready" | Check that the Agent is signed in and the kill switch is off, and that one app-mode tunnel forwards to `127.0.0.1:47800` (`vca remote`, `dnspilot status`, `dnspilot tunnel ls`). Otherwise set `remote.provider.dnspilot.hostname`. |
| App can't reach the DNSPilot tunnel | Is this app's key on the tunnel's App clients list (Settings → DNSPilot shows "Ready")? Does the tunnel forward to `127.0.0.1:47800` with access mode App? Is it turned on in the console? |
| "This PC wasn’t selected when you approved VCA" | Disconnect DNSPilot in the app, connect again and select the PC under devices. |
| "DNSPilot did not allow this" | An organization owner or admin must approve tunnel access; tunnels need a paid plan. |

| Resources shows a session but pause is dimmed | The session was started outside VCA: it is shown for monitoring only. VCA never suspends, kills or reprioritizes a process it did not start. |
| A restored window opened but did not move | The window did not appear within 30 seconds. Move it by hand; the next snapshot saves the new place. |
| A restored session shows another conversation | Its transcript is gone, so the agent's latest conversation in that folder opened. Check the progress view. |

Help: open an issue at https://github.com/JinsongRoh/Voice-Command-AI/issues.

## 10. Bring your windows back after a restart

The **Windows** menu keeps a list of the AI CLI windows on a computer. After a reboot or a power cut it reopens each one at the same place, under your name for it, in the same conversation. It works on Windows, macOS and Linux desktops, and for tmux sessions on Linux servers.

**How the list fills.** Every 5 minutes VCA looks at running agent sessions (Claude Code, Codex and the other known agents) that have a terminal window, and saves any change. Nothing is written when nothing moved, and the previous file stays as a backup, so a power cut leaves the last good list. Plain shells are not listed. A session you renamed in VCA uses that name as its fixed window title.

```toml
[restore]
enabled = true                 # false turns the timed snapshot off
snapshot_interval_secs = 300   # at least 30
auto_register = true           # list agent sessions with a terminal window
launch_mode = "direct"         # or "managed"; see "Managed windows" below
```

**Coming from `layout.json`.** Use *Import layout.json* in the Windows menu, or `vca restore import layout.json`. Titles and positions are kept. The agent is taken to be Claude Code, in the latest conversation of the folder, until the snapshot finds the running session.

**Restoring.** *Restore all* or *Restore selected* first shows what will happen. Windows open one at a time with a pause between them (6 seconds by default, per profile), and each one reports its state: waiting, opening, placed, skipped (already open, or folder missing) or failed, with a retry. Sessions do not start working on their own. The exact conversation is resumed by its session id (`claude --resume <id>`, `codex resume <id>`); when the id is unknown or its transcript is gone, the latest conversation in the folder opens instead (`claude --continue`, `codex resume --last`).

Windows Terminal puts each new window in front, so avoid typing elsewhere during a restore. Without Windows Terminal, a console window opens instead.

**Managed windows (Windows, opt-in).** With `launch_mode = "managed"` a restored window runs `vca run --name <title> -- <agent> <arguments>` instead of the agent itself. The agent then runs as a session of VCA, attached to the window: you work in the window as before, and the app's **Terminal** tab mirrors the screen and types into it directly, without console input injection. The window keeps its title, folder, the pause between windows and the message after restore. VCA uses the `vca.exe` installed next to `vcad.exe` (never one found on `PATH`); when it is missing, or with `launch_mode = "direct"` (the default), the window runs the agent directly and VCA only observes it. The default is direct because managed sessions end whenever VCA stops or updates. macOS, Linux and tmux windows always run the agent directly for now.

- **Closing the window** only detaches it: the session keeps running in VCA, stays on the session list and in the Terminal tab, and the next restore skips it as already open. Attach a window again with `vca attach <session>` (the id or name from `vca ls`). *Bring to front* finds the window while it is open; after it is closed there is no window to bring forward.
- **Leaving it:** press `Ctrl-]` then `d` to detach on purpose; exiting the agent closes the session.
- **Stopping VCA** ends managed sessions (`[shutdown] managed_sessions = "terminate"`, the default), unlike agents started directly, which keep running. Restore reopens them afterwards in the same conversation.
- A folder the agent does not trust yet still shows the agent's own trust question in the window; restored folders are normally trusted already.

```text
vca restore list [--profile P] [--json]
vca restore save [--profile P]               # measure the open windows now
vca restore run [--profile P] [--only A,B] [--dry-run]
vca restore import <layout.json> [--profile P]
```

`--only` keeps windows whose title contains one of the words. `vca restore run` starts `vcad` first when it is not running.

**Profiles.** Keep separate lists such as Work and Home. The snapshot updates the active profile. Removing a window from the list, or deleting a profile, never closes anything.

**If a monitor changed.** A window whose monitor moved or changed keeps its relative place. One that would land off every monitor goes to the nearest monitor. Positions are physical pixels and may be negative.

**On the computer itself.** The desktop app adds, for this PC only: a desktop shortcut, *Restore at sign-in* (off by default; turning it on adds a "VCA Session Restore" shortcut to your Startup folder, listed in Task Manager under Startup apps, and turning it off removes it) and *Send a message after restore* (off by default: each restored session then starts working unattended and spends your quota).

**From a phone.** The Windows menu shows the list of the computer selected in the header. Viewing needs any paired device; editing and running need admin permission. A run also needs a code from your authenticator app (or the step-up window of a recent code), opens only folders inside `workspace_roots`, and is written to the audit log with every edit (`restore_run`, `restore_edit`). The shortcut, the sign-in toggle and the message after restore cannot be changed remotely.

**Screen layout.** The *Screen layout* tab of the Windows menu arranges terminal windows that are already open, one monitor at a time. Pick a grid (1, 2, 4, 6, 8 or 10 cells, or your own rows and columns), then drag windows into the cells, or select a window and then a cell. *Auto fill* uses the session order, and can keep the windows of one project folder or one agent next to each other. *Arrange windows* moves them; *Undo* puts them back where they were just before. Several sessions in one window are tabs: they move together and cannot be split. Nothing opens or closes, and the focus stays where it is.

*Saved layouts.* **Save layout** keeps the current grid under a name. Under **Saved layouts**, each layout has **Apply** (arrange the windows now), **Load** (put the layout into the grid so you can adjust it; nothing moves until you press *Arrange windows*), **Rename** and **Delete** (no window moves or closes). A layout finds windows by session, folder and title; windows that are not open are reported as skipped, and **Load** says how many it found. When exactly one layout is saved, opening the tab loads it into the grid by itself (again, nothing moves); once you change the grid, it stays as you left it.

Viewing needs any paired device. Arranging needs admin permission, is written to the audit log (`window_arrange`) and, from a paired phone or another computer, also needs a code from your authenticator app (or the step-up window of a recent code), like a restore run. The kill switch stops arrangements from paired devices.

**What restore will not do.** It only starts known agents, stores no free-form arguments, and never adds a flag that bypasses permission prompts: if a captured session used one, it is dropped and a note says so. It moves only windows it opened itself, and it never closes a window or ends a process.

## 11. Alerts

When a session needs a decision or your input, it appears in the **Alerts** menu with its name, the request and the time, newest first. The number on the menu counts what is unhandled. Tap an item to open that session at the decision card. An item leaves by itself once it is answered, here or anywhere else, and the same decision never alerts twice. The check mark clears an item without answering it.

*When approval is needed* (in Alerts and Settings):

- **Ask** (default): shows Go and Later. Later keeps the item in the list.
- **Switch automatically**: opens the session, unless you are typing or dictating, have text in the message box or have a dialog open. Then it asks. It never answers for you.
- **Don't switch**: the screen never changes and there is no Go / Later prompt. A short notice and, while the app is in the background, an operating system notification still appear (each only if that alert category is on in Settings → Alerts); the list and the count update as usual.

**Exact and inferred approval alerts.** Exact approval alerts (with the request and Yes/No buttons) come from agent hooks, which you install yourself with `vca hooks install` (opt-in; VCA never adds hooks on its own). Without hooks, VCA infers approval prompts of Claude Code and Codex sessions running in your own terminal: a tool that usually asks (a shell command, a file edit, a web fetch, an MCP tool) has had no result for a few seconds (6 s; 20 s for web fetches and MCP tools) while the agent and its processes are idle. Such an alert comes a few seconds late and says **Seems to need confirmation**. It has no answer buttons: choose in the PC window, or answer with the key buttons (1, Enter, Esc, arrows) if the session has them. It leaves by itself once the tool runs or the conversation moves on. A prompt that was already open when VCA started is inferred only for file edits, web fetches and MCP tools, not for shell commands.

**Questions from the agent.** When Claude Code asks you something with a form (several questions, each with choices), the session shows a **Question from the agent** card and an alert. Answer each question: pick a choice (the one marked **Recommended** is the agent's suggestion), choose several where it says **Choose all that apply**, or **Type your own answer**. The card counts how many are answered; **Submit** sends all answers at once. VCA types them into the agent's form the way you would, and only after it sees the form on the screen. If the connection to the PC drops while sending, the card says the answers are **Not confirmed** and offers **Send again**: a question is never answered twice, so this is safe. If the agent did not take the answers, the card asks you to check the terminal.

On a computer, allow desktop notifications in the Alerts menu to get an operating system notification while the app is in the background; clicking it opens the session. On phones and tablets alerts show while the app is open. Notifications while the phone app is closed (phone approval alerts through DNSPilot's encrypted push relay) are not part of version 0.1.0.

### Spoken alerts with your own TTS engine

With **Settings > Voice > Spoken alerts** on, the app reads alert titles aloud (for example "Session needs a decision"). It only reads these short, generic titles: never the request text and never activity updates, which can hold commands or paths. Where the voice comes from is set in **Voice source**:

- **This device** (default): the voice of your phone or computer. Voice, speed and Preview are shown below it.
- **PC server: *engine name***: the PC synthesizes the title with a TTS engine you configured and sends the audio back; the app plays it. Use Preview to hear it. If the PC cannot speak (engine down, too many alerts, older server), the device voice is used instead. Audio is kept in memory, so a repeated alert is not synthesized twice.

The choice appears once the PC has at least one TTS engine. VCA ships no voices and no model weights: you run your own engine (for example MeloTTS, Chatterbox, CosyVoice, XTTS, F5-TTS, Fish Speech or any other server) and point VCA at it. VCA talks to the OpenAI-compatible speech endpoint, `POST {base_url}/audio/speech` with `{model, input, voice, response_format, speed}`, which many self-hosted TTS servers offer directly or through a small wrapper. VCA's own small wrapper for MeloTTS and Supertonic is in [`tools/tts-server`](../tools/tts-server/README.md); [Your own speech engines](speech-engines.md) walks through it, and the desktop app can add and test TTS engines in **Settings > Speech engines on this PC** instead of editing the file below.

Example `config.toml` for a TTS server on the same PC:

```toml
[tts]
default_engine = "local-tts"
# max_audio_bytes = 2097152   # largest audio answer accepted
# requests_per_minute = 20    # per device

[[tts.engines]]
name = "local-tts"            # a-z, 0-9, - and _
kind = "openai_compatible"
base_url = "http://127.0.0.1:8880/v1"
model = "tts-1"               # whatever your server expects
voice = "default"             # the voice for every language...
format = "mp3"                # mp3, opus, aac, flac or wav
# api_key_env = "MY_TTS_KEY"  # only if your server needs a key

[tts.engines.voices]          # ...unless a language has its own voice
ko = "my-korean-voice"
en = "my-english-voice"
```

**Choosing a speaker.** If your engine offers several voices, list them as `speakers` and each user picks one in Settings (first Female or Male, then a voice). The `id` is what your server expects as `voice`; the `label` is a plain name for the CLI and for older apps (the app shows its own translated names for female and male speakers). Without `speakers`, there is no picker and `voice` / `voices` apply as before.

```toml
[[tts.engines]]
name = "studio-voices"
base_url = "http://127.0.0.1:8880/v1"
model = "my-model"
voice = "F1"                  # used when a request names no speaker
# Shown under the speaker choice. Use it when the engine's license requires
# telling people that the audio is machine-generated.
disclosure = "Voices are generated by a machine."

[[tts.engines.speakers]]
id = "F1"                     # sent to your server as "voice"
label = "Female 1"
gender = "female"             # female, male or other

[[tts.engines.speakers]]
id = "F2"
label = "Female 2"
gender = "female"

[[tts.engines.speakers]]
id = "M1"
label = "Male 1"
gender = "male"
```

A request may only name an `id` from this list; any other voice is refused with `invalid_request`, so a client cannot make the PC send arbitrary values to your server. If a saved speaker is removed from the list, the app falls back to the first female speaker.

Restart vcad after editing. Then test from the PC:

```
vca tts list                                  # names, languages and speakers
vca tts say "Session needs a decision" --engine local-tts --out alert.mp3
vca tts say "Session needs a decision" --engine studio-voices --voice F1 --out alert.mp3
vca tts login local-tts                       # only if the server needs a key
```

`vca tts login <name>` stores the key on this PC (input hidden, or read from stdin); `vca tts logout <name>` deletes it. Keys never go in `config.toml` and are never sent to the app. The app only learns engine names, languages, speakers and the disclosure text, never URLs, models or keys.

Limits: the text is at most 300 characters, each device may ask for 20 alerts a minute by default, and the PC logs neither the text nor the audio.

**Licenses.** Each TTS engine has its own license for its code and, separately, for its model weights and voices; some allow only non-commercial use or require attribution, and cloned voices may need the speaker's consent. Check the licenses of the engine and model you use for your own use case. VCA does not include or distribute any of them.

### Pronunciation dictionary

**Settings → Voice → Spoken alerts → Pronunciation dictionary** teaches how spoken alerts say names written in Latin letters, for example a project called *SkyHarbor*. Each entry has **Written as** (Latin letters and digits; "Sky Harbor" and "sky-harbor" count as the same name) and **Say it as**. **Teach by voice** lets you say the name instead of typing it; check what was heard, then save. **Preview** plays it. Entries are marked **Built-in** (common product names that ship with VCA), **Suggested** (a reading VCA proposed for a session name, from your correction LLM if one is set up, else from rules) or **Yours**; yours always win, and a deleted built-in entry stays off. **Fix pronunciation** in a session's menu goes straight to its name.

The dictionary is kept on the PC (`pronunciations.json` in the data folder): the PC's TTS engine and every connected device use the same entries. Changing it needs the command permission.

### Status briefing and result summaries

**Status briefing.** Tap **Listen to status** (the speaker button in the top bar, or the button in the Alerts menu) to hear a short status of the selected computer in at most three sentences: how many sessions are in each state, which session has waited longest and for how long, and which sessions have an error. For example: "5 sessions: 1 waiting for approval, 2 running, 2 idle. Waiting longest: session 3, api, for 12 minutes." The text is also shown as a notice. The PC builds it from the session list with fixed sentences in English, Korean, Japanese and Spanish (other app languages get English); no language model is involved and nothing beyond the session list is used. Session numbers are the card numbers you see in the app, and Korean numbers are read as "삼번 세션".

You can also ask by voice: dictate exactly one of the briefing phrases into an empty message box, such as "현황 알려줘", "상태 알려줘", "status", "status update", "what's going on", "状況を教えて" or "qué está pasando". Case, spaces and punctuation do not matter, but the whole dictation must be the phrase: "status of the build?" is sent to the agent as usual. When a phrase is taken as a briefing, a notice says so and offers **Send anyway**, which sends the phrase to the session instead.

The voice is the one chosen for spoken alerts (the device voice or a TTS engine on the PC). A briefing is spoken because you asked for it, even when spoken alerts are off.

**Turning the voice on and off quickly.** The speaker button in the top bar and in the Alerts menu switches spoken alerts on and off; it is the same setting as **Settings > Voice > Spoken alerts**. Turning it off stops the voice at once. On a phone, the first time you turn it on a line explains what will be read: alert titles, and work summaries only if you turn them on.

**What the alerts say.** A finished turn names the session by its card number and, when it is safe, the topic of its work, for example "삼번 세션, 로그인 버그 수정 작업이 끝났습니다" or "Session 3, Fix login bug, finished." The topic is the session's title; it is left out when it looks like a command, a path or code. An error says which session and gives a short generic reason, never the error text: "오번 세션에서 오류가 발생했습니다: 권한 문제".

**One voice at a time.** Alerts, briefings and summaries play one after another instead of cutting each other off: requests for a decision or input first, then errors, then briefings, then completions and summaries. The same alert is never read twice, an item that waited more than a minute is dropped, and at most five wait (the least urgent are dropped first). While something is being read, a speaker button appears in the top bar: tap it, or press Esc on a computer, to stop everything that is playing or waiting. Hands-free read-backs and listening take the voice for themselves; queued items wait and continue afterwards.

**Result summaries (off by default).** With **Settings > Voice > Spoken alerts > Read work results aloud** on (and spoken alerts on), the app reads a one or two sentence summary when a session finishes a turn (it goes from running to idle or to waiting for your input), for example "Session 2: fixed the failing test and pushed the branch." This reads conversation content aloud, so it is off by default and applies only to the device where you turn it on. Quiet hours silence it like other spoken alerts.

- The summary is made on the PC with the language model of the speech settings: the profile named by `[speech] default_llm`, else the first `[[speech.llms]]` profile (see "Several engines and correction models" above). `default_llm = "none"` turns language-model summaries off. Without a model, when a limit applies, or when the model does not answer within the profile's `timeout_ms` (`[speech.llm] timeout_ms`, 4 seconds by default; raise it for slower models), the app says the "what finished" line instead, such as "Session 2, Fix login bug, finished." The summary replaces that line; it is not read in addition to it.
- The summary is in the app's language even when the agent wrote in another one.
- Before the text goes to the model, the PC removes likely secrets (API keys and tokens, `password=` style values, credentials inside URLs, private keys, long hexadecimal or base64 strings) and, unless **Include commands, file paths and code** is on, code blocks. The model is also told to leave out commands, paths and code in that case. At most 6,000 characters go to the model and at most 200 characters come back.
- Each turn is summarized at most once (later requests for it are answered from memory or with the fixed line), each device may ask 6 times a minute, and the PC makes at most 20 summaries a minute in total. Nothing is requested while the setting is off, and the PC refuses summaries for a device that has not turned it on.
- A device only hears about sessions it can already see; a view-only device gets no summary of a remote shell. The PC logs neither the conversation nor the summary.

Use a model you trust with your conversations: a self-hosted model keeps everything on your own machines, while a cloud API sends the (redacted) text to that provider. Spoken voices are machine-generated.

## 12. The conductor

A conductor is one AI (Claude Code by default, Codex CLI, Gemini CLI or a built-in loop with your own LLM) that oversees the other AI sessions on this PC from the **Conductor** menu. It is off by default, is turned on only in the desktop app on the PC, never answers permission decisions and sends nothing to another session without your confirmation. See [The conductor](conductor.md) for how to enable it, what it can and cannot do, its safety rules and its quota.

## 13. Your to-do list from agents

Agents often need you for something they cannot do themselves: run a command in an administrator shell, sign in somewhere, approve a request in a web console, check a device, provide a value or review a change. Such requests are easy to miss in a long chat, so VCA collects them in a to-do list. The list is kept by the server of each PC (in its data folder), so the desktop app and every paired phone see the same tasks.

The **Alerts** menu has two tabs: **Alerts** (decisions and questions waiting in sessions, as in section 11) and **To-do**. The number on the menu is the pending decisions plus the open tasks an agent is waiting on. A session card shows **Waiting on you** while one of its tasks blocks the agent.

**Where tasks come from.**

- **Asked by the agent.** An agent connected to VCA's MCP server (`vca mcp`, for example the conductor) can call `request_human_action` with a title, steps, an optional command, whether it needs administrator rights, whether the agent is blocked, and an optional due time. Asking twice for the same thing returns the open task. `list_my_requests` shows the agent the state of what it asked. No tool can check a task off or send a result: only you do that.
- **Found in the chat.** When a session finishes a turn, the PC reads the agent's final message for requests addressed to you, in English, Korean, Japanese and Spanish: phrases such as "please run", "you need to", "as administrator", "실행해 주세요", "관리자 PowerShell", "実行してください", "por favor ejecuta". A code block right after such a line becomes the task's command, a list after a line ending in a colon becomes its steps, and links are kept. Closing lines ("let me know if…", "궁금한 점이 있으면 알려 주세요") and offers ("would you like me to…?") are ignored. The same request from the same session is added only once. Messages that were already there when VCA started are not read.
- **Added by you.** **New task** in the To-do tab, or **Add to to-do** (the bookmark button) on any agent message in the chat, which fills in the title, the text and the first code block of that message.

Decisions and questions the agent shows in its own prompt stay in the Alerts tab; they are not copied into the list.

**Optional: finding requests with a language model.** In **To-do settings (this PC)** (desktop app on the PC only) you can also let the PC ask the language model of the speech settings (the `[speech] default_llm` profile, else the first `[[speech.llms]]` profile) to find requests. It is off by default. Likely secrets are removed before the text leaves the PC, at most 6,000 characters are sent, the model is told the message is untrusted data, only a strict JSON answer is used, and the PC makes at most 30 such requests an hour. Use a model you trust with your conversations.

**Working with the list.** Check a task off with its checkbox (and back on from the **Done** filter). Open a task to see its steps and command, **Start** it, **Snooze** it (1 hour, 4 hours, tomorrow morning), move it up or down, **Edit** it (title, steps, command, administrator rights, priority, due time) or **Dismiss** it. Filter by status, session or "needs admin only", or search. **Export Markdown** and **Export CSV** save (and copy) the list, or the tasks of one session when a session is selected in the filter.

**Commands are never run for you.** A command in a task is stored and shown with the same danger check as the remote shell (risky parts highlighted, with the reasons). You can copy it. On the PC itself, for a task that needs administrator rights, **Open admin PowerShell** opens an administrator PowerShell through the normal Windows permission prompt (UAC): the window shows the command and puts it on the clipboard, and you paste it and press Enter yourself. VCA never bypasses UAC, never types the command, and never opens an administrator shell from a phone or another computer; there the task says it must be run on the PC, and you can snooze it as a reminder. On macOS and Linux, copy the command and run it with `sudo` in a terminal.

**Sending the result back.** **Send result** writes the result to the session that asked, as a normal message: your text, command output you paste (or saved earlier with **Paste output**), and files you attach (uploaded the same way as in the message box). **Preview** shows the message first; **Send and check off** sends it and checks the task off.

**Reminders.** The PC reminds you with an in-app notice and, with spoken alerts on, a short generic spoken line (never the task text):

- when a task's due time passes;
- every 30 minutes while an agent waits on you (set the interval in To-do settings; 0 turns it off);
- once when a task has been open for longer than 8 hours (set the hours; 0 turns it off);
- when a snooze ends;
- when the agent finished another turn (or its session ended) after asking: the task then asks **Is this still needed?** with **Still needed** and **Dismiss**.

**Today's tasks.** **Today's tasks** reads a short count of open tasks, blocking ones, ones that need an administrator shell and overdue ones, with fixed sentences and no task text. Set an hour in To-do settings to hear it once a day after that hour on devices with spoken alerts on.

**Permissions.** Any paired device can see the list and hear the digest. Checking off, snoozing, editing, adding tasks and sending results need the command permission. The settings and the administrator shell work only in the desktop app on the PC. Every change is written to the audit log (`task_create`, `task_update`, `task_send_result`, `task_admin_shell`, `task_configure`) with ids only, never the task text. Done and dismissed tasks are kept for 30 days.

## 14. Updating VCA

**Desktop app (Windows, macOS, Linux).** The app updates itself from the project's GitHub Releases. Open **Settings → About**:

- **Check for updates** asks now. The status says you have the latest version, or names the new version with its release notes.
- **Download update** downloads it and checks its signature (a file that is not signed with the project's key is never installed). A progress bar shows the download.
- **Restart to update** installs it. Before anything happens, the app asks for confirmation and tells you what it affects: it stops the VCA server it started, so **sessions VCA started may end**. Sessions you started yourself in a terminal, which VCA only observes, keep running. The app then installs the new version and opens again (on Windows a small installer window shows the progress).
- **Check for updates automatically** (on by default) checks 30 seconds after the app starts and then once a day. When it finds a new version you only see a small notice, **New version X is available**, with a button that opens Settings. Nothing is downloaded or installed until you click.

Problems are shown in plain words, for example when the PC is offline or a download's signature is invalid; **Try again** repeats the step. On Linux, an AppImage replaces itself, and a `.deb` or `.rpm` install is updated as the same kind of package (the system asks for your password). If a release has no package of your install's kind, the app shows the release page so you can download it yourself.

A server you started yourself (`vca daemon`) is never stopped by the app; update it like you installed it.

**iPhone.** Through the App Store, once the app is published there (*coming soon*). A build from source is updated by installing a new build from Xcode.

**Android.** Through Google Play, once the app is published there (*coming soon*). A build from source is updated by installing the new APK (`adb install -r`, or open the APK on the phone); your pairings are kept.
