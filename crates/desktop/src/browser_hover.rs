//! The link under the pointer in the browser pane, for the status line under
//! its page: what a browser shows at the bottom of its window. It cannot be
//! drawn over the page as a browser does, since the page is a native view
//! above everything the app draws.
//!
//! Each platform's webview says which link the pointer is over: WebKit on
//! macOS through a private callback to its UI delegate -- Safari's status URL
//! comes from it -- WebKitGTK through a public signal, and WebView2 through
//! its status text, with its own status bubble turned off so the line under
//! the page is the only one.

use std::sync::Mutex;

use tauri::{AppHandle, Webview};
use tracing::error;

use crate::browser_pane::{emit, PageEvent};

#[cfg(target_os = "linux")]
mod gtk;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod webview2;

/// Start telling the window which link is under the pointer in a pane just
/// made. Only changes are told: the platforms report every move.
pub(crate) fn listen_for_hover(webview: &Webview, app: &AppHandle, owner: &str) {
    let report = {
        let app = app.clone();
        let owner = owner.to_string();
        let last = Mutex::new(None::<String>);
        move |url: Option<String>| {
            let url = url.filter(|url| !url.is_empty());
            let mut last = last.lock().unwrap();
            if *last != url {
                last.clone_from(&url);
                emit(&app, &owner, PageEvent::Hover { url });
            }
        }
    };
    #[cfg(target_os = "macos")]
    let listening = macos::listen_for_hover(webview, owner, report);
    #[cfg(target_os = "linux")]
    let listening = gtk::listen_for_hover(webview, report);
    #[cfg(windows)]
    let listening = webview2::listen_for_hover(webview, report);
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    let listening: tauri::Result<()> = {
        let _ = (webview, report);
        Ok(())
    };
    if let Err(e) = listening {
        error!("browser pane: could not listen for the link under the pointer: {e}");
    }
}

/// Stop for a pane about to close. Only macOS holds anything for it: the
/// stand-in delegate, which the app keeps since WebKit holds delegates weakly.
pub(crate) fn stop_listening(webview: &Webview, owner: &str) {
    #[cfg(target_os = "macos")]
    if let Err(e) = macos::stop_listening(webview, owner) {
        error!("browser pane: could not stop listening for the link under the pointer: {e}");
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (webview, owner);
}
