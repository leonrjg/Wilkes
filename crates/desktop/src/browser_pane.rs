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
//! A window may also have the agent's pane: the browser MCP tools fetch and
//! read pages through (`agent_browser`). It lives in the main window, made
//! hidden the first time an agent needs it, and is shown only when the user
//! opens it, to watch it or to take over -- sign in, pass a check. It opens
//! the same data store, so a sign-in made in either pane is the other's too.
//!
//! Every download from a pane is staged beside the library and stored by the
//! rules every download keeps (`wilkes_core::acquire::store_file_in_root`):
//! no file is overwritten, a taken name is numbered, and content the library
//! already has is not stored twice.
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

use std::collections::{HashMap, HashSet};
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
/// The event the main window hears about the page in the agent's pane.
pub const AGENT_PAGE_EVENT: &str = "browser-agent:page";
/// The window the agent's pane lives in.
pub(crate) const AGENT_OWNER: &str = "main";

/// The data store every pane opens; any fixed value would do, but it must not
/// change, or every sign-in made in the pane is forgotten.
#[cfg(target_os = "macos")]
const DATA_STORE: [u8; 16] = [
    0x32, 0x79, 0x85, 0x6d, 0x56, 0x0b, 0x4e, 0xa1, 0x94, 0x7c, 0xe6, 0x26, 0x29, 0x52, 0xb9, 0x9d,
];

/// Whose pane: the user's own, or the one the agent browses in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneKind {
    User,
    Agent,
}

/// One pane: the window it is in, and whose it is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PaneId {
    pub(crate) owner: String,
    pub(crate) kind: PaneKind,
}

impl PaneId {
    /// The pane of `kind` in `window`. The agent's is in the main window only.
    pub(crate) fn of(window: &Window, kind: PaneKind) -> Result<Self, String> {
        if kind == PaneKind::Agent && window.label() != AGENT_OWNER {
            return Err("The agent browser is in the main window".to_string());
        }
        Ok(Self {
            owner: window.label().to_string(),
            kind,
        })
    }

    pub(crate) fn agent() -> Self {
        Self {
            owner: AGENT_OWNER.to_string(),
            kind: PaneKind::Agent,
        }
    }

    pub(crate) fn label(&self) -> String {
        match self.kind {
            PaneKind::User => format!("browser-pane-{}", self.owner),
            PaneKind::Agent => format!("browser-agent-{}", self.owner),
        }
    }

    fn event(&self) -> &'static str {
        match self.kind {
            PaneKind::User => PAGE_EVENT,
            PaneKind::Agent => AGENT_PAGE_EVENT,
        }
    }
}

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
    /// Whether an agent is using the pane: fetching or reading through it.
    /// The agent's pane only.
    Busy {
        busy: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Back,
    Forward,
    Reload,
}

/// The filter each pane darkens a light page with, keyed by the pane's label;
/// a pane without an entry does not darken. Kept here rather than in the page
/// because every page load replaces the page.
#[derive(Default)]
pub struct DarkFilters(Mutex<HashMap<String, String>>);

impl DarkFilters {
    fn get(&self, pane: &PaneId) -> Option<String> {
        self.0.lock().unwrap().get(&pane.label()).cloned()
    }

