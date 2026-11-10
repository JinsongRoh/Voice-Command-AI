//! A list of the running processes that opens no process handles.
//!
//! On Windows, one `NtQuerySystemInformation(SystemProcessInformation)`
//! call returns every process with its id, its parent's id, its image name
//! and its creation time. Nothing is opened, so a long-running scanner holds
//! no handle to any process (`sysinfo` keeps an `OpenProcess` handle with
//! `PROCESS_VM_READ` in every process it caches), and no protected or system
//! process is ever touched. The creation time tells a reused PID from the
//! process that had it before.
//!
//! [`creation_time`] opens one process with
//! `PROCESS_QUERY_LIMITED_INFORMATION`, reads its creation time and closes
//! the handle again: used right before typing into a process's console.

/// One running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcEntry {
    pub pid: u32,
    /// The id of the process that created it. Windows keeps it after that
    /// process exits, and PIDs are reused: compare creation times before
    /// trusting it.
    pub parent: Option<u32>,
    /// Image name, such as `claude.exe`.
    pub name: String,
    /// Creation time in 100 ns units since 1601-01-01 (a Windows
    /// `FILETIME`).
    pub created: u64,
}

/// 100 ns units between 1601-01-01 and 1970-01-01.
const EPOCH_DIFFERENCE: u64 = 116_444_736_000_000_000;

/// A `FILETIME` as whole seconds since the Unix epoch (0 before it).
pub fn filetime_to_unix_secs(filetime: u64) -> u64 {
    filetime.saturating_sub(EPOCH_DIFFERENCE) / 10_000_000
}

