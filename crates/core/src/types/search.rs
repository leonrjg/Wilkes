//! Search queries and their results, related documents, folder tidying, and
//! what the search runtime can do.

use super::*;

// ── Byte range (replaces std::ops::Range<usize> for serde compat) ────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

// ── Indexing configuration ────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct IndexingConfig {
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub supported_extensions: Vec<String>,
}

// ── Search mode ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum SearchMode {
    #[default]
    Grep,
    Semantic,
    /// Wording and meaning in one search: the exact lane and the semantic lane
    /// run over the same catalog and their rankings are fused. Each admitted
    /// document says which lane(s) admitted it — see [`MatchEvidence`].
    Hybrid,
}

impl SearchMode {
    /// True for the modes whose retrieval reaches the semantic index, and so
    /// require a loaded embedder and a built index to contribute anything.
    pub fn uses_semantic_index(self) -> bool {
        matches!(self, SearchMode::Semantic | SearchMode::Hybrid)
    }

    /// True for the modes that match the query text against document content
    /// literally, and so honour `is_regex` and `case_sensitive`.
    pub fn uses_exact_matching(self) -> bool {
        matches!(self, SearchMode::Grep | SearchMode::Hybrid)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchScope {
    #[default]
    Corpus,
    All,
    File {
        path: PathBuf,
    },
}

// ── Query ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchQuery {
    pub pattern: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    pub root: PathBuf,
    /// 0 = unlimited
    pub max_results: usize,
    /// Respect .gitignore / .ignore files during the walk.
    #[serde(default = "default_true")]
    pub respect_gitignore: bool,
    /// Skip files larger than this many bytes (0 = unlimited).
    #[serde(default)]
    pub max_file_size: u64,
    /// Lines of context to include around each match (text files only).
    #[serde(default = "default_context_lines")]
    pub context_lines: u32,
    /// Which search backend to use.
    #[serde(default)]
    pub mode: SearchMode,
    /// Optional result scope inside `root`.
    #[serde(default)]
    pub scope: SearchScope,
    /// The global list of supported extensions from settings.
    #[serde(default)]
    pub supported_extensions: Vec<String>,
    /// Optional saved smart collection intersected with `scope` by the backend.
    #[serde(default)]
    pub collection_id: Option<String>,
    /// Optional document tags. A document must contain every requested tag.
    #[serde(default)]
    pub tag_ids: Vec<String>,
}

pub(super) fn default_true() -> bool {
    true
}

pub(super) fn default_context_lines() -> u32 {
    2
}

// ── Results ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Match {
    /// Byte range into the extracted text.
    /// Some for plain-text files (used for highlight positioning).
    /// None for PDF chunks (highlight routes through origin.bbox instead).
    pub text_range: Option<ByteRange>,
    pub matched_text: String,
    pub context_before: String,
    pub context_after: String,
    pub origin: SourceOrigin,
    /// Cosine similarity score for semantic matches; None for grep matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileMatches {
    pub path: PathBuf,
    pub file_type: FileType,
    /// Composed cached document title when available from the search catalog.
    /// The application may refresh it again at the metadata boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Direct matches against document identity fields. Kept separate from
    /// content matches because they have no truthful line, page, or byte
    /// position inside the document.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_matches: Vec<SearchFieldMatch>,
    pub matches: Vec<Match>,
    /// Why this document is in the result set, when more than one kind of
    /// retrieval could have put it there. Populated only by
    /// [`SearchMode::Hybrid`]; a single-lane search leaves it empty because
    /// the mode already answers the question.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<MatchEvidence>,
}

impl FileMatches {
    pub fn total_match_count(&self) -> usize {
        self.field_matches.len() + self.matches.len()
    }
}

/// One retrieval lane's claim on a document, stated as the fact it establishes
/// rather than as the mechanism that found it.
///
/// `ExactPhrase` means the query text occurs in the document (or in its
/// filename, title or author) as written. `RelatedPassage` means a passage of
/// the document is a nearest neighbour of the query in embedding space — it
/// discusses the subject, in whatever words it uses. A document can carry both,
/// and that is the case the combined mode exists to surface.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum MatchEvidence {
    ExactPhrase,
    RelatedPassage,
}

