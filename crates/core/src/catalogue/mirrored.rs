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

use super::providers::{acquisition, fetchers, CatalogueFetch, FetchReporter};
use super::store::{CatalogueStore, ListingOrder, ListingQuery, SyncOutcome};
use crate::integrations::custom::manifest::{
    BrowseControls, ControlOption, FilterControl, Manifest, SortControl,
};
use crate::integrations::{LiteratureSource, ProviderCapabilities};
use crate::types::{
    BrowsePage, BrowseRequest, CatalogueAcquisition, CatalogueGrain, CatalogueRecord,
    FilterSuggestion, IntegrationState, IntegrationStatus, LiteratureAcquisition,
    LiteratureSearchResult,
};

/// The filter and sort names a catalogue listing declares. Named once so the
/// declaration and the reading of a request cannot disagree over a literal.
const SUBJECT: &str = "subject";
const LIBRARY: &str = "library";
const ANY: &str = "any";
const ALL: &str = "all";
const BY_RELEVANCE: &str = "relevance";
const BY_TITLE: &str = "title";
const BY_LENGTH: &str = "pages";

/// A listing page when the caller names no size.
const DEFAULT_BROWSE_LIMIT: usize = 25;
/// How many subjects one suggestion offers.
const SUGGESTIONS: usize = 20;

