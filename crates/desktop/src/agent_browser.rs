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
//!
//! A download is taken from the page the pane already shows when that page is
//! the address asked for, or where opening that address led -- a redirect, a
//! bot check that forwarded once the user passed it there -- rather than by
//! opening the address again: some sites (ETH's Research Collection) ask a
//! fresh visit to pass their check again, however recently it was passed.
//! What the page shows is taken as it loaded, on macOS, without asking the
//! site again; elsewhere it is downloaded from the page's own address through
//! the pane's session.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use tauri::webview::Color;
use tauri::{AppHandle, Manager, Url, Webview};
use tokio::sync::oneshot;
use wilkes_agent::browser::{AgentBrowser, BrowserPage, StagedDownload};

#[cfg(target_os = "macos")]
use crate::browser_pane::staging_dir;
use crate::browser_pane::{
    clear_staging, create_pane, emit, is_web_page, web_url, PageEvent, PaneId, AGENT_OWNER,
};

/// How long a page has to load before the agent is told it did not.
const PAGE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a page answered with a bot check's status is given to pass the
/// browser and forward itself to what was asked for.
const CHECK_WAIT: Duration = Duration::from_secs(10);

/// How long a web page shown where a file was asked for is given to move on
/// to the file by itself. ETH's Research Collection answers a file's address
/// with its own page, which fetches the file by script a few seconds after it
/// has loaded.
const FORWARD_WAIT: Duration = Duration::from_secs(10);

/// How often the pane is looked at again while a page loads or a check runs,
/// besides whenever a page finishes loading.
const LOOK_AGAIN: Duration = Duration::from_secs(2);

/// The statuses a bot check answers with before the browser has passed it:
/// Cloudflare's are 403 and 503. A page answered with one of these may yet
/// forward itself; one answered with any other error will not.
fn may_be_a_check(status: u16) -> bool {
    matches!(status, 403 | 503)
}

fn refused(status: u16, waited: bool) -> String {
    let waited = if waited {
        format!(
            " and did not move on within {} seconds",
            CHECK_WAIT.as_secs()
        )
    } else {
        String::new()
    };
    format!(
        "The site answered HTTP {status}{waited}. If it asks for a sign-in or a check a person must pass, ask the user to open Agent browser from the browser button's menu in Wilkes, where the page is showing, deal with it there, and try again."
    )
}

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
    /// What the agent's pane is showing, as far as an operation needs to know.
    page: Mutex<PageState>,
    /// The address last opened in the agent's pane, by an operation or by the
    /// user from its address bar. The page shown is that address or one it
    /// led to by itself, unless the user followed a link from it.
    opened: Mutex<Option<Url>>,
    /// The address of the last page the agent's pane finished loading, after
    /// any redirect. Noted here rather than asked of the webview: a WKWebView
    /// that has shown nothing yet has no address, and wry's `url()` panics on
    /// one.
    shown: Mutex<Option<Url>>,
    /// One operation at a time.
    turn: tokio::sync::Mutex<()>,
}

/// The page in the agent's pane: the HTTP status its document was answered
/// with, and whether it has finished loading. Reset when an operation opens
/// an address, so a page from before cannot pass for the one asked for.
#[derive(Clone, Debug, Default, PartialEq)]
struct PageState {
    /// None for a document that came with no HTTP status.
    status: Option<u16>,
    loaded: bool,
    /// The name the response gave the document, to save it by. Said by the
    /// platform with the response on macOS, the one platform that takes the
    /// document as it loaded.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    suggested_name: Option<String>,
    /// The document's MIME type, as its response gave it. Said on macOS.
    mime_type: Option<String>,
}

/// What the platform says about a document's response as it arrives.
pub(crate) struct Response {
    pub status: Option<u16>,
    pub suggested_name: Option<String>,
    pub mime_type: Option<String>,
}

impl PageState {
    /// Whether the document is a web page, by its response's type. A page
    /// whose type was not said is not known to be one.
    fn is_web_page(&self) -> bool {
        self.mime_type.as_deref().is_some_and(|mime| {
            let mime = mime.trim().to_ascii_lowercase();
            mime == "text/html" || mime == "application/xhtml+xml"
        })
    }

    /// What the operation should do about the page.
    fn verdict(&self) -> Verdict {
        match self.status {
            _ if !self.loaded => Verdict::Loading,
            Some(status) if !(200..300).contains(&status) => Verdict::Refused(status),
            _ => Verdict::Content,
        }
    }
}

#[derive(Debug, PartialEq)]
enum Verdict {
    Loading,
    Content,
    Refused(u16),
}

