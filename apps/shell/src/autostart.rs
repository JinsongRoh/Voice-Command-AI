//! Desktop only: "Start VCA when I sign in" (Settings > General). Off until
//! the user turns it on.
//!
//! The sign-in entry starts this app's own executable with the fixed flag
//! [`AUTOSTART_FLAG`]; nothing user-provided goes into it. A launch at sign-in
//! is a normal launch (the window opens and, with "Run the VCA server on
//! this PC" on, the bundled server starts, see [`crate::host`]): the app has
//! no tray to hide in, so it does not start hidden.
//!
//! * Windows: the value named after the product (`Voice Command AI`) under
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, holding
//!   `"<exe>" --autostart`. It is listed in Settings > Apps > Startup and in
//!   Task Manager; when it is turned off there (`StartupApproved\Run`), the
//!   toggle shows off, and turning it on here clears that override. The
//!   NSIS uninstaller of Tauri removes a `Run` value of that name (not on
//!   updates).
//! * macOS: a LaunchAgent, `~/Library/LaunchAgents/<identifier>.autostart.plist`,
//!   with `RunAtLoad` (used from the next sign-in).
//! * Linux: `~/.config/autostart/<identifier>.desktop` (XDG autostart); an
//!   AppImage registers the AppImage file, not its mount point.
//!
//! The state is read from the OS each time (`vca_autostart_status`), so the
//! toggle stays right when the entry is removed elsewhere. Only
//! [`EntryStore`] touches the OS; the tests use a fake or a temporary
//! folder, never this machine's real entry.
//!
//! Commands: `vca_autostart_status`, `vca_autostart_set`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::AppHandle;
#[cfg(not(windows))]
use tauri::Manager;

use crate::{CommandError, CommandResult};

/// The flag the sign-in entry passes to the app.
pub const AUTOSTART_FLAG: &str = "--autostart";

/// This run was started by the sign-in entry.
static LAUNCHED_AT_LOGIN: AtomicBool = AtomicBool::new(false);

/// Notes whether the app was started by the sign-in entry (called once at
/// startup with the command line arguments).
pub fn note_launch(args: impl IntoIterator<Item = String>) {
    if args.into_iter().any(|a| a == AUTOSTART_FLAG) {
        LAUNCHED_AT_LOGIN.store(true, Ordering::Relaxed);
        tracing::info!("started at sign-in");
    }
}

/// `vca_autostart_status` and `vca_autostart_set`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutostartStatus {
    /// The sign-in entry is present and not turned off elsewhere.
    pub enabled: bool,
    /// This run was started by it.
    pub launched_at_login: bool,
}

/// Raw storage of the sign-in entry: the only part that touches the OS.
pub trait EntryStore {
    /// The stored entry, `None` when there is none.
    fn read(&self) -> io::Result<Option<String>>;
    fn write(&self, content: &str) -> io::Result<()>;
    /// Removes the entry; no error when there is none.
    fn remove(&self) -> io::Result<()>;
    /// The entry was turned off outside VCA while left in place (Windows
    /// Task Manager / Settings > Apps > Startup).
    fn turned_off_elsewhere(&self) -> io::Result<bool> {
        Ok(false)
    }
    /// Drops such an override (the user turned the option on in VCA).
    fn clear_turned_off(&self) -> io::Result<()> {
        Ok(())
    }
}

/// The entry format of each OS. Each OS builds one; the tests cover all.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A command line for the `Run` registry key.
    WindowsRun,
    /// A launchd property list.
    LaunchAgent,
    /// An XDG desktop entry.
    DesktopEntry,
}

impl Format {
    fn native() -> Self {
        if cfg!(windows) {
            Self::WindowsRun
        } else if cfg!(target_os = "macos") {
            Self::LaunchAgent
        } else {
            Self::DesktopEntry
        }
    }
}

/// What the entry starts and how it is labelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySpec {
    /// This app's executable (absolute).
    pub exe: PathBuf,
    /// The launchd label (macOS).
    pub label: String,
    /// The display name (Linux `Name=`).
    pub name: String,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_owned())
}

/// The executable path as text, refused when it could not be stored safely.
fn exe_text(exe: &Path) -> io::Result<&str> {
    let text = exe
        .to_str()
        .ok_or_else(|| invalid("the app's path is not valid Unicode"))?;
    if !exe.is_absolute() {
        return Err(invalid("the app's path is not absolute"));
    }
    if text.chars().any(char::is_control) {
        return Err(invalid("the app's path contains control characters"));
    }
    Ok(text)
}

