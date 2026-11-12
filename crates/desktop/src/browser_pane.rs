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
//!
//! A new webview paints itself white until its first page has something to
//! draw, which in dark mode is a white flash for as long as that page takes
//! to arrive. So the pane is made in the app's own background colour and
//! given back a browser's white once its first page is in: that white is also
//! what shows behind a page that sets no background of its own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::webview::{Color, DownloadEvent, NewWindowResponse, PageLoadEvent};
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
pub(crate) enum PageEvent {
    Loading {
        url: String,
    },
    Loaded {
        url: String,
    },
    Title {
        title: String,
    },
    /// A download from the page, being saved into the library root.
    DownloadStarted {
        url: String,
        name: String,
        path: String,
    },
    DownloadFinished {
        url: String,
        name: String,
        path: String,
        success: bool,
    },
    /// A download that was not saved, and why.
    DownloadRefused {
        url: String,
        reason: String,
    },
    /// Cmd/Ctrl+F in the pane: open the find strip.
    FindRequested,
    /// The link the pointer is now over, or none.
    Hover {
        url: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Back,
    Forward,
    Reload,
}

/// The filter each window's pane darkens a light page with, keyed by the
/// owning window's label; a window without an entry does not darken. Kept here
/// rather than in the page because every page load replaces the page.
#[derive(Default)]
pub struct DarkFilters(Mutex<HashMap<String, String>>);

impl DarkFilters {
    fn get(&self, owner: &str) -> Option<String> {
        self.0.lock().unwrap().get(owner).cloned()
    }

    fn set(&self, owner: &str, filter: Option<String>) {
        let mut filters = self.0.lock().unwrap();
        match filter {
            Some(filter) => filters.insert(owner.to_string(), filter),
            None => filters.remove(owner),
        };
    }
}

/// The script that darkens the page in view, or stops darkening it. It judges
/// the page once its markup is in: a page whose own background is dark is
/// already in dark mode. The filter is the one the reader's pages are drawn
/// with (`--pdf-dark-filter`), on the root so it covers the whole canvas; the
/// root's background is pinned to the page's light one, which is what turns
/// dark.
fn dark_filter_script(filter: Option<&str>) -> String {
    // A JSON string is a JavaScript string literal, so the filter reaches the
    // page as data however it is spelled.
    let filter = filter.map_or_else(
        || "null".to_string(),
        |f| serde_json::to_string(f).expect("a string serializes"),
    );
    format!(
        r#"(() => {{
  const filter = {filter};
  const id = "wilkes-dark-filter";
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
    if (filter === null || !light) {{
      style?.remove();
      return;
    }}
    if (!style) {{
      style = document.createElement("style");
      style.id = id;
      document.documentElement.appendChild(style);
    }}
    style.textContent = `html {{ filter: ${{filter}} !important; background-color: rgb(${{r}}, ${{g}}, ${{b}}) !important; }}`;
  }};
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", apply, {{ once: true }});
  else apply();
}})();"#
    )
}

fn darken(webview: &Webview, filter: Option<&str>) {
    if let Err(e) = webview.eval(dark_filter_script(filter)) {
        error!("browser pane: could not apply the dark-mode filter: {e}");
    }
}

