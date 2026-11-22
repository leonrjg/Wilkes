//! Where the window's own controls -- macOS's traffic lights -- are drawn over
//! the page.
//!
//! A workspace window's title bar is overlaid on the page (`titleBarStyle:
//! Overlay` in tauri.conf.json), so the page's top row has to start after the
//! traffic lights and be centred on them. This module puts them where that row
//! is, keeps them there, and measures where they are for the page to lay
//! itself out against.
//!
//! AppKit lays the traffic lights out again whenever it likes -- on a new
//! title, a resize, a change of appearance -- and puts them back at its own
//! place each time. They are put back at ours as it does so, from the
//! notifications their frames post, so outside full screen they are in one
//! place for the window's whole life and one measure of them stays true.
//! (Tauri's `trafficLightPosition` only puts them back when the window is next
//! drawn, which left them at AppKit's place from a new title until the next
//! resize.)

use serde::Serialize;
static PLACES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static MOVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The rectangle the traffic lights take, from the close button's left edge
/// to the zoom button's right, in CSS pixels from the webview's top-left.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub(crate) struct WindowControls {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
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
        // TEMP-PROBE
        {
            static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let probe = webview.clone();
                tokio::spawn(async move {
                    let start = std::time::Instant::now();
                    for tick in 0..330u32 {
                        let t = start.elapsed().as_millis();
                        let _ = probe.with_webview(move |platform| unsafe {
                            let view: &objc2_app_kit::NSView = &*platform.inner().cast();
                            let window: &objc2_app_kit::NSWindow = &*platform.ns_window().cast();
                            let m = measure(view, window);
                            let strip = window.standardWindowButton(objc2_app_kit::NSWindowButton::CloseButton).and_then(|c| c.superview()).and_then(|v| v.superview()).map(|v| v.frame());
                            eprintln!("PROBE strip-at-tick {strip:?}");
                            eprintln!("PROBE t={t} lights={m:?} places={} moves={} win={:?}", PLACES.load(std::sync::atomic::Ordering::Relaxed), MOVES.load(std::sync::atomic::Ordering::Relaxed), window.frame().size);
                        });
                        let window = probe.window();
                        match tick {
                            40 => { eprintln!("PROBE action set_title"); let _ = window.set_title("Wilkes — probe"); }
                            80 => { eprintln!("PROBE action set_size"); let _ = window.set_size(tauri::LogicalSize::new(1100.0, 700.0)); }
                            120 => { eprintln!("PROBE action set_title2"); let _ = window.set_title("Wilkes — probe 2"); }
                            160 => { eprintln!("PROBE action fullscreen"); let _ = window.set_fullscreen(true); }
                            200 => { eprintln!("PROBE action unfullscreen"); let _ = window.set_fullscreen(false); }
                            240 => { eprintln!("PROBE action theme"); let _ = window.set_theme(Some(tauri::Theme::Dark)); }
                            270 => { eprintln!("PROBE action maximize"); let _ = window.maximize(); }
                            300 => { eprintln!("PROBE action unmaximize"); let _ = window.unmaximize(); let _ = window.set_theme(None); }
                            _ => {}
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                });
            }
        }
        let (sender, measured) = tokio::sync::oneshot::channel();
        webview
            .with_webview(move |platform| {
                // SAFETY: `with_webview` calls back on the main thread, with
                // this webview's live WKWebView and the NSWindow holding it.
                let controls =
                    unsafe { measure(&*platform.inner().cast(), &*platform.ns_window().cast()) };
                eprintln!("PROBE command measured {controls:?}");
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

/// Puts `window`'s traffic lights where the page's top row is and keeps them
/// there. Once for each window whose title bar is overlaid on its page.
pub(crate) fn pin(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        let on_main = window.clone();
        let pinned = window.run_on_main_thread(move || match on_main.ns_window() {
            // SAFETY: on the main thread, with this window's live NSWindow.
            Ok(ns_window) => unsafe { placement::pin(&*ns_window.cast()) },
            Err(error) => tracing::error!("window controls: no window to place: {error}"),
        });
        if let Err(error) = pinned {
            tracing::error!("window controls: could not reach the window: {error}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
    }
}

#[cfg(target_os = "macos")]
mod placement {
    use std::cell::RefCell;
    use std::ptr::NonNull;
    use std::rc::Rc;

    use block2::RcBlock;
    use objc2::rc::{Retained, Weak};
    use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
    use objc2_app_kit::{
        NSButton, NSView, NSViewFrameDidChangeNotification, NSWindow, NSWindowButton,
        NSWindowDidExitFullScreenNotification, NSWindowStyleMask, NSWindowWillCloseNotification,
    };
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSNotificationName};

    /// From the window's left edge to the close button's.
    const LEFT: f64 = 16.0;
    /// The height of the strip along the window's top that AppKit lays the
    /// buttons out in. It keeps them the same distance above the strip's
    /// bottom whatever its height, so a strip taller than AppKit's own 32
    /// moves them down by the difference.
    const STRIP_HEIGHT: f64 = 41.0;

    /// The strip and its close, minimise and zoom buttons, in that order.
    fn parts(window: &NSWindow) -> Option<(Retained<NSView>, [Retained<NSButton>; 3])> {
        let close = window.standardWindowButton(NSWindowButton::CloseButton)?;
        let minimise = window.standardWindowButton(NSWindowButton::MiniaturizeButton)?;
        let zoom = window.standardWindowButton(NSWindowButton::ZoomButton)?;
        // The buttons' own superview is the title bar; the strip holds that.
        let strip = unsafe { close.superview()?.superview()? };
        Some((strip, [close, minimise, zoom]))
    }

    /// Puts the strip and the buttons at their place, `pitch` from one
    /// button's left edge to the next's. Touches nothing already there, so
    /// the notifications its own moves post end here. Not in full screen,
    /// where the buttons are AppKit's to show with the menu bar.
    fn place(window: &NSWindow, pitch: f64) {
        super::PLACES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
            return;
        }
        let Some((strip, buttons)) = parts(window) else {
            tracing::error!("window controls: the window lost its buttons");
            return;
        };
        let Some(holder) = (unsafe { strip.superview() }) else {
            tracing::error!("window controls: the buttons' strip is in no view");
            return;
        };
        let mut frame = strip.frame();
        let bottom = holder.bounds().size.height - STRIP_HEIGHT;
        if frame.size.height != STRIP_HEIGHT || frame.origin.y != bottom {
            frame.size.height = STRIP_HEIGHT;
            frame.origin.y = bottom;
            eprintln!("PROBE strip {:?} -> h{} y{} holder={:?} {}", strip.frame(), frame.size.height, frame.origin.y, holder.bounds(), holder.class().name().to_string_lossy());
            strip.setFrame(frame);
            eprintln!("PROBE strip now {:?}", strip.frame());
            super::MOVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        for (index, button) in buttons.iter().enumerate() {
            let mut origin = button.frame().origin;
            let x = LEFT + index as f64 * pitch;
            if origin.x != x {
                origin.x = x;
                eprintln!("PROBE button {index} {:?} -> x{x}", button.frame());
                button.setFrameOrigin(origin);
                super::MOVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    /// Places `window`'s buttons, and again each time AppKit moves them or
    /// their strip, until the window closes.
    ///
    /// # Safety
    /// On the main thread.
    pub(super) unsafe fn pin(window: &NSWindow) {
        let Some((strip, buttons)) = parts(window) else {
            tracing::error!("window controls: the window has no buttons to place");
            return;
        };
        // AppKit's spacing, which depends on the system's version: read while
        // the buttons are as it laid them out, never while it is moving them.
        let pitch = buttons[1].frame().origin.x - buttons[0].frame().origin.x;
        place(window, pitch);

        let center = NSNotificationCenter::defaultCenter();
        let observers: Rc<RefCell<Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>>> =
            Rc::default();
        let observe = |name: &NSNotificationName,
                       object: &AnyObject,
                       block: &RcBlock<dyn Fn(NonNull<NSNotification>)>| {
            // SAFETY: `object` is the view or window the name is posted by,
            // and with no queue the block runs where the notification is
            // posted: on the main thread, as these all are.
            let observer = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    Some(object),
                    None,
                    block,
                )
            };
            observers.borrow_mut().push(observer);
        };

        let weak = Weak::new(window);
        let replace = RcBlock::new(move |n: NonNull<NSNotification>| {
            if super::PLACES.load(std::sync::atomic::Ordering::Relaxed) < 140 {
                let n = unsafe { n.as_ref() };
                let o = unsafe { n.object() };
                if let Some(o) = o {
                    let class = o.class().name().to_string_lossy().into_owned();
                    let frame = o.downcast_ref::<NSView>().map(|v| v.frame());
                    eprintln!("PROBE note {} from {class} frame={frame:?}", unsafe { n.name() });
                }
            }
            if let Some(window) = weak.load() {
                place(&window, pitch);
            }
        });
        strip.setPostsFrameChangedNotifications(true);
        observe(NSViewFrameDidChangeNotification, &strip, &replace);
        for button in &buttons {
            button.setPostsFrameChangedNotifications(true);
            observe(NSViewFrameDidChangeNotification, button, &replace);
        }
        // AppKit lays them out for the window while it still counts as in
        // full screen, which `place` leaves alone.
        observe(NSWindowDidExitFullScreenNotification, window, &replace);

        let closing = Rc::clone(&observers);
        let forget = RcBlock::new(move |_: NonNull<NSNotification>| {
            let center = NSNotificationCenter::defaultCenter();
            for observer in closing.borrow_mut().drain(..) {
                // SAFETY: an observer this centre returned.
                unsafe { center.removeObserver(observer.as_ref()) };
            }
        });
        observe(NSWindowWillCloseNotification, window, &forget);
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
        left: close.origin.x,
        top: close.origin.y,
        right: zoom.origin.x + zoom.size.width,
        bottom: close.origin.y + close.size.height,
    })
}
