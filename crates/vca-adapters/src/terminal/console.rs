//! Typing into a Windows console with `WriteConsoleInputW`.
//!
//! A process can be attached to one console at a time, and vcad may have a
//! console of its own or none. The writing is therefore done by a short-lived
//! helper, `vca console-send --pid <pid> [--submit]`, which reads the text
//! from its standard input, leaves its own (hidden) console, attaches to the
//! target's console, opens `CONIN$` and writes key events to it. vcad starts
//! the helper with `CREATE_NO_WINDOW` (see [`console_send_command`]).
//!
//! # Process identity
//!
//! A PID alone does not name a process: Windows reuses PIDs. vcad records
//! the target's creation time with the PID ([`super::TerminalTarget`]) and
//! passes it as `--created`. The helper opens the process first, compares
//! its creation time and refuses on a mismatch, and keeps that handle open
//! until it is done, so the PID cannot be handed to another process while
//! it types.
//!
//! Only documented console APIs are used: `FreeConsole`, `AttachConsole`,
//! `CreateFileW("CONIN$")` and `WriteConsoleInputW`. Nothing is injected
//! into the other process: the records go through the console host, the
//! same way a paste does.
//!
//! The record layout is plain data ([`KeyRecord`]) so it is tested on every
//! platform.
//!
//! # Pacing
//!
//! Text is typed in chunks of at most [`TYPE_CHUNK_UNITS`] UTF-16 units.
//! After each chunk the helper waits until the program has read every
//! pending record (`GetNumberOfConsoleInputEvents` is zero), then pauses
//! [`CHUNK_SETTLE`] before the next one. Writing a long text in one go
//! lost its beginning: a Node/Bun agent such as Claude Code reads the
//! console 1024 characters at a time, and when one burst arrived as two
//! reads back to back only the last read survived in its prompt (23
//! attachment mentions, 1426 characters, arrived as the last 402). Paced,
//! every read is small and handled before the next one arrives.

use std::time::Duration;

use vca_core::ControlKey;

use super::KeyStep;

/// The helper's subcommand of `vca`.
pub const CONSOLE_SEND_SUBCOMMAND: &str = "console-send";

/// Pause between the text and a separate Enter. Agents such as Claude Code
/// treat a burst of input as a paste and take an Enter inside it as a
/// newline, so the prompt would not be sent.
pub const SUBMIT_PAUSE: Duration = Duration::from_millis(250);

/// Most UTF-16 units written to the console at once when typing text (two
/// records each). Far below the 1024 characters a Node program reads at a
/// time, even when a few chunks pile up.
pub const TYPE_CHUNK_UNITS: usize = 200;
/// Pause after the program took a chunk in, so it handles it (and redraws)
/// before the next one arrives.
pub const CHUNK_SETTLE: Duration = Duration::from_millis(40);
/// How long the program may take to read what was written before the next
/// chunk. Typing stops with an error after that rather than piling up text
/// that would be read as one burst.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the helper checks whether the input was read.
const DRAIN_POLL: Duration = Duration::from_millis(5);

/// `VK_RETURN`.
pub const VK_RETURN: u16 = 0x0D;
/// Scan code of the Enter key.
pub const SCAN_RETURN: u16 = 0x1C;

/// One `KEY_EVENT_RECORD` of a console input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyRecord {
    pub key_down: bool,
    pub repeat_count: u16,
    pub virtual_key: u16,
    pub scan_code: u16,
    /// One UTF-16 code unit (a character outside the BMP takes two key
    /// presses, one per surrogate, as the console expects).
    pub unicode_char: u16,
    pub control_key_state: u32,
}

/// A key press and release for every UTF-16 code unit of `text`, all with
/// the same virtual key and scan code (zero for plain text, as a paste).
pub fn key_records(text: &str, virtual_key: u16, scan_code: u16) -> Vec<KeyRecord> {
    text.encode_utf16()
        .flat_map(|unit| {
            [true, false].map(|key_down| KeyRecord {
                key_down,
                repeat_count: 1,
                virtual_key,
                scan_code,
                unicode_char: unit,
                control_key_state: 0,
            })
        })
        .collect()
}

/// The text records, then (with `submit`) the Enter records sent after
/// [`SUBMIT_PAUSE`] (no pause when there is no text).
pub fn input_batches(text: &str, submit: bool) -> (Vec<KeyRecord>, Option<Vec<KeyRecord>>) {
    let enter = submit.then(|| key_records("\r", VK_RETURN, SCAN_RETURN));
    (key_records(text, 0, 0), enter)
}

