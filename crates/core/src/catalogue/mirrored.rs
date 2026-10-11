//! A catalogue as a literature source: asked like any other, answered from
//! the mirror.
//!
//! This is the whole of what makes a catalogue "one more source". Everything a
//! caller does with a provider — look it up by id, ask it a query, list what
//! it returned, add a result — goes through
//! [`LiteratureSource`], and the one thing that differs is on this side of the
//! trait: the answer is recall over records fetched earlier, not a request.
//!
//! Which is also why asking one costs nothing and reaches nobody, and why a
//! caller that must not send a query off the machine unprompted can ask these
//! and only these ([`ProviderCapabilities::mirrored`]).

use std::path::{Path, PathBuf};

use anyhow::Context;
use async_trait::async_trait;

use super::providers::{fetchers, CatalogueFetch, FetchReporter};
use super::store::{CatalogueStore, SyncOutcome};
use crate::integrations::custom::manifest::Manifest;
use crate::integrations::{LiteratureSource, ProviderCapabilities};
use crate::types::{
    CatalogueAcquisition, CatalogueGrain, CatalogueHit, IntegrationState, IntegrationStatus,
    LiteratureAcquisition, LiteratureSearchResult,
};

/// One catalogue: how it is fetched, and where what was fetched is kept.
pub struct MirroredSource {
    fetch: Box<dyn CatalogueFetch>,
    mirror_dir: PathBuf,
}

/// Every catalogue this build knows, each reading and writing the mirror in
/// `mirror_dir`.
pub fn mirrored_sources(mirror_dir: &Path) -> Vec<MirroredSource> {
    fetchers()
        .into_iter()
        .map(|fetch| MirroredSource {
            fetch,
            mirror_dir: mirror_dir.to_path_buf(),
        })
        .collect()
}

impl MirroredSource {
    /// What kind of source this is. Every record it supplies is of this kind,
    /// so it is said once here rather than chosen per query.
    pub fn grain(&self) -> CatalogueGrain {
        self.fetch.grain()
    }

    /// The id as the fetcher declares it: the same string as
    /// [`LiteratureSource::id`], with the lifetime a progress report is keyed
    /// by.
    pub fn fetch_id(&self) -> &'static str {
        self.fetch.id()
    }

    /// Fetches the catalogue from its publisher and replaces what the mirror
    /// held for it. A failed fetch leaves the mirror as it was.
    pub async fn sync(&self, progress: &FetchReporter) -> anyhow::Result<SyncOutcome> {
        let records = self
            .fetch
            .fetch_all(progress)
            .await
            .with_context(|| format!("fetching {}", self.fetch.name()))?;
        CatalogueStore::open(&self.mirror_dir)?
            .replace_provider(self.fetch.id(), &records)
            .with_context(|| format!("storing {}", self.fetch.name()))
    }
}

/// A mirror record as the result every source answers in.
///
/// Open access is stated rather than read: these are catalogues of openly
/// published material, and the licence each record carries says under what
/// terms.
fn result_from(hit: CatalogueHit) -> LiteratureSearchResult {
    let present = |text: String| Some(text).filter(|text| !text.trim().is_empty());
    let record = hit.record;
    LiteratureSearchResult {
        id: record.external_id,
        doi: None,
        title: Some(record.title),
        year: None,
        publication_date: None,
        venue: None,
        citation_count: None,
        is_open_access: true,
        pdf_url: record.pdf_url,
        // A documentation set has no landing page of its own; its outline is
        // where it is read.
        landing_page_url: record.landing_url.or(record.outline_url),
        open_access_status: None,
        license: present(record.license),
        authors: present(record.authors),
        publisher: None,
        language: None,
        file_format: None,
        file_size: None,
        summary: present(record.summary),
        subject: present(record.subject),
        kind: Some(record.grain.as_str().to_string()),
        pages: record.pages,
        recall_score: Some(hit.recall_score),
        acquisition: match hit.acquisition {
            CatalogueAcquisition::File => LiteratureAcquisition::Direct,
            CatalogueAcquisition::Course => LiteratureAcquisition::Course,
            CatalogueAcquisition::None => LiteratureAcquisition::None,
        },
    }
}

#[async_trait]
impl LiteratureSource for MirroredSource {
    fn id(&self) -> &str {
        self.fetch.id()
    }

    fn name(&self) -> &str {
        self.fetch.name()
    }

    /// Written in code, not described by a manifest.
    fn manifest(&self) -> Option<&Manifest> {
        None
    }

