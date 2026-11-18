//! The agent's browser: the MCP tools' way to fetch and read pages as the
//! user's own browser would, signed in where the user signed in.
//!
//! It is the agent's pane in the main window (`browser_pane`, `PaneKind::Agent`):
//! made hidden the first time an agent needs it, sharing the user's pane's
//! data store, and shown only when the user opens it from the browser button's
//! menu -- to watch, or to take over where a site asks for a person.
//!
//! One agent operation runs at a time: there is one pane, and a page read
//! while another is being fetched would be read from the wrong page. While an
//! operation runs, every download that starts in the agent's pane is its: the
//! first is what it fetched, any other is discarded. A download the user
//! starts there with no operation running goes to the library like one from
//! their own pane.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tauri::webview::Color;
use tauri::{AppHandle, Manager, Url, Webview};
use tokio::sync::oneshot;
use wilkes_agent::browser::{AgentBrowser, BrowserPage, StagedDownload};

use crate::browser_pane::{
    clear_staging, create_pane, emit, is_web_page, web_url, PageEvent, PaneId, AGENT_OWNER,
};

/// How long a page has to load before the agent is told it did not.
const PAGE_TIMEOUT: Duration = Duration::from_secs(60);

const NEEDS_USER: &str = "The site answered with a page instead of the file, most likely a sign-in or a check. The agent browser now shows that page: ask the user to open Agent browser from the browser button's menu in Wilkes, deal with it there, and try again.";

/// What the page says about itself, as `READ_SCRIPT` encodes it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageReport {
    url: String,
    title: String,
    content_type: String,
    text: String,
}

/// The page's address, title, type and text as the browser renders it.
const READ_SCRIPT: &str = r#"JSON.stringify({
  url: location.href,
  title: document.title,
  contentType: document.contentType ?? "",
  text: (document.body ?? document.documentElement)?.innerText ?? ""
})"#;

/// A download the agent's pane finished.
struct Fetched {
    path: PathBuf,
    success: bool,
}

/// The operation in progress's claim on the agent pane's downloads.
struct Claim {
    /// The download it took, once one started.
    url: Option<String>,
    answer: Option<oneshot::Sender<Fetched>>,
}

#[derive(Default)]
pub struct AgentState {
    claim: Mutex<Option<Claim>>,
    /// Downloads no one waits for any more: an operation's that was dropped,
    /// or a second one that started while an operation ran.
    abandoned: Mutex<HashSet<String>>,
    /// Told when the agent's pane finishes loading a page.
    loaded: Mutex<Option<oneshot::Sender<()>>>,
    /// One operation at a time.
    turn: tokio::sync::Mutex<()>,
}

/// A download starting in the agent's pane at `url`: whether it is an agent
/// operation's, and so not the library's.
pub(crate) fn claim(app: &AppHandle, url: &str) -> bool {
    let state = app.state::<AgentState>();
    let mut claim = state.claim.lock().unwrap();
    match claim.as_mut() {
        None => false,
        Some(claim) if claim.url.is_none() => {
            claim.url = Some(url.to_string());
            // A navigation that became a download never finishes loading a
            // page: whoever waits for it is done waiting.
            page_finished(app);
            true
        }
        Some(_) => {
            state.abandoned.lock().unwrap().insert(url.to_string());
            true
        }
    }
}

/// A download in the agent's pane finished at `path`: whether an agent
/// operation took it, by handing it on or discarding it.
pub(crate) fn finished(app: &AppHandle, url: &str, path: &Path, success: bool) -> bool {
    let state = app.state::<AgentState>();
    {
        let mut claim = state.claim.lock().unwrap();
        if let Some(claim) = claim
            .as_mut()
            .filter(|claim| claim.url.as_deref() == Some(url))
        {
            let fetched = Fetched {
                path: path.to_path_buf(),
                success,
            };
            match claim.answer.take().map(|answer| answer.send(fetched)) {
                Some(Ok(())) => {}
                // The operation went between claiming the download and its end.
                Some(Err(_)) | None => clear_staging(path),
            }
            return true;
        }
    }
    if state.abandoned.lock().unwrap().remove(url) {
        clear_staging(path);
        return true;
    }
    false
}

/// The agent's pane finished loading a page.
pub(crate) fn page_finished(app: &AppHandle) {
    if let Some(loaded) = app.state::<AgentState>().loaded.lock().unwrap().take() {
        // Nothing is waiting if the operation went meanwhile.
        loaded.send(()).ok();
    }
}