/// The agent's pane got the `response` to its document: a new page, not
/// loaded yet. Where the platform says so as the response arrives.
pub(crate) fn response_arrived(app: &AppHandle, pane: &PaneId, response: Response) {
    if pane.kind == crate::browser_pane::PaneKind::Agent {
        *app.state::<AgentState>().page.lock().unwrap() = PageState {
            status: response.status,
            loaded: false,
            suggested_name: response.suggested_name,
            mime_type: response.mime_type,
        };
    }
}

/// The user opened `url` in the agent's pane from its address bar: what it
/// shows from now on comes from there.
pub(crate) fn opening(app: &AppHandle, url: &Url) {
    *app.state::<AgentState>().opened.lock().unwrap() = Some(url.clone());
}

/// The same address, whatever place in the page each points at.
fn same_address(a: &Url, b: &Url) -> bool {
    let (mut a, mut b) = (a.clone(), b.clone());
    a.set_fragment(None);
    b.set_fragment(None);
    a == b
}

/// Whether a download of `asked` is to be taken from the page the pane shows,
/// last loaded at `shown`, rather than by opening `asked` again: the pane is
/// at that address, or `asked` is the address last `opened` there and the
/// pane is where it led, and the page has loaded what was asked for or is
/// loading still. A pane that has loaded nothing shows nothing to take. A
/// page the site refused is opened afresh: a check not passed yet starts
/// over, and an error may not be the site's answer the next time.
fn reusable(asked: &Url, opened: Option<&Url>, shown: Option<&Url>, page: &PageState) -> bool {
    let Some(shown) = shown else {
        return false;
    };
    let from_asked =
        same_address(shown, asked) || opened.is_some_and(|opened| same_address(opened, asked));
    from_asked && !matches!(page.verdict(), Verdict::Refused(_))
}

/// The agent's pane's document was answered with `status`, said once it has
/// loaded. Where the platform only says so then: WebView2.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn response_completed(app: &AppHandle, status: Option<u16>) {
    app.state::<AgentState>().page.lock().unwrap().status = status;
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
            wake(app);
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

/// The agent's pane finished loading the page at `url`.
pub(crate) fn page_finished(app: &AppHandle, url: &Url) {
    // The blank page the pane is made with is not a page anyone asked for:
    // its load ending must not pass for the one an operation is waiting on.
    if url.as_str() != "about:blank" {
        let state = app.state::<AgentState>();
        state.page.lock().unwrap().loaded = true;
        *state.shown.lock().unwrap() = Some(url.clone());
    }
    wake(app);
}

