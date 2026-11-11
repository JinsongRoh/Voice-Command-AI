//! ConPTY spawn that can start the child inside a Job Object.
//!
//! portable-pty creates the process with only the pseudo-console attribute,
//! so a job can be assigned only after `CreateProcessW` returns; anything the
//! child starts in that window escapes the job. This module mirrors
//! portable-pty's ConPTY backend (same pseudo-console flags, command line
//! quoting, environment and working directory) and adds
//! `PROC_THREAD_ATTRIBUTE_JOB_LIST` to the same attribute list, so the child
//! is a member of the job from its first instruction (Windows 10+).
//!
//! The pieces implement portable-pty's [`MasterPty`], [`Child`] and
//! [`ChildKiller`] traits so [`crate::PtySession`] drives them exactly like
//! portable-pty's own.

#![allow(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::sync::Mutex;
use std::{mem, ptr};

use portable_pty::{Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize};
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, STILL_ACTIVE, WAIT_FAILED};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, GetProcessId, INFINITE,
    InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

// The flags portable-pty 0.9 passes (it answers the cursor query that
// INHERIT_CURSOR makes ConPTY send at startup; see `answer_cursor_queries`).
const PSEUDOCONSOLE_INHERIT_CURSOR: u32 = 0x1;
const PSEUDOCONSOLE_RESIZE_QUIRK: u32 = 0x2;
const PSEUDOCONSOLE_WIN32_INPUT_MODE: u32 = 0x4;

/// A spawned child and the master side of its pseudo-console.
pub(crate) struct Spawned {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    /// The child was created inside the requested job.
    pub in_job: bool,
}

/// Opens a pseudo-console and starts `cmd` in it, inside `job` when given.
/// When the job attribute cannot be used the child is started without it
/// and `in_job` is false, so the caller can fall back to assigning it.
pub(crate) fn spawn(
    cmd: &CommandBuilder,
    size: PtySize,
    job: Option<BorrowedHandle<'_>>,
) -> io::Result<Spawned> {
    let (in_read, in_write) = pipe()?;
    let (out_read, out_write) = pipe()?;
    let con = PseudoConsole::new(size, &in_read, &out_write)?;
    // ConPTY duplicated its ends; ours must close so EOF propagates.
    drop(in_read);
    drop(out_write);

    let (proc, in_job) = match job {
        Some(job) => match create_process(cmd, &con, Some(job)) {
            Ok(proc) => (proc, true),
            Err(e) => {
                tracing::warn!(
                    "cannot start the process inside its job ({e}); starting it \
                     without and assigning the job afterwards"
                );
                (create_process(cmd, &con, None)?, false)
            }
        },
        None => (create_process(cmd, &con, None)?, false),
    };

    let master = ConPtyMaster {
        inner: Mutex::new(MasterInner {
            con,
            reader: out_read,
            writer: Some(in_write),
            size,
        }),
    };
    Ok(Spawned {
        master: Box::new(master),
        child: Box::new(ConPtyChild { proc }),
        in_job,
    })
}

fn pipe() -> io::Result<(File, File)> {
    let mut read: HANDLE = ptr::null_mut();
    let mut write: HANDLE = ptr::null_mut();
    // SAFETY: both out-pointers are valid; null security attributes give
    // non-inheritable handles.
    if unsafe { CreatePipe(&mut read, &mut write, ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreatePipe succeeded, so both handles are valid and owned by us.
    Ok(unsafe { (File::from_raw_handle(read), File::from_raw_handle(write)) })
}

fn coord(size: PtySize) -> COORD {
    COORD {
        X: size.cols.min(i16::MAX as u16) as i16,
        Y: size.rows.min(i16::MAX as u16) as i16,
    }
}

struct PseudoConsole(HPCON);

impl PseudoConsole {
    fn new(size: PtySize, input: &File, output: &File) -> io::Result<Self> {
        let mut con: HPCON = 0;
        // SAFETY: both handles are valid pipe ends; `con` is a valid out-pointer.
        let hr = unsafe {
            CreatePseudoConsole(
                coord(size),
                input.as_raw_handle(),
                output.as_raw_handle(),
                PSEUDOCONSOLE_INHERIT_CURSOR
                    | PSEUDOCONSOLE_RESIZE_QUIRK
                    | PSEUDOCONSOLE_WIN32_INPUT_MODE,
                &mut con,
            )
        };
        if hr != 0 {
            return Err(io::Error::other(format!(
                "failed to create pseudo console: HRESULT {hr:#x}"
            )));
        }
        Ok(Self(con))
    }
}

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a pseudo-console we created; closed exactly once.
        unsafe { ClosePseudoConsole(self.0) };
    }
}

