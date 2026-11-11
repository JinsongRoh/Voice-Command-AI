//! The desktop shortcut and the sign-in shortcut. Owner-only: they are only
//! reachable over the local control channel, never from a paired device.
//!
//! Both start the desktop app as `vca-desktop --restore`, which asks the
//! running server to reopen the saved windows (starting the bundled server
//! first when needed) and exits without opening a window. The server never
//! writes a shortcut that starts itself. Restore at sign-in is off until the
//! user turns it on; an older sign-in shortcut that started vcad directly is
//! rewritten to the desktop app at startup ([`migrate_sign_in`]).
//!
//! Restore at sign-in is a plain shortcut in the user's own Startup folder,
//! the mechanism Windows shows most plainly: it is listed under Settings >
//! Apps > Startup and in Task Manager's Startup apps, where it can be turned
//! off, and it is a visible file the user can delete. It is written only
//! when the user turns the option on (Settings > Windows > Restore at
//! sign-in) and removed when it is turned off. No `Run` registry value and
//! no scheduled task is ever written.
//!
//! The shortcuts are written natively with the shell's link object
//! (`IShellLinkW` + `IPersistFile`, see [`crate::win_shell`]); no script
//! interpreter is started.

use std::path::{Path, PathBuf};

use vca_core::Error;
use vca_core::error::ErrorCode;

/// File name of the sign-in shortcut (the user never sees it).
const SIGN_IN_LINK: &str = "VCA Session Restore.lnk";
/// The shortcut's comment (its tooltip, and the description in Startup apps).
#[cfg_attr(not(windows), allow(dead_code))]
const DESCRIPTION: &str = "Voice Command AI: reopen your saved AI CLI windows";
/// Longest wait after sign-in, in seconds.
pub const MAX_SIGN_IN_DELAY_SECS: u32 = 600;

/// One Windows shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutSpec {
    /// Known folder: `Desktop` or `Startup`.
    pub folder: &'static str,
    pub file_name: String,
    pub target: PathBuf,
    pub args: String,
}

/// A file name for the shortcut from a (localized) label.
pub fn link_file_name(label: &str) -> String {
    let clean: String = label
        .chars()
        .filter(|c| !c.is_control() && !r#"\/:*?"<>|"#.contains(*c))
        .take(80)
        .collect();
    let clean = clean.trim().trim_end_matches('.');
    if clean.is_empty() {
        SIGN_IN_LINK.to_owned()
    } else {
        format!("{clean}.lnk")
    }
}

/// File name of the desktop app next to vcad.
const DESKTOP_APP: &str = if cfg!(windows) {
    "vca-desktop.exe"
} else {
    "vca-desktop"
};

/// The desktop app installed next to `vcad` (what the shortcuts start).
pub fn desktop_app_next_to(vcad: &Path) -> Option<PathBuf> {
    let app = vcad.parent()?.join(DESKTOP_APP);
    app.is_file().then_some(app)
}

/// True for a shortcut target that is a vcad executable (the old target).
pub fn targets_vcad(target: &Path) -> bool {
    target
        .file_stem()
        .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("vcad"))
}

pub fn desktop_shortcut(target: &Path, label: &str) -> ShortcutSpec {
    ShortcutSpec {
        folder: "Desktop",
        file_name: link_file_name(label),
        target: target.to_owned(),
        args: "--restore".into(),
    }
}

pub fn sign_in_shortcut(target: &Path, delay_secs: u32) -> ShortcutSpec {
    ShortcutSpec {
        folder: "Startup",
        file_name: SIGN_IN_LINK.into(),
        target: target.to_owned(),
        args: format!(
            "--restore --restore-delay {}",
            delay_secs.min(MAX_SIGN_IN_DELAY_SECS)
        ),
    }
}

/// The user's Startup folder.
fn startup_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(dir) = crate::win_shell::known_folder(crate::win_shell::Folder::Startup) {
        return Some(dir);
    }
    let appdata = PathBuf::from(std::env::var_os("APPDATA")?);
    Some(
        appdata
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup"),
    )
}

/// True when restore at sign-in is on.
pub fn sign_in_enabled() -> bool {
    startup_dir().is_some_and(|dir| dir.join(SIGN_IN_LINK).is_file())
}

