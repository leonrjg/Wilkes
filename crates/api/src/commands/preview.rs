use std::sync::{Arc, Mutex};

use wilkes_core::embed::index::db::SemanticIndex;
use wilkes_core::types::{ByteRange, MatchRef, PreviewData, SourceOrigin, SupersededArea};

/// The live semantic index, or nothing when none is open.
pub type IndexHandle = Arc<Mutex<Option<SemanticIndex>>>;

/// Load preview data for a match.
pub async fn preview(
    match_ref: MatchRef,
    index: Option<IndexHandle>,
) -> anyhow::Result<PreviewData> {
    match &match_ref.origin {
        SourceOrigin::TextFile { .. } => preview_text(&match_ref).await,
        SourceOrigin::PdfPage { page, bbox } => {
            preview_pdf(&match_ref, *page, bbox.clone(), index).await
        }
    }
}

async fn preview_text(match_ref: &MatchRef) -> anyhow::Result<PreviewData> {
    let content = tokio::fs::read_to_string(&match_ref.path).await?;
    let language = detect_language(&match_ref.path);

    let (highlight_line, highlight_range) = if let Some(range) = &match_ref.text_range {
        let start = char_boundary_at_or_before(&content, range.start);
        let end = char_boundary_at_or_before(&content, range.end.max(start));
        // The selected line is one plus the number of newlines before it. This
        // also handles a selection that begins exactly at the first character
        // after a newline (where `str::lines().count()` would under-count).
        let highlight_line = content[..start]
            .chars()
            .filter(|character| *character == '\n')
            .count() as u32
            + 1;

        (highlight_line, ByteRange { start, end })
    } else {
        let line = match &match_ref.origin {
            SourceOrigin::TextFile { line, .. } => *line,
            _ => 1,
        };
        if line == 0 {
            (0, ByteRange { start: 0, end: 0 })
        } else {
            (line, line_range(&content, line))
        }
    };

    Ok(PreviewData::Text {
        content,
        language,
        highlight_line,
        highlight_range,
    })
}

fn char_boundary_at_or_before(content: &str, offset: usize) -> usize {
    let capped = offset.min(content.len());
    if content.is_char_boundary(capped) {
        return capped;
    }
    content
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index < capped)
        .last()
        .unwrap_or(0)
}

async fn preview_pdf(
    match_ref: &MatchRef,
    page: u32,
    highlight_bbox: Option<wilkes_core::types::BoundingBox>,
    index: Option<IndexHandle>,
) -> anyhow::Result<PreviewData> {
    Ok(pdf_preview(&match_ref.path, page, highlight_bbox, index))
}

/// What a PDF preview is, wherever it is asked for.
///
/// One constructor because there are two doors — a match and a file opened
/// directly — and a document that copied its formulas differently depending on
/// which one the reader came through would be the same incoherence this
/// carries the areas to remove.
///
/// The bytes of the document are not in it: the frontend loads the file itself
/// through the asset protocol, so nothing here crosses IPC but page, geometry
/// and the reading's own text.
pub(crate) fn pdf_preview(
    path: &std::path::Path,
    page: u32,
    highlight_bbox: Option<wilkes_core::types::BoundingBox>,
    index: Option<IndexHandle>,
) -> PreviewData {
    PreviewData::Pdf {
        page,
        highlight_bbox,
        superseded: superseded_areas(path, index),
    }
}

/// The page areas this document's reading owns rather than its glyphs.
///
/// Read from the index and nowhere else. Deriving them live would mean
/// extracting the document — and running a recognizer over it — every time
/// somebody opened it, to recover something that was established when it was
/// indexed. A document the index has never read has none, and the reader then
/// offers what the page draws, which is all that is known about it.
fn superseded_areas(path: &std::path::Path, index: Option<IndexHandle>) -> Vec<SupersededArea> {
    let Some(index) = index else {
        tracing::debug!(
            "superseded_areas: no index handle was passed for {}; the preview will show the page's own glyphs",
            path.display()
        );
        return Vec::new();
    };
    let guard = match index.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let Some(index) = guard.as_ref() else {
        tracing::info!(
            "superseded_areas: no index is open, so this workspace's reading regions for {} cannot be read; the preview will show the page's own glyphs",
            path.display()
        );
        return Vec::new();
    };
    match index.superseded_areas_for_path(path) {
        Ok(areas) => areas,
        Err(error) => {
            tracing::warn!(
                "reading regions for {} could not be read: {error:#}",
                path.display()
            );
            Vec::new()
        }
    }
}

