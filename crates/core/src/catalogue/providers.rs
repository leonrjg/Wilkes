//! The open teaching catalogues Wilkes mirrors, and the trait they share.
//!
//! Each provider answers one question — *what do you currently hold?* — and
//! nothing else. No provider is asked to search, because none of them ranks
//! for teaching (see the module docs); search happens locally in
//! [`super::store`] over the union of what these return.

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::network::ProviderHttpClient;
use crate::types::{CatalogueFetchProgress, CatalogueGrain, CatalogueRecord};

/// Where a provider says how far through its catalogue it has got.
///
/// Passed in rather than returned because a whole-catalogue fetch is minutes
/// long and the interesting part is the middle of it: a caller handed only the
/// final `Vec` learns nothing until there is nothing left to learn.
pub struct FetchReporter {
    provider: &'static str,
    tx: Option<mpsc::Sender<CatalogueFetchProgress>>,
}

impl FetchReporter {
    pub fn new(provider: &'static str, tx: mpsc::Sender<CatalogueFetchProgress>) -> Self {
        Self {
            provider,
            tx: Some(tx),
        }
    }

    /// A reporter nobody is listening to, for callers that only want the result.
    pub fn silent() -> Self {
        Self {
            provider: "",
            tx: None,
        }
    }

    /// One page landed. `try_send` because progress must never slow the fetch
    /// down, and a report nobody drained is a report nobody wanted.
    pub fn page(&self, pages: usize, records: usize) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(CatalogueFetchProgress {
                provider: self.provider.to_string(),
                pages,
                records,
            });
        }
    }
}

/// A catalogue of acquirable teaching resources.
///
/// Modelled on [`crate::integrations::citations::CitationSource`]: the
/// provider-specific identifiers and wire shapes stay behind the trait, and
/// only [`CatalogueRecord`] crosses it. Adding a fifth catalogue means writing
/// a fifth implementation and registering it, and touching nothing else.
#[async_trait]
pub trait CatalogueSource: Send + Sync {
    fn id(&self) -> &'static str;

    /// What this provider's records can answer. A provider serves exactly one
    /// grain — a documentation set is never a textbook — so this is a property
    /// of the source, not of each record.
    fn grain(&self) -> CatalogueGrain;

    /// Everything the provider currently offers.
    ///
    /// Whole-catalogue rather than incremental because none of these providers
    /// exposes a change feed; see [`super::store::CatalogueStore::replace_provider`].
    ///
    /// `progress` is told after each request completes. A provider that serves
    /// its catalogue whole reports once; a paged one reports per page.
    async fn fetch_all(&self, progress: &FetchReporter) -> anyhow::Result<Vec<CatalogueRecord>>;
}

/// Every catalogue this build knows.
pub fn registry() -> Vec<Box<dyn CatalogueSource>> {
    vec![
        Box::new(LibreTexts::default()),
        Box::new(OpenStax::default()),
        Box::new(MitOpenCourseWare::default()),
        Box::new(DevDocs::default()),
    ]
}

/// Trims a provider blurb to something an index can hold without letting one
/// verbose record dominate the term statistics.
///
/// Character-aware by construction: `chars().take(..)` never lands mid-glyph,
/// which `&s[..n]` would on the first accented title it met.
fn clip(text: &str, chars: usize) -> String {
    let cleaned = text
        .replace("<p>", " ")
        .replace("</p>", " ")
        .replace("<i>", "")
        .replace("</i>", "")
        .replace("<em>", "")
        .replace("</em>", "")
        .replace("<strong>", "")
        .replace("</strong>", "");
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(chars).collect()
}

const SUMMARY_CHARS: usize = 1600;

// ── LibreTexts ───────────────────────────────────────────────────────────────

/// The largest open-textbook catalogue here, and the strongest STEM coverage.
pub struct LibreTexts {
    base_url: String,
    http: ProviderHttpClient,
}

impl Default for LibreTexts {
    fn default() -> Self {
        Self::new("https://commons.libretexts.org")
    }
}

impl LibreTexts {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: ProviderHttpClient::new("LibreTexts"),
        }
    }
}

