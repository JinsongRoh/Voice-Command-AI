//! Documented Windows shell APIs used instead of script interpreters:
//!
//! * [`create_shortcut`]: `IShellLinkW` + `IPersistFile::Save` (the restore
//!   shortcuts on the desktop and, when the user turns it on, in Startup);
//! * [`known_folder`]: `SHGetKnownFolderPath` (Desktop, Startup);
//! * [`run_as_admin`]: `ShellExecuteExW` with the `runas` verb, which shows
//!   Windows' own UAC prompt (the administrator shell of a human task).
//!
//! COM runs on a short-lived thread of its own, so no apartment is left on a
//! runtime worker thread.

#![allow(unsafe_code)]

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Startup, IShellLinkW, KF_FLAG_DEFAULT, SEE_MASK_NOASYNC,
    SHELLEXECUTEINFOW, SHGetKnownFolderPath, ShellExecuteExW, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWMINNOACTIVE, SW_SHOWNORMAL};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, w};

/// A known folder of the current user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    Desktop,
    Startup,
}

impl Folder {
    fn id(self) -> GUID {
        match self {
            Self::Desktop => FOLDERID_Desktop,
            Self::Startup => FOLDERID_Startup,
        }
    }
}

fn com_error(what: &str, err: &windows::core::Error) -> io::Error {
    io::Error::other(format!("{what}: {err}"))
}

/// COM on this thread until dropped.
struct Apartment {
    initialized: bool,
}

impl Apartment {
    fn enter() -> Self {
        // SAFETY: no reserved pointer; balanced by `CoUninitialize` on drop
        // when it succeeded (S_OK or S_FALSE).
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        Self {
            initialized: hr.is_ok(),
        }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: pairs the successful `CoInitializeEx` in `enter`.
            unsafe { CoUninitialize() };
        }
    }
}

/// Runs `f` on a new thread with COM initialized (single-threaded
/// apartment, as the shell expects) and waits for it.
fn with_com<T: Send + 'static>(
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    std::thread::Builder::new()
        .name("vca-shell-com".into())
        .spawn(move || {
            let _apartment = Apartment::enter();
            f()
        })?
        .join()
        .map_err(|_| io::Error::other("the shell thread panicked"))?
}

/// The path of a known folder (redirected folders included).
pub fn known_folder(folder: Folder) -> Option<PathBuf> {
    let id = folder.id();
    // SAFETY: `id` outlives the call; the returned string is copied, then
    // freed with `CoTaskMemFree` as the API requires (also on failure).
    unsafe {
        let raw = SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, None).ok()?;
        let path = raw.as_wide().to_vec();
        CoTaskMemFree(Some(raw.0 as *const _));
        Some(PathBuf::from(OsString::from_wide(&path))).filter(|p| p.is_absolute())
    }
}

/// One shortcut file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    /// The `.lnk` file to write.
    pub path: PathBuf,
    pub target: PathBuf,
    pub args: String,
    pub description: String,
}

/// Writes `shortcut` with the shell's own link object (`IShellLinkW`) and
/// `IPersistFile::Save`, replacing an existing file of the same name. The
/// program starts minimized without taking the focus (what
/// `WshShortcut.WindowStyle = 7` wrote before).
pub fn create_shortcut(shortcut: &Shortcut) -> io::Result<()> {
    let shortcut = shortcut.clone();
    with_com(move || {
        let path = HSTRING::from(shortcut.path.as_os_str());
        let target = HSTRING::from(shortcut.target.as_os_str());
        let args = HSTRING::from(shortcut.args.as_str());
        let description = HSTRING::from(shortcut.description.as_str());
        // SAFETY: COM is initialized on this thread (`with_com`); every
        // string outlives the call it is passed to.
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| com_error("cannot create the shell link object", &e))?;
            link.SetPath(&target)
                .map_err(|e| com_error("cannot set the shortcut target", &e))?;
            link.SetArguments(&args)
                .map_err(|e| com_error("cannot set the shortcut arguments", &e))?;
            link.SetDescription(&description)
                .map_err(|e| com_error("cannot set the shortcut description", &e))?;
            link.SetShowCmd(SW_SHOWMINNOACTIVE)
                .map_err(|e| com_error("cannot set the shortcut window style", &e))?;
            let file: IPersistFile = link
                .cast()
                .map_err(|e| com_error("the shell link cannot be saved", &e))?;
            file.Save(&path, true)
                .map_err(|e| com_error("cannot save the shortcut", &e))?;
        }
        Ok(())
    })
}

