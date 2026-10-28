//! Downloads the browser pane's context menu starts on macOS -- "Download
//! Linked File", "Download Image" -- into the library like any other.
//!
//! WebKit tells the navigation delegate of those through a private callback,
//! `_webView:contextMenuDidCreateDownload:`, not the two public ones wry
//! implements, so they reach no one: nothing says where to save them, and
//! they go nowhere. The pane gets a stand-in in front of wry's navigation
//! delegate that takes that one callback and hands every other on, and
//! gives such a download a delegate of its own. That delegate decides
//! nothing itself: it asks the pane's download routing, which every other
//! download goes through as well.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, Sel};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSObject, NSString, NSURL};
use tauri::webview::DownloadEvent;
use tauri::{AppHandle, Url, Webview};
use tracing::error;

use crate::browser_pane::route_download;

/// Where a pane's downloads are reported: the window that owns the pane.
struct Route {
    app: AppHandle,
    owner: String,
}

/// The address a download was asked for.
fn url_of(download: &AnyObject) -> Option<Url> {
    // SAFETY: a WKDownload's originalRequest, and that request's URL.
    let request: Option<Retained<AnyObject>> = unsafe { msg_send![download, originalRequest] };
    let url: Option<Retained<NSURL>> = unsafe { msg_send![&*request?, URL] };
    let url = url?.absoluteString()?.to_string();
    match Url::parse(&url) {
        Ok(url) => Some(url),
        Err(e) => {
            error!("browser pane: a context-menu download has no usable address {url}: {e}");
            None
        }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this implements
    // no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WilkesBrowserPaneDownloadDelegate"]
    #[ivars = Route]
    struct DownloadDelegate;

    impl DownloadDelegate {
        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        fn decide_destination(
            &self,
            download: &AnyObject,
            _response: &AnyObject,
            suggested_filename: &NSString,
            completion_handler: &DynBlock<dyn Fn(*const NSURL)>,
        ) {
            let Route { app, owner } = self.ivars();
            let mut destination = PathBuf::from(suggested_filename.to_string());
            let accepted = url_of(download).is_some_and(|url| {
                route_download(
                    app,
                    owner,
                    DownloadEvent::Requested {
                        url,
                        destination: &mut destination,
                    },
                )
            });
            if accepted {
                let path = NSString::from_str(&destination.display().to_string());
                let file = NSURL::fileURLWithPath_isDirectory(&path, false);
                completion_handler.call((Retained::as_ptr(&file),));
            } else {
                completion_handler.call((std::ptr::null(),));
            }
        }

        #[unsafe(method(downloadDidFinish:))]
        fn did_finish(&self, download: &AnyObject) {
            self.finished(download, true);
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        fn did_fail(&self, download: &AnyObject, _error: &AnyObject, _resume_data: *mut AnyObject) {
            self.finished(download, false);
        }
    }
);

impl DownloadDelegate {
    fn finished(&self, download: &AnyObject, success: bool) {
        let Route { app, owner } = self.ivars();
        if let Some(url) = url_of(download) {
            route_download(
                app,
                owner,
                DownloadEvent::Finished {
                    url,
                    path: None,
                    success,
                },
            );
        }
    }
}

struct StandInIvars {
    /// wry's navigation delegate, which answers everything but this.
    original: Option<Retained<AnyObject>>,
    /// Shared by every download the pane's context menu starts. WebKit holds
    /// a download's delegate weakly, so it lives here.
    downloads: Retained<DownloadDelegate>,
}

define_class!(
    // SAFETY: as for `DownloadDelegate`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WilkesBrowserPaneNavigationDelegate"]
    #[ivars = StandInIvars]
    struct NavigationStandIn;

    impl NavigationStandIn {
        #[unsafe(method(_webView:contextMenuDidCreateDownload:))]
        fn context_menu_did_create_download(&self, _web_view: &AnyObject, download: &AnyObject) {
            let delegate = &*self.ivars().downloads;
            // SAFETY: a WKDownload's delegate, which it holds weakly.
            let _: () = unsafe { msg_send![download, setDelegate: delegate] };
        }

        // WebKit asks once, when the delegate is set, which callbacks it
        // answers; the stand-in answers its own and all of wry's.
        #[unsafe(method(respondsToSelector:))]
        fn responds_to_selector(&self, selector: Sel) -> Bool {
            // SAFETY: plain respondsToSelector: calls.
            let own: bool = unsafe { msg_send![super(self), respondsToSelector: selector] };
            let original = self.ivars().original.as_ref().is_some_and(|original| unsafe {
                msg_send![&**original, respondsToSelector: selector]
            });
            Bool::new(own || original)
        }

        #[unsafe(method(forwardingTargetForSelector:))]
        fn forwarding_target(&self, _selector: Sel) -> *mut AnyObject {
            self.ivars()
                .original
                .as_ref()
                .map_or(std::ptr::null_mut(), |original| Retained::as_ptr(original).cast_mut())
        }
    }
);

thread_local! {
    /// Each pane's stand-in, by the window that owns the pane: WebKit holds
    /// its navigation delegate weakly. Main thread only.
    static STAND_INS: RefCell<HashMap<String, Retained<NavigationStandIn>>> = RefCell::new(HashMap::new());
}

/// Route a pane's context-menu downloads into the library from now on.
pub(crate) fn listen(webview: &Webview, app: &AppHandle, owner: &str) {
    let route = Route {
        app: app.clone(),
        owner: owner.to_string(),
    };
    let listening = webview.with_webview(move |platform| {
        let mtm = MainThreadMarker::new().expect("with_webview calls back on the main thread");
        // SAFETY: `inner` is the pane's live WKWebView, on the main thread.
        let view = unsafe { &*platform.inner().cast::<AnyObject>() };
        let owner = route.owner.clone();
        let downloads = DownloadDelegate::alloc(mtm).set_ivars(route);
        // SAFETY: NSObject's init, on freshly allocated objects.
        let downloads: Retained<DownloadDelegate> = unsafe { msg_send![super(downloads), init] };
        let original: Option<Retained<AnyObject>> = unsafe { msg_send![view, navigationDelegate] };
        let stand_in = NavigationStandIn::alloc(mtm).set_ivars(StandInIvars {
            original,
            downloads,
        });
        let stand_in: Retained<NavigationStandIn> = unsafe { msg_send![super(stand_in), init] };
        let _: () = unsafe { msg_send![view, setNavigationDelegate: &*stand_in] };
        STAND_INS.with_borrow_mut(|stand_ins| stand_ins.insert(owner, stand_in));
    });
    if let Err(e) = listening {
        error!("browser pane: could not take its context menu's downloads: {e}");
    }
}

/// Give a pane about to close back wry's navigation delegate.
pub(crate) fn stop_listening(webview: &Webview, owner: &str) {
    let owner = owner.to_string();
    let stopped = webview.with_webview(move |platform| {
        if let Some(stand_in) = STAND_INS.with_borrow_mut(|stand_ins| stand_ins.remove(&owner)) {
            // SAFETY: as in `listen`.
            let view = unsafe { &*platform.inner().cast::<AnyObject>() };
            let original = stand_in.ivars().original.as_deref();
            let _: () = unsafe { msg_send![view, setNavigationDelegate: original] };
        }
    });
    if let Err(e) = stopped {
        error!("browser pane: could not give back its navigation delegate: {e}");
    }
}
