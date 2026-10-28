//! Finding with WebKitGTK's find controller, which highlights every match and
//! counts them, up to a cap.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gdk::{keys::constants as key, ModifierType};
use gtk::glib::{ObjectExt, Propagation};
use gtk::prelude::WidgetExt;
use tauri::Webview;
use webkit2gtk::{FindControllerExt, FindOptions, WebViewExt};

use super::Found;

/// How many matches are counted; past it WebKitGTK reports `u32::MAX`, and
/// the page has more than this.
const MAX_COUNTED: u32 = 1000;

/// Call `on_find` for Ctrl+F in the pane, instead of handing it to the page.
pub fn listen_for_find_key(
    webview: &Webview,
    on_find: impl Fn() + Send + 'static,
) -> tauri::Result<()> {
    webview.with_webview(move |platform| {
        platform.inner().connect_key_press_event(move |_, event| {
            let control = event.state().contains(ModifierType::CONTROL_MASK);
            if control && event.keyval().to_lower() == key::f {
                on_find();
                Propagation::Stop
            } else {
                Propagation::Proceed
            }
        });
    })
}

pub async fn find(webview: &Webview, query: String, backwards: bool) -> Result<Found, String> {
    let (answer, answered) = tokio::sync::oneshot::channel();
    webview
        .with_webview(move |platform| {
            let Some(finder) = platform.inner().find_controller() else {
                answer
                    .send(Err("The pane's page cannot be searched".to_string()))
                    .ok();
                return;
            };
            // The controller answers each search with one of two signals; the
            // first to arrive answers this find, and both are let go.
            let answer = Rc::new(Cell::new(Some(answer)));
            let listening = Rc::new(RefCell::new(Vec::new()));
            let settle = {
                let answer = answer.clone();
                let listening = listening.clone();
                move |finder: &webkit2gtk::FindController, found: Found| {
                    for handler in listening.borrow_mut().drain(..) {
                        finder.disconnect(handler);
                    }
                    if let Some(answer) = answer.take() {
                        // Nothing is waiting if the command was dropped meanwhile.
                        answer.send(Ok(found)).ok();
                    }
                }
            };
            let on_found = {
                let settle = settle.clone();
                finder.connect_found_text(move |finder, count| {
                    let matches = (count != u32::MAX).then_some(count);
                    settle(
                        finder,
                        Found {
                            found: true,
                            matches,
                        },
                    );
                })
            };
            let on_failed = finder.connect_failed_to_find_text(move |finder| {
                settle(
                    finder,
                    Found {
                        found: false,
                        matches: Some(0),
                    },
                );
            });
            listening.borrow_mut().extend([on_found, on_failed]);

            if finder.search_text().as_deref() == Some(query.as_str()) {
                if backwards {
                    finder.search_previous();
                } else {
                    finder.search_next();
                }
            } else {
                let mut options = FindOptions::CASE_INSENSITIVE | FindOptions::WRAP_AROUND;
                if backwards {
                    options |= FindOptions::BACKWARDS;
                }
                finder.search(&query, options.bits(), MAX_COUNTED);
            }
        })
        .map_err(|e| e.to_string())?;
    answered
        .await
        .map_err(|_| "The pane closed before its page was searched".to_string())?
}

pub fn end_find(webview: &Webview) -> tauri::Result<()> {
    webview.with_webview(|platform| {
        if let Some(finder) = platform.inner().find_controller() {
            finder.search_finish();
        }
    })
}