/// Every running process (except the idle process, PID 0). `None` where
/// this listing is not available (other platforms) or the call failed.
pub fn list() -> Option<Vec<ProcEntry>> {
    #[cfg(windows)]
    {
        sys::list()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The creation time (see [`ProcEntry::created`]) of process `pid`, read
/// through a handle that is closed again before returning. `None` when the
/// process does not exist or cannot be queried, and on other platforms.
pub fn creation_time(pid: u32) -> Option<u64> {
    #[cfg(windows)]
    {
        sys::creation_time(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod sys {
    use windows_sys::Wdk::System::SystemInformation::{
        NtQuerySystemInformation, SystemProcessInformation,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::System::WindowsProgramming::SYSTEM_PROCESS_INFORMATION;

    use super::ProcEntry;

    /// `STATUS_INFO_LENGTH_MISMATCH`: the buffer was too small.
    const INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
    /// Offset of `CreateTime` in the documented `Reserved1` bytes
    /// (`WorkingSetPrivateSize`, `HardFaultCount`,
    /// `NumberOfThreadsHighWatermark`, `CycleTime`, then `CreateTime`).
    const CREATE_TIME_OFFSET: usize = 24;
    /// Upper bound for the buffer (a few thousand processes need ~2 MB).
    const MAX_BUFFER: usize = 64 << 20;

    pub fn list() -> Option<Vec<ProcEntry>> {
        // `u64` elements keep the records 8-byte aligned.
        let mut buf: Vec<u64> = vec![0; (512 << 10) / 8];
        let len = loop {
            let bytes = u32::try_from(buf.len() * 8).ok()?;
            let mut needed = 0u32;
            // SAFETY: `buf` is writable for `bytes` bytes and `needed`
            // outlives the call.
            let status = unsafe {
                NtQuerySystemInformation(
                    SystemProcessInformation,
                    buf.as_mut_ptr().cast(),
                    bytes,
                    &mut needed,
                )
            };
            if status == INFO_LENGTH_MISMATCH {
                // Processes may start in between: add some room.
                let want = (needed as usize).max(buf.len() * 8) + (64 << 10);
                if want > MAX_BUFFER {
                    return None;
                }
                buf = vec![0; want.div_ceil(8)];
                continue;
            }
            if status < 0 {
                return None;
            }
            break (needed as usize).min(buf.len() * 8);
        };
        let base = buf.as_ptr().cast::<u8>();
        let record = std::mem::size_of::<SYSTEM_PROCESS_INFORMATION>();
        let mut out = Vec::with_capacity(512);
        let mut offset = 0usize;
        while offset + record <= len {
            // SAFETY: the record lies within the `len` bytes the call
            // filled; the kernel aligns records to 8 bytes. Read unaligned
            // anyway, to depend on nothing.
            let info = unsafe {
                std::ptr::read_unaligned(base.add(offset).cast::<SYSTEM_PROCESS_INFORMATION>())
            };
            let pid = info.UniqueProcessId as usize as u32;
            if pid != 0 {
                let parent = info.Reserved2 as usize as u32;
                let mut created = [0u8; 8];
                created
                    .copy_from_slice(&info.Reserved1[CREATE_TIME_OFFSET..CREATE_TIME_OFFSET + 8]);
                let name = image_name(base, len, &info);
                out.push(ProcEntry {
                    pid,
                    parent: (parent != 0 && parent != pid).then_some(parent),
                    name,
                    created: u64::from_le_bytes(created),
                });
            }
            // A record never overlaps the next one: an offset shorter than a
            // record (a hooked or corrupt answer) ends the walk.
            let next = info.NextEntryOffset as usize;
            if next == 0 || next < record {
                break;
            }
            offset = match offset.checked_add(next) {
                Some(o) => o,
                None => break,
            };
        }
        Some(out)
    }

    /// The image name of a record. Read only when it lies entirely inside
    /// the `len` bytes the call filled (security software that hooks the
    /// call may point it elsewhere), unit by unit with unaligned reads.
    fn image_name(base: *const u8, len: usize, info: &SYSTEM_PROCESS_INFORMATION) -> String {
        let bytes = usize::from(info.ImageName.Length) & !1;
        let start = info.ImageName.Buffer as usize;
        let begin = base as usize;
        if info.ImageName.Buffer.is_null() || bytes == 0 {
            return String::new();
        }
        let inside = start >= begin
            && start
                .checked_add(bytes)
                .is_some_and(|end| end <= begin.saturating_add(len));
        if !inside {
            return String::new();
        }
        let offset = start - begin;
        let units: Vec<u16> = (0..bytes / 2)
            .map(|i| {
                // SAFETY: `offset + 2 * i + 2 <= len` (checked above), inside
                // the buffer, which is alive; read unaligned.
                unsafe { std::ptr::read_unaligned(base.add(offset + 2 * i).cast::<u16>()) }
            })
            .collect();
        String::from_utf16_lossy(&units)
    }

    pub fn creation_time(pid: u32) -> Option<u64> {
        if pid == 0 {
            return None;
        }
        // SAFETY: plain call; a null result is handled.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return None;
        }
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: the handle is open and the four FILETIMEs outlive the call.
        let ok =
            unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
        // SAFETY: closes the handle opened above, once.
        unsafe { CloseHandle(handle) };
        (ok != 0)
            .then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetimes_convert_to_unix_seconds() {
        assert_eq!(filetime_to_unix_secs(EPOCH_DIFFERENCE), 0);
        assert_eq!(filetime_to_unix_secs(EPOCH_DIFFERENCE + 15_000_000), 1);
        assert_eq!(filetime_to_unix_secs(0), 0);
    }

    #[cfg(windows)]
    #[test]
    fn this_process_is_listed_with_its_creation_time() {
        let me = std::process::id();
        let procs = list().expect("process list");
        let entry = procs.iter().find(|p| p.pid == me).expect("own process");
        assert!(!entry.name.is_empty());
        assert!(entry.parent.is_some());
        assert_eq!(creation_time(me), Some(entry.created));
        assert!(procs.len() > 5);
    }

    #[cfg(windows)]
    #[test]
    fn a_missing_process_has_no_creation_time() {
        assert_eq!(creation_time(0), None);
        // PIDs are multiples of four; this one is never handed out.
        assert_eq!(creation_time(u32::MAX - 2), None);
    }
}