    fn set(&self, pane: &PaneId, filter: Option<String>) {
        let mut filters = self.0.lock().unwrap();
        match filter {
            Some(filter) => filters.insert(pane.label(), filter),
            None => filters.remove(&pane.label()),
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

/// Only web pages: the pane is not a way into the filesystem or the app.
pub(crate) fn web_url(url: &str) -> Result<Url, String> {
    let url = Url::parse(url).map_err(|e| format!("Not a web address: {e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        scheme => Err(format!(
            "The browser pane does not open {scheme}: addresses"
        )),
    }
}

pub(crate) fn pane(window: &Window, kind: PaneKind) -> Result<Webview, String> {
    let id = PaneId::of(window, kind)?;
    window
        .app_handle()
        .get_webview(&id.label())
        .ok_or_else(|| "The browser pane is not open".to_string())
}

/// Where each download in flight is staged, by its pane's label and its URL:
/// WebKit says where a download finished on no platform but Windows, so the
/// shell remembers.
#[derive(Default)]
pub struct Downloads(Mutex<HashMap<(String, String), PathBuf>>);

/// The addresses each pane was asked to save to the library, until their
/// download finishes. A web page so saved becomes its article, with its
/// pictures inside, as a feed's post does; any other file goes into the
/// library as it came.
#[derive(Default)]
pub struct PageSaves(Mutex<HashSet<(String, String)>>);

/// Whether a downloaded file is a web page, by its own first bytes: WebKit's
/// suggested name for a page need not end in `.html`.
pub(crate) fn is_web_page(path: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    let mut head = Vec::with_capacity(1024);
    std::fs::File::open(path)?
        .take(1024)
        .read_to_end(&mut head)?;
    let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
    let head = head.trim_start_matches('\u{feff}').trim_start();
    // Markup from its first character: a doctype, a comment, the root element.
    Ok(head.starts_with('<') && (head.starts_with("<!doctype html") || head.contains("<html")))
}

/// A directory of its own beside the library for one download to arrive in.
pub(crate) fn staging_dir(app: &AppHandle, view: &str) -> Result<PathBuf, String> {
    let staging = crate::view_context(app, view)
        .data_dir
        .join("page-staging")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&staging).map_err(|e| format!("Could not stage it: {e}"))?;
    Ok(staging)
}

/// Remove a download's staging directory, once what it held is dealt with.
pub(crate) fn clear_staging(staged: &Path) {
    let Some(staging) = staged.parent() else {
        return;
    };
    if let Err(e) = std::fs::remove_dir_all(staging) {
        if e.kind() != std::io::ErrorKind::NotFound {
            error!("browser pane: could not clear {}: {e}", staging.display());
        }
    }
}

/// Put a pane's finished download where it belongs, and say so.
async fn finish_download(
    app: AppHandle,
    pane: PaneId,
    url: String,
    staged: PathBuf,
    success: bool,
    saving_page: bool,
) {
    if !success {
        clear_staging(&staged);
        emit(
            &app,
            &pane,
            PageEvent::DownloadFinished {
                url,
                name: file_name(&staged),
                path: String::new(),
                success: false,
            },
        );
        return;
    }
    let outcome: Result<PathBuf, String> = async {
        let ctx = crate::view_context(&app, &pane.owner);
        ctx.ensure_writable().map_err(|e| e.to_string())?;
        let root = ctx
            .get_settings()
            .await
            .last_directory
            .filter(|root| root.is_dir())
            .ok_or_else(|| "No library folder is open to save it in".to_string())?;
        if saving_page && is_web_page(&staged).map_err(|e| e.to_string())? {
            return ctx
                .save_web_page(&url, &staged, &root)
                .await
                .map_err(|e| format!("Its article could not be saved: {e:#}"));
        }
        let name = file_name(&staged);
        let (file, source) = (staged.clone(), url.clone());
        tauri::async_runtime::spawn_blocking(move || {
            wilkes_core::acquire::store_file_in_root(&root, &file, &name, None, &source)
        })
        .await
        .map_err(|e| e.to_string())?
        .map(|saved| PathBuf::from(saved.path))
    }
    .await;
    clear_staging(&staged);
    match outcome {
        Ok(path) => emit(
            &app,
            &pane,
            PageEvent::DownloadFinished {
                url,
                name: file_name(&path),
                path: path.display().to_string(),
                success: true,
            },
        ),
        Err(reason) => {
            error!("browser pane: saving a download: {reason}");
            emit(&app, &pane, PageEvent::DownloadRefused { url, reason });
        }
    }
}

/// The library root a download from the pane is saved into: the open root of
/// the workspace the pane's window shows, when the workspace may be written
/// to. A download is the user's own act of putting a file there -- unlike a
/// catalogue fetch, which Wilkes makes and keeps in its uploads directory.
fn download_root(app: &AppHandle, view: &str) -> Result<PathBuf, String> {
    let ctx = crate::view_context(app, view);
    ctx.ensure_writable().map_err(|e| e.to_string())?;
    tauri::async_runtime::block_on(ctx.get_settings())
        .last_directory
        .filter(|root| root.is_dir())
        .ok_or_else(|| "No library folder is open to save it in".to_string())
}

/// Stage a download from a pane, then send it to the library root -- or, in
/// the agent's pane while an agent is fetching, to the agent.
pub(crate) fn route_download(app: &AppHandle, pane: &PaneId, event: DownloadEvent<'_>) -> bool {
    match event {
        DownloadEvent::Requested { url, destination } => {
            let url = url.to_string();
            let key = (pane.label(), url.clone());
            let refuse = |reason: String| {
                app.state::<PageSaves>().0.lock().unwrap().remove(&key);
                emit(app, pane, PageEvent::DownloadRefused { url: url.clone(), reason });
                false
            };
            let Some(name) = destination
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
            else {
                return refuse("The download has no file name".into());
            };
            let agents = pane.kind == PaneKind::Agent && crate::agent_browser::claim(app, &url);
            // The agent stores what it fetched in the library its call names,
            // so only a download for the open library needs one open.
            if !agents {
                if let Err(reason) = download_root(app, &pane.owner) {
                    return refuse(reason);
                }
            }
            let path = match staging_dir(app, &pane.owner) {
                Ok(staging) => staging.join(&name),
                Err(reason) => return refuse(reason),
            };
            *destination = path.clone();
            app.state::<Downloads>().0.lock().unwrap().insert(key, path);
            if !agents {
                emit(app, pane, PageEvent::DownloadStarted { url, name });
            }
            true
        }
        DownloadEvent::Finished { url, path, success } => {
            let url = url.to_string();
            let key = (pane.label(), url.clone());
            let remembered = app.state::<Downloads>().0.lock().unwrap().remove(&key);
            let Some(path) = path.or(remembered) else {
                error!("browser pane: a download finished that was never started");
                return true;
            };
            if pane.kind == PaneKind::Agent
                && crate::agent_browser::finished(app, &url, &path, success)
            {
                return true;
            }
            let saving_page = app.state::<PageSaves>().0.lock().unwrap().remove(&key);
            tauri::async_runtime::spawn(finish_download(
                app.clone(),
                pane.clone(),
                url,
                path,
                success,
                saving_page,
            ));
            true
        }
        _ => true,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

pub(crate) fn emit(app: &AppHandle, pane: &PaneId, event: PageEvent) {
    if let Err(e) = app.emit_to(pane.owner.as_str(), pane.event(), event) {
        error!(
            "browser pane: could not tell {} about its page: {e}",
            pane.owner
        );
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

/// The size the agent's pane lays pages out at while nobody watches: a
/// browser window's, so a page shows the layout it would show the user.
const UNSEEN_SIZE: LogicalSize<f64> = LogicalSize::new(1280.0, 900.0);

/// Make `pane` in `window`, showing `url`, at `bounds` -- or, without them,
/// hidden at a browser window's size, for the agent to use unwatched.
/// `background` is the colour of what is behind it until its first page is in.
pub(crate) fn create_pane(
    window: &Window,
    pane: &PaneId,
    url: Url,
    bounds: Option<&PaneBounds>,
    background: Color,
) -> Result<Webview, String> {
    let app = window.app_handle().clone();
    let label = pane.label();
    let first_load = AtomicBool::new(true);
    let builder = WebviewBuilder::new(&label, WebviewUrl::External(url))
        .background_color(background)
        .on_page_load({
            let app = app.clone();
            let pane = pane.clone();
            move |webview, payload| {
                let finished = matches!(payload.event(), PageLoadEvent::Finished);
                let url = payload.url().to_string();
                let event = match payload.event() {
                    PageLoadEvent::Started => PageEvent::Loading { url },
                    PageLoadEvent::Finished => PageEvent::Loaded { url },
                };
                emit(&app, &pane, event);
                // Started is the new page committed, so the script lands in it
                // and judges it as soon as its markup is in. Finished judges
                // it again with every stylesheet applied.
                darken(&webview, app.state::<DarkFilters>().get(&pane).as_deref());
                if finished && first_load.swap(false, Ordering::Relaxed) {
                    paint_own_background(&webview);
                }
                if finished && pane.kind == PaneKind::Agent {
                    crate::agent_browser::page_finished(&app, payload.url());
                }
            }
        })
        .on_document_title_changed({
            let app = app.clone();
            let pane = pane.clone();
            move |_, title| emit(&app, &pane, PageEvent::Title { title })
        })
        .on_download({
            let app = app.clone();
            let pane = pane.clone();
            move |_, event| route_download(&app, &pane, event)
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
    let builder = builder.data_directory(
        crate::view_context(&app, window.label())
            .shared_data_dir
            .join("browser"),
    );

    let (position, size) = match bounds {
        Some(bounds) => (
            LogicalPosition::new(bounds.x, bounds.y),
            LogicalSize::new(bounds.width, bounds.height),
        ),
        None => (LogicalPosition::new(0.0, 0.0), UNSEEN_SIZE),
    };
    let webview = window
        .add_child(builder, position, size)
        .map_err(|e| e.to_string())?;
    if bounds.is_none() {
        webview.hide().map_err(|e| e.to_string())?;
    }
    crate::browser_find::listen_for_find_key(&webview, &app, pane);
    crate::browser_hover::listen_for_hover(&webview, &app, pane);
    // On macOS the navigation stand-in below hears each page's status.
    #[cfg(not(target_os = "macos"))]
    if pane.kind == PaneKind::Agent {
        crate::browser_status::listen(&webview, &app);
    }
    #[cfg(target_os = "macos")]
    crate::browser_webkit_download::listen(&webview, &app, pane);
    Ok(webview)
}

/// Show the calling window's pane of `kind` at `bounds`, at `url` if one is
/// given, creating it for that `url` in `background`, the colour of what is
/// behind it, if the window has none yet. Without a `url` only a pane that
/// exists is shown: the agent's, made hidden by an agent, which the user has
/// opened to watch.
#[tauri::command]
pub async fn browser_pane_show(
    window: Window,
    kind: PaneKind,
    url: Option<String>,
    bounds: PaneBounds,
    background: Color,
) -> Result<(), String> {
    let url = url.as_deref().map(web_url).transpose()?;
    if let (PaneKind::Agent, Some(url)) = (kind, &url) {
        crate::agent_browser::opening(window.app_handle(), url);
    }
    if let Ok(webview) = pane(&window, kind) {
        if let Some(url) = url {
            webview.navigate(url).map_err(|e| e.to_string())?;
        }
        place(&webview, &bounds)?;
        return webview.show().map_err(|e| e.to_string());
    }
    let Some(url) = url else {
        return Ok(());
    };
    create_pane(
        &window,
        &PaneId::of(&window, kind)?,
        url,
        Some(&bounds),
        background,
    )
    .map(|_| ())
}

/// Follow the placeholder. A pane that is not open has nothing to move: the
/// page reports its layout until it hears the pane is gone.
#[tauri::command]
pub async fn browser_pane_set_bounds(
    window: Window,
    kind: PaneKind,
    bounds: PaneBounds,
) -> Result<(), String> {
    match pane(&window, kind) {
        Ok(webview) => place(&webview, &bounds),
        Err(_) => Ok(()),
    }
}

/// The filter the calling window's pane darkens light pages with; `None`
/// stops. Held for pages yet to load, and applied to the one in view.
#[tauri::command]
pub async fn browser_pane_set_dark_filter(
    window: Window,
    kind: PaneKind,
    filter: Option<String>,
) -> Result<(), String> {
    let id = PaneId::of(&window, kind)?;
    if let Ok(webview) = pane(&window, kind) {
        darken(&webview, filter.as_deref());
    }
    window.app_handle().state::<DarkFilters>().set(&id, filter);
    Ok(())
}

/// Step aside while the page needs the pointer: a native view takes every
/// mouse event over it, so a drag in the page that crosses the pane would lose
/// its moves and its release.
#[tauri::command]
pub async fn browser_pane_set_visible(
    window: Window,
    kind: PaneKind,
    visible: bool,
) -> Result<(), String> {
    let webview = pane(&window, kind)?;
    if visible {
        webview.show()
    } else {
        webview.hide()
    }
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn browser_pane_step(window: Window, kind: PaneKind, step: Step) -> Result<(), String> {
    let webview = pane(&window, kind)?;
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
pub(crate) const SAVE_PAGE_SCRIPT: &str = r#"(() => {
  const link = document.createElement("a");
  link.href = location.href;
  link.download = "";
  (document.body ?? document.documentElement).appendChild(link);
  link.click();
  link.remove();
})();"#;

/// Save what the calling window's pane is showing into the library root.
#[tauri::command]
pub async fn browser_pane_save_page(window: Window, kind: PaneKind) -> Result<(), String> {
    let id = PaneId::of(&window, kind)?;
    let webview = pane(&window, kind)?;
    let url = webview.url().map_err(|e| e.to_string())?.to_string();
    window
        .app_handle()
        .state::<PageSaves>()
        .0
        .lock()
        .unwrap()
        .insert((id.label(), url));
    #[cfg(target_os = "macos")]
    {
        crate::browser_webkit_download::save_page(&webview, &id);
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    webview.eval(SAVE_PAGE_SCRIPT).map_err(|e| e.to_string())
}

/// Forget every cookie and site record the panes hold, then reload, so the
/// page in view shows what a signed-out visit sees.
#[tauri::command]
pub async fn browser_pane_clear_data(window: Window, kind: PaneKind) -> Result<(), String> {
    let webview = pane(&window, kind)?;
    webview
        .clear_all_browsing_data()
        .map_err(|e| e.to_string())?;
    webview.reload().map_err(|e| e.to_string())
}

/// Close the calling window's pane of `kind`. The agent's is only hidden: an
/// agent may be using it, and the user closing it is the user looking away.
#[tauri::command]
pub async fn browser_pane_close(window: Window, kind: PaneKind) -> Result<(), String> {
    let id = PaneId::of(&window, kind)?;
    let Ok(webview) = pane(&window, kind) else {
        return Ok(());
    };
    if kind == PaneKind::Agent {
        return webview.hide().map_err(|e| e.to_string());
    }
    crate::browser_find::stop_listening(&webview, &id);
    crate::browser_hover::stop_listening(&webview, &id);
    #[cfg(target_os = "macos")]
    crate::browser_webkit_download::stop_listening(&webview, &id);
    webview.close().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_page_is_told_from_another_file_by_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        // WebKit's name for a page need not say it is one.
        assert!(is_web_page(&write(
            "mysterious-beeps",
            b"\xef\xbb\xbf\n<!DOCTYPE html>\n<html lang=\"en\">"
        ))
        .unwrap());
        assert!(is_web_page(&write(
            "a.html",
            b"<!-- generated -->\n<html><body></body></html>"
        ))
        .unwrap());
        assert!(!is_web_page(&write("paper.pdf", b"%PDF-1.7\n%\xe2\xe3")).unwrap());
        assert!(!is_web_page(&write("notes.txt", b"Some notes about <html> tags")).unwrap());
    }

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

    fn user(owner: &str) -> PaneId {
        PaneId {
            owner: owner.to_string(),
            kind: PaneKind::User,
        }
    }

    #[test]
    fn the_dark_filter_is_held_per_pane() {
        let filters = DarkFilters::default();
        filters.set(&user("main"), Some("brightness(0.5)".to_string()));
        assert_eq!(
            filters.get(&user("main")).as_deref(),
            Some("brightness(0.5)")
        );
        assert_eq!(filters.get(&user("document")), None);
        assert_eq!(filters.get(&PaneId::agent()), None);
        filters.set(&user("main"), None);
        assert_eq!(filters.get(&user("main")), None);
    }

    #[test]
    fn each_window_has_its_own_pane_and_the_agent_its_own() {
        assert_eq!(user("main").label(), "browser-pane-main");
        assert_ne!(user("main").label(), user("document").label());
        assert_eq!(PaneId::agent().label(), "browser-agent-main");
        assert_eq!(PaneId::agent().event(), AGENT_PAGE_EVENT);
        assert_eq!(user("main").event(), PAGE_EVENT);
    }
}
