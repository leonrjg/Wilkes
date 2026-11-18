//! The HTTP status of each page the agent's pane loads, on the platforms
//! where it is heard apart from the navigation itself: WebKitGTK says it as
//! the response is committed, WebView2 once the navigation completes. On
//! macOS the pane's navigation stand-in hears it (`browser_webkit_download`).
//!
//! The agent judges a page by it: a document answered with an error is not
//! what was asked for, and a bot check's status says the page may yet move on.

use tauri::{AppHandle, Webview};
use tracing::error;

/// Start noting the status of every page the agent's pane loads.
pub(crate) fn listen(webview: &Webview, app: &AppHandle) {
    #[cfg(target_os = "linux")]
    let listening = gtk::listen(webview, app.clone());
    #[cfg(windows)]
    let listening = webview2::listen(webview, app.clone());
    #[cfg(not(any(target_os = "linux", windows)))]
    let listening: tauri::Result<()> = {
        let _ = (webview, app);
        Ok(())
    };
    if let Err(e) = listening {
        error!("agent browser: could not listen for its pages' statuses: {e}");
    }
}

#[cfg(target_os = "linux")]
mod gtk {
    use tauri::{AppHandle, Webview};
    use webkit2gtk::{LoadEvent, URIResponseExt, WebResourceExt, WebViewExt};

    use crate::browser_pane::PaneId;

    pub fn listen(webview: &Webview, app: AppHandle) -> tauri::Result<()> {
        webview.with_webview(move |platform| {
            platform.inner().connect_load_changed(move |view, event| {
                if event != LoadEvent::Committed {
                    return;
                }
                let status = view
                    .main_resource()
                    .and_then(|resource| resource.response())
                    .map(|response| response.status_code())
                    // Not an HTTP document: WebKitGTK says 0.
                    .filter(|status| *status != 0)
                    .and_then(|status| u16::try_from(status).ok());
                crate::agent_browser::response_arrived(&app, &PaneId::agent(), status);
            });
        })
    }
}

#[cfg(windows)]
mod webview2 {
    use tauri::{AppHandle, Webview};
    use tracing::error;
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2NavigationCompletedEventArgs2;
    use webview2_com::NavigationCompletedEventHandler;
    use windows_core::Interface;

    pub fn listen(webview: &Webview, app: AppHandle) -> tauri::Result<()> {
        webview.with_webview(move |platform| {
            // SAFETY: COM calls on the pane's own live WebView2, on the thread
            // that made it, where `with_webview` calls back.
            let listening = unsafe {
                (|| -> windows_core::Result<()> {
                    let core = platform.controller().CoreWebView2()?;
                    let handler =
                        NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                            let Some(args) = args else { return Ok(()) };
                            let mut status = 0;
                            args.cast::<ICoreWebView2NavigationCompletedEventArgs2>()?
                                .HttpStatusCode(&mut status)?;
                            // 0: no HTTP status, as for a page that failed to load.
                            let status = u16::try_from(status).ok().filter(|status| *status != 0);
                            crate::agent_browser::response_completed(&app, status);
                            Ok(())
                        }));
                    let mut token = 0;
                    core.add_NavigationCompleted(&handler, &mut token)
                })()
            };
            if let Err(e) = listening {
                error!("agent browser: WebView2 would not report its pages' statuses: {e}");
            }
        })
    }
}
