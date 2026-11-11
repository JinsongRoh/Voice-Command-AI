# Releasing VCA

The desktop app (Windows, macOS, Linux) updates itself from this repository's
GitHub Releases. This page is for maintainers: how to build a release, sign
the updater artifacts, write `latest.json` and publish it. Publishing is done
by hand for now; an automated release workflow comes later.

The phone apps are not updated this way: they are published to the App Store
and Google Play. See [Store and platform signing](#store-and-platform-signing)
for every platform's release artifact.

## Store and platform signing

Release signing is done by the company's release process (MetaVu Co., Ltd.).
**No signing key, certificate, keystore, provisioning profile or password is
in this repository**, and none may be added to it, to an issue or to a build
log. Builds from a fork or from source are unsigned (or signed with your own
identity) and cannot update an official install.

| Platform | Release artifact | Signing |
| --- | --- | --- |
| Windows x86_64 | NSIS installer `*_x64-setup.exe` | Authenticode, publisher MetaVu Co., Ltd., plus the updater's minisign `.sig` |
| macOS (Apple silicon, Intel) | `.dmg`, and `.app.tar.gz` for the updater | Apple Developer ID, notarized and stapled; minisign `.sig` for the updater archive |
| Linux (x86_64, ARM64) | AppImage, `.deb`, `.rpm` | Minisign `.sig` for the in-app updater |
| Android | Android App Bundle (`.aab`) for Google Play | Upload key; Google Play App Signing signs what users install |
| iOS | App Store archive (`.ipa`) uploaded to App Store Connect | Apple Distribution certificate and an App Store provisioning profile |

**Versions and build numbers.** Every release raises the version in all of
these, together: `version` under `[workspace.package]` in `Cargo.toml` (server
and CLI) and `apps/Cargo.toml` (apps), and `version` in
`apps/desktop/package.json`, `apps/mobile/package.json` and
`apps/ui/package.json`. The stores also need a build number that grows with
every upload, even for the same version: the Android `versionCode` and the iOS
build number (`CFBundleVersion`). Tauri derives both from the version unless
`bundle.android.versionCode` or `bundle.iOS.bundleVersion` is set in
`apps/mobile/src-tauri/tauri.conf.json`; set them to upload another build of
the same version. The desktop updater offers a release only when its version
is higher than the installed one.

The rest of this page covers the desktop updater artifacts.

## How the in-app updater works

- The app reads
  `https://github.com/JinsongRoh/Voice-Command-AI/releases/latest/download/latest.json`
  (`plugins.updater.endpoints` in `apps/desktop/src-tauri/tauri.conf.json`).
  That URL always points at the newest release that is **not** a draft or a
  pre-release.
- `latest.json` names the new version, its release notes, and for each
  platform the download URL and the minisign signature of that file.
- The app downloads the file for its platform and checks the signature
  against the public key in `tauri.conf.json` (`plugins.updater.pubkey`).
  A file that does not match is never installed.
- Nothing is installed until the user clicks **Restart to update** and
  confirms. The app first stops the VCA server it started (sessions VCA
  launched may end; sessions the user started in a terminal are not
  affected), then installs and opens again.
  - Windows: the NSIS installer runs in passive mode (a progress window, no
    questions) and reopens the app.
  - macOS: the `.app` is replaced, then the app restarts.
  - Linux (x86_64 and ARM64): an AppImage replaces itself. A `.deb` or `.rpm`
    install is updated with its own package type (the system asks for an
    administrator password through `pkexec`). If the release has no package
    of that type, the app shows the release page instead of an install
    button.

> **The first public release must already contain the updater.** An install
> made from a build without it can never update itself; its users would have
> to download the next version by hand.

## The signing key

Updater artifacts are signed with a minisign key pair made by the Tauri CLI
(`npx tauri signer generate`).

- The **public key** is in `tauri.conf.json`. It is safe to publish.
- The **private key** and its password stay outside the repository, with the
  maintainers (for example `~/.tauri/vca-updater.key`, readable only by its
  owner). Never commit it, paste it into an issue or a log, or copy it to a
  build machine that does not need it.
- **Back it up.** If the private key is lost, installed apps can never accept
  another update: every user would have to reinstall by hand from a build
  with a new public key.
- For a future automated release workflow, the key goes into the repository
  secrets `TAURI_SIGNING_PRIVATE_KEY` (the key file's content) and
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

`bundle.createUpdaterArtifacts` is on, so `tauri build` signs the updater
artifacts with the key from the environment and fails without it. Builds
that are not releases turn it off with `src-tauri/tauri.unsigned.conf.json`:
`pnpm build:unsigned` in `apps/desktop` (the same as `pnpm build`, without
updater artifacts), or `--config '{"bundle":{"createUpdaterArtifacts":false}}'`
on a `tauri build` command line.

## 1. Set the version

The app version is `version` under `[workspace.package]` in
`apps/Cargo.toml`. Raise it (semantic versioning, e.g. `0.2.0`); the
updater offers a release only when its version is higher than the installed
one. Keep the other version fields in step (see
[Versions and build numbers](#store-and-platform-signing)).

## 2. Build and sign

Updater artifacts per platform:

| Platform                 | Updater artifact                       | `latest.json` platforms                           |
| ------------------------ | -------------------------------------- | ------------------------------------------------- |
| Windows x86_64           | `bundle/nsis/*_x64-setup.exe` + `.sig` | `windows-x86_64`, `windows-x86_64-nsis`           |
| macOS Apple silicon      | `*_aarch64.app.tar.gz` + `.sig`        | `darwin-aarch64`, `darwin-aarch64-app`            |
| macOS Intel              | `*_x64.app.tar.gz` + `.sig`            | `darwin-x86_64`, `darwin-x86_64-app`              |
| Linux (x86_64 and ARM64) | `bundle/appimage/*.AppImage` + `.sig`  | `linux-x86_64-appimage`, `linux-aarch64-appimage` |
| Linux (x86_64 and ARM64) | `bundle/deb/*.deb` + `.sig`            | `linux-x86_64-deb`, `linux-aarch64-deb`           |
| Linux (x86_64 and ARM64) | `bundle/rpm/*.rpm` + `.sig`            | `linux-x86_64-rpm`, `linux-aarch64-rpm`           |

Bundles are written to `apps/target/release/bundle/`. Linux entries always
name the package type, so a `.deb` install is never offered an AppImage.

### Windows

In PowerShell, with the key in the environment of this one process:

```powershell
$env:TAURI_SIGNING_PRIVATE_KEY = Get-Content -Raw "$HOME\.tauri\vca-updater.key"
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = Read-Host -MaskInput "Key password"
cd apps/desktop
npx -y pnpm@10.28.0 build
```

This builds the bundled server (`vcad`), the UI and the installers. Next to
`bundle/nsis/Voice Command AI_<version>_x64-setup.exe` you get its `.sig`.
Close the terminal (or remove both variables) afterwards.

### macOS (sign on Windows, the key never goes to the Mac)

1. On the Mac, build the `.app` with Tauri's own updater archive turned off
   (it would need the key), then pack the archive the same way Tauri does:

   ```sh
   cd apps/desktop
   node scripts/build-vcad.mjs
   npx tauri build --bundles app --config src-tauri/tauri.vcad.conf.json \
     --config '{"bundle":{"createUpdaterArtifacts":false}}'
   cd ../target/release/bundle/macos
   COPYFILE_DISABLE=1 tar -czf "Voice Command AI_<version>_aarch64.app.tar.gz" "Voice Command AI.app"
   ```

   The `.app` must be at the root of the archive. `COPYFILE_DISABLE=1` keeps
   macOS metadata files (`._*`) out of it. Use `_x64` in the name for an
   Intel build (`--target x86_64-apple-darwin`, or an Intel Mac). Sign and
   notarize the `.app` with your Apple Developer ID before packing it, as for
   any Mac release.

2. Copy the archive to the Windows machine that holds the key and sign it:

   ```powershell
   $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = Read-Host -MaskInput "Key password"
   cd apps/desktop
   npx tauri signer sign -f "$HOME\.tauri\vca-updater.key" "C:\path\to\Voice Command AI_<version>_aarch64.app.tar.gz"
   ```

   This writes `...app.tar.gz.sig` next to it. The signature covers the
   file's content only, so the file may be renamed afterwards.

### Linux (x86_64 and ARM64)

Build on a Linux machine of each architecture (the AppImage, `.deb` and
`.rpm` are built for the machine's own architecture):

```sh
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/vca-updater.key)"
read -rs -p "Key password: " TAURI_SIGNING_PRIVATE_KEY_PASSWORD; export TAURI_SIGNING_PRIVATE_KEY_PASSWORD
cd apps/desktop
npx -y pnpm@10.28.0 build
```

To keep the key off the build machine, build with
`--config '{"bundle":{"createUpdaterArtifacts":false}}'` instead, copy the
`.AppImage`, `.deb` and `.rpm` to the machine with the key and sign each with
`npx tauri signer sign -f <key file> <file>`, as for macOS.

**On GitHub Actions.** The **Release build (Linux)** workflow
(`.github/workflows/release-linux.yml`) does the unsigned build for both
architectures on GitHub-hosted runners. It runs only when started by hand
(Actions → Release build (Linux) → Run workflow, from the default branch)
and uses no secrets. It builds the `.deb` and the `.rpm`; the AppImage is
not part of the current releases. Each run uploads, per architecture, the
`.deb`, the `.rpm` and a `SHA256SUMS` file as workflow artifacts named
`voice-command-ai-<version>-linux-<x86_64|aarch64>-<deb|rpm|sha256>`, and
prints the checksums in the run summary. Download them, check the
checksums, then sign each package with the key as above. The workflow fails
if the version fields disagree, if a package lacks the app, `vcad` or
`vca`, or if any `.sig` appears.

The workflow builds on Ubuntu 22.04 by default, the oldest GitHub-hosted
runner image for both architectures, because the binaries only run on
systems with the same or a newer glibc than they were linked against. Its
summary prints the newest glibc version each binary needs. Choose Ubuntu
24.04 (glibc 2.39) only when the 22.04 runners are retired, and say so in
the release notes.

## 3. Write `latest.json`

Collect the artifacts with their `.sig` files (for example under
`dist/artifacts/`), write the release notes to a file, then:

```sh
python tools/release/make-latest-json.py --version 0.2.0 \
  --notes-file notes.md --stage dist/release dist/artifacts
```

The script finds each signed artifact, works out its platform from the
file name, and writes `latest.json` with URLs of the form
`https://github.com/JinsongRoh/Voice-Command-AI/releases/download/v0.2.0/<file>`.
`--stage dist/release` copies every artifact, its `.sig` and `latest.json`
there under exactly the names the URLs use (GitHub turns spaces in asset
names into dots), ready to upload. Unsigned files (`.msi`, `.dmg`) are
skipped; upload them separately if you offer them.

If a macOS archive's name does not say its architecture, name it
explicitly: `--artifact darwin-aarch64-app=PATH` (or `darwin-x86_64-app`).
The same option works for every platform, e.g.
`--artifact linux-aarch64-deb=PATH`. Run
`python tools/release/make-latest-json.py --help` for all options. The
script's tests: `python -m pytest tools/release`.

## 4. Publish on GitHub

Create the release with the tag `v<version>` and upload everything in the
staging folder, including `latest.json`:

```sh
gh release create v0.2.0 --title "Voice Command AI 0.2.0" --notes-file notes.md dist/release/*
```

- Publish it as a normal release (not a draft or pre-release): the app only
  looks at the latest release.
- To publish later, upload with `--draft` first, check the assets, then
  publish.
- Check that the manifest is served:
  `curl -L https://github.com/JinsongRoh/Voice-Command-AI/releases/latest/download/latest.json`.

Installed apps find the update at their next automatic check (30 seconds
after start, then once a day) or when the user clicks **Check for updates**
in Settings → About.

## If something goes wrong

- **A broken release:** publish a fixed, higher version. The updater never
  goes back to a lower version.
- **"The update's signature is invalid":** the `.sig` in `latest.json` does
  not belong to the uploaded file, or the file was signed with another key.
  Re-sign and re-upload both.
- **"The latest release has no update for this system":** `latest.json` has
  no entry for that platform; add the artifact and upload a new
  `latest.json`.