/// A browse request as the store is asked it: every name and value checked
/// against what the catalogue declares.
struct CheckedListing {
    keyword: Option<String>,
    subjects: Vec<String>,
    all_subjects: bool,
    libraries: Vec<String>,
    order: ListingOrder,
    page: usize,
    limit: usize,
}

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

    /// What a listing of this catalogue offers, as the filter panel draws it.
    fn browse_controls(&self) -> BrowseControls {
        let facets = self.fetch.browse_facets();
        let option = |value: &str, label: &str| ControlOption {
            value: value.to_string(),
            label: label.to_string(),
        };
        let filter =
            |name: &str, label: &str, input: &str, options, modes: &[&str]| FilterControl {
                equivalent_to: None,
                equivalent_values: Default::default(),
                name: name.to_string(),
                label: label.to_string(),
                group: None,
                input: input.to_string(),
                options,
                modes: modes.iter().map(|mode| mode.to_string()).collect(),
                from_result: None,
            };
        let mut filters = Vec::new();
        if facets.subjects {
            filters.push(filter(
                SUBJECT,
                "Subject",
                "suggest",
                Vec::new(),
                &[ANY, ALL],
            ));
        }
        if !facets.libraries.is_empty() {
            let options = facets
                .libraries
                .iter()
                .map(|(prefix, label)| option(prefix, label))
                .collect();
            // A book is shelved in one library, so several are only ever "any".
            filters.push(filter(LIBRARY, "Library", "options", options, &[ANY]));
        }
        let mut orders = vec![
            option(BY_RELEVANCE, "Relevance"),
            option(BY_TITLE, "Title A–Z"),
        ];
        if facets.pages {
            orders.push(option(BY_LENGTH, "Longest first"));
        }
        BrowseControls {
            keyword: true,
            keyword_required: false,
            total: true,
            sort: Some(SortControl {
                equivalent_to: [(BY_RELEVANCE.to_string(), "most_relevant".to_string())]
                    .into_iter()
                    .collect(),
                default: BY_TITLE.to_string(),
                keyword_default: Some(BY_RELEVANCE.to_string()),
                options: orders,
            }),
            modes: if filters.is_empty() {
                Vec::new()
            } else {
                vec![option(ANY, "Any of"), option(ALL, "All of")]
            },
            filters,
        }
    }

    /// Reads a browse request against [`Self::browse_controls`], refusing by
    /// name anything they do not declare.
    fn checked(&self, request: &BrowseRequest) -> anyhow::Result<CheckedListing> {
        let controls = self.browse_controls();
        let keyword = request
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_string);
        let mut listing = CheckedListing {
            subjects: Vec::new(),
            all_subjects: false,
            libraries: Vec::new(),
            order: ListingOrder::Title,
            page: request.page.unwrap_or(1).max(1),
            limit: request.limit.unwrap_or(DEFAULT_BROWSE_LIMIT),
            keyword,
        };
        let declared = || {
            controls
                .filters
                .iter()
                .map(|filter| filter.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let control = |name: &str| {
            controls
                .filters
                .iter()
                .find(|filter| filter.name == name)
                .ok_or_else(|| {
                    if controls.filters.is_empty() {
                        anyhow::anyhow!("{} has no filter “{name}”; it has none", self.name())
                    } else {
                        anyhow::anyhow!(
                            "{} has no filter “{name}”; it has {}",
                            self.name(),
                            declared()
                        )
                    }
                })
        };
        for (name, mode) in &request.modes {
            let filter = control(name)?;
            anyhow::ensure!(
                filter.modes.iter().any(|declared| declared == mode),
                "{} cannot combine {} values by “{mode}”",
                self.name(),
                filter.label
            );
        }
        for (name, value) in &request.filters {
            let filter = control(name)?;
            let values: Vec<String> = value.values().into_iter().map(str::to_string).collect();
            anyhow::ensure!(
                values.len() <= 1 || !filter.modes.is_empty(),
                "{} takes one {} value",
                self.name(),
                filter.label
            );
            if let Some(refused) = values.iter().find(|value| {
                !filter.options.is_empty()
                    && !filter.options.iter().any(|option| &option.value == *value)
            }) {
                anyhow::bail!("{} has no {} “{refused}”", self.name(), filter.label);
            }
            match name.as_str() {
                SUBJECT => {
                    listing.subjects = values;
                    listing.all_subjects =
                        request.modes.get(SUBJECT).map(String::as_str) == Some(ALL);
                }
                LIBRARY => listing.libraries = values,
                other => unreachable!("declared filter {other} has no reading"),
            }
        }
        let sort = controls.sort.as_ref().expect("catalogues declare a sort");
        let order = match request.sort.as_deref().filter(|sort| !sort.is_empty()) {
            Some(order) => {
                anyhow::ensure!(
                    sort.options.iter().any(|option| option.value == order),
                    "{} cannot be ordered by “{order}”",
                    self.name()
                );
                order
            }
            None if listing.keyword.is_some() => BY_RELEVANCE,
            None => BY_TITLE,
        };
        listing.order = match order {
            BY_RELEVANCE => {
                anyhow::ensure!(
                    listing.keyword.is_some(),
                    "ordering {} by relevance needs a keyword",
                    self.name()
                );
                ListingOrder::Relevance
            }
            BY_TITLE => ListingOrder::Title,
            BY_LENGTH => ListingOrder::LongestFirst,
            other => unreachable!("declared order {other} has no reading"),
        };
        Ok(listing)
    }

    /// The mirror, once this catalogue has been fetched into it.
    fn fetched_store(&self) -> anyhow::Result<CatalogueStore> {
        let store = CatalogueStore::open(&self.mirror_dir)?;
        anyhow::ensure!(
            store
                .holdings()?
                .iter()
                .any(|held| held.provider == self.id()),
            "{} has not been fetched; sync it in Settings › Integrations › Catalogues",
            self.name()
        );
        Ok(store)
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
fn result_from(record: CatalogueRecord, recall_score: Option<f64>) -> LiteratureSearchResult {
    let present = |text: String| Some(text).filter(|text| !text.trim().is_empty());
    let acquisition = acquisition(&record);
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
        image_url: record.image_url,
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
        recall_score,
        acquisition: match acquisition {
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
            // Recall is one page; a listing pages through every record.
            browse: Some(self.browse_controls()),
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
        Ok(recall
            .hits
            .into_iter()
            .map(|hit| result_from(hit.record, Some(hit.recall_score)))
            .collect())
    }

    async fn browse(&self, request: &BrowseRequest) -> anyhow::Result<BrowsePage> {
        let checked = self.checked(request)?;
        let listing = self.fetched_store()?.list(&ListingQuery {
            provider: self.id(),
            keyword: checked.keyword.as_deref(),
            subjects: &checked.subjects,
            all_subjects: checked.all_subjects,
            id_prefixes: &checked.libraries,
            order: checked.order,
            page: checked.page,
            limit: checked.limit,
        })?;
        if let Some(keyword) = checked.keyword.as_deref() {
            anyhow::ensure!(
                !listing.terms.is_empty(),
                "nothing in “{keyword}” could be searched for; single letters and very common words are dropped before the query runs"
            );
        }
        Ok(BrowsePage {
            results: listing
                .records
                .into_iter()
                .map(|listed| result_from(listed.record, listed.recall_score))
                .collect(),
            total: Some(listing.total),
        })
    }

    fn check_browse(&self, request: &BrowseRequest) -> anyhow::Result<()> {
        self.checked(request).map(|_| ())
    }

    /// The subjects this catalogue's records carry that contain `text`,
    /// most-carried first, each with how many records carry it.
    async fn suggest(&self, filter: &str, text: &str) -> anyhow::Result<Vec<FilterSuggestion>> {
        let controls = self.browse_controls();
        anyhow::ensure!(
            controls
                .filters
                .iter()
                .any(|declared| declared.name == filter && declared.input == "suggest"),
            "{} suggests nothing for “{filter}”",
            self.name()
        );
        let wanted = text.trim().to_lowercase();
        let noun = match self.grain() {
            CatalogueGrain::Textbook => ("book", "books"),
            CatalogueGrain::Course => ("course", "courses"),
            CatalogueGrain::Reference => ("set", "sets"),
        };
        Ok(self
            .fetched_store()?
            .subjects(self.id())?
            .into_iter()
            .filter(|(subject, _)| subject.to_lowercase().contains(&wanted))
            .take(SUGGESTIONS)
            .map(|(subject, count)| FilterSuggestion {
                hint: Some(format!(
                    "{count} {}",
                    if count == 1 { noun.0 } else { noun.1 }
                )),
                label: subject.clone(),
                value: subject,
            })
            .collect())
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
    use crate::types::FilterValue;

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
            image_url: Some(format!("https://example.test/{id}.png")),
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
        assert_eq!(
            result.image_url.as_deref(),
            Some("https://example.test/a.png")
        );
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
            assert!(!capabilities.lookup && !capabilities.references);
            assert!(source.manifest().is_none());
        }
    }

    fn filters(source: &MirroredSource) -> Vec<String> {
        source
            .capabilities()
            .browse
            .expect("every catalogue lists")
            .filters
            .into_iter()
            .map(|filter| filter.name)
            .collect()
    }

    #[test]
    fn each_catalogue_offers_only_the_filters_its_records_can_answer() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            filters(&source(dir.path(), "libretexts")),
            [SUBJECT, LIBRARY]
        );
        assert_eq!(filters(&source(dir.path(), "openstax")), [SUBJECT]);
        assert_eq!(filters(&source(dir.path(), "mit_ocw")), [SUBJECT]);
        // One subject for every set: nothing to filter, but still an order.
        let devdocs = source(dir.path(), "devdocs").capabilities().browse.unwrap();
        assert!(devdocs.filters.is_empty() && devdocs.modes.is_empty());
        assert!(devdocs.sort.is_some());
        let orders = |id: &str| -> Vec<String> {
            source(dir.path(), id)
                .capabilities()
                .browse
                .unwrap()
                .sort
                .unwrap()
                .options
                .into_iter()
                .map(|option| option.value)
                .collect()
        };
        assert_eq!(orders("libretexts"), [BY_RELEVANCE, BY_TITLE, BY_LENGTH]);
        assert_eq!(orders("openstax"), [BY_RELEVANCE, BY_TITLE]);
    }

    fn shelved(id: &str, title: &str, subject: &str, pages: Option<i64>) -> CatalogueRecord {
        CatalogueRecord {
            subject: subject.into(),
            pages,
            ..record("libretexts", id, title, CatalogueGrain::Textbook)
        }
    }

    fn libretexts_mirror() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        CatalogueStore::open(dir.path())
            .unwrap()
            .replace_provider(
                "libretexts",
                &[
                    shelved("math-1", "Calculus", "Analysis", Some(300)),
                    shelved("math-2", "Algebra", "Algebra, Analysis", Some(120)),
                    shelved("chem-3", "Organic Chemistry", "Organic", None),
                    shelved("phys-4", "Mechanics", "Classical Mechanics", Some(500)),
                ],
            )
            .unwrap();
        dir
    }

    fn request(filters: &[(&str, &[&str])]) -> BrowseRequest {
        BrowseRequest {
            filters: filters
                .iter()
                .map(|(name, values)| {
                    (
                        name.to_string(),
                        FilterValue::Many(values.iter().map(|v| v.to_string()).collect()),
                    )
                })
                .collect(),
            ..BrowseRequest::default()
        }
    }

    fn titles(page: &BrowsePage) -> Vec<&str> {
        page.results
            .iter()
            .map(|result| result.title.as_deref().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn a_listing_with_nothing_set_is_every_record_by_title() {
        let dir = libretexts_mirror();
        let page = source(dir.path(), "libretexts")
            .browse(&BrowseRequest::default())
            .await
            .unwrap();
        assert_eq!(
            titles(&page),
            ["Algebra", "Calculus", "Mechanics", "Organic Chemistry"]
        );
        assert_eq!(page.total, Some(4));
        // A listing without a keyword ranks nothing, so it claims no score.
        assert!(page.results.iter().all(|r| r.recall_score.is_none()));
    }

    #[tokio::test]
    async fn a_listing_narrows_by_library_and_by_subject() {
        let dir = libretexts_mirror();
        let libretexts = source(dir.path(), "libretexts");

        let maths = libretexts
            .browse(&request(&[(LIBRARY, &["math"])]))
            .await
            .unwrap();
        assert_eq!(titles(&maths), ["Algebra", "Calculus"]);

        // A subject is matched whole: "Mechanics" is not "Classical Mechanics".
        let partial = libretexts
            .browse(&request(&[(SUBJECT, &["Mechanics"])]))
            .await
            .unwrap();
        assert_eq!(partial.total, Some(0));

        let any = libretexts
            .browse(&request(&[(SUBJECT, &["Algebra", "Organic"])]))
            .await
            .unwrap();
        assert_eq!(titles(&any), ["Algebra", "Organic Chemistry"]);

        let mut all = request(&[(SUBJECT, &["Algebra", "Analysis"])]);
        all.modes.insert(SUBJECT.into(), ALL.into());
        let all = libretexts.browse(&all).await.unwrap();
        assert_eq!(titles(&all), ["Algebra"]);
    }

    #[tokio::test]
    async fn a_listing_pages_and_orders_by_length() {
        let dir = libretexts_mirror();
        let mut longest = BrowseRequest {
            sort: Some(BY_LENGTH.into()),
            limit: Some(2),
            page: Some(2),
            ..BrowseRequest::default()
        };
        let second = source(dir.path(), "libretexts")
            .browse(&longest)
            .await
            .unwrap();
        // 500, 300 | 120, then the book with no page count last.
        assert_eq!(titles(&second), ["Algebra", "Organic Chemistry"]);
        assert_eq!(second.total, Some(4));
        longest.page = Some(3);
        let past = source(dir.path(), "libretexts")
            .browse(&longest)
            .await
            .unwrap();
        assert!(past.results.is_empty());
    }

    #[tokio::test]
    async fn a_keyword_listing_ranks_by_default_and_counts_its_matches() {
        let dir = libretexts_mirror();
        let page = source(dir.path(), "libretexts")
            .browse(&BrowseRequest {
                query: Some("entropy".into()),
                ..request(&[(LIBRARY, &["math", "phys"])])
            })
            .await
            .unwrap();
        // Every record's summary mentions heat and entropy; the library
        // narrows those matches to three.
        assert_eq!(page.total, Some(3));
        assert!(page.results.iter().all(|r| r.recall_score.is_some()));
    }

    #[test]
    fn what_a_catalogue_does_not_declare_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let libretexts = source(dir.path(), "libretexts");
        let refusal = |request: BrowseRequest| {
            format!("{:#}", libretexts.check_browse(&request).unwrap_err())
        };
        assert!(refusal(request(&[("author", &["x"])])).contains("no filter “author”"));
        assert!(refusal(request(&[(LIBRARY, &["astrology"])])).contains("“astrology”"));
        let mut none = request(&[(LIBRARY, &["math"])]);
        none.modes.insert(LIBRARY.into(), ALL.into());
        assert!(refusal(none).contains("cannot combine"));
        assert!(refusal(BrowseRequest {
            sort: Some(BY_RELEVANCE.into()),
            ..BrowseRequest::default()
        })
        .contains("needs a keyword"));
        assert!(refusal(BrowseRequest {
            sort: Some("newest".into()),
            ..BrowseRequest::default()
        })
        .contains("“newest”"));
        // DevDocs declares no length, so it cannot be ordered by one.
        assert!(source(dir.path(), "devdocs")
            .check_browse(&BrowseRequest {
                sort: Some(BY_LENGTH.into()),
                ..BrowseRequest::default()
            })
            .is_err());
        assert!(libretexts
            .check_browse(&request(&[(SUBJECT, &["Algebra"]), (LIBRARY, &["math"])]))
            .is_ok());
    }

    #[tokio::test]
    async fn a_catalogue_never_fetched_says_so_rather_than_listing_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let error = source(dir.path(), "openstax")
            .browse(&BrowseRequest::default())
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("has not been fetched"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn subjects_are_suggested_whole_and_most_carried_first() {
        let dir = libretexts_mirror();
        let libretexts = source(dir.path(), "libretexts");
        let found = libretexts.suggest(SUBJECT, "a").await.unwrap();
        let labels: Vec<_> = found.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Analysis", "Algebra", "Classical Mechanics", "Organic"]
        );
        assert_eq!(found[0].hint.as_deref(), Some("2 books"));
        assert_eq!(found[1].hint.as_deref(), Some("1 book"));
        assert!(libretexts.suggest(LIBRARY, "m").await.is_err());
    }
}
