//! Feeds in a workspace: the eight operations, the refresh runner, and how
//! an item becomes a file.
//!
//! **The invariant.** One writer per workspace, one request at a time: a
//! refresh takes feeds in turn, every request any feed operation makes waits
//! at least [`REQUEST_SPACING`] after the previous one ended, and every await
//! is raced against a cancellation token, so Stop, a workspace switch or
//! shutdown ends a refresh within the three-second cancellation limit even in
//! the middle of a request. A refresh runs no model and starts no worker.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use wilkes_core::feeds::article::{self, SiteTemplate};
use wilkes_core::feeds::document::{self, Picture, Provenance};
use wilkes_core::feeds::identity::{self, link_identifiers, link_key, provider_key};
use wilkes_core::feeds::syndication::{offered_feeds, read_feed};
use wilkes_core::feeds::{FeedSource, SourceEntry};
use wilkes_core::network::{ProviderHttpClient, ProviderHttpError};
use wilkes_core::types::{BrowseRequest, LiteratureAcquisition, LiteratureSearchResult};

use super::AppContext;
use crate::research::feeds::FeedItemStatus;
use crate::research::{FeedItemRecord, FeedRecord};

/// The one event feeds raise: which feeds' status or items changed, and how
/// many items wait in the inbox.
pub const FEEDS_CHANGED_EVENT: &str = "feeds-changed";

/// Items a first refresh puts in the inbox; the rest are marked seen.
const FIRST_REFRESH_INBOX: usize = 10;
/// Items auto-import takes per feed per refresh, newest first.
const AUTO_IMPORT_CAP: usize = 20;
/// A listing is read this many results to a page...
const LISTING_PAGE_SIZE: usize = 50;
/// ...for at most this many pages, until a page holds an item already seen.
const LISTING_MAX_PAGES: usize = 5;
/// The least time between the end of one request and the start of the next.
const REQUEST_SPACING: Duration = Duration::from_secs(1);
/// How often the timer looks again when nothing is due sooner, so a changed
/// interval or a new feed is noticed.
const TIMER_RECHECK: Duration = Duration::from_secs(60 * 60);

// ── Requests and upkeep state ─────────────────────────────────────────────────

/// What has been asked of the runner and not yet taken.
#[derive(Default)]
pub(super) struct FeedRequests {
    /// Every feed: start, the timer, Refresh now, a feed added.
    all: bool,
    /// Learning-catalogue listings only: a catalogue sync finished.
    catalogue: bool,
}

impl FeedRequests {
    fn is_empty(&self) -> bool {
        !self.all && !self.catalogue
    }
}

/// The refresh runner's state, shaped like the text index's upkeep: requests
/// are merged, one runner drains them, and a cancel returns at once.
pub(super) struct FeedUpkeep {
    requests: parking_lot::Mutex<FeedRequests>,
    running: std::sync::atomic::AtomicBool,
    /// The token every feed operation in flight is raced against. Stop
    /// cancels it and puts a fresh one in its place.
    cancel: parking_lot::Mutex<CancellationToken>,
    /// Cancelled once, at shutdown: the parent of every `cancel` token, and
    /// what ends the timer.
    lifetime: CancellationToken,
    gate: RequestGate,
    timer_started: std::sync::atomic::AtomicBool,
    /// By host, the text of the last post page read from it and that page's
    /// link key: what the next post of the site is read against.
    templates: parking_lot::Mutex<HashMap<String, (String, Arc<SiteTemplate>)>>,
}

impl Default for FeedUpkeep {
    fn default() -> Self {
        let lifetime = CancellationToken::new();
        Self {
            requests: parking_lot::Mutex::new(FeedRequests::default()),
            running: std::sync::atomic::AtomicBool::new(false),
            cancel: parking_lot::Mutex::new(lifetime.child_token()),
            lifetime,
            gate: RequestGate::new(REQUEST_SPACING),
            timer_started: std::sync::atomic::AtomicBool::new(false),
            templates: parking_lot::Mutex::new(HashMap::new()),
        }
    }
}

/// A feed operation was stopped.
#[derive(Debug)]
pub(crate) struct Cancelled;

/// One request at a time, each starting at least `spacing` after the last
/// one ended, and each abandoned the moment its token is cancelled.
pub(crate) struct RequestGate {
    last_end: tokio::sync::Mutex<Option<Instant>>,
    spacing: Duration,
}

impl RequestGate {
    pub(crate) fn new(spacing: Duration) -> Self {
        Self {
            last_end: tokio::sync::Mutex::new(None),
            spacing,
        }
    }

    /// Run `request` when its turn comes. Dropped, not awaited, when `cancel`
    /// fires: an in-flight request is abandoned.
    pub(crate) async fn run<T>(
        &self,
        cancel: &CancellationToken,
        request: impl std::future::Future<Output = T>,
    ) -> Result<T, Cancelled> {
        let mut last_end = tokio::select! {
            _ = cancel.cancelled() => return Err(Cancelled),
            guard = self.last_end.lock() => guard,
        };
        if let Some(ended) = *last_end {
            let wait = (ended + self.spacing).saturating_duration_since(Instant::now());
            tokio::select! {
                _ = cancel.cancelled() => return Err(Cancelled),
                _ = tokio::time::sleep(wait) => {}
            }
        }
        let outcome = tokio::select! {
            _ = cancel.cancelled() => None,
            outcome = request => Some(outcome),
        };
        *last_end = Some(Instant::now());
        outcome.ok_or(Cancelled)
    }
}

/// Run `work` off the async runtime, raced against `cancel`. When cancelled,
/// the blocking work is not awaited; it finishes on its own thread, and every
/// store write it makes is a transaction of its own.
async fn blocking<T: Send + 'static>(
    cancel: &CancellationToken,
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<anyhow::Result<T>, Cancelled> {
    let task = tokio::task::spawn_blocking(work);
    tokio::select! {
        _ = cancel.cancelled() => Err(Cancelled),
        joined = task => Ok(joined.unwrap_or_else(|error| Err(anyhow::anyhow!("feed store task failed: {error}")))),
    }
}

/// An http(s) address, as typed.
fn web_address(address: &str) -> anyhow::Result<String> {
    let address = address.trim();
    url::Url::parse(address)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| url.to_string())
        .ok_or_else(|| anyhow::anyhow!("{address} is not a web address"))
}

/// A link's host, lowercased, as the template cache is keyed.
fn link_host(link: &str) -> Option<String> {
    url::Url::parse(link)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn http_client() -> ProviderHttpClient {
    ProviderHttpClient::new("feeds")
}

/// Sent with every page, feed and picture request. Some sites refuse a
/// request that names no client at all.
fn request_headers() -> [(&'static str, String); 1] {
    [(
        "User-Agent",
        format!("Wilkes/{} (feed reader)", env!("CARGO_PKG_VERSION")),
    )]
}

fn http_message(error: &ProviderHttpError) -> String {
    match error.status {
        Some(status) => format!("HTTP {status}"),
        None => error
            .message
            .lines()
            .next()
            .unwrap_or("request failed")
            .to_string(),
    }
}

// ── API types ─────────────────────────────────────────────────────────────────

/// A feed to add.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewFeed {
    /// For a syndication source, the address of the feed or of a page that
    /// advertises one.
    pub source: FeedSource,
    /// Defaulted from the feed's own title, or the provider's name.
    #[serde(default)]
    pub name: Option<String>,
    pub root: PathBuf,
    /// Under `root`, `/`-separated. Defaulted to `Feeds/<name>`.
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub auto_import: bool,
}

/// A change to a feed; an absent field is left as it is.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedUpdate {
    #[serde(default)]
    pub name: Option<String>,
    /// Where items imported from now on go; what is already imported stays
    /// where it is.
    #[serde(default)]
    pub root: Option<PathBuf>,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub auto_import: Option<bool>,
}

/// A feed an address offers, as its own document describes it.
#[derive(Clone, Debug, Serialize)]
pub struct OfferedFeed {
    pub url: String,
    /// The feed's own name for itself.
    pub title: Option<String>,
    /// How many items it lists now.
    pub entries: usize,
}

