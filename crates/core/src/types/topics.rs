//! Topic clustering over chunks.

use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkTopicsQuery {
    pub root: PathBuf,
    /// When present, discover topics only within this indexed document.
    /// Absence retains the active-root cloud semantics.
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default = "chunk_topic_default_granularity")]
    pub granularity: BookmarkClusterGranularity,
}

pub(super) fn chunk_topic_default_granularity() -> BookmarkClusterGranularity {
    BookmarkClusterGranularity::MuchFewer
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkTopicMember {
    pub chunk_id: i64,
    pub file_path: PathBuf,
    pub chunk_text: String,
    pub extraction_byte_range: ByteRange,
    pub origin: SourceOrigin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkTopic {
    /// SHA-256 over the sorted member chunk ids. Reindexing naturally changes
    /// the key when membership drifts.
    pub cluster_key: String,
    pub chunks: Vec<ChunkTopicMember>,
    pub representative_chunk_id: i64,
    pub chunk_count: usize,
    pub distinct_document_count: usize,
    pub cohesion: f32,
    /// For a document-scoped topic, the number of other indexed documents
    /// containing at least one passage that meets the topic's own cohesion
    /// boundary. Root-scoped topics leave this absent because their membership
    /// already describes the selected root's distribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_coverage: Option<TopicLibraryCoverage>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TopicLibraryCoverage {
    pub related_document_count: usize,
    /// Other indexed documents across all configured library roots; the source
    /// document is deliberately excluded from both numerator and denominator.
    pub eligible_document_count: usize,
    /// The highest-similarity qualifying passages retained for each related
    /// document, ready to surface through the normal search-results pipeline.
    #[serde(default)]
    pub chunks: Vec<ChunkTopicMember>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChunkTopicsResult {
    pub topics: Vec<ChunkTopic>,
    pub total_chunk_count: usize,
    pub sampled_chunk_count: usize,
    pub total_document_count: usize,
    pub sampled_document_count: usize,
    pub input_cap: usize,
}