/// An operation's claim on the agent pane's downloads, for as long as it runs.
/// A download it claimed and did not see finish is abandoned with it.
struct ClaimGuard<'a> {
    state: &'a AgentState,
}

impl<'a> ClaimGuard<'a> {
    fn new(state: &'a AgentState, answer: oneshot::Sender<Fetched>) -> Self {
        *state.claim.lock().unwrap() = Some(Claim {
            url: None,
            answer: Some(answer),
        });
        Self { state }
    }

    /// Whether a download has started for this operation.
    fn started(&self) -> bool {
        self.state
            .claim
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|claim| claim.url.is_some())
    }
}

impl Drop for ClaimGuard<'_> {
    fn drop(&mut self) {
        if let Some(Claim {
            url: Some(url),
            answer: Some(_),
        }) = self.state.claim.lock().unwrap().take()
        {
            self.state.abandoned.lock().unwrap().insert(url);
        }
    }
}

/// The window told an agent is using its browser, for as long as this lives.
struct Busy<'a> {
    app: &'a AppHandle,
}

impl<'a> Busy<'a> {
    fn new(app: &'a AppHandle) -> Self {
        emit(app, &PaneId::agent(), PageEvent::Busy { busy: true });
        Self { app }
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        emit(self.app, &PaneId::agent(), PageEvent::Busy { busy: false });
    }
}

pub struct DesktopAgentBrowser {
    app: AppHandle,
}

impl DesktopAgentBrowser {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    fn state(&self) -> &AgentState {
        self.app.state::<AgentState>().inner()
    }

    /// The agent's pane, made hidden in the main window if it is not there.
    fn webview(&self) -> Result<Webview, String> {
        let pane = PaneId::agent();
        if let Some(webview) = self.app.get_webview(&pane.label()) {
            return Ok(webview);
        }
        let window = self
            .app
            .get_window(AGENT_OWNER)
            .ok_or_else(|| "The Wilkes window is not open".to_string())?;
        create_pane(
            &window,
            &pane,
            Url::parse("about:blank").expect("a valid address"),
            None,
            Color(255, 255, 255, 255),
        )
    }

    /// Wait to be told the next page the pane loads has finished.
    fn expect_load(&self) -> oneshot::Receiver<()> {
        let (loaded, waiting) = oneshot::channel();
        *self.state().loaded.lock().unwrap() = Some(loaded);
        waiting
    }

    /// Open `url` in the pane and wait for it to load.
    async fn open(&self, webview: &Webview, url: &Url) -> Result<(), String> {
        let loaded = self.expect_load();
        webview.navigate(url.clone()).map_err(|e| e.to_string())?;
        match tokio::time::timeout(PAGE_TIMEOUT, loaded).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err("The agent browser closed before the page loaded".to_string()),
            Err(_) => Err(format!(
                "The page did not finish loading within {} seconds",
                PAGE_TIMEOUT.as_secs()
            )),
        }
    }

    /// Start downloading `url` in the pane as the user's own pane saves a
    /// page: open it, and if opening it did not already start a download --
    /// a file served as an attachment can -- save what it shows. Loaded first
    /// rather than downloaded outright, because some sites tell the two apart:
    /// the ACM Digital Library answers a bare download request with a page,
    /// and serves the PDF to a browser that has loaded it.
    async fn start_download(
        &self,
        webview: &Webview,
        url: &Url,
        claim: &ClaimGuard<'_>,
    ) -> Result<(), String> {
        match self.open(webview, url).await {
            _ if claim.started() => Ok(()),
            Ok(()) => save_shown_page(webview),
            Err(error) => Err(error),
        }
    }
}

/// Save what the agent's pane shows, as the user's pane's Save to library
/// does.
fn save_shown_page(webview: &Webview) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        crate::browser_webkit_download::save_page(webview, &PaneId::agent());
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    webview
        .eval(crate::browser_pane::SAVE_PAGE_SCRIPT)
        .map_err(|e| e.to_string())
}

