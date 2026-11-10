//! Local user verification by the operating system: Windows Hello
//! (`UserConsentVerifier`) on Windows, LocalAuthentication (Touch ID or the
//! login password, `LAPolicyDeviceOwnerAuthentication`) on macOS. Linux and
//! headless servers have none; there the VCA authenticator code is used.
//!
//! vcad shows the prompt itself, so a local process cannot claim that the
//! owner verified: the operating system asks the human. Tests use
//! [`FakeVerifier`].

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Which verifier this computer has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierKind {
    WindowsHello,
    MacLocalAuth,
    /// No OS verifier: the authenticator code is used.
    None,
}

/// Whether the verifier can be used right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Available,
    /// Why not (not set up, disabled by policy, no hardware, ...).
    Unavailable(String),
}

/// The outcome of one prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    Verified,
    Canceled,
    Failed(String),
    Unavailable(String),
}

/// An operating-system verifier.
#[async_trait]
pub trait LocalVerifier: Send + Sync {
    fn kind(&self) -> VerifierKind;
    async fn availability(&self) -> Availability;
    /// Shows the prompt with `reason`. `window` is the native handle of the
    /// window the prompt should belong to (an `HWND` on Windows), if known.
    async fn verify(&self, reason: &str, window: Option<u64>) -> VerifyOutcome;
}

/// No OS verifier (Linux, headless).
pub struct NoVerifier;

#[async_trait]
impl LocalVerifier for NoVerifier {
    fn kind(&self) -> VerifierKind {
        VerifierKind::None
    }
    async fn availability(&self) -> Availability {
        Availability::Unavailable("no operating-system verifier on this computer".into())
    }
    async fn verify(&self, _reason: &str, _window: Option<u64>) -> VerifyOutcome {
        VerifyOutcome::Unavailable("no operating-system verifier on this computer".into())
    }
}

/// The verifier of this platform.
pub fn platform() -> Arc<dyn LocalVerifier> {
    #[cfg(windows)]
    {
        Arc::new(windows_hello::WindowsHello)
    }
    #[cfg(target_os = "macos")]
    {
        Arc::new(mac::MacLocalAuth)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Arc::new(NoVerifier)
    }
}

/// A scripted verifier for tests.
pub struct FakeVerifier {
    kind: VerifierKind,
    available: bool,
    outcomes: Mutex<Vec<VerifyOutcome>>,
    prompts: Mutex<Vec<String>>,
}

impl FakeVerifier {
    /// An available verifier that answers `Verified` unless scripted.
    pub fn available() -> Arc<Self> {
        Arc::new(Self {
            kind: VerifierKind::WindowsHello,
            available: true,
            outcomes: Mutex::new(Vec::new()),
            prompts: Mutex::new(Vec::new()),
        })
    }

    pub fn unavailable() -> Arc<Self> {
        Arc::new(Self {
            kind: VerifierKind::None,
            available: false,
            outcomes: Mutex::new(Vec::new()),
            prompts: Mutex::new(Vec::new()),
        })
    }

    /// The next prompt answers `outcome`.
    pub fn push(&self, outcome: VerifyOutcome) {
        self.outcomes.lock().expect("poisoned").push(outcome);
    }

