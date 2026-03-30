use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── Byte range (replaces std::ops::Range<usize> for serde compat) ────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

// ── Query ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchQuery {
    pub pattern: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    pub root: PathBuf,
    pub file_type_filters: Vec<String>,
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
}

fn default_true() -> bool { true }
fn default_context_lines() -> u32 { 2 }

// ── Results ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Match {
    pub text_range: ByteRange,
    pub matched_text: String,
    pub context_before: String,
    pub context_after: String,
    pub origin: SourceOrigin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileMatches {
    pub path: PathBuf,
    pub file_type: FileType,
    pub matches: Vec<Match>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum FileType {
    PlainText,
    Pdf,
}

// ── Source Mapping ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceMap {
    pub segments: Vec<SourceSegment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceSegment {
    pub text_range: ByteRange,
    pub origin: SourceOrigin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SourceOrigin {
    TextFile { line: u32, col: u32 },
    PdfPage { page: u32, bbox: Option<BoundingBox> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl SourceMap {
    /// Resolve a byte offset in extracted text to a SourceOrigin.
    pub fn resolve(&self, offset: usize) -> Option<SourceOrigin> {
        // Walk segments to find which one contains the offset.
        // Segments should be ordered by text_range.start.
        for seg in &self.segments {
            if offset >= seg.text_range.start && offset < seg.text_range.end {
                return Some(seg.origin.clone());
            }
        }
        // Fall back to last segment
        self.segments.last().map(|s| s.origin.clone())
    }
}

// ── Extraction ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExtractedContent {
    pub text: String,
    pub source_map: SourceMap,
    pub metadata: FileMetadata,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileMetadata {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub mime: Option<String>,
    pub title: Option<String>,
    pub page_count: Option<u32>,
}

// ── Preview ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchRef {
    pub path: PathBuf,
    pub origin: SourceOrigin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PreviewData {
    Text {
        content: String,
        language: Option<String>,
        highlight_line: u32,
        highlight_range: ByteRange,
    },
    Pdf {
        page: u32,
        highlight_bbox: Option<BoundingBox>,
    },
}

// ── Settings ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub bookmarked_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub last_directory: Option<PathBuf>,
    pub respect_gitignore: bool,
    pub max_file_size: u64,
    pub context_lines: u32,
    pub theme: Theme,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bookmarked_dirs: Vec::new(),
            last_directory: None,
            respect_gitignore: true,
            max_file_size: 10 * 1024 * 1024,
            context_lines: 2,
            theme: Theme::System,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Theme {
    System,
    Light,
    Dark,
}

// ── File listing ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub file_type: FileType,
    pub extension: String,
}

// ── Capabilities ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchCapabilities {
    pub supports_regex: bool,
    pub supports_case_sensitivity: bool,
    pub is_indexed: bool,
    pub supported_file_types: Vec<String>,
}

// ── Search completion stats ───────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SearchStats {
    pub files_scanned: usize,
    pub total_matches: usize,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub errors: Vec<String>,
}
