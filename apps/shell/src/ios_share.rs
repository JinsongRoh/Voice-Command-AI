//! iOS share sheet for `vca_share_text_file` (see `share_file`).
//!
//! `navigator.share` in WKWebView presents the system sheet as a popover
//! anchored inside the web view, which on an iPhone squeezes it into a small
//! box with oversized icons. Presenting `UIActivityViewController` from the
//! key window's view controller gives the normal full-width sheet.
//!
//! Plain Objective-C messages (as in the mobile crate's `ios_launch_url`):
//! no Swift plugin, because current Xcode drops non-public `@_cdecl` entry
//! points from optimized builds.

// Sending Objective-C messages and passing blocks needs the runtime API.
#![allow(unsafe_code)]

use std::cell::Cell;
use std::path::Path;

use block2::RcBlock;
use objc2::encode::{Encode, Encoding};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool};
use objc2::{MainThreadMarker, class, msg_send};
use objc2_foundation::{NSArray, NSString, NSURL};
use objc2_ui_kit::UIApplication;

/// `CGPoint` (64-bit: `CGFloat` is `double`).
#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

/// `CGSize`.
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

/// `CGRect`.
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

// SAFETY: the same layout and field types as the C structs, with their names.
unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
// SAFETY: as above.
unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}
// SAFETY: as above.
unsafe impl Encode for CGRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}

/// `UIPopoverArrowDirection` with no direction allowed: no arrow.
const NO_ARROW: usize = 0;

/// Presents the share sheet for the file at `path`. Must run on the main
/// thread. `done` is called once: `Ok(true)` when the user shared (an
/// activity completed), `Ok(false)` when the sheet was closed, `Err` when
/// nothing could be presented.
pub(crate) fn present(path: &Path, done: impl FnOnce(Result<bool, String>) + 'static) {
    let Some(mtm) = MainThreadMarker::new() else {
        done(Err(
            "the share sheet must be opened on the main thread".to_owned()
        ));
        return;
    };
    let Some(path) = path.to_str() else {
        done(Err("the file path is not valid UTF-8".to_owned()));
        return;
    };
    let Some(window) = key_window(mtm) else {
        done(Err("no window to present the share sheet from".to_owned()));
        return;
    };
    let Some(presenter) = top_view_controller(&window) else {
        done(Err(
            "no view controller to present the share sheet from".to_owned()
        ));
        return;
    };

    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    let items = NSArray::from_retained_slice(&[url]);
    // SAFETY: `alloc` and `initWithActivityItems:applicationActivities:`
    // with an NSArray of NSURLs and no custom activities.
    let sheet: Option<Retained<AnyObject>> = unsafe {
        let alloc: Allocated<AnyObject> = msg_send![class!(UIActivityViewController), alloc];
        msg_send![
            alloc,
            initWithActivityItems: &*items,
            applicationActivities: Option::<&AnyObject>::None,
        ]
    };
    let Some(sheet) = sheet else {
        done(Err("could not create the share sheet".to_owned()));
        return;
    };

    // `void (^)(UIActivityType, BOOL completed, NSArray *, NSError *)`. UIKit
    // calls it once the sheet is dismissed; `done` is taken so a second
    // call does nothing.
    let done = Cell::new(Some(done));
    let completion = RcBlock::new(
        move |_activity: *mut AnyObject,
              completed: Bool,
              _items: *mut AnyObject,
              _error: *mut AnyObject| {
            if let Some(done) = done.take() {
                done(Ok(completed.as_bool()));
            }
        },
    );
    // SAFETY: the property copies the block; the argument types match.
    unsafe {
        let _: () = msg_send![&*sheet, setCompletionWithItemsHandler: &*completion];
    }

    anchor_popover(&sheet, &presenter);

    // SAFETY: presenting a view controller from the topmost one, on the
    // main thread, without a completion block.
    unsafe {
        let _: () = msg_send![
            &*presenter,
            presentViewController: &*sheet,
            animated: true,
            completion: Option::<&block2::DynBlock<dyn Fn()>>::None,
        ];
    }
}

/// On iPad the sheet is a popover: anchor it at the bottom centre of the
/// presenter's view with no arrow, so it never points at a random spot. On
/// iPhone the popover settings are ignored (the sheet comes up from the
/// bottom).
fn anchor_popover(sheet: &AnyObject, presenter: &AnyObject) {
    // SAFETY: `popoverPresentationController` and `view` are read-only
    // properties that return an object or nil; the setters take the types
    // given (UIView, CGRect, UIPopoverArrowDirection = NSUInteger).
    unsafe {
        let popover: Option<Retained<AnyObject>> = msg_send![sheet, popoverPresentationController];
        let view: Option<Retained<AnyObject>> = msg_send![presenter, view];
        let (Some(popover), Some(view)) = (popover, view) else {
            return;
        };
        let bounds: CGRect = msg_send![&*view, bounds];
        let anchor = CGRect {
            origin: CGPoint {
                x: bounds.origin.x + bounds.size.width / 2.0,
                y: bounds.origin.y + bounds.size.height,
            },
            size: CGSize {
                width: 0.0,
                height: 0.0,
            },
        };
        let _: () = msg_send![&*popover, setSourceView: &*view];
        let _: () = msg_send![&*popover, setSourceRect: anchor];
        let _: () = msg_send![&*popover, setPermittedArrowDirections: NO_ARROW];
    }
}

/// The key window of the app's window scenes, or else the first window of
/// one (tao opens a single window).
fn key_window(mtm: MainThreadMarker) -> Option<Retained<AnyObject>> {
    let scenes = UIApplication::sharedApplication(mtm).connectedScenes();
    let mut first = None;
    for scene in scenes.iter() {
        // SAFETY: `isKindOfClass:` takes a class and returns a BOOL.
        let is_window_scene: bool =
            unsafe { msg_send![&*scene, isKindOfClass: class!(UIWindowScene)] };
        if !is_window_scene {
            continue;
        }
        // SAFETY: `-[UIWindowScene windows]` returns an NSArray of UIWindow.
        let windows: Option<Retained<NSArray<AnyObject>>> = unsafe { msg_send![&*scene, windows] };
        for window in windows.iter().flat_map(|w| w.iter()) {
            // SAFETY: `isKeyWindow` returns a BOOL.
            let is_key: bool = unsafe { msg_send![&*window, isKeyWindow] };
            if is_key {
                return Some(window);
            }
            if first.is_none() {
                first = Some(window);
            }
        }
    }
    first
}

/// The window's root view controller, or the one it currently presents
/// (presenting from a controller that already presents something fails).
fn top_view_controller(window: &AnyObject) -> Option<Retained<AnyObject>> {
    // SAFETY: `rootViewController` and `presentedViewController` return a
    // view controller or nil; `isBeingDismissed` returns a BOOL.
    unsafe {
        let root: Option<Retained<AnyObject>> = msg_send![window, rootViewController];
        let mut top = root?;
        loop {
            let next: Option<Retained<AnyObject>> = msg_send![&*top, presentedViewController];
            let Some(next) = next else {
                return Some(top);
            };
            let leaving: bool = msg_send![&*next, isBeingDismissed];
            if leaving {
                return Some(top);
            }
            top = next;
        }
    }
}
