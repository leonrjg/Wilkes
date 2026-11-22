//! Literature search, acquisition, saved works and the catalogue.

use super::*;

/// One work as an external provider's `lookup` capability describes it, which
/// is what the metadata cache stores about a file once the file has a DOI.
///
/// Provider-neutral: the provider that produced it is the key it is stored and
/// listed under, never a field of it, so the same shape holds whatever service
/// answered.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderWork {
    pub doi: String,
    /// The provider's own identifier for the work.
    pub work_id: String,
    pub title: Option<String>,
    pub year: Option<i64>,
    pub publication_date: Option<String>,
    pub venue: Option<String>,
    /// None when the provider reported no count, which is not a count of zero.
    pub citation_count: Option<i64>,
    pub external_ids: HashMap<String, serde_json::Value>,
    pub cached_at_ms: i64,
}

/// One filter's value in a [`BrowseRequest`]: one string, or several.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum FilterValue {
    One(String),
    Many(Vec<String>),
}

impl FilterValue {
    /// The values given, blanks dropped: a blank is "not set", never a value.
    pub fn values(&self) -> Vec<&str> {
        let all: Vec<&str> = match self {
            Self::One(value) => vec![value.as_str()],
            Self::Many(values) => values.iter().map(String::as_str).collect(),
        };
        all.into_iter()
            .filter(|value| !value.trim().is_empty())
            .collect()
    }
}

/// One entry a provider suggested for a filter, as the user typed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FilterSuggestion {
    /// What the filter is set to if this is chosen.
    pub value: String,
    pub label: String,
    /// Tells two entries of one name apart: an author's institution, a
    /// journal's publisher.
    pub hint: Option<String>,
}

/// One page of a provider's listing.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BrowsePage {
    pub results: Vec<LiteratureSearchResult>,
    /// How many records match in all, when the provider says.
    pub total: Option<u64>,
}

/// A request to list a provider's works by filter and sort rather than by
/// keyword alone. What may be asked is declared by the provider's `browse`
/// capability; anything else is refused by name.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowseRequest {
    /// A keyword to combine with the filters. Empty means none.
    #[serde(default)]
    pub query: Option<String>,
    /// Filter name → value, or values. An on/off filter takes `"true"`; any
    /// other takes what its declaration admits. Several values need a filter
    /// that declares modes.
    #[serde(default)]
    pub filters: std::collections::BTreeMap<String, FilterValue>,
    /// Filter name → how its values are combined, one of the modes that
    /// filter declares. Absent means the filter's first.
    #[serde(default)]
    pub modes: std::collections::BTreeMap<String, String>,
    /// One of the provider's sort option values. Empty means its default.
    #[serde(default)]
    pub sort: Option<String>,
    /// 1-based.
    #[serde(default)]
    pub page: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Provider-neutral result returned by a literature source: a remote index's
/// answer, or a record of the local catalogue mirror.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LiteratureSearchResult {
    pub id: String,
    pub doi: Option<String>,
    pub title: Option<String>,
    pub year: Option<i64>,
    pub publication_date: Option<String>,
    pub venue: Option<String>,
    /// None when the provider reported no count, which is not a count of zero.
    pub citation_count: Option<i64>,
    pub is_open_access: bool,
    pub pdf_url: Option<String>,
    pub landing_page_url: Option<String>,
    /// Optional image supplied by the provider, shown beside the result.
    #[serde(default)]
    pub image_url: Option<String>,
    pub open_access_status: Option<String>,
    pub license: Option<String>,
    #[serde(default)]
    pub authors: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub file_format: Option<String>,
    #[serde(default)]
    pub file_size: Option<String>,
    /// What the work says it is about, in its publisher's words. A textbook
    /// is chosen by this; a bibliographic index rarely returns one.
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    /// What kind of thing the result is, where the source says: a learning
    /// catalogue's grain (`textbook`, `course`, `reference`). Absent for a
    /// source that indexes works of every kind and names none.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub pages: Option<i64>,
    /// How well the result matched, for a source that scores its own answer:
    /// larger is better, and comparable only between sources answered from the
    /// same index. Absent for a remote service, whose order is all it reports.
    #[serde(default)]
    pub recall_score: Option<f64>,
    /// How the selected result becomes a file. Provider resolution is
    /// deliberately on-demand: listing ten results must not spend ten
    /// credentialed requests before the user chooses one.
    #[serde(default)]
    pub acquisition: LiteratureAcquisition,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LiteratureAcquisition {
    #[default]
    None,
    Direct,
    Provider,
    /// Many files and a document describing them, assembled from the
    /// publisher's own pages. Not served whole at any URL, so it is neither
    /// `Direct` nor something a resolver can turn into one.
    Course,
}

