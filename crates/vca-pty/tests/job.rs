//! Windows: managed children start inside their Job Object.
#![cfg(windows)]
#![allow(unsafe_code)]

mod common;

use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};

use common::{TIMEOUT, fake_agent_opts, read_until};
use vca_core::Input;
use vca_pty::PtySession;
use windows_sys::Win32::System::JobObjects::{CreateJobObjectW, IsProcessInJob};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

fn new_job() -> OwnedHandle {
    // SAFETY: null attributes and name create an anonymous job.
    let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    assert!(!h.is_null(), "CreateJobObjectW failed");
    // SAFETY: a valid handle we own.
    unsafe { OwnedHandle::from_raw_handle(h) }
}

fn in_job(pid: u32, job: &OwnedHandle) -> bool {
    // SAFETY: plain FFI; the handle is checked and then owned.
    let p = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    assert!(!p.is_null(), "cannot open the child");
    // SAFETY: valid handle we own.
    let p = unsafe { OwnedHandle::from_raw_handle(p) };
    let mut result = 0;
    // SAFETY: both handles are valid; `result` is a valid out-pointer.
    let ok = unsafe { IsProcessInJob(p.as_raw_handle(), job.as_raw_handle(), &mut result) };
    assert_ne!(ok, 0, "IsProcessInJob failed");
    result != 0
}

fn text(s: &str) -> Input {
    Input::Text {
        text: s.into(),
        submit: true,
        at_prompt: false,
    }
}

#[tokio::test]
async fn child_is_created_inside_the_job_and_works_normally() {
    let job = new_job();
    let (session, placed) = PtySession::spawn_in_job(fake_agent_opts(), job.as_handle()).unwrap();
    assert!(
        placed,
        "the job attribute should be accepted on Windows 10+"
    );
    let pid = session.pid().expect("pid");
    assert!(in_job(pid, &job), "the child is not in the job");

    // The ConPTY plumbing behaves like portable-pty's.
    let mut sub = session.subscribe_from(0);
    let mut out = String::new();
    read_until(&mut sub, &mut out, "> ").await;
    session.send_input(&text("hello job")).await.unwrap();
    read_until(&mut sub, &mut out, "Echo: hello job").await;
    session.send_input(&text("/exit 42")).await.unwrap();
    let status = tokio::time::timeout(TIMEOUT, session.wait())
        .await
        .expect("process did not exit")
        .unwrap();
    assert_eq!(status.code, Some(42));
}

#[tokio::test]
async fn falls_back_to_a_plain_spawn_when_the_job_is_refused() {
    // A file handle is not a job: CreateProcessW refuses the attribute and
    // the child starts without it.
    let file = tempfile::tempfile().unwrap();
    let (session, placed) = PtySession::spawn_in_job(fake_agent_opts(), file.as_handle()).unwrap();
    assert!(!placed);
    let mut sub = session.subscribe_from(0);
    let mut out = String::new();
    read_until(&mut sub, &mut out, "> ").await;
    session.kill().unwrap();
    tokio::time::timeout(TIMEOUT, session.wait())
        .await
        .expect("process did not exit")
        .unwrap();
}
