//! WebView2 puts the link under the pointer in its status text, which it
//! otherwise shows in a bubble of its own over the page.

use tauri::Webview;
use tracing::error;
use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_12;
use webview2_com::{take_pwstr, StatusBarTextChangedEventHandler};
use windows_core::{Interface, PWSTR};

pub fn listen_for_hover(
    webview: &Webview,
    on_hover: impl Fn(Option<String>) + Send + 'static,
) -> tauri::Result<()> {
    webview.with_webview(move |platform| {
        // SAFETY: COM calls on the pane's own live WebView2, on the thread
        // that made it, where `with_webview` calls back.
        let listening = unsafe {
            (|| -> windows_core::Result<()> {
                let core = platform.controller().CoreWebView2()?;
                core.Settings()?.SetIsStatusBarEnabled(false)?;
                let handler =
                    StatusBarTextChangedEventHandler::create(Box::new(move |sender, _| {
                        let Some(sender) = sender else { return Ok(()) };
                        let mut text = PWSTR::null();
                        sender
                            .cast::<ICoreWebView2_12>()?
                            .StatusBarText(&mut text)?;
                        on_hover(Some(take_pwstr(text)));
                        Ok(())
                    }));
                let mut token = 0;
                core.cast::<ICoreWebView2_12>()?
                    .add_StatusBarTextChanged(&handler, &mut token)
            })()
        };
        if let Err(e) = listening {
            error!("browser pane: WebView2 would not report its status text: {e}");
        }
    })
}