impl OfferedFeed {
    fn read(url: String, document: &wilkes_core::feeds::syndication::FeedDocument) -> Self {
        Self {
            url,
            title: document.title.clone(),
            entries: document.entries.len() + document.without_link,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedItemsQuery {
    #[serde(default)]
    pub status: Option<FeedItemStatus>,
    #[serde(default)]
    pub feed_id: Option<String>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// An item as the inbox shows it.
#[derive(Clone, Debug, Serialize)]
pub struct FeedItemView {
    #[serde(flatten)]
    pub item: FeedItemRecord,
    /// The library already holds this work, by DOI or arXiv id. Only files
    /// whose metadata has been read are known.
    pub held: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct FeedItemsPage {
    pub items: Vec<FeedItemView>,
    pub inbox_total: i64,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum FeedItemAction {
    /// Fetch the item and put it in its feed's folder. With
    /// `allow_missing_images`, a picture that cannot be fetched is left out
    /// instead of failing the item.
    Import {
        #[serde(default)]
        allow_missing_images: bool,
    },
    Dismiss,
}

#[derive(Clone, Debug, Serialize)]
pub struct FeedsChanged {
    pub feed_ids: Vec<String>,
    pub inbox: i64,
}

/// One item a refresh listed, ready to be compared with the seen record.
struct Listed {
    link_key: String,
    link: String,
    doi: Option<String>,
    arxiv_id: Option<String>,
    published: Option<DateTime<Utc>>,
    work: LiteratureSearchResult,
}

enum Listing {
    Items {
        items: Vec<Listed>,
        /// Set when the listing could not reach an item already seen.
        missed: bool,
    },
    Failed {
        message: String,
        retry_after: Option<Duration>,
    },
}

/// What importing one item came to.
enum Imported {
    File(PathBuf),
    /// Its page shows only the start of a paid post.
    PreviewOnly,
}

// ── Operations ────────────────────────────────────────────────────────────────

impl AppContext {
    fn feeds_store(&self) -> anyhow::Result<Arc<std::sync::Mutex<crate::research::ResearchStore>>> {
        self.research_store()
    }

    fn emit_feeds_changed(&self, feed_ids: Vec<String>) {
        let inbox = self
            .feeds_store()
            .and_then(|store| {
                store
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .inbox_total()
            })
            .unwrap_or_else(|error| {
                warn!("feeds: cannot count the inbox: {error:#}");
                0
            });
        self.events.emit(
            FEEDS_CHANGED_EVENT,
            serde_json::to_value(FeedsChanged { feed_ids, inbox }).unwrap_or_default(),
        );
    }

    /// Every feed, with its status and how many items wait in its inbox.
    pub fn feeds_list(&self) -> anyhow::Result<Vec<FeedRecord>> {
        self.feeds_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .feeds()
    }

    /// Add a feed and refresh it. A syndication source must be a feed itself:
    /// the feeds a page offers are looked up with [`Self::feed_discover`], and
    /// one of them chosen.
    pub async fn feed_add(self: &Arc<Self>, new: NewFeed) -> anyhow::Result<FeedRecord> {
        self.ensure_writable().map_err(anyhow::Error::msg)?;
        let settings = self.get_settings().await;
        let root = self.feed_root(&settings, &new.root)?;
        let cancel = self.feed_upkeep.cancel.lock().clone();
        let (source, title) = match new.source {
            FeedSource::Syndication { url } => {
                let url = web_address(&url)?;
                let body = self.fetch(&url, &cancel).await?;
                let document = read_feed(&body).map_err(|error| {
                    anyhow::anyhow!(
                        "{url} is not a feed ({error}); look up the feeds its page offers and choose one"
                    )
                })?;
                (FeedSource::Syndication { url }, document.title)
            }
            FeedSource::Listing {
                provider,
                mut request,
            } => {
                let info = crate::commands::integrations::literature::providers(
                    &settings.integrations,
                    Some(&self.catalogue_dir),
                )
                .into_iter()
                .find(|known| known.provider == provider)
                .ok_or_else(|| anyhow::anyhow!("no literature provider named {provider}"))?;
                let name = info.name.clone();
                request.sort = newest_first(&info)?;
                request.page = None;
                request.limit = None;
                let title = request
                    .query
                    .as_deref()
                    .filter(|query| !query.trim().is_empty())
                    .map(|query| format!("{name}: {}", query.trim()))
                    .unwrap_or(name);
                (FeedSource::Listing { provider, request }, Some(title))
            }
        };
        let name = new
            .name
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .or(title)
            .ok_or_else(|| anyhow::anyhow!("the feed names itself nothing; give it a name"))?;
        let folder = match new
            .folder
            .map(|folder| folder.trim().trim_matches('/').to_string())
        {
            Some(folder) if !folder.is_empty() => folder,
            _ => format!("Feeds/{}", folder_component(&name)),
        };
        check_folder(&folder)?;
        let feed = FeedRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            source,
            root,
            folder,
            auto_import: new.auto_import,
            created_at_ms: now_ms(),
            last_attempt_at_ms: None,
            last_success_at_ms: None,
            last_error: None,
            retry_after_ms: None,
            may_have_missed: false,
            inbox_count: 0,
        };
        self.feeds_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert_feed(&feed)?;
        self.emit_feeds_changed(vec![feed.id.clone()]);
        self.request_feed_refresh(|requests| requests.all = true);
        Ok(feed)
    }

    pub async fn feed_update(&self, id: &str, update: FeedUpdate) -> anyhow::Result<FeedRecord> {
        self.ensure_writable().map_err(anyhow::Error::msg)?;
        let name = update
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let folder = update
            .folder
            .as_deref()
            .map(|folder| folder.trim().trim_matches('/'))
            .filter(|folder| !folder.is_empty());
        if let Some(folder) = folder {
            check_folder(folder)?;
        }
        let root = match &update.root {
            Some(root) => Some(self.feed_root(&self.get_settings().await, root)?),
            None => None,
        };
        let feed = self
            .feeds_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .update_feed(id, name, root.as_deref(), folder, update.auto_import)?;
        self.emit_feeds_changed(vec![feed.id.clone()]);
        Ok(feed)
    }

    /// Remove a feed and its items. With `delete_files`, the files it
    /// imported are deleted too, wherever it put them — its root or folder may
    /// have changed since — and each folder they leave empty; nothing else is
    /// touched.
    pub async fn feed_remove(&self, id: &str, delete_files: bool) -> anyhow::Result<()> {
        self.ensure_writable().map_err(anyhow::Error::msg)?;
        let store = self.feeds_store()?;
        let imported: Vec<PathBuf> = {
            let store = store.lock().unwrap_or_else(|p| p.into_inner());
            let mut paths = Vec::new();
            let mut offset = 0;
            loop {
                let page =
                    store.feed_items(Some(FeedItemStatus::Imported), Some(id), offset, 500)?;
                if page.is_empty() {
                    break;
                }
                offset += page.len();
                paths.extend(page.into_iter().filter_map(|item| item.imported_path));
            }
            paths
        };
        let Some(feed) = store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove_feed(id)?
        else {
            anyhow::bail!("no feed with id {id}");
        };
        if delete_files {
            let (roots, _) = super::library_roots(&self.get_settings().await);
            let mut folders: Vec<PathBuf> = Vec::new();
            for path in imported {
                // Wherever the feed put it, but never outside the workspace's
                // library: a library removed from the workspace is not touched.
                if !roots.iter().any(|root| path.starts_with(root)) {
                    warn!(
                        "feeds: {} is not in this workspace's library folders; it was not deleted",
                        path.display()
                    );
                    continue;
                }
                match tokio::fs::remove_file(&path).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        warn!("feeds: could not delete {}: {error}", path.display());
                        continue;
                    }
                }
                if let Some(folder) = path.parent() {
                    if !roots.iter().any(|root| root == folder)
                        && !folders.iter().any(|held| held == folder)
                    {
                        folders.push(folder.to_path_buf());
                    }
                }
            }
            // Each folder the feed imported into, if that leaves it empty.
            for folder in folders {
                if let Ok(mut entries) = tokio::fs::read_dir(&folder).await {
                    if entries.next_entry().await.ok().flatten().is_none() {
                        if let Err(error) = tokio::fs::remove_dir(&folder).await {
                            warn!("feeds: could not remove {}: {error}", folder.display());
                        }
                    }
                }
            }
        }
        self.emit_feeds_changed(vec![feed.id]);
        Ok(())
    }

    /// Refresh every feed now.
    pub fn feeds_refresh(self: &Arc<Self>) -> anyhow::Result<()> {
        self.ensure_writable().map_err(anyhow::Error::msg)?;
        self.request_feed_refresh(|requests| requests.all = true);
        Ok(())
    }

    /// Stop the refresh in progress, and any import in flight.
    pub fn feeds_refresh_cancel(&self) {
        self.cancel_feed_upkeep();
    }

    /// Items by status and feed, newest first.
    pub fn feed_items(&self, query: FeedItemsQuery) -> anyhow::Result<FeedItemsPage> {
        let limit = query.limit.unwrap_or(50).clamp(1, 500);
        let store = self.feeds_store()?;
        let store = store.lock().unwrap_or_else(|p| p.into_inner());
        let items = store.feed_items(
            query.status,
            query.feed_id.as_deref(),
            query.offset.unwrap_or(0),
            limit,
        )?;
        let inbox_total = store.inbox_total()?;
        let dois: Vec<String> = items.iter().flat_map(item_dois).collect();
        let mut held = store.saved_work_dois(&dois)?;
        drop(store);
        if let Some(cache) = self.metadata_cache() {
            match cache.lock() {
                Ok(cache) => held.extend(cache.held_dois(&dois)?),
                Err(_) => warn!("feeds: the metadata cache is poisoned; held works are not marked"),
            }
        }
        Ok(FeedItemsPage {
            items: items
                .into_iter()
                .map(|item| FeedItemView {
                    held: item_dois(&item).iter().any(|doi| held.contains(doi)),
                    item,
                })
                .collect(),
            inbox_total,
        })
    }

    /// Import or dismiss one item.
    pub async fn feed_item_set_status(
        self: &Arc<Self>,
        id: &str,
        action: FeedItemAction,
    ) -> anyhow::Result<FeedItemRecord> {
        self.ensure_writable().map_err(anyhow::Error::msg)?;
        let store = self.feeds_store()?;
        let item = store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .feed_item(id)?
            .ok_or_else(|| anyhow::anyhow!("no feed item with id {id}"))?;
        match action {
            FeedItemAction::Dismiss => {
                store
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .set_feed_item_status(id, FeedItemStatus::Dismissed)?;
            }
            FeedItemAction::Import {
                allow_missing_images,
            } => {
                anyhow::ensure!(
                    item.status != FeedItemStatus::Imported,
                    "this item is already in the library"
                );
                anyhow::ensure!(
                    !item.preview_only,
                    "only a preview of this post is public; importing it would import the preview as the post"
                );
                let cancel = self.feed_upkeep.cancel.lock().clone();
                match self.import_item(&item, allow_missing_images, &cancel).await {
                    Err(Cancelled) => anyhow::bail!("the import was stopped"),
                    Ok(Err(error)) => {
                        self.emit_feeds_changed(vec![item.feed_id.clone()]);
                        return Err(error);
                    }
                    Ok(Ok(_)) => {}
                }
            }
        }
        self.emit_feeds_changed(vec![item.feed_id.clone()]);
        let updated = store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .feed_item(id)?;
        updated.ok_or_else(|| anyhow::anyhow!("no feed item with id {id}"))
    }

    /// The library root a feed writes under: one of the library's own.
    fn feed_root(
        &self,
        settings: &wilkes_core::types::Settings,
        root: &Path,
    ) -> anyhow::Result<PathBuf> {
        let canonical = std::fs::canonicalize(root).map_err(|error| {
            anyhow::anyhow!("library root {} is unavailable: {error}", root.display())
        })?;
        let (roots, _) = super::library_roots(settings);
        anyhow::ensure!(
            roots.iter().any(|listed| canonical.starts_with(listed)),
            "{} is not one of this workspace's library folders",
            root.display()
        );
        Ok(canonical)
    }

    /// The feeds an address offers, each fetched and read to be sure it is
    /// one: the address itself when it is a feed, or else those its page
    /// advertises or links (see [`offered_feeds`]). Refused when there are
    /// none, with what to do instead.
    pub async fn feed_discover(&self, address: &str) -> anyhow::Result<Vec<OfferedFeed>> {
        let url = web_address(address)?;
        let cancel = self.feed_upkeep.cancel.lock().clone();
        let body = self.fetch(&url, &cancel).await?;
        if let Ok(document) = read_feed(&body) {
            return Ok(vec![OfferedFeed::read(url, &document)]);
        }
        let candidates = offered_feeds(&String::from_utf8_lossy(&body), &url);
        let mut offered = Vec::new();
        for candidate in candidates {
            match self.fetch(&candidate, &cancel).await {
                Ok(body) => match read_feed(&body) {
                    Ok(document) => offered.push(OfferedFeed::read(candidate, &document)),
                    Err(error) => {
                        info!("feeds: {candidate}, linked from {url}, is not a feed: {error}")
                    }
                },
                Err(error) => {
                    if cancel.is_cancelled() {
                        return Err(error);
                    }
                    warn!("feeds: {candidate}, linked from {url}, could not be read: {error:#}");
                }
            }
        }
        anyhow::ensure!(
            !offered.is_empty(),
            "{url} is neither a feed nor a page that offers one. To follow a page's links, \
             add the page as a custom provider and follow its listing."
        );
        Ok(offered)
    }

    /// One GET through the gate.
    async fn fetch(&self, url: &str, cancel: &CancellationToken) -> anyhow::Result<Vec<u8>> {
        let client = http_client();
        let headers = request_headers();
        match self
            .feed_upkeep
            .gate
            .run(cancel, client.get_bytes(url.to_string(), &headers))
            .await
        {
            Err(Cancelled) => anyhow::bail!("stopped"),
            Ok(Err(error)) => anyhow::bail!("{url}: {}", http_message(&error)),
            Ok(Ok(body)) => Ok(body),
        }
    }
}

/// The sort a followed listing is read in: the provider's newest-first, so
/// what is new comes first and a refresh can stop at what it has seen. A
/// listing with no sort at all — a page of links read through a custom
/// manifest — is read in its own order. A provider that sorts but cannot
/// sort newest first cannot be followed.
fn newest_first(
    info: &crate::commands::integrations::literature::LiteratureProviderInfo,
) -> anyhow::Result<Option<String>> {
    let Some(browse) = info.capabilities.browse.as_ref() else {
        anyhow::bail!(
            "{} cannot list works by filter, so there is no listing to follow",
            info.name
        );
    };
    let Some(sort) = browse.sort.as_ref() else {
        return Ok(None);
    };
    sort.equivalent_to
        .iter()
        .find(|(_, canonical)| canonical.as_str() == "newest")
        .map(|(value, _)| Some(value.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} cannot list its works newest first, so a refresh could not tell what is new",
                info.name
            )
        })
}

/// The DOIs an item is known by: its own, and its arXiv id's.
fn item_dois(item: &FeedItemRecord) -> Vec<String> {
    let mut dois = Vec::new();
    if let Some(doi) = &item.doi {
        dois.push(doi.clone());
    }
    if let Some(id) = &item.arxiv_id {
        if let Some(doi) =
            wilkes_core::metadata::doi::normalize_doi(&format!("10.48550/arXiv.{id}"))
        {
            dois.push(doi);
        }
    }
    dois
}

/// A feed's name as one folder name.
fn folder_component(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = cleaned.trim_matches('.').trim().to_string();
    if cleaned.is_empty() {
        "Feed".to_string()
    } else {
        cleaned.chars().take(80).collect()
    }
}

/// The folder rule the import step applies to every component.
fn check_folder(folder: &str) -> anyhow::Result<()> {
    for component in folder.split('/') {
        crate::commands::files::validate_new_file_name(component)
            .map_err(|error| anyhow::anyhow!("the folder {folder:?} is not usable: {error}"))?;
    }
    Ok(())
}

// ── The runner ────────────────────────────────────────────────────────────────

impl AppContext {
    /// Queue a refresh, and start the runner unless one is running.
    pub(super) fn request_feed_refresh(self: &Arc<Self>, add: impl FnOnce(&mut FeedRequests)) {
        if self.shutting_down.load(Ordering::Acquire) || self.is_application_managed() {
            return;
        }
        add(&mut self.feed_upkeep.requests.lock());
        if self.feed_upkeep.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let ctx = Arc::clone(self);
        tokio::spawn(async move { ctx.run_feed_upkeep().await });
    }

