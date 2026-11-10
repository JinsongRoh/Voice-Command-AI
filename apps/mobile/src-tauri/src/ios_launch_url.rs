//! iOS cold-start deep links.
//!
//! With the UIScene lifecycle (which UIKit uses for apps built with current
//! SDKs), a URL that *launches* the app is delivered only in
//! `UISceneConnectionOptions.URLContexts` of
//! `scene:willConnectToSession:options:`. tao 0.37 ignores those options (it
//! only forwards `scene:openURLContexts:`, i.e. links that arrive while the
//! app runs), so `tauri-plugin-deep-link` never hears about the launch URL and
//! its `getCurrent()` stays empty: a `vca://pair` link opened from the camera
//! fills the pairing form on a warm start but not on a cold one.
//!
//! [`install`] wraps tao's scene delegate method: it records the connection
//! options' URLs with [`vca_shell::record_launch_link`] (the UI reads them via
//! `vca_launch_links`) and then calls tao's implementation unchanged.

// Swapping an Objective-C method implementation needs the runtime API.
#![allow(unsafe_code)]

use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{msg_send, sel};
use objc2_foundation::NSSet;
use objc2_ui_kit::{UIOpenURLContext, UISceneConnectionOptions};

/// `-[TaoSceneDelegate scene:willConnectToSession:options:]`.
type WillConnect = unsafe extern "C-unwind" fn(
    this: *mut AnyObject,
    cmd: Sel,
    scene: *mut AnyObject,
    session: *mut AnyObject,
    options: *mut UISceneConnectionOptions,
);

static ORIGINAL: OnceLock<WillConnect> = OnceLock::new();

/// Hooks tao's scene delegate. Call after the tauri app is built (which
/// registers the delegate class) and before it runs (when UIKit connects the
/// first scene). Does nothing if tao's class or method is not found.
pub fn install() {
    if ORIGINAL.get().is_some() {
        return;
    }
    let Some(class) = AnyClass::get(c"TaoSceneDelegate") else {
        return;
    };
    let selector = sel!(scene:willConnectToSession:options:);
    // Only a method the class itself defines: replacing an inherited one
    // would change the superclass for every subclass.
    let methods = class.instance_methods();
    let Some(method) = methods.iter().find(|m| m.name() == selector) else {
        return;
    };
    let replacement: WillConnect = will_connect;
    // SAFETY: `will_connect` has the exact signature of the method it
    // replaces (`void (id, SEL, UIScene*, UISceneSession*,
    // UISceneConnectionOptions*)`), and the original IMP is called back
    // with that same signature. This runs on the main thread before
    // `UIApplicationMain`, so the method cannot be running concurrently.
    unsafe {
        let original =
            method.set_implementation(std::mem::transmute::<WillConnect, Imp>(replacement));
        let _ = ORIGINAL.set(std::mem::transmute::<Imp, WillConnect>(original));
    }
}

unsafe extern "C-unwind" fn will_connect(
    this: *mut AnyObject,
    cmd: Sel,
    scene: *mut AnyObject,
    session: *mut AnyObject,
    options: *mut UISceneConnectionOptions,
) {
    // SAFETY: UIKit passes a valid (or nil) connection options object.
    if let Some(options) = unsafe { options.as_ref() } {
        // `URLContexts` is nil on a plain launch (no URL). The generated
        // binding declares it non-null and panics on nil, which aborts the
        // app at startup, so read it as nullable.
        // SAFETY: `URLContexts` takes no arguments and returns an NSSet or nil.
        let contexts: Option<Retained<NSSet<UIOpenURLContext>>> =
            unsafe { msg_send![options, URLContexts] };
        for context in contexts.iter().flat_map(|set| set.iter()) {
            if let Some(url) = context.URL().absoluteString() {
                vca_shell::record_launch_link(url.to_string());
            }
        }
    }
    if let Some(original) = ORIGINAL.get() {
        // SAFETY: tao's original implementation, same arguments.
        unsafe { original(this, cmd, scene, session, options) };
    }
}