    fn base_url(&self) -> &str {
        self.fetch.base_url()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            paged_search: false,
            // A catalogue serves one grain and the mirror has no order but
            // recall, so there is nothing to filter or sort by.
            browse: None,
            lookup: false,
            references: false,
            resolve_download: false,
            mirrored: true,
        }
    }

    async fn search(
        &self,
        query: &str,
        limit: usize,
        page: usize,
    ) -> anyhow::Result<Vec<LiteratureSearchResult>> {
        anyhow::ensure!(
            page == 1,
            "{} is answered in one page; page {page} does not exist",
            self.name()
        );
        let recall =
            CatalogueStore::open(&self.mirror_dir)?.search_provider(query, self.id(), limit)?;
        // Not "nothing found": the mirror was never asked. Only the store
        // knows which words it drops, so this is the one place that can say.
        anyhow::ensure!(
            !recall.terms.is_empty(),
            "nothing in “{}” could be searched for; single letters and very common words are dropped before the query runs",
            query.trim()
        );
        Ok(recall.hits.into_iter().map(result_from).collect())
    }

    async fn status(&self, enabled: bool) -> anyhow::Result<IntegrationStatus> {
        let held = CatalogueStore::open(&self.mirror_dir)?
            .holdings()?
            .into_iter()
            .find(|held| held.provider == self.id());
        let (state, message) = match held {
            Some(held) => (
                IntegrationState::Ready,
                format!(
                    "{} is answered from {} records held on this machine.",
                    self.name(),
                    held.records
                ),
            ),
            None => (
                IntegrationState::Disabled,
                format!(
                    "{} has not been fetched, so it is not searched.",
                    self.name()
                ),
            ),
        };
        Ok(IntegrationStatus {
            id: self.id().to_string(),
            enabled,
            state,
            message,
            version: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CatalogueRecord;

    fn record(provider: &str, id: &str, title: &str, grain: CatalogueGrain) -> CatalogueRecord {
        CatalogueRecord {
            provider: provider.into(),
            external_id: id.into(),
            title: title.into(),
            summary: "An introduction to heat, work and entropy.".into(),
            subject: String::new(),
            authors: "A. Author".into(),
            license: "cc-by".into(),
            landing_url: Some(format!("https://example.test/{id}")),
            pdf_url: Some(format!("https://example.test/{id}.pdf")),
            outline_url: None,
            grain,
            pages: Some(412),
        }
    }

    fn source(dir: &Path, id: &str) -> MirroredSource {
        mirrored_sources(dir)
            .into_iter()
            .find(|source| source.id() == id)
            .unwrap()
    }

    #[tokio::test]
    async fn a_catalogue_answers_from_its_own_records_in_the_shared_result_shape() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = CatalogueStore::open(dir.path()).unwrap();
            store
                .replace_provider(
                    "libretexts",
                    &[record(
                        "libretexts",
                        "a",
                        "Thermodynamics",
                        CatalogueGrain::Textbook,
                    )],
                )
                .unwrap();
            store
                .replace_provider(
                    "openstax",
                    &[record(
                        "openstax",
                        "b",
                        "Thermodynamics and Heat",
                        CatalogueGrain::Textbook,
                    )],
                )
                .unwrap();
        }

        let results = source(dir.path(), "libretexts")
            .search("thermodynamics", 10, 1)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert_eq!(result.id, "a");
        assert_eq!(result.title.as_deref(), Some("Thermodynamics"));
        assert_eq!(result.kind.as_deref(), Some("textbook"));
        assert_eq!(result.pages, Some(412));
        assert_eq!(result.acquisition, LiteratureAcquisition::Direct);
        assert!(result.summary.is_some());
        // An empty column is absent, not an empty string a row would print.
        assert_eq!(result.subject, None);
        assert!(result.recall_score.is_some());
    }

    #[tokio::test]
    async fn a_query_of_nothing_searchable_is_an_error_and_not_an_empty_answer() {
        let dir = tempfile::tempdir().unwrap();
        let error = source(dir.path(), "devdocs")
            .search("a", 10, 1)
            .await
            .expect_err("no term survives");
        assert!(error.to_string().contains("could be searched for"));
    }

    #[tokio::test]
    async fn a_course_is_neither_a_direct_file_nor_a_resolver_s_to_find() {
        let dir = tempfile::tempdir().unwrap();
        let mut course = record(
            "mit_ocw",
            "c",
            "Statistical Mechanics",
            CatalogueGrain::Course,
        );
        course.pdf_url = None;
        CatalogueStore::open(dir.path())
            .unwrap()
            .replace_provider("mit_ocw", &[course])
            .unwrap();

        let source = source(dir.path(), "mit_ocw");
        let results = source.search("statistical mechanics", 10, 1).await.unwrap();
        assert_eq!(results[0].acquisition, LiteratureAcquisition::Course);
        assert_eq!(source.resolve_download(&results[0]).await.unwrap(), None);
    }

    #[test]
    fn every_catalogue_says_it_is_mirrored_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        for source in mirrored_sources(dir.path()) {
            let capabilities = source.capabilities();
            assert!(capabilities.mirrored, "{}", source.id());
            assert!(capabilities.browse.is_none() && !capabilities.lookup);
            assert!(source.manifest().is_none());
        }
    }
}