    /// Refresh the learning catalogues' listings, after a catalogue sync.
    pub(crate) fn request_catalogue_feed_refresh(self: &Arc<Self>) {
        self.request_feed_refresh(|requests| requests.catalogue = true);
    }

    /// Empty the queue and cancel everything in flight. Returns at once.
    pub(super) fn cancel_feed_upkeep(&self) {
        *self.feed_upkeep.requests.lock() = FeedRequests::default();
        let mut cancel = self.feed_upkeep.cancel.lock();
        cancel.cancel();
        *cancel = self.feed_upkeep.lifetime.child_token();
    }

    /// End the timer and everything in flight, for good.
    pub(super) fn shut_down_feeds(&self) {
        self.cancel_feed_upkeep();
        self.feed_upkeep.lifetime.cancel();
    }

    async fn run_feed_upkeep(self: Arc<Self>) {
        loop {
            let requests = std::mem::take(&mut *self.feed_upkeep.requests.lock());
            if requests.is_empty() {
                self.feed_upkeep.running.store(false, Ordering::Release);
                if self.feed_upkeep.requests.lock().is_empty()
                    || self.feed_upkeep.running.swap(true, Ordering::AcqRel)
                {
                    return;
                }
                continue;
            }
            self.feed_pass(requests).await;
        }
    }

    /// The learning catalogues: listings of these change only when they sync.
    fn catalogue_providers(&self) -> HashSet<String> {
        use wilkes_core::integrations::LiteratureSource as _;
        wilkes_core::catalogue::mirrored::mirrored_sources(&self.catalogue_dir)
            .iter()
            .map(|source| source.id().to_string())
            .collect()
    }

    async fn feed_pass(self: &Arc<Self>, requests: FeedRequests) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let cancel = self.feed_upkeep.cancel.lock().clone();
        let feeds = match self.feeds_list() {
            Ok(feeds) => feeds,
            Err(error) => {
                warn!("feeds: cannot read the feeds: {error:#}");
                return;
            }
        };
        let catalogues = self.catalogue_providers();
        let started = Instant::now();
        let mut refreshed = 0usize;
        for feed in feeds {
            let wanted = requests.all
                || matches!(&feed.source, FeedSource::Listing { provider, .. } if catalogues.contains(provider));
            if !wanted {
                continue;
            }
            if cancel.is_cancelled() {
                break;
            }
            match self.refresh_feed(&feed, &cancel).await {
                Ok(()) => refreshed += 1,
                Err(Cancelled) => {
                    info!("feeds: refresh stopped during {}", feed.name);
                    self.emit_feeds_changed(vec![feed.id.clone()]);
                    break;
                }
            }
            self.emit_feeds_changed(vec![feed.id.clone()]);
        }
        info!(
            "feeds: refreshed {refreshed} feed(s) in {:.1}s",
            started.elapsed().as_secs_f64()
        );
    }

