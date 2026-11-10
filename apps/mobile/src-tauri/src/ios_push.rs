//! iOS side of approval alerts: notification permission, the APNs device
//! token and the "opened from a notification" signal. No maintained official
//! Tauri plugin hands out the token (`tauri-plugin-notification` is local
//! notifications only), so this is the smallest native hook, in the style of
//! [`crate::ios_launch_url`].
//!
//! tao's app delegate has no remote notification callbacks, so [`install`]
//! adds the two UIKit calls to it. Built only with the `native-push` feature:
//! `registerForRemoteNotifications` fails without the `aps-environment`
//! entitlement, which needs the Push Notifications capability on the App ID.
//!
//! The token is handed to the shell and never logged. DNSPilot's spec does not
//! say where a push carries its ciphertext, so a tap reports "opened" without
//! a payload and the app shows the newest pending decision.

// Adding Objective-C methods and delegates needs the runtime API.
#![allow(unsafe_code)]

use std::sync::Arc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, ffi, msg_send, sel};
use objc2_foundation::{NSData, NSError};
use objc2_ui_kit::UIApplication;
use objc2_user_notifications::{
    UNAuthorizationOptions, UNNotificationResponse, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};
use vca_shell::push::{
    NativePush, PushPlatform, record_native_failure, record_native_token, record_opened,
};

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class has no
    // Drop implementation.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VcaPushDelegate"]
    struct PushDelegate;

    // SAFETY: NSObjectProtocol has no additional safety requirements.
    unsafe impl NSObjectProtocol for PushDelegate {}

    // SAFETY: the signature matches the protocol's optional method.
    unsafe impl UNUserNotificationCenterDelegate for PushDelegate {
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            _response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            record_opened(None);
            completion.call(());
        }
    }
);

/// `(self, _cmd, application, argument)` of both UIKit callbacks.
type Callback = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject);

/// `-[AppDelegate application:didRegisterForRemoteNotificationsWithDeviceToken:]`.
unsafe extern "C-unwind" fn did_register(
    _this: *mut AnyObject,
    _cmd: Sel,
    _app: *mut AnyObject,
    token: *mut AnyObject,
) {
    // SAFETY: UIKit passes a valid NSData.
    match unsafe { token.cast::<NSData>().as_ref() } {
        Some(data) => {
            record_native_token(data.to_vec().iter().map(|b| format!("{b:02x}")).collect())
        }
        None => record_native_failure("push_registration_failed"),
    }
}

/// `-[AppDelegate application:didFailToRegisterForRemoteNotificationsWithError:]`.
unsafe extern "C-unwind" fn did_fail(
    _this: *mut AnyObject,
    _cmd: Sel,
    _app: *mut AnyObject,
    _error: *mut AnyObject,
) {
    record_native_failure("push_registration_failed");
}

/// Adds the APNs callbacks to tao's app delegate and sets the notification
/// delegate. Call after the tauri app is built (which registers tao's class)
/// and before it runs: a tap that launches the app goes to this delegate.
pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(class) = AnyClass::get(c"AppDelegate") else {
        return;
    };
    let register: Callback = did_register;
    let fail: Callback = did_fail;
    // SAFETY: both functions match the selectors' signature, and the encoding
    // string says so. `class_addMethod` leaves an existing method alone.
    unsafe {
        let class = std::ptr::from_ref(class).cast_mut();
        ffi::class_addMethod(
            class,
            sel!(application:didRegisterForRemoteNotificationsWithDeviceToken:),
            std::mem::transmute::<Callback, objc2::runtime::Imp>(register),
            c"v@:@@".as_ptr(),
        );
        ffi::class_addMethod(
            class,
            sel!(application:didFailToRegisterForRemoteNotificationsWithError:),
            std::mem::transmute::<Callback, objc2::runtime::Imp>(fail),
            c"v@:@@".as_ptr(),
        );
    }
    // SAFETY: plain NSObject initialization on the main thread.
    let delegate: Retained<PushDelegate> = unsafe { msg_send![PushDelegate::alloc(mtm), init] };
    UNUserNotificationCenter::currentNotificationCenter()
        .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // The center holds its delegate weakly; this one lasts as long as the app.
    std::mem::forget(delegate);
}

fn register_for_remote() {
    if let Some(mtm) = MainThreadMarker::new() {
        UIApplication::sharedApplication(mtm).registerForRemoteNotifications();
    }
}

/// Runs a function on the main thread (`AppHandle::run_on_main_thread`).
pub type RunOnMain = Arc<dyn Fn(fn()) + Send + Sync>;

pub struct IosPush {
    pub run_on_main: RunOnMain,
}

impl NativePush for IosPush {
    fn platform(&self) -> PushPlatform {
        PushPlatform::Ios
    }

    fn request_token(&self) {
        let run_on_main = Arc::clone(&self.run_on_main);
        let answered = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            if granted.as_bool() {
                run_on_main(register_for_remote);
            } else {
                record_native_failure("push_permission");
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &answered,
            );
    }
}