#[derive(Deserialize)]
struct LibreTextsPage {
    #[serde(default)]
    #[allow(dead_code)]
    err: bool,
    /// Reported by the provider and deliberately not used for termination:
    /// it undercounts what the pages actually contain. Kept because a future
    /// divergence between it and the walked count is worth being able to see.
    #[serde(rename = "numTotal", default)]
    #[allow(dead_code)]
    num_total: i64,
    #[serde(default)]
    books: Vec<LibreTextsBook>,
}

#[derive(Deserialize)]
struct LibreTextsBook {
    #[serde(rename = "bookID", default)]
    book_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    library: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    license: String,
    #[serde(default)]
    links: LibreTextsLinks,
    #[serde(rename = "exportInfo", default)]
    export_info: Option<LibreTextsExportInfo>,
}

#[derive(Deserialize, Default)]
struct LibreTextsLinks {
    #[serde(default)]
    online: Option<String>,
    #[serde(default)]
    pdf: Option<String>,
}

#[derive(Deserialize, Default)]
struct LibreTextsExportInfo {
    #[serde(rename = "contentPageCount", default)]
    content_page_count: Option<i64>,
}

/// Enough pages to cover a catalogue several times the observed size, so a
/// growing provider is not silently truncated, while a provider that paginates
/// forever still terminates.
const MAX_PAGES: usize = 200;

/// How many consecutive pages may contribute nothing new before the walk stops.
///
/// One is not enough. LibreTexts interleaves repeats — a single page deep in
/// the walk can be entirely ids already seen while the next page carries three
/// hundred new ones — so stopping at the first dry page ended the walk at 1,452
/// books out of a catalogue that keeps yielding past page 150. Three
/// consecutive dry pages has not been observed mid-catalogue.
const DRY_PAGES_BEFORE_STOP: usize = 3;

#[async_trait]
impl CatalogueSource for LibreTexts {
    fn id(&self) -> &'static str {
        "libretexts"
    }

    fn grain(&self) -> CatalogueGrain {
        CatalogueGrain::Textbook
    }

    async fn fetch_all(&self, progress: &FetchReporter) -> anyhow::Result<Vec<CatalogueRecord>> {
        let mut out: Vec<CatalogueRecord> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut dry = 0usize;
        for page in 1..=MAX_PAGES {
            let url = format!(
                "{}/api/v1/commons/catalog?limit=100&page={page}",
                self.base_url
            );
            let body: LibreTextsPage = self.http.get_json(url, &[]).await?;
            if body.books.is_empty() {
                break;
            }
            let before = seen.len();
            for book in body.books {
                if !seen.insert(book.book_id.clone()) {
                    continue;
                }
                out.push(CatalogueRecord {
                    provider: "libretexts".into(),
                    external_id: book.book_id,
                    title: clip(&book.title, 400),
                    summary: clip(&book.summary, SUMMARY_CHARS),
                    subject: if book.subject.trim().is_empty() {
                        book.library.clone()
                    } else {
                        book.subject.clone()
                    },
                    authors: clip(&book.author, 400),
                    license: book.license,
                    landing_url: book.links.online.clone(),
                    pdf_url: book.links.pdf,
                    outline_url: book.links.online,
                    grain: CatalogueGrain::Textbook,
                    pages: book.export_info.and_then(|e| e.content_page_count),
                });
            }
            progress.page(page, out.len());
            // Termination is by exhaustion, not by the provider's own count.
            // `numTotal` (4,095) is reached by the *offered* count at page 41
            // while new ids are still arriving at page 150, so trusting it lost
            // roughly 1,700 books.
            if seen.len() == before {
                dry += 1;
                if dry >= DRY_PAGES_BEFORE_STOP {
                    break;
                }
            } else {
                dry = 0;
            }
        }
        Ok(out)
    }
}

// ── OpenStax ─────────────────────────────────────────────────────────────────

/// Small, uniformly edited, and the strongest pedagogy of the four.
pub struct OpenStax {
    base_url: String,
    http: ProviderHttpClient,
}

impl Default for OpenStax {
    fn default() -> Self {
        Self::new("https://openstax.org")
    }
}

impl OpenStax {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: ProviderHttpClient::new("OpenStax"),
        }
    }
}

#[derive(Deserialize)]
struct OpenStaxPage {
    #[serde(default)]
    items: Vec<OpenStaxBook>,
}