/// Renders the entry for `format`.
pub fn render(format: Format, spec: &EntrySpec) -> io::Result<String> {
    let exe = exe_text(&spec.exe)?;
    match format {
        Format::WindowsRun => {
            // Windows paths cannot contain quotes; the quotes keep a path
            // with spaces in one piece.
            if exe.contains('"') {
                return Err(invalid("the app's path contains a quote"));
            }
            Ok(format!("\"{exe}\" {AUTOSTART_FLAG}"))
        }
        Format::LaunchAgent => Ok(format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>{AUTOSTART_FLAG}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#,
            label = xml_escape(&spec.label),
            exe = xml_escape(exe),
        )),
        Format::DesktopEntry => Ok(format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name={name}\n\
             Exec={exec} {AUTOSTART_FLAG}\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n",
            name = desktop_string(&spec.name),
            exec = desktop_exec_arg(exe),
        )),
    }
}

/// True when a stored entry starts the app (it may be present but off).
pub fn entry_is_on(format: Format, content: &str) -> bool {
    match format {
        Format::WindowsRun => !content.trim().is_empty(),
        Format::LaunchAgent => {
            // `<key>Disabled</key><true/>` keeps launchd from loading it.
            let compact: String = content.split_whitespace().collect();
            !compact.contains("<key>Disabled</key><true/>")
        }
        Format::DesktopEntry => !content.lines().map(str::trim).any(|line| {
            line.eq_ignore_ascii_case("Hidden=true")
                || line.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
        }),
    }
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// A desktop entry string value (`\` is an escape there).
fn desktop_string(text: &str) -> String {
    text.replace('\\', "\\\\")
}

/// One `Exec=` argument, always quoted. The Exec rules escape `"`, `` ` ``,
/// `$` and `\` inside quotes and double `%` (field codes); the string rules
/// of the file then escape each `\` again.
fn desktop_exec_arg(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    desktop_string(&quoted)
}

/// Whether the entry starts the app at sign-in.
pub fn is_enabled(store: &dyn EntryStore, format: Format) -> io::Result<bool> {
    let Some(content) = store.read()? else {
        return Ok(false);
    };
    Ok(entry_is_on(format, &content) && !store.turned_off_elsewhere()?)
}

/// Turns the entry on (`Some`: written for that spec, replacing an older
/// one) or off (`None`). Returns the state read back.
pub fn set_enabled(
    store: &dyn EntryStore,
    format: Format,
    on: Option<&EntrySpec>,
) -> io::Result<bool> {
    if let Some(spec) = on {
        let content = render(format, spec)?;
        store.write(&content)?;
    } else {
        store.remove()?;
    }
    store.clear_turned_off()?;
    is_enabled(store, format)
}

/// An entry kept in a file (macOS LaunchAgent, Linux autostart).
#[cfg_attr(windows, allow(dead_code))]
pub struct FileStore {
    pub path: PathBuf,
}

impl EntryStore for FileStore {
    fn read(&self) -> io::Result<Option<String>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write(&self, content: &str) -> io::Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| invalid("the entry has no folder"))?;
        std::fs::create_dir_all(dir)?;
        // Written next to the entry, then renamed over it.
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &self.path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    fn remove(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// The `Run` value of the current user (Windows).
#[cfg(windows)]
pub struct RunKeyStore {
    /// The value name: the product name, as Tauri's uninstaller expects.
    pub name: String,
}

#[cfg(windows)]
mod run_key {
    use std::io;

    use windows_registry::CURRENT_USER;

    pub const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    pub const APPROVED: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    /// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`: a missing key or value.
    const NOT_FOUND: i32 = 0x8007_0002_u32 as i32;

    /// `None` for a missing key or value.
    fn found<T>(r: windows_registry::Result<T>) -> io::Result<Option<T>> {
        match r {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.code().0 == NOT_FOUND => Ok(None),
            Err(e) => Err(io::Error::other(e.message())),
        }
    }

    /// The value's raw data.
    pub fn read(key: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
        let Some(key) = found(CURRENT_USER.open(key))? else {
            return Ok(None);
        };
        Ok(found(key.get_value(name))?.map(|v| v.to_vec()))
    }

    /// The value as text; `Some("")` for data that is not a string (still
    /// an entry, shown as on).
    pub fn read_string(key: &str, name: &str) -> io::Result<Option<String>> {
        let Some(key) = found(CURRENT_USER.open(key))? else {
            return Ok(None);
        };
        if found(key.get_value(name))?.is_none() {
            return Ok(None);
        }
        Ok(Some(key.get_string(name).unwrap_or_default()))
    }

    pub fn write_string(key: &str, name: &str, data: &str) -> io::Result<()> {
        found(
            CURRENT_USER
                .create(key)
                .and_then(|k| k.set_string(name, data)),
        )?;
        Ok(())
    }

    pub fn remove(key: &str, name: &str) -> io::Result<()> {
        if let Some(key) = found(CURRENT_USER.options().read().write().open(key))? {
            found(key.remove_value(name))?;
        }
        Ok(())
    }
}

#[cfg(windows)]
impl EntryStore for RunKeyStore {
    fn read(&self) -> io::Result<Option<String>> {
        run_key::read_string(run_key::RUN, &self.name)
    }

    fn write(&self, content: &str) -> io::Result<()> {
        run_key::write_string(run_key::RUN, &self.name, content)
    }

    fn remove(&self) -> io::Result<()> {
        run_key::remove(run_key::RUN, &self.name)
    }

    fn turned_off_elsewhere(&self) -> io::Result<bool> {
        Ok(run_key::read(run_key::APPROVED, &self.name)?
            .is_some_and(|data| startup_approved_off(&data)))
    }

    fn clear_turned_off(&self) -> io::Result<()> {
        run_key::remove(run_key::APPROVED, &self.name)
    }
}

/// `StartupApproved\Run` data: an odd first byte (`03`) marks an entry the
/// user turned off; `02` / `06` are on.
#[cfg_attr(not(windows), allow(dead_code))]
fn startup_approved_off(data: &[u8]) -> bool {
    data.first().is_some_and(|b| b & 1 == 1)
}

/// The executable the entry starts: this app's own, or the AppImage file
/// when running from one (its mount point changes on each run).
fn own_executable() -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_file())
    {
        return Ok(appimage);
    }
    let exe = std::env::current_exe()?;
    // A quarantined app runs from a random read-only copy (App
    // Translocation) that is gone after the next sign-in.
    #[cfg(target_os = "macos")]
    if exe.to_string_lossy().contains("/AppTranslocation/") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "move Voice Command AI to the Applications folder and open it from there first",
        ));
    }
    Ok(exe)
}

