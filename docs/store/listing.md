# Store listing: Voice Command AI 0.1.0

Draft copy for the App Store and Google Play. Publisher: **MetaVu Co., Ltd.**
Korean copy: [listing.ko.md](listing.ko.md). Privacy answers:
[privacy-answers.md](privacy-answers.md). App Review notes:
[review-notes.md](review-notes.md).

Character limits are given in brackets; counts were checked when this draft
was written. Do not name third-party agent or tunnel products in the listing:
it invites trademark rejections and dates quickly.

## Languages

- **App Store Connect:** Primary language: English (U.S.). App languages:
  English, Korean, Japanese, Spanish.
- **Google Play:** Primary language: English (U.S.) – en-US. App languages:
  English, Korean, Japanese, Spanish.

English is the default and primary language: this file is the source copy, and
[listing.ko.md](listing.ko.md) is its Korean localization. The app itself
follows the device language among these four and falls back to English.

## Names

| Field | Text | Count / limit |
|---|---|---|
| App name (App Store and Play title) | Voice Command AI | 16 / 30 |
| Subtitle (App Store) | Voice control for AI coding | 27 / 30 |
| Short description (Play) | Command the AI coding agents on your own PC by voice, from anywhere. | 68 / 80 |

## Promotional text (App Store, 170)

Your voice. Every agent. One command center. Watch and command the AI coding agents running on your own PC, from your phone, end-to-end encrypted.

## Keywords (App Store, 100, comma separated, no spaces)

```
coding,agent,developer,terminal,dictation,remote,CLI,speech,programming,assistant,hands-free,LLM
```

Words already in the app name (voice, command, AI) are indexed anyway and are
left out.

## Full description (App Store and Play, 4000)

Voice Command AI puts every AI coding agent running on your computer in your pocket. See each session as a live card, pick one, speak, and your words go straight to that session. No keyboard focus, no windows stealing your keystrokes.

Voice Command AI is a companion app. It connects to the free, open-source Voice Command AI server (vcad) running on your own Windows, Mac or Linux computer. There is no Voice Command AI cloud and no account: your sessions, voice and keys stay between your phone and your computer.

SESSION CARDS
- Every agent session on your computer as a live card: project, agent, status, alerts and running sub-agents.
- Start a new session in a project folder, or pick up sessions that are already open.
- Stream any session as a readable chat or as the raw terminal.

VOICE FIRST
- Tap the microphone, speak, review the transcript, send.
- Hands-free mode and the "Voco" wake word for when your hands are busy.
- Mixed-language speech and a developer vocabulary for technical terms.
- Speech recognition runs on the engine you choose on your computer: a self-hosted server or your own cloud account.

ALERTS YOU CAN ACT ON
- One list of every session waiting for a decision or input; answer with one tap.
- Spoken alerts and a status briefing in the device voice or your own text-to-speech engine.
- A to-do list of things your agents need you to do yourself.

MADE FOR THE PHONE
- Swipe between sessions, attach photos, and pick a light, dark or black theme.
- Pair several computers (home, office, build server) with one app.
- A locked-down remote shell for admin devices, behind your biometric lock and an authenticator code.

SECURE BY DESIGN
- Pair by scanning a QR code shown on your computer.
- End-to-end encryption between your phone and your computer, whichever network carries it: your local network, a VPN or a tunnel you choose.
- Short-lived tokens, two-factor authentication, an audit log and a kill switch on the computer.
- No analytics, no ads, no tracking.

WHAT YOU NEED
- A computer running the Voice Command AI server (free, open source).
- An AI coding agent on that computer.
- For voice input: a speech-to-text engine you set up on the computer.

Voice Command AI is open source under the MIT license.

## What's new in 0.1.0

First release.
- Live session cards for the AI coding agents on your computer.
- Voice input with tap-to-dictate, hands-free mode and the Voco wake word.
- Alerts with one-tap answers, spoken briefings and a to-do list.
- QR pairing, end-to-end encryption and an app lock with your biometric sign-in.

## Categories

| Store | Primary | Secondary |
|---|---|---|
| App Store | Developer Tools | Productivity |
| Google Play | Tools (app) | (none) |

`tauri.conf.json` already sets `bundle.category` to `DeveloperTool`.

## Content rating notes

- No violence, sexual content, profanity, gambling, drugs or user-to-user
  communication with strangers. No ads, no in-app purchases, no location.
- No unrestricted web access: the app shows only its own interface and the
  user's own sessions.
- The app shows text and terminal output from the user's own coding agents.
  That content comes from the user's own computer, not from other users.
- Expected result: App Store 4+, IARC "Everyone" / PEGI 3 / GRAC All.
- Google Play target audience: 18 and over (a developer tool); not designed
  for children.

## Store assets still needed

- Screenshots: iPhone 6.9" (1320 x 2868) and 6.5", iPad 13" only if iPad
  support stays enabled; Play phone screenshots (at least 2) and a
  1024 x 500 feature graphic.
- Privacy policy URL and support URL (public pages; `docs/privacy.md` is the
  source for the policy).
- App icon 1024 x 1024 without transparency (App Store).
