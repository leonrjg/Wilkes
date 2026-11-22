//! Reading order, citation links and the citation graph.

use super::*;

/// Which folder to put in reading order, and optionally a theme to rank its
/// documents by instead.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadingOrderQuery {
    /// A library root or a folder beneath one.
    pub root: PathBuf,
    /// One to four words. When given, every document with text also says how
    /// much of it discusses the theme.
    #[serde(default)]
    pub theme: Option<String>,
}

/// The documents of a folder in the order they build on one another, as
/// [`crate::reading_order`] reads it off their text.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadingOrder {
    pub root: PathBuf,
    /// Every document of the folder: first to last, then the ones to read at
    /// any time, then the ones that could not be placed.
    pub documents: Vec<ReadingOrderDocument>,
    /// The subjects the folder's documents are named by, with how many of
    /// them have or name each, most first.
    pub subjects: Vec<ReadingOrderSubject>,
    /// The theme as it was read, when one was asked for.
    #[serde(default)]
    pub theme: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadingOrderDocument {
    pub path: PathBuf,
    pub placement: ReadingPlacement,
    /// The subjects its title, file name and folders name it by and its own
    /// text uses.
    pub subjects: Vec<String>,
    /// The documents it builds on, earliest in the order first.
    pub after: Vec<ReadingOrderLink>,
    /// How many documents of the folder build on it.
    pub needed_by: usize,
    /// The share of its passages that use the theme. `None` without a theme,
    /// and for a document with no text.
    #[serde(default)]
    pub theme_share: Option<f32>,
}

/// Where a document stands among the others of its folder.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadingPlacement {
    /// Others build on it and it builds on none of them.
    Foundation,
    /// It builds on some and others build on it.
    Bridge,
    /// It builds on others and none builds on it.
    Specialised,
    /// It has a subject, and nothing ties it to another document: read it at
    /// any time.
    Standalone,
    /// Not placed: its title and folders name nothing its text uses, and it
    /// names no other document's subject.
    NoSubject,
    /// Not placed: its text has not been read, or it has none.
    NoText,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadingOrderLink {
    pub path: PathBuf,
    /// Never empty.
    pub reasons: Vec<ReadingOrderReason>,
}

/// Why a document comes after another.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadingOrderReason {
    /// Its text names these subjects of the other.
    Names { subjects: Vec<String> },
    /// Same subject, and its title or folder puts it at a higher level.
    Level,
    /// Same subject, and it is a later volume.
    Volume,
    /// Its subject is a narrower case of the other's.
    Narrower,
    /// It cites the other.
    Cites,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadingOrderSubject {
    pub name: String,
    pub documents: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CitationLinksQuery {
    pub root: PathBuf,
    pub path: PathBuf,
}

/// Citation neighbours of a document that are present in the library, resolved
/// by DOI. `references` are documents the anchor cites; `cited_by` are
/// documents that cite the anchor. `all_references` contains every outgoing
/// work known to the citation provider, including works absent from the
/// library, with the bibliographic fields the provider returned for it. Both
/// library directions carry the same metadata-enriched [`FileEntry`] shape as
/// every other document list.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CitationLinks {
    pub references: Vec<FileEntry>,
    pub cited_by: Vec<FileEntry>,
    #[serde(default)]
    pub all_references: Vec<CitationReference>,
}

/// One work a document cites, as the citation provider resolved it: the
/// normalized DOI that keys the edge, and what the provider said about the
/// work in the same batched request. Every field but the DOI is the provider's
/// to know; `None` is "the provider did not say", never an empty value.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CitationReference {
    pub doi: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Display names, comma-separated, as the provider lists them.
    #[serde(default)]
    pub authors: Option<String>,
    #[serde(default)]
    pub year: Option<i64>,
    #[serde(default)]
    pub venue: Option<String>,
}

/// Where a citation graph reaches: one library root, or every root of the
/// workspace when `root` is absent.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CitationGraphQuery {
    #[serde(default)]
    pub root: Option<PathBuf>,
    /// Works outside the scope are listed only when at least this many
    /// documents in it cite them. 0 is read as 1.
    #[serde(default = "default_min_external_citations")]
    pub min_external_citations: usize,
}

pub(super) fn default_min_external_citations() -> usize {
    2
}

/// The citation graph of every document in a scope, built from the same
/// DOI-keyed edges as [`CitationLinks`].
///
/// A document is a node by its DOI, so copies of one paper are one node with
/// several entries. Documents without a DOI cannot be joined and are listed
/// apart. `coverage` says how much of the scope the graph actually covers.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CitationGraph {
    pub documents: Vec<CitationGraphDocument>,
    /// `source` cites `target`; both index `documents`.
    pub edges: Vec<CitationGraphEdge>,
    /// Works outside the scope cited by at least `min_external_citations` of
    /// its documents, most cited first.
    pub external: Vec<ExternalCitedWork>,
    /// How many distinct works outside the scope its documents cite, before
    /// the threshold.
    pub external_total: usize,
    /// Documents whose metadata has been read and carries no DOI.
    pub without_doi: Vec<FileEntry>,
    pub coverage: CitationCoverage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CitationGraphDocument {
    pub doi: String,
    /// Every file in the scope with this DOI. Never empty.
    pub entries: Vec<FileEntry>,
    /// Indices into `documents` of the documents in the scope that cite it.
    pub cited_by: Vec<usize>,
    /// Whether a provider has answered for this work's references. `false`
    /// means its outgoing edges are unknown, not that it cites nothing.
    pub references_fetched: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CitationGraphEdge {
    pub source: usize,
    pub target: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalCitedWork {
    pub reference: CitationReference,
    /// Indices into `documents` of the documents in the scope that cite it.
    pub cited_by: Vec<usize>,
    /// The file, when the work is in the library but outside this scope.
    pub library_entry: Option<FileEntry>,
}

/// How much of a scope the citation graph covers.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CitationCoverage {
    /// Files in the scope.
    pub files: usize,
    /// Files whose metadata has not been read yet; they are being read.
    pub pending_metadata: usize,
    /// Files with no modification time, whose metadata is never read.
    pub metadata_unavailable: usize,
    /// Distinct DOIs among the files: the graph's documents.
    pub with_doi: usize,
    /// Documents whose references a provider has answered for.
    pub references_fetched: usize,
    /// Whether any enabled provider can fetch references at all.
    pub references_provider: bool,
}
