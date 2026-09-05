//! Every request to show a document that originates outside this process.
//!
//! Three kinds arrive, and they are one kind here: a file handed over by the
//! operating system ("Open With", a drop on the dock icon, a command line),
//! and a `wilkes://` link clicked in another application. They differ in what
//! they can say, not in what has to happen to them — a window has to exist,
//! be focused, and be listening before the request can be shown, and none of
//! those is true at the moment the request arrives.
//!
//! ## The link grammar
//!
//! ```text
//! wilkes://open?path=<absolute path>
//!              [&workspace=<workspace id or name>]
//!              [&page=<1-based page>]
//!              [&line=<1-based line>[&col=<1-based column>]]
//! ```
//!
//! `workspace` is what decides *which window*. Without it the request names a
//! file and nothing else, so it is shown in the standalone reader — the same
//! window the operating system's own file opens land in, which has no
//! workspace, no roots and no search. With it the request names a file *in a
//! library*, and it is shown in the main window, which switches to that
//! workspace and opens the document exactly as a click in the file list would.
//! That is the whole of the routing rule, and it is decided here rather than
//! in the frontend so that the queue below can name one window per request.
//!
//! `page` and `line` are the place within the document, resolved to the
//! [`SourceOrigin`] the viewer already navigates by. Only a request naming a
//! single path can carry one: a multi-file "Open With" has no first document
//! for a page number to belong to.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Url, WebviewUrl, WebviewWindowBuilder};
use wilkes_core::types::SourceOrigin;

/// The window the configuration creates, which holds the workspace.
pub(crate) const MAIN_WINDOW_LABEL: &str = "main";
/// The reader with no workspace behind it, created on demand.
pub(crate) const DOCUMENT_WINDOW_LABEL: &str = "document";
pub(crate) const NATIVE_OPEN_EVENT: &str = "native-open";

/// The scheme other applications address Wilkes with. Registered with the
/// operating system by the bundler, from `plugins.deep-link.desktop.schemes`
/// in `tauri.conf.json`; delivered to us as a `RunEvent::Opened` URL on macOS
/// and as an argument on Windows and Linux.
pub(crate) const DEEP_LINK_SCHEME: &str = "wilkes";
/// The only thing a link may ask for. Named rather than ignored so that a
/// link asking for something else is answered instead of being read as an
/// open of a file called "search".
const DEEP_LINK_OPEN_ACTION: &str = "open";

/// One user-authorized request from outside the process. Invalid inputs stay
/// attached to the request so the window can report them instead of silently
/// dropping part of a multi-file open, or opening a document at a place the
/// link did not ask for.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub(crate) struct NativeOpenRequest {
    pub(crate) paths: Vec<String>,
    pub(crate) errors: Vec<String>,
    /// The workspace the request named, and therefore the window it is shown
    /// in. `None` is the standalone reader.
    pub(crate) workspace: Option<String>,
    /// Where to land inside `paths[0]`. Only ever set for a request carrying
    /// exactly one path.
    pub(crate) origin: Option<SourceOrigin>,
}

impl NativeOpenRequest {
    fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.errors.is_empty()
    }

    /// Which window has to show this. The one routing decision, taken once.
    fn window_label(&self) -> &'static str {
        match self.workspace {
            Some(_) => MAIN_WINDOW_LABEL,
            None => DOCUMENT_WINDOW_LABEL,
        }
    }

    /// Abandon the open and say why. A request that cannot be honoured as
    /// written is not opened at some other place instead: a link that
    /// disagrees with itself is a defect in whatever wrote it, and opening
    /// the document anyway would leave the user to notice that the page they
    /// asked for is not the page they got.
    fn refuse(&mut self, error: String) {
        self.paths.clear();
        self.origin = None;
        self.errors.push(error);
    }
}

#[derive(Default)]
struct DeliveryState {
    frontend_ready: bool,
    queued: VecDeque<NativeOpenRequest>,
}

/// The native lifecycle may deliver a request before the webview that has to
/// show it has registered its event listener. The queue and readiness bit are
/// one lock so the listener-registration/drain handshake cannot lose a
/// request between those two operations.
///
/// Keyed by window label: the standalone reader and the main window each have
/// their own readiness, and a link that arrives at startup waits for the
/// window it is addressed to rather than for whichever one happens to be up.
#[derive(Default)]
pub(crate) struct NativeOpenState(Mutex<HashMap<String, DeliveryState>>);