/// Tell whoever waits that the pane has something new to look at.
fn wake(app: &AppHandle) {
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
        // A pane before this one, gone with its window, left its page behind.
        *self.state().opened.lock().unwrap() = None;
        *self.state().shown.lock().unwrap() = None;
        *self.state().page.lock().unwrap() = PageState::default();
        create_pane(
            &window,
            &pane,
            Url::parse("about:blank").expect("a valid address"),
            None,
            Color(255, 255, 255, 255),
        )
    }

    /// Wait to be told the pane has something new to look at.
    fn expect_load(&self) -> oneshot::Receiver<()> {
        let (loaded, waiting) = oneshot::channel();
        *self.state().loaded.lock().unwrap() = Some(loaded);
        waiting
    }

    /// The page in the pane, read on the main thread after every browser
    /// event already queued there: a status the platform reports in a
    /// handler of its own, after the one that says the page loaded, is in by
    /// then.
    async fn page(&self, webview: &Webview) -> Result<PageState, String> {
        let (answer, answered) = oneshot::channel();
        let app = self.app.clone();
        webview
            .with_webview(move |_| {
                let page = app.state::<AgentState>().page.lock().unwrap().clone();
                answer.send(page).ok();
            })
            .map_err(|e| e.to_string())?;
        answered
            .await
            .map_err(|_| "The agent browser closed".to_string())
    }

    /// Open `url` in the pane and wait until it shows what was asked for.
    async fn open(
        &self,
        webview: &Webview,
        url: &Url,
        claim: Option<&ClaimGuard<'_>>,
    ) -> Result<(), String> {
        *self.state().page.lock().unwrap() = PageState::default();
        *self.state().opened.lock().unwrap() = Some(url.clone());
        let loaded = self.expect_load();
        webview.navigate(url.clone()).map_err(|e| e.to_string())?;
        self.settle(webview, claim, Some(loaded)).await
    }

    /// Whether the pane shows the page a download of `url` is to be taken
    /// from, rather than opening `url` again (`reusable`).
    async fn shows_page_from(&self, webview: &Webview, url: &Url) -> Result<bool, String> {
        // Read after the pane's queued events, as the page is.
        let page = self.page(webview).await?;
        let shown = self.state().shown.lock().unwrap().clone();
        let opened = self.state().opened.lock().unwrap().clone();
        Ok(reusable(url, opened.as_ref(), shown.as_ref(), &page))
    }

    /// Wait until the pane shows what was asked for: a page loaded with a 2xx
    /// status. A page answered with a bot check's status is given time to
    /// pass the browser and forward itself -- which it does in the pane,
    /// hidden or not -- and any other error is the site's answer. A download,
    /// which holds a `claim`, wants a file: a web page loaded in its place is
    /// given time to move on to it, and is what the download takes if it
    /// does not. A download the page started is also an end. With `next`,
    /// told of the load the caller just started, the pane is looked at once
    /// that load ends; without, at once.
    async fn settle(
        &self,
        webview: &Webview,
        claim: Option<&ClaimGuard<'_>>,
        mut next: Option<oneshot::Receiver<()>>,
    ) -> Result<(), String> {
        let load_deadline = Instant::now() + PAGE_TIMEOUT;
        let mut check_deadline: Option<Instant> = None;
        let mut forward_deadline: Option<Instant> = None;
        loop {
            if let Some(next) = next.take() {
                let _ = tokio::time::timeout(LOOK_AGAIN, next).await;
            }
            if claim.is_some_and(ClaimGuard::started) {
                return Ok(());
            }
            let now = Instant::now();
            let page = self.page(webview).await?;
            match page.verdict() {
                Verdict::Content if claim.is_some() && page.is_web_page() => {
                    let deadline = *forward_deadline.get_or_insert_with(|| {
                        tracing::info!("agent browser: waiting for a page to move on to the file");
                        now + FORWARD_WAIT
                    });
                    if now >= deadline {
                        return Ok(());
                    }
                }
                Verdict::Content => return Ok(()),
                Verdict::Refused(status) if !may_be_a_check(status) => {
                    return Err(refused(status, false));
                }
                Verdict::Refused(status) => {
                    let deadline = *check_deadline.get_or_insert_with(|| {
                        tracing::info!(status, "agent browser: waiting for a check to pass");
                        now + CHECK_WAIT
                    });
                    if now >= deadline {
                        return Err(refused(status, true));
                    }
                }
                Verdict::Loading if now >= load_deadline && check_deadline.is_none() => {
                    return Err(format!(
                        "The page did not finish loading within {} seconds",
                        PAGE_TIMEOUT.as_secs()
                    ));
                }
                Verdict::Loading => {}
            }
            next = Some(self.expect_load());
        }
    }

    /// Take what the pane shows as it loaded it, by the name its response
    /// gave it -- the PDF a site let through once, without asking again.
    #[cfg(target_os = "macos")]
    async fn take_shown_page(
        &self,
        webview: &Webview,
        _fetched: oneshot::Receiver<Fetched>,
    ) -> Result<StagedDownload, String> {
        let (answer, answered) = oneshot::channel();
        let app = self.app.clone();
        webview
            .with_webview(move |platform| {
                // Read with the document, on the main thread, so a page that
                // replaces it meanwhile cannot lend it its name.
                let name = app
                    .state::<AgentState>()
                    .page
                    .lock()
                    .unwrap()
                    .suggested_name
                    .clone();
                crate::browser_webkit_download::loaded_document(&platform, move |data| {
                    // Nothing is waiting if the operation went meanwhile.
                    answer.send(data.map(|data| (name, data))).ok();
                });
            })
            .map_err(|e| e.to_string())?;
        let (name, data) = answered
            .await
            .map_err(|_| "The agent browser closed".to_string())??;
        let name = name.ok_or_else(|| "WebKit gave the page no name to save it by".to_string())?;
        let name = Path::new(&name)
            .file_name()
            .ok_or_else(|| format!("The page's name is not a file name: {name}"))?
            .to_os_string();
        let dir = staging_dir(&self.app, AGENT_OWNER)?;
        let file = dir.join(&name);
        let staged = StagedDownload::new(dir, file.clone(), name.to_string_lossy().into_owned());
        tokio::fs::write(&file, data)
            .await
            .map_err(|e| format!("Could not stage the page: {e}"))?;
        Ok(staged)
    }

    /// Download what the pane shows from its own address, through the pane's
    /// own session, as the user's pane's Save to library does.
    #[cfg(not(target_os = "macos"))]
    async fn take_shown_page(
        &self,
        webview: &Webview,
        fetched: oneshot::Receiver<Fetched>,
    ) -> Result<StagedDownload, String> {
        webview
            .eval(crate::browser_pane::SAVE_PAGE_SCRIPT)
            .map_err(|e| e.to_string())?;
        staged(fetched).await
    }
}

