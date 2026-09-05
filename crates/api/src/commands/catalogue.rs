//! The catalogue mirror as application operations, rather than one shell's routes.
//!
//! These lived in `wilkes-server`'s handlers, which is why the desktop had no
//! catalogue: the sync loop, the provider registry walk and the per-provider
//! failure accounting were all inside an axum function that Tauri cannot call.
//! Both shells now call the same four operations here, and each shell's job is
//! reduced to naming its own error type.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use wilkes_core::acquire::{download_to_root, DownloadParams, DownloadProgress, DownloadResponse};
use wilkes_core::catalogue::ocw;
use wilkes_core::catalogue::providers::FetchReporter;
use wilkes_core::catalogue::{registry, CatalogueStore};
use wilkes_core::network::ProviderHttpClient;
use wilkes_core::types::{
    CatalogueCourseProgress, CatalogueCourseStage, CatalogueFetchProgress, CatalogueGrain,
    CatalogueHit, CatalogueProviderStatus,
};

use crate::context::{AppContext, EventEmitter};

/// Named separately from `embed-*` for the reason the generation stream is:
/// borrowing another feature's events would put a UI into a state with no
/// terminal event of its own to leave it.
pub const SYNC_PROGRESS_EVENT: &str = "catalogue-sync-progress";
pub const DOWNLOAD_PROGRESS_EVENT: &str = "catalogue-download-progress";
/// A course is many downloads and a manifest walk before them, so it has a
/// stream of its own. Sharing `catalogue-download-progress` would leave a UI
/// counting bytes with no way to say which of forty documents it was on, and
/// no terminal event for the course as a whole.
pub const COURSE_PROGRESS_EVENT: &str = "catalogue-course-progress";

/// How many reports may queue before the fetch stops waiting for a listener.
/// Reporting is lossy on purpose; see [`FetchReporter::page`].
const PROGRESS_QUEUE: usize = 64;

/// One batch's ceiling. A caller with more gaps than this has a loop to write,
/// and the loop is theirs: 64 queries is already a table scan per query.
pub const MAX_QUERIES: usize = 64;

/// Hits per query when the caller does not say. Wide enough to re-rank
/// against, short enough to render.
pub const DEFAULT_LIMIT: usize = 24;

/// Why a catalogue operation could not be carried out.
///
/// Two variants because the two have different audiences: `Request` is the
/// caller's to fix and names what it got wrong, `Failed` is ours. A shell that
/// collapsed them would answer a misspelled grain with a 500, and a broken
/// mirror with a 400.
#[derive(Debug)]
pub enum CatalogueError {
    Request(String),
    Failed(String),
}

impl std::fmt::Display for CatalogueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(message) | Self::Failed(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CatalogueError {}

/// One gap to look for, as it arrives from a caller.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueProbe {
    /// Echoed back on the matching result so a caller batching many gaps can
    /// reattach each answer without relying on ordering.
    pub key: String,
    pub text: String,
    /// Which kinds of source this query will accept. Absent or empty means all
    /// of them.
    ///
    /// A set rather than one value: the caller knows which kinds could answer
    /// its question and that is often more than one — a broad subject is
    /// better served by a course than a textbook, but a textbook still teaches
    /// it, and filtering to the single preferred kind silently hides every
    /// provider that publishes at another grain.
    #[serde(default)]
    pub grains: Option<Vec<String>>,
}