/// Detect a language hint for CodeMirror syntax highlighting.
pub fn detect_language(path: &std::path::Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let lang = match ext.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "json" | "jsonl" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" => "css",
        "xml" => "xml",
        "sql" => "sql",
        "sh" | "bash" | "zsh" => "shell",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "rb" => "ruby",
        "swift" => "swift",
        "kt" => "kotlin",
        "cs" => "csharp",
        _ => return None,
    };
    Some(lang.into())
}

/// Return the byte range (in the whole file string) of the given 1-based line.
fn line_range(content: &str, line: u32) -> ByteRange {
    let target = line.saturating_sub(1) as usize;
    let mut offset = 0usize;
    for (i, l) in content.lines().enumerate() {
        if i == target {
            return ByteRange {
                start: offset,
                end: offset + l.len(),
            };
        }
        offset += l.len() + 1; // +1 for '\n'
    }
    ByteRange { start: 0, end: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use tempfile::NamedTempFile;

    /// The preview a reader opens a PDF with carries the areas whose text this
    /// document's reading owns — the whole point of routing it through the
    /// index — and carries none for a document the index has never read.
    #[tokio::test]
    async fn a_pdf_preview_carries_the_reading_s_own_areas() {
        use wilkes_core::embed::index::chunk::Chunk;
        use wilkes_core::embed::index::db::PreparedFile;
        use wilkes_core::embed::index::SemanticIndex;
        use wilkes_core::types::{
            BoundingBox, ByteRange, EmbeddingEngine, ReadingRegion, SourceOrigin,
        };

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut index =
            SemanticIndex::create(root, "m", 1, EmbeddingEngine::Candle, Some(root)).unwrap();

        let path = root.join("paper.pdf");
        std::fs::write(&path, "pdf bytes").unwrap();
        let full_text = "Page formula: y_{B} = w mod q.\n".to_string();
        let start = full_text.find("y_{B}").unwrap();
        let end = start + "y_{B} = w mod q".len();
        index
            .write_file(PreparedFile {
                path: path.clone(),
                full_text: full_text.clone(),
                retained: wilkes_core::types::RetainedExtraction {
                    reading_regions: vec![ReadingRegion {
                        area_id: "p1-i0".to_string(),
                        page: 1,
                        bbox: BoundingBox {
                            x: 10.0,
                            y: 20.0,
                            width: 300.0,
                            height: 24.0,
                        },
                        text_range: ByteRange { start, end },
                    }],
                    ..Default::default()
                },
                chunks: vec![(
                    Chunk {
                        file_path: path.clone(),
                        text: full_text.clone(),
                        byte_range: ByteRange {
                            start: 0,
                            end: full_text.len(),
                        },
                        origin: SourceOrigin::PdfPage {
                            page: 1,
                            bbox: None,
                        },
                    },
                    vec![1.0],
                )],
            })
            .unwrap();

        let handle: IndexHandle = Arc::new(Mutex::new(Some(index)));
        let match_ref = |path: PathBuf| MatchRef {
            path,
            origin: SourceOrigin::PdfPage {
                page: 1,
                bbox: None,
            },
            text_range: None,
        };

        let data = preview(match_ref(path), Some(handle.clone()))
            .await
            .unwrap();
        let PreviewData::Pdf { superseded, .. } = data else {
            panic!("a PDF match previews as a PDF");
        };
        assert_eq!(superseded.len(), 1);
        assert_eq!(superseded[0].text, "y_{B} = w mod q");

        let unknown = root.join("never-read.pdf");
        std::fs::write(&unknown, "pdf bytes").unwrap();
        let data = preview(match_ref(unknown), Some(handle)).await.unwrap();
        let PreviewData::Pdf { superseded, .. } = data else {
            panic!("a PDF match previews as a PDF");
        };
        assert!(superseded.is_empty());
    }

    #[test]
    fn test_detect_language() {
        assert_eq!(
            detect_language(Path::new("test.rs")),
            Some("rust".to_string())
        );
        assert_eq!(
            detect_language(Path::new("test.py")),
            Some("python".to_string())
        );
        assert_eq!(detect_language(Path::new("test.unknown")), None);
    }

    #[test]
    fn test_line_range() {
        let content = "line 1\nline 2\nline 3";
        let range = line_range(content, 1);
        assert_eq!(range.start, 0);
        assert_eq!(range.end, 6);

        let range = line_range(content, 2);
        assert_eq!(range.start, 7);
        assert_eq!(range.end, 13);
    }

    #[tokio::test]
    async fn test_preview_text() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "line 1\nline 2\nline 3").unwrap();
        let path = tmp.path().to_path_buf();

        let match_ref = MatchRef {
            path: path.clone(),
            origin: SourceOrigin::TextFile { line: 2, col: 2 },
            text_range: Some(ByteRange { start: 8, end: 13 }),
        };

        let preview = preview_text(&match_ref).await.unwrap();
        if let PreviewData::Text {
            content,
            highlight_line,
            ..
        } = preview
        {
            assert!(content.contains("line 2"));
            assert_eq!(highlight_line, 2);
        } else {
            panic!("Expected Text preview");
        }
    }

    #[tokio::test]
    async fn test_preview_text_clamps_stale_range_to_character_boundaries() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "aé🙂z").unwrap();
        let match_ref = MatchRef {
            path: tmp.path().to_path_buf(),
            origin: SourceOrigin::TextFile { line: 1, col: 0 },
            // Both offsets are inside multi-byte UTF-8 characters. A persisted
            // bookmark can become stale after the file is edited, so preview
            // must never slice at these raw offsets.
            text_range: Some(ByteRange { start: 2, end: 5 }),
        };

        let preview = preview_text(&match_ref).await.unwrap();
        let PreviewData::Text {
            highlight_range, ..
        } = preview
        else {
            panic!("Expected Text preview");
        };
        assert_eq!(highlight_range.start, 1);
        assert_eq!(highlight_range.end, 3);
    }

    #[tokio::test]
    async fn test_preview_text_reports_line_when_range_starts_after_newline() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "first\nsecond").unwrap();
        let match_ref = MatchRef {
            path: tmp.path().to_path_buf(),
            origin: SourceOrigin::TextFile { line: 2, col: 0 },
            text_range: Some(ByteRange { start: 6, end: 12 }),
        };

        let preview = preview_text(&match_ref).await.unwrap();
        let PreviewData::Text { highlight_line, .. } = preview else {
            panic!("Expected Text preview");
        };
        assert_eq!(highlight_line, 2);
    }

    #[tokio::test]
    async fn test_preview_text_no_highlight_when_line_zero() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "line 1\nline 2\nline 3").unwrap();
        let path = tmp.path().to_path_buf();

        let match_ref = MatchRef {
            path: path.clone(),
            origin: SourceOrigin::TextFile { line: 0, col: 0 },
            text_range: None,
        };

        let preview = preview_text(&match_ref).await.unwrap();
        if let PreviewData::Text {
            highlight_line,
            highlight_range,
            ..
        } = preview
        {
            assert_eq!(highlight_line, 0, "line 0 should produce no highlight");
            assert_eq!(highlight_range.start, 0);
            assert_eq!(
                highlight_range.end, 0,
                "range should be empty when no highlight"
            );
        } else {
            panic!("Expected Text preview");
        }
    }

    #[tokio::test]
    async fn test_preview_text_falls_back_for_non_text_origin_without_range() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "line 1\nline 2").unwrap();
        let path = tmp.path().to_path_buf();

        let match_ref = MatchRef {
            path,
            origin: SourceOrigin::PdfPage {
                page: 3,
                bbox: None,
            },
            text_range: None,
        };

        let preview = preview_text(&match_ref).await.unwrap();
        if let PreviewData::Text {
            highlight_line,
            highlight_range,
            ..
        } = preview
        {
            assert_eq!(highlight_line, 1);
            assert_eq!(highlight_range.start, 0);
            assert_eq!(highlight_range.end, 6);
        } else {
            panic!("Expected Text preview");
        }
    }

    #[tokio::test]
    async fn test_preview_pdf() {
        let match_ref = MatchRef {
            path: PathBuf::from("test.pdf"),
            origin: SourceOrigin::PdfPage {
                page: 5,
                bbox: Some(wilkes_core::types::BoundingBox {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }),
            },
            text_range: None,
        };

        let res = preview(match_ref, None).await.unwrap();
        if let PreviewData::Pdf {
            page,
            highlight_bbox,
            superseded,
        } = res
        {
            assert_eq!(page, 5);
            assert!(highlight_bbox.is_some());
            // No index to ask: the page speaks for itself.
            assert!(superseded.is_empty());
        } else {
            panic!("Expected Pdf preview");
        }
    }
}
