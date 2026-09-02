use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

pub(crate) const DOCUMENT_WINDOW_LABEL: &str = "document";
pub(crate) const NATIVE_OPEN_EVENT: &str = "native-open";

/// One user-authorized request from the operating system. Invalid inputs stay
/// attached to the request so the document window can report them instead of
/// silently dropping part of a multi-file open.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct NativeOpenRequest {
    pub(crate) paths: Vec<String>,
    pub(crate) errors: Vec<String>,
}

#[derive(Default)]
struct DeliveryState {
    frontend_ready: bool,
    queued: VecDeque<NativeOpenRequest>,
}

/// The native lifecycle may deliver files before the document webview has
/// registered its event listener. The queue and readiness bit are one lock so
/// the listener-registration/drain handshake cannot lose a request between
/// those two operations.
#[derive(Default)]
pub(crate) struct NativeOpenState(Mutex<DeliveryState>);

impl NativeOpenState {
    fn prepare_new_window(&self) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.frontend_ready = false;
    }

    fn enqueue(&self, request: NativeOpenRequest) -> bool {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.frontend_ready {
            true
        } else {
            state.queued.push_back(request);
            false
        }
    }

    pub(crate) fn mark_ready_and_drain(&self) -> Vec<NativeOpenRequest> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.frontend_ready = true;
        state.queued.drain(..).collect()
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

pub(crate) fn request_from_urls(urls: Vec<tauri::Url>) -> NativeOpenRequest {
    request_from_paths(urls.into_iter().map(|url| {
        if url.scheme() != "file" {
            return Err(format!("Wilkes can only open local files, not {url}"));
        }
        url.to_file_path()
            .map_err(|_| format!("Cannot decode file URL: {url}"))
    }))
}

/// Extract file operands without assuming every argument is UTF-8. The first
/// argument is skipped only when it resolves to the running executable; this
/// supports both std::env (which includes argv[0]) and single-instance plugin
/// deliveries from platforms that may omit it.
pub(crate) fn request_from_args(args: Vec<OsString>, cwd: &Path) -> NativeOpenRequest {
    let current_exe = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::canonicalize(path).ok());
    let mut paths = Vec::new();
    for (index, argument) in args.into_iter().enumerate() {
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
        if argument.to_string_lossy().starts_with('-') {
            continue;
        }
        paths.push(Ok(resolved));
    }
    request_from_paths(paths)
}

fn focus_document_window(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(DOCUMENT_WINDOW_LABEL)
        .ok_or_else(|| "Document window was not created".to_string())?;
    window.show().map_err(|error| error.to_string())?;
    let _ = window.unminimize();
    window.set_focus().map_err(|error| error.to_string())
}

fn ensure_document_window(app: &AppHandle) -> Result<(), String> {
    if app.get_webview_window(DOCUMENT_WINDOW_LABEL).is_some() {
        return focus_document_window(app);
    }

    app.state::<NativeOpenState>().prepare_new_window();
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
    focus_document_window(app)
}

pub(crate) fn deliver(app: &AppHandle, request: NativeOpenRequest) -> Result<(), String> {
    if request.paths.is_empty() && request.errors.is_empty() {
        return Ok(());
    }
    ensure_document_window(app)?;
    let ready = app.state::<NativeOpenState>().enqueue(request.clone());
    if ready {
        let window = app
            .get_webview_window(DOCUMENT_WINDOW_LABEL)
            .ok_or_else(|| "Document window disappeared before delivery".to_string())?;
        window
            .emit(NATIVE_OPEN_EVENT, request)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn arguments_admit_files_outside_any_workspace_and_report_invalid_inputs() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("paper with spaces.pdf");
        std::fs::write(&file, b"pdf").unwrap();

        let request = request_from_args(
            vec![
                OsString::from("--ignored"),
                file.as_os_str().to_owned(),
                OsString::from("missing.txt"),
            ],
            directory.path(),
        );

        assert_eq!(
            request.paths,
            vec![std::fs::canonicalize(file).unwrap().to_str().unwrap()]
        );
        assert_eq!(request.errors.len(), 1);
        assert!(request.errors[0].contains("missing.txt"));
    }

    #[test]
    fn file_urls_are_decoded_and_non_file_urls_are_rejected() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("notes.md");
        std::fs::write(&file, "hello").unwrap();
        let file_url = tauri::Url::from_file_path(&file).unwrap();
        let https_url = tauri::Url::parse("https://example.com/paper.pdf").unwrap();

        let request = request_from_urls(vec![file_url, https_url]);

        assert_eq!(request.paths.len(), 1);
        assert_eq!(request.errors.len(), 1);
        assert!(request.errors[0].contains("local files"));
    }

    #[test]
    fn readiness_drain_and_live_delivery_are_atomic_states() {
        let state = NativeOpenState::default();
        let request = NativeOpenRequest {
            paths: vec!["/tmp/a.pdf".into()],
            errors: Vec::new(),
        };
        assert!(!state.enqueue(request.clone()));
        assert_eq!(state.mark_ready_and_drain(), vec![request.clone()]);
        assert!(state.enqueue(request));
        assert!(state.mark_ready_and_drain().is_empty());
    }
}