#[derive(Deserialize)]
struct OpenStaxBook {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    meta: Option<OpenStaxMeta>,
    #[serde(default)]
    book_subjects: Vec<serde_json::Value>,
    #[serde(default)]
    pdf_url: Option<String>,
    #[serde(default)]
    license_name: Option<String>,
}

#[derive(Deserialize)]
struct OpenStaxMeta {
    #[serde(default)]
    html_url: Option<String>,
}

/// `book_subjects` arrives as objects in the listing and as bare strings in
/// some older records; take whichever is there rather than assuming one.
fn openstax_subjects(values: &[serde_json::Value]) -> String {
    values
        .iter()
        .filter_map(|value| match value {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(map) => map
                .get("subject_name")
                .or_else(|| map.get("name"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[async_trait]
impl CatalogueSource for OpenStax {
    fn id(&self) -> &'static str {
        "openstax"
    }

    fn grain(&self) -> CatalogueGrain {
        CatalogueGrain::Textbook
    }

    async fn fetch_all(&self, progress: &FetchReporter) -> anyhow::Result<Vec<CatalogueRecord>> {
        let url = format!(
            "{}/apps/cms/api/v2/pages/?type=books.Book&limit=500\
             &fields=title,book_subjects,description,pdf_url,license_name",
            self.base_url
        );
        let body: OpenStaxPage = self.http.get_json(url, &[]).await?;
        // Served whole, so there is one report and it is the last one.
        progress.page(1, body.items.len());
        Ok(body
            .items
            .into_iter()
            .map(|book| CatalogueRecord {
                provider: "openstax".into(),
                external_id: book.id.to_string(),
                title: clip(&book.title, 400),
                summary: clip(&book.description, SUMMARY_CHARS),
                subject: openstax_subjects(&book.book_subjects),
                authors: "OpenStax".into(),
                license: book.license_name.unwrap_or_else(|| "cc-by".into()),
                landing_url: book.meta.and_then(|m| m.html_url),
                pdf_url: book.pdf_url,
                outline_url: None,
                grain: CatalogueGrain::Textbook,
                pages: None,
            })
            .collect())
    }
}

// ── MIT OpenCourseWare ───────────────────────────────────────────────────────

/// Courses, not books: this is the catalogue that answers *in what order*,
/// which is the question a subject with no existing coverage actually asks.
pub struct MitOpenCourseWare {
    base_url: String,
    http: ProviderHttpClient,
}

impl Default for MitOpenCourseWare {
    fn default() -> Self {
        Self::new("https://api.learn.mit.edu")
    }
}

impl MitOpenCourseWare {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: ProviderHttpClient::new("MIT OpenCourseWare"),
        }
    }
}

#[derive(Deserialize)]
struct MitPage {
    #[serde(default)]
    #[allow(dead_code)]
    count: i64,
    #[serde(default)]
    results: Vec<MitCourse>,
}

#[derive(Deserialize)]
struct MitCourse {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    topics: Vec<MitTopic>,
}

#[derive(Deserialize)]
struct MitTopic {
    #[serde(default)]
    name: String,
}

#[async_trait]
impl CatalogueSource for MitOpenCourseWare {
    fn id(&self) -> &'static str {
        "mit_ocw"
    }

    fn grain(&self) -> CatalogueGrain {
        CatalogueGrain::Course
    }

    async fn fetch_all(&self, progress: &FetchReporter) -> anyhow::Result<Vec<CatalogueRecord>> {
        let mut out: Vec<CatalogueRecord> = Vec::new();
        let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
        let mut dry = 0usize;
        for page in 0..MAX_PAGES {
            let offset = page * 100;
            let url = format!(
                "{}/api/v1/courses/?limit=100&offset={offset}",
                self.base_url
            );
            let body: MitPage = self.http.get_json(url, &[]).await?;
            if body.results.is_empty() {
                break;
            }
            let before = seen.len();
            for course in body.results {
                if !seen.insert(course.id) {
                    continue;
                }
                out.push(CatalogueRecord {
                    provider: "mit_ocw".into(),
                    external_id: course.id.to_string(),
                    title: clip(&course.title, 400),
                    summary: clip(&course.description, SUMMARY_CHARS),
                    subject: course
                        .topics
                        .iter()
                        .map(|t| t.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    authors: "MIT".into(),
                    license: "cc-by-nc-sa".into(),
                    landing_url: course.url.clone(),
                    pdf_url: None,
                    outline_url: course.url,
                    grain: CatalogueGrain::Course,
                    pages: None,
                });
            }
            progress.page(page + 1, out.len());
            if seen.len() == before {
                dry += 1;
                if dry >= DRY_PAGES_BEFORE_STOP {
                    break;
                }
            } else {
                dry = 0;
            }
        }
        Ok(out)
    }
}

// ── DevDocs ──────────────────────────────────────────────────────────────────

/// Documentation sets. This is the only catalogue here that can answer a gap
/// like `Python lists`, where the authoritative source is a language reference
/// and no textbook chapter is the right answer.
pub struct DevDocs {
    base_url: String,
    http: ProviderHttpClient,
}

impl Default for DevDocs {
    fn default() -> Self {
        Self::new("https://devdocs.io")
    }
}

impl DevDocs {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: ProviderHttpClient::new("DevDocs"),
        }
    }
}