/// A provider's on-demand answer to “what file does this result name?”. The
/// caller passes this to the one existing downloader; providers never write.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedLiteratureDownload {
    pub url: String,
    pub filename: Option<String>,
}

/// How a selected result becomes files: a URL for the one existing
/// downloader, or a program a manifest names that writes them itself.
///
/// Deliberately not serializable. A program and its arguments are produced
/// from a saved manifest and run by the backend that produced them; no client
/// ever receives one, so no client request can hand one back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadPlan {
    Fetch(ResolvedLiteratureDownload),
    Run(ProgramRun),
}

/// One program invocation, with every value but the output directory written.
///
/// Its `Debug` names the program and nothing else: arguments and variables
/// may hold a secret's value.
#[derive(Clone, PartialEq, Eq)]
pub struct ProgramRun {
    /// As the manifest names it, which is the name the user allows.
    pub program: String,
    pub args: Vec<ProgramArg>,
    /// Variables set beside the ones the program inherits.
    pub env: Vec<(String, String)>,
    /// Every secret value the run carries, for redacting what the program
    /// says back: a program that fails may echo its own arguments.
    pub secrets: Vec<String>,
}

impl std::fmt::Debug for ProgramRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgramRun")
            .field("program", &self.program)
            .finish_non_exhaustive()
    }
}

/// One argument, as the pieces between its `{output_dir}`s.
///
/// Held in pieces rather than as a string with the placeholder left in, so a
/// result value that happens to read `{output_dir}` stays that text and is
/// never taken for where the program writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramArg(pub Vec<String>);

impl ProgramArg {
    pub fn write(&self, output_dir: &str) -> String {
        self.0.join(output_dir)
    }
}

/// A literature result kept for later: a work the user has found but does not
/// hold yet.
///
/// Not a [`Bookmark`]. A bookmark points into a file of the library; a saved
/// work is known only by the provider that returned it, so it carries the
/// result itself — copied when it was saved, because the provider may be
/// unreachable, or a mirrored catalogue re-synced under new ids, when the user
/// comes back to it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedWork {
    pub id: String,
    /// The provider that returned the work, as the registry names it.
    pub provider: String,
    pub work: LiteratureSearchResult,
    #[serde(default)]
    pub note: Option<String>,
    pub saved_at: String,
    /// Where the work landed once it was added to the library. A saved work
    /// stays saved after it is acquired, marked by this.
    #[serde(default)]
    pub acquired: Option<SavedWorkAcquisition>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedWorkAcquisition {
    pub path: PathBuf,
    /// Content fingerprint of the acquired file, so the mark follows a rename
    /// exactly as a bookmark does. `None` only when the file could not be
    /// stat-ed when it was recorded.
    #[serde(default)]
    pub identity: Option<crate::metadata::cache::FileIdentity>,
    pub acquired_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewSavedWork {
    pub provider: String,
    pub work: LiteratureSearchResult,
    #[serde(default)]
    pub note: Option<String>,
}

/// What kind of source a catalogue record is, which decides what it can
/// answer. A gap in an API's behaviour is answered by `Reference` and never
/// well by `Textbook`; a subject with no ordering yet is answered by `Course`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CatalogueGrain {
    /// A concept built up from its prerequisites: an open textbook.
    Textbook,
    /// A subject with a sequence — a syllabus, lecture notes, a course.
    Course,
    /// An API, a language construct, a standard. Authoritative documentation,
    /// where no textbook chapter is the right answer.
    Reference,
}

impl CatalogueGrain {
    pub fn as_str(self) -> &'static str {
        match self {
            CatalogueGrain::Textbook => "textbook",
            CatalogueGrain::Course => "course",
            CatalogueGrain::Reference => "reference",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "textbook" => Some(CatalogueGrain::Textbook),
            "course" => Some(CatalogueGrain::Course),
            "reference" => Some(CatalogueGrain::Reference),
            _ => None,
        }
    }
}

/// One acquirable teaching resource, as the catalogue mirror stores it.
///
/// The mirror's row, not what a search returns: a catalogue is searched as a
/// [`crate::integrations::LiteratureSource`] like any other, and answers in
/// [`LiteratureSearchResult`]s.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CatalogueRecord {
    pub provider: String,
    pub external_id: String,
    pub title: String,
    /// The blurb. This is what a caller's probe is matched against, so a
    /// record without one is close to unfindable.
    pub summary: String,
    pub subject: String,
    pub authors: String,
    pub license: String,
    pub landing_url: Option<String>,
    /// Present only where the provider serves the whole work at a stable URL.
    /// Its absence is why admission stays a separate decision from discovery.
    pub pdf_url: Option<String>,
    pub outline_url: Option<String>,
    pub grain: CatalogueGrain,
    pub pages: Option<i64>,
    /// The cover or picture the provider shows for the work. Optional by
    /// design: DevDocs publishes none, and a few OpenStax books have no cover.
    pub image_url: Option<String>,
}

