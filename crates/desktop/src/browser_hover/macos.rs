//! WebKit tells a webview's UI delegate what the pointer moves over through a
//! private callback, `_webView:mouseDidMoveOverElement:withFlags:userInfo:`.
//! wry owns the pane's delegate, so the pane gets a stand-in in front of it:
//! the stand-in takes that one callback and hands every other on to wry's.

use std::cell::RefCell;
use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSObject, NSURL};
use tauri::Webview;

struct Ivars {
    /// wry's delegate, which answers everything but hovering.
    original: Option<Retained<AnyObject>>,
    on_hover: Box<dyn Fn(Option<String>)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the stand-in
    // implements no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WilkesBrowserPaneUIDelegate"]
    #[ivars = Ivars]
    struct HoverDelegate;

    impl HoverDelegate {
        #[unsafe(method(_webView:mouseDidMoveOverElement:withFlags:userInfo:))]
        fn mouse_did_move(
            &self,
            _web_view: *mut AnyObject,
            element: *mut AnyObject,
            _flags: usize,
            _user_info: *mut AnyObject,
        ) {
            // SAFETY: WebKit hands over its hit-test result, or nothing.
            let link = unsafe { element.as_ref() }.and_then(link_of);
            (self.ivars().on_hover)(link);
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

/// The address of the link a hit-test result is over, if any.
fn link_of(element: &AnyObject) -> Option<String> {
    // SAFETY: asked before it is called, as the result is WebKit's own class.
    let answers: bool = unsafe { msg_send![element, respondsToSelector: sel!(absoluteLinkURL)] };
    if !answers {
        return None;
    }
    let url: Option<Retained<NSURL>> = unsafe { msg_send![element, absoluteLinkURL] };
    url.and_then(|url| url.absoluteString())
        .map(|url| url.to_string())
}

thread_local! {
    /// Each pane's stand-in, by the window that owns the pane: WebKit holds
    /// its delegate weakly, so the stand-in lives here. Main thread only.
    static DELEGATES: RefCell<HashMap<String, Retained<HoverDelegate>>> = RefCell::new(HashMap::new());
}

pub fn listen_for_hover(
    webview: &Webview,
    owner: &str,
    on_hover: impl Fn(Option<String>) + Send + 'static,
) -> tauri::Result<()> {
    let owner = owner.to_string();
    webview.with_webview(move |platform| {
        let mtm = MainThreadMarker::new().expect("with_webview calls back on the main thread");
        // SAFETY: `inner` is the pane's live WKWebView, on the main thread.
        let view = unsafe { &*platform.inner().cast::<AnyObject>() };
        let original: Option<Retained<AnyObject>> = unsafe { msg_send![view, UIDelegate] };
        let stand_in = HoverDelegate::alloc(mtm).set_ivars(Ivars {
            original,
            on_hover: Box::new(on_hover),
        });
        // SAFETY: NSObject's init, on a freshly allocated stand-in.
        let stand_in: Retained<HoverDelegate> = unsafe { msg_send![super(stand_in), init] };
        let _: () = unsafe { msg_send![view, setUIDelegate: &*stand_in] };
        DELEGATES.with_borrow_mut(|delegates| delegates.insert(owner, stand_in));
    })
}

/// Give the pane back wry's delegate and let the stand-in go.
pub fn stop_listening(webview: &Webview, owner: &str) -> tauri::Result<()> {
    let owner = owner.to_string();
    webview.with_webview(move |platform| {
        if let Some(stand_in) = DELEGATES.with_borrow_mut(|delegates| delegates.remove(&owner)) {
            // SAFETY: as in `listen_for_hover`.
            let view = unsafe { &*platform.inner().cast::<AnyObject>() };
            let original = stand_in.ivars().original.as_deref();
            let _: () = unsafe { msg_send![view, setUIDelegate: original] };
        }
    })
}