/// An initialized `PROC_THREAD_ATTRIBUTE_LIST`, deleted on drop.
struct AttributeList {
    // usize-aligned storage for the opaque list.
    data: Vec<usize>,
}

impl AttributeList {
    fn new(count: u32) -> io::Result<Self> {
        let mut bytes = 0usize;
        // SAFETY: a null list with a valid size pointer only queries the
        // required size (the call "fails" with ERROR_INSUFFICIENT_BUFFER).
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), count, 0, &mut bytes) };
        let mut data = vec![0usize; bytes.div_ceil(mem::size_of::<usize>()).max(1)];
        // SAFETY: `data` holds at least `bytes` writable, aligned bytes.
        if unsafe {
            InitializeProcThreadAttributeList(data.as_mut_ptr().cast(), count, 0, &mut bytes)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { data })
    }

    fn as_mut_ptr(&mut self) -> *mut core::ffi::c_void {
        self.data.as_mut_ptr().cast()
    }

    /// # Safety
    /// `value` must point to `size` bytes that stay valid until the list is
    /// used by `CreateProcessW` and dropped.
    unsafe fn update(
        &mut self,
        attribute: u32,
        value: *const core::ffi::c_void,
        size: usize,
    ) -> io::Result<()> {
        // SAFETY: the list is initialized; the caller guarantees `value`.
        let ok = unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                0,
                attribute as usize,
                value,
                size,
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: the list was initialized in `new`.
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

fn create_process(
    cmd: &CommandBuilder,
    con: &PseudoConsole,
    job: Option<BorrowedHandle<'_>>,
) -> io::Result<OwnedHandle> {
    // Must outlive the attribute list and CreateProcessW.
    let jobs: [HANDLE; 1] = [job.map_or(ptr::null_mut(), |j| j.as_raw_handle())];
    let mut attrs = AttributeList::new(if job.is_some() { 2 } else { 1 })?;
    // SAFETY: for this attribute the value *is* the HPCON (as documented),
    // which lives as long as `con`.
    unsafe {
        attrs.update(
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            con.0 as *const core::ffi::c_void,
            mem::size_of::<HPCON>(),
        )?;
    }
    if job.is_some() {
        // SAFETY: `jobs` lives until after CreateProcessW; the handle is a
        // valid job handle borrowed for this call.
        unsafe {
            attrs.update(
                PROC_THREAD_ATTRIBUTE_JOB_LIST,
                jobs.as_ptr().cast(),
                mem::size_of_val(&jobs),
            )?;
        }
    }

    let mut si = STARTUPINFOEXW::default();
    si.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
    // Invalid std handles so the child cannot inherit redirected ones from
    // this process (same as portable-pty).
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
    si.lpAttributeList = attrs.as_mut_ptr();

    let (exe, mut cmdline) = command_line(cmd)?;
    let mut env = environment_block(cmd);
    let cwd = current_directory(cmd);
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: all strings are NUL-terminated wide buffers that outlive the
    // call; `si` is a valid STARTUPINFOEXW with an initialized attribute list.
    let ok = unsafe {
        CreateProcessW(
            exe.as_ptr(),
            cmdline.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            env.as_mut_ptr().cast(),
            cwd.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateProcessW succeeded; both handles are valid and ours.
    let (proc, _thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(pi.hProcess),
            OwnedHandle::from_raw_handle(pi.hThread),
        )
    };
    Ok(proc)
}

fn search_path(cmd: &CommandBuilder, exe: &OsStr) -> OsString {
    if let Some(path) = cmd.get_env("PATH") {
        let extensions = cmd.get_env("PATHEXT").unwrap_or(OsStr::new(".EXE"));
        for dir in std::env::split_paths(path) {
            let candidate = dir.join(exe);
            if candidate.exists() {
                return candidate.into_os_string();
            }
            for ext in std::env::split_paths(extensions) {
                let ext = ext.to_string_lossy();
                let candidate = dir.join(exe).with_extension(ext.trim_start_matches('.'));
                if candidate.exists() {
                    return candidate.into_os_string();
                }
            }
        }
    }
    exe.to_owned()
}

/// The module name and the quoted, NUL-terminated command line.
fn command_line(cmd: &CommandBuilder) -> io::Result<(Vec<u16>, Vec<u16>)> {
    let argv = cmd.get_argv();
    let first = argv
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no program to run"))?;
    let exe = search_path(cmd, first);
    let mut line = Vec::new();
    append_quoted(&exe, &mut line);
    for arg in argv.iter().skip(1) {
        if arg.encode_wide().any(|c| c == 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid command line argument {arg:?}"),
            ));
        }
        line.push(u16::from(b' '));
        append_quoted(arg, &mut line);
    }
    line.push(0);
    let mut exe: Vec<u16> = exe.encode_wide().collect();
    exe.push(0);
    Ok((exe, line))
}

