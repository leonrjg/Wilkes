//! PROTOTYPE: a native child webview docked over a placeholder in the page.
//!
//! The page reports the placeholder's rectangle and this module keeps one child
//! webview of the main window at that rectangle. The webview is a native view
//! above the page's own, so nothing the page draws -- menus, modals, toasts --
//! can appear over it. That is what this prototype is here to show.

use serde::Deserialize;
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, Url, WebviewBuilder, WebviewUrl};

const LABEL: &str = "browser-pane";

/// The placeholder's rectangle in CSS pixels, relative to the main webview.
#[derive(Debug, Deserialize)]
pub struct PaneBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[tauri::command]
pub async fn browser_pane_show(
    app: AppHandle,
    url: String,
    bounds: PaneBounds,
) -> Result<(), String> {
    let url = Url::parse(&url).map_err(|e| format!("invalid url: {e}"))?;
    if let Some(webview) = app.get_webview(LABEL) {
        webview.navigate(url).map_err(|e| e.to_string())?;
        return place(&webview, &bounds);
    }
    let window = app
        .get_window("main")
        .ok_or_else(|| "no main window".to_string())?;
    window
        .add_child(
            WebviewBuilder::new(LABEL, WebviewUrl::External(url)),
            LogicalPosition::new(bounds.x, bounds.y),
            LogicalSize::new(bounds.width, bounds.height),
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn browser_pane_set_bounds(app: AppHandle, bounds: PaneBounds) -> Result<(), String> {
    match app.get_webview(LABEL) {
        Some(webview) => place(&webview, &bounds),
        None => Ok(()),
    }
}

#[tauri::command]
pub async fn browser_pane_close(app: AppHandle) -> Result<(), String> {
    match app.get_webview(LABEL) {
        Some(webview) => webview.close().map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

fn place(webview: &tauri::Webview, bounds: &PaneBounds) -> Result<(), String> {
    webview
        .set_position(LogicalPosition::new(bounds.x, bounds.y))
        .map_err(|e| e.to_string())?;
    webview
        .set_size(LogicalSize::new(bounds.width, bounds.height))
        .map_err(|e| e.to_string())
}
