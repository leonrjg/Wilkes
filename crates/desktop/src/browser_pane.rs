//! The docked browser pane: one native child webview per window, laid over a
//! placeholder the page lays out.
//!
//! The page owns the layout and reports the placeholder's rectangle; this
//! module owns the webview and keeps it at that rectangle. The webview is a
//! native view above the page's own, so nothing the page draws -- menus,
//! modals, toasts -- appears over it. That is accepted: the alternative, an
//! `<iframe>`, is refused by most of the sites a reader follows a link to.
//!
//! Every pane shares one persistent data store, kept apart from the app's own
//! webview, so a sign-in to a publisher or a library proxy survives restarts.
//! What the page in the pane is doing (its address, whether it is loading,
//! its title) goes back to the window that owns the pane as events.

use serde::{Deserialize, Serialize};
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, Webview, WebviewBuilder,
    WebviewUrl, Window,
};
use tracing::error;

/// The event a window hears about the page in its pane.
pub const PAGE_EVENT: &str = "browser-pane:page";

/// The data store every pane opens; any fixed value would do, but it must not
/// change, or every sign-in made in the pane is forgotten.
#[cfg(target_os = "macos")]
const DATA_STORE: [u8; 16] = [
    0x32, 0x79, 0x85, 0x6d, 0x56, 0x0b, 0x4e, 0xa1, 0x94, 0x7c, 0xe6, 0x26, 0x29, 0x52, 0xb9, 0x9d,
];

/// The placeholder's rectangle in CSS pixels, relative to the owning window's
/// own webview.
#[derive(Debug, Deserialize)]
pub struct PaneBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PageEvent {
    Loading { url: String },
    Loaded { url: String },
    Title { title: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Back,
    Forward,
    Reload,
}

fn label(owner: &str) -> String {
    format!("browser-pane-{owner}")
}

/// Only web pages: the pane is not a way into the filesystem or the app.
fn web_url(url: &str) -> Result<Url, String> {
    let url = Url::parse(url).map_err(|e| format!("Not a web address: {e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        scheme => Err(format!(
            "The browser pane does not open {scheme}: addresses"
        )),
    }
}

fn pane(window: &Window) -> Result<Webview, String> {
    window
        .app_handle()
        .get_webview(&label(window.label()))
        .ok_or_else(|| "The browser pane is not open".to_string())
}

fn emit(app: &AppHandle, owner: &str, event: PageEvent) {
    if let Err(e) = app.emit_to(owner, PAGE_EVENT, event) {
        error!("browser pane: could not tell {owner} about its page: {e}");
    }
}

fn place(webview: &Webview, bounds: &PaneBounds) -> Result<(), String> {
    webview
        .set_position(LogicalPosition::new(bounds.x, bounds.y))
        .map_err(|e| e.to_string())?;
    webview
        .set_size(LogicalSize::new(bounds.width, bounds.height))
        .map_err(|e| e.to_string())
}

/// Open `url` in the calling window's pane, creating the pane at `bounds` if
/// the window has none yet.
#[tauri::command]
pub async fn browser_pane_show(
    window: Window,
    url: String,
    bounds: PaneBounds,
) -> Result<(), String> {
    let url = web_url(&url)?;
    if let Ok(webview) = pane(&window) {
        webview.navigate(url).map_err(|e| e.to_string())?;
        return place(&webview, &bounds);
    }

    let app = window.app_handle().clone();
    let owner = window.label().to_string();
    let label = label(&owner);
    let builder = WebviewBuilder::new(&label, WebviewUrl::External(url))
        .on_page_load({
            let app = app.clone();
            let owner = owner.clone();
            move |_, payload| {
                let url = payload.url().to_string();
                let event = match payload.event() {
                    PageLoadEvent::Started => PageEvent::Loading { url },
                    PageLoadEvent::Finished => PageEvent::Loaded { url },
                };
                emit(&app, &owner, event);
            }
        })
        .on_document_title_changed({
            let app = app.clone();
            let owner = owner.clone();
            move |_, title| emit(&app, &owner, PageEvent::Title { title })
        })
        // A page that asks for a new window gets the pane it is already in.
        .on_new_window({
            let app = app.clone();
            let label = label.clone();
            move |url, _| {
                match app.get_webview(&label) {
                    Some(webview) => {
                        if let Err(e) = webview.navigate(url) {
                            error!("browser pane: could not follow a new-window link: {e}");
                        }
                    }
                    None => error!("browser pane: a new-window link arrived after {label} closed"),
                }
                NewWindowResponse::Deny
            }
        });
    #[cfg(target_os = "macos")]
    let builder = builder.data_store_identifier(DATA_STORE);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.data_directory(crate::app_context(&app).shared_data_dir.join("browser"));

    window
        .add_child(
            builder,
            LogicalPosition::new(bounds.x, bounds.y),
            LogicalSize::new(bounds.width, bounds.height),
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Follow the placeholder. A pane that is not open has nothing to move: the
/// page reports its layout until it hears the pane is gone.
#[tauri::command]
pub async fn browser_pane_set_bounds(window: Window, bounds: PaneBounds) -> Result<(), String> {
    match pane(&window) {
        Ok(webview) => place(&webview, &bounds),
        Err(_) => Ok(()),
    }
}

/// Step aside while the page needs the pointer: a native view takes every
/// mouse event over it, so a drag in the page that crosses the pane would lose
/// its moves and its release.
#[tauri::command]
pub async fn browser_pane_set_visible(window: Window, visible: bool) -> Result<(), String> {
    let webview = pane(&window)?;
    if visible {
        webview.show()
    } else {
        webview.hide()
    }
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn browser_pane_step(window: Window, step: Step) -> Result<(), String> {
    let webview = pane(&window)?;
    match step {
        Step::Back => webview.eval("history.back()"),
        Step::Forward => webview.eval("history.forward()"),
        Step::Reload => webview.reload(),
    }
    .map_err(|e| e.to_string())
}

/// Forget every cookie and site record the panes hold, then reload, so the
/// page in view shows what a signed-out visit sees.
#[tauri::command]
pub async fn browser_pane_clear_data(window: Window) -> Result<(), String> {
    let webview = pane(&window)?;
    webview
        .clear_all_browsing_data()
        .map_err(|e| e.to_string())?;
    webview.reload().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn browser_pane_close(window: Window) -> Result<(), String> {
    match pane(&window) {
        Ok(webview) => webview.close().map_err(|e| e.to_string()),
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pane_opens_web_addresses_only() {
        assert!(web_url("https://example.org/a").is_ok());
        assert!(web_url("http://example.org").is_ok());
        assert!(web_url("file:///etc/passwd").is_err());
        assert!(web_url("tauri://localhost").is_err());
        assert!(web_url("javascript:alert(1)").is_err());
        assert!(web_url("not a url").is_err());
    }

    #[test]
    fn each_window_has_its_own_pane() {
        assert_eq!(label("main"), "browser-pane-main");
        assert_ne!(label("main"), label("document"));
    }
}
