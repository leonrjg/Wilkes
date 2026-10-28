//! Finding text in the browser pane's page, with the platform's own find.
//!
//! Each platform's webview can search its page natively, PDFs included, which
//! a script in the page cannot: WebKit's PDF viewer gives the page no text.
//! Windows needs nothing here -- WebView2 answers Ctrl+F with its own find
//! bar. On macOS and Linux the pane's header shows a find strip, opened by
//! Cmd/Ctrl+F while the pane has focus, and these commands do the finding.
//!
//! The app's own webview keeps Cmd/Ctrl+F for itself: the reader's find bar
//! listens for it there. That rules out a Find menu item on macOS, which
//! takes the key from every webview, the app's included.

use serde::Serialize;
use tauri::{AppHandle, Manager, Webview, Window};
use tracing::error;

use crate::browser_pane::{emit, pane, PageEvent};

#[cfg(target_os = "linux")]
mod gtk;
#[cfg(target_os = "macos")]
mod macos;

/// What a find turned up. `matches` is how many there are, where the
/// platform counts them: WebKitGTK does, WKWebView only says whether it found
/// one.
#[derive(Debug, Serialize, PartialEq)]
pub struct Found {
    found: bool,
    matches: Option<u32>,
}

/// Ask the window's page for its find strip, with the keyboard in it: the
/// strip is the app's, and keys go to whichever webview has focus.
fn request_find(app: &AppHandle, owner: &str) {
    match app.get_webview(owner) {
        Some(webview) => {
            if let Err(e) = webview.set_focus() {
                error!("browser pane: could not move the keyboard to the find strip: {e}");
            }
        }
        None => error!("browser pane: {owner} has no webview to find in"),
    }
    emit(app, owner, PageEvent::FindRequested);
}

/// Start answering Cmd/Ctrl+F in a pane just made, where the platform needs
/// it: only while the pane has the keyboard, and in place of its page.
pub(crate) fn listen_for_find_key(webview: &Webview, app: &AppHandle, owner: &str) {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let request = {
            let app = app.clone();
            let owner = owner.to_string();
            move || request_find(&app, &owner)
        };
        #[cfg(target_os = "macos")]
        let listening = macos::listen_for_find_key(webview, owner, request);
        #[cfg(target_os = "linux")]
        let listening = gtk::listen_for_find_key(webview, request);
        if let Err(e) = listening {
            error!("browser pane: could not listen for Cmd/Ctrl+F: {e}");
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = (webview, app, owner);
}

/// Stop answering Cmd+F for a pane about to close. On Linux the key's handler
/// goes with the pane's widget; on macOS it is the app's, and stays until
/// removed.
pub(crate) fn stop_listening(webview: &Webview, owner: &str) {
    #[cfg(target_os = "macos")]
    if let Err(e) = macos::stop_listening(webview, owner) {
        error!("browser pane: could not stop listening for Cmd+F: {e}");
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (webview, owner);
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const UNSUPPORTED: &str = "On Windows the pane finds with WebView2's own find bar";

/// Find `query` in the calling window's pane: the next match, or with
/// `backwards` the one before. Asking again with the same text steps on.
#[tauri::command]
pub async fn browser_pane_find(
    window: Window,
    query: String,
    backwards: bool,
) -> Result<Found, String> {
    let webview = pane(&window)?;
    #[cfg(target_os = "macos")]
    return macos::find(&webview, query, backwards).await;
    #[cfg(target_os = "linux")]
    return gtk::find(&webview, query, backwards).await;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (webview, query, backwards);
        Err(UNSUPPORTED.to_string())
    }
}

/// Stop finding and give the keyboard back to the page. WebKitGTK clears its
/// highlights; WKWebView has no way to, so the last match stays selected.
#[tauri::command]
pub async fn browser_pane_end_find(window: Window) -> Result<(), String> {
    let webview = pane(&window)?;
    #[cfg(target_os = "linux")]
    gtk::end_find(&webview).map_err(|e| e.to_string())?;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    return webview.set_focus().map_err(|e| e.to_string());
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = webview;
        Err(UNSUPPORTED.to_string())
    }
}