    /// Refresh one feed: list it, store what is new, and auto-import when its
    /// switch is on. A failure is recorded on the feed; nothing stored is
    /// deleted.
    async fn refresh_feed(
        self: &Arc<Self>,
        feed: &FeedRecord,
        cancel: &CancellationToken,
    ) -> Result<(), Cancelled> {
        let now = now_ms();
        if let Some(until) = feed.retry_after_ms.filter(|until| *until > now) {
            info!(
                "feeds: {} asked to be left alone for another {}s",
                feed.name,
                (until - now) / 1000
            );
            return Ok(());
        }
        let store = match self.feeds_store() {
            Ok(store) => store,
            Err(error) => {
                warn!("feeds: {error:#}");
                return Ok(());
            }
        };
        let feed_id = feed.id.clone();
        let attempted = {
            let store = Arc::clone(&store);
            blocking(cancel, move || {
                store
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .feed_attempted(&feed_id, now)
            })
            .await?
        };
        if let Err(error) = attempted {
            warn!("feeds: {error:#}");
            return Ok(());
        }

        let known = {
            let store = Arc::clone(&store);
            let feed_id = feed.id.clone();
            match blocking(cancel, move || {
                store
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .feed_item_keys(&feed_id)
            })
            .await?
            {
                Ok(known) => known,
                Err(error) => {
                    warn!("feeds: {error:#}");
                    return Ok(());
                }
            }
        };
        let first_refresh = known.is_empty() && feed.last_success_at_ms.is_none();

        let listing = match &feed.source {
            FeedSource::Syndication { url } => self.list_syndication(feed, url, cancel).await?,
            FeedSource::Listing { provider, request } => {
                self.list_provider(provider, request, &known, first_refresh, cancel)
                    .await?
            }
        };
        let (listed, missed) = match listing {
            Listing::Items { items, missed } => (items, missed),
            Listing::Failed {
                message,
                retry_after,
            } => {
                warn!("feeds: {} failed: {message}", feed.name);
                let retry_after_ms = retry_after.map(|delay| now + delay.as_millis() as i64);
                let store = Arc::clone(&store);
                let feed_id = feed.id.clone();
                let _ = blocking(cancel, move || {
                    store.lock().unwrap_or_else(|p| p.into_inner()).feed_failed(
                        &feed_id,
                        &message,
                        retry_after_ms,
                    )
                })
                .await?;
                return Ok(());
            }
        };

        // New is "not in the seen record": dates never decide it. A source
        // that lists one link twice lists one item.
        let mut seen_in_listing = HashSet::new();
        let mut new: Vec<Listed> = Vec::new();
        let mut any_seen = false;
        for item in listed {
            if known.contains(&item.link_key) {
                any_seen = true;
            } else if seen_in_listing.insert(item.link_key.clone()) {
                new.push(item);
            }
        }
        // Newest first; an item with no date keeps its place after the dated.
        new.sort_by(|a, b| b.published.cmp(&a.published));
        let may_have_missed = match &feed.source {
            FeedSource::Syndication { .. } => {
                feed.last_success_at_ms.is_some() && !new.is_empty() && !any_seen
            }
            FeedSource::Listing { .. } => missed,
        };
        if may_have_missed {
            info!(
                "feeds: every item {} returned was new; older ones may have been missed",
                feed.name
            );
        }

        let records: Vec<FeedItemRecord> = new
            .into_iter()
            .enumerate()
            .map(|(index, item)| FeedItemRecord {
                id: uuid::Uuid::new_v4().to_string(),
                feed_id: feed.id.clone(),
                link_key: item.link_key,
                link: item.link,
                doi: item.doi,
                arxiv_id: item.arxiv_id,
                published: item
                    .published
                    .map(|date| date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                first_seen_at_ms: now,
                status: if first_refresh && index >= FIRST_REFRESH_INBOX {
                    FeedItemStatus::Seen
                } else {
                    FeedItemStatus::Inbox
                },
                preview_only: false,
                work: item.work,
                imported_path: None,
                last_error: None,
            })
            .collect();
        let undated = records
            .iter()
            .filter(|item| item.published.is_none())
            .count();
        if undated > 0 {
            info!(
                "feeds: {} gave {undated} item(s) no real date; they are shown by when they were first seen",
                feed.name
            );
        }
        let stored = {
            let store = Arc::clone(&store);
            let records = records.clone();
            let feed_id = feed.id.clone();
            blocking(cancel, move || {
                let mut store = store.lock().unwrap_or_else(|p| p.into_inner());
                for record in &records {
                    store.insert_feed_item(record)?;
                }
                store.feed_succeeded(&feed_id, now, may_have_missed)
            })
            .await?
        };
        if let Err(error) = stored {
            warn!("feeds: storing {}'s items: {error:#}", feed.name);
            return Ok(());
        }
        self.emit_feeds_changed(vec![feed.id.clone()]);

        if feed.auto_import {
            let inbox: Vec<&FeedItemRecord> = records
                .iter()
                .filter(|item| item.status == FeedItemStatus::Inbox)
                .take(AUTO_IMPORT_CAP)
                .collect();
            for item in inbox {
                match self.import_item(item, false, cancel).await? {
                    Ok(Imported::File(path)) => {
                        info!("feeds: imported {}", path.display())
                    }
                    Ok(Imported::PreviewOnly) => {}
                    Err(error) => warn!("feeds: {} stays in the inbox: {error:#}", item.link),
                }
                self.emit_feeds_changed(vec![feed.id.clone()]);
            }
        }
        Ok(())
    }

    async fn list_syndication(
        &self,
        feed: &FeedRecord,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<Listing, Cancelled> {
        let client = http_client();
        let headers = request_headers();
        let body = match self
            .feed_upkeep
            .gate
            .run(cancel, client.get_bytes(url.to_string(), &headers))
            .await?
        {
            Ok(body) => body,
            Err(error) => {
                return Ok(Listing::Failed {
                    message: format!("{url}: {}", http_message(&error)),
                    retry_after: error.retry_after,
                })
            }
        };
        let document = match read_feed(&body) {
            Ok(document) => document,
            Err(error) => {
                return Ok(Listing::Failed {
                    message: error.to_string(),
                    retry_after: None,
                })
            }
        };
        if document.without_link > 0 {
            warn!(
                "feeds: {} listed {} item(s) with no link; they cannot be fetched and were left out",
                feed.name, document.without_link
            );
        }
        let mut items = Vec::with_capacity(document.entries.len());
        for entry in document.entries {
            match listed_entry(&feed.name, entry) {
                Some(item) => items.push(item),
                None => continue,
            }
        }
        Ok(Listing::Items {
            items,
            missed: false,
        })
    }

    async fn list_provider(
        &self,
        provider: &str,
        request: &BrowseRequest,
        known: &HashSet<String>,
        first_refresh: bool,
        cancel: &CancellationToken,
    ) -> Result<Listing, Cancelled> {
        let settings = self.get_settings().await;
        let pages = if first_refresh { 1 } else { LISTING_MAX_PAGES };
        let mut items = Vec::new();
        for page in 1..=pages {
            let mut request = request.clone();
            request.page = Some(page);
            request.limit = Some(LISTING_PAGE_SIZE);
            let answer = self
                .feed_upkeep
                .gate
                .run(
                    cancel,
                    crate::commands::integrations::literature::browse(
                        &settings.integrations,
                        Some(&self.catalogue_dir),
                        provider.to_string(),
                        request,
                    ),
                )
                .await?;
            let answer = match answer {
                Ok(answer) => answer,
                Err(error) => {
                    return Ok(Listing::Failed {
                        message: error.to_string(),
                        retry_after: None,
                    })
                }
            };
            if let Some(error) = answer.error {
                return Ok(Listing::Failed {
                    message: error,
                    retry_after: None,
                });
            }
            let results = answer.results.unwrap_or_default();
            let count = results.len();
            let mut reached_known = false;
            for work in results {
                let item = listed_work(provider, work);
                reached_known |= known.contains(&item.link_key);
                items.push(item);
            }
            if reached_known || count < LISTING_PAGE_SIZE {
                return Ok(Listing::Items {
                    items,
                    missed: false,
                });
            }
        }
        Ok(Listing::Items {
            items,
            missed: !first_refresh,
        })
    }

    /// Import one item. `Err(Cancelled)` when stopped; an import failure is
    /// recorded on the item, which stays where it was.
    async fn import_item(
        self: &Arc<Self>,
        item: &FeedItemRecord,
        allow_missing_images: bool,
        cancel: &CancellationToken,
    ) -> Result<anyhow::Result<Imported>, Cancelled> {
        let store = match self.feeds_store() {
            Ok(store) => store,
            Err(error) => return Ok(Err(error)),
        };
        let feed = match store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .feed(&item.feed_id)
        {
            Ok(Some(feed)) => feed,
            Ok(None) => return Ok(Err(anyhow::anyhow!("its feed was removed"))),
            Err(error) => return Ok(Err(error)),
        };
        let outcome = match &feed.source {
            FeedSource::Listing { provider, .. } => {
                self.import_provider_item(&feed, provider, item, cancel)
                    .await?
            }
            FeedSource::Syndication { .. } => {
                self.import_post(&feed, item, allow_missing_images, cancel)
                    .await?
            }
        };
        let id = item.id.clone();
        let written = match &outcome {
            Ok(Imported::File(path)) => {
                let path = path.clone();
                blocking(cancel, move || {
                    store
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .feed_item_imported(&id, &path)
                })
                .await?
            }
            Ok(Imported::PreviewOnly) => {
                blocking(cancel, move || {
                    store
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .feed_item_preview_only(&id)
                })
                .await?
            }
            Err(error) => {
                let message = format!("{error:#}");
                blocking(cancel, move || {
                    store
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .feed_item_failed(&id, &message)
                })
                .await?
            }
        };
        if let Err(error) = written {
            warn!("feeds: recording {}: {error:#}", item.link);
        }
        Ok(outcome)
    }

    async fn import_provider_item(
        &self,
        feed: &FeedRecord,
        provider: &str,
        item: &FeedItemRecord,
        cancel: &CancellationToken,
    ) -> Result<anyhow::Result<Imported>, Cancelled> {
        let settings = self.get_settings().await;
        let staging = self.data_dir.join("uploads");
        let acquired = self
            .feed_upkeep
            .gate
            .run(
                cancel,
                crate::commands::integrations::literature::acquire(
                    &settings.integrations,
                    Some(&self.catalogue_dir),
                    &staging,
                    settings.max_download_bytes,
                    None,
                    provider.to_string(),
                    item.work.clone(),
                ),
            )
            .await?;
        let acquired = match acquired {
            Ok(acquired) => acquired,
            Err(error) => return Ok(Err(anyhow::anyhow!("{error}"))),
        };
        let paths: Vec<PathBuf> = acquired
            .files
            .iter()
            .map(|file| PathBuf::from(&file.path))
            .collect();
        let imported = crate::commands::files::import_files_into_root(
            paths,
            feed.root.clone(),
            Some(feed.folder.clone()),
            settings.supported_extensions.clone(),
            crate::commands::files::FileImportMode::Move,
        )
        .await;
        Ok(imported.and_then(|imported| {
            imported
                .into_iter()
                .next()
                .map(Imported::File)
                .ok_or_else(|| anyhow::anyhow!("the provider's download held no file"))
        }))
    }

    /// What the post's site puts on every page, read from another of its
    /// posts: the last one read from the same host, or else the newest other
    /// item of the feed on that host, fetched for it. `None` when the feed
    /// holds no other post of the site — there is nothing to compare with,
    /// and the article is kept as Readability found it.
    async fn site_template(
        &self,
        feed: &FeedRecord,
        item: &FeedItemRecord,
        cancel: &CancellationToken,
    ) -> Result<anyhow::Result<Option<Arc<SiteTemplate>>>, Cancelled> {
        let Some(host) = link_host(&item.link) else {
            return Ok(Err(anyhow::anyhow!("{} has no host", item.link)));
        };
        if let Some((key, template)) = self.feed_upkeep.templates.lock().get(&host) {
            if *key != item.link_key {
                return Ok(Ok(Some(Arc::clone(template))));
            }
        }
        let other = match self.feeds_store().and_then(|store| {
            let store = store.lock().unwrap_or_else(|p| p.into_inner());
            let mut offset = 0;
            loop {
                let page = store.feed_items(None, Some(&feed.id), offset, 200)?;
                if page.is_empty() {
                    return Ok(None);
                }
                offset += page.len();
                if let Some(other) = page.into_iter().find(|other| {
                    other.link_key != item.link_key
                        && link_host(&other.link).as_deref() == Some(host.as_str())
                }) {
                    return Ok(Some(other));
                }
            }
        }) {
            Ok(Some(other)) => other,
            Ok(None) => {
                info!(
                    "feeds: {} is the only post of {host} in {}; it is kept as extracted",
                    item.link, feed.name
                );
                return Ok(Ok(None));
            }
            Err(error) => return Ok(Err(error)),
        };
        let page = match self
            .feed_upkeep
            .gate
            .run(
                cancel,
                http_client().get_bytes(other.link.clone(), &request_headers()),
            )
            .await?
        {
            Ok(page) => String::from_utf8_lossy(&page).into_owned(),
            Err(error) => {
                return Ok(Err(anyhow::anyhow!(
                    "{}, the post its site's layout is read from, could not be fetched: {}",
                    other.link,
                    http_message(&error)
                )))
            }
        };
        let read = tokio::task::spawn_blocking(move || SiteTemplate::from_page(&page));
        let template = tokio::select! {
            _ = cancel.cancelled() => return Err(Cancelled),
            joined = read => match joined {
                Ok(template) => Arc::new(template),
                Err(error) => return Ok(Err(anyhow::anyhow!("reading {}: {error}", other.link))),
            },
        };
        self.feed_upkeep
            .templates
            .lock()
            .insert(host, (other.link_key, Arc::clone(&template)));
        Ok(Ok(Some(template)))
    }

    /// A post: its page, its article, its pictures, one file.
    async fn import_post(
        &self,
        feed: &FeedRecord,
        item: &FeedItemRecord,
        allow_missing_images: bool,
        cancel: &CancellationToken,
    ) -> Result<anyhow::Result<Imported>, Cancelled> {
        let client = http_client();
        let headers = request_headers();
        let page = match self
            .feed_upkeep
            .gate
            .run(cancel, client.get_bytes(item.link.clone(), &headers))
            .await?
        {
            Ok(page) => String::from_utf8_lossy(&page).into_owned(),
            Err(error) => {
                return Ok(Err(anyhow::anyhow!(
                    "its page could not be fetched: {}",
                    http_message(&error)
                )))
            }
        };
        if identity::is_preview_only(&page) {
            return Ok(Ok(Imported::PreviewOnly));
        }
        let template = match self.site_template(feed, item, cancel).await? {
            Ok(template) => template,
            Err(error) => return Ok(Err(error)),
        };
        let extracted = {
            let link = item.link.clone();
            tokio::task::spawn_blocking(move || article::extract(&page, &link, template.as_deref()))
        };
        let extracted = tokio::select! {
            _ = cancel.cancelled() => return Err(Cancelled),
            joined = extracted => joined,
        };
        let article = match extracted {
            Ok(Ok(article)) => article,
            Ok(Err(error)) => return Ok(Err(anyhow::anyhow!("{error}"))),
            Err(error) => return Ok(Err(anyhow::anyhow!("extraction failed: {error}"))),
        };
        if let Some(host) = link_host(&item.link) {
            self.feed_upkeep.templates.lock().insert(
                host,
                (
                    item.link_key.clone(),
                    Arc::new(article.site_template.clone()),
                ),
            );
        }

        let mut pictures: HashMap<String, Picture> = HashMap::new();
        for address in &article.pictures {
            let fetched = self
                .feed_upkeep
                .gate
                .run(cancel, client.get_bytes(address.clone(), &headers))
                .await?;
            let failure = match fetched {
                Ok(bytes) => match document::picture_mime(&bytes) {
                    Some(mime) => {
                        pictures.insert(address.clone(), Picture { mime, bytes });
                        continue;
                    }
                    None => format!("the picture {address} is not PNG, JPEG, GIF, WebP or SVG"),
                },
                Err(error) => format!(
                    "the picture {address} could not be fetched: {}",
                    http_message(&error)
                ),
            };
            if allow_missing_images {
                warn!("feeds: {failure}; left out of {}", item.link);
            } else {
                return Ok(Err(anyhow::anyhow!(failure)));
            }
        }

        let settings = self.get_settings().await;
        // The source's values first; the page fills what the source left
        // empty, and never overrides it.
        let published = item
            .published
            .as_deref()
            .and_then(|date| DateTime::parse_from_rfc3339(date).ok())
            .map(|date| date.with_timezone(&Utc))
            .or_else(|| {
                article
                    .published
                    .as_deref()
                    .and_then(|date| DateTime::parse_from_rfc3339(date).ok())
                    .map(|date| date.with_timezone(&Utc))
                    .and_then(|date| identity::publication_date(Some(date), None))
            });
        let title = item.work.title.clone().or(article.title.clone());
        let provenance = Provenance {
            title: title.clone(),
            author: item.work.authors.clone().or(article.byline.clone()),
            link: item.link.clone(),
            published,
            feed: feed.name.clone(),
            fetched: Utc::now(),
            item_id: item.id.clone(),
        };
        let html = document::render(
            &provenance,
            &document::embed_pictures(&article.html, &pictures),
        );
        if settings.max_file_size > 0 && html.len() as u64 > settings.max_file_size {
            return Ok(Err(anyhow::anyhow!(
                "the post with its pictures is {:.1} MB, over the {:.1} MB limit above which files are not indexed",
                html.len() as f64 / 1_048_576.0,
                settings.max_file_size as f64 / 1_048_576.0
            )));
        }

        let date = published.unwrap_or_else(|| {
            Utc.timestamp_millis_opt(item.first_seen_at_ms)
                .single()
                .unwrap_or_else(Utc::now)
        });
        let mut name = document::file_name(date, title.as_deref(), "Post");
        if feed.root.join(&feed.folder).join(&name).exists() {
            name = document::disambiguated(&name, &item.id);
        }
        // Beside uploads, never in it: the server serves uploads as its
        // library, and a file staged there would be in the library before it
        // was imported into it.
        let staging = self.data_dir.join("feed-staging").join(&item.id);
        let written = async {
            tokio::fs::create_dir_all(&staging).await?;
            let path = staging.join(&name);
            tokio::fs::write(&path, html.as_bytes()).await?;
            let imported = crate::commands::files::import_files_into_root(
                vec![path],
                feed.root.clone(),
                Some(feed.folder.clone()),
                settings.supported_extensions.clone(),
                crate::commands::files::FileImportMode::Move,
            )
            .await?;
            imported
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("the import step returned no file"))
        }
        .await;
        if let Err(error) = tokio::fs::remove_dir_all(&staging).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!("feeds: could not clear {}: {error}", staging.display());
            }
        }
        Ok(written.map(Imported::File))
    }

    // ── The timer ────────────────────────────────────────────────────────────

    /// Start the workspace's timer, once. It refreshes every feed when the
    /// newest refresh is older than `feeds_refresh_hours` — at start too,
    /// which is the catch-up — and ends with the context. It holds the
    /// context weakly, so a retired workspace is not kept alive by it.
    pub(super) fn start_feed_timer(self: &Arc<Self>) {
        if self.is_application_managed()
            || self.feed_upkeep.timer_started.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let weak = Arc::downgrade(self);
        let lifetime = self.feed_upkeep.lifetime.clone();
        tokio::spawn(async move {
            let mut requested_at_ms: Option<i64> = None;
            loop {
                let wait = {
                    let Some(ctx) = weak.upgrade() else { break };
                    match ctx.feed_timer_due(requested_at_ms).await {
                        Some(wait) if wait.is_zero() => {
                            ctx.request_feed_refresh(|requests| requests.all = true);
                            requested_at_ms = Some(now_ms());
                            continue;
                        }
                        Some(wait) => wait.min(TIMER_RECHECK),
                        None => TIMER_RECHECK,
                    }
                };
                tokio::select! {
                    _ = lifetime.cancelled() => break,
                    _ = tokio::time::sleep(wait) => {}
                }
            }
        });
    }

    /// How long until every feed is due, or `None` when there are no feeds.
    async fn feed_timer_due(&self, requested_at_ms: Option<i64>) -> Option<Duration> {
        let hours = self.get_settings().await.feeds_refresh_hours.max(1);
        let feeds = match self.feeds_list() {
            Ok(feeds) => feeds,
            Err(error) => {
                warn!("feeds: the timer cannot read the feeds: {error:#}");
                return None;
            }
        };
        if feeds.is_empty() {
            return None;
        }
        let last = feeds
            .iter()
            .filter_map(|feed| feed.last_attempt_at_ms)
            .max()
            .max(requested_at_ms);
        let Some(last) = last else {
            return Some(Duration::ZERO);
        };
        let due = last + (hours as i64) * 3_600_000;
        Some(Duration::from_millis((due - now_ms()).max(0) as u64))
    }
}

