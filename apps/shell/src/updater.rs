//! Desktop only: in-app updates from the project's GitHub Releases.
//!
//! The official updater plugin reads `latest.json` from the endpoint in
//! `tauri.conf.json` (`plugins.updater`), checks the minisign signature of
//! the download against the public key there, and installs it (Windows:
//! the NSIS installer in passive mode, which reopens the app; macOS: the
//! replaced `.app`; Linux: the AppImage, or the `.deb`/`.rpm` through
//! `pkexec`). The webview never calls the plugin itself: it gets only the
//! commands below, and nothing is installed until the user clicks.
//!
//! Before installing, the VCA server that this app started (the bundled
//! vcad, which owns the agent sessions it launched) is stopped through the
//! same graceful path as `vca_host_stop`. Agent sessions VCA only observes
//! (started in a terminal) are separate processes and keep running.
//!
//! Commands: `vca_update_check`, `vca_update_download`,
//! `vca_update_preflight` and `vca_update_install`.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::utils::config::BundleType;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};

use crate::host::Host;
use crate::{CommandError, CommandResult};

/// The release list, for updates this install cannot apply itself.
pub const RELEASES_URL: &str = "https://github.com/JinsongRoh/Voice-Command-AI/releases";

/// Progress events are sent at most once per this many bytes (plus the
/// first and the last one).
const PROGRESS_STEP: u64 = 256 * 1024;

/// How an available update can be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallKind {
    /// Downloaded, verified and installed by the app.
    Auto,
    /// The release has no package for this kind of install (for example a
    /// `.deb` install while the release only carries an AppImage): the user
    /// downloads it from the release page.
    Manual,
}

/// `vca_update_check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateCheck {
    pub current_version: String,
    /// `None`: this is the latest version.
    pub available: Option<AvailableUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AvailableUpdate {
    pub version: String,
    /// Release notes from `latest.json`.
    pub notes: Option<String>,
    /// RFC 3339 publication date, when given.
    pub date: Option<String>,
    pub install: InstallKind,
    /// The release page of this version.
    pub release_url: String,
}

/// `vca_update_download` progress, on its channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DownloadEvent {
    Progress {
        downloaded: u64,
        /// Total size in bytes, when the server reports it.
        total: Option<u64>,
    },
    /// Downloaded; the signature is checked next.
    Finished,
}

/// `vca_update_preflight`: what installing would affect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdatePreflight {
    /// The VCA server runs and was started by this app: installing stops it.
    pub managed_server: bool,
    /// Running sessions that VCA started (they may end with the server).
    pub managed_sessions: usize,
}

struct Pending {
    update: Update,
    install: InstallKind,
    /// The verified package, once downloaded.
    bytes: Option<Arc<Vec<u8>>>,
}

/// The update found by the last check, managed as Tauri state on desktop.
#[derive(Default)]
pub struct UpdaterState {
    pending: tokio::sync::Mutex<Option<Pending>>,
    busy: AtomicBool,
}

/// Clears the busy flag when a download or install ends.
struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl UpdaterState {
    fn begin(&self) -> CommandResult<BusyGuard<'_>> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err(CommandError::new(
                "update_busy",
                "an update is already being downloaded or installed",
            ));
        }
        Ok(BusyGuard(&self.busy))
    }
}

/// The error code shown to the user for an updater error.
pub fn error_code(e: &UpdaterError) -> &'static str {
    match e {
        UpdaterError::Reqwest(_) | UpdaterError::Network(_) => "update_offline",
        UpdaterError::Minisign(_)
        | UpdaterError::Base64(_)
        | UpdaterError::SignatureUtf8(_)
        | UpdaterError::SignedVersionMismatch { .. }
        | UpdaterError::MissingSignedVersion => "update_signature",
        UpdaterError::InvalidUpdaterFormat | UpdaterError::BinaryNotFoundInArchive => {
            "update_bad_package"
        }
        UpdaterError::ReleaseNotFound | UpdaterError::Serialization(_) => "update_no_release",
        UpdaterError::TargetNotFound(_) | UpdaterError::TargetsNotFound(_) => "update_no_platform",
        UpdaterError::UnsupportedOs | UpdaterError::UnsupportedArch => "update_unsupported",
        _ => "update_failed",
    }
}

fn command_error(e: UpdaterError) -> CommandError {
    CommandError::new(error_code(&e), e.to_string())
}

