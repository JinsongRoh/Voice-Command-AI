# Privacy answers: App Store and Google Play (0.1.0)

Draft answers for the App Store privacy details ("nutrition label") and the
Google Play Data safety form, derived from the code of the phone app as of
0.1.0. Publisher: **MetaVu Co., Ltd.** The user-facing source is
[docs/privacy.md](../privacy.md). Have the answers confirmed before
submission; the points marked **Confirm** need a company decision.

## How the app handles data (facts from the code)

- **No developer server.** The app talks only to the user's own computer
  running `vcad`, over a Noise `XX` channel (X25519, ChaCha20-Poly1305,
  SHA-256) that is end-to-end encrypted between the phone and that computer.
  MetaVu does not receive sessions, prompts, audio, photos, keys or usage.
- **No analytics, ads, crash reporting or tracking SDKs** in the app's own
  code. The diagnostics log is off by default, stays on the device and is
  never uploaded (`apps/ui/src/diag/log.ts`); the user can copy it by hand.
- **Microphone audio** is captured only while the user dictates, or while
  hands-free mode or the Voco wake word is on with the app open. It goes
  encrypted to the user's computer, which passes it to the speech-to-text
  engine the user configured there (self-hosted or the user's own cloud
  account). The app holds no engine URLs or keys.
- **Photos and camera:** a photo the user takes or picks is sent to a
  session on the user's computer. The camera also scans the pairing QR code.
- **Biometrics** are checked by the OS (Face ID, fingerprint); the app gets
  only pass or fail.
- **Stored on the phone:** device private key and tokens per paired computer
  (iOS Keychain; Android app-private files), paired computers (names and
  addresses), settings, the last 100 hands-free voice answers (local storage).
- **Notifications** are local, shown while the app runs. Remote push (APNs,
  FCM) is not built into 0.1.0 (the `native-push` Cargo feature is off).
- **Connectivity:** traffic travels over the network path the user chooses
  (local network, a VPN, or a tunnel provider). Those providers see only
  ciphertext, times and IP addresses.
- **Android only, third-party SDK:** QR scanning uses Google ML Kit barcode
  scanning (via the Tauri barcode-scanner plugin). Google states that ML Kit
  may send device and app metadata and API performance metrics to Google.
  On iOS the scanner uses Apple's camera framework only.
- **Optional DNSPilot tunnel enrollment (decided 2026-11-10):** the app
  contains an optional "app-mode tunnel" setup that signs in to the DNSPilot
  console (OAuth device flow) and registers a per-install public client
  certificate and tunnel host names there. DNSPilot comes from the team that
  maintains VCA, so this counts as collection by the developer and is
  declared below: Identifiers (device ID and user ID), purpose App
  functionality, linked to the user, not used for tracking, optional (only
  if the user connects DNSPilot).

**Principle stated to users (privacy policy section 1):** VCA is free and
open source and runs on the user's own devices. Used that way, MetaVu
collects no personal data. DNSPilot, which is optional, is the only VCA team
service that receives data. Third parties outside the team: Google ML Kit on
Android, GitHub for desktop update checks, and the engines and providers the
user configures.

## App Store: privacy details

**Data Used to Track You:** none.
**Data Linked to You:** Identifiers: User ID and Device ID, purpose App
Functionality. Only collected when the user connects DNSPilot.
**Data Not Linked to You:** none.

Answer **"Yes, we collect data from this app"** and declare only the two
DNSPilot identifiers above (not used for tracking). Everything else stays out
of the label. Apple counts data as collected only when it leaves the device
in a way the developer or its partners can access beyond real-time
servicing. All other app traffic ends at the user's own computer, end-to-end
encrypted, and the iOS build has no third-party SDK that collects data.

Privacy manifest: the app and its Rust/Tauri dependencies use
required-reason APIs (for example file timestamps, `UserDefaults` via the
WebView, system boot time). Xcode's privacy report on the archive will list
what needs a `PrivacyInfo.xcprivacy`; add one to the iOS project if the
report or App Store Connect asks for it.

## Google Play: Data safety

| Question | Answer |
|---|---|
| Does your app collect or share any of the required user data types? | **Yes**, because of the ML Kit SDK (see below). Without ML Kit's disclosure the answer would be No. |
| Is all of the user data collected by your app encrypted in transit? | **Yes** |
| Do you provide a way for users to request that their data is deleted? | **No account exists**; data stays on the device and the user's own computer. Uninstalling removes the app's data. (Select the option that fits; there is no server-side data to delete.) |

Data types (from Google's published ML Kit data disclosure; **check the
current version of Google's "ML Kit: Data disclosure" page before
submitting**):

| Data type | Collected | Shared | Ephemeral | Required | Purpose |
|---|---|---|---|---|---|
| App info and performance: Diagnostics | Yes (by ML Kit) | No | No | Required (cannot be turned off) | Analytics |
| Device or other IDs | Yes (by ML Kit) | No | No | Required | Analytics |

Not collected (they go only to the user's own computer, end-to-end
encrypted, and MetaVu cannot read them):

| Data type | Reason it is not "collected" |
|---|---|
| Audio: voice or sound recordings | Sent end-to-end encrypted to the user's own computer, not to MetaVu. |
| Photos and videos | Same: only the photo the user picks goes to the user's computer. |
| Messages / other user-generated content (prompts, session text) | Same. |
| Files and docs (attachments) | Same. |
| Personal info, location, contacts, financial, health, web history | Not accessed. |

**Confirm:** Google's guidance counts data that leaves the device as
"collected"; data sent end-to-end encrypted to a destination the developer
cannot read is generally not declared. If the company prefers the
conservative reading, declare Audio, Photos, Files and Messages as collected,
not shared, purpose App functionality, and processed on the user's own
computer.

## Permissions summary (for both review forms)

| Permission | iOS key | Android | Why |
|---|---|---|---|
| Microphone | `NSMicrophoneUsageDescription` | `RECORD_AUDIO` | Dictation, hands-free mode, Voco wake word |
| Camera | `NSCameraUsageDescription` | `CAMERA` | Pairing QR code, Take photo |
| Photo library | `NSPhotoLibraryUsageDescription` | (system picker, no permission) | Attach a picked photo |
| Local network | `NSLocalNetworkUsageDescription` | (none) | Direct connection to the user's PC on the LAN |
| Face ID / biometrics | `NSFaceIDUsageDescription` | `USE_BIOMETRIC` (from androidx.biometric) | App lock, confirming remote shell and risky commands |
| Notifications | runtime prompt | `POST_NOTIFICATIONS` | Alerts while the app runs |
| Network | (none) | `INTERNET` | Reaching the user's PC |
| Vibration | (none) | `VIBRATE` (scanner plugin) | Feedback on a scanned code |