/// A feed entry as an item, or `None` when its link cannot be fetched.
fn listed_entry(feed_name: &str, entry: SourceEntry) -> Option<Listed> {
    let Some(key) = link_key(&entry.link) else {
        warn!(
            "feeds: {feed_name} listed {:?}, which is not a web address; left out",
            entry.link
        );
        return None;
    };
    let (doi, arxiv_id) = link_identifiers(&entry.link);
    let work = LiteratureSearchResult {
        id: key.clone(),
        doi: doi.clone(),
        title: entry.title,
        year: entry
            .published
            .map(|date| i64::from(chrono::Datelike::year(&date))),
        publication_date: entry
            .published
            .map(|date| date.format("%Y-%m-%d").to_string()),
        venue: Some(feed_name.to_string()),
        citation_count: None,
        is_open_access: false,
        pdf_url: None,
        landing_page_url: Some(entry.link.clone()),
        image_url: entry.image_url,
        open_access_status: None,
        license: None,
        authors: entry.author,
        publisher: None,
        language: None,
        file_format: Some("html".to_string()),
        file_size: None,
        summary: None,
        subject: None,
        kind: Some("post".to_string()),
        pages: None,
        recall_score: None,
        acquisition: LiteratureAcquisition::None,
    };
    Some(Listed {
        link_key: key,
        link: entry.link,
        doi,
        arxiv_id,
        published: entry.published,
        work,
    })
}