/// `text` as UTF-16 chunks of at most `max_units` units (at least one per
/// chunk), never splitting a surrogate pair, and never starting a chunk
/// with a line break where it can be avoided: a CR or LF that arrives
/// alone, after the pause between chunks, is read as Enter and submits the
/// text typed so far.
pub fn text_chunks(text: &str, max_units: usize) -> Vec<Vec<u16>> {
    let max_units = max_units.max(2);
    let mut chunks = Vec::new();
    let mut current: Vec<u16> = Vec::with_capacity(max_units);
    let mut buf = [0u16; 2];
    for c in text.chars() {
        let units = c.encode_utf16(&mut buf);
        if current.len() + units.len() > max_units {
            let cut = if c == '\r' || c == '\n' {
                line_break_cut(&current, max_units - units.len())
            } else {
                current.len()
            };
            let rest = current.split_off(cut);
            chunks.push(std::mem::replace(&mut current, rest));
        }
        current.extend_from_slice(units);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Where to cut `current` so that the next chunk starts with its last
/// character that is not a line break (the line break follows it). The
/// carried part must leave room (`room` units) in the next chunk; when no
/// such cut exists, the whole of `current` stays.
fn line_break_cut(current: &[u16], room: usize) -> usize {
    let is_break = |u: u16| u == 0x0D || u == 0x0A;
    let is_low_surrogate = |u: u16| (0xDC00..=0xDFFF).contains(&u);
    let mut i = current.len();
    while i > 1 {
        i -= 1;
        let u = current[i];
        if is_low_surrogate(u) || is_break(u) {
            continue;
        }
        return if current.len() - i <= room {
            i
        } else {
            current.len()
        };
    }
    current.len()
}

/// The text the helper may type, whether or not it submits. Text is never
/// a key: CR is what the Enter key types and would run the line, so every
/// line break becomes LF (a new line in a prompt, as in a paste), and the
/// other control characters (Ctrl+C, Escape, Ctrl+D, ...) that would act on
/// the program are dropped. Tab stays. The only Enter is the separate one
/// sent with `submit`. Leading line breaks go: the first write follows a
/// drained input buffer, so one there would arrive first and alone and be
/// read as Enter (see [`text_chunks`]).
pub fn sanitize_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    match out.find(|c| c != '\n') {
        Some(start) => out.split_off(start),
        None => String::new(),
    }
}

/// `text` without control characters other than Tab.
fn strip_controls(text: &str) -> String {
    text.chars()
        .filter(|&c| !c.is_control() || c == '\t')
        .collect()
}

/// Longest pause a key sequence may ask for: the helper holds the console
/// (and the target process) while it waits.
pub const MAX_KEY_PAUSE: Duration = Duration::from_secs(5);

/// The key records of `text`, one batch per chunk of [`TYPE_CHUNK_UNITS`].
pub fn chunked_records(text: &str) -> Vec<Vec<KeyRecord>> {
    text_chunks(text, TYPE_CHUNK_UNITS)
        .into_iter()
        .map(|units| {
            units
                .into_iter()
                .flat_map(|unit| {
                    [true, false].map(|key_down| KeyRecord {
                        key_down,
                        repeat_count: 1,
                        virtual_key: 0,
                        scan_code: 0,
                        unicode_char: unit,
                        control_key_state: 0,
                    })
                })
                .collect()
        })
        .collect()
}

/// What the helper typed: UTF-16 units of text, the writes they took, and
/// whether the program had read everything when the helper finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TypeReport {
    pub units: usize,
    pub chunks: usize,
    pub drained: bool,
}

impl std::fmt::Display for TypeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "typed {} UTF-16 units in {} writes{}",
            self.units,
            self.chunks,
            if self.drained {
                ""
            } else {
                " (not all read yet)"
            }
        )
    }
}

/// `ENHANCED_KEY` in `dwControlKeyState`: the arrow keys of the separate
/// cursor block.
const ENHANCED_KEY: u32 = 0x0100;
/// `LEFT_CTRL_PRESSED`.
const LEFT_CTRL_PRESSED: u32 = 0x0008;

/// A press and release of a control key, with the virtual key, scan code
/// and character a keyboard would produce. The console host turns arrow
/// keys into the escape sequences a terminal application reads.
pub fn control_key_records(key: ControlKey) -> Vec<KeyRecord> {
    let (virtual_key, scan_code, unicode_char, control_key_state) = match key {
        ControlKey::Enter => (VK_RETURN, SCAN_RETURN, u16::from(b'\r'), 0),
        ControlKey::Escape => (0x1B, 0x01, 0x1B, 0),
        ControlKey::Interrupt => (0x43, 0x2E, 0x03, LEFT_CTRL_PRESSED),
        ControlKey::Tab => (0x09, 0x0F, u16::from(b'\t'), 0),
        ControlKey::Backspace => (0x08, 0x0E, 0x08, 0),
        ControlKey::Up => (0x26, 0x48, 0, ENHANCED_KEY),
        ControlKey::Down => (0x28, 0x50, 0, ENHANCED_KEY),
        ControlKey::Left => (0x25, 0x4B, 0, ENHANCED_KEY),
        ControlKey::Right => (0x27, 0x4D, 0, ENHANCED_KEY),
    };
    [true, false]
        .map(|key_down| KeyRecord {
            key_down,
            repeat_count: 1,
            virtual_key,
            scan_code,
            unicode_char,
            control_key_state,
        })
        .to_vec()
}