/// Quotes one argument the way `CommandLineToArgvW` parses it.
fn append_quoted(arg: &OsStr, line: &mut Vec<u16>) {
    let wide: Vec<u16> = arg.encode_wide().collect();
    // Space, tab, newline, vertical tab, double quote.
    let special = |c: &u16| matches!(*c, 0x20 | 0x09 | 0x0a | 0x0b | 0x22);
    if !wide.is_empty() && !wide.iter().any(special) {
        line.extend(wide);
        return;
    }
    let backslash = u16::from(b'\\');
    let quote = u16::from(b'"');
    line.push(quote);
    let mut i = 0;
    while i < wide.len() {
        let mut slashes = 0;
        while i < wide.len() && wide[i] == backslash {
            i += 1;
            slashes += 1;
        }
        if i == wide.len() {
            line.extend(std::iter::repeat_n(backslash, slashes * 2));
            break;
        } else if wide[i] == quote {
            line.extend(std::iter::repeat_n(backslash, slashes * 2 + 1));
        } else {
            line.extend(std::iter::repeat_n(backslash, slashes));
        }
        line.push(wide[i]);
        i += 1;
    }
    line.push(quote);
}

/// The full environment portable-pty would pass (base environment plus the
/// caller's changes), as a double-NUL-terminated block.
fn environment_block(cmd: &CommandBuilder) -> Vec<u16> {
    let mut block = Vec::new();
    for (key, value) in cmd.iter_full_env_as_str() {
        block.extend(OsStr::new(key).encode_wide());
        block.push(u16::from(b'='));
        block.extend(OsStr::new(value).encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// The working directory: the requested one if it exists, else the user
/// profile (as portable-pty does).
fn current_directory(cmd: &CommandBuilder) -> Option<Vec<u16>> {
    let home = cmd.get_env("USERPROFILE").filter(|p| Path::new(p).is_dir());
    let cwd = cmd
        .get_cwd()
        .map(OsString::as_os_str)
        .filter(|p| Path::new(p).is_dir());
    let dir = Path::new(cwd.or(home)?);
    let dir = if dir.is_relative() {
        std::env::current_dir()
            .map(|c| c.join(dir))
            .unwrap_or_else(|_| dir.to_owned())
    } else {
        dir.to_owned()
    };
    let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
    wide.push(0);
    Some(wide)
}

struct MasterInner {
    con: PseudoConsole,
    reader: File,
    writer: Option<File>,
    size: PtySize,
}

struct ConPtyMaster {
    inner: Mutex<MasterInner>,
}

impl ConPtyMaster {
    fn lock(&self) -> std::sync::MutexGuard<'_, MasterInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl MasterPty for ConPtyMaster {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        let mut inner = self.lock();
        // SAFETY: `inner.con` is a live pseudo-console.
        let hr = unsafe { ResizePseudoConsole(inner.con.0, coord(size)) };
        anyhow::ensure!(hr == 0, "failed to resize console: HRESULT {hr:#x}");
        inner.size = size;
        Ok(())
    }

    fn get_size(&self) -> anyhow::Result<PtySize> {
        Ok(self.lock().size)
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn io::Read + Send>> {
        Ok(Box::new(self.lock().reader.try_clone()?))
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn io::Write + Send>> {
        let writer = self
            .lock()
            .writer
            .take()
            .ok_or_else(|| anyhow::anyhow!("writer already taken"))?;
        Ok(Box::new(writer))
    }
}

#[derive(Debug)]
struct ConPtyChild {
    proc: OwnedHandle,
}

fn exit_code(proc: &OwnedHandle) -> io::Result<Option<u32>> {
    let mut code = 0u32;
    // SAFETY: `proc` is a valid process handle; `code` is a valid out-pointer.
    if unsafe { GetExitCodeProcess(proc.as_raw_handle(), &mut code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((code != STILL_ACTIVE as u32).then_some(code))
}

fn terminate(proc: &OwnedHandle) -> io::Result<()> {
    // SAFETY: `proc` is a valid process handle with PROCESS_TERMINATE
    // (CreateProcessW returns full access).
    if unsafe { TerminateProcess(proc.as_raw_handle(), 1) } == 0 {
        // Typically "access denied" because the process already exited.
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl ChildKiller for ConPtyChild {
    fn kill(&mut self) -> io::Result<()> {
        terminate(&self.proc)
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        match self.proc.try_clone() {
            Ok(proc) => Box::new(ConPtyKiller { proc: Some(proc) }),
            Err(e) => {
                tracing::warn!("cannot duplicate the child handle: {e}");
                Box::new(ConPtyKiller { proc: None })
            }
        }
    }
}

impl Child for ConPtyChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(exit_code(&self.proc)?.map(ExitStatus::with_exit_code))
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        // SAFETY: `self.proc` is a valid process handle.
        if unsafe { WaitForSingleObject(self.proc.as_raw_handle(), INFINITE) } == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        exit_code(&self.proc)?
            .map(ExitStatus::with_exit_code)
            .ok_or_else(|| io::Error::other("process still running after wait"))
    }

    fn process_id(&self) -> Option<u32> {
        // SAFETY: `self.proc` is a valid process handle.
        let pid = unsafe { GetProcessId(self.proc.as_raw_handle()) };
        (pid != 0).then_some(pid)
    }

    fn as_raw_handle(&self) -> Option<RawHandle> {
        Some(self.proc.as_raw_handle())
    }
}

#[derive(Debug)]
struct ConPtyKiller {
    proc: Option<OwnedHandle>,
}

impl ChildKiller for ConPtyKiller {
    fn kill(&mut self) -> io::Result<()> {
        match &self.proc {
            Some(proc) => terminate(proc),
            None => Err(io::Error::other("no handle to the child process")),
        }
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(ConPtyKiller {
            proc: self.proc.as_ref().and_then(|p| p.try_clone().ok()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(arg: &str) -> String {
        let mut line = Vec::new();
        append_quoted(OsStr::new(arg), &mut line);
        String::from_utf16(&line).unwrap()
    }

    #[test]
    fn quotes_like_command_line_to_argv() {
        assert_eq!(quoted("plain"), "plain");
        assert_eq!(quoted(""), "\"\"");
        assert_eq!(quoted("a b"), "\"a b\"");
        assert_eq!(quoted(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quoted(r"C:\dir\ x\"), r#""C:\dir\ x\\""#);
        assert_eq!(quoted("é ü"), "\"é ü\"");
    }
}
