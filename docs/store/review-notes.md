# App Review notes (0.1.0)

Notes for Apple App Review and Google Play review, plus what the company must
set up so a reviewer can actually use the app. Publisher: **MetaVu Co., Ltd.**

## Demo mode: does one exist?

No, not in the phone builds. `apps/ui/src/main.tsx` uses the simulated
servers (`MockTransport`, mock transcriber) only when the UI runs in a plain
browser (development and web demos). Inside the Tauri shell it always starts
the real app (`NativeApp`), so a store build shows the pairing screen until it
is paired with a real computer. There is no "Try the demo" entry.

Options, in order of preference:

1. **Hosted demo computer (works with today's build).** Run `vcad` on a
   company-owned demo computer with a demo agent session, reachable from the
   internet through a connectivity provider (not LAN-only: reviewers are not
   on your network). Give the reviewer a pairing link and the steps below.
   Pairing secrets are one-time: create several pairing links, or one that
   stays valid for the review window, and refresh them for each submission.
   Keep the demo account free of real code and secrets; revoke the reviewer
   devices afterwards.
2. **In-app demo mode (needs development).** A "Try the demo" button on the
   pairing screen that loads the existing `MockTransport` machines. This
   would let reviewers and new users try the app without a computer; it is
   the most robust answer to "the app needs hardware we don't have".
3. **Video only.** A screen recording of pairing and a voice command. Apple
   accepts a video as supporting material, but usually still wants a working
   path; use it together with option 1.

## Reviewer note (paste into App Review Information / Play "App access")

> Voice Command AI is a companion app for software developers. It controls
> AI coding agents that run on the user's own computer, through the free,
> open-source Voice Command AI server ("vcad") installed on that computer. The
> app has no accounts and no cloud service of its own: every connection goes
> end-to-end encrypted from the phone to the user's own computer.
>
> To test, we have prepared a demo computer:
> 1. Open the app and allow notifications if asked.
> 2. On "Add a computer", tap Scan QR code and scan the attached QR image, or
>    open this pairing link on the device: `<vca://pair link>`.
>    If asked for a code, enter: `<authenticator code or instructions>`.
> 3. The demo computer appears with a few agent sessions. Tap a session to
>    see its live chat view.
> 4. Tap the microphone, say "list the files in this project", review the
>    text and tap Send. The agent's reply appears in the session.
> 5. Hands-free mode: Settings > Voice. Say "Voco" and then a command.
>
> Microphone: used only while dictating, or while hands-free mode or the Voco
> wake word is on with the app open. Audio goes only to the paired computer
> and the speech engine configured there.
> Local network: used to connect directly to the user's computer on the same
> network.
> Face ID / biometrics: optional app lock and confirmation before a remote
> shell opens.
> Remote push notifications are not part of this version.
>
> Contact: `<name, e-mail, phone>`

Fill in the placeholders for each submission. Verify the menu names in step 5
against the current build before pasting.

## Points the reviewer may raise

- **Apple guideline 4.2.7 (remote desktop clients).** The app controls
  software on the user's own computer. Make clear that the computer is owned
  by the user, all agent work runs on that computer, and the app is not a
  thin client for a cloud service or a store of apps.
- **Guideline 2.1 (app completeness).** Without a working demo computer the
  app cannot be tested and will be rejected; keep the demo computer and its
  agent session running through the whole review.
- **Guideline 5.1.1 (data collection).** The purpose strings and the privacy
  answers in [privacy-answers.md](privacy-answers.md) state that audio and
  photos go only to the user's own computer.
- **Google Play "App access".** Declare that all or some functionality is
  restricted and give the same instructions as above.