/// How a record can be fetched, if it can be.
///
/// Derived from the record rather than stored on it: it is a fact about which
/// provider published it and what that provider serves, and a column would be
/// a second copy of the registry that could disagree with the registry.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogueAcquisition {
    /// One URL, one file. The ordinary case, and what `pdf_url` means.
    File,
    /// Many files and a course document assembled from the provider's own
    /// pages. A course is not served whole at any URL, which is exactly why
    /// its `pdf_url` is absent and why offering it the file button would be
    /// offering a click that could only fail.
    Course,
    /// Discovery only: this record can be read where it lives and nowhere else.
    None,
}

/// One catalogue record matched against a text query, with the *recall* score
/// that surfaced it.
///
/// The score is BM25 over title, subject and summary, and is explicitly not a
/// ranking a caller should consume. It exists to cut thousands of records down
/// to a few dozen that the caller can then rank properly against something
/// Wilkes does not know about — which learner is asking, and what they already
/// know. Wilkes cannot answer that and does not pretend to.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueHit {
    #[serde(flatten)]
    pub record: CatalogueRecord,
    pub recall_score: f64,
    /// What clicking "add" on this record would do. Computed here so a caller
    /// does not have to re-derive it from the provider id, which is how one
    /// consumer comes to offer a button another does not.
    pub acquisition: CatalogueAcquisition,
}

/// One query's recall, with the terms it was actually run with.
///
/// The terms are returned because an empty `hits` has two causes that a caller
/// must not confuse: nothing in the mirror matched, or nothing in the query
/// survived stopword and length filtering. The second is ordinary — a probe of
/// `C` or `the` reduces to no terms at all — and only the store knows it
/// happened, since reconstructing it outside would mean a second copy of the
/// filtering rules.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueRecall {
    /// What the FTS query was built from, in the order the terms were taken.
    pub terms: Vec<String>,
    pub hits: Vec<CatalogueHit>,
}

/// How far one provider's fetch has got.
///
/// `records` rather than a percentage: none of these catalogues says how much
/// it holds before it has been walked, and the one that publishes a total —
/// LibreTexts' `numTotal` — is wrong by about 1,700 books. A count that rises
/// is honest; a bar claiming to know the end would not be.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueFetchProgress {
    pub provider: String,
    /// Requests completed against this provider. One for a catalogue served
    /// whole, one per page for a paged one.
    pub pages: usize,
    pub records: usize,
}

/// How far acquiring one course has got.
///
/// Two counters rather than one because the two halves fail differently and
/// take different lengths of time: reading the manifest is a few dozen small
/// JSON requests that either work or leave nothing to download, and fetching
/// the documents is minutes of bytes. A caller shown only "3 of 22" during
/// the second half would have nothing at all to show during the first.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueCourseProgress {
    /// The course being acquired, as it was asked for.
    pub course_url: String,
    pub stage: CatalogueCourseStage,
    /// Manifest entries read, or documents written, depending on `stage`.
    pub done: usize,
    /// How many there are in total, once that is known. Absent while reading
    /// the manifest, because the number of entries is the manifest's first
    /// answer and not something that can be assumed before it arrives.
    pub total: Option<usize>,
    /// What is being read or written right now, for a caller that wants to
    /// name it rather than only count it.
    pub current: Option<String>,
}

/// Which half of a course acquisition a report belongs to.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogueCourseStage {
    /// Walking `content_map.json` and the `data.json` it names.
    Manifest,
    /// Fetching the documents the manifest turned out to name.
    Documents,
}

/// What the mirror holds for a source that is answered from it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MirrorStatus {
    /// What kind of source this is; every record it supplies is of this kind.
    pub grain: CatalogueGrain,
    pub records: i64,
    /// Null when it has never been fetched, which is also what makes the
    /// source one that is not searched.
    pub synced_at_ms: Option<i64>,
}

/// Per-provider state of the local catalogue mirror.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueProviderStatus {
    pub provider: String,
    pub grain: CatalogueGrain,
    pub records: i64,
    pub synced_at_ms: Option<i64>,
}
