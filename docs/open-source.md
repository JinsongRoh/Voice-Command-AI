# Open-source credits

Voice Command AI (VCA) is released under the [MIT License](../LICENSE). It is
built on the work of many open-source projects. This page names the main ones
and explains how their licenses are honoured. The complete, generated list of
every component, with versions, copyright notices and license texts, is in
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

## Key projects

| Project | Used for | License |
| --- | --- | --- |
| [Rust](https://www.rust-lang.org) and its standard library | the server (`vcad`), CLI (`vca`) and app core | MIT OR Apache-2.0 |
| [Tauri](https://tauri.app), [Wry](https://github.com/tauri-apps/wry), [Tao](https://github.com/tauri-apps/tao) and Tauri plugins | desktop and mobile app shells | MIT OR Apache-2.0 (Tao: Apache-2.0) |
| [Tokio](https://tokio.rs) and [Hyper](https://hyper.rs) | async runtime and HTTP | MIT |
| [tokio-tungstenite](https://github.com/snapview/tokio-tungstenite) | WebSocket transport | MIT |
| [snow](https://github.com/mcginty/snow) | Noise protocol for pairing and encrypted links | Apache-2.0 OR MIT |
| [dalek-cryptography](https://github.com/dalek-cryptography) (x25519, curve25519) and [RustCrypto](https://github.com/RustCrypto) (ChaCha20-Poly1305 and others) | cryptography | BSD-3-Clause; MIT OR Apache-2.0 |
| [rustls](https://github.com/rustls/rustls), [ring](https://github.com/briansmith/ring) and [webpki-roots](https://github.com/rustls/webpki-roots) | TLS | Apache-2.0 OR ISC OR MIT; Apache-2.0 AND ISC; CDLA-Permissive-2.0 |
| [serde](https://serde.rs), [rmp-serde](https://github.com/3Hren/msgpack-rust) and [toml](https://github.com/toml-rs/toml) | data formats | MIT OR Apache-2.0 (rmp-serde: MIT) |
| [portable-pty](https://github.com/wezterm/wezterm) | terminals for agent sessions | MIT |
| [cpal](https://github.com/RustAudio/cpal) | microphone capture in the apps | Apache-2.0 |
| [clap](https://github.com/clap-rs/clap) and [tracing](https://github.com/tokio-rs/tracing) | CLI and logging | MIT OR Apache-2.0 (clap); MIT (tracing) |
| [React](https://react.dev) | user interface | MIT |
| [xterm.js](https://xtermjs.org) | terminal view | MIT |
| [MessagePack for JavaScript](https://github.com/msgpack/msgpack-javascript) | wire protocol in the UI | ISC |
| [Inter](https://rsms.me/inter/), [JetBrains Mono](https://www.jetbrains.com/lp/mono/) and [Pretendard](https://github.com/orioncactus/pretendard) | fonts (via [Fontsource](https://fontsource.org)) | SIL Open Font License 1.1 |
| [AndroidX](https://developer.android.com/jetpack/androidx), [Material Components](https://github.com/material-components/material-components-android), [Kotlin](https://kotlinlang.org) and [Jackson](https://github.com/FasterXML/jackson) | Android app runtime | Apache-2.0 |
| [Google ML Kit](https://developers.google.com/ml-kit) barcode scanning | pairing QR codes in the Android app | [ML Kit Terms](https://developers.google.com/ml-kit/terms) (proprietary, free to redistribute) |
| [Feather](https://feathericons.com) | a few icon outlines | MIT |

## How the licenses are complied with

- **Generated, not hand-kept.** `python tools/licenses/generate.py` reads
  `cargo metadata` and `cargo tree` for both Rust workspaces, the UI's
  production dependencies from `apps/ui/pnpm-lock.yaml` and the Android Gradle
  files. It writes `THIRD_PARTY_NOTICES.md` and the data for the in-app screen.
  `--check` fails when either file is stale, so CI can catch a dependency
  change without updated notices.
- **Notices travel with the apps.** The desktop installers include `LICENSE`
  and `THIRD_PARTY_NOTICES.md`. The desktop and mobile apps show every
  component, its license text and its repository offline under
  **Settings > Open-source licenses**.
- **Copyright notices are kept.** For MIT, BSD, ISC and Zlib components, the
  copyright lines from each component's license files are listed with it, and
  the license text is reproduced once.
- **Dual-licensed components** (for example `MIT OR Apache-2.0`) are used
  under one of the offered licenses, shown in the notices next to the declared
  expression.
- **MPL-2.0 components** (`cssparser`, `cssparser-macros`, `dtoa-short`,
  `option-ext`, `selectors`) are used unmodified; the notices link to their
  source code, as the license requires.
- **Bundled native code** is credited too: the Microsoft Edge WebView2 loader
  on Windows (BSD-3-Clause) and libdbus on Linux (Academic Free License 2.1).
- **Fonts** keep their names and copyright notices, and are only distributed
  as part of the app, as the SIL Open Font License 1.1 allows.

## What is not bundled

VCA ships no speech-to-text, text-to-speech or language model engines and
no model weights.

- Speech, correction LLM and TTS engines are services you choose and set up
  yourself, locally or with your own cloud account (see
  [Your own speech engines](speech-engines.md)). The same applies to
  connectivity providers such as DNSPilot, Tailscale or Cloudflare Tunnel.
- `tools/stt-server` (on faster-whisper and CTranslate2, MIT) and
  `tools/tts-server` (for MeloTTS, MIT, or Supertonic: code MIT, weights
  OpenRAIL-M) are optional reference servers. They are not part of the apps
  or their installers, and they download no models themselves.
- **You are responsible for the licenses and terms of the engines, models and
  services you install or connect**, including use restrictions and any
  requirement to disclose machine-generated speech.

## Updating the notices

After changing a dependency (`Cargo.lock`, `apps/Cargo.lock`,
`apps/ui/pnpm-lock.yaml` or a Gradle file), install the UI dependencies and run:

```sh
python tools/licenses/generate.py
python tools/licenses/generate.py --check
python -m pytest tools/licenses
```