#[async_trait]
impl AgentBrowser for DesktopAgentBrowser {
    async fn download(&self, url: &str) -> Result<StagedDownload, String> {
        let url = web_url(url)?;
        let _turn = self.state().turn.lock().await;
        let _busy = Busy::new(&self.app);
        let webview = self.webview()?;
        let (answer, fetched) = oneshot::channel();
        let claim = ClaimGuard::new(self.state(), answer);
        self.start_download(&webview, &url, &claim).await?;
        let Fetched { path, success } = fetched
            .await
            .map_err(|_| "The agent browser stopped before the download finished".to_string())?;
        drop(claim);
        let dir = path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "The download has no staging directory".to_string())?;
        let suggested_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let staged = StagedDownload::new(dir, path, suggested_name);
        if !success {
            return Err("The agent browser could not download it".to_string());
        }
        // The page is already in the pane, where the user will look for it.
        if is_web_page(staged.file()).map_err(|e| e.to_string())? {
            tracing::warn!(
                url = %url,
                bytes = std::fs::metadata(staged.file()).map(|m| m.len()).unwrap_or(0),
                title = ?page_title(staged.file()),
                "agent browser: the download came back as a page"
            );
            return Err(NEEDS_USER.to_string());
        }
        Ok(staged)
    }

    async fn read(&self, url: &str) -> Result<BrowserPage, String> {
        let url = web_url(url)?;
        let _turn = self.state().turn.lock().await;
        let _busy = Busy::new(&self.app);
        let webview = self.webview()?;
        self.open(&webview, &url).await?;
        let report = crate::browser_script::evaluate(&webview, READ_SCRIPT).await?;
        let page: PageReport = serde_json::from_str(&report)
            .map_err(|e| format!("The page's report could not be read: {e}"))?;
        let content_type = page.content_type.to_ascii_lowercase();
        if !(content_type.is_empty()
            || content_type.contains("html")
            || content_type.starts_with("text/"))
        {
            return Err(format!(
                "This address serves a file ({}), not a page; use download with through_browser=true",
                page.content_type
            ));
        }
        Ok(BrowserPage {
            url: page.url,
            title: page.title,
            text: page.text,
        })
    }
}

/// The `<title>` of a downloaded page, for saying in the log what a site
/// answered with instead of the file.
fn page_title(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(64 * 1024)
        .read_to_end(&mut head)
        .ok()?;
    let head = String::from_utf8_lossy(&head);
    // ASCII lowercasing keeps every character's length, so a position found
    // in the lowered text is the same position in the text.
    let lowered = head.to_ascii_lowercase();
    let open = lowered.find("<title")?;
    let start = open + lowered[open..].find('>')? + 1;
    let end = start + lowered[start..].find("</title")?;
    Some(head[start..end].trim().chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claimed(state: &AgentState) -> Option<String> {
        state
            .claim
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|claim| claim.url.clone())
    }

    #[test]
    fn an_operation_claims_its_first_download_and_abandons_it_when_dropped() {
        let state = AgentState::default();
        let (answer, _fetched) = oneshot::channel();
        let guard = ClaimGuard::new(&state, answer);
        assert!(!guard.started());
        state.claim.lock().unwrap().as_mut().unwrap().url = Some("https://a/1".to_string());
        assert!(guard.started());
        assert_eq!(claimed(&state).as_deref(), Some("https://a/1"));
        drop(guard);
        assert!(state.claim.lock().unwrap().is_none());
        assert!(state.abandoned.lock().unwrap().contains("https://a/1"));
    }

    #[test]
    fn a_delivered_download_is_not_abandoned() {
        let state = AgentState::default();
        let (answer, _fetched) = oneshot::channel();
        let guard = ClaimGuard::new(&state, answer);
        {
            let mut claim = state.claim.lock().unwrap();
            let claim = claim.as_mut().unwrap();
            claim.url = Some("https://a/1".to_string());
            claim.answer.take();
        }
        drop(guard);
        assert!(state.abandoned.lock().unwrap().is_empty());
    }

    #[test]
    fn a_downloaded_page_is_named_by_its_title() {
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("page");
        std::fs::write(&page, "<html><head><TITLE> Just a moment… </TITLE>").unwrap();
        assert_eq!(page_title(&page).as_deref(), Some("Just a moment…"));
        std::fs::write(&page, "<html><body>no title").unwrap();
        assert_eq!(page_title(&page), None);
    }

    #[test]
    fn the_read_script_reports_a_json_string() {
        assert!(READ_SCRIPT.starts_with("JSON.stringify("));
        let report: PageReport = serde_json::from_str(
            r#"{"url":"https://a/","title":"T","contentType":"text/html","text":"x"}"#,
        )
        .unwrap();
        assert_eq!(report.content_type, "text/html");
    }
}