/// What the helper writes for a key sequence: record batches and pauses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleAction {
    Write(Vec<KeyRecord>),
    Sleep(Duration),
}

/// The console records (and pauses) of a key sequence. Long text is split
/// into chunks and paced like [`send`] does: the helper waits for each to
/// be read, then [`CHUNK_SETTLE`] before the next one.
pub fn key_step_actions(steps: &[KeyStep]) -> Vec<ConsoleAction> {
    steps
        .iter()
        .flat_map(|step| match step {
            KeyStep::Key { key } => vec![ConsoleAction::Write(control_key_records(*key))],
            // Control keys are `Key` steps; text never carries them.
            KeyStep::Text { text } => {
                let mut actions = Vec::new();
                for (i, chunk) in chunked_records(&strip_controls(text))
                    .into_iter()
                    .enumerate()
                {
                    if i > 0 {
                        actions.push(ConsoleAction::Sleep(CHUNK_SETTLE));
                    }
                    actions.push(ConsoleAction::Write(chunk));
                }
                actions
            }
            KeyStep::Pause { ms } => vec![ConsoleAction::Sleep(
                Duration::from_millis(*ms).min(MAX_KEY_PAUSE),
            )],
        })
        .collect()
}

/// The helper's command line for typing `steps` into the console of `pid`
/// (the steps go to its stdin as JSON).
pub fn console_keys_command(
    pid: u32,
    created: Option<u64>,
    steps: &[KeyStep],
) -> super::CommandSpec {
    let mut args = console_send_args(pid, created, false);
    args.push("--keys".to_owned());
    let mut spec = super::CommandSpec::new(&console_helper_program(), args);
    spec.stdin = Some(serde_json::to_string(steps).unwrap_or_else(|_| "[]".into()));
    spec
}

/// Types a key sequence into the console of process `pid`, pausing where
/// the sequence says. Leaves this process without a console: call it only
/// from the `vca console-send` helper.
#[cfg(windows)]
pub fn send_keys(pid: u32, created: Option<u64>, steps: &[KeyStep]) -> std::io::Result<()> {
    let console = sys::AttachedConsole::attach(pid, created)?;
    console.wait_drained(DRAIN_TIMEOUT)?;
    for action in key_step_actions(steps) {
        match action {
            ConsoleAction::Write(records) => {
                console.write(&records)?;
                console.wait_drained(DRAIN_TIMEOUT)?;
            }
            ConsoleAction::Sleep(d) => std::thread::sleep(d),
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn send_keys(_pid: u32, _created: Option<u64>, _steps: &[KeyStep]) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "console input is available on Windows only",
    ))
}

/// Arguments of the helper for `pid`, created at `created` (see the module
/// docs).
pub fn console_send_args(pid: u32, created: Option<u64>, submit: bool) -> Vec<String> {
    let mut args = vec![
        CONSOLE_SEND_SUBCOMMAND.to_owned(),
        "--pid".to_owned(),
        pid.to_string(),
    ];
    if let Some(created) = created {
        args.push("--created".to_owned());
        args.push(created.to_string());
    }
    if submit {
        args.push("--submit".to_owned());
    }
    args
}

/// The helper program: the `vca` CLI next to this program (the desktop app
/// and the release archive ship both). Never a `vca` found on `PATH` or in
/// the working directory: the helper receives what the person typed, and
/// anything named `vca` in a writable folder could take it.
pub fn console_helper() -> std::io::Result<std::path::PathBuf> {
    let name = format!("vca{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe()?;
    let helper = exe
        .parent()
        .map(|dir| dir.join(&name))
        .ok_or_else(|| std::io::Error::other("this program has no folder"))?;
    if helper.is_file() {
        Ok(helper)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "the console helper {} is missing (it is installed next to vcad)",
                helper.display()
            ),
        ))
    }
}