/// The `latest.json` platform to fall back on when the release has no
/// package for this install: Linux `.deb`/`.rpm` installs (and binaries not
/// installed by a bundle at all) look up the AppImage entry, only to learn
/// the version; such an update is installed by hand. `None` elsewhere: the
/// plugin has already tried every entry that could apply.
pub fn manual_fallback_target(os: &str, arch: &str, bundle: Option<BundleType>) -> Option<String> {
    (os == "linux" && bundle != Some(BundleType::AppImage))
        .then(|| format!("linux-{arch}-appimage"))
}

/// Runs the check (`check(None)`: the plugin's own platform choice) and,
/// when the release has no package for this install, the check against
/// `fallback`, whose result can only be installed by hand.
pub async fn check_with_fallback<T, F, Fut>(
    check: F,
    fallback: Option<String>,
) -> Result<Option<(T, InstallKind)>, UpdaterError>
where
    F: Fn(Option<String>) -> Fut,
    Fut: Future<Output = Result<Option<T>, UpdaterError>>,
{
    match check(None).await {
        Ok(found) => Ok(found.map(|u| (u, InstallKind::Auto))),
        Err(UpdaterError::TargetNotFound(_) | UpdaterError::TargetsNotFound(_))
            if fallback.is_some() =>
        {
            Ok(check(fallback).await?.map(|u| (u, InstallKind::Manual)))
        }
        Err(e) => Err(e),
    }
}

/// Running sessions VCA started itself (in its own terminal), from vcad's
/// `sessions` body. Observed and cloud sessions do not depend on vcad.
pub fn count_managed_sessions(sessions_body: &serde_json::Value) -> usize {
    let Some(sessions) = sessions_body.get("sessions").and_then(|s| s.as_array()) else {
        return 0;
    };
    sessions
        .iter()
        .filter(|s| s.get("control").and_then(|c| c.as_str()) == Some("managed_pty"))
        .filter(|s| {
            let state = s
                .get("state")
                .and_then(|st| st.get("type"))
                .and_then(|t| t.as_str());
            !matches!(state, Some("exited") | Some("error"))
        })
        .count()
}

pub fn release_url(version: &str) -> String {
    format!("{RELEASES_URL}/tag/v{version}")
}

fn available(update: &Update, install: InstallKind) -> AvailableUpdate {
    AvailableUpdate {
        version: update.version.clone(),
        notes: update.body.clone().filter(|n| !n.trim().is_empty()),
        date: update
            .raw_json
            .get("pub_date")
            .and_then(|d| d.as_str())
            .map(str::to_owned),
        install,
        release_url: release_url(&update.version),
    }
}

/// The OS and architecture names `latest.json` uses.
fn updater_platform() -> Option<(&'static str, &'static str)> {
    let target = tauri_plugin_updater::target()?;
    let (os, arch) = target.split_once('-')?;
    let os = match os {
        "linux" => "linux",
        "darwin" => "darwin",
        "windows" => "windows",
        _ => return None,
    };
    let arch = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "i686" => "i686",
        "armv7" => "armv7",
        "riscv64" => "riscv64",
        _ => return None,
    };
    Some((os, arch))
}

#[tauri::command]
pub async fn vca_update_check(
    app: AppHandle,
    state: State<'_, UpdaterState>,
) -> CommandResult<UpdateCheck> {
    let current_version = app.package_info().version.to_string();
    let fallback = updater_platform().and_then(|(os, arch)| {
        manual_fallback_target(os, arch, tauri::utils::platform::bundle_type())
    });
    let check = |target: Option<String>| {
        let app = app.clone();
        async move {
            let mut builder = app.updater_builder();
            if let Some(target) = target {
                builder = builder.target(target);
            }
            builder.build()?.check().await
        }
    };
    let found = check_with_fallback(check, fallback)
        .await
        .map_err(command_error)?;
    let mut pending = state.pending.lock().await;
    let available = found.map(|(update, install)| {
        let info = available(&update, install);
        *pending = Some(Pending {
            update,
            install,
            bytes: None,
        });
        info
    });
    if available.is_none() {
        *pending = None;
    }
    Ok(UpdateCheck {
        current_version,
        available,
    })
}