impl NativeOpenState {
    fn with<T>(&self, label: &str, act: impl FnOnce(&mut DeliveryState) -> T) -> T {
        let mut windows = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        act(windows.entry(label.to_string()).or_default())
    }

    fn prepare_new_window(&self, label: &str) {
        self.with(label, |state| state.frontend_ready = false);
    }

    fn enqueue(&self, label: &str, request: NativeOpenRequest) -> bool {
        self.with(label, |state| {
            if state.frontend_ready {
                true
            } else {
                state.queued.push_back(request);
                false
            }
        })
    }

    pub(crate) fn mark_ready_and_drain(&self, label: &str) -> Vec<NativeOpenRequest> {
        self.with(label, |state| {
            state.frontend_ready = true;
            state.queued.drain(..).collect()
        })
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn validate_path(path: PathBuf) -> Result<String, String> {
    let original = display_path(&path);
    let canonical =
        std::fs::canonicalize(&path).map_err(|error| format!("Cannot open {original}: {error}"))?;
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| format!("Cannot inspect {original}: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("Cannot open non-file path: {original}"));
    }
    canonical
        .into_os_string()
        .into_string()
        .map_err(|_| format!("Cannot open a path that is not valid UTF-8: {original}"))
}

fn request_from_paths(
    paths: impl IntoIterator<Item = Result<PathBuf, String>>,
) -> NativeOpenRequest {
    let mut request = NativeOpenRequest::default();
    let mut seen = HashSet::new();
    for path in paths {
        match path.and_then(validate_path) {
            Ok(path) if seen.insert(path.clone()) => request.paths.push(path),
            Ok(_) => {}
            Err(error) => request.errors.push(error),
        }
    }
    request
}

fn is_deep_link(url: &Url) -> bool {
    url.scheme().eq_ignore_ascii_case(DEEP_LINK_SCHEME)
}

/// A link's number, or the reason it is not one. Zero is rejected along with
/// the rest: pages and lines are counted from one everywhere a match names
/// them, and zero is the viewer's own word for "nowhere in particular".
fn link_number(name: &str, raw: &str, errors: &mut Vec<String>) -> Option<u32> {
    match raw.parse::<u32>() {
        Ok(value) if value >= 1 => Some(value),
        _ => {
            errors.push(format!(
                "A link's {name} must be a whole number of at least 1, not \"{raw}\""
            ));
            None
        }
    }
}

fn looks_like_pdf(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

/// Read one `wilkes://` link. See the grammar at the top of this module.
pub(crate) fn request_from_deep_link(url: &Url) -> NativeOpenRequest {
    let mut request = NativeOpenRequest::default();

    let action = url.host_str().unwrap_or_default().to_owned();
    if !action.eq_ignore_ascii_case(DEEP_LINK_OPEN_ACTION) {
        request.errors.push(format!(
            "Wilkes does not know how to \"{action}\". A link must read \
             wilkes://{DEEP_LINK_OPEN_ACTION}?path=..."
        ));
        return request;
    }

    let mut path: Option<String> = None;
    let (mut page, mut line, mut col) = (None, None, None);
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "path" => path = Some(value.into_owned()),
            "workspace" => {
                let named = value.trim().to_owned();
                request.workspace = (!named.is_empty()).then_some(named);
            }
            "page" => page = link_number("page", &value, &mut request.errors),
            "line" => line = link_number("line", &value, &mut request.errors),
            "col" => col = link_number("col", &value, &mut request.errors),
            unknown => request.errors.push(format!(
                "Wilkes has nothing to do with the link parameter \"{unknown}\""
            )),
        }
    }

    let Some(path) = path.filter(|path| !path.trim().is_empty()) else {
        request
            .errors
            .push("A wilkes:// link must name the path it opens".to_string());
        return request;
    };
    match validate_path(PathBuf::from(path)) {
        Ok(path) => request.paths.push(path),
        Err(error) => {
            request.errors.push(error);
            return request;
        }
    }

    request.origin = match (page, line, col) {
        (None, None, None) => None,
        (Some(page), None, None) => Some(SourceOrigin::PdfPage { page, bbox: None }),
        (None, Some(line), col) => Some(SourceOrigin::TextFile {
            line,
            col: col.unwrap_or(1),
        }),
        (None, None, Some(_)) => {
            request.refuse("A link that names a column must name its line too".to_string());
            return request;
        }
        (Some(_), _, _) => {
            request.refuse(
                "A link names a page or a line, not both: a page belongs to a PDF and \
                 a line to a text document"
                    .to_string(),
            );
            return request;
        }
    };

    // The place has to belong to the document. The viewer navigates a PDF by
    // page and everything else by line, so a page number for a text file is
    // not a place it can go to -- and honouring the open while dropping the
    // page would show the reader the first page of a document their link said
    // to open at the fortieth.
    let is_pdf = looks_like_pdf(&request.paths[0]);
    let mismatch = match request.origin {
        Some(SourceOrigin::PdfPage { .. }) if !is_pdf => {
            Some("A link names a page of a PDF, and this document is not one")
        }
        Some(SourceOrigin::TextFile { .. }) if is_pdf => {
            Some("A link names a line of a text document, and this document is a PDF")
        }
        _ => None,
    };
    if let Some(mismatch) = mismatch {
        request.refuse(format!("{mismatch}: {}", request.paths[0]));
    }
    request
}

/// Split what the operating system handed over into one request per window it
/// has to reach. File operands are one request — a multi-file "Open With" is
/// one act by one person — and every link is its own, because each names its
/// own destination.
pub(crate) fn requests_from_urls(urls: Vec<Url>) -> Vec<NativeOpenRequest> {
    let (links, files): (Vec<Url>, Vec<Url>) = urls.into_iter().partition(is_deep_link);
    let mut requests: Vec<NativeOpenRequest> =
        links.iter().map(request_from_deep_link).collect();
    requests.push(request_from_paths(files.into_iter().map(|url| {
        if url.scheme() != "file" {
            return Err(format!("Wilkes can only open local files, not {url}"));
        }
        url.to_file_path()
            .map_err(|_| format!("Cannot decode file URL: {url}"))
    })));
    requests.retain(|request| !request.is_empty());
    requests
}

/// Extract link and file operands without assuming every argument is UTF-8.
/// The first argument is skipped only when it resolves to the running
/// executable; this supports both std::env (which includes argv[0]) and
/// single-instance plugin deliveries from platforms that may omit it.
pub(crate) fn requests_from_args(args: Vec<OsString>, cwd: &Path) -> Vec<NativeOpenRequest> {
    let current_exe = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::canonicalize(path).ok());
    let mut requests = Vec::new();
    let mut paths = Vec::new();
    for (index, argument) in args.into_iter().enumerate() {
        let text = argument.to_string_lossy().into_owned();
        // Windows and Linux hand a link over as an argument, so classifying
        // is the first thing done with one -- before it is joined to the
        // working directory and reported as a file that is not there.
        if let Ok(url) = Url::parse(&text) {
            if is_deep_link(&url) {
                requests.push(request_from_deep_link(&url));
                continue;
            }
        }
        let path = PathBuf::from(&argument);
        let resolved = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        let is_executable = index == 0
            && current_exe.as_ref().is_some_and(|executable| {
                std::fs::canonicalize(&resolved)
                    .ok()
                    .as_ref()
                    .is_some_and(|candidate| candidate == executable)
            });
        if is_executable {
            continue;
        }
        if text.starts_with('-') {
            continue;
        }
        paths.push(Ok(resolved));
    }
    requests.push(request_from_paths(paths));
    requests.retain(|request| !request.is_empty());
    requests
}