/// The helper program's path as text, for a command line: where
/// [`console_helper`] expects it, even when it is missing (running it then
/// fails instead of searching `PATH`). Check [`console_helper`] first.
pub fn console_helper_program() -> String {
    let name = format!("vca{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&name)))
        .map_or(name, |p| p.to_string_lossy().into_owned())
}

/// The helper's command line for `pid` (the text goes to its stdin).
pub fn console_send_command(
    pid: u32,
    created: Option<u64>,
    text: &str,
    submit: bool,
) -> super::CommandSpec {
    let mut spec = super::CommandSpec::new(
        &console_helper_program(),
        console_send_args(pid, created, submit),
    );
    spec.stdin = Some(text.to_owned());
    spec
}

/// Writes `text` (and, with `submit`, a separate Enter) to the console of
/// process `pid`, paced (see the module docs). Leaves this process without
/// a console: call it only from the `vca console-send` helper.
#[cfg(windows)]
pub fn send(
    pid: u32,
    created: Option<u64>,
    text: &str,
    submit: bool,
) -> std::io::Result<TypeReport> {
    let text = sanitize_text(text);
    let text = text.as_str();
    let chunks = chunked_records(text);
    let enter = submit.then(|| key_records("\r", VK_RETURN, SCAN_RETURN));
    let console = sys::AttachedConsole::attach(pid, created)?;
    // Whatever is pending (the user typing, another writer) goes first, so
    // this text never joins it in one read.
    console.wait_drained(DRAIN_TIMEOUT)?;
    let mut report = TypeReport {
        units: text.encode_utf16().count(),
        chunks: chunks.len(),
        drained: true,
    };
    let last = chunks.len().saturating_sub(1);
    for (i, chunk) in chunks.iter().enumerate() {
        if i > 0 {
            std::thread::sleep(CHUNK_SETTLE);
        }
        console.write(chunk)?;
        if i < last || enter.is_some() {
            console.wait_drained(DRAIN_TIMEOUT).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!(
                        "typing stopped after {} of {} writes: {e}",
                        i + 1,
                        chunks.len()
                    ),
                )
            })?;
        } else {
            report.drained = console.wait_drained(DRAIN_TIMEOUT).is_ok();
        }
    }
    if let Some(enter) = enter {
        // Let the agent finish taking the text in first: an Enter inside a
        // burst is taken as a newline.
        if !chunks.is_empty() {
            std::thread::sleep(SUBMIT_PAUSE);
        }
        console.write(&enter)?;
        report.drained = console.wait_drained(DRAIN_TIMEOUT).is_ok();
    }
    Ok(report)
}

#[cfg(not(windows))]
pub fn send(
    _pid: u32,
    _created: Option<u64>,
    _text: &str,
    _submit: bool,
) -> std::io::Result<TypeReport> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "console input is available on Windows only",
    ))
}

/// The helper's subcommand that prints the visible screen of a console.
pub const CONSOLE_READ_SUBCOMMAND: &str = "console-read";

/// The helper's command line for reading the visible screen of the console
/// of `pid` (printed to its stdout). Reading changes nothing in the console.
pub fn console_read_command(pid: u32, created: Option<u64>) -> super::CommandSpec {
    let mut args = vec![
        CONSOLE_READ_SUBCOMMAND.to_owned(),
        "--pid".to_owned(),
        pid.to_string(),
    ];
    if let Some(created) = created {
        args.push("--created".to_owned());
        args.push(created.to_string());
    }
    super::CommandSpec::new(&console_helper_program(), args)
}

/// The rows of the console window of process `pid` (the visible part of
/// its screen buffer), trailing blanks trimmed, one line per row. Leaves
/// this process without a console: call it only from the `vca console-read`
/// helper.
#[cfg(windows)]
pub fn read_screen(pid: u32, created: Option<u64>) -> std::io::Result<String> {
    sys::read_screen(pid, created)
}