/// Downloads the update found by the last check and verifies its signature.
/// Installs nothing.
#[tauri::command]
pub async fn vca_update_download(
    state: State<'_, UpdaterState>,
    on_event: Channel<DownloadEvent>,
) -> CommandResult<()> {
    let _busy = state.begin()?;
    let update = {
        let pending = state.pending.lock().await;
        let pending = pending.as_ref().ok_or_else(no_update)?;
        if pending.install != InstallKind::Auto {
            return Err(manual_only());
        }
        if pending.bytes.is_some() {
            return Ok(());
        }
        pending.update.clone()
    };
    let mut downloaded = 0u64;
    let mut reported = 0u64;
    let bytes = update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                let done = total.is_some_and(|t| downloaded >= t);
                if reported == 0 || downloaded - reported >= PROGRESS_STEP || done {
                    reported = downloaded.max(1);
                    let _ = on_event.send(DownloadEvent::Progress { downloaded, total });
                }
            },
            || {
                let _ = on_event.send(DownloadEvent::Finished);
            },
        )
        .await
        .map_err(command_error)?;
    let mut pending = state.pending.lock().await;
    match pending.as_mut() {
        Some(p) if p.update.version == update.version => {
            p.bytes = Some(Arc::new(bytes));
            Ok(())
        }
        _ => Err(no_update()),
    }
}

#[tauri::command]
pub async fn vca_update_preflight(app: AppHandle) -> CommandResult<UpdatePreflight> {
    let Some(host) = app.try_state::<Arc<Host>>().map(|h| Arc::clone(h.inner())) else {
        return Ok(UpdatePreflight {
            managed_server: false,
            managed_sessions: 0,
        });
    };
    let status = host.status().await;
    let managed_server = status.running && status.managed;
    let managed_sessions = if managed_server {
        host.push_snapshot()
            .await
            .map_or(0, |(sessions, _)| count_managed_sessions(&sessions))
    } else {
        0
    };
    Ok(UpdatePreflight {
        managed_server,
        managed_sessions,
    })
}

/// Stops the VCA server this app started, installs the downloaded update
/// and reopens the app. Called only after the user confirmed.
#[tauri::command]
pub async fn vca_update_install(
    app: AppHandle,
    state: State<'_, UpdaterState>,
) -> CommandResult<()> {
    let _busy = state.begin()?;
    let (update, bytes) = {
        let pending = state.pending.lock().await;
        let pending = pending.as_ref().ok_or_else(no_update)?;
        let bytes = pending.bytes.clone().ok_or_else(|| {
            CommandError::new("update_not_downloaded", "download the update first")
        })?;
        (pending.update.clone(), bytes)
    };
    let host = app.try_state::<Arc<Host>>().map(|h| Arc::clone(h.inner()));
    let mut stopped = false;
    if let Some(host) = &host {
        let status = host.status().await;
        if status.running && status.managed {
            host.stop().await?;
            stopped = true;
        }
    }
    // Windows: starts the installer and exits the app (the installer opens
    // the new version). macOS and Linux: replaces the app, then we relaunch.
    let result = tauri::async_runtime::spawn_blocking(move || update.install(bytes.as_slice()))
        .await
        .map_err(|e| CommandError::new("update_install", e.to_string()))?;
    if let Err(e) = result {
        // The app was not replaced: start the server again, so paired
        // phones and new sessions work as before.
        if stopped {
            if let Some(host) = &host {
                if let Err(start) = host.start().await {
                    tracing::warn!("cannot restart the VCA server: {}", start.message);
                }
            }
        }
        return Err(CommandError::new("update_install", e.to_string()));
    }
    app.restart();
}

fn no_update() -> CommandError {
    CommandError::new("update_none", "check for updates first")
}

