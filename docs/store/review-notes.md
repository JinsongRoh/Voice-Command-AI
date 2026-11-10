# App Review notes (0.1.0)

Notes for Apple App Review and Google Play review. Publisher: **MetaVu Co., Ltd.**

## How a reviewer tests the app

The phone app is a companion to a desktop app on the user's own computer, so
a reviewer has three ways in, in order of effort:

1. **In-app demo mode.** The first screen has a "Try the demo" button. It
   opens sample computers and agent sessions with a "Demo" strip on top. The
   demo makes no network connections. "Exit demo" returns to pairing.
2. **Walkthrough video.** About two minutes, narrated: the desktop app, voice
   commands, approvals from the phone, remote control from a tablet or another
   computer, and the Command Center. Attach it in App Store Connect and link
   it for Google Play.
3. **The real thing.** The desktop app is free and open source. A reviewer
   can install it on a Windows or macOS computer and pair the phone by QR
   code in about two minutes.

Before submitting, the download links below must work: the GitHub release
with the signed desktop installers, and the site pages (home, privacy,
support). Store review checks the privacy policy URL.

## Reviewer note: Apple (App Review Information > Notes)

> Voice Command AI is a companion app for software developers. It controls
> AI coding agents (Claude Code, Codex CLI, Gemini CLI, Antigravity and
> others) that run on the user's own computer, through the free, open-source
> Voice Command AI desktop app. The app has no accounts, no sign-in and no
> cloud service of its own: the phone pairs with the user's computer by QR
> code and every connection is end-to-end encrypted.
>
> Quickest way to review (no computer needed):
> 1. Open the app. On the first screen tap "Try the demo".
> 2. Sample computers and agent sessions open; a "Demo" strip shows on top.
> 3. Open "payments-api" and answer its pending decision with "Yes".
> 4. Tap the microphone: the demo fills in a sample transcript.
> 5. Check Alerts, Command Center and Windows in the bottom bar.
> 6. "Exit demo" returns to the pairing screen.
> The demo makes no network connections and stores nothing.
>
> To test with a real computer (optional):
> 1. Download the free desktop app for Windows or macOS:
>    https://vca.metavu.io (or GitHub Releases:
>    https://github.com/JinsongRoh/Voice-Command-AI/releases/latest).
>    Installers are signed (Windows) and notarized (macOS).
> 2. In the desktop app open Settings > Connection & security > "Connect a
>    phone" and create a QR code (valid for 60 seconds).
> 3. In the phone app tap Pair > "Scan QR code" and scan it. The computer and its
>    agent sessions appear.
> The source code is public at
> https://github.com/JinsongRoh/Voice-Command-AI.
>
> A narrated walkthrough video is attached.
>
> Permissions:
> - Microphone: only while dictating, or while hands-free mode is on with the
>   app open. Audio goes only to the paired computer and the speech engine
>   configured there.
> - Camera: only to scan the pairing QR code.
> - Local network: to connect directly to the user's computer on the same
>   network.
> - Face ID: optional app lock and confirmation before a remote shell opens.
>
> Contact: `<name, e-mail, phone>`

## Reviewer note: Google Play (App access, 500 characters max)

> No account or login. The app pairs with the user's own computer by QR code.
> To review without a computer: on the first screen tap "Try the demo" (sample
> computers and sessions, no network). To test for real, install the free
> desktop app from vca.metavu.io (Windows/macOS) and scan its QR code
> (Settings > Connect a phone). Video: see store listing.
>
> (348 characters.)

Check the length in the Play form before saving. Play also takes the video
as a YouTube link in the store listing.

## Points the reviewer may raise

- **Apple guideline 4.2.7 (remote desktop clients).** The app controls
  software on the user's own computer. Make clear that the computer is owned
  by the user, all agent work runs on that computer, and the app is not a
  thin client for a cloud service or a store of apps.
- **Guideline 2.1 (app completeness).** Demo mode, the video and the free
  desktop app together give a full test path. If a review is rejected
  anyway, the fallback is a company demo computer with a pairing link for
  the reviewer.
- **Guideline 5.1.1 (data collection).** The purpose strings and the privacy
  answers in [privacy-answers.md](privacy-answers.md) state that audio and
  photos go only to the user's own computer.
- **Google Play "App access".** Declare that some functionality is
  restricted and give the instructions above.
