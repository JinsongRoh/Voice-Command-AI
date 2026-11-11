//! iOS audio session for dictation.
//!
//! cpal's iOS backend opens a RemoteIO unit but never touches the shared
//! `AVAudioSession`; whether the microphone delivers samples depends on the
//! category the session happens to be in. WebKit changes that category on its
//! own (for example around its file/photo picker and media playback), and a
//! unit that was started under a category without recording opens without an
//! error yet receives no audio, so dictation would listen forever.
//!
//! [`activate`] is called before every capture: it puts the session into
//! `PlayAndRecord` and activates it, so the state the previous picker or
//! playback left behind does not matter. It is best effort: when anything is
//! missing it does nothing and cpal behaves as before.

/// Makes the shared audio session record-capable and returns a one-line
/// description of the session before and after (for the debug log). A no-op
/// off iOS.
#[cfg(not(target_os = "ios"))]
pub fn activate() -> String {
    String::new()
}

/// Describes the shared audio session: category, mode, sample rate, input
/// availability and the current route. Empty off iOS.
#[cfg(not(target_os = "ios"))]
pub fn describe() -> String {
    String::new()
}

/// Describes the shared audio session: category, mode, sample rate, input
/// availability and the current route.
#[cfg(target_os = "ios")]
#[allow(unsafe_code)]
pub fn describe() -> String {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2_foundation::NSString;

    let Some(class) = AnyClass::get(c"AVAudioSession") else {
        return "no AVAudioSession".into();
    };
    // SAFETY: plain property getters of the documented AVAudioSession API.
    unsafe {
        let session: Option<Retained<AnyObject>> = msg_send![class, sharedInstance];
        let Some(session) = session else {
            return "no shared session".into();
        };
        let category: Option<Retained<NSString>> = msg_send![&*session, category];
        let mode: Option<Retained<NSString>> = msg_send![&*session, mode];
        let rate: f64 = msg_send![&*session, sampleRate];
        let input: Bool = msg_send![&*session, isInputAvailable];
        let other: Bool = msg_send![&*session, isOtherAudioPlaying];
        let route: Option<Retained<AnyObject>> = msg_send![&*session, currentRoute];
        let route: Option<Retained<NSString>> = route.and_then(|r| msg_send![&*r, description]);
        format!(
            "category={} mode={} rate={rate} inputAvailable={} otherAudio={} route={}",
            category.map(|v| v.to_string()).unwrap_or_default(),
            mode.map(|v| v.to_string()).unwrap_or_default(),
            input.as_bool(),
            other.as_bool(),
            route
                .map(|v| v.to_string().replace('\n', " "))
                .unwrap_or_default(),
        )
    }
}

#[cfg(target_os = "ios")]
#[allow(unsafe_code)]
pub fn activate() -> String {
    let before = describe();
    let errors = set_category();
    format!(
        "before: {before} | errors: [{errors}] | after: {}",
        describe()
    )
}

#[cfg(target_os = "ios")]
#[allow(unsafe_code)]
fn set_category() -> String {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSError, NSString};

    /// `AVAudioSessionCategoryOptionAllowBluetooth | ...DefaultToSpeaker`.
    const OPTIONS: usize = 0x4 | 0x8;

    let Some(class) = AnyClass::get(c"AVAudioSession") else {
        tracing::warn!("AVAudioSession is not available");
        return "AVAudioSession is not available".into();
    };
    // SAFETY: `sharedInstance` is a class method returning the singleton
    // session (or nil); the selectors below match AVAudioSession's documented
    // signatures (`BOOL` results are folded into `Result` by `error: _`).
    unsafe {
        let session: Option<Retained<AnyObject>> = msg_send![class, sharedInstance];
        let Some(session) = session else {
            return "no shared session".into();
        };
        let mut errors = Vec::new();
        let category = NSString::from_str("AVAudioSessionCategoryPlayAndRecord");
        let set: Result<(), Retained<NSError>> =
            msg_send![&*session, setCategory: &*category, withOptions: OPTIONS, error: _];
        if let Err(e) = set {
            tracing::warn!("cannot set the audio session category: {e:?}");
            errors.push(format!("setCategory: {e:?}"));
        }
        let active: Result<(), Retained<NSError>> = msg_send![&*session, setActive: true, error: _];
        if let Err(e) = active {
            tracing::warn!("cannot activate the audio session: {e:?}");
            errors.push(format!("setActive: {e:?}"));
        }
        errors.join("; ")
    }
}
