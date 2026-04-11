use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// ── Byte range (replaces std::ops::Range<usize> for serde compat) ────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub enum SearchMode {
    #[default]
    Grep,
    Semantic,
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
    /// Which search backend to use.
    #[serde(default)]
    pub mode: SearchMode,
    /// The global list of supported extensions from settings.
    #[serde(default)]
    pub supported_extensions: Vec<String>,
}

fn default_true() -> bool {
    true
}
fn default_context_lines() -> u32 {
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
    pub matches: Vec<Match>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileType {
    PlainText,
    Pdf,
}

impl FileType {
    pub fn detect(path: &std::path::Path, supported_extensions: &[String]) -> Option<Self> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());

        if let Some(ext) = &ext {
            if supported_extensions
                .iter()
                .any(|s| s.to_ascii_lowercase() == *ext)
            {
                if ext == "pdf" {
                    return Some(FileType::Pdf);
                } else {
                    return Some(FileType::PlainText);
                }
            }
        }

        // Special case: check well-known filenames if no extension or unknown extension
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let name_lc = name.to_ascii_lowercase();
            if [
                "makefile",
                "dockerfile",
                "jenkinsfile",
                "procfile",
                "gemfile",
                "rakefile",
                "vagrantfile",
                "podfile",
                "brewfile",
            ]
            .contains(&name_lc.as_str())
            {
                return Some(FileType::PlainText);
            }
        }
        None
    }
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
    TextFile {
        line: u32,
        col: u32,
    },
    PdfPage {
        page: u32,
        bbox: Option<BoundingBox>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl BoundingBox {
    pub fn merge(&self, other: &Self) -> Self {
        let x1 = self.x.min(other.x);
        let y1 = self.y.min(other.y);
        let x2 = (self.x + self.width).max(other.x + other.width);
        let y2 = (self.y + self.height).max(other.y + other.height);
        BoundingBox {
            x: x1,
            y: y1,
            width: (x2 - x1).max(0.0),
            height: (y2 - y1).max(0.0),
        }
    }
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

    /// Resolve a byte range in extracted text to a merged SourceOrigin.
    /// If the range spans multiple PDF segments on the same page, their bboxes are merged.
    pub fn resolve_range(&self, range: ByteRange) -> Option<SourceOrigin> {
        let mut merged_bbox: Option<BoundingBox> = None;
        let mut page_num: Option<u32> = None;
        let mut first_origin: Option<SourceOrigin> = None;

        for seg in &self.segments {
            // Check if segment overlaps with the range
            if seg.text_range.start < range.end && seg.text_range.end > range.start {
                if first_origin.is_none() {
                    first_origin = Some(seg.origin.clone());
                }

                if let SourceOrigin::PdfPage { page, bbox } = &seg.origin {
                    if let Some(p) = page_num {
                        if p != *page {
                            // If match spans multiple pages, we stick to segments on the first page
                            // that overlaps with the match start.
                            continue;
                        }
                    } else {
                        page_num = Some(*page);
                    }

                    if let Some(b) = bbox {
                        merged_bbox = match merged_bbox {
                            Some(existing) => Some(existing.merge(b)),
                            None => Some(b.clone()),
                        };
                    }
                }
            }
        }

        if let Some(p) = page_num {
            Some(SourceOrigin::PdfPage {
                page: p,
                bbox: merged_bbox,
            })
        } else {
            first_origin.or_else(|| self.resolve(range.start))
        }
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
    pub text_range: Option<ByteRange>,
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

// ── Embedder model ────────────────────────────────────────────────────────────

/// Identifies an embedding model. For fastembed models this is the Debug representation
/// of the `EmbeddingModel` enum variant (e.g. "BGEBaseENV15"); for SBERT/Candle models
/// it is the HuggingFace model code. Serialises as a plain string.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(transparent)]
pub struct EmbedderModel(pub String);

impl EmbedderModel {
    pub fn model_id(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for EmbedderModel {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(EmbedderModel(s))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SelectedEmbedder {
    pub engine: EmbeddingEngine,
    pub model: EmbedderModel,
    pub dimension: usize,
}

impl SelectedEmbedder {
    pub fn default_for(engine: EmbeddingEngine) -> Self {
        Self {
            engine,
            model: EmbedderModel(engine.default_model().to_string()),
            dimension: 384,
        }
    }
}

impl Default for SelectedEmbedder {
    fn default() -> Self {
        Self::default_for(EmbeddingEngine::default())
    }
}

// ── Model descriptor (returned by list_models) ────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub model_id: String,
    pub display_name: String,
    pub description: String,
    pub dimension: usize,
    pub is_cached: bool,
    pub is_default: bool,
    pub is_recommended: bool,
    /// Total bytes of all model files. Populated from disk for cached models;
    /// `None` for uncached models until explicitly fetched from HuggingFace.
    pub size_bytes: Option<u64>,
    /// How many texts to embed at once. `None` means process all texts as one batch
    /// (required for some quantized models to ensure consistent results).
    pub preferred_batch_size: Option<usize>,
}

// ── Embedding engine ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum EmbeddingEngine {
    SBERT,
    Candle,
    #[default]
    Fastembed,
}

impl EmbeddingEngine {
    pub fn as_str(&self) -> &'static str {
        match self {
            EmbeddingEngine::SBERT => "sbert",
            EmbeddingEngine::Candle => "candle",
            EmbeddingEngine::Fastembed => "fastembed",
        }
    }

    /// Default device string for this engine. Used when no explicit override is set.
    pub fn default_device(&self) -> &'static str {
        match self {
            EmbeddingEngine::SBERT => "auto",
            EmbeddingEngine::Candle => "auto",
            EmbeddingEngine::Fastembed => "cpu",
        }
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            EmbeddingEngine::SBERT => "intfloat/e5-small-v2",
            EmbeddingEngine::Candle => "sentence-transformers/all-MiniLM-L6-v2",
            EmbeddingEngine::Fastembed => "AllMiniLML6V2",
        }
    }

    pub fn supports_custom_models(&self) -> bool {
        match self {
            EmbeddingEngine::SBERT => true,
            EmbeddingEngine::Candle => true,
            EmbeddingEngine::Fastembed => false,
        }
    }

    pub fn supported_engines() -> Vec<Self> {
        let mut engines = vec![EmbeddingEngine::SBERT];
        #[cfg(feature = "candle")]
        engines.push(EmbeddingEngine::Candle);
        #[cfg(feature = "fastembed")]
        engines.push(EmbeddingEngine::Fastembed);
        engines
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CustomModel {
    pub engine: EmbeddingEngine,
    pub model_id: String,
}

// ── Semantic settings ─────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize)]
pub struct SemanticSettings {
    pub enabled: bool,
    #[serde(default = "SemanticSettings::default_selected")]
    pub selected: SelectedEmbedder,
    /// Per-engine device overrides ("auto", "cpu", "mps", "cuda").
    /// Missing entries fall back to each engine's own default_device().
    #[serde(default)]
    pub engine_devices: HashMap<EmbeddingEngine, String>,
    pub index_path: Option<PathBuf>,
    /// List of arbitrary HuggingFace IDs manually added by the user, scoped by engine.
    #[serde(default, deserialize_with = "deserialize_custom_models")]
    pub custom_models: Vec<CustomModel>,
    #[serde(default = "SemanticSettings::default_chunk_size")]
    pub chunk_size: usize,
    #[serde(default = "SemanticSettings::default_chunk_overlap")]
    pub chunk_overlap: usize,
    /// Idle timeout for worker processes in seconds.
    #[serde(default = "SemanticSettings::default_worker_timeout")]
    pub worker_timeout_secs: u64,
}

#[derive(Deserialize)]
struct SemanticSettingsSerde {
    enabled: bool,
    #[serde(default = "SemanticSettings::default_selected")]
    selected: SelectedEmbedder,
    #[serde(default)]
    engine_devices: HashMap<EmbeddingEngine, String>,
    index_path: Option<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_custom_models")]
    custom_models: Vec<CustomModel>,
    #[serde(default = "SemanticSettings::default_chunk_size")]
    chunk_size: usize,
    #[serde(default = "SemanticSettings::default_chunk_overlap")]
    chunk_overlap: usize,
    #[serde(default = "SemanticSettings::default_worker_timeout")]
    worker_timeout_secs: u64,
}

#[derive(Deserialize)]
struct LegacySemanticSettingsSerde {
    enabled: bool,
    #[serde(default)]
    engine: EmbeddingEngine,
    #[serde(default = "SemanticSettings::default_model")]
    model: EmbedderModel,
    #[serde(default = "SemanticSettings::default_dimension")]
    dimension: usize,
    #[serde(default)]
    engine_devices: HashMap<EmbeddingEngine, String>,
    index_path: Option<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_custom_models")]
    custom_models: Vec<CustomModel>,
    #[serde(default = "SemanticSettings::default_chunk_size")]
    chunk_size: usize,
    #[serde(default = "SemanticSettings::default_chunk_overlap")]
    chunk_overlap: usize,
    #[serde(default = "SemanticSettings::default_worker_timeout")]
    worker_timeout_secs: u64,
}

fn deserialize_custom_models<'de, D>(deserializer: D) -> Result<Vec<CustomModel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;

    if let Some(arr) = value.as_array() {
        let mut result = Vec::new();
        for item in arr {
            if let Some(s) = item.as_str() {
                // Migration: old Vec<String> format. Default to SBERT.
                result.push(CustomModel {
                    engine: EmbeddingEngine::SBERT,
                    model_id: s.to_string(),
                });
            } else if let Ok(custom) = serde_json::from_value::<CustomModel>(item.clone()) {
                result.push(custom);
            } else {
                return Err(D::Error::custom("Invalid custom_model format"));
            }
        }
        Ok(result)
    } else {
        Ok(Vec::new())
    }
}

impl SemanticSettings {
    fn default_selected() -> SelectedEmbedder {
        SelectedEmbedder::default_for(EmbeddingEngine::default())
    }

    fn default_model() -> EmbedderModel {
        EmbedderModel(EmbeddingEngine::default().default_model().to_string())
    }

    fn default_chunk_size() -> usize {
        600
    }

    fn default_chunk_overlap() -> usize {
        128
    }

    fn default_worker_timeout() -> u64 {
        300
    }

    fn default_dimension() -> usize {
        384 // Default for AllMiniLML6V2
    }

    /// Returns the effective device string for the given engine,
    /// falling back to that engine's built-in default when no override is set.
    pub fn device_for(&self, engine: EmbeddingEngine) -> &str {
        self.engine_devices
            .get(&engine)
            .map(String::as_str)
            .unwrap_or_else(|| engine.default_device())
    }
}

impl<'de> Deserialize<'de> for SemanticSettings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("selected").is_some() {
            let parsed = serde_json::from_value::<SemanticSettingsSerde>(value)
                .map_err(serde::de::Error::custom)?;
            Ok(Self {
                enabled: parsed.enabled,
                selected: parsed.selected,
                engine_devices: parsed.engine_devices,
                index_path: parsed.index_path,
                custom_models: parsed.custom_models,
                chunk_size: parsed.chunk_size,
                chunk_overlap: parsed.chunk_overlap,
                worker_timeout_secs: parsed.worker_timeout_secs,
            })
        } else {
            let parsed = serde_json::from_value::<LegacySemanticSettingsSerde>(value)
                .map_err(serde::de::Error::custom)?;
            Ok(Self {
                enabled: parsed.enabled,
                selected: SelectedEmbedder {
                    engine: parsed.engine,
                    model: parsed.model,
                    dimension: parsed.dimension,
                },
                engine_devices: parsed.engine_devices,
                index_path: parsed.index_path,
                custom_models: parsed.custom_models,
                chunk_size: parsed.chunk_size,
                chunk_overlap: parsed.chunk_overlap,
                worker_timeout_secs: parsed.worker_timeout_secs,
            })
        }
    }
}

