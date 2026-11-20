//! Downloads in the browser pane on macOS that wry leaves unrouted, sent into
//! the library like any other.
//!
//! The context menu's -- "Download Linked File", "Download Image" -- WebKit
//! reports to the navigation delegate through a private callback,
//! `_webView:contextMenuDidCreateDownload:`, not the two public ones wry
//! implements, so they reach no one: nothing says where to save them, and
//! they go nowhere. The pane gets a stand-in in front of wry's navigation
//! delegate that takes that one callback and hands every other on, and
//! gives such a download a delegate of its own.
//!
//! Save to library starts its download here too, natively, rather than by a
//! script in the page: a page served as an attachment -- a PDF from the ACM
//! Digital Library -- is shown in a sandbox that runs no script at all.
//!
//! The downloads' delegate decides nothing itself: it asks the pane's
//! download routing, which every other download goes through as well.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use block2::{Block, DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, Sel};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSObject, NSString, NSURLRequest, NSURL};
use tauri::webview::DownloadEvent;
use tauri::{AppHandle, Url, Webview};
use tracing::error;

use crate::browser_pane::{route_download, PaneId};

/// Where a pane's downloads are reported: the pane they came from.
struct Route {
    app: AppHandle,
    pane: PaneId,
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
            let Route { app, pane } = self.ivars();
            let mut destination = PathBuf::from(suggested_filename.to_string());
            let accepted = url_of(download).is_some_and(|url| {
                route_download(
                    app,
                    pane,
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
        let Route { app, pane } = self.ivars();
        if let Some(url) = url_of(download) {
            route_download(
                app,
                pane,
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
    /// Shared by every download started here. WebKit holds a download's
    /// delegate weakly, so it lives here.
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
        // Every page's response passes here before wry decides on it: the
        // status of the agent's documents is noted, and the decision is wry's.
        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_policy_for_response(
            &self,
            web_view: &AnyObject,
            response: &AnyObject,
            handler: &Block<dyn Fn(isize)>,
        ) {
            // SAFETY: a WKNavigationResponse, whose response is an
            // NSURLResponse, an NSHTTPURLResponse for a page fetched by HTTP.
            let main_frame: bool = unsafe { msg_send![response, isForMainFrame] };
            if main_frame {
                let answer: Option<Retained<AnyObject>> = unsafe { msg_send![response, response] };
                let status = answer.as_ref().and_then(|answer| {
                    let http: bool =
                        unsafe { msg_send![&**answer, respondsToSelector: objc2::sel!(statusCode)] };
                    http.then(|| {
                        let code: isize = unsafe { msg_send![&**answer, statusCode] };
                        u16::try_from(code).ok()
                    })
                    .flatten()
                });
                // The name the site gave the document, or one made from its
                // address and type: what the agent saves it as.
                let name = answer.as_ref().and_then(|answer| {
                    let name: Option<Retained<NSString>> =
                        unsafe { msg_send![&**answer, suggestedFilename] };
                    name.map(|name| name.to_string())
                });
                let mime_type = answer.as_ref().and_then(|answer| {
                    let mime: Option<Retained<NSString>> =
                        unsafe { msg_send![&**answer, MIMEType] };
                    mime.map(|mime| mime.to_string())
                });
                let Route { app, pane } = self.ivars().downloads.ivars();
                crate::agent_browser::response_arrived(
                    app,
                    pane,
                    crate::agent_browser::Response {
                        status,
                        suggested_name: name,
                        mime_type,
                    },
                );
            }
            let original = self.ivars().original.as_ref().filter(|original| unsafe {
                msg_send![&***original, respondsToSelector: objc2::sel!(webView:decidePolicyForNavigationResponse:decisionHandler:)]
            });
            match original {
                // SAFETY: wry's own answer to the same call, with what WebKit gave.
                Some(original) => unsafe {
                    let _: () = msg_send![
                        &**original,
                        webView: web_view,
                        decidePolicyForNavigationResponse: response,
                        decisionHandler: handler
                    ];
                },
                // WKNavigationResponsePolicyAllow, what WebKit does with no
                // delegate to ask.
                None => handler.call((1,)),
            }
        }

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
    /// Each pane's stand-in, by the pane's label: WebKit holds
    /// its navigation delegate weakly. Main thread only.
    static STAND_INS: RefCell<HashMap<String, Retained<NavigationStandIn>>> = RefCell::new(HashMap::new());
}

/// Route a pane's context-menu downloads, and what it is asked to save, into
/// the library from now on.
pub(crate) fn listen(webview: &Webview, app: &AppHandle, pane: &PaneId) {
    let route = Route {
        app: app.clone(),
        pane: pane.clone(),
    };
    let listening = webview.with_webview(move |platform| {
        let mtm = MainThreadMarker::new().expect("with_webview calls back on the main thread");
        // SAFETY: `inner` is the pane's live WKWebView, on the main thread.
        let view = unsafe { &*platform.inner().cast::<AnyObject>() };
        let label = route.pane.label();
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
        STAND_INS.with_borrow_mut(|stand_ins| stand_ins.insert(label, stand_in));
    });
    if let Err(e) = listening {
        error!("browser pane: could not take its context menu's downloads: {e}");
    }
}

/// Download what the pane is showing -- the PDF in its viewer, the page's
/// HTML -- from its own address, through the pane's own session, so a
/// sign-in or a bot check the page needed carries over.
pub(crate) fn save_page(webview: &Webview, pane: &PaneId) {
    let label = pane.label();
    let saving = webview.with_webview(move |platform| {
        let Some(downloads) = STAND_INS.with_borrow(|stand_ins| {
            stand_ins
                .get(&label)
                .map(|stand_in| stand_in.ivars().downloads.clone())
        }) else {
            error!("browser pane: {label} has no download delegate to save with");
            return;
        };
        // SAFETY: `inner` is the pane's live WKWebView, on the main thread.
        let view = unsafe { &*platform.inner().cast::<AnyObject>() };
        let url: Option<Retained<NSURL>> = unsafe { msg_send![view, URL] };
        let Some(url) = url else {
            error!("browser pane: {label} has no page to save");
            return;
        };
        let request = NSURLRequest::requestWithURL(&url);
        let started = RcBlock::new(move |download: *mut AnyObject| {
            // SAFETY: WebKit hands over a live WKDownload, whose delegate it
            // holds weakly; `downloads` is kept by the stand-in.
            match unsafe { download.as_ref() } {
                Some(download) => {
                    let _: () = unsafe { msg_send![download, setDelegate: &*downloads] };
                }
                None => error!("browser pane: WebKit started no download to save the page"),
            }
        });
        // SAFETY: WKWebView's startDownloadUsingRequest:completionHandler:
        // (macOS 11.3), on the main thread.
        let _: () = unsafe {
            msg_send![view, startDownloadUsingRequest: &*request, completionHandler: &*started]
        };
    });
    if let Err(e) = saving {
        error!("browser pane: could not save its page: {e}");
    }
}

/// The document the pane shows, as it loaded it -- the PDF in its viewer, the
/// page's HTML -- handed to `done` without asking the site again: a site that
/// let the page through once, after a check, need not serve it twice.
/// WebKit says it through `_getMainResourceDataWithCompletionHandler:`, which
/// Safari saves pages with; it has no public counterpart.
pub(crate) fn loaded_document(
    platform: &tauri::webview::PlatformWebview,
    done: impl FnOnce(Result<Vec<u8>, String>) + 'static,
) {
    // SAFETY: `inner` is the pane's live WKWebView, on the main thread, where
    // `with_webview` calls back.
    let view = unsafe { &*platform.inner().cast::<AnyObject>() };
    let selector = objc2::sel!(_getMainResourceDataWithCompletionHandler:);
    let available: bool = unsafe { msg_send![view, respondsToSelector: selector] };
    if !available {
        done(Err(
            "This version of macOS's WebKit cannot hand over the page the agent browser shows"
                .to_string(),
        ));
        return;
    }
    // WebKit answers once; the block only has to hand that on.
    let done = std::cell::Cell::new(Some(done));
    let answered = RcBlock::new(move |data: *mut AnyObject, error: *mut AnyObject| {
        // SAFETY: WebKit hands over an NSData or an NSError, each live for the
        // call; an NSData's bytes are `length` long.
        let outcome = match (unsafe { data.as_ref() }, unsafe { error.as_ref() }) {
            (_, Some(error)) => {
                let description: Option<Retained<NSString>> =
                    unsafe { msg_send![error, localizedDescription] };
                Err(format!(
                    "WebKit could not hand over the page: {}",
                    description.map_or_else(|| "no reason given".to_string(), |d| d.to_string())
                ))
            }
            (Some(data), None) => {
                let length: usize = unsafe { msg_send![data, length] };
                let bytes: *const u8 = unsafe { msg_send![data, bytes] };
                Ok(if length == 0 {
                    Vec::new()
                } else {
                    unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec()
                })
            }
            (None, None) => Err("WebKit handed over no page".to_string()),
        };
        if let Some(done) = done.take() {
            done(outcome);
        }
    });
    // SAFETY: WKWebView's _getMainResourceDataWithCompletionHandler:, checked
    // above, on the main thread.
    let _: () = unsafe { msg_send![view, _getMainResourceDataWithCompletionHandler: &*answered] };
}

/// Give a pane about to close back wry's navigation delegate.
pub(crate) fn stop_listening(webview: &Webview, pane: &PaneId) {
    let label = pane.label();
    let stopped = webview.with_webview(move |platform| {
        if let Some(stand_in) = STAND_INS.with_borrow_mut(|stand_ins| stand_ins.remove(&label)) {
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
