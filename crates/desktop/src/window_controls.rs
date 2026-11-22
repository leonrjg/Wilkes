//! Where the window's own controls -- macOS's traffic lights -- are drawn over
//! the page.
//!
//! The main window's title bar is overlaid on the page (`titleBarStyle:
//! Overlay` in tauri.conf.json), so the page's top row has to start after the
//! traffic lights and be centred on them. Where AppKit draws them depends on
//! the system's version: the `trafficLightPosition` there moves them down to
//! roughly the top row's height, and this measures where they actually are,
//! for the page to lay itself out against.

use serde::Serialize;

/// The traffic lights' extent, in CSS pixels from the webview's top-left.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub(crate) struct WindowControls {
    /// The buttons' vertical centre.
    center_y: f64,
    /// The right edge of the last button.
    right: f64,
}

/// The traffic lights over the calling webview, or none where none are drawn
/// over it: on other platforms, and in full screen, where macOS shows them
/// with the menu bar instead.
#[tauri::command]
pub(crate) async fn window_controls(
    webview: tauri::Webview,
) -> Result<Option<WindowControls>, String> {
    #[cfg(target_os = "macos")]
    {
        let (sender, measured) = tokio::sync::oneshot::channel();
        webview
            .with_webview(move |platform| {
                // SAFETY: `with_webview` calls back on the main thread, with
                // this webview's live WKWebView and the NSWindow holding it.
                let controls =
                    unsafe { measure(&*platform.inner().cast(), &*platform.ns_window().cast()) };
                if sender.send(controls).is_err() {
                    tracing::warn!("window controls: measured after the request was dropped");
                }
            })
            .map_err(|error| format!("Could not reach the window: {error}"))?;
        measured
            .await
            .map_err(|_| "The window closed before its controls were measured".to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = webview;
        Ok(None)
    }
}

/// The close and zoom buttons' rectangles, in `view`'s coordinates turned
/// top-down as the page's are.
#[cfg(target_os = "macos")]
unsafe fn measure(
    view: &objc2_app_kit::NSView,
    window: &objc2_app_kit::NSWindow,
) -> Option<WindowControls> {
    use objc2_app_kit::{NSButton, NSWindowButton, NSWindowStyleMask};
    use objc2_foundation::NSRect;

    if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
        return None;
    }
    let close = window.standardWindowButton(NSWindowButton::CloseButton)?;
    let zoom = window.standardWindowButton(NSWindowButton::ZoomButton)?;
    if close.isHidden() || zoom.isHidden() {
        return None;
    }
    let in_view = |button: &NSButton| -> NSRect {
        let mut rect = button.convertRect_toView(button.bounds(), Some(view));
        if !view.isFlipped() {
            rect.origin.y = view.bounds().size.height - rect.origin.y - rect.size.height;
        }
        rect
    };
    let close = in_view(&close);
    let zoom = in_view(&zoom);
    Some(WindowControls {
        center_y: close.origin.y + close.size.height / 2.0,
        right: zoom.origin.x + zoom.size.width,
    })
}