/// Turns restore at sign-in off: removes VCA's own shortcut.
pub fn remove_sign_in() -> Result<(), Error> {
    if let Some(path) = startup_dir().map(|d| d.join(SIGN_IN_LINK)) {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

/// Rewrites VCA's own sign-in shortcut when it still starts vcad (written by
/// an older version): same arguments, the desktop app as target. The user
/// turned the option on, so it stays on. Returns true when it was rewritten.
#[cfg(windows)]
pub fn migrate_sign_in(desktop_app: &Path) -> Result<bool, Error> {
    let Some(path) = startup_dir().map(|d| d.join(SIGN_IN_LINK)) else {
        return Ok(false);
    };
    if !path.is_file() {
        return Ok(false);
    }
    let (target, args) = crate::win_shell::read_shortcut(&path).map_err(|e| {
        Error::api(
            ErrorCode::Internal,
            format!("the sign-in shortcut could not be read: {e}"),
        )
    })?;
    if !targets_vcad(&target) {
        return Ok(false);
    }
    let delay = args
        .split_whitespace()
        .skip_while(|a| *a != "--restore-delay")
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    create(&sign_in_shortcut(desktop_app, delay))?;
    Ok(true)
}

#[cfg(not(windows))]
pub fn migrate_sign_in(_: &Path) -> Result<bool, Error> {
    Ok(false)
}

/// Creates the shortcut with the shell's own link object (`IShellLinkW`,
/// saved through `IPersistFile`). The program starts minimized without
/// taking the focus, as before.
#[cfg(windows)]
pub fn create(spec: &ShortcutSpec) -> Result<(), Error> {
    use crate::win_shell::{Folder, Shortcut, create_shortcut, known_folder};

    let folder = match spec.folder {
        "Desktop" => Folder::Desktop,
        "Startup" => Folder::Startup,
        other => {
            return Err(Error::api(
                ErrorCode::Internal,
                format!("unknown shortcut folder {other}"),
            ));
        }
    };
    let dir = known_folder(folder)
        .or_else(|| (folder == Folder::Startup).then(startup_dir).flatten())
        .ok_or_else(|| Error::api(ErrorCode::Internal, "the shortcut folder was not found"))?;
    std::fs::create_dir_all(&dir)?;
    create_shortcut(&Shortcut {
        path: dir.join(&spec.file_name),
        target: spec.target.clone(),
        args: spec.args.clone(),
        description: DESCRIPTION.to_owned(),
    })
    .map_err(|e| {
        Error::api(
            ErrorCode::Internal,
            format!("the shortcut could not be created: {e}"),
        )
    })
}

#[cfg(not(windows))]
pub fn create(_: &ShortcutSpec) -> Result<(), Error> {
    Err(Error::api(
        ErrorCode::Unsupported,
        "shortcuts are available on Windows only for now",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_become_safe_file_names() {
        assert_eq!(link_file_name("VCA 세션 복구"), "VCA 세션 복구.lnk");
        assert_eq!(link_file_name(r#"a\b/c:d*e?"<>|"#), "abcde.lnk");
        assert_eq!(link_file_name("..\\..\\evil"), "....evil.lnk");
        assert_eq!(link_file_name("   "), SIGN_IN_LINK);
    }

    #[test]
    fn old_vcad_targets_are_recognized() {
        // Shortcuts are a Windows feature; backslashes separate folders only there.
        let path = |p: &str| {
            if cfg!(windows) {
                PathBuf::from(p)
            } else {
                PathBuf::from(p.replace('\\', "/"))
            }
        };
        assert!(targets_vcad(&path("C:\\VCA\\vcad.exe")));
        assert!(targets_vcad(&path("C:\\VCA\\VCAD.EXE")));
        assert!(!targets_vcad(&path("C:\\VCA\\vca-desktop.exe")));
        let dir = tempfile::tempdir().unwrap();
        let vcad = dir.path().join("vcad.exe");
        assert_eq!(desktop_app_next_to(&vcad), None);
        std::fs::write(dir.path().join(DESKTOP_APP), b"").unwrap();
        assert_eq!(
            desktop_app_next_to(&vcad),
            Some(dir.path().join(DESKTOP_APP))
        );
    }

    #[test]
    fn shortcuts_run_the_desktop_app_with_the_restore_flag() {
        let exe = Path::new("C:\\Program Files\\VCA\\vca-desktop.exe");
        let desktop = desktop_shortcut(exe, "VCA restore");
        assert_eq!(desktop.folder, "Desktop");
        assert_eq!(desktop.args, "--restore");
        assert_eq!(desktop.target, exe);
        let startup = sign_in_shortcut(exe, 60);
        assert_eq!(startup.folder, "Startup");
        assert_eq!(startup.args, "--restore --restore-delay 60");
        assert_eq!(
            sign_in_shortcut(exe, 99_999).args,
            "--restore --restore-delay 600"
        );
    }
}
