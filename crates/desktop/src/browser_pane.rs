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
//!
//! In dark mode a light page is inverted the way the reader inverts a PDF.
//! Pages see the window's appearance, so a site with a dark theme of its own
//! shows it and is left alone: inverting it would make it light again.

use std::collections::HashMap;
use std::sync::Mutex;

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

/// How strongly each window's pane inverts a light page, keyed by the owning
/// window's label; a window without an entry does not invert. Kept here rather
/// than in the page because every page load replaces the page.
#[derive(Default)]
pub struct Inversions(Mutex<HashMap<String, f64>>);

impl Inversions {
    fn get(&self, owner: &str) -> Option<f64> {
        self.0.lock().unwrap().get(owner).copied()
    }

    fn set(&self, owner: &str, strength: Option<f64>) {
        let mut inversions = self.0.lock().unwrap();
        match strength {
            Some(strength) => inversions.insert(owner.to_string(), strength),
            None => inversions.remove(owner),
        };
    }
}

/// The script that inverts the page in view, or stops inverting it. It judges
/// the page once its markup is in: a page whose own background is dark is
/// already in dark mode. The filter is the reader's (`reader.css`,
/// `.pdf-dark-mode`), on the root so it covers the whole canvas; the root's
/// background is pinned to the page's light one, which is what turns dark.
fn inversion_script(strength: Option<f64>) -> String {
    let strength = strength.map_or_else(|| "null".to_string(), |s| s.to_string());
    format!(
        r#"(() => {{
  const strength = {strength};
  const id = "wilkes-dark-inversion";
  const background = (element) => {{
    if (!element) return null;
    const parts = getComputedStyle(element).backgroundColor.match(/[\d.]+/g);
    if (!parts) return null;
    const [r, g, b, a = 1] = parts.map(Number);
    return a < 0.1 ? null : [r, g, b];
  }};
  const apply = () => {{
    let style = document.getElementById(id);
    const [r, g, b] = background(document.body) ?? background(document.documentElement) ?? [255, 255, 255];
    const light = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255 > 0.5;
    if (strength === null || !light) {{
      style?.remove();
      return;
    }}
    if (!style) {{
      style = document.createElement("style");
      style.id = id;
      document.documentElement.appendChild(style);
    }}
    style.textContent = `html {{ filter: invert(${{strength}}) hue-rotate(180deg) brightness(1.1) !important; background-color: rgb(${{r}}, ${{g}}, ${{b}}) !important; }}`;
  }};
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", apply, {{ once: true }});
  else apply();
}})();"#
    )
}

fn invert(webview: &Webview, strength: Option<f64>) {
    if let Err(e) = webview.eval(inversion_script(strength)) {
        error!("browser pane: could not apply dark-mode inversion: {e}");
    }
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

pub(crate) fn pane(window: &Window) -> Result<Webview, String> {
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
            move |webview, payload| {
                let url = payload.url().to_string();
                let event = match payload.event() {
                    PageLoadEvent::Started => PageEvent::Loading { url },
                    PageLoadEvent::Finished => PageEvent::Loaded { url },
                };
                emit(&app, &owner, event);
                // Started is the new page committed, so the script lands in it
                // and judges it as soon as its markup is in. Finished judges
                // it again with every stylesheet applied.
                invert(&webview, app.state::<Inversions>().get(&owner));
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

/// How strongly the calling window's pane inverts light pages; `None` stops.
/// Held for pages yet to load, and applied to the one in view.
#[tauri::command]
pub async fn browser_pane_set_inversion(
    window: Window,
    strength: Option<f64>,
) -> Result<(), String> {
    window
        .app_handle()
        .state::<Inversions>()
        .set(window.label(), strength);
    if let Ok(webview) = pane(&window) {
        invert(&webview, strength);
    }
    Ok(())
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
    fn the_inversion_script_carries_the_strength_or_stops() {
        assert!(inversion_script(Some(0.9)).contains("const strength = 0.9;"));
        assert!(inversion_script(None).contains("const strength = null;"));
    }

    #[test]
    fn inversion_is_held_per_window() {
        let inversions = Inversions::default();
        inversions.set("main", Some(0.9));
        assert_eq!(inversions.get("main"), Some(0.9));
        assert_eq!(inversions.get("document"), None);
        inversions.set("main", None);
        assert_eq!(inversions.get("main"), None);
    }

    #[test]
    fn each_window_has_its_own_pane() {
        assert_eq!(label("main"), "browser-pane-main");
        assert_ne!(label("main"), label("document"));
    }
}
