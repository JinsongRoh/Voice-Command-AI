//! Windows specifics of the Agent local API
//! (<https://dns.dnspilot.kr/docs/en/agent-local-api>):
//!
//! - after connecting to `\\.\pipe\dnspilot-local`, check that the pipe
//!   server is really the Agent: its process id
//!   (`GetNamedPipeServerProcessId`) must be the one of the
//!   `DNSPilotAgent` service;
//! - the per-user local token is read from
//!   `%ProgramData%\DNSPilot\local-api\<your SID>.token` (readable only by
//!   that user).

#![allow(unsafe_code)]

use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT,
    SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_STATUS_PROCESS,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::contract;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Process id of the server end of a connected named pipe.
pub fn pipe_server_pid(pipe: &impl AsRawHandle) -> std::io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: the handle is a live pipe handle owned by `pipe`; `pid` is a
    // valid out pointer.
    let ok = unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle() as HANDLE, &mut pid) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(pid)
}

/// Process id of the running `DNSPilotAgent` service, or `None` when the
/// service is not installed or not running.
pub fn agent_service_pid() -> Option<u32> {
    let name = wide(contract::WINDOWS_SERVICE_NAME);
    // SAFETY: plain Win32 service-manager calls with valid arguments; every
    // handle opened here is closed before returning.
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return None;
        }
        let service = OpenServiceW(scm, name.as_ptr(), SERVICE_QUERY_STATUS);
        if service.is_null() {
            CloseServiceHandle(scm);
            return None;
        }
        let mut status: SERVICE_STATUS_PROCESS = std::mem::zeroed();
        let mut needed = 0u32;
        let ok = QueryServiceStatusEx(
            service,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        );
        CloseServiceHandle(service);
        CloseServiceHandle(scm);
        (ok != 0 && status.dwProcessId != 0).then_some(status.dwProcessId)
    }
}

/// String SID (`S-1-5-21-...`) of the user running this process.
pub fn current_user_sid() -> std::io::Result<String> {
    // SAFETY: standard token query; the buffer is sized by the first call,
    // the token handle and the SID string are released before returning.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        if len == 0 {
            let e = std::io::Error::last_os_error();
            CloseHandle(token);
            return Err(e);
        }
        // u64 elements keep the buffer aligned for TOKEN_USER.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
        CloseHandle(token);
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut sid_str: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid_str) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut n = 0usize;
        while *sid_str.add(n) != 0 {
            n += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, n));
        LocalFree(sid_str.cast());
        Ok(sid)
    }
}

/// `%ProgramData%\DNSPilot\local-api\<SID>.token` of the current user.
pub fn token_file() -> std::io::Result<PathBuf> {
    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    Ok(program_data
        .join(contract::WINDOWS_TOKEN_DIR)
        .join(format!("{}.token", current_user_sid()?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_user_has_a_sid() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
        let path = token_file().unwrap();
        assert!(path.to_string_lossy().ends_with(&format!("{sid}.token")));
    }
}