    /// The reasons shown so far.
    pub fn prompts(&self) -> Vec<String> {
        self.prompts.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl LocalVerifier for FakeVerifier {
    fn kind(&self) -> VerifierKind {
        self.kind
    }
    async fn availability(&self) -> Availability {
        if self.available {
            Availability::Available
        } else {
            Availability::Unavailable("fake: unavailable".into())
        }
    }
    async fn verify(&self, reason: &str, _window: Option<u64>) -> VerifyOutcome {
        self.prompts
            .lock()
            .expect("poisoned")
            .push(reason.to_owned());
        if !self.available {
            return VerifyOutcome::Unavailable("fake: unavailable".into());
        }
        let mut outcomes = self.outcomes.lock().expect("poisoned");
        if outcomes.is_empty() {
            VerifyOutcome::Verified
        } else {
            outcomes.remove(0)
        }
    }
}

/// Longest text of an OS prompt.
#[cfg(any(windows, target_os = "macos", test))]
const PROMPT_CHARS: usize = 120;

/// Keeps the reason shown in the OS dialog short and printable. Only the
/// Windows Hello and Touch ID prompts show a reason.
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn prompt_text(reason: &str) -> String {
    // Direction marks could make the text read differently from what it is.
    let hidden = |c: char| {
        c.is_control()
            || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}')
            || matches!(c, '\u{2066}'..='\u{2069}')
    };
    let clean: Vec<char> = reason.chars().filter(|c| !hidden(*c)).collect();
    // The start names the action and the host, the end the rest of a list:
    // padding in the middle must not push either out of sight.
    let clean: String = if clean.len() > PROMPT_CHARS {
        let tail = PROMPT_CHARS / 3;
        let head = PROMPT_CHARS - tail - 1;
        let mut short: String = clean[..head].iter().collect();
        short.push('…');
        short.extend(&clean[clean.len() - tail..]);
        short
    } else {
        clean.into_iter().collect()
    };
    let clean = clean.trim();
    if clean.is_empty() {
        "Voice Command AI".to_owned()
    } else {
        format!("Voice Command AI: {clean}")
    }
}

#[cfg(windows)]
mod windows_hello {
    //! Windows Hello through `UserConsentVerifier`. Desktop (non-packaged)
    //! apps must parent the prompt to a window, which
    //! `IUserConsentVerifierInterop` does; vcad has no window of its own, so
    //! it uses the window the desktop app passes, else the foreground
    //! window (the app the person just clicked in).

    use std::time::{Duration, Instant};

    use super::{Availability, LocalVerifier, VerifierKind, VerifyOutcome, prompt_text};
    use async_trait::async_trait;
    use windows::Security::Credentials::UI::{
        UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
    };
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsWindow};
    use windows::core::HSTRING;
    use windows_future::{AsyncStatus, IAsyncOperation};

    pub struct WindowsHello;

    /// A prompt nobody answers is canceled. Shorter than the wait in
    /// `Security`, so this thread ends first.
    const PROMPT_TIMEOUT: Duration = Duration::from_secs(180);
    const POLL: Duration = Duration::from_millis(100);

    /// Waits for the prompt's answer; `None` when the time ran out.
    fn wait_bounded(
        op: &IAsyncOperation<UserConsentVerificationResult>,
    ) -> windows::core::Result<Option<UserConsentVerificationResult>> {
        let started = Instant::now();
        while op.Status()? == AsyncStatus::Started {
            if started.elapsed() >= PROMPT_TIMEOUT {
                op.Cancel()?;
                return Ok(None);
            }
            std::thread::sleep(POLL);
        }
        op.GetResults().map(Some)
    }

    fn availability_blocking() -> Availability {
        let result = UserConsentVerifier::CheckAvailabilityAsync().and_then(|op| op.get());
        match result {
            Ok(UserConsentVerifierAvailability::Available) => Availability::Available,
            Ok(UserConsentVerifierAvailability::DeviceNotPresent) => {
                Availability::Unavailable("no Windows Hello device".into())
            }
            Ok(UserConsentVerifierAvailability::NotConfiguredForUser) => {
                Availability::Unavailable("Windows Hello is not set up for this user".into())
            }
            Ok(UserConsentVerifierAvailability::DisabledByPolicy) => {
                Availability::Unavailable("Windows Hello is disabled by policy".into())
            }
            Ok(UserConsentVerifierAvailability::DeviceBusy) => {
                Availability::Unavailable("Windows Hello is busy".into())
            }
            Ok(other) => {
                Availability::Unavailable(format!("Windows Hello unavailable ({})", other.0))
            }
            Err(e) => Availability::Unavailable(format!("Windows Hello unavailable: {e}")),
        }
    }