/// One query batched with many others may legitimately match nothing; the
/// whole request failing because one probe was empty would make batching worse
/// than looping.
#[derive(Clone, Debug, Serialize)]
pub struct CatalogueQueryResult {
    pub key: String,
    /// What the query reduced to. Empty with empty `hits` means the query held
    /// no usable term — not that the mirror holds nothing on the subject.
    pub terms: Vec<String>,
    pub hits: Vec<CatalogueHit>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CatalogueSearchResponse {
    pub results: Vec<CatalogueQueryResult>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CatalogueSyncOutcome {
    pub provider: String,
    pub grain: &'static str,
    /// Present on success. Absent with `error` set means this provider failed
    /// and the others in the same request did not — a partial sync is reported
    /// as partial rather than collapsed into one failure.
    pub records: Option<usize>,
    /// What the provider handed over, before deduplication. Both LibreTexts
    /// and MIT OpenCourseWare repeat ids across a paged fetch, so `offered`
    /// runs well ahead of `records`; a provider whose gap suddenly widens is
    /// one whose pagination has changed under us.
    pub offered: Option<usize>,
    pub duplicates: Option<usize>,
    pub unusable: Option<usize>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CatalogueSyncResponse {
    pub providers: Vec<CatalogueSyncOutcome>,
    pub total_records: i64,
}

/// One document of a course that was fetched.
#[derive(Clone, Debug, Serialize)]
pub struct CourseDocument {
    pub filename: String,
    pub path: String,
    pub bytes: usize,
    /// True when the identical bytes were already in the course directory, so
    /// re-acquiring a course is a cheap no-op rather than a wall of refusals.
    pub already_present: bool,
}

/// One document that could not be fetched, named with the reason.
///
/// A separate list from `skipped`: a skip is this build declining a resource
/// it understands and does not want, a failure is a document it wanted and did
/// not get. Collapsing them would let a broken link read as a policy decision.
#[derive(Clone, Debug, Serialize)]
pub struct CourseFailure {
    pub filename: String,
    pub url: String,
    pub error: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CourseSkip {
    pub title: String,
    pub reason: String,
}

/// What acquiring one course produced.
#[derive(Clone, Debug, Serialize)]
pub struct CatalogueCourseResponse {
    pub course_url: String,
    pub title: String,
    /// What the course's folder is called — `1.77 Water Quality Control
    /// (Spring 2006)` — under uploads now and under the library root once
    /// imported. The same name in both places, so a course does not change
    /// identity by being admitted.
    pub folder: String,
    /// The directory under uploads holding the whole course.
    pub directory: String,
    /// The generated Markdown document: the syllabus, the calendar, the
    /// reading list and an index of everything beside it.
    pub document: String,
    pub documents: Vec<CourseDocument>,
    pub failures: Vec<CourseFailure>,
    pub skipped: Vec<CourseSkip>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CatalogueStatusResponse {
    pub providers: Vec<CatalogueProviderStatus>,
    pub total_records: i64,
}

fn open(catalogue_dir: &Path) -> Result<CatalogueStore, CatalogueError> {
    CatalogueStore::open(catalogue_dir)
        .map_err(|error| CatalogueError::Failed(format!("Catalogue store unavailable: {error:#}")))
}

/// Every provider this build knows, and what the mirror holds for it.
///
/// The registry is the outer join, not the store: a provider that has never
/// synced has no rows and so appears nowhere in the store's own grouping, and
/// a settings page that listed only what had already been fetched would show
/// an empty list and no way to understand it. A provider the mirror still
/// holds rows for but this build no longer registers is reported too, since
/// those rows exist and can still be searched.
pub fn status(catalogue_dir: &Path) -> Result<CatalogueStatusResponse, CatalogueError> {
    let store = open(catalogue_dir)?;
    let stored = store
        .status()
        .map_err(|error| CatalogueError::Failed(format!("Catalogue status failed: {error:#}")))?;
    let total_records = store
        .total_records()
        .map_err(|error| CatalogueError::Failed(format!("Catalogue count failed: {error:#}")))?;

    let mut providers: Vec<CatalogueProviderStatus> = Vec::new();
    for source in registry() {
        match stored.iter().find(|row| row.provider == source.id()) {
            Some(row) => providers.push(row.clone()),
            None => providers.push(CatalogueProviderStatus {
                provider: source.id().to_string(),
                grain: source.grain(),
                records: 0,
                synced_at_ms: None,
            }),
        }
    }
    for row in stored {
        if !providers.iter().any(|known| known.provider == row.provider) {
            providers.push(row);
        }
    }
    Ok(CatalogueStatusResponse {
        providers,
        total_records,
    })
}

pub fn search(
    catalogue_dir: &Path,
    queries: Vec<CatalogueProbe>,
    limit: usize,
) -> Result<CatalogueSearchResponse, CatalogueError> {
    if queries.is_empty() {
        return Err(CatalogueError::Request(
            "Catalogue search names no queries".into(),
        ));
    }
    if queries.len() > MAX_QUERIES {
        return Err(CatalogueError::Request(format!(
            "Catalogue search exceeds the documented request cap of {MAX_QUERIES} queries"
        )));
    }
    // Every grain is checked before any query runs, so a batch is not half
    // answered before the caller is told it misspelled one.
    for query in &queries {
        for grain in query.grains.iter().flatten() {
            if CatalogueGrain::parse(grain).is_none() {
                return Err(CatalogueError::Request(format!(
                    "Unknown catalogue grain {grain:?}; expected textbook, course or reference"
                )));
            }
        }
    }
    let store = open(catalogue_dir)?;
    let mut results = Vec::with_capacity(queries.len());
    for query in queries {
        let grains: Vec<CatalogueGrain> = query
            .grains
            .unwrap_or_default()
            .iter()
            .filter_map(|grain| CatalogueGrain::parse(grain))
            .collect();
        let recall = store.search(&query.text, &grains, limit).map_err(|error| {
            CatalogueError::Failed(format!("Catalogue search failed: {error:#}"))
        })?;
        results.push(CatalogueQueryResult {
            key: query.key,
            terms: recall.terms,
            hits: recall.hits,
        });
    }
    Ok(CatalogueSearchResponse { results })
}

/// Refreshes the named providers, or every one of them when none is named.
///
/// Naming all four is a minutes-long call: a caller that wants to show
/// progress names one provider at a time and drives the loop itself, which is
/// what the settings panel does.
pub async fn sync(
    catalogue_dir: &Path,
    providers: Option<Vec<String>>,
    progress: Option<mpsc::Sender<CatalogueFetchProgress>>,
) -> Result<CatalogueSyncResponse, CatalogueError> {
    let requested = providers.unwrap_or_default();
    let sources = registry();
    if let Some(unknown) = requested
        .iter()
        .find(|name| !sources.iter().any(|source| source.id() == name.as_str()))
    {
        return Err(CatalogueError::Request(format!(
            "Unknown catalogue provider {unknown:?}"
        )));
    }
    let mut store = open(catalogue_dir)?;
    let mut outcomes = Vec::new();
    for source in sources {
        if !requested.is_empty() && !requested.iter().any(|name| name == source.id()) {
            continue;
        }
        // A provider that is down must not take the others with it, and must
        // not be silent about it either: the failure is reported per provider
        // and the mirror keeps whatever that provider last supplied.
        let reporter = match &progress {
            Some(tx) => FetchReporter::new(source.id(), tx.clone()),
            None => FetchReporter::silent(),
        };
        tracing::info!(provider = source.id(), "catalogue fetch started");
        let started = std::time::Instant::now();
        match source.fetch_all(&reporter).await {
            Ok(records) => match store.replace_provider(source.id(), &records) {
                Ok(written) => {
                    tracing::info!(
                        provider = source.id(),
                        offered = written.offered,
                        stored = written.stored,
                        duplicates = written.duplicates,
                        unusable = written.unusable,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "catalogue fetch complete"
                    );
                    outcomes.push(CatalogueSyncOutcome {
                        provider: source.id().to_string(),
                        grain: source.grain().as_str(),
                        records: Some(written.stored),
                        offered: Some(written.offered),
                        duplicates: Some(written.duplicates),
                        unusable: Some(written.unusable),
                        error: None,
                    })
                }
                Err(error) => {
                    tracing::warn!(provider = source.id(), "catalogue write failed: {error:#}");
                    outcomes.push(CatalogueSyncOutcome {
                        provider: source.id().to_string(),
                        grain: source.grain().as_str(),
                        records: None,
                        offered: None,
                        duplicates: None,
                        unusable: None,
                        error: Some(format!("{error:#}")),
                    });
                }
            },
            Err(error) => {
                tracing::warn!(provider = source.id(), "catalogue fetch failed: {error:#}");
                outcomes.push(CatalogueSyncOutcome {
                    provider: source.id().to_string(),
                    grain: source.grain().as_str(),
                    records: None,
                    offered: None,
                    duplicates: None,
                    unusable: None,
                    error: Some(format!("{error:#}")),
                });
            }
        }
    }
    let total_records = store
        .total_records()
        .map_err(|error| CatalogueError::Failed(format!("Catalogue count failed: {error:#}")))?;
    Ok(CatalogueSyncResponse {
        providers: outcomes,
        total_records,
    })
}

/// Fetches a candidate's bytes into this workspace's uploads directory.
///
/// Uploads rather than a library root because this is Wilkes writing into its
/// own area: a library root is a place the user put their files, and dropping
/// fetched bytes there would be writing to a directory whose contents the user
/// believes they control. Importing from uploads into the root is a second,
/// separate step, and it is the user's.
pub async fn acquire(
    uploads_dir: &Path,
    url: String,
    filename: Option<String>,
    progress: Option<mpsc::Sender<DownloadProgress>>,
) -> Result<DownloadResponse, CatalogueError> {
    tokio::fs::create_dir_all(uploads_dir)
        .await
        .map_err(|error| {
            CatalogueError::Failed(format!("Cannot prepare uploads directory: {error}"))
        })?;
    download_to_root(uploads_dir, DownloadParams { url, filename }, progress)
        .await
        .map_err(|message| {
            tracing::warn!("catalogue acquisition failed: {message}");
            CatalogueError::Request(message)
        })
}

/// Fetches one MIT OpenCourseWare course: its documents, and a generated
/// Markdown file describing them.
///
/// A course record has no `pdf_url` because a course is not a file, so this is
/// a second *expander* rather than a second downloader — the bytes still go
/// through [`download_to_root`], once per document, into a directory named
/// after the course. Giving each course its own directory is what makes the
/// per-document names usable: a dozen courses each publish something called
/// `lecture5.pdf`, and `download_to_root` refuses same-name-different-content
/// by design, so a flat uploads directory would fail partway through the
/// second course a user asked for.
///
/// Failure is per document. One dead link out of forty is reported in
/// `failures` and costs nothing else; only an unreadable manifest — where
/// there is no course to speak of — fails the call.
pub async fn acquire_course(
    uploads_dir: &Path,
    course_url: String,
    course_progress: Option<mpsc::Sender<CatalogueCourseProgress>>,
    download_progress: Option<mpsc::Sender<DownloadProgress>>,
) -> Result<CatalogueCourseResponse, CatalogueError> {
    let http = ProviderHttpClient::new("MIT OpenCourseWare");
    let manifest = ocw::read_course(&http, &course_url, course_progress.as_ref())
        .await
        .map_err(|error| {
            tracing::warn!(course = %course_url, "OCW course manifest unreadable: {error:#}");
            CatalogueError::Request(format!("Could not read that course: {error:#}"))
        })?;

    let directory = uploads_dir.join(&manifest.folder);
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| {
            CatalogueError::Failed(format!("Cannot prepare course directory: {error}"))
        })?;

    // Written before the documents, so a course whose fetches are cancelled
    // partway still leaves behind the thing that says what it was.
    let document_name = format!("{}.md", manifest.folder);
    let document_path = directory.join(&document_name);
    tokio::fs::write(&document_path, ocw::course_document(&manifest))
        .await
        .map_err(|error| {
            CatalogueError::Failed(format!("Cannot write the course document: {error}"))
        })?;

    let total = manifest.files.len();
    let mut documents = Vec::new();
    let mut failures = Vec::new();
    for (index, file) in manifest.files.iter().enumerate() {
        if let Some(tx) = &course_progress {
            let _ = tx.try_send(CatalogueCourseProgress {
                course_url: manifest.course_url.clone(),
                stage: CatalogueCourseStage::Documents,
                done: index,
                total: Some(total),
                current: Some(file.filename.clone()),
            });
        }
        let params = DownloadParams {
            url: file.url.clone(),
            filename: Some(file.filename.clone()),
        };
        match download_to_root(&directory, params, download_progress.clone()).await {
            Ok(response) => documents.push(CourseDocument {
                filename: file.filename.clone(),
                path: response.path,
                bytes: response.bytes,
                already_present: response.already_present,
            }),
            Err(message) => {
                tracing::warn!(url = %file.url, "course document failed: {message}");
                failures.push(CourseFailure {
                    filename: file.filename.clone(),
                    url: file.url.clone(),
                    error: message,
                });
            }
        }
    }
    if let Some(tx) = &course_progress {
        let _ = tx.try_send(CatalogueCourseProgress {
            course_url: manifest.course_url.clone(),
            stage: CatalogueCourseStage::Documents,
            done: total,
            total: Some(total),
            current: None,
        });
    }
    tracing::info!(
        course = %manifest.course_url,
        documents = documents.len(),
        failed = failures.len(),
        skipped = manifest.skipped.len(),
        "course acquired"
    );

    Ok(CatalogueCourseResponse {
        course_url: manifest.course_url,
        title: manifest.title,
        folder: manifest.folder,
        directory: directory.display().to_string(),
        document: document_path.display().to_string(),
        documents,
        failures,
        skipped: manifest
            .skipped
            .into_iter()
            .map(|skip| CourseSkip {
                title: skip.title,
                reason: skip.reason,
            })
            .collect(),
    })
}

/// Forwards progress to the shell's event stream until the sender is dropped.
///
/// A task rather than a callback because the fetch and the emitter belong to
/// different worlds: one is `&self` inside core, the other an `Arc<dyn
/// EventEmitter>` owned by the application, and a channel is the seam that
/// already exists between them for embedding and recognition progress.
fn forward<T>(events: Arc<dyn EventEmitter>, name: &'static str) -> mpsc::Sender<T>
where
    T: serde::Serialize + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<T>(PROGRESS_QUEUE);
    tokio::spawn(async move {
        while let Some(update) = rx.recv().await {
            events.emit(name, serde_json::to_value(&update).unwrap_or_default());
        }
    });
    tx
}

impl AppContext {
    /// Refreshes the catalogues, reporting each page on `catalogue-sync-progress`.
    ///
    /// The shell-facing half of [`sync`]: same operation, plus the event stream
    /// that turns a five-minute silence into something a panel can render.
    pub async fn catalogue_sync(
        &self,
        providers: Option<Vec<String>>,
    ) -> Result<CatalogueSyncResponse, CatalogueError> {
        let tx = forward::<CatalogueFetchProgress>(self.emitter(), SYNC_PROGRESS_EVENT);
        sync(&self.catalogue_dir, providers, Some(tx)).await
    }

    /// Fetches a whole course, reporting the walk and the documents on
    /// `catalogue-course-progress` and each document's bytes on
    /// `catalogue-download-progress`.
    pub async fn catalogue_acquire_course(
        &self,
        course_url: String,
    ) -> Result<CatalogueCourseResponse, CatalogueError> {
        let course = forward::<CatalogueCourseProgress>(self.emitter(), COURSE_PROGRESS_EVENT);
        let bytes = forward::<DownloadProgress>(self.emitter(), DOWNLOAD_PROGRESS_EVENT);
        acquire_course(
            &self.data_dir.join("uploads"),
            course_url,
            Some(course),
            Some(bytes),
        )
        .await
    }

    /// Fetches a candidate, reporting bytes on `catalogue-download-progress`.
    pub async fn catalogue_acquire(
        &self,
        url: String,
        filename: Option<String>,
    ) -> Result<DownloadResponse, CatalogueError> {
        let tx = forward::<DownloadProgress>(self.emitter(), DOWNLOAD_PROGRESS_EVENT);
        acquire(&self.data_dir.join("uploads"), url, filename, Some(tx)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_names_every_registered_provider_before_any_sync() {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = status(dir.path()).expect("status");
        assert_eq!(status.total_records, 0);
        assert_eq!(status.providers.len(), registry().len());
        // Never synced is a state, not an absence: the panel has to be able to
        // say "this provider holds nothing yet" and offer the button.
        assert!(status
            .providers
            .iter()
            .all(|p| p.records == 0 && p.synced_at_ms.is_none()));
        assert!(status.providers.iter().any(|p| p.provider == "libretexts"));
    }

    #[test]
    fn an_unknown_grain_is_the_callers_mistake_and_names_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = search(
            dir.path(),
            vec![CatalogueProbe {
                key: "k".into(),
                text: "graph algorithms".into(),
                grains: Some(vec!["monograph".into()]),
            }],
            8,
        )
        .expect_err("unknown grain must be refused");
        match error {
            CatalogueError::Request(message) => assert!(message.contains("monograph"), "{message}"),
            CatalogueError::Failed(message) => panic!("must not be a server fault: {message}"),
        }
    }

    #[test]
    fn an_empty_or_oversized_batch_is_refused_as_a_request_fault() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            search(dir.path(), Vec::new(), 8).expect_err("empty batch"),
            CatalogueError::Request(_)
        ));
        let too_many = (0..MAX_QUERIES + 1)
            .map(|i| CatalogueProbe {
                key: i.to_string(),
                text: "graph algorithms".into(),
                grains: None,
            })
            .collect();
        assert!(matches!(
            search(dir.path(), too_many, 8).expect_err("oversized batch"),
            CatalogueError::Request(_)
        ));
    }

    #[test]
    fn a_probe_with_no_usable_terms_is_answered_rather_than_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let response = search(
            dir.path(),
            vec![CatalogueProbe {
                key: "k".into(),
                text: "the and of it".into(),
                grains: None,
            }],
            8,
        )
        .expect("a stopword probe is a real question with a real answer");
        assert_eq!(response.results.len(), 1);
        assert!(response.results[0].terms.is_empty());
        assert!(response.results[0].hits.is_empty());
    }

    /// Records what the shell would have been told.
    struct Recorder(std::sync::Mutex<Vec<(String, serde_json::Value)>>);

    impl EventEmitter for Recorder {
        fn emit(&self, name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((name.to_string(), payload));
        }
    }

    /// The seam between a fetch inside core and the shell's event stream. It is
    /// worth a test of its own because the failure it prevents is silent: a
    /// sync that works perfectly and reports nothing looks exactly like a sync
    /// that has hung.
    #[tokio::test]
    async fn progress_reaches_the_event_stream_under_its_own_name() {
        let recorder = Arc::new(Recorder(std::sync::Mutex::new(Vec::new())));
        let tx = forward::<CatalogueFetchProgress>(recorder.clone(), SYNC_PROGRESS_EVENT);
        tx.send(CatalogueFetchProgress {
            provider: "libretexts".into(),
            pages: 3,
            records: 300,
        })
        .await
        .expect("send");
        drop(tx);
        // The forwarder ends when the sender does; yielding lets it drain.
        for _ in 0..8 {
            tokio::task::yield_now().await;
            if !recorder.0.lock().unwrap().is_empty() {
                break;
            }
        }

        let events = recorder.0.lock().unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, SYNC_PROGRESS_EVENT);
        assert_eq!(events[0].1["provider"], "libretexts");
        assert_eq!(events[0].1["pages"], 3);
        assert_eq!(events[0].1["records"], 300);
    }

    #[tokio::test]
    async fn syncing_an_unregistered_provider_is_the_callers_mistake() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = sync(dir.path(), Some(vec!["wikibooks".into()]), None)
            .await
            .expect_err("unknown provider must be refused");
        match error {
            CatalogueError::Request(message) => assert!(message.contains("wikibooks"), "{message}"),
            CatalogueError::Failed(message) => panic!("must not be a server fault: {message}"),
        }
    }
}