fn focus_window(app: &AppHandle, label: &str) -> Result<(), String> {
    let window = app
        .get_webview_window(label)
        .ok_or_else(|| format!("The {label} window was not created"))?;
    window.show().map_err(|error| error.to_string())?;
    let _ = window.unminimize();
    window.set_focus().map_err(|error| error.to_string())
}

/// The window a request is addressed to, brought forward. The main window is
/// created by the configuration and is never built here: a request that finds
/// it missing has arrived during shutdown, and saying so is better than
/// resurrecting a window whose workspace is already gone.
fn ensure_window(app: &AppHandle, label: &str) -> Result<(), String> {
    if label == MAIN_WINDOW_LABEL || app.get_webview_window(label).is_some() {
        return focus_window(app, label);
    }

    app.state::<NativeOpenState>().prepare_new_window(label);
    WebviewWindowBuilder::new(
        app,
        DOCUMENT_WINDOW_LABEL,
        WebviewUrl::App("index.html?mode=document".into()),
    )
    .title("Wilkes — Documents")
    .inner_size(1100.0, 800.0)
    .min_inner_size(600.0, 300.0)
    .center()
    .build()
    .map_err(|error| format!("Could not create document window: {error}"))?;
    focus_window(app, label)
}

pub(crate) fn deliver(app: &AppHandle, request: NativeOpenRequest) -> Result<(), String> {
    if request.is_empty() {
        return Ok(());
    }
    let label = request.window_label();
    ensure_window(app, label)?;
    let ready = app.state::<NativeOpenState>().enqueue(label, request.clone());
    if ready {
        let window = app
            .get_webview_window(label)
            .ok_or_else(|| format!("The {label} window disappeared before delivery"))?;
        window
            .emit(NATIVE_OPEN_EVENT, request)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Deliver everything one launch or one link click produced, reporting each
/// failure rather than abandoning the rest of the batch on the first.
pub(crate) fn deliver_all(app: &AppHandle, requests: Vec<NativeOpenRequest>) {
    for request in requests {
        if let Err(error) = deliver(app, request) {
            tracing::error!("open request delivery failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn only(mut requests: Vec<NativeOpenRequest>) -> NativeOpenRequest {
        assert_eq!(requests.len(), 1, "expected one request: {requests:?}");
        requests.remove(0)
    }

    #[test]
    fn arguments_admit_files_outside_any_workspace_and_report_invalid_inputs() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("paper with spaces.pdf");
        std::fs::write(&file, b"pdf").unwrap();

        let request = only(requests_from_args(
            vec![
                OsString::from("--ignored"),
                file.as_os_str().to_owned(),
                OsString::from("missing.txt"),
            ],
            directory.path(),
        ));

        assert_eq!(
            request.paths,
            vec![std::fs::canonicalize(file).unwrap().to_str().unwrap()]
        );
        assert_eq!(request.errors.len(), 1);
        assert!(request.errors[0].contains("missing.txt"));
        assert_eq!(request.workspace, None);
    }

    #[test]
    fn file_urls_are_decoded_and_non_file_urls_are_rejected() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("notes.md");
        std::fs::write(&file, "hello").unwrap();
        let file_url = Url::from_file_path(&file).unwrap();
        let https_url = Url::parse("https://example.com/paper.pdf").unwrap();

        let request = only(requests_from_urls(vec![file_url, https_url]));

        assert_eq!(request.paths.len(), 1);
        assert_eq!(request.errors.len(), 1);
        assert!(request.errors[0].contains("local files"));
    }

    #[test]
    fn readiness_drain_and_live_delivery_are_atomic_states_per_window() {
        let state = NativeOpenState::default();
        let request = NativeOpenRequest {
            paths: vec!["/tmp/a.pdf".into()],
            ..Default::default()
        };
        assert!(!state.enqueue(DOCUMENT_WINDOW_LABEL, request.clone()));
        // The other window's readiness is its own: draining one must not
        // release what is queued for the other.
        assert!(state.mark_ready_and_drain(MAIN_WINDOW_LABEL).is_empty());
        assert_eq!(
            state.mark_ready_and_drain(DOCUMENT_WINDOW_LABEL),
            vec![request.clone()]
        );
        assert!(state.enqueue(DOCUMENT_WINDOW_LABEL, request));
        assert!(state.mark_ready_and_drain(DOCUMENT_WINDOW_LABEL).is_empty());
    }

    /// The routing rule, stated once here so it cannot drift: a link that
    /// names no workspace is a document and nothing else, and goes to the
    /// reader that has no workspace behind it.
    #[test]
    fn a_link_without_a_workspace_opens_the_standalone_reader() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("paper.pdf");
        std::fs::write(&file, b"pdf").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();

        let request = request_from_deep_link(
            &Url::parse(&format!(
                "wilkes://open?path={}",
                urlencoding_for_test(canonical.to_str().unwrap())
            ))
            .unwrap(),
        );

        assert_eq!(request.paths, vec![canonical.to_str().unwrap()]);
        assert_eq!(request.errors, Vec::<String>::new());
        assert_eq!(request.workspace, None);
        assert_eq!(request.window_label(), DOCUMENT_WINDOW_LABEL);
    }

    #[test]
    fn a_link_naming_a_workspace_and_a_page_opens_the_main_window_there() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("book.pdf");
        std::fs::write(&file, b"pdf").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();

        let request = request_from_deep_link(
            &Url::parse(&format!(
                "wilkes://open?path={}&workspace=Corpus%20One&page=42",
                urlencoding_for_test(canonical.to_str().unwrap())
            ))
            .unwrap(),
        );

        assert_eq!(request.workspace.as_deref(), Some("Corpus One"));
        assert_eq!(request.window_label(), MAIN_WINDOW_LABEL);
        assert_eq!(
            request.origin,
            Some(SourceOrigin::PdfPage {
                page: 42,
                bbox: None
            })
        );
        assert!(request.errors.is_empty());
    }

    #[test]
    fn a_link_names_a_line_of_a_text_document() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("notes.md");
        std::fs::write(&file, "hello").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();

        let request = request_from_deep_link(
            &Url::parse(&format!(
                "wilkes://open?path={}&line=12",
                urlencoding_for_test(canonical.to_str().unwrap())
            ))
            .unwrap(),
        );

        assert_eq!(
            request.origin,
            Some(SourceOrigin::TextFile { line: 12, col: 1 })
        );
    }

    /// A place that cannot belong to the document is a defect in whatever
    /// wrote the link, and the open is abandoned rather than quietly landing
    /// somewhere else.
    #[test]
    fn a_page_of_something_that_is_not_a_pdf_is_refused_rather_than_dropped() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("notes.md");
        std::fs::write(&file, "hello").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();

        let request = request_from_deep_link(
            &Url::parse(&format!(
                "wilkes://open?path={}&page=3",
                urlencoding_for_test(canonical.to_str().unwrap())
            ))
            .unwrap(),
        );

        assert!(request.paths.is_empty());
        assert_eq!(request.origin, None);
        assert_eq!(request.errors.len(), 1);
        assert!(request.errors[0].contains("not one"));
    }

    #[test]
    fn a_link_that_names_neither_an_action_nor_a_path_says_which_is_missing() {
        let no_path = request_from_deep_link(&Url::parse("wilkes://open").unwrap());
        assert!(no_path.errors[0].contains("must name the path"));

        let wrong_action =
            request_from_deep_link(&Url::parse("wilkes://search?q=bayes").unwrap());
        assert!(wrong_action.errors[0].contains("does not know how to \"search\""));
        assert!(wrong_action.paths.is_empty());
    }

    #[test]
    fn a_link_arriving_as_an_argument_is_read_as_a_link_and_not_as_a_file() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("book.pdf");
        std::fs::write(&file, b"pdf").unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();
        let dropped = directory.path().join("dropped.pdf");
        std::fs::write(&dropped, b"pdf").unwrap();

        let requests = requests_from_args(
            vec![
                OsString::from(format!(
                    "wilkes://open?path={}&workspace=library",
                    urlencoding_for_test(canonical.to_str().unwrap())
                )),
                dropped.as_os_str().to_owned(),
            ],
            directory.path(),
        );

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].workspace.as_deref(), Some("library"));
        assert_eq!(requests[0].paths, vec![canonical.to_str().unwrap()]);
        assert_eq!(requests[1].workspace, None);
        assert_eq!(
            requests[1].paths,
            vec![std::fs::canonicalize(&dropped).unwrap().to_str().unwrap()]
        );
    }

    /// Percent-encoding only what a query value may not contain literally.
    /// The production side never encodes -- it only ever reads what another
    /// application wrote -- so this belongs to the tests and nowhere else.
    fn urlencoding_for_test(value: &str) -> String {
        value
            .bytes()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                    (byte as char).to_string()
                }
                other => format!("%{other:02X}"),
            })
            .collect()
    }
}