    #[allow(unsafe_code)]
    fn owner_window(requested: Option<u64>) -> HWND {
        if let Some(raw) = requested.filter(|r| *r != 0) {
            let hwnd = HWND(raw as usize as *mut core::ffi::c_void);
            // SAFETY: IsWindow only inspects the handle value.
            if unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                return hwnd;
            }
        }
        // SAFETY: no arguments; returns a handle or null.
        unsafe { GetForegroundWindow() }
    }

    #[allow(unsafe_code)]
    fn verify_blocking(reason: &str, window: Option<u64>) -> VerifyOutcome {
        if let Availability::Unavailable(why) = availability_blocking() {
            return VerifyOutcome::Unavailable(why);
        }
        let message = HSTRING::from(prompt_text(reason));
        let hwnd = owner_window(window);
        let op: windows::core::Result<IAsyncOperation<UserConsentVerificationResult>> =
            if hwnd.0.is_null() {
                UserConsentVerifier::RequestVerificationAsync(&message)
            } else {
                windows::core::factory::<UserConsentVerifier, IUserConsentVerifierInterop>()
                    // SAFETY: `hwnd` is a live top-level window handle (or
                    // the foreground window) and `message` outlives the call.
                    .and_then(|interop| unsafe {
                        interop.RequestVerificationForWindowAsync(hwnd, &message)
                    })
            };
        match op.and_then(|op| wait_bounded(&op)) {
            Ok(None) => VerifyOutcome::Canceled,
            Ok(Some(UserConsentVerificationResult::Verified)) => VerifyOutcome::Verified,
            Ok(Some(UserConsentVerificationResult::Canceled)) => VerifyOutcome::Canceled,
            Ok(Some(UserConsentVerificationResult::RetriesExhausted)) => {
                VerifyOutcome::Failed("too many attempts".into())
            }
            Ok(Some(UserConsentVerificationResult::DeviceBusy)) => {
                VerifyOutcome::Failed("Windows Hello is busy".into())
            }
            Ok(Some(UserConsentVerificationResult::DeviceNotPresent))
            | Ok(Some(UserConsentVerificationResult::NotConfiguredForUser))
            | Ok(Some(UserConsentVerificationResult::DisabledByPolicy)) => {
                VerifyOutcome::Unavailable("Windows Hello is not available".into())
            }
            Ok(Some(other)) => VerifyOutcome::Failed(format!("Windows Hello result {}", other.0)),
            Err(e) => VerifyOutcome::Failed(format!("Windows Hello: {e}")),
        }
    }

    #[async_trait]
    impl LocalVerifier for WindowsHello {
        fn kind(&self) -> VerifierKind {
            VerifierKind::WindowsHello
        }

        async fn availability(&self) -> Availability {
            tokio::task::spawn_blocking(availability_blocking)
                .await
                .unwrap_or_else(|e| Availability::Unavailable(e.to_string()))
        }

        async fn verify(&self, reason: &str, window: Option<u64>) -> VerifyOutcome {
            let reason = reason.to_owned();
            tokio::task::spawn_blocking(move || verify_blocking(&reason, window))
                .await
                .unwrap_or_else(|e| VerifyOutcome::Failed(e.to_string()))
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    //! LocalAuthentication with `LAPolicyDeviceOwnerAuthentication`: Touch
    //! ID when available, else (or after failures) the login password.

    use std::sync::mpsc;
    use std::time::Duration;

    use super::{Availability, LocalVerifier, VerifierKind, VerifyOutcome, prompt_text};
    use async_trait::async_trait;
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};

    /// `LAErrorUserCancel`, `LAErrorSystemCancel`, `LAErrorAppCancel`.
    const CANCEL_CODES: [isize; 3] = [-2, -4, -9];
    const PROMPT_TIMEOUT: Duration = Duration::from_secs(180);

    pub struct MacLocalAuth;

    #[allow(unsafe_code)]
    fn availability_blocking() -> Availability {
        // SAFETY: plain Objective-C calls on a fresh context.
        let ctx = unsafe { LAContext::new() };
        match unsafe { ctx.canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthentication) } {
            Ok(()) => Availability::Available,
            Err(e) => Availability::Unavailable(e.localizedDescription().to_string()),
        }
    }

    #[allow(unsafe_code)]
    fn verify_blocking(reason: &str) -> VerifyOutcome {
        if let Availability::Unavailable(why) = availability_blocking() {
            return VerifyOutcome::Unavailable(why);
        }
        // SAFETY: as above.
        let ctx = unsafe { LAContext::new() };
        let (tx, rx) = mpsc::channel::<Result<(), (isize, String)>>();
        let block = RcBlock::new(move |ok: Bool, err: *mut NSError| {
            let outcome = if ok.as_bool() {
                Ok(())
            } else if err.is_null() {
                Err((0, "verification failed".to_owned()))
            } else {
                // SAFETY: LocalAuthentication passes a valid NSError when
                // it reports a failure.
                let err = unsafe { &*err };
                Err((err.code(), err.localizedDescription().to_string()))
            };
            let _ = tx.send(outcome);
        });
        let text = NSString::from_str(&prompt_text(reason));
        // SAFETY: `block` and `text` live until the reply (we wait below).
        unsafe {
            ctx.evaluatePolicy_localizedReason_reply(
                LAPolicy::DeviceOwnerAuthentication,
                &text,
                &block,
            );
        }
        match rx.recv_timeout(PROMPT_TIMEOUT) {
            Ok(Ok(())) => VerifyOutcome::Verified,
            Ok(Err((code, _))) if CANCEL_CODES.contains(&code) => VerifyOutcome::Canceled,
            Ok(Err((_, why))) => VerifyOutcome::Failed(why),
            Err(_) => {
                // SAFETY: ends the pending prompt.
                unsafe { ctx.invalidate() };
                VerifyOutcome::Canceled
            }
        }
    }

    #[async_trait]
    impl LocalVerifier for MacLocalAuth {
        fn kind(&self) -> VerifierKind {
            VerifierKind::MacLocalAuth
        }

        async fn availability(&self) -> Availability {
            tokio::task::spawn_blocking(availability_blocking)
                .await
                .unwrap_or_else(|e| Availability::Unavailable(e.to_string()))
        }

        async fn verify(&self, reason: &str, _window: Option<u64>) -> VerifyOutcome {
            let reason = reason.to_owned();
            tokio::task::spawn_blocking(move || verify_blocking(&reason))
                .await
                .unwrap_or_else(|e| VerifyOutcome::Failed(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_text_is_short_and_printable() {
        assert_eq!(prompt_text(""), "Voice Command AI");
        assert_eq!(
            prompt_text("메일 설정 변경\n"),
            "Voice Command AI: 메일 설정 변경"
        );
        assert!(prompt_text(&"x".repeat(500)).chars().count() < 160);
    }

    #[test]
    fn a_long_prompt_keeps_both_ends() {
        let padded = format!(
            "Send a key: evil.example ({}) tail.example",
            "x".repeat(500)
        );
        let text = prompt_text(&padded);
        assert!(text.contains("evil.example"), "{text}");
        assert!(text.contains("tail.example"), "{text}");
        assert!(text.contains('…'));
        assert!(!prompt_text("a\u{202e}b").contains('\u{202e}'));
    }

    #[tokio::test]
    async fn fake_verifier_scripts_outcomes() {
        let v = FakeVerifier::available();
        v.push(VerifyOutcome::Canceled);
        assert_eq!(v.verify("a", None).await, VerifyOutcome::Canceled);
        assert_eq!(v.verify("b", None).await, VerifyOutcome::Verified);
        assert_eq!(v.prompts(), ["a", "b"]);
        let none = FakeVerifier::unavailable();
        assert!(matches!(
            none.verify("c", None).await,
            VerifyOutcome::Unavailable(_)
        ));
        assert!(matches!(
            NoVerifier.availability().await,
            Availability::Unavailable(_)
        ));
    }
}