#[cfg(not(windows))]
pub fn read_screen(_pid: u32, _created: Option<u64>) -> std::io::Result<String> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "console screens are available on Windows only",
    ))
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod sys {
    use std::io;

    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, CONSOLE_SCREEN_BUFFER_INFO, COORD, FreeConsole, GetConsoleScreenBufferInfo,
        GetNumberOfConsoleInputEvents, INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT, KEY_EVENT_RECORD,
        KEY_EVENT_RECORD_0, PeekConsoleInputW, ReadConsoleOutputCharacterW, WriteConsoleInputW,
    };
    use windows_sys::Win32::System::Threading::{
        CreateMutexW, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, ReleaseMutex, WaitForSingleObject,
    };

    /// How long a helper waits for another helper typing into the same
    /// console.
    const CONSOLE_MUTEX_WAIT_MS: u32 = 60_000;
    /// Most pending records inspected to tell key events from others.
    const PEEK_LIMIT: u32 = 4096;

    /// A named mutex per target console, held while typing: two helpers
    /// (started by two vcad requests, or by vcad and a person) never
    /// interleave their keys.
    struct ConsoleMutex(HANDLE);

    impl ConsoleMutex {
        fn acquire(pid: u32, created: Option<u64>) -> io::Result<Self> {
            let name = format!("Local\\vca-console-{pid}-{}", created.unwrap_or(0));
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            // SAFETY: `wide` is NUL-terminated and outlives the call; no
            // security attributes; not initially owned.
            let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the handle was just created.
            let waited = unsafe { WaitForSingleObject(handle, CONSOLE_MUTEX_WAIT_MS) };
            if waited == WAIT_OBJECT_0 || waited == WAIT_ABANDONED {
                return Ok(Self(handle));
            }
            // SAFETY: closes the handle created above, once.
            unsafe { CloseHandle(handle) };
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("another helper is still typing into the console of process {pid}"),
            ))
        }
    }

    impl Drop for ConsoleMutex {
        fn drop(&mut self) {
            // SAFETY: this thread owns the mutex (acquired in `acquire`);
            // the handle is closed once.
            unsafe {
                ReleaseMutex(self.0);
                CloseHandle(self.0);
            }
        }
    }

    use super::KeyRecord;

    fn record(key: &KeyRecord) -> INPUT_RECORD {
        INPUT_RECORD {
            EventType: KEY_EVENT as u16,
            Event: INPUT_RECORD_0 {
                KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: i32::from(key.key_down),
                    wRepeatCount: key.repeat_count,
                    wVirtualKeyCode: key.virtual_key,
                    wVirtualScanCode: key.scan_code,
                    uChar: KEY_EVENT_RECORD_0 {
                        UnicodeChar: key.unicode_char,
                    },
                    dwControlKeyState: key.control_key_state,
                },
            },
        }
    }

    /// An open handle to the target process (closed when dropped). While
    /// it is open, Windows does not give the PID to another process. It
    /// does not keep the process alive: see [`ProcessHandle::ensure_running`].
    pub(super) struct ProcessHandle {
        handle: HANDLE,
        pid: u32,
    }

    impl ProcessHandle {
        /// Opens `pid` and checks that it is the process created at
        /// `created` and still running; refuses a process that got a
        /// reused PID.
        pub(super) fn open(pid: u32, created: Option<u64>) -> io::Result<Self> {
            // SAFETY: plain call; a null result is handled.
            let handle = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
            if handle.is_null() {
                let err = io::Error::last_os_error();
                return Err(io::Error::new(
                    err.kind(),
                    format!("process {pid} is not running: {err}"),
                ));
            }
            let this = Self { handle, pid };
            this.ensure_running()?;
            if let Some(expected) = created {
                let actual = this.created()?;
                if actual != expected {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "process {pid} is not the session's process any more \
                             (it exited and the id was reused); nothing was typed"
                        ),
                    ));
                }
            }
            Ok(this)
        }

        /// Fails once the process has exited. Its console then belongs to
        /// whatever started it (usually a shell), which would read the
        /// rest of the text, and the Enter, as a command line.
        pub(super) fn ensure_running(&self) -> io::Result<()> {
            // SAFETY: the handle is open with SYNCHRONIZE access.
            match unsafe { WaitForSingleObject(self.handle, 0) } {
                WAIT_TIMEOUT => Ok(()),
                WAIT_OBJECT_0 => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "process {} has exited; nothing more is typed into its console",
                        self.pid
                    ),
                )),
                _ => Err(io::Error::last_os_error()),
            }
        }

        fn created(&self) -> io::Result<u64> {
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
            // SAFETY: the handle is open; the FILETIMEs outlive the call.
            let ok = unsafe {
                GetProcessTimes(
                    self.handle,
                    &mut created,
                    &mut exited,
                    &mut kernel,
                    &mut user,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
        }
    }

    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            // SAFETY: closes the handle opened in `open`, once.
            unsafe { CloseHandle(self.handle) };
        }
    }

    /// The console of another process with its input buffer open, until
    /// dropped (then this process has no console).
    pub struct AttachedConsole {
        input: HANDLE,
        /// Keeps the target's PID from being reused while typing, and tells
        /// whether it still runs before every write.
        process: ProcessHandle,
        /// Released after the console is let go (fields drop after
        /// `Drop::drop`).
        _turn: ConsoleMutex,
    }

    impl AttachedConsole {
        pub fn attach(pid: u32, created: Option<u64>) -> io::Result<Self> {
            let process = ProcessHandle::open(pid, created)?;
            let turn = ConsoleMutex::acquire(pid, created)?;
            // SAFETY: plain calls without pointers; leaving a console that
            // this process does not have is harmless.
            unsafe {
                FreeConsole();
                if AttachConsole(pid) == 0 {
                    let err = io::Error::last_os_error();
                    return Err(io::Error::new(
                        err.kind(),
                        format!("cannot attach to the console of process {pid}: {err}"),
                    ));
                }
            }
            let name: Vec<u16> = "CONIN$".encode_utf16().chain(Some(0)).collect();
            // SAFETY: `name` is NUL-terminated and outlives the call; no
            // security attributes and no template handle.
            let input = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if input == INVALID_HANDLE_VALUE || input.is_null() {
                let err = io::Error::last_os_error();
                // SAFETY: detaches from the console attached above.
                unsafe { FreeConsole() };
                return Err(io::Error::new(
                    err.kind(),
                    format!("cannot open the console input: {err}"),
                ));
            }
            Ok(Self {
                input,
                process,
                _turn: turn,
            })
        }

        /// Writes `keys`, unless the target process has exited (between
        /// chunks, during a pause, before the Enter): its console's input
        /// would go to the shell that started it.
        pub fn write(&self, keys: &[KeyRecord]) -> io::Result<()> {
            self.process.ensure_running()?;
            let records: Vec<INPUT_RECORD> = keys.iter().map(record).collect();
            let mut done = 0usize;
            while done < records.len() {
                let rest = &records[done..];
                let len = u32::try_from(rest.len()).unwrap_or(u32::MAX);
                let mut written = 0u32;
                // SAFETY: `rest` holds `len` initialized records and
                // `written` outlives the call.
                let ok =
                    unsafe { WriteConsoleInputW(self.input, rest.as_ptr(), len, &mut written) };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                if written == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "the console took no input",
                    ));
                }
                done += written as usize;
            }
            Ok(())
        }

        /// Key events in the input buffer that no program has read yet.
        /// Focus, mouse and window size events are queued by the console
        /// host too and may never be read; they do not count.
        pub fn pending(&self) -> io::Result<u32> {
            let mut n = 0u32;
            // SAFETY: `n` outlives the call; the handle is open.
            if unsafe { GetNumberOfConsoleInputEvents(self.input, &mut n) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if n == 0 {
                return Ok(0);
            }
            let want = n.min(PEEK_LIMIT);
            // SAFETY: INPUT_RECORD is plain data; all zeros is a valid value.
            let blank: INPUT_RECORD = unsafe { std::mem::zeroed() };
            let mut records = vec![blank; want as usize];
            let mut read = 0u32;
            // SAFETY: `records` holds `want` writable records and `read`
            // outlives the call; peeking leaves the buffer as it is.
            let ok =
                unsafe { PeekConsoleInputW(self.input, records.as_mut_ptr(), want, &mut read) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let read = read.min(want);
            let keys = records[..read as usize]
                .iter()
                .filter(|r| u32::from(r.EventType) == KEY_EVENT)
                .count();
            // Past the inspected window, assume the rest are key events.
            Ok(u32::try_from(keys).unwrap_or(u32::MAX) + n.saturating_sub(read))
        }

        /// Waits until the program has read everything in the input buffer.
        pub fn wait_drained(&self, timeout: std::time::Duration) -> io::Result<()> {
            let start = std::time::Instant::now();
            loop {
                let n = self.pending()?;
                if n == 0 {
                    return Ok(());
                }
                if start.elapsed() >= timeout {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "the program did not read its input within {} s \
                             ({n} key events still waiting)",
                            timeout.as_secs()
                        ),
                    ));
                }
                std::thread::sleep(super::DRAIN_POLL);
            }
        }
    }

    impl Drop for AttachedConsole {
        fn drop(&mut self) {
            // SAFETY: closes the handle opened in `attach`, then detaches.
            unsafe {
                CloseHandle(self.input);
                FreeConsole();
            }
        }
    }

    /// The console output of another process, open until dropped (then
    /// this process has no console).
    struct AttachedOutput(HANDLE);

    impl Drop for AttachedOutput {
        fn drop(&mut self) {
            // SAFETY: closes the handle opened in `read_screen`, then detaches.
            unsafe {
                CloseHandle(self.0);
                FreeConsole();
            }
        }
    }

    /// See [`super::read_screen`]. Takes no console turn: reading the
    /// screen buffer never reaches the program.
    pub(super) fn read_screen(pid: u32, created: Option<u64>) -> io::Result<String> {
        let process = ProcessHandle::open(pid, created)?;
        // SAFETY: plain calls without pointers; leaving a console that this
        // process does not have is harmless.
        unsafe {
            FreeConsole();
            if AttachConsole(pid) == 0 {
                let err = io::Error::last_os_error();
                return Err(io::Error::new(
                    err.kind(),
                    format!("cannot attach to the console of process {pid}: {err}"),
                ));
            }
        }
        let name: Vec<u16> = "CONOUT$".encode_utf16().chain(Some(0)).collect();
        // SAFETY: `name` is NUL-terminated and outlives the call; no
        // security attributes and no template handle.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            let err = io::Error::last_os_error();
            // SAFETY: detaches from the console attached above.
            unsafe { FreeConsole() };
            return Err(io::Error::new(
                err.kind(),
                format!("cannot open the console output: {err}"),
            ));
        }
        let output = AttachedOutput(handle);
        // SAFETY: CONSOLE_SCREEN_BUFFER_INFO is plain data; all zeros is valid.
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: the handle is open and `info` outlives the call.
        if unsafe { GetConsoleScreenBufferInfo(output.0, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let width = u32::try_from(info.dwSize.X.max(0)).unwrap_or(0);
        let mut lines = Vec::new();
        for y in info.srWindow.Top..=info.srWindow.Bottom {
            let mut buf = vec![0u16; width as usize];
            let mut read = 0u32;
            let at = COORD { X: 0, Y: y };
            // SAFETY: `buf` holds `width` writable units and `read`
            // outlives the call.
            let ok = unsafe {
                ReadConsoleOutputCharacterW(output.0, buf.as_mut_ptr(), width, at, &mut read)
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let row = String::from_utf16_lossy(&buf[..(read as usize).min(buf.len())]);
            lines.push(row.trim_end().to_owned());
        }
        drop(output);
        drop(process);
        Ok(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_becomes_a_press_and_release_per_code_unit() {
        let recs = key_records("ab", 0, 0);
        assert_eq!(recs.len(), 4);
        assert_eq!(
            recs[0],
            KeyRecord {
                key_down: true,
                repeat_count: 1,
                virtual_key: 0,
                scan_code: 0,
                unicode_char: u16::from(b'a'),
                control_key_state: 0,
            }
        );
        assert!(!recs[1].key_down);
        assert_eq!(recs[1].unicode_char, u16::from(b'a'));
        assert!(recs[2].key_down);
        assert_eq!(recs[3].unicode_char, u16::from(b'b'));
        assert!(key_records("", 0, 0).is_empty());
    }

    #[test]
    fn non_bmp_characters_use_surrogate_pairs() {
        // U+1F600 is D83D DE00 in UTF-16; Hangul stays one unit.
        let units: Vec<u16> = key_records("\u{1F600}한", 0, 0)
            .iter()
            .filter(|r| r.key_down)
            .map(|r| r.unicode_char)
            .collect();
        assert_eq!(units, [0xD83D, 0xDE00, 0xD55C]);
    }

    #[test]
    fn enter_is_a_separate_key_with_its_codes() {
        let (text, enter) = input_batches("dir", true);
        assert_eq!(text.len(), 6);
        assert!(text.iter().all(|r| r.virtual_key == 0 && r.scan_code == 0));
        let enter = enter.unwrap();
        assert_eq!(enter.len(), 2);
        for (rec, down) in enter.iter().zip([true, false]) {
            assert_eq!(rec.key_down, down);
            assert_eq!(rec.virtual_key, 0x0D);
            assert_eq!(rec.scan_code, 0x1C);
            assert_eq!(rec.unicode_char, u16::from(b'\r'));
            assert_eq!(rec.repeat_count, 1);
        }
        assert_eq!(input_batches("dir", false).1, None);
        assert_eq!(SUBMIT_PAUSE, Duration::from_millis(250));
    }

    #[test]
    fn control_keys_carry_their_codes() {
        let down = control_key_records(ControlKey::Down);
        assert_eq!(down.len(), 2);
        assert!(down[0].key_down && !down[1].key_down);
        assert_eq!(down[0].virtual_key, 0x28);
        assert_eq!(down[0].scan_code, 0x50);
        assert_eq!(down[0].unicode_char, 0);
        assert_eq!(down[0].control_key_state, ENHANCED_KEY);
        let enter = control_key_records(ControlKey::Enter);
        assert_eq!(enter, input_batches("", true).1.unwrap());
        let actions = key_step_actions(&[
            KeyStep::Text { text: " ".into() },
            KeyStep::Pause { ms: 7 },
            KeyStep::Key {
                key: ControlKey::Enter,
            },
        ]);
        assert_eq!(
            actions,
            [
                ConsoleAction::Write(key_records(" ", 0, 0)),
                ConsoleAction::Sleep(Duration::from_millis(7)),
                ConsoleAction::Write(enter),
            ]
        );
        let cmd = console_keys_command(4, None, &[KeyStep::Pause { ms: 1 }]);
        assert_eq!(cmd.args, ["console-send", "--pid", "4", "--keys"]);
        assert_eq!(cmd.stdin.as_deref(), Some(r#"[{"type":"pause","ms":1}]"#));
    }

    #[test]
    fn long_text_is_typed_in_small_chunks() {
        // 50 attachment mentions: about 3100 characters.
        let text: String = (0..50)
            .map(|i| {
                format!("Attached file: @.vca/attachments/20261101-1240{i:02}-IMG_67{i:02}.jpg ")
            })
            .collect();
        let chunks = text_chunks(&text, TYPE_CHUNK_UNITS);
        assert!(chunks.len() >= text.len() / TYPE_CHUNK_UNITS);
        assert!(
            chunks
                .iter()
                .all(|c| !c.is_empty() && c.len() <= TYPE_CHUNK_UNITS)
        );
        let joined: Vec<u16> = chunks.concat();
        assert_eq!(String::from_utf16(&joined).unwrap(), text);
        // Records: a press and a release per unit, in the same chunks.
        let records = chunked_records(&text);
        assert_eq!(records.len(), chunks.len());
        assert_eq!(
            records.iter().map(Vec::len).sum::<usize>(),
            2 * text.encode_utf16().count()
        );
        assert_eq!(records.concat(), key_records(&text, 0, 0));
        // Short text: one write; none for nothing.
        assert_eq!(chunked_records("dir").len(), 1);
        assert!(chunked_records("").is_empty());
    }

    #[test]
    fn chunks_never_split_a_surrogate_pair() {
        // Each emoji is two units: with an odd limit a pair would straddle.
        let text = "a\u{1F600}\u{1F600}\u{1F600}";
        let chunks = text_chunks(text, 3);
        assert_eq!(chunks.len(), 3);
        for c in &chunks {
            assert!(String::from_utf16(c).is_ok(), "{c:x?}");
        }
        assert_eq!(String::from_utf16(&chunks.concat()).unwrap(), text);
        // Korean stays one unit per syllable.
        assert_eq!(
            text_chunks("한국어", 2),
            [vec![0xD55C, 0xAD6D], vec![0xC5B4]]
        );
    }

    #[test]
    fn typed_text_never_carries_enter_or_control_keys() {
        assert_eq!(sanitize_text("dir /s\tx"), "dir /s\tx");
        // CR is the Enter key: every line break becomes LF.
        assert_eq!(sanitize_text("a\rb\r\nc\nd"), "a\nb\nc\nd");
        // Ctrl+C, Escape, Ctrl+D and DEL are dropped.
        assert_eq!(sanitize_text("x\x03\x1b[A\x04\u{7f}y"), "x[Ay");
        assert!(!sanitize_text("one\r\ntwo\r").contains('\r'));
        // A leading line break would be the first record typed: Enter.
        assert_eq!(sanitize_text("\r\n\nnext"), "next");
        assert_eq!(sanitize_text("\n"), "");
        assert_eq!(sanitize_text("\u{3}\nx"), "x");
        assert_eq!(sanitize_text("a\n\nb"), "a\n\nb");
        // Key sequences type text without controls.
        let actions = key_step_actions(&[KeyStep::Text {
            text: "a\rb".into(),
        }]);
        assert_eq!(actions, [ConsoleAction::Write(key_records("ab", 0, 0))]);
    }

    #[test]
    fn a_chunk_never_starts_with_a_line_break() {
        // The cut would fall right before the LF: the last character moves
        // along with it.
        let text = format!("{}\nrest", "x".repeat(TYPE_CHUNK_UNITS));
        let chunks = text_chunks(&text, TYPE_CHUNK_UNITS);
        assert_eq!(String::from_utf16(&chunks.concat()).unwrap(), text);
        for c in &chunks[1..] {
            assert!(c[0] != 0x0A && c[0] != 0x0D, "{c:?}");
        }
        assert!(chunks.iter().all(|c| c.len() <= TYPE_CHUNK_UNITS));
        // Between CR and LF too.
        let text = format!("{}\r\nnext", "y".repeat(TYPE_CHUNK_UNITS - 1));
        let chunks = text_chunks(&text, TYPE_CHUNK_UNITS);
        assert_eq!(String::from_utf16(&chunks.concat()).unwrap(), text);
        for c in &chunks[1..] {
            assert!(c[0] != 0x0A && c[0] != 0x0D, "{c:?}");
        }
        // Only line breaks: nothing to move, the text still arrives whole.
        let text = "\n".repeat(5);
        assert_eq!(text_chunks(&text, 2).concat().len(), 5);
    }

    /// Once the target has exited, its console belongs to the shell that
    /// started it: nothing more is typed (the rest would run as a command).
    #[cfg(windows)]
    #[test]
    fn an_exited_process_gets_no_more_input() {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "exit", "0"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        child.wait().unwrap();
        // `child` still holds a handle, so the PID names the exited process.
        let err = sys::ProcessHandle::open(child.id(), None)
            .err()
            .expect("an exited process is refused");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
        assert!(err.to_string().contains("exited"), "{err}");
        let me = sys::ProcessHandle::open(std::process::id(), None).unwrap();
        me.ensure_running().unwrap();
    }

    #[test]
    fn pauses_are_bounded() {
        let actions = key_step_actions(&[KeyStep::Pause { ms: u64::MAX }]);
        assert_eq!(actions, [ConsoleAction::Sleep(MAX_KEY_PAUSE)]);
    }

    #[test]
    fn the_helper_is_never_looked_up_on_path() {
        let program = std::path::PathBuf::from(console_helper_program());
        assert!(program.is_absolute(), "{program:?}");
        // Next to the test binary there is no `vca`: an error, not `PATH`.
        if !program.is_file() {
            assert_eq!(
                console_helper().unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
        }
    }

    #[test]
    fn key_sequences_chunk_their_text() {
        let long = "x".repeat(TYPE_CHUNK_UNITS * 2 + 1);
        let actions = key_step_actions(&[KeyStep::Text { text: long }]);
        // Three writes, paced like `send`: a settle pause between them.
        assert_eq!(actions.len(), 5);
        for (i, action) in actions.iter().enumerate() {
            if i % 2 == 0 {
                assert!(matches!(action, ConsoleAction::Write(_)), "{i}");
            } else {
                assert_eq!(action, &ConsoleAction::Sleep(CHUNK_SETTLE));
            }
        }
        let report = TypeReport {
            units: 401,
            chunks: 3,
            drained: false,
        };
        assert_eq!(
            report.to_string(),
            "typed 401 UTF-16 units in 3 writes (not all read yet)"
        );
    }

    #[test]
    fn helper_arguments() {
        assert_eq!(
            console_send_args(77, None, true),
            ["console-send", "--pid", "77", "--submit"]
        );
        assert_eq!(
            console_send_args(5, None, false),
            ["console-send", "--pid", "5"]
        );
        assert_eq!(
            console_send_args(5, Some(133), false),
            ["console-send", "--pid", "5", "--created", "133"]
        );
        let cmd = console_send_command(77, None, "dir", true);
        assert_eq!(cmd.stdin.as_deref(), Some("dir"));
        assert_eq!(cmd.args, console_send_args(77, None, true));
    }
}