/// The target and arguments of an existing shortcut file (to migrate an
/// old restore shortcut).
pub fn read_shortcut(path: &Path) -> io::Result<(PathBuf, String)> {
    let path = HSTRING::from(path.as_os_str());
    with_com(move || {
        fn text(buf: &[u16]) -> OsString {
            let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
            OsString::from_wide(&buf[..end])
        }
        // SAFETY: COM is initialized on this thread (`with_com`); the
        // buffers outlive the calls that fill them, and no find data is
        // requested.
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| com_error("cannot create the shell link object", &e))?;
            let file: IPersistFile = link
                .cast()
                .map_err(|e| com_error("the shell link cannot be loaded", &e))?;
            file.Load(&path, STGM_READ)
                .map_err(|e| com_error("cannot read the shortcut", &e))?;
            let mut target = vec![0u16; 4096];
            // SLGP_RAWPATH: the path as stored, no environment expansion.
            link.GetPath(&mut target, std::ptr::null_mut(), 4)
                .map_err(|e| com_error("cannot read the shortcut target", &e))?;
            let mut args = vec![0u16; 4096];
            link.GetArguments(&mut args)
                .map_err(|e| com_error("cannot read the shortcut arguments", &e))?;
            Ok((
                PathBuf::from(text(&target)),
                text(&args).to_string_lossy().into_owned(),
            ))
        }
    })
}

/// Starts `program` with `parameters` as administrator through
/// `ShellExecuteExW` and the `runas` verb: Windows shows its UAC prompt and
/// this returns once the person answered it. A declined prompt is
/// `ERROR_CANCELLED` (1223).
pub fn run_as_admin(program: &Path, parameters: &str) -> io::Result<()> {
    let program = HSTRING::from(program.as_os_str());
    let parameters = HSTRING::from(parameters);
    with_com(move || {
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            // The call waits for the prompt and the start; this thread ends
            // right after it.
            fMask: SEE_MASK_NOASYNC,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(program.as_ptr()),
            lpParameters: PCWSTR(parameters.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        // SAFETY: `info` is fully initialized with its size set, and the
        // strings it points to outlive the call.
        unsafe { ShellExecuteExW(&mut info) }
            .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_folders_resolve() {
        let desktop = known_folder(Folder::Desktop).unwrap();
        let startup = known_folder(Folder::Startup).unwrap();
        assert!(desktop.is_absolute());
        assert!(startup.ends_with("Startup"), "{startup:?}");
    }

    /// Writes a real shortcut into a temporary folder (never into Startup or
    /// on the desktop) and reads its bytes back.
    #[test]
    fn creates_a_shortcut_natively() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VCA test.lnk");
        create_shortcut(&Shortcut {
            path: path.clone(),
            target: PathBuf::from(r"C:\Windows\System32\notepad.exe"),
            args: "--restore --restore-delay 60".into(),
            description: "VCA test".into(),
        })
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        // Shell link header: size 0x4C and the ShellLink class id.
        assert_eq!(&bytes[..4], &[0x4C, 0, 0, 0]);
        assert_eq!(&bytes[4..8], &[0x01, 0x14, 0x02, 0x00]);
        // ShowCommand at offset 0x3C.
        assert_eq!(u32::from_le_bytes(bytes[0x3C..0x40].try_into().unwrap()), 7);
        let text = String::from_utf16_lossy(
            &bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        );
        let text_odd = String::from_utf16_lossy(
            &bytes[1..]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        );
        assert!(
            text.contains("--restore --restore-delay 60")
                || text_odd.contains("--restore --restore-delay 60")
        );
    }
}
