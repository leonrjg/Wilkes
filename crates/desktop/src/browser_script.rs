//! Running a script in a browser pane's page and getting back what it says,
//! which Tauri's own `eval` does not hand back: each platform's webview is
//! asked directly, as wry itself does for its callbacks.
//!
//! The script must evaluate to a string; anything richer is for the script to
//! encode, so every platform answers with the same thing.

use tauri::Webview;
use tokio::sync::oneshot;

type Answer = oneshot::Sender<Result<String, String>>;

/// Run `script` in the page `webview` shows and return the string it
/// evaluates to.
pub(crate) async fn evaluate(webview: &Webview, script: &str) -> Result<String, String> {
    let (answer, answered) = oneshot::channel();
    #[cfg(target_os = "macos")]
    macos::evaluate(webview, script.to_string(), answer)?;
    #[cfg(target_os = "linux")]
    gtk::evaluate(webview, script.to_string(), answer)?;
    #[cfg(windows)]
    webview2::evaluate(webview, script.to_string(), answer)?;
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = (webview, script, answer);
        return Err("Reading a page is not supported on this platform".to_string());
    }
    answered
        .await
        .map_err(|_| "The page went away before its script answered".to_string())?
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::Cell;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::msg_send;
    use objc2_foundation::NSString;
    use tauri::Webview;

    use super::Answer;

    pub fn evaluate(webview: &Webview, script: String, answer: Answer) -> Result<(), String> {
        webview
            .with_webview(move |platform| {
                // SAFETY: `inner` is the pane's live WKWebView, on the main
                // thread, where `with_webview` calls back.
                let view = unsafe { &*platform.inner().cast::<AnyObject>() };
                // WebKit answers once; the block only has to hand that on.
                let answer = Cell::new(Some(answer));
                let done = RcBlock::new(move |value: *mut AnyObject, error: *mut AnyObject| {
                    // SAFETY: WebKit hands over the script's value or an
                    // NSError, each live for the call.
                    let outcome = match unsafe { error.as_ref() } {
                        Some(error) => {
                            let description: Option<Retained<NSString>> =
                                unsafe { msg_send![error, localizedDescription] };
                            Err(description.map_or_else(
                                || "The page's script failed".to_string(),
                                |description| description.to_string(),
                            ))
                        }
                        None => unsafe { value.as_ref() }
                            .and_then(|value| value.downcast_ref::<NSString>())
                            .map(|text| text.to_string())
                            .ok_or_else(|| "The page's script returned no text".to_string()),
                    };
                    if let Some(answer) = answer.take() {
                        // Nothing is waiting if the caller went meanwhile.
                        answer.send(outcome).ok();
                    }
                });
                // SAFETY: WKWebView's evaluateJavaScript:completionHandler:,
                // on the main thread.
                let _: () = unsafe {
                    msg_send![view, evaluateJavaScript: &*NSString::from_str(&script), completionHandler: &*done]
                };
            })
            .map_err(|e| e.to_string())
    }
}

#[cfg(target_os = "linux")]
mod gtk {
    use javascriptcore::ValueExt;
    use tauri::Webview;
    use webkit2gtk::WebViewExt;

    use super::Answer;

    pub fn evaluate(webview: &Webview, script: String, answer: Answer) -> Result<(), String> {
        webview
            .with_webview(move |platform| {
                // The call that exists on every WebKitGTK the pane runs on;
                // its successor needs 2.40.
                #[allow(deprecated)]
                platform.inner().run_javascript(
                    &script,
                    None::<&webkit2gtk::gio::Cancellable>,
                    move |result| {
                        let outcome = result.map_err(|e| e.to_string()).and_then(|result| {
                            result
                                .js_value()
                                .filter(|value| value.is_string())
                                .map(|value| value.to_str().to_string())
                                .ok_or_else(|| "The page's script returned no text".to_string())
                        });
                        // Nothing is waiting if the caller went meanwhile.
                        answer.send(outcome).ok();
                    },
                );
            })
            .map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
mod webview2 {
    use std::sync::Mutex;

    use tauri::Webview;
    use webview2_com::ExecuteScriptCompletedHandler;
    use windows_core::HSTRING;

    use super::Answer;

    pub fn evaluate(webview: &Webview, script: String, answer: Answer) -> Result<(), String> {
        let answer = std::sync::Arc::new(Mutex::new(Some(answer)));
        let send = {
            let answer = std::sync::Arc::clone(&answer);
            move |outcome: Result<String, String>| {
                if let Some(answer) = answer.lock().unwrap().take() {
                    // Nothing is waiting if the caller went meanwhile.
                    answer.send(outcome).ok();
                }
            }
        };
        webview
            .with_webview(move |platform| {
                let handler_send = send.clone();
                // SAFETY: COM calls on the pane's own live WebView2, on the
                // thread that made it, where `with_webview` calls back.
                let started = unsafe {
                    (|| -> windows_core::Result<()> {
                        let core = platform.controller().CoreWebView2()?;
                        core.ExecuteScript(
                            &HSTRING::from(script.as_str()),
                            &ExecuteScriptCompletedHandler::create(Box::new(
                                move |status, json| {
                                    // WebView2 answers with the value as JSON:
                                    // here, a JSON string.
                                    let outcome = status.map_err(|e| e.to_string()).and_then(|()| {
                                        serde_json::from_str::<String>(&json).map_err(|_| {
                                            "The page's script returned no text".to_string()
                                        })
                                    });
                                    handler_send(outcome);
                                    Ok(())
                                },
                            )),
                        )
                    })()
                };
                if let Err(e) = started {
                    send(Err(e.to_string()));
                }
            })
            .map_err(|e| e.to_string())
    }
}