/// One document admitted by the application's authoritative search catalog.
/// Providers search this list instead of independently walking the filesystem.
#[derive(Clone, Debug)]
pub struct SearchDocument {
    pub path: PathBuf,
    pub file_type: FileType,
    pub title: Option<String>,
    pub author: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchField {
    Filename,
    Title,
    Author,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchFieldMatch {
    pub field: SearchField,
    pub matched_text: String,
    pub context_before: String,
    pub context_after: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelatedDocumentsQuery {
    pub root: PathBuf,
    pub path: PathBuf,
    #[serde(default)]
    pub scope: SearchScope,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub collection_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelatedDocument {
    #[serde(flatten)]
    pub entry: FileEntry,
    pub score: f32,
}

/// Suggested homes for the documents directly inside one library folder.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TidyPlan {
    pub folder: PathBuf,
    /// Every folder a document could be moved to, in path order: each library
    /// root and folder beneath one, except the folder being tidied and any
    /// folder that holds only folders.
    pub destinations: Vec<TidyDestination>,
    /// One per document of the folder that the semantic index holds, in path
    /// order.
    pub suggestions: Vec<TidySuggestion>,
    /// Documents of the folder the semantic index does not hold. With nothing
    /// to compare them by, they get no suggestion.
    pub unindexed: Vec<FileEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TidyDestination {
    pub path: PathBuf,
    /// Documents directly in the folder.
    pub documents: usize,
    /// How many of those the semantic index holds. Only these say what the
    /// folder is used for; a folder with none is judged by its name alone.
    pub indexed_documents: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TidySuggestion {
    pub entry: FileEntry,
    /// The best few destinations, best first. Never empty.
    pub ranking: Vec<TidyPlacement>,
    /// The best destination barely leads the next: the document fits both.
    pub close_call: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TidyPlacement {
    pub destination: PathBuf,
    /// Standard deviations above the other destinations, averaged over the
    /// signals the destination has. Comparable within one suggestion only.
    pub score: f32,
}

// ── Capabilities ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchCapabilities {
    pub supports_regex: bool,
    pub supports_case_sensitivity: bool,
    pub is_indexed: bool,
    pub supported_file_types: Vec<String>,
    /// True if this provider requires a pre-built index.
    #[serde(default)]
    pub requires_index: bool,
    /// True if the semantic index has been built and is ready.
    #[serde(default)]
    pub semantic_index_built: bool,
    /// List of embedding engines compiled into the app.
    #[serde(default)]
    pub supported_engines: Vec<EmbeddingEngine>,
}

// ── Search completion stats ───────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SearchStats {
    /// Number of supported files whose contents were actually searched. This
    /// includes files that produced no matches and excludes policy-filtered
    /// files that were never opened.
    pub files_scanned: usize,
    pub total_matches: usize,
    /// Time spent enumerating and filtering documents before the search worker
    /// starts. Kept separate from `elapsed_ms`, which remains worker time.
    #[serde(default)]
    pub catalog_elapsed_ms: u64,
    pub elapsed_ms: u64,
    /// PDFs served from stored semantic-index text during an exact search.
    #[serde(default)]
    pub indexed_pdf_reads: usize,
    /// PDFs extracted live because indexed text was disabled or unavailable.
    #[serde(default)]
    pub live_pdf_fallbacks: usize,
    /// Live PDF fallbacks caused by an enabled but non-resident index.
    #[serde(default)]
    pub index_unavailable_fallbacks: usize,
    #[serde(default)]
    pub errors: Vec<String>,
    /// Exact generated passages whose embeddings contributed to the final
    /// semantic query vector. Empty for grep, disabled HyDE, or degradation to
    /// the raw query.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hyde_documents: Vec<String>,
}
