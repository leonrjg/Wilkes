//! Document and file metadata, file listings and data paths.

use super::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub doi: Option<String>,
    pub created_at: Option<String>,
    /// What each enabled provider's `lookup` said about this document, keyed
    /// by provider id (`openalex`, `custom:crossref`). Provider-neutral: the
    /// id is the key, never a field, so a provider added tomorrow appears here
    /// with no change to this type.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub providers: std::collections::BTreeMap<String, ProviderWork>,
}

impl DocumentMetadata {
    /// How often the document is cited, by the provider that comes first in
    /// [`crate::integrations::builtin::precedence`] among those that reported
    /// a count. Providers count differently, so one is chosen rather than the
    /// counts being mixed; a provider that reported none is not one of them,
    /// and is not read as having counted zero.
    pub fn citation_count(&self) -> Option<i64> {
        self.providers
            .iter()
            .filter_map(|(id, work)| work.citation_count.map(|count| (id, count)))
            .min_by_key(|(id, _)| crate::integrations::builtin::precedence(id))
            .map(|(_, count)| count)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileMetadata {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub mime: Option<String>,
    pub title: Option<String>,
    pub page_count: Option<u32>,
}

// ── File listing ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub file_type: FileType,
    pub extension: String,
    pub created_at_ms: Option<i64>,
    pub modified_at_ms: Option<i64>,
    /// Document title from cached extracted metadata. `None` until the metadata
    /// cache has processed this file.
    #[serde(default)]
    pub title: Option<String>,
    /// Document author from cached extracted metadata. `None` until the metadata
    /// cache has processed this file.
    #[serde(default)]
    pub author: Option<String>,
    /// Document DOI from cached extracted metadata (normalized, no URL prefix).
    /// `None` until the metadata cache has processed this file or when the
    /// document carries no DOI.
    #[serde(default)]
    pub doi: Option<String>,
    /// Document publication date ("YYYY-MM") from cached extracted metadata.
    /// `None` until the metadata cache has processed this file.
    #[serde(default)]
    pub publication_date: Option<String>,
    /// Semantic Scholar citation count from cached document metadata. `None`
    /// until metadata extraction has found a DOI and the integration has
    /// enriched it.
    #[serde(default)]
    pub citation_count: Option<i64>,
    #[serde(default)]
    pub metadata_conflicts: HashMap<String, Vec<MetadataConflictValue>>,
    #[serde(default)]
    pub tags: Vec<Tag>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MetadataConflictValue {
    pub source: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileListResponse {
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub omitted: Vec<OmittedFileEntry>,
    /// Physical directories visited by the same walk. Search ignores these;
    /// the desktop uses them to render empty folders as valid move targets.
    #[serde(default)]
    pub directories: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OmittedFileEntry {
    #[serde(flatten)]
    pub file: FileEntry,
    pub reason: OmittedFileReason,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum OmittedFileReason {
    TooLarge,
    UnsupportedExtension,
}

// ── Paths ────────────────────────────────────────────────────────────────────

/// Where this installation keeps things, as the Data settings page says it.
///
/// Two paths because there are two answers, and `app_data` used to give the
/// second one under the first one's name: what the page called the
/// application's data directory was the active workspace's directory inside
/// it, so the installation root — the thing that holds `workspaces/`, the
/// model cache and the catalogue mirror — could not be named at all.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DataPaths {
    /// The installation's data directory: the one that contains `workspaces/`.
    pub app_data: String,
    /// The active workspace's own directory, under `app_data/workspaces/<id>`.
    /// Its databases — the semantic index among them — live here.
    pub workspace: String,
}
