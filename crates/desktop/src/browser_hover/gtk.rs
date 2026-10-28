//! WebKitGTK reports what is under the pointer with `mouse-target-changed`.

use tauri::Webview;
use webkit2gtk::{HitTestResultExt, WebViewExt};

pub fn listen_for_hover(
    webview: &Webview,
    on_hover: impl Fn(Option<String>) + Send + 'static,
) -> tauri::Result<()> {
    webview.with_webview(move |platform| {
        platform
            .inner()
            .connect_mouse_target_changed(move |_, target, _| {
                on_hover(target.link_uri().map(|uri| uri.to_string()));
            });
    })
}