/// This app's names: the product name (Windows value name, Linux `Name=`)
/// and its identifier (macOS label, file names).
fn names(app: &AppHandle) -> (String, String) {
    let config = app.config();
    let name = config
        .product_name
        .clone()
        .unwrap_or_else(|| app.package_info().name.clone());
    (name, config.identifier.clone())
}

/// This OS's store for this app's entry.
fn native_store(app: &AppHandle) -> CommandResult<Box<dyn EntryStore + Send>> {
    let (name, identifier) = names(app);
    #[cfg(windows)]
    {
        let _ = identifier;
        Ok(Box::new(RunKeyStore { name }))
    }
    #[cfg(target_os = "macos")]
    {
        let _ = name;
        let dir = app
            .path()
            .home_dir()
            .map_err(|e| CommandError::new("autostart", e.to_string()))?
            .join("Library")
            .join("LaunchAgents");
        Ok(Box::new(FileStore {
            path: dir.join(format!("{}.plist", label(&identifier))),
        }))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = name;
        let dir = app
            .path()
            .config_dir()
            .map_err(|e| CommandError::new("autostart", e.to_string()))?
            .join("autostart");
        Ok(Box::new(FileStore {
            path: dir.join(format!("{identifier}.desktop")),
        }))
    }
}

/// The launchd label (and plist file name) of the entry.
fn label(identifier: &str) -> String {
    format!("{identifier}.autostart")
}

/// What the entry starts: this executable.
fn native_spec(app: &AppHandle) -> CommandResult<EntrySpec> {
    let (name, identifier) = names(app);
    Ok(EntrySpec {
        exe: own_executable().map_err(failed)?,
        label: label(&identifier),
        name,
    })
}

fn failed(e: io::Error) -> CommandError {
    CommandError::new("autostart", e.to_string())
}

fn status(enabled: bool) -> AutostartStatus {
    AutostartStatus {
        enabled,
        launched_at_login: LAUNCHED_AT_LOGIN.load(Ordering::Relaxed),
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> CommandResult<T> + Send + 'static,
) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| CommandError::new("autostart", e.to_string()))?
}

/// Reads the sign-in entry from the OS.
#[tauri::command]
pub async fn vca_autostart_status(app: AppHandle) -> CommandResult<AutostartStatus> {
    let store = native_store(&app)?;
    blocking(move || {
        is_enabled(store.as_ref(), Format::native())
            .map(status)
            .map_err(failed)
    })
    .await
}