impl Default for SemanticSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            selected: Self::default_selected(),
            engine_devices: HashMap::new(),
            index_path: None,
            custom_models: Vec::new(),
            chunk_size: Self::default_chunk_size(),
            chunk_overlap: Self::default_chunk_overlap(),
            worker_timeout_secs: Self::default_worker_timeout(),
        }
    }
}

// ── Index status ──────────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexStatus {
    pub indexed_files: usize,
    pub total_chunks: usize,
    pub built_at: Option<u64>,
    pub build_duration_ms: Option<u64>,
    pub engine: EmbeddingEngine,
    pub model_id: String,
    pub dimension: usize,
    pub root_path: Option<std::path::PathBuf>,
    pub db_size_bytes: Option<u64>,
}

// ── Settings ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub bookmarked_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub recent_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub last_directory: Option<PathBuf>,
    pub respect_gitignore: bool,
    pub max_file_size: u64,
    pub context_lines: usize,
    pub theme: Theme,
    #[serde(default)]
    pub search_prefer_semantic: bool,
    pub semantic: SemanticSettings,
    #[serde(default = "default_supported_extensions")]
    pub supported_extensions: Vec<String>,
    #[serde(default)]
    pub max_results: usize,
}

fn default_supported_extensions() -> Vec<String> {
    vec![
        "txt",
        "md",
        "json",
        "xml",
        "html",
        "htm",
        "log",
        "csv",
        "jsonl",
        "pdf",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bookmarked_dirs: Vec::new(),
            recent_dirs: Vec::new(),
            last_directory: None,
            respect_gitignore: true,
            max_file_size: 10 * 1024 * 1024,
            context_lines: 2,
            theme: Theme::default(),
            search_prefer_semantic: false,
            semantic: SemanticSettings::default(),
            supported_extensions: default_supported_extensions(),
            max_results: 50,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileListResponse {
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub omitted: Vec<OmittedFileEntry>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DataPaths {
    pub app_data: String,
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
    pub files_scanned: usize,
    pub total_matches: usize,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_file_type_detect() {
        let extensions = vec!["txt".to_string(), "pdf".to_string()];

        assert_eq!(
            FileType::detect(Path::new("test.txt"), &extensions),
            Some(FileType::PlainText)
        );
        assert_eq!(
            FileType::detect(Path::new("test.pdf"), &extensions),
            Some(FileType::Pdf)
        );
        assert_eq!(
            FileType::detect(Path::new("Makefile"), &extensions),
            Some(FileType::PlainText)
        );
        assert_eq!(FileType::detect(Path::new("test.exe"), &extensions), None);
    }

    #[test]
    fn test_bounding_box_merge() {
        let b1 = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let b2 = BoundingBox {
            x: 5.0,
            y: 5.0,
            width: 10.0,
            height: 10.0,
        };
        let merged = b1.merge(&b2);

        assert_eq!(merged.x, 0.0);
        assert_eq!(merged.y, 0.0);
        assert_eq!(merged.width, 15.0);
        assert_eq!(merged.height, 15.0);
    }

    #[test]
    fn test_source_map_resolve() {
        let map = SourceMap {
            segments: vec![
                SourceSegment {
                    text_range: ByteRange { start: 0, end: 10 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                SourceSegment {
                    text_range: ByteRange { start: 10, end: 20 },
                    origin: SourceOrigin::TextFile { line: 2, col: 1 },
                },
            ],
        };

        match map.resolve(5).unwrap() {
            SourceOrigin::TextFile { line, .. } => assert_eq!(line, 1),
            _ => panic!("Expected TextFile origin"),
        }

        match map.resolve(15).unwrap() {
            SourceOrigin::TextFile { line, .. } => assert_eq!(line, 2),
            _ => panic!("Expected TextFile origin"),
        }
    }

    #[test]
    fn test_source_map_resolve_range_pdf() {
        let map = SourceMap {
            segments: vec![
                SourceSegment {
                    text_range: ByteRange { start: 0, end: 10 },
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: Some(BoundingBox {
                            x: 0.0,
                            y: 0.0,
                            width: 10.0,
                            height: 10.0,
                        }),
                    },
                },
                SourceSegment {
                    text_range: ByteRange { start: 10, end: 20 },
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: Some(BoundingBox {
                            x: 5.0,
                            y: 5.0,
                            width: 10.0,
                            height: 10.0,
                        }),
                    },
                },
            ],
        };

        let origin = map.resolve_range(ByteRange { start: 5, end: 15 }).unwrap();
        match origin {
            SourceOrigin::PdfPage { page, bbox } => {
                assert_eq!(page, 1);
                let b = bbox.unwrap();
                assert_eq!(b.x, 0.0);
                assert_eq!(b.y, 0.0);
                assert_eq!(b.width, 15.0);
                assert_eq!(b.height, 15.0);
            }
            _ => panic!("Expected PdfPage origin"),
        }
    }

    #[test]
    fn test_embedding_engine_methods() {
        assert_eq!(EmbeddingEngine::SBERT.as_str(), "sbert");
        assert_eq!(EmbeddingEngine::Candle.as_str(), "candle");
        assert_eq!(EmbeddingEngine::Fastembed.as_str(), "fastembed");

        assert_eq!(EmbeddingEngine::SBERT.default_device(), "auto");
        assert_eq!(EmbeddingEngine::Candle.default_device(), "auto");
        assert_eq!(EmbeddingEngine::Fastembed.default_device(), "cpu");

        assert!(EmbeddingEngine::SBERT.supports_custom_models());
        assert!(EmbeddingEngine::Candle.supports_custom_models());
        assert!(!EmbeddingEngine::Fastembed.supports_custom_models());
    }

    #[test]
    fn test_semantic_settings_defaults() {
        let settings = SemanticSettings::default();
        assert_eq!(settings.enabled, false);
        assert_eq!(settings.selected.engine, EmbeddingEngine::default());
        assert_eq!(settings.selected.model.model_id(), "AllMiniLML6V2");
        assert_eq!(settings.selected.dimension, 384);
        assert_eq!(settings.chunk_size, 600);
        assert_eq!(settings.chunk_overlap, 128);
        assert_eq!(settings.worker_timeout_secs, 300);

        assert_eq!(settings.device_for(EmbeddingEngine::SBERT), "auto");

        let mut settings = SemanticSettings::default();
        settings
            .engine_devices
            .insert(EmbeddingEngine::SBERT, "cuda".to_string());
        assert_eq!(settings.device_for(EmbeddingEngine::SBERT), "cuda");
    }

    #[test]
    fn test_source_map_resolve_fallback() {
        let map = SourceMap {
            segments: vec![SourceSegment {
                text_range: ByteRange { start: 0, end: 10 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            }],
        };

        // Offset beyond all segments should fall back to last segment
        match map.resolve(100).unwrap() {
            SourceOrigin::TextFile { line, .. } => assert_eq!(line, 1),
            _ => panic!("Expected TextFile origin"),
        }
    }

    #[test]
    fn test_source_map_resolve_range_multi_page() {
        let map = SourceMap {
            segments: vec![
                SourceSegment {
                    text_range: ByteRange { start: 0, end: 10 },
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: None,
                    },
                },
                SourceSegment {
                    text_range: ByteRange { start: 10, end: 20 },
                    origin: SourceOrigin::PdfPage {
                        page: 2,
                        bbox: None,
                    },
                },
            ],
        };

        // Range spanning page 1 and 2
        let origin = map.resolve_range(ByteRange { start: 5, end: 15 }).unwrap();
        match origin {
            SourceOrigin::PdfPage { page, .. } => assert_eq!(page, 1),
            _ => panic!("Expected PdfPage origin on page 1"),
        }
    }

    #[test]
    fn test_source_map_resolve_range_no_overlap() {
        let map = SourceMap {
            segments: vec![SourceSegment {
                text_range: ByteRange { start: 10, end: 20 },
                origin: SourceOrigin::TextFile { line: 2, col: 1 },
            }],
        };

        // Range before any segment
        let origin = map.resolve_range(ByteRange { start: 0, end: 5 }).unwrap();
        match origin {
            SourceOrigin::TextFile { line, .. } => assert_eq!(line, 2),
            _ => panic!("Expected fallback to last segment"),
        }
    }

    #[test]
    fn test_embedding_engine_supported() {
        let engines = EmbeddingEngine::supported_engines();
        assert!(!engines.is_empty());
        assert!(engines.contains(&EmbeddingEngine::SBERT));
    }

    #[test]
    fn test_deserialize_custom_models() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[allow(dead_code)]
            #[serde(deserialize_with = "deserialize_custom_models")]
            models: Vec<CustomModel>,
        }

        // Test old format (Vec<String>)
        let json = r#"{"models": ["model1", "model2"]}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(w.models.len(), 2);
        assert_eq!(w.models[0].model_id, "model1");
        assert_eq!(w.models[0].engine, EmbeddingEngine::SBERT);

        // Test new format (Vec<CustomModel>)
        let json = r#"{"models": [{"engine": "Candle", "model_id": "model3"}]}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(w.models.len(), 1);
        assert_eq!(w.models[0].model_id, "model3");
        assert_eq!(w.models[0].engine, EmbeddingEngine::Candle);
    }

    #[test]
    fn test_deserialize_custom_models_invalid() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[allow(dead_code)]
            #[serde(deserialize_with = "deserialize_custom_models")]
            models: Vec<CustomModel>,
        }

        let json = r#"{"models": [123]}"#;
        let res: Result<Wrapper, _> = serde_json::from_str(json);
        assert!(res.is_err());
    }

    #[test]
    fn test_semantic_settings_deserialize_legacy_fields() {
        let json = r#"{
            "enabled": true,
            "engine": "Candle",
            "model": "sentence-transformers/all-MiniLM-L12-v2",
            "dimension": 384,
            "engine_devices": {},
            "index_path": null,
            "custom_models": [],
            "chunk_size": 600,
            "chunk_overlap": 128,
            "worker_timeout_secs": 300
        }"#;
        let settings: SemanticSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.selected.engine, EmbeddingEngine::Candle);
        assert_eq!(
            settings.selected.model.model_id(),
            "sentence-transformers/all-MiniLM-L12-v2"
        );
        assert_eq!(settings.selected.dimension, 384);
    }

    #[test]
    fn test_file_type_detect_none() {
        assert_eq!(FileType::detect(Path::new("unknown"), &[]), None);
        assert_eq!(FileType::detect(Path::new("test.unknown"), &[]), None);
    }

    #[test]
    fn test_file_type_detect_known_names() {
        let extensions = vec![];
        assert_eq!(
            FileType::detect(Path::new("Dockerfile"), &extensions),
            Some(FileType::PlainText)
        );
        assert_eq!(
            FileType::detect(Path::new("Makefile"), &extensions),
            Some(FileType::PlainText)
        );
        assert_eq!(
            FileType::detect(Path::new("dockerfile"), &extensions),
            Some(FileType::PlainText)
        );
    }

    #[test]
    fn test_deserialize_custom_models_non_array() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[allow(dead_code)]
            #[serde(deserialize_with = "deserialize_custom_models")]
            models: Vec<CustomModel>,
        }

        let json = r#"{"models": "not an array"}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert!(w.models.is_empty());
    }

    #[test]
    fn test_search_query_defaults() {
        let json = r#"{"pattern": "p", "is_regex": false, "case_sensitive": false, "root": ".", "file_type_filters": [], "max_results": 10}"#;
        let q: SearchQuery = serde_json::from_str(json).unwrap();
        assert_eq!(q.respect_gitignore, true);
        assert_eq!(q.max_file_size, 0);
        assert_eq!(q.context_lines, 2);
        assert_eq!(q.mode, SearchMode::Grep);
    }

    #[test]
    fn test_embedder_model_serde() {
        let m = EmbedderModel("model-1".to_string());
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, "\"model-1\"");

        let m2: EmbedderModel = serde_json::from_str(&json).unwrap();
        assert_eq!(m2, m);
        assert_eq!(m2.model_id(), "model-1");
    }

    #[test]
    fn test_settings_default() {
        let s = Settings::default();
        assert!(s.supported_extensions.contains(&"pdf".to_string()));
        assert_eq!(s.context_lines, 2);
    }
}