/// A provider's result as an item: identified by its web address when it has
/// one, and by the provider's own id when it has none.
fn listed_work(provider: &str, work: LiteratureSearchResult) -> Listed {
    let doi = work
        .doi
        .as_deref()
        .and_then(wilkes_core::metadata::doi::normalize_doi);
    let link = work
        .landing_page_url
        .clone()
        .or_else(|| doi.as_ref().map(|doi| format!("https://doi.org/{doi}")))
        .or_else(|| work.pdf_url.clone());
    let key = link
        .as_deref()
        .and_then(link_key)
        .unwrap_or_else(|| provider_key(provider, &work.id));
    let arxiv_id = link
        .as_deref()
        .and_then(|link| link_identifiers(link).1)
        .or_else(|| {
            doi.as_deref()
                .and_then(|doi| doi.strip_prefix("10.48550/arxiv."))
                .map(str::to_string)
        });
    let published = work
        .publication_date
        .as_deref()
        .and_then(|date| chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| Utc.from_utc_datetime(&date))
        .and_then(|date| identity::publication_date(Some(date), None));
    Listed {
        link_key: key.clone(),
        link: link.unwrap_or(key),
        doi,
        arxiv_id,
        published,
        work,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::EventEmitter;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct Quiet;
    impl EventEmitter for Quiet {
        fn emit(&self, _name: &str, _payload: serde_json::Value) {}
    }

    /// What the local server answers for one path.
    #[derive(Clone)]
    enum Reply {
        Ok(&'static str, Vec<u8>),
        Status(u16),
        /// Accept the request and never answer it.
        Hang,
    }

    /// A local server: a reply per path, set at any time, and when each
    /// request arrived and was answered.
    struct Server {
        base: String,
        routes: Arc<Mutex<HashMap<String, Reply>>>,
        log: Arc<Mutex<Vec<(String, Instant, Instant)>>>,
        most_in_flight: Arc<AtomicUsize>,
    }

    impl Server {
        fn set(&self, path: &str, reply: Reply) {
            self.routes.lock().unwrap().insert(path.to_string(), reply);
        }

        fn url(&self, path: &str) -> String {
            format!("{}{path}", self.base)
        }
    }

    async fn serve() -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Arc<Mutex<HashMap<String, Reply>>> = Arc::default();
        let log: Arc<Mutex<Vec<(String, Instant, Instant)>>> = Arc::default();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let most_in_flight = Arc::new(AtomicUsize::new(0));
        {
            let (routes, log, most_in_flight) = (
                Arc::clone(&routes),
                Arc::clone(&log),
                Arc::clone(&most_in_flight),
            );
            tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    let (routes, log, in_flight, most_in_flight) = (
                        Arc::clone(&routes),
                        Arc::clone(&log),
                        Arc::clone(&in_flight),
                        Arc::clone(&most_in_flight),
                    );
                    tokio::spawn(async move {
                        let arrived = Instant::now();
                        let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        most_in_flight.fetch_max(now, Ordering::SeqCst);
                        let mut request = Vec::new();
                        let mut buffer = [0u8; 4096];
                        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                            match socket.read(&mut buffer).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => request.extend_from_slice(&buffer[..n]),
                            }
                        }
                        let head = String::from_utf8_lossy(&request);
                        let target = head.split_whitespace().nth(1).unwrap_or("/");
                        let path = target.split('?').next().unwrap_or("/").to_string();
                        let reply = routes.lock().unwrap().get(&path).cloned();
                        let (status, kind, body) = match reply {
                            Some(Reply::Ok(kind, body)) => (200, kind, body),
                            Some(Reply::Status(status)) => (status, "text/plain", b"no".to_vec()),
                            Some(Reply::Hang) => {
                                tokio::time::sleep(Duration::from_secs(120)).await;
                                return;
                            }
                            None => (404, "text/plain", b"no".to_vec()),
                        };
                        // Some time on the wire, so two requests at once would
                        // show as two in flight.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        let head = format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(head.as_bytes()).await;
                        let _ = socket.write_all(&body).await;
                        let _ = socket.shutdown().await;
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                        log.lock().unwrap().push((path, arrived, Instant::now()));
                    });
                }
            });
        }
        Server {
            base,
            routes,
            log,
            most_in_flight,
        }
    }

    async fn context(root: &Path) -> (tempfile::TempDir, Arc<AppContext>) {
        let dir = tempfile::tempdir().unwrap();
        let paths = wilkes_core::worker::manager::WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            uv_bin: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: dir.path().to_path_buf(),
        };
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            paths,
            Arc::new(Quiet),
        );
        ctx.update_settings(serde_json::json!({ "last_directory": root }))
            .await
            .unwrap();
        (dir, ctx)
    }

    fn rss(server: &Server, items: &[(String, Option<String>)]) -> Reply {
        let mut feed = String::from(
            "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>Blog</title><link>https://example.org</link><description>d</description>",
        );
        for (path, date) in items {
            feed.push_str(&format!(
                "<item><title>Post {path}</title><link>{}?utm_source=rss</link>",
                server.url(path)
            ));
            if let Some(date) = date {
                feed.push_str(&format!("<pubDate>{date}</pubDate>"));
            }
            feed.push_str("</item>");
        }
        feed.push_str("</channel></rss>");
        Reply::Ok("application/rss+xml", feed.into_bytes())
    }

    fn day(n: i64) -> String {
        (Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0).single().unwrap() + chrono::Duration::days(n))
            .to_rfc2822()
    }

    /// A post page; `post` makes its text its own, as every post's is.
    fn article_page(server: &Server, post: &str, paid: bool) -> Reply {
        let paragraphs: String = (0..8)
            .map(|i| format!("<p>Paragraph {i} of the post {post}, long enough to be read as prose by anyone who opens it and keeps reading.</p>"))
            .collect();
        Reply::Ok(
            "text/html",
            format!(
                "<html><head><title>Post</title>\
                 <script type=\"application/ld+json\">{{\"@type\":\"NewsArticle\",\"isAccessibleForFree\":{}}}</script></head>\
                 <body><nav><a href=\"/\">Home</a></nav><article><h1>Post</h1>{paragraphs}\
                 <figure><img src=\"{}\"><figcaption>A figure</figcaption></figure></article>\
                 <footer>Footer</footer></body></html>",
                !paid,
                server.url("/img.png")
            )
            .into_bytes(),
        )
    }

    /// A short post beside the sidebar its site puts on every page.
    fn link_blog_page(post: &str) -> Reply {
        Reply::Ok(
            "text/html",
            format!(
                r#"<html><head><title>Post</title></head><body>
<div id="wrapper"><div id="primary"><div class="entry"><div>{post}</div></div>
<div class="recent"><h2>Recent articles</h2><ul><li><a href="/a/">An older article</a></li><li><a href="/b/">Another older one</a></li></ul></div></div>
<div id="secondary"><section><h3>Monthly briefing</h3><p>Sponsor me for $10/month and get a curated email digest of the month's most important developments.</p><p>Pay me to send you less!</p></section></div></div>
</body></html>"#
            )
            .into_bytes(),
        )
    }

    fn png() -> Reply {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        Reply::Ok("image/png", bytes)
    }

    fn feed_record(ctx: &AppContext, root: &Path, url: String, auto_import: bool) -> FeedRecord {
        let feed = FeedRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Blog".into(),
            source: FeedSource::Syndication { url },
            root: std::fs::canonicalize(root).unwrap(),
            folder: "Feeds/Blog".into(),
            auto_import,
            created_at_ms: now_ms(),
            last_attempt_at_ms: None,
            last_success_at_ms: None,
            last_error: None,
            retry_after_ms: None,
            may_have_missed: false,
            inbox_count: 0,
        };
        ctx.feeds_store()
            .unwrap()
            .lock()
            .unwrap()
            .insert_feed(&feed)
            .unwrap();
        feed
    }

    fn reload(ctx: &AppContext, id: &str) -> FeedRecord {
        ctx.feeds_store()
            .unwrap()
            .lock()
            .unwrap()
            .feed(id)
            .unwrap()
            .unwrap()
    }

    fn items(ctx: &AppContext, status: FeedItemStatus) -> Vec<FeedItemRecord> {
        ctx.feeds_store()
            .unwrap()
            .lock()
            .unwrap()
            .feed_items(Some(status), None, 0, 500)
            .unwrap()
    }

    /// Lilian Weng's feed holds her whole archive, 53 posts since 2017; a
    /// first refresh must not drop them all in the inbox.
    #[tokio::test]
    async fn a_first_refresh_inboxes_the_ten_newest_and_a_second_adds_nothing() {
        let library = tempfile::tempdir().unwrap();
        let server = serve().await;
        let entries: Vec<_> = (0..53)
            .map(|i| (format!("/posts/{i}/"), Some(day(i * 30))))
            .collect();
        server.set("/feed", rss(&server, &entries));
        let (_dir, ctx) = context(library.path()).await;
        let feed = feed_record(&ctx, library.path(), server.url("/feed"), false);
        let cancel = CancellationToken::new();

        ctx.refresh_feed(&feed, &cancel).await.unwrap();
        let inbox = items(&ctx, FeedItemStatus::Inbox);
        assert_eq!(inbox.len(), 10);
        assert_eq!(items(&ctx, FeedItemStatus::Seen).len(), 43);
        let newest: HashSet<String> = (43..53).map(|i| format!("/posts/{i}/")).collect();
        assert!(inbox
            .iter()
            .all(|item| newest.iter().any(|path| item.link.contains(path.as_str()))));
        let feed = reload(&ctx, &feed.id);
        assert!(feed.last_success_at_ms.is_some() && !feed.may_have_missed);

        ctx.refresh_feed(&feed, &cancel).await.unwrap();
        assert_eq!(items(&ctx, FeedItemStatus::Inbox).len(), 10);
        assert_eq!(items(&ctx, FeedItemStatus::Seen).len(), 43);
    }

    /// Simon Willison's feed holds 30 entries; a refresh that finds all of
    /// them new may have missed older ones, and says so until a refresh
    /// returns one it has seen.
    #[tokio::test]
    async fn a_refresh_of_only_new_items_may_have_missed_some_until_one_is_seen() {
        let library = tempfile::tempdir().unwrap();
        let server = serve().await;
        server.set("/feed", rss(&server, &[("/first/".into(), Some(day(0)))]));
        let (_dir, ctx) = context(library.path()).await;
        let feed = feed_record(&ctx, library.path(), server.url("/feed"), false);
        let cancel = CancellationToken::new();
        ctx.refresh_feed(&feed, &cancel).await.unwrap();

        let thirty: Vec<_> = (1..=30)
            .map(|i| (format!("/p/{i}/"), Some(day(i))))
            .collect();
        server.set("/feed", rss(&server, &thirty));
        ctx.refresh_feed(&reload(&ctx, &feed.id), &cancel)
            .await
            .unwrap();
        assert!(reload(&ctx, &feed.id).may_have_missed);
        assert_eq!(items(&ctx, FeedItemStatus::Inbox).len(), 31);

        let mut next = thirty.clone();
        next.push(("/p/31/".into(), Some(day(31))));
        server.set("/feed", rss(&server, &next));
        ctx.refresh_feed(&reload(&ctx, &feed.id), &cancel)
            .await
            .unwrap();
        assert!(!reload(&ctx, &feed.id).may_have_missed);
    }

    /// A feed moved to another library imports there from then on; what it
    /// imported before stays put, and removing the feed with its files finds
    /// both, leaving whatever else is in its folders.
    #[tokio::test]
    async fn a_moved_feed_imports_into_its_new_root_and_removal_finds_every_import() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let server = serve().await;
        server.set("/feed", rss(&server, &[("/a".into(), Some(day(1)))]));
        server.set("/a", article_page(&server, "a", false));
        server.set("/b", article_page(&server, "b", false));
        server.set("/img.png", png());
        let (_dir, ctx) = context(first.path()).await;
        ctx.update_settings(serde_json::json!({ "favorites": [second.path()] }))
            .await
            .unwrap();
        let feed = feed_record(&ctx, first.path(), server.url("/feed"), true);
        let cancel = CancellationToken::new();
        ctx.refresh_feed(&feed, &cancel).await.unwrap();

        let refused = ctx
            .feed_update(
                &feed.id,
                FeedUpdate {
                    root: Some(elsewhere.path().to_path_buf()),
                    ..FeedUpdate::default()
                },
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("not one of this workspace's library folders"),
            "{refused}"
        );

        let moved = ctx
            .feed_update(
                &feed.id,
                FeedUpdate {
                    root: Some(second.path().to_path_buf()),
                    folder: Some("Posts".into()),
                    ..FeedUpdate::default()
                },
            )
            .await
            .unwrap();
        let second_root = std::fs::canonicalize(second.path()).unwrap();
        assert_eq!(moved.root, second_root);
        server.set(
            "/feed",
            rss(
                &server,
                &[("/b".into(), Some(day(2))), ("/a".into(), Some(day(1)))],
            ),
        );
        ctx.refresh_feed(&moved, &cancel).await.unwrap();

        let first_folder = std::fs::canonicalize(first.path())
            .unwrap()
            .join("Feeds/Blog");
        let second_folder = second_root.join("Posts");
        let paths: Vec<PathBuf> = items(&ctx, FeedItemStatus::Imported)
            .into_iter()
            .map(|item| item.imported_path.unwrap())
            .collect();
        assert_eq!(paths.len(), 2);
        assert!(
            paths.iter().any(|path| path.starts_with(&first_folder)),
            "{paths:?}"
        );
        assert!(
            paths.iter().any(|path| path.starts_with(&second_folder)),
            "{paths:?}"
        );
        std::fs::write(second_folder.join("notes.txt"), "mine").unwrap();

        ctx.feed_remove(&feed.id, true).await.unwrap();
        assert!(paths.iter().all(|path| !path.exists()));
        assert!(!first_folder.exists());
        assert!(second_folder.join("notes.txt").exists());
    }

    /// The first post of a site is read against another of its posts, fetched
    /// for it; the next is read against the page already read, with no
    /// request of its own.
    #[tokio::test]
    async fn a_post_is_imported_without_what_its_site_puts_on_every_page() {
        let library = tempfile::tempdir().unwrap();
        let server = serve().await;
        server.set(
            "/feed",
            rss(
                &server,
                &[
                    ("/short".into(), Some(day(2))),
                    ("/other".into(), Some(day(1))),
                ],
            ),
        );
        server.set("/short", link_blog_page("<p>A link post says, in one paragraph of real length, why the linked page is worth reading and what in it is new to anyone.</p><p>And a second paragraph adds a quote from the page itself.</p>"));
        server.set("/other", link_blog_page("<p>Another post of the same blog, with its own paragraph of real length that says something different from the first one.</p><p>And a closing line of its own.</p>"));
        let (_dir, ctx) = context(library.path()).await;
        let feed = feed_record(&ctx, library.path(), server.url("/feed"), false);
        let cancel = CancellationToken::new();
        ctx.refresh_feed(&feed, &cancel).await.unwrap();
        let inbox = items(&ctx, FeedItemStatus::Inbox);
        let id = |path: &str| {
            inbox
                .iter()
                .find(|item| item.link.contains(path))
                .unwrap()
                .id
                .clone()
        };

        let action = FeedItemAction::Import {
            allow_missing_images: false,
        };
        for path in ["/short", "/other"] {
            let imported = ctx.feed_item_set_status(&id(path), action).await.unwrap();
            let html = std::fs::read_to_string(imported.imported_path.unwrap()).unwrap();
            assert!(html.contains("paragraph of real length"), "{html}");
            for absent in ["Recent articles", "Monthly briefing", "Sponsor me"] {
                assert!(!html.contains(absent), "{absent} kept in {path}: {html}");
            }
        }
        let fetched = |path: &str| {
            server
                .log
                .lock()
                .unwrap()
                .iter()
                .filter(|(logged, _, _)| logged == path)
                .count()
        };
        // `/other` once for `/short`'s template and once to import it;
        // `/short` once.
        assert_eq!(fetched("/other"), 2);
        assert_eq!(fetched("/short"), 1);
    }

    /// A paid post's page is a preview: it stays in the inbox, marked, and is
    /// never imported; the free one is written into the feed's folder with
    /// its picture inside.
    #[tokio::test]
    async fn auto_import_writes_the_free_post_and_keeps_the_paid_one_as_a_preview() {
        let library = tempfile::tempdir().unwrap();
        let server = serve().await;
        server.set(
            "/feed",
            rss(
                &server,
                &[
                    ("/paid".into(), Some(day(2))),
                    ("/free".into(), Some(day(1))),
                ],
            ),
        );
        server.set("/paid", article_page(&server, "paid", true));
        server.set("/free", article_page(&server, "free", false));
        server.set("/img.png", png());
        let (_dir, ctx) = context(library.path()).await;
        let feed = feed_record(&ctx, library.path(), server.url("/feed"), true);

        ctx.refresh_feed(&feed, &CancellationToken::new())
            .await
            .unwrap();
        let inbox = items(&ctx, FeedItemStatus::Inbox);
        assert_eq!(inbox.len(), 1);
        assert!(inbox[0].preview_only && inbox[0].link.ends_with("/paid?utm_source=rss"));
        let imported = items(&ctx, FeedItemStatus::Imported);
        assert_eq!(imported.len(), 1);
        let path = imported[0].imported_path.clone().unwrap();
        assert!(path.starts_with(
            std::fs::canonicalize(library.path())
                .unwrap()
                .join("Feeds/Blog")
        ));
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "2024-01-02 Post free.html"
        );
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains("src=\"data:image/png;base64,"));
        assert!(html.contains("<meta name=\"wilkes:feed\" content=\"Blog\">"));
        assert!(!html.contains("Footer") && !html.contains("Home"));
        // A manual import of the preview is refused.
        assert!(ctx
            .feed_item_set_status(
                &inbox[0].id,
                FeedItemAction::Import {
                    allow_missing_images: false
                }
            )
            .await
            .is_err());
    }

    /// A picture that cannot be fetched fails the item, naming it; told to go
    /// ahead without it, the import leaves it out. A post past the indexing
    /// limit fails, naming the limit.
    #[tokio::test]
    async fn a_missing_picture_or_an_oversized_post_fails_the_item_and_says_why() {
        let library = tempfile::tempdir().unwrap();
        let server = serve().await;
        server.set("/feed", rss(&server, &[("/free".into(), Some(day(1)))]));
        server.set("/free", article_page(&server, "free", false));
        server.set("/img.png", Reply::Status(404));
        let (_dir, ctx) = context(library.path()).await;
        let feed = feed_record(&ctx, library.path(), server.url("/feed"), true);
        ctx.refresh_feed(&feed, &CancellationToken::new())
            .await
            .unwrap();

        let item = items(&ctx, FeedItemStatus::Inbox).remove(0);
        let error = item.last_error.clone().unwrap();
        assert!(
            error.contains("/img.png") && error.contains("404"),
            "{error}"
        );

        ctx.update_settings(serde_json::json!({ "max_file_size": 300 }))
            .await
            .unwrap();
        let error = ctx
            .feed_item_set_status(
                &item.id,
                FeedItemAction::Import {
                    allow_missing_images: true,
                },
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("limit"), "{error}");

        ctx.update_settings(serde_json::json!({ "max_file_size": 52_428_800u64 }))
            .await
            .unwrap();
        let imported = ctx
            .feed_item_set_status(
                &item.id,
                FeedItemAction::Import {
                    allow_missing_images: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(imported.status, FeedItemStatus::Imported);
        let html = std::fs::read_to_string(imported.imported_path.unwrap()).unwrap();
        assert!(!html.contains("<img"));
    }

    /// One request at a time, each starting at least a second after the last
    /// one ended.
    #[tokio::test]
    async fn requests_go_one_at_a_time_a_second_apart() {
        let server = serve().await;
        for path in ["/a", "/b", "/c"] {
            server.set(path, Reply::Ok("text/plain", b"ok".to_vec()));
        }
        let gate = Arc::new(RequestGate::new(REQUEST_SPACING));
        let cancel = CancellationToken::new();
        let client = http_client();
        let mut tasks = Vec::new();
        for path in ["/a", "/b", "/c"] {
            let (gate, cancel, client, url) = (
                Arc::clone(&gate),
                cancel.clone(),
                client.clone(),
                server.url(path),
            );
            tasks.push(tokio::spawn(async move {
                gate.run(&cancel, client.get_bytes(url, &[]))
                    .await
                    .unwrap()
                    .unwrap()
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(server.most_in_flight.load(Ordering::SeqCst), 1);
        let mut log = server.log.lock().unwrap().clone();
        log.sort_by_key(|(_, arrived, _)| *arrived);
        assert_eq!(log.len(), 3);
        for pair in log.windows(2) {
            let gap = pair[1].1.duration_since(pair[0].2);
            assert!(gap >= Duration::from_millis(950), "{gap:?}");
        }
    }

    /// Stop ends a refresh in the middle of a request that never answers,
    /// well within three seconds, with nothing written.
    #[tokio::test]
    async fn stop_ends_a_refresh_stuck_in_a_request_within_three_seconds() {
        let library = tempfile::tempdir().unwrap();
        let server = serve().await;
        server.set("/feed", Reply::Hang);
        let (_dir, ctx) = context(library.path()).await;
        let feed = feed_record(&ctx, library.path(), server.url("/feed"), false);
        let cancel = ctx.feed_upkeep.cancel.lock().clone();
        let refresh = {
            let ctx = Arc::clone(&ctx);
            tokio::spawn(async move { ctx.refresh_feed(&feed, &cancel).await })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        let stopped = Instant::now();
        ctx.feeds_refresh_cancel();
        assert!(
            stopped.elapsed() < Duration::from_millis(50),
            "cancel must return at once"
        );
        let outcome = tokio::time::timeout(Duration::from_secs(3), refresh)
            .await
            .expect("the refresh must end within three seconds")
            .unwrap();
        assert!(outcome.is_err());
        assert!(items(&ctx, FeedItemStatus::Inbox).is_empty());
        // A fresh token is in place: the next refresh is not born stopped.
        assert!(!ctx.feed_upkeep.cancel.lock().is_cancelled());
    }

    /// A page offers the feed its head advertises and the one its body links,
    /// each read to be sure; what only looks like a feed is not offered. A
    /// feed is added by its own address, never by a page's.
    #[tokio::test]
    async fn a_page_offers_its_feeds_and_one_is_chosen() {
        let library = tempfile::tempdir().unwrap();
        let server = serve().await;
        server.set("/feed", rss(&server, &[]));
        let Reply::Ok(kind, everything) = rss(&server, &[("/p/1/".into(), Some(day(1)))]) else {
            unreachable!()
        };
        let entries = String::from_utf8(everything)
            .unwrap()
            .replace("<title>Blog</title>", "<title>Blog: Entries</title>");
        server.set("/atom/entries/", Reply::Ok(kind, entries.into_bytes()));
        server.set(
            "/blog",
            Reply::Ok(
                "text/html",
                b"<html><head><link rel=\"alternate\" type=\"application/rss+xml\" href=\"/feed\"></head>\
                  <body><a href=\"/atom/entries/\" title=\"Atom feed\"></a><a href=\"/rss/old\">RSS</a></body></html>"
                    .to_vec(),
            ),
        );
        server.set("/rss/old", Reply::Status(404));
        server.set(
            "/nofeed",
            Reply::Ok("text/html", b"<html><body>hi</body></html>".to_vec()),
        );
        let (_dir, ctx) = context(library.path()).await;

        let offered = ctx.feed_discover(&server.url("/blog")).await.unwrap();
        let described: Vec<(String, Option<String>, usize)> = offered
            .into_iter()
            .map(|feed| (feed.url, feed.title, feed.entries))
            .collect();
        assert_eq!(
            described,
            vec![
                (server.url("/feed"), Some("Blog".to_string()), 0),
                (
                    server.url("/atom/entries/"),
                    Some("Blog: Entries".to_string()),
                    1
                ),
            ]
        );
        // A feed's own address offers itself alone.
        assert_eq!(
            ctx.feed_discover(&server.url("/atom/entries/"))
                .await
                .unwrap()
                .len(),
            1
        );
        let error = ctx
            .feed_discover(&server.url("/nofeed"))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("custom provider"), "{error}");

        let add = |url: String| NewFeed {
            source: FeedSource::Syndication { url },
            name: None,
            root: library.path().to_path_buf(),
            folder: None,
            auto_import: false,
        };
        let error = ctx
            .feed_add(add(server.url("/blog")))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("is not a feed"), "{error}");
        let feed = ctx
            .feed_add(add(server.url("/atom/entries/")))
            .await
            .unwrap();
        assert_eq!(
            feed.source,
            FeedSource::Syndication {
                url: server.url("/atom/entries/")
            }
        );
        assert_eq!(feed.name, "Blog: Entries");
        assert_eq!(feed.folder, "Feeds/Blog Entries");

        let elsewhere = tempfile::tempdir().unwrap();
        assert!(ctx
            .feed_add(NewFeed {
                source: FeedSource::Syndication {
                    url: server.url("/feed")
                },
                name: None,
                root: elsewhere.path().to_path_buf(),
                folder: None,
                auto_import: false,
            })
            .await
            .is_err());
        ctx.shutdown().await;
    }

    #[test]
    fn a_provider_result_is_identified_by_its_address_or_its_own_id() {
        let mut work: LiteratureSearchResult = serde_json::from_value(serde_json::json!({
            "id": "W1",
            "doi": "https://doi.org/10.48550/arXiv.2401.01234",
            "publication_date": "2025-03-04",
            "is_open_access": true,
        }))
        .unwrap();
        let listed = listed_work("openalex", work.clone());
        assert_eq!(listed.link_key, "https://doi.org/10.48550/arxiv.2401.01234");
        assert_eq!(listed.arxiv_id.as_deref(), Some("2401.01234"));
        assert_eq!(
            listed
                .published
                .map(|date| date.date_naive().to_string())
                .as_deref(),
            Some("2025-03-04")
        );
        work.doi = None;
        assert_eq!(
            listed_work("openalex", work).link_key,
            "provider:openalex/W1"
        );
    }
}
