//! Tags, smart collections, the search log and bookmarks.

use super::*;

// ── Research state ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tag {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewTag {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateTag {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentTagUpdate {
    pub paths: Vec<PathBuf>,
    #[serde(default)]
    pub add_tag_ids: Vec<String>,
    #[serde(default)]
    pub remove_tag_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmartCollection {
    pub id: String,
    pub name: String,
    pub expression: String,
    pub filter_schema_version: i64,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewSmartCollection {
    pub name: String,
    pub expression: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateSmartCollection {
    pub name: String,
    pub expression: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CollectionValidation {
    pub valid: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchLogStatus {
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchLogEntry {
    pub id: String,
    pub query: SearchQuery,
    pub collection_name: Option<String>,
    pub collection_revision: Option<i64>,
    pub initiated_by: String,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub result_count: usize,
    pub duration_ms: Option<u64>,
    pub status: SearchLogStatus,
    pub error_message: Option<String>,
}

// ── Preview ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchRef {
    pub path: PathBuf,
    pub origin: SourceOrigin,
    pub text_range: Option<ByteRange>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub path: PathBuf,
    pub origin: SourceOrigin,
    #[serde(default)]
    pub text_range: Option<ByteRange>,
    pub quote: String,
    pub created_at: String,
    #[serde(default)]
    pub note: Option<String>,
    /// Per-line rectangles (page coordinates) covering exactly the selected
    /// text. Empty for text bookmarks.
    #[serde(default)]
    pub rects: Vec<BoundingBox>,
    /// Content fingerprint of the bookmarked file, captured at creation. Lets a
    /// bookmark survive a rename: when `path` goes missing, the current path is
    /// re-resolved from this identity via the metadata cache. `None` only when
    /// the file could not be stat-ed at creation.
    #[serde(default)]
    pub identity: Option<crate::metadata::cache::FileIdentity>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookmarkClusterGranularity {
    MuchFewer,
    Fewer,
    #[default]
    Balanced,
    More,
    MuchMore,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BookmarkClustersQuery {
    pub bookmark_ids: Vec<String>,
    #[serde(default)]
    pub granularity: BookmarkClusterGranularity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BookmarkCluster {
    /// Content-derived identity: `sha256` over the sorted member
    /// `bookmark_id:input_hash` pairs. Clusters are recomputed on every call
    /// and have no persistent id, so a label cannot be keyed to the cluster
    /// itself — only to its membership. This is also the only stable handle the
    /// UI can patch a late-arriving label against; `representative_bookmark_id`
    /// moves when granularity changes.
    pub cluster_key: String,
    pub bookmark_ids: Vec<String>,
    pub representative_bookmark_id: String,
    pub cohesion: f32,
    /// `None` until the label has been generated, or forever if generation is
    /// disabled. A missing label is not an error.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BookmarkClustersResult {
    pub clusters: Vec<BookmarkCluster>,
    pub unclustered_bookmark_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewBookmark {
    pub path: PathBuf,
    pub origin: SourceOrigin,
    #[serde(default)]
    pub text_range: Option<ByteRange>,
    pub quote: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub rects: Vec<BoundingBox>,
}
