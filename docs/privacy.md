# Privacy Policy

**Effective date:** 10 November 2026
**Applies to:** Voice Command AI (VCA) 0.1.0 and later: the desktop app for Windows, macOS and Linux, the mobile app for iOS and Android, the server `vcad` and the command-line tool `vca`.
**Controller:** MetaVu Co., Ltd.
**Contact:** [GitHub Issues](https://github.com/JinsongRoh/Voice-Command-AI/issues) for questions; [private vulnerability reporting](../SECURITY.md) for security or privacy problems.

A copy of this policy is published at <https://vca.metavu.io/privacy.html>.

## 1. Summary

- **There is no VCA account and no VCA cloud.** VCA runs on your own computers and phones. MetaVu runs no server that relays your sessions, stores your data or receives your voice.
- **No analytics, advertising, tracking or crash reporting** is built into VCA. MetaVu receives no usage data from the apps.
- **Your phone talks to your own PC.** Everything between a paired device and the server on your PC is end-to-end encrypted (the Noise protocol).
- **Data leaves your devices only to services you choose and set up:** the speech-to-text, language model and text-to-speech engines you configure, the connectivity provider you pick, cloud agent accounts you add, and DNSPilot if you connect it. On Android, Google ML Kit, which scans pairing QR codes, may send usage metrics to Google (section 12).
- **Diagnostics are off by default.** The diagnostic log is kept only on your device, and only if you turn it on. It is never uploaded; you decide whether to copy it and send it to anyone.

## 2. How VCA works

```
phone / other computer ◄── Noise end-to-end encryption ──► vcad on your PC ──► engines you configure
                       (LAN, Tailscale, Cloudflare,                          (STT, LLM, TTS,
                        DNSPilot or your own provider)                        cloud agent providers)
```

The server `vcad` runs on your PC and owns your AI agent sessions. The VCA app on your phone or computer pairs with it once (with a one-time code and, by default, an authenticator code) and then shows those sessions. The desktop app includes the server.

## 3. Data VCA handles and where it goes

| Data | Goes to | When |
|---|---|---|
| Session lists, chat and terminal output, your messages, attachments, dictation audio | Your PC (`vcad`), over the end-to-end encrypted channel | While a paired device is connected |
| Dictation audio and the recognized text | The speech-to-text engine you configured on the PC | Only while you dictate, or while hands-free mode listens (section 4) |
| Recognized text and recent dictation | The correction LLM you configured | Only if you turn on context correction |
| Alert titles, briefings, summaries | The text-to-speech engine you configured on the PC | Only if you choose a PC engine as the voice source; the default is the device's own voice |
| Session names written in Latin letters | The correction LLM you configured | Once per name, to suggest how spoken alerts pronounce it, only if a correction LLM is set up |
| Agent replies (secrets removed first) | The LLM you configured | Only if you turn on spoken result summaries or LLM to-do detection |
| Session previews and summaries (secrets removed first) | The model behind the conductor (your agent's provider or your own LLM) | Only while you use the conductor, which is off by default |
| Prompts for cloud agent sessions and their replies | The cloud agent provider you added an account for | Only for cloud sessions you start |
| Encrypted VCA traffic | The connectivity provider you choose | When a device connects from outside the PC |
| Update checks and downloads | GitHub Releases | Desktop app only, while **Check for updates automatically** is on (the default) and when you check by hand (section 8) |

## 4. Microphone

VCA uses the microphone only for features you start, and the app's lock pauses all of them.

- **Tap to dictate (default) or hold to talk.** Recording starts when you tap the microphone button (or hold it, if you choose hold-to-talk) and stops when you tap again, discard or send. The audio goes over the encrypted channel to your PC, which passes it to the speech-to-text engine you configured. The text appears as you speak.
- **Weak network.** If a recording cannot reach your PC, the app keeps it so you can tap **Retry**, or discard it. Nothing is sent anywhere else.
- **Hands-free mode and the "Voco" wake word** (off by default). While hands-free mode is on, the microphone stays open and the app detects speech on the device. While it waits for the wake word, only a short piece (about the first 2 seconds) of each utterance is sent to your PC's speech-to-text engine to check for "Voco"; nothing else is kept or sent. After the wake word, what you say is transcribed like a dictation. The app does not listen while it is locked, while it speaks, or when you turn hands-free mode off.
- **Teach a pronunciation by voice, and dictating to the conductor**, record only while you press the button, in the same way.
- **Your keyboard's own dictation** (for example the iOS keyboard microphone, Gboard voice typing, Windows **Win+H** or macOS Dictation) is handled by your device's speech service under its own settings, not by VCA.

VCA stores no audio. The speech-to-text engine you chose receives the audio under its own terms: a self-hosted engine keeps it on your machines, a cloud API (such as OpenAI or Google Cloud) receives it under that provider's terms. See [Your own speech engines](speech-engines.md).

## 5. Alerts and notifications

**In version 0.1.0, the phone apps do not use push notifications.** They do not register with Apple Push Notification service or Firebase Cloud Messaging, and no push token leaves the phone. Phones show alerts while the VCA app is open, as notifications created on the phone itself. The desktop app shows system notifications while it runs in the background. While the app is locked, a notification shows only the session name and a generic line, never the request.

**Phone approval alerts (planned, not in this release).** The code contains an opt-in design for notifications while the phone app is closed, through DNSPilot's end-to-end encrypted push relay: the phone would hand its push token to your PC over the encrypted channel, the desktop app on the PC would send DNSPilot a payload encrypted to the phone's key that holds only a session reference, and the visible text would be a fixed, generic line. The server on your PC keeps such a registration (in its `push/` folder) only if a phone registers one, which 0.1.0 store builds cannot do. Before this feature is turned on in a release, this policy will be updated and the change listed in the changelog.

## 6. Connectivity and DNSPilot

By default `vcad` listens only on `127.0.0.1`, so nothing outside the PC can reach it. When you choose direct LAN, Tailscale, Cloudflare Tunnel, DNSPilot or your own provider, that provider carries the connection. VCA treats every provider as an untrusted transport: the traffic inside is end-to-end encrypted between your device and `vcad`, so the provider sees only encrypted data, connection times and network addresses.

**DNSPilot** is a separate service with its own account, terms and privacy policy (it is developed by the team that maintains VCA). If you choose **Connect DNSPilot** in the desktop app, the app talks to the DNSPilot console (`dns.dnspilot.kr`) with your approval to create an app-mode tunnel and register the app's client key; a phone registers its own key with a one-time invite. DNSPilot tokens and the app's client key stay in the device's credential store. **Disconnect DNSPilot** turns off the tunnels the app created and revokes the keys and tokens it registered. To delete your DNSPilot account, its tunnels or registered devices, use the [DNSPilot console](https://dns.dnspilot.kr).

## 7. Engines and cloud agents you connect

**VCA ships no speech servers and no model weights**, and it never sends anything to an engine you did not configure. Engine API keys stay on the PC: paired devices can never send or read them, and the apps learn only engine names, never URLs, models or keys. Stored keys are only sent to the address they were saved with, over https or on the same computer.

**Agents.** Your agent CLIs (Claude Code, Codex and others) talk to their own providers as they always do; VCA does not change that. VCA contacts a cloud agent provider (Jules, Cursor, Devin, GitHub Copilot, Claude managed agents or routines, Codex cloud) only if you add an account for it on the PC. Its credentials stay on the PC.

## 8. Updates

The desktop app checks `latest.json` on GitHub Releases 30 seconds after it starts and then once a day, while **Check for updates automatically** is on (the default), and when you press **Check for updates**. GitHub sees the request (your IP address and the app's request) under [GitHub's privacy statement](https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement). Updates are verified against the app's signing key and installed only when you confirm. The phone apps are updated by the App Store and Google Play.

## 9. Diagnostics and logs

**Diagnostic log on the device (off by default).** Settings → About → **Diagnostic log** keeps, only if you turn it on, a log of the last 7 days on that device: timings, counts, status and error codes for the microphone, recognition, the wake word, the app lock and the connection. It never holds audio, transcripts, prompts, file paths, keys or addresses; values that look like them are masked. It is **never uploaded**. You can copy it and send it to someone yourself; if you attach it to a public GitHub issue, everyone can read it. Turning it off clears it.

**Logs on the PC.** `vcad` keeps a connection log (`remote/audit.log`: connection and sign-in attempts) and an activity log (`audit.jsonl`: remote shells, the conductor, window restore and arrangement, to-do changes). For messages sent to sessions, the activity log records only their length, never their text. Remote shell commands are recorded with secrets masked. Authenticator codes are never written. The desktop app keeps the bundled server's log (`host/vcad.log`). These logs stay on the PC.

## 10. What is stored on your PC

`vcad` keeps its files in your user profile. You can move them with the `VCA_CONFIG` and `VCA_DATA_DIR` environment variables.

| OS | Configuration | Data |
|---|---|---|
| Windows | `%APPDATA%\vca\vca\config\` | `%APPDATA%\vca\vca\data\` |
| macOS | `~/Library/Application Support/dev.vca.vca/` | `~/Library/Application Support/dev.vca.vca/` |
| Linux | `~/.config/vca/` | `~/.local/share/vca/` |

In the data folder:

| Path | Contents |
|---|---|
| `remote/keys/` | The server's private key and the key that signs access tokens. |
| `remote/devices.json`, `remote/tokens.json` | Paired devices (names, public keys, permissions) and refresh tokens. |
| `remote/totp.json` | Your authenticator secret and hashes of your backup codes. |
| `remote/audit.log` | Connection and sign-in attempts. |
| `audit.jsonl` (and `audit.jsonl.1`) | The activity log (section 9). Rotates at 5 MB. |
| `tasks.json` | Your to-do list. Done and dismissed tasks are kept for 30 days. |
| `restore.json` | Saved windows and screen layouts: titles, folders, positions and agent session ids. |
| `session-names.json`, `hidden-sessions.json` | Names you gave sessions and sessions you hid. |
| `conductor.json`, `conductor/` | Conductor settings, its instruction file and MCP configuration. |
| `speech/`, `tts/` | Speech and TTS preferences, your commonly misheard words (`speech/personal.json`), and engine API keys stored with `vca speech login` or `vca tts login`. |
| `pronunciations.json` | Your pronunciation dictionary for spoken alerts. |
| `cloud/` | Cloud agent account credentials, if you added any. |
| `push/` | Reserved for phone approval alerts (section 5); empty with 0.1.0 store builds. |

Attachments you send to a session are saved in that project's folder under `.vca/attachments/`. VCA adds `/.vca/` to the project's `.git/info/exclude` so they are not committed by accident.

On macOS and Linux these files are created readable only by your user (`0600`); on Windows they inherit the permissions of your user profile folder. Never copy the data folder to another person or machine.

## 11. What is stored in the apps

| Data | Where |
|---|---|
| The device's private key and tokens for each paired PC, DNSPilot tokens and the app's tunnel client key | The OS credential store: Windows Credential Manager, macOS and iOS Keychain, or the Secret Service on Linux (GNOME Keyring, KWallet), under the service name `io.metavu.voicecommandai.app`. If no credential store is usable (for example a Linux session without a Secret Service) and on Android, they are stored as files in the app's private data folder. |
| Paired PCs (names and addresses) | The app's data folder, `machines/` |
| The bundled server's state and log (desktop) | The app's data folder, `host/` |
| Settings, the last 100 voice answers given in hands-free mode, and the diagnostic log if you turned it on | The app's local storage on that device |

The app's data folder is named after the app id: `io.metavu.voicecommandai.desktop` on desktops (in `%APPDATA%` on Windows, `~/Library/Application Support` on macOS, `~/.local/share` on Linux) and the app's private storage on phones.

## 12. Third parties

VCA works with these services only when you use the feature named. Each processes data under its own terms.

| Service | Used for | Data it receives |
|---|---|---|
| Speech, LLM and TTS providers you configure (self-hosted, OpenAI, Google Cloud, Anthropic or another) | Speech recognition, correction, summaries, spoken alerts | What section 3 lists for that engine |
| Cloud agent providers you add (Jules, Cursor, Devin, GitHub Copilot, Anthropic, OpenAI Codex) | Cloud agent sessions | Your prompts for those sessions and the repository you name |
| Connectivity providers you choose (Tailscale, Cloudflare, DNSPilot, your own) | Reaching your PC from outside | Encrypted traffic, connection times, network addresses |
| DNSPilot | App-mode tunnels, if you connect it | Your DNSPilot account data, the app's public key, encrypted traffic |
| Google ML Kit (Android) | Scanning pairing QR codes | Google's ML Kit terms say it may contact Google servers and send metrics about the performance and use of its APIs. VCA does not add to or read this data. On iOS the scanner uses the system camera framework. |
| GitHub | Desktop updates; issues you open | Update requests; whatever you post |
| Your device's keyboard dictation service | Only if you dictate with the keyboard | Your speech, under the device's settings |

## 13. Retention

MetaVu holds no personal data from the apps, so there is nothing for MetaVu to retain or delete. On your own devices: the activity log rotates at 5 MB (one older file is kept); done and dismissed to-dos are kept for 30 days; the diagnostic log keeps at most 7 days; hands-free mode keeps its last 100 answers; everything else stays until you delete it (section 15). Providers you connect keep data under their own policies.

## 14. Mobile permissions

The phone apps ask for a permission when a feature first needs it.

| Permission | Why | When it is used |
|---|---|---|
| Microphone | Dictation, hands-free mode and the wake word, teaching a pronunciation | Only while you dictate or hands-free mode listens (section 4) |
| Camera | Scanning a pairing QR code, and **Take photo** | Only while the scanner or the camera is open |
| Photo library (iOS) | **Photo library** in the attachment menu | Only the photos you pick are read and sent to your PC |
| Notifications | Alerts about sessions that need you, shown by the app on the phone | While the app is open |
| Face ID or biometrics | Unlocking the app; confirming it is you before a remote shell opens or a risky command runs | Only for those actions |

Android also declares network access (to reach your PC) and vibration; these need no prompt.

## 15. Your choices: delete everything

1. **Undo changes to other programs** (in the desktop app on the PC): uninstall the agent hooks in **Settings → Alerts → Agent connection** (or `vca hooks uninstall`); turn off **Start VCA when I sign in** and **Restore at sign-in**; press **Disconnect DNSPilot** if you connected it. Remove the `vca shell-init` line from your shell profile if you added it.
2. **Clear the diagnostic log:** Settings → About → **Diagnostic log**, turn it off (this clears it) on each device.
3. **Revoke and stop:** revoke paired devices (`vca devices`, `vca revoke <device-id>`, or Settings → Paired devices), then stop `vcad`.
4. **Delete the server's folders:** the configuration and data folders in section 10 (and the folders named by `VCA_CONFIG` or `VCA_DATA_DIR`, if you set them).
5. **Delete the desktop app's data:** uninstall the app, delete its data folder (`io.metavu.voicecommandai.desktop`) and remove the desktop shortcut it created. Delete the credential store entries under `io.metavu.voicecommandai.app`: Windows Credential Manager (Windows Credentials), Keychain Access on macOS, or Seahorse or KWallet on Linux.
6. **Delete the phone app:** uninstalling the app removes its data and its keychain or keystore entries on most devices. On iOS, Keychain items can survive an uninstall; pairing again creates new keys, and revoking the old device on the PC makes them useless.
7. **Delete attachments:** remove the `.vca/` folder in each project, and the `/.vca/` line from `.git/info/exclude` if you like.
8. **Elsewhere:** delete the data your speech, LLM, TTS or cloud agent provider keeps; remove tunnels and devices, or delete your account, in the [DNSPilot console](https://dns.dnspilot.kr); remove devices you added for VCA in your Tailscale or Cloudflare account.

Because MetaVu holds no personal data from the apps, requests to access, correct or delete such data can be fulfilled entirely with the steps above. For anything else, contact us (section 19).

## 16. Children

VCA is a tool for software developers and is not directed to children. It collects no personal data for MetaVu from anyone, including children.

## 17. Security

Pairing uses one-time codes and, by default, an authenticator (TOTP) code. Device traffic is end-to-end encrypted with the Noise protocol, tokens are short-lived, devices can be revoked one by one, and a kill switch drops every remote connection. The apps open locked and unlock with the device's own sign-in. See [Security setup](security-setup.md). Report vulnerabilities through [private vulnerability reporting](../SECURITY.md).

## 18. Changes to this policy

We publish changes to this policy in this file and on <https://vca.metavu.io/privacy.html>, with a new effective date. Significant changes are also listed in the [changelog](../CHANGELOG.md).

## 19. Contact

MetaVu Co., Ltd.

- Questions about this policy: open an issue on [GitHub](https://github.com/JinsongRoh/Voice-Command-AI/issues). Issues are public; do not include personal data.
- Security or privacy problems in VCA: use [private vulnerability reporting](../SECURITY.md).