#[derive(Deserialize)]
struct DevDocsEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    links: Option<DevDocsLinks>,
    #[serde(default)]
    license: Option<String>,
}

#[derive(Deserialize)]
struct DevDocsLinks {
    #[serde(default)]
    home: Option<String>,
}

#[async_trait]
impl CatalogueSource for DevDocs {
    fn id(&self) -> &'static str {
        "devdocs"
    }

    fn grain(&self) -> CatalogueGrain {
        CatalogueGrain::Reference
    }

    async fn fetch_all(&self, progress: &FetchReporter) -> anyhow::Result<Vec<CatalogueRecord>> {
        let url = format!("{}/docs.json", self.base_url);
        let body: Vec<DevDocsEntry> = self.http.get_json(url, &[]).await?;
        progress.page(1, body.len());
        Ok(body
            .into_iter()
            .map(|entry| {
                let versioned = match entry.version.as_deref() {
                    Some(v) if !v.is_empty() => format!("{} {}", entry.name, v),
                    _ => entry.name.clone(),
                };
                CatalogueRecord {
                    provider: "devdocs".into(),
                    external_id: entry.slug.clone(),
                    // The manifest carries no prose, so the searchable text has
                    // to be built. It names the thing and what kind of thing it
                    // is, which is what a reference-grain probe asks for.
                    summary: format!(
                        "Official API and language reference documentation for {}. \
                         Built-in types, standard library, syntax and semantics.",
                        entry.name
                    ),
                    title: clip(&versioned, 200),
                    subject: "reference documentation".into(),
                    authors: entry.name,
                    license: entry.license.unwrap_or_default(),
                    landing_url: entry.links.and_then(|l| l.home),
                    pdf_url: None,
                    outline_url: Some(format!(
                        "https://documents.devdocs.io/{}/index.json",
                        entry.slug
                    )),
                    grain: CatalogueGrain::Reference,
                    pages: None,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_never_splits_a_glyph() {
        // Byte-slicing this at 5 would panic; character-aware trimming does not.
        let clipped = clip("Über Ölmengen und Maßstäbe", 5);
        assert_eq!(clipped.chars().count(), 5);
        assert_eq!(clipped, "Über ");
    }

    #[test]
    fn clip_strips_the_markup_providers_embed_in_blurbs() {
        assert_eq!(
            clip("<p>A book about <i>things</i>   and\nstuff</p>", 200),
            "A book about things and stuff"
        );
    }

    #[test]
    fn openstax_subjects_read_both_wire_shapes() {
        let objects = vec![serde_json::json!({ "subject_name": "Mathematics" })];
        assert_eq!(openstax_subjects(&objects), "Mathematics");
        let strings = vec![serde_json::json!("Physics"), serde_json::json!("Chemistry")];
        assert_eq!(openstax_subjects(&strings), "Physics, Chemistry");
    }

    #[test]
    fn every_registered_provider_has_a_distinct_id() {
        let registry = registry();
        let mut ids: Vec<&str> = registry.iter().map(|source| source.id()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "catalogue provider ids must be distinct");
    }

    #[tokio::test]
    async fn libretexts_maps_a_catalogue_page_to_records() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/commons/catalog")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("limit".into(), "100".into()),
                mockito::Matcher::UrlEncoded("page".into(), "1".into()),
            ]))
            .with_status(200)
            .with_body(
                r#"{"err":false,"numTotal":1,"books":[{
                    "bookID":"math-70414","title":"Calculus","summary":"Limits and derivatives.",
                    "subject":"","library":"math","author":"P. Seeburger","license":"ccbysa",
                    "links":{"online":"https://example.invalid/online","pdf":"https://example.invalid/pdf"},
                    "exportInfo":{"contentPageCount":330}}]}"#,
            )
            .create_async()
            .await;
        // The walk stops on consecutive empty pages, so the pages after the
        // first must answer too — a provider that simply runs out is the
        // ordinary case and the test has to represent it.
        let empty = server
            .mock("GET", "/api/v1/commons/catalog")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"{"err":false,"numTotal":1,"books":[]}"#)
            .expect_at_least(1)
            .create_async()
            .await;

        let records = LibreTexts::new(&server.url())
            .fetch_all(&FetchReporter::silent())
            .await
            .expect("fetch");
        mock.assert_async().await;
        empty.assert_async().await;

        assert_eq!(records.len(), 1);
        let book = &records[0];
        assert_eq!(book.external_id, "math-70414");
        assert_eq!(book.title, "Calculus");
        // Empty `subject` falls back to the library it came from, so the
        // record still carries a searchable topic.
        assert_eq!(book.subject, "math");
        assert_eq!(book.pages, Some(330));
        assert_eq!(book.grain, CatalogueGrain::Textbook);
    }

    #[tokio::test]
    async fn a_paged_fetch_reports_each_page_as_it_lands() {
        let mut server = mockito::Server::new_async().await;
        let _first = server
            .mock("GET", "/api/v1/commons/catalog")
            .match_query(mockito::Matcher::UrlEncoded("page".into(), "1".into()))
            .with_status(200)
            .with_body(
                r#"{"err":false,"numTotal":2,"books":[
                    {"bookID":"math-1","title":"Calculus","descrip":"Limits.",
                     "subject":"math","library":"math","author":"A","license":"ccby",
                     "links":{"online":"https://example.invalid/1"}}]}"#,
            )
            .create_async()
            .await;
        let _rest = server
            .mock("GET", "/api/v1/commons/catalog")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"{"err":false,"numTotal":2,"books":[]}"#)
            .expect_at_least(1)
            .create_async()
            .await;

        let (tx, mut rx) = mpsc::channel(64);
        let records = LibreTexts::new(&server.url())
            .fetch_all(&FetchReporter::new("libretexts", tx))
            .await
            .expect("fetch");
        assert_eq!(records.len(), 1);

        // The first page is reported before the walk ends, which is the whole
        // point: a caller must not have to wait for the last page to learn that
        // the first one landed.
        let first = rx.try_recv().expect("a page must be reported");
        assert_eq!(first.provider, "libretexts");
        assert_eq!(first.pages, 1);
        assert_eq!(first.records, 1);
    }

    #[tokio::test]
    async fn a_provider_served_whole_reports_once() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/docs.json")
            .with_status(200)
            .with_body(
                r#"[{"name":"Python","slug":"python~3.12","version":"3.12",
                     "links":{"home":"https://python.org"},"license":"PSF"}]"#,
            )
            .create_async()
            .await;

        let (tx, mut rx) = mpsc::channel(64);
        DevDocs::new(&server.url())
            .fetch_all(&FetchReporter::new("devdocs", tx))
            .await
            .expect("fetch");
        let only = rx.try_recv().expect("one report");
        assert_eq!((only.pages, only.records), (1, 1));
        assert!(rx.try_recv().is_err(), "a whole catalogue is one page");
    }

    #[tokio::test]
    async fn devdocs_builds_searchable_prose_for_a_manifest_with_none() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/docs.json")
            .with_status(200)
            .with_body(
                r#"[{"name":"Python","slug":"python~3.12","version":"3.12",
                     "links":{"home":"https://python.org"},"license":"PSF"}]"#,
            )
            .create_async()
            .await;

        let records = DevDocs::new(&server.url())
            .fetch_all(&FetchReporter::silent())
            .await
            .expect("fetch");
        mock.assert_async().await;

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].title, "Python 3.12");
        assert_eq!(records[0].grain, CatalogueGrain::Reference);
        assert!(records[0].summary.contains("Python"));
        assert!(!records[0].summary.is_empty());
    }
}