/// The download the agent's pane finished for an operation, staged.
async fn staged(fetched: oneshot::Receiver<Fetched>) -> Result<StagedDownload, String> {
    let Fetched { path, success } = fetched
        .await
        .map_err(|_| "The agent browser stopped before the download finished".to_string())?;
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
    Ok(staged)
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
        // Loaded first rather than downloaded outright, because some sites
        // tell the two apart: the ACM Digital Library answers a bare download
        // request with a page, and serves the PDF to a browser that has
        // loaded it. Opening it may start a download of its own, of a file
        // served as an attachment; otherwise what it shows is taken.
        if self.shows_page_from(&webview, &url).await? {
            tracing::info!(url = %url, "agent browser: taking the download from the page it shows, not opening it again");
            self.settle(&webview, Some(&claim), None).await?;
        } else {
            self.open(&webview, &url, Some(&claim)).await?;
        }
        let staged = if claim.started() {
            staged(fetched).await?
        } else {
            self.take_shown_page(&webview, fetched).await?
        };
        drop(claim);
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
        self.open(&webview, &url, None).await?;
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

    fn url(address: &str) -> Url {
        Url::parse(address).unwrap()
    }

    fn page(status: Option<u16>, loaded: bool) -> PageState {
        PageState {
            status,
            loaded,
            suggested_name: None,
            mime_type: None,
        }
    }

    #[test]
    fn a_page_is_told_from_a_file_by_its_type() {
        let typed = |mime: Option<&str>| PageState {
            mime_type: mime.map(str::to_string),
            ..page(Some(200), true)
        };
        assert!(typed(Some("text/html")).is_web_page());
        assert!(typed(Some("application/xhtml+xml")).is_web_page());
        assert!(!typed(Some("application/pdf")).is_web_page());
        assert!(!typed(None).is_web_page());
    }

    const ASKED: &str = "https://www.research-collection.ethz.ch/bitstreams/5c1c/download";
    const LED_TO: &str =
        "https://www.research-collection.ethz.ch/server/api/core/bitstreams/5c1c/content";

    /// Whether a download of `ASKED` reuses the pane's page, with `opened` the
    /// address last opened there and `shown` the page it last loaded.
    fn reuses(opened: Option<&str>, shown: Option<&str>, page: PageState) -> bool {
        let (opened, shown) = (opened.map(url), shown.map(url));
        reusable(&url(ASKED), opened.as_ref(), shown.as_ref(), &page)
    }

    #[test]
    fn a_download_is_taken_from_the_page_already_at_its_address() {
        // A check passed in the pane, which reloaded the same address.
        assert!(reuses(Some(ASKED), Some(ASKED), page(Some(200), true)));
        // Opened by the user from the address bar, or before the pane
        // remembered what it opened: the address shown is enough.
        assert!(reuses(None, Some(ASKED), page(Some(200), true)));
        // Another place in the same page.
        let elsewhere_in_it = format!("{ASKED}#page=2");
        assert!(reuses(None, Some(&elsewhere_in_it), page(Some(200), true)));
        // Still loading what the check forwarded to: waited for, not reloaded.
        assert!(reuses(Some(ASKED), Some(ASKED), page(Some(200), false)));
    }

    #[test]
    fn a_download_is_taken_from_the_page_its_address_led_to() {
        assert!(reuses(Some(ASKED), Some(LED_TO), page(Some(200), true)));
        // A PDF shown from elsewhere, with no HTTP status.
        assert!(reuses(Some(ASKED), Some(LED_TO), page(None, true)));
    }

    #[test]
    fn an_address_is_opened_afresh_where_the_pane_shows_no_page_from_it() {
        // A pane just made, which has loaded nothing -- even where the address
        // was opened in a pane before it.
        assert!(!reuses(None, None, page(None, false)));
        assert!(!reuses(Some(ASKED), None, page(Some(200), true)));
        // Another address opened since.
        let other = "https://dl.acm.org/doi/pdf/10.1145/1";
        assert!(!reuses(Some(other), Some(LED_TO), page(Some(200), true)));
        // The check not passed yet starts over; an error is asked again.
        assert!(!reuses(Some(ASKED), Some(LED_TO), page(Some(403), true)));
        assert!(!reuses(Some(ASKED), Some(ASKED), page(Some(404), true)));
    }

    #[test]
    fn a_page_is_what_was_asked_for_once_loaded_with_a_2xx_status() {
        let page = |status, loaded| page(status, loaded).verdict();
        assert_eq!(page(Some(200), false), Verdict::Loading);
        assert_eq!(page(Some(200), true), Verdict::Content);
        assert_eq!(page(Some(204), true), Verdict::Content);
        // A document with no HTTP status: a file shown from elsewhere.
        assert_eq!(page(None, true), Verdict::Content);
        assert_eq!(page(Some(403), true), Verdict::Refused(403));
        assert_eq!(page(Some(404), true), Verdict::Refused(404));
        assert!(may_be_a_check(403) && may_be_a_check(503));
        assert!(!may_be_a_check(404) && !may_be_a_check(401));
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
