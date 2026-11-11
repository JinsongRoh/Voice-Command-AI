# Contributing to Voice Command AI

Thank you for helping. Code, agent adapters, translations, developer vocabulary, bug reports and documentation fixes are all welcome.

Before you start:

- Read the [code of conduct](CODE_OF_CONDUCT.md).
- For a security problem, do **not** open an issue: follow the [security policy](SECURITY.md).
- For a larger change, open an issue first so we can agree on the approach before you spend time on it.

## Repository layout

| Path | What it is |
|---|---|
| `crates/` | The Rust core: `vca-server` (the `vcad` daemon), `vca-cli` (the `vca` command), `vca-core`, `vca-net`, `vca-pty`, `vca-adapters`, `vca-broker`, `vca-speech`, `vca-providers`, `vca-cloud`, `vca-client`. One Cargo workspace, at the repository root. |
| `apps/ui/` | The shared React and TypeScript front end used by the desktop and mobile apps. |
| `apps/desktop/`, `apps/mobile/`, `apps/shell/` | The Tauri 2 apps and their shared Rust shell. A second Cargo workspace, `apps/Cargo.toml`. |
| `lexicon/` | The developer vocabulary used to correct speech recognition. |
| `tools/` | Helper tools, such as the reference TTS server. |
| `docs/` | User and project documentation. |

See [docs/architecture.md](docs/architecture.md) for how the parts fit together.

## Development setup

### Rust

Install Rust with [rustup](https://rustup.rs). The repository's `rust-toolchain.toml` selects the stable channel with `rustfmt` and `clippy`, so rustup installs the right toolchain the first time you run `cargo` in the repository.

### Node and pnpm

The UI and the desktop app need Node.js 20.19 or later (CI uses Node 22) and pnpm **10.28.0**, the version pinned in `packageManager` of `apps/ui/package.json` and `apps/desktop/package.json`. Either enable it through Corepack:

```bash
corepack enable
corepack prepare pnpm@10.28.0 --activate
```

or run it without installing: `npx pnpm@10.28.0 <command>`.

### Tauri prerequisites

The desktop and mobile apps are built with Tauri 2. Follow the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your OS. In short:

- **Windows:** the Visual Studio Build Tools with "Desktop development with C++", and the Microsoft Edge WebView2 runtime (included in Windows 10 and 11).
- **macOS:** the Xcode command line tools (`xcode-select --install`); for the iOS app, full Xcode.
- **Linux (Debian or Ubuntu):** the WebView, audio and D-Bus development packages, as used in CI:

  ```bash
  sudo apt-get install -y libwebkit2gtk-4.1-dev librsvg2-dev libasound2-dev libdbus-1-dev pkg-config
  ```

- **Android:** Android Studio with the SDK and NDK, as described in the Tauri prerequisites.

The server and CLI alone (`crates/`) need only Rust (and, on Windows, the C++ build tools).

### Build and run

The [user guide](docs/user-guide.md) explains how to install `vcad` and `vca`, build the apps and pair a device. The Tauri apps embed the built UI, so build `apps/ui` (`pnpm install && pnpm build`) before you build or test the `apps/` workspace.

## Checks to run before you open a pull request

CI runs the same checks on Linux, macOS and Windows. Run them locally first.

**Rust core** (repository root):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

**Tauri apps** (the second workspace; build `apps/ui` first):

```bash
cargo fmt --manifest-path apps/Cargo.toml --all --check
cargo clippy --manifest-path apps/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path apps/Cargo.toml --workspace
```

**UI** (`apps/ui`):

```bash
pnpm install --frozen-lockfile
pnpm check
```

`pnpm check` runs, in order, `pnpm lint` (ESLint), `pnpm format:check` (Prettier), `pnpm typecheck`, `pnpm test` (Vitest) and `pnpm build`. Fix formatting with `pnpm format`.

**Desktop app scripts** (`apps/desktop`): `pnpm test`.

Some tests stop real processes. They run only when `VCA_PROCESS_CONTROL_TESTS=1` is set, which CI does on its disposable virtual machines. Never set it on your own workstation.

## Commits and pull requests

- Write commit messages, code comments, documentation and pull requests in **English**. Other languages belong only in translation and vocabulary files.
- Use conventional prefixes with an optional scope: `feat:`, `fix:`, `docs:`, `test:`, `refactor:`, `chore:`, for example `feat(ui): ...` or `fix(pty,server): ...`.
- Keep each commit focused on one change, and describe the user-visible effect.
- Update the documentation (and [CHANGELOG.md](CHANGELOG.md), under the unreleased version) when you change behaviour.
- Fill in the pull request checklist.

## Translations

The app's text lives in `apps/ui/src/i18n/`: `en.json` (the source), `ko.json`, `ja.json` and `es.json`.

- Add every new key to **all four** files, with the same key names and placeholders (`{name}`). A key missing in one language falls back to English, but please keep the files in parity.
- Use only keys that exist in `en.json`; a test checks this.
- Keep the tone short and plain, and keep product names and commands (`vca`, `vcad`, `pnpm`) untranslated.

Voice phrases that the app recognizes (for example hands-free confirmations and the status briefing phrases) are listed in the [user guide](docs/user-guide.md); a change there needs a matching documentation update.

### Developer vocabulary

`lexicon/` holds the canonical spelling of technical terms and the ways speech recognition mishears them, one folder per spoken language (`en`, `ko`, `ja`, `zh`, `es`, `de`, `fr`). Any `*.toml` file in a language folder is loaded. See [lexicon/README.md](lexicon/README.md) for the file format.

## Adding an agent adapter

Agents are described in `crates/vca-adapters` (see [docs/architecture.md](docs/architecture.md), section 5).

- **Most agents need only a generic spec.** If the agent is a terminal program, add an `AgentKind` variant in `crates/vca-core/src/session.rs` and an `AgentSpec` entry in `AgentSpec::builtin()` in `crates/vca-adapters/src/generic.rs`: display name, the program to launch, its process names and, for agents that run under Node or Python, script markers. The generic adapter then discovers it, launches it as a managed session and reads prompts and waiting states from its output.
- **A dedicated adapter** is needed for hooks, transcripts on disk or a native input channel (like `claude/` and `codex/`). Implement the `AgentAdapter` trait in `crates/vca-adapters/src/adapter.rs` and register it in `builtin_adapters()` and `adapter_for()`.
- Then update the places that list agent kinds: the CLI output (`crates/vca-cli`), session restore (`crates/vca-server/src/restore`), and the UI's agent names and types (`apps/ui/src/protocol/`). The compiler points out most of them.
- Add detection tests (see `crates/vca-adapters/tests/`) and document the agent in the user guide.
- Only use an agent's official, documented interfaces, and never add flags that bypass the agent's own permission prompts.

## Licensing of contributions

VCA is licensed under the [MIT License](LICENSE). By submitting a contribution you agree that it is licensed under the same MIT License (inbound = outbound), and that you have the right to submit it. Do not add code, assets, voices or model weights whose license is incompatible with MIT. When you add a dependency, check its license; see [docs/open-source.md](docs/open-source.md) for how third-party licenses are tracked.

## Never commit secrets

Do not commit API keys, tokens, pairing links, authenticator secrets or backup codes, private keys, `.env` files, or configuration that contains them. Keep personal paths, machine names, internal hostnames and IP addresses out of code, tests, logs and screenshots; use placeholders such as `example.com`. If you committed a secret by accident, revoke it first, then tell us through a [private security report](SECURITY.md).