/// Turns "Start VCA when I sign in" on or off; returns the state read back.
#[tauri::command]
pub async fn vca_autostart_set(app: AppHandle, enabled: bool) -> CommandResult<AutostartStatus> {
    let store = native_store(&app)?;
    // Turning it off never needs the executable.
    let spec = if enabled {
        Some(native_spec(&app)?)
    } else {
        None
    };
    blocking(move || {
        set_enabled(store.as_ref(), Format::native(), spec.as_ref())
            .map(status)
            .map_err(failed)
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// An in-memory entry, with the Windows "turned off elsewhere" override.
    #[derive(Default)]
    struct Fake {
        entry: RefCell<Option<String>>,
        off_elsewhere: RefCell<bool>,
        fail_writes: bool,
    }

    impl EntryStore for Fake {
        fn read(&self) -> io::Result<Option<String>> {
            Ok(self.entry.borrow().clone())
        }
        fn write(&self, content: &str) -> io::Result<()> {
            if self.fail_writes {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
            }
            *self.entry.borrow_mut() = Some(content.to_owned());
            Ok(())
        }
        fn remove(&self) -> io::Result<()> {
            *self.entry.borrow_mut() = None;
            Ok(())
        }
        fn turned_off_elsewhere(&self) -> io::Result<bool> {
            Ok(*self.off_elsewhere.borrow())
        }
        fn clear_turned_off(&self) -> io::Result<()> {
            *self.off_elsewhere.borrow_mut() = false;
            Ok(())
        }
    }

    fn spec(exe: &str) -> EntrySpec {
        EntrySpec {
            exe: PathBuf::from(exe),
            label: "io.metavu.voicecommandai.desktop.autostart".into(),
            name: "Voice Command AI".into(),
        }
    }

    #[cfg(windows)]
    const EXE: &str = r"C:\Program Files\Voice Command AI\vca-desktop.exe";
    #[cfg(not(windows))]
    const EXE: &str = "/Applications/Voice Command AI.app/Contents/MacOS/vca-desktop";

    #[test]
    fn the_launch_flag_is_noticed() {
        let args = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        note_launch(args("--restore"));
        assert!(!LAUNCHED_AT_LOGIN.load(Ordering::Relaxed));
        note_launch(args("--autostart"));
        assert!(LAUNCHED_AT_LOGIN.load(Ordering::Relaxed));
    }

    #[test]
    fn turning_on_and_off_round_trips() {
        let store = Fake::default();
        let format = Format::WindowsRun;
        assert!(!is_enabled(&store, format).unwrap());
        assert!(set_enabled(&store, format, Some(&spec(EXE))).unwrap());
        assert_eq!(
            store.entry.borrow().as_deref(),
            Some(format!("\"{EXE}\" --autostart").as_str())
        );
        assert!(is_enabled(&store, format).unwrap());
        assert!(!set_enabled(&store, format, None).unwrap());
        assert_eq!(*store.entry.borrow(), None);
        // Off when already off is fine.
        assert!(!set_enabled(&store, format, None).unwrap());
    }

    #[test]
    fn an_entry_removed_or_turned_off_elsewhere_reads_off() {
        let store = Fake::default();
        set_enabled(&store, Format::WindowsRun, Some(&spec(EXE))).unwrap();
        *store.off_elsewhere.borrow_mut() = true;
        assert!(!is_enabled(&store, Format::WindowsRun).unwrap());
        // Turning it on in VCA drops the override.
        assert!(set_enabled(&store, Format::WindowsRun, Some(&spec(EXE))).unwrap());
        assert!(!*store.off_elsewhere.borrow());
        store.entry.replace(None);
        assert!(!is_enabled(&store, Format::WindowsRun).unwrap());
    }

    #[test]
    fn a_failed_write_is_reported() {
        let store = Fake {
            fail_writes: true,
            ..Fake::default()
        };
        let err = set_enabled(&store, Format::WindowsRun, Some(&spec(EXE))).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(!is_enabled(&store, Format::WindowsRun).unwrap());
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for exe in ["relative/vca-desktop", "/opt/vca\n--evil", "/opt/vca\u{7}"] {
            for format in [
                Format::WindowsRun,
                Format::LaunchAgent,
                Format::DesktopEntry,
            ] {
                assert!(render(format, &spec(exe)).is_err(), "{exe:?} {format:?}");
            }
        }
        #[cfg(windows)]
        assert!(render(Format::WindowsRun, &spec(r#"C:\a"b\vca.exe"#)).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn the_run_value_quotes_the_path() {
        let text = render(Format::WindowsRun, &spec(EXE)).unwrap();
        assert_eq!(
            text,
            r#""C:\Program Files\Voice Command AI\vca-desktop.exe" --autostart"#
        );
    }

    #[test]
    fn the_launch_agent_starts_the_app_with_the_flag() {
        let mut s = spec("/Applications/A & B <x>.app/Contents/MacOS/vca-desktop");
        if cfg!(windows) {
            s.exe = PathBuf::from(r"C:\A & B <x>\vca-desktop.exe");
        }
        let plist = render(Format::LaunchAgent, &s).unwrap();
        assert!(plist.contains("<string>io.metavu.voicecommandai.desktop.autostart</string>"));
        assert!(plist.contains("A &amp; B &lt;x&gt;"));
        assert!(!plist.contains("A & B"));
        assert!(plist.contains("<string>--autostart</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(entry_is_on(Format::LaunchAgent, &plist));
        let disabled = plist.replace(
            "<key>RunAtLoad</key>",
            "<key>Disabled</key>\n\t<true/>\n\t<key>RunAtLoad</key>",
        );
        assert!(!entry_is_on(Format::LaunchAgent, &disabled));
    }

    #[test]
    fn the_desktop_entry_quotes_and_escapes_the_path() {
        assert_eq!(
            desktop_exec_arg("/opt/Voice Command AI/vca-desktop"),
            r#""/opt/Voice Command AI/vca-desktop""#
        );
        // `$`, `"` and `` ` `` get one backslash, which the string rules
        // double; a literal backslash ends up as four; `%` is doubled.
        assert_eq!(
            desktop_exec_arg(r#"/a $b"c`d\e%f"#),
            r#""/a \\$b\\"c\\`d\\\\e%%f""#
        );
        let mut s = spec("/home/me/Apps/Voice Command AI.AppImage");
        if cfg!(windows) {
            s.exe = PathBuf::from(r"C:\Apps\Voice Command AI.AppImage");
        }
        let entry = render(Format::DesktopEntry, &s).unwrap();
        assert!(entry.starts_with("[Desktop Entry]\nType=Application\n"));
        assert!(entry.contains("Name=Voice Command AI\n"));
        assert!(entry.contains(" --autostart\n"));
        assert!(entry.contains("X-GNOME-Autostart-enabled=true\n"));
        assert!(entry_is_on(Format::DesktopEntry, &entry));
        assert!(!entry_is_on(
            Format::DesktopEntry,
            &entry.replace(
                "X-GNOME-Autostart-enabled=true",
                "X-GNOME-Autostart-enabled=false"
            )
        ));
        assert!(!entry_is_on(
            Format::DesktopEntry,
            &format!("{entry}Hidden=true\n")
        ));
    }

    #[test]
    fn startup_approved_data_is_read() {
        assert!(!startup_approved_off(&[]));
        assert!(!startup_approved_off(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!startup_approved_off(&[6, 0, 0, 0]));
        assert!(startup_approved_off(&[3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]));
    }

    /// The file store in a temporary folder (never the real autostart folder).
    #[test]
    fn the_file_store_writes_and_removes_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore {
            path: dir.path().join("LaunchAgents").join("x.autostart.plist"),
        };
        let format = Format::LaunchAgent;
        let s = spec(if cfg!(windows) {
            r"C:\VCA\vca-desktop.exe"
        } else {
            "/opt/vca"
        });
        assert!(!is_enabled(&store, format).unwrap());
        assert!(set_enabled(&store, format, Some(&s)).unwrap());
        assert!(store.path.is_file());
        // Rewritten in place, no temporary file left behind.
        assert!(set_enabled(&store, format, Some(&s)).unwrap());
        let names: Vec<_> = std::fs::read_dir(store.path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("x.autostart.plist")]);
        // Removed by the user elsewhere: reads off.
        std::fs::remove_file(&store.path).unwrap();
        assert!(!is_enabled(&store, format).unwrap());
        assert!(!set_enabled(&store, format, None).unwrap());
    }

    /// Writes and removes a test value in this user's real `Run` key. Never
    /// run on a shared machine; run by hand only: `cargo test -p vca-shell
    /// real_run_key -- --ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore = "writes the real HKCU Run key"]
    fn real_run_key() {
        let store = RunKeyStore {
            name: "VCA autostart test (safe to delete)".into(),
        };
        let format = Format::WindowsRun;
        assert!(set_enabled(&store, format, Some(&spec(EXE))).unwrap());
        assert!(is_enabled(&store, format).unwrap());
        assert!(!set_enabled(&store, format, None).unwrap());
    }
}