fn manual_only() -> CommandError {
    CommandError::new(
        "update_manual",
        "this install cannot update itself; download the new version from the release page",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The desktop app's configuration, as shipped.
    const DESKTOP_CONF: &str = include_str!("../../desktop/src-tauri/tauri.conf.json");
    const DESKTOP_CAPABILITY: &str =
        include_str!("../../desktop/src-tauri/capabilities/default.json");
    const MOBILE_CONF: &str = include_str!("../../mobile/src-tauri/tauri.conf.json");

    fn json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn the_desktop_config_enables_signed_updates_from_github_releases() {
        let conf = json(DESKTOP_CONF);
        assert_eq!(conf["bundle"]["createUpdaterArtifacts"], true);
        let updater = &conf["plugins"]["updater"];
        let parsed: tauri_plugin_updater::Config =
            serde_json::from_value(updater.clone()).expect("the plugin accepts its config");
        assert_eq!(
            parsed
                .endpoints
                .iter()
                .map(|u| u.as_str())
                .collect::<Vec<_>>(),
            ["https://github.com/JinsongRoh/Voice-Command-AI/releases/latest/download/latest.json"]
        );
        assert!(!parsed.dangerous_insecure_transport_protocol);
        assert!(!parsed.dangerous_accept_invalid_certs);
        assert!(!parsed.allow_downgrades);
        assert_eq!(updater["windows"]["installMode"], "passive");
        // A minisign public key (base64 of the `.pub` file), never a secret key.
        let key = data_encoding::BASE64
            .decode(parsed.pubkey.as_bytes())
            .expect("the public key is base64");
        let key = String::from_utf8(key).unwrap();
        assert!(key.starts_with("untrusted comment: minisign public key"));
        assert!(!key.contains("secret"));
    }

    #[test]
    fn the_webview_gets_no_updater_or_process_permissions() {
        let cap = json(DESKTOP_CAPABILITY);
        let permissions = cap["permissions"].as_array().unwrap();
        for p in permissions {
            let id = p.as_str().or_else(|| p["identifier"].as_str()).unwrap();
            assert!(
                !id.starts_with("updater:") && !id.starts_with("process:"),
                "{id} would let the webview install or relaunch on its own"
            );
        }
    }

    #[test]
    fn the_phone_apps_have_no_updater() {
        let conf = json(MOBILE_CONF);
        assert!(conf["plugins"].get("updater").is_none());
        assert_ne!(conf["bundle"]["createUpdaterArtifacts"], true);
    }

    #[test]
    fn errors_map_to_plain_codes() {
        assert_eq!(
            error_code(&UpdaterError::Network("status 404".into())),
            "update_offline"
        );
        assert_eq!(
            error_code(&UpdaterError::ReleaseNotFound),
            "update_no_release"
        );
        assert_eq!(
            error_code(&UpdaterError::TargetsNotFound(vec![
                "linux-x86_64-deb".into()
            ])),
            "update_no_platform"
        );
        assert_eq!(
            error_code(&UpdaterError::SignatureUtf8("x".into())),
            "update_signature"
        );
        assert_eq!(
            error_code(&UpdaterError::SignedVersionMismatch {
                signed: "1.0.0".into(),
                announced: "0.9.0".into()
            }),
            "update_signature"
        );
        assert_eq!(
            error_code(&UpdaterError::InvalidUpdaterFormat),
            "update_bad_package"
        );
        assert_eq!(
            error_code(&UpdaterError::UnsupportedOs),
            "update_unsupported"
        );
        assert_eq!(
            error_code(&UpdaterError::Io(std::io::Error::other("disk"))),
            "update_failed"
        );
    }

    #[test]
    fn only_linux_non_appimage_installs_fall_back_to_a_manual_update() {
        for arch in ["x86_64", "aarch64"] {
            let expected = Some(format!("linux-{arch}-appimage"));
            assert_eq!(
                manual_fallback_target("linux", arch, Some(BundleType::Deb)),
                expected
            );
            assert_eq!(
                manual_fallback_target("linux", arch, Some(BundleType::Rpm)),
                expected
            );
            assert_eq!(manual_fallback_target("linux", arch, None), expected);
            assert_eq!(
                manual_fallback_target("linux", arch, Some(BundleType::AppImage)),
                None
            );
        }
        assert_eq!(
            manual_fallback_target("windows", "x86_64", Some(BundleType::Nsis)),
            None
        );
        assert_eq!(
            manual_fallback_target("darwin", "aarch64", Some(BundleType::App)),
            None
        );
    }

    /// A fake check: the platforms `latest.json` lists for version 9.9.9.
    fn fake(
        platforms: &'static [&'static str],
        own: &'static [&'static str],
    ) -> impl Fn(Option<String>) -> std::future::Ready<Result<Option<String>, UpdaterError>> {
        move |target| {
            let candidates: Vec<String> = match target {
                Some(t) => vec![t],
                None => own.iter().map(|s| (*s).to_owned()).collect(),
            };
            std::future::ready(
                match candidates.iter().find(|c| platforms.contains(&c.as_str())) {
                    Some(c) => Ok(Some(c.clone())),
                    None => Err(UpdaterError::TargetsNotFound(candidates)),
                },
            )
        }
    }

    #[tokio::test]
    async fn a_deb_install_installs_a_deb_when_the_release_has_one() {
        let found = check_with_fallback(
            fake(
                &["linux-x86_64-appimage", "linux-x86_64-deb"],
                &["linux-x86_64-deb", "linux-x86_64"],
            ),
            Some("linux-x86_64-appimage".into()),
        )
        .await
        .unwrap();
        assert_eq!(
            found,
            Some(("linux-x86_64-deb".to_owned(), InstallKind::Auto))
        );
    }

    #[tokio::test]
    async fn a_deb_install_without_a_deb_in_the_release_gets_a_manual_update() {
        let found = check_with_fallback(
            fake(
                &["linux-aarch64-appimage"],
                &["linux-aarch64-deb", "linux-aarch64"],
            ),
            Some("linux-aarch64-appimage".into()),
        )
        .await
        .unwrap();
        assert_eq!(
            found,
            Some(("linux-aarch64-appimage".to_owned(), InstallKind::Manual))
        );
    }

    #[tokio::test]
    async fn without_a_fallback_a_missing_platform_is_an_error() {
        let err = check_with_fallback(
            fake(
                &["darwin-aarch64"],
                &["windows-x86_64-nsis", "windows-x86_64"],
            ),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error_code(&err), "update_no_platform");
    }

    #[tokio::test]
    async fn other_errors_do_not_fall_back() {
        let check = |_target: Option<String>| {
            std::future::ready(Err::<Option<String>, _>(UpdaterError::Network(
                "offline".into(),
            )))
        };
        let err = check_with_fallback(check, Some("linux-x86_64-appimage".into()))
            .await
            .unwrap_err();
        assert_eq!(error_code(&err), "update_offline");
    }

    #[test]
    fn counts_only_running_sessions_vca_started() {
        let body = serde_json::json!({
            "type": "sessions",
            "sessions": [
                { "id": "a", "control": "managed_pty", "state": { "type": "running" } },
                { "id": "b", "control": "managed_pty", "state": { "type": "awaiting_decision" } },
                { "id": "c", "control": "managed_pty", "state": { "type": "exited", "code": 0 } },
                { "id": "d", "control": "observe_only", "state": { "type": "running" } },
                { "id": "e", "control": "terminal_api", "state": { "type": "idle" } },
                { "id": "f", "control": "cloud", "state": { "type": "running" } }
            ]
        });
        assert_eq!(count_managed_sessions(&body), 2);
        assert_eq!(count_managed_sessions(&serde_json::json!({})), 0);
    }

    #[test]
    fn manual_updates_link_the_release_page() {
        assert_eq!(
            release_url("0.2.0"),
            "https://github.com/JinsongRoh/Voice-Command-AI/releases/tag/v0.2.0"
        );
    }

    #[test]
    fn a_published_latest_json_resolves_every_platform() {
        // The shape `tools/release/make-latest-json.py` writes.
        let manifest = serde_json::json!({
            "version": "0.2.0",
            "notes": "Fixes",
            "pub_date": "2026-11-01T00:00:00Z",
            "platforms": {
                "windows-x86_64": { "url": "https://example.invalid/a.exe", "signature": "s" },
                "windows-x86_64-nsis": { "url": "https://example.invalid/a.exe", "signature": "s" },
                "darwin-aarch64": { "url": "https://example.invalid/a.tar.gz", "signature": "s" },
                "darwin-aarch64-app": { "url": "https://example.invalid/a.tar.gz", "signature": "s" },
                "linux-x86_64-appimage": { "url": "https://example.invalid/a.AppImage", "signature": "s" },
                "linux-aarch64-appimage": { "url": "https://example.invalid/b.AppImage", "signature": "s" },
                "linux-aarch64-deb": { "url": "https://example.invalid/b.deb", "signature": "s" }
            }
        });
        let release: tauri_plugin_updater::RemoteRelease =
            serde_json::from_value(manifest).unwrap();
        assert_eq!(release.version.to_string(), "0.2.0");
        for target in [
            "windows-x86_64-nsis",
            "darwin-aarch64-app",
            "linux-x86_64-appimage",
            "linux-aarch64-appimage",
            "linux-aarch64-deb",
        ] {
            assert!(release.download_url(target).is_ok(), "{target}");
        }
        assert!(release.download_url("linux-x86_64-deb").is_err());
    }
}
