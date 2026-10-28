//! Finding with WKWebView's own find, which searches whatever the view shows:
//! a page, or a PDF in WebKit's viewer.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::AnyObject;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSView, NSWindow};
use objc2_foundation::NSString;
use objc2_web_kit::{WKFindConfiguration, WKFindResult, WKWebView};
use tauri::Webview;
use tracing::error;

use super::Found;

thread_local! {
    /// Each pane's key monitor, by the window that owns the pane. AppKit
    /// calls monitors on the main thread, which is where these live.
    static MONITORS: RefCell<HashMap<String, Retained<AnyObject>>> = RefCell::new(HashMap::new());
}

/// Whether the keyboard of `window`, the key window, is in `view` or in
/// anything inside it, such as WebKit's PDF viewer.
fn holds_keyboard(view: &NSView, window: &NSWindow) -> bool {
    window.isKeyWindow()
        && window.firstResponder().is_some_and(|responder| {
            responder
                .downcast_ref::<NSView>()
                .is_some_and(|responder| responder.isDescendantOf(view))
        })
}

/// Call `on_find` for Cmd+F while the pane has the keyboard, in place of its
/// page. A monitor sees each key before AppKit hands it on, so the app's own
/// webview still gets its Cmd+F.
pub fn listen_for_find_key(
    webview: &Webview,
    owner: &str,
    on_find: impl Fn() + Send + 'static,
) -> tauri::Result<()> {
    let owner = owner.to_string();
    webview.with_webview(move |platform| {
        let mtm = MainThreadMarker::new().expect("with_webview calls back on the main thread");
        // SAFETY: `inner` is the pane's live WKWebView, an NSView, and this
        // is the main thread. Held weakly: a pane whose window went without
        // closing it first is not kept alive by its monitor.
        let view = unsafe { Retained::retain(platform.inner().cast::<NSView>()) }
            .expect("a pane has a webview");
        let view = Weak::from_retained(&view);
        let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit hands the monitor a live event.
            let key = unsafe { event.as_ref() };
            let flags = key.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask;
            let cmd_f = flags == NSEventModifierFlags::Command
                && key
                    .charactersIgnoringModifiers()
                    .is_some_and(|c| c.to_string() == "f");
            let pane = view.load();
            if cmd_f
                && pane.is_some_and(|pane| {
                    key.window(mtm)
                        .is_some_and(|window| holds_keyboard(&pane, &window))
                })
            {
                on_find();
                std::ptr::null_mut()
            } else {
                event.as_ptr()
            }
        });
        // SAFETY: the handler returns the event or nothing, as AppKit asks.
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler)
        };
        match monitor {
            Some(monitor) => {
                let replaced = MONITORS.with_borrow_mut(|monitors| monitors.insert(owner, monitor));
                if let Some(replaced) = replaced {
                    // SAFETY: a monitor AppKit returned, removed once.
                    unsafe { NSEvent::removeMonitor(&replaced) };
                }
            }
            None => error!("browser pane: AppKit gave no key monitor for Cmd+F"),
        }
    })
}

pub fn stop_listening(webview: &Webview, owner: &str) -> tauri::Result<()> {
    let owner = owner.to_string();
    webview.with_webview(move |_| {
        if let Some(monitor) = MONITORS.with_borrow_mut(|monitors| monitors.remove(&owner)) {
            // SAFETY: a monitor AppKit returned, removed once.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
    })
}

pub async fn find(webview: &Webview, query: String, backwards: bool) -> Result<Found, String> {
    let (answer, answered) = tokio::sync::oneshot::channel();
    webview
        .with_webview(move |platform| {
            let mtm = MainThreadMarker::new().expect("with_webview calls back on the main thread");
            // SAFETY: as in `listen_for_find_key`; `inner` is a WKWebView.
            let view = unsafe { &*platform.inner().cast::<WKWebView>() };
            let configuration = unsafe { WKFindConfiguration::new(mtm) };
            unsafe {
                configuration.setBackwards(backwards);
                configuration.setCaseSensitive(false);
                configuration.setWraps(true);
            }
            // WebKit answers once; the block only has to hand that on.
            let answer = Cell::new(Some(answer));
            let done = RcBlock::new(move |result: NonNull<WKFindResult>| {
                // SAFETY: WebKit hands the block a live result.
                let found = unsafe { result.as_ref().matchFound() };
                if let Some(answer) = answer.take() {
                    // Nothing is waiting if the command was dropped meanwhile.
                    answer.send(found).ok();
                }
            });
            unsafe {
                view.findString_withConfiguration_completionHandler(
                    &NSString::from_str(&query),
                    Some(&configuration),
                    &done,
                );
            }
        })
        .map_err(|e| e.to_string())?;
    let found = answered
        .await
        .map_err(|_| "The pane closed before its page was searched".to_string())?;
    Ok(Found {
        found,
        matches: None,
    })
}