/// Give the pane the background a browser paints behind a page: white. It is
/// made with the app's colour instead, which on macOS Tauri gets by switching
/// the webview's background off altogether -- the app shows through -- and
/// has no way to switch back on, so there it is switched back on here.
fn paint_own_background(webview: &Webview) {
    #[cfg(target_os = "macos")]
    let painted = webview.with_webview(|platform| {
        use objc2::{msg_send, runtime::AnyObject};
        use objc2_foundation::{ns_string, NSNumber};
        // SAFETY: `inner` is the pane's live WKWebView, and this runs on the
        // main thread, where `with_webview` calls back.
        unsafe {
            let view = &*platform.inner().cast::<AnyObject>();
            let yes = NSNumber::new_bool(true);
            let _: () = msg_send![view, setValue: &*yes, forKey: ns_string!("drawsBackground")];
        }
    });
    #[cfg(not(target_os = "macos"))]
    let painted = webview.set_background_color(None);
    if let Err(e) = painted {
        error!("browser pane: could not give the page its own background: {e}");
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

/// Where each download in flight is going, by its URL: WebKit says where a
/// download finished on no platform but Windows, so the shell remembers.
#[derive(Default)]
pub struct Downloads(Mutex<HashMap<String, PathBuf>>);

/// `name` in `root`, or `name (2)`, `name (3)`… if it is taken: a download
/// never replaces a file already in the library.
fn free_path(root: &Path, name: &str) -> PathBuf {
    let candidate = root.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map_or_else(|| name.to_string(), |s| s.to_string_lossy().into_owned());
    let extension = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| root.join(format!("{stem} ({n}){extension}")))
        .find(|candidate| !candidate.exists())
        .expect("some numbered name is free")
}

/// The library root a download from the pane is saved into: the active
/// workspace's open root, when the workspace may be written to. A download is
/// the user's own act of putting a file there -- unlike a catalogue fetch,
/// which Wilkes makes and keeps in its uploads directory.
fn download_root(app: &AppHandle) -> Result<PathBuf, String> {
    let ctx = crate::app_context(app);
    ctx.ensure_writable().map_err(|e| e.to_string())?;
    tauri::async_runtime::block_on(ctx.get_settings())
        .last_directory
        .filter(|root| root.is_dir())
        .ok_or_else(|| "No library folder is open to save it in".to_string())
}

/// Send a download from the pane to the library root instead of the system's
/// Downloads folder, under the name WebKit suggests.
pub(crate) fn route_download(app: &AppHandle, owner: &str, event: DownloadEvent<'_>) -> bool {
    match event {
        DownloadEvent::Requested { url, destination } => {
            let url = url.to_string();
            let Some(name) = destination
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
            else {
                emit(
                    app,
                    owner,
                    PageEvent::DownloadRefused {
                        url,
                        reason: "The download has no file name".into(),
                    },
                );
                return false;
            };
            let root = match download_root(app) {
                Ok(root) => root,
                Err(reason) => {
                    emit(app, owner, PageEvent::DownloadRefused { url, reason });
                    return false;
                }
            };
            let path = free_path(&root, &name);
            *destination = path.clone();
            app.state::<Downloads>()
                .0
                .lock()
                .unwrap()
                .insert(url.clone(), path.clone());
            emit(
                app,
                owner,
                PageEvent::DownloadStarted {
                    url,
                    name: file_name(&path),
                    path: path.display().to_string(),
                },
            );
            true
        }
        DownloadEvent::Finished { url, path, success } => {
            let url = url.to_string();
            let remembered = app.state::<Downloads>().0.lock().unwrap().remove(&url);
            let Some(path) = path.or(remembered) else {
                error!("browser pane: a download finished that was never started: {url}");
                return true;
            };
            emit(
                app,
                owner,
                PageEvent::DownloadFinished {
                    url,
                    name: file_name(&path),
                    path: path.display().to_string(),
                    success,
                },
            );
            true
        }
        _ => true,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

pub(crate) fn emit(app: &AppHandle, owner: &str, event: PageEvent) {
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

/// Open `url` in the calling window's pane, creating the pane at `bounds` in
/// `background`, the colour of what is behind it, if the window has none yet.
#[tauri::command]
pub async fn browser_pane_show(
    window: Window,
    url: String,
    bounds: PaneBounds,
    background: Color,
) -> Result<(), String> {
    let url = web_url(&url)?;
    if let Ok(webview) = pane(&window) {
        webview.navigate(url).map_err(|e| e.to_string())?;
        return place(&webview, &bounds);
    }

    let app = window.app_handle().clone();
    let owner = window.label().to_string();
    let label = label(&owner);
    let first_load = AtomicBool::new(true);
    let builder = WebviewBuilder::new(&label, WebviewUrl::External(url))
        .background_color(background)
        .on_page_load({
            let app = app.clone();
            let owner = owner.clone();
            move |webview, payload| {
                let finished = matches!(payload.event(), PageLoadEvent::Finished);
                let url = payload.url().to_string();
                let event = match payload.event() {
                    PageLoadEvent::Started => PageEvent::Loading { url },
                    PageLoadEvent::Finished => PageEvent::Loaded { url },
                };
                emit(&app, &owner, event);
                // Started is the new page committed, so the script lands in it
                // and judges it as soon as its markup is in. Finished judges
                // it again with every stylesheet applied.
                darken(&webview, app.state::<DarkFilters>().get(&owner).as_deref());
                if finished && first_load.swap(false, Ordering::Relaxed) {
                    paint_own_background(&webview);
                }
            }
        })
        .on_document_title_changed({
            let app = app.clone();
            let owner = owner.clone();
            move |_, title| emit(&app, &owner, PageEvent::Title { title })
        })
        .on_download({
            let app = app.clone();
            let owner = owner.clone();
            move |_, event| route_download(&app, &owner, event)
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

    let webview = window
        .add_child(
            builder,
            LogicalPosition::new(bounds.x, bounds.y),
            LogicalSize::new(bounds.width, bounds.height),
        )
        .map_err(|e| e.to_string())?;
    crate::browser_find::listen_for_find_key(&webview, &app, &owner);
    crate::browser_hover::listen_for_hover(&webview, &app, &owner);
    #[cfg(target_os = "macos")]
    crate::browser_webkit_download::listen(&webview, &app, &owner);
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

/// The filter the calling window's pane darkens light pages with; `None`
/// stops. Held for pages yet to load, and applied to the one in view.
#[tauri::command]
pub async fn browser_pane_set_dark_filter(
    window: Window,
    filter: Option<String>,
) -> Result<(), String> {
    if let Ok(webview) = pane(&window) {
        darken(&webview, filter.as_deref());
    }
    window
        .app_handle()
        .state::<DarkFilters>()
        .set(window.label(), filter);
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

/// Clicks a `download` link to the page's own address: the webview then
/// downloads what the address serves through the pane's own session, so a
/// sign-in the page needed carries over, and the download is routed into the
/// library root like any other. Not on macOS, where WebKit shows a page served
/// as an attachment -- a PDF that asks to be downloaded -- in a sandbox that
/// runs no script, so the pane starts the download natively instead.
#[cfg(not(target_os = "macos"))]
const SAVE_PAGE_SCRIPT: &str = r#"(() => {
  const link = document.createElement("a");
  link.href = location.href;
  link.download = "";
  (document.body ?? document.documentElement).appendChild(link);
  link.click();
  link.remove();
})();"#;

/// Save what the calling window's pane is showing into the library root.
#[tauri::command]
pub async fn browser_pane_save_page(window: Window) -> Result<(), String> {
    let webview = pane(&window)?;
    #[cfg(target_os = "macos")]
    {
        crate::browser_webkit_download::save_page(&webview, window.label());
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    webview.eval(SAVE_PAGE_SCRIPT).map_err(|e| e.to_string())
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
        Ok(webview) => {
            crate::browser_find::stop_listening(&webview, window.label());
            crate::browser_hover::stop_listening(&webview, window.label());
            #[cfg(target_os = "macos")]
            crate::browser_webkit_download::stop_listening(&webview, window.label());
            webview.close().map_err(|e| e.to_string())
        }
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
    fn the_dark_filter_script_carries_the_filter_as_a_string_or_stops() {
        assert!(dark_filter_script(Some("brightness(0.5)"))
            .contains(r#"const filter = "brightness(0.5)";"#));
        assert!(dark_filter_script(Some(r#"x"); alert(1); ("#))
            .contains(r#"const filter = "x\"); alert(1); (";"#));
        assert!(dark_filter_script(None).contains("const filter = null;"));
    }

    #[test]
    fn the_dark_filter_is_held_per_window() {
        let filters = DarkFilters::default();
        filters.set("main", Some("brightness(0.5)".to_string()));
        assert_eq!(filters.get("main").as_deref(), Some("brightness(0.5)"));
        assert_eq!(filters.get("document"), None);
        filters.set("main", None);
        assert_eq!(filters.get("main"), None);
    }

    #[test]
    fn a_download_takes_a_free_name_in_the_root() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            free_path(root.path(), "paper.epub"),
            root.path().join("paper.epub")
        );
        std::fs::write(root.path().join("paper.epub"), b"").unwrap();
        assert_eq!(
            free_path(root.path(), "paper.epub"),
            root.path().join("paper (2).epub")
        );
        std::fs::write(root.path().join("paper (2).epub"), b"").unwrap();
        assert_eq!(
            free_path(root.path(), "paper.epub"),
            root.path().join("paper (3).epub")
        );
        std::fs::write(root.path().join("README"), b"").unwrap();
        assert_eq!(
            free_path(root.path(), "README"),
            root.path().join("README (2)")
        );
    }

    #[test]
    fn each_window_has_its_own_pane() {
        assert_eq!(label("main"), "browser-pane-main");
        assert_ne!(label("main"), label("document"));
    }
}
