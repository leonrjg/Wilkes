//! Path -> extracted text, for `fs/read_text_file` and (later) the
//! `get_document_text` MCP verb. The same `ExtractedContent` that backs
//! search: there is no second text-extraction path.

use std::path::Path;
use std::time::Instant;

use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::types::{ExtractedContent, SourceOrigin};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextExcerpt {
    pub text: String,
    pub truncated: bool,
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

fn registry() -> ExtractorRegistry {
    let mut registry = ExtractorRegistry::new();
    registry.register(Box::new(PdfExtractor::new()));
    registry
}

/// Extract `path`'s text, optionally scoped to a single PDF page and/or a
/// `fs/read_text_file`-style 1-based line window.
///
/// Mirrors the plain-text-vs-PDF dispatch `GrepSearchProvider` uses for
/// search: plain text is read directly (it needs no extraction), PDFs go
/// through the same `ExtractorRegistry` that backs search and preview, so
/// there is only one PDF text-extraction path in the codebase.
pub fn read_text(
    path: &Path,
    page: Option<u32>,
    line: Option<u32>,
    limit: Option<u32>,
) -> anyhow::Result<String> {
    read_text_range(path, page.map(|page| (page, page)), line, limit)
}

pub fn read_text_range(
    path: &Path,
    page_range: Option<(u32, u32)>,
    line: Option<u32>,
    limit: Option<u32>,
) -> anyhow::Result<String> {
    let started_at = Instant::now();
    let pdf = is_pdf(path);
    let text = if pdf {
        let registry = registry();
        let extractor = registry
            .find(path, None)
            .ok_or_else(|| anyhow::anyhow!("no PDF extractor registered"))?;
        let content = extractor.extract(path)?;
        match page_range {
            Some((start, end)) => page_range_text(&content, start, end),
            None => content.text,
        }
    } else {
        std::fs::read_to_string(path)?
    };

    let output = match (line, limit) {
        (None, None) => text,
        (line, limit) => slice_lines(&text, line.unwrap_or(1), limit),
    };
    tracing::info!(
        path = %path.display(),
        is_pdf = pdf,
        page_range = ?page_range,
        line = ?line,
        limit = ?limit,
        elapsed_ms = started_at.elapsed().as_millis(),
        output_bytes = output.len(),
        "chat: read_text_range completed"
    );
    Ok(output)
}

/// Extract a bounded text excerpt for the active document that Wilkes pushes
/// into every prompt. For PDFs with a current page, this is strict page text:
/// an empty page does not fall back to the whole document.
pub fn read_active_excerpt(
    path: &Path,
    page: Option<u32>,
    max_chars: usize,
) -> anyhow::Result<TextExcerpt> {
    let text = if is_pdf(path) {
        let registry = registry();
        let extractor = registry
            .find(path, None)
            .ok_or_else(|| anyhow::anyhow!("no PDF extractor registered"))?;
        let content = extractor.extract(path)?;
        match page {
            Some(page) => page_text_strict(&content, page).unwrap_or_default(),
            None => content.text,
        }
    } else {
        std::fs::read_to_string(path)?
    };

    Ok(limit_excerpt(&text, max_chars))
}

/// Return the span of `content.text` covered by segments on the given PDF page.
/// Segment byte ranges come from the extractor's own `SourceMap`, so they are
/// always char-aligned -- never an ad hoc `&s[..n]`.
/// Falls back to the whole document if the page has no segments (e.g. a
/// non-PDF document, or a page number past the end).
fn page_text_strict(content: &ExtractedContent, page: u32) -> Option<String> {
    page_range_text_strict(content, page, page)
}

fn page_range_text(content: &ExtractedContent, start_page: u32, end_page: u32) -> String {
    let out = page_range_text_strict(content, start_page, end_page).unwrap_or_default();
    if out.is_empty() {
        content.text.clone()
    } else {
        out
    }
}

fn page_range_text_strict(
    content: &ExtractedContent,
    start_page: u32,
    end_page: u32,
) -> Option<String> {
    let (start_page, end_page) = if start_page <= end_page {
        (start_page, end_page)
    } else {
        (end_page, start_page)
    };
    let mut start: Option<usize> = None;
    let mut end: Option<usize> = None;
    for segment in &content.source_map.segments {
        if let SourceOrigin::PdfPage { page: p, .. } = &segment.origin {
            if (start_page..=end_page).contains(p) {
                start = Some(start.map_or(segment.text_range.start, |s| {
                    s.min(segment.text_range.start)
                }));
                end = Some(end.map_or(segment.text_range.end, |e| e.max(segment.text_range.end)));
            }
        }
    }
    let (start, end) = (start?, end?);
    content.text.get(start..end).map(ToOwned::to_owned)
}

fn slice_lines(text: &str, line: u32, limit: Option<u32>) -> String {
    let skip = line.saturating_sub(1) as usize;
    let lines = text.lines().skip(skip);
    let selected: Vec<&str> = match limit {
        Some(limit) => lines.take(limit as usize).collect(),
        None => lines.collect(),
    };
    selected.join("\n")
}

pub fn limit_excerpt(text: &str, max_chars: usize) -> TextExcerpt {
    if max_chars == 0 {
        return TextExcerpt {
            text: String::new(),
            truncated: !text.is_empty(),
        };
    }

    let mut end = text.len();
    let mut truncated = false;
    if text.chars().count() > max_chars {
        truncated = true;
        end = text
            .char_indices()
            .nth(max_chars)
            .map(|(idx, _)| idx)
            .unwrap_or(text.len());
    }

    TextExcerpt {
        text: text[..end].to_string(),
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wilkes_core::types::{ByteRange, FileMetadata, SourceMap, SourceOrigin, SourceSegment};

    #[test]
    fn reads_plain_text_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "line one\nline two\nline three\n").unwrap();

        let text = read_text(&path, None, None, None).unwrap();
        assert_eq!(text, "line one\nline two\nline three\n");
    }

    #[test]
    fn slices_line_window() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();

        let text = read_text(&path, None, Some(2), Some(2)).unwrap();
        assert_eq!(text, "two\nthree");
    }

    #[test]
    fn unsupported_extension_errors() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("image.png");
        std::fs::write(&path, b"\x89PNG").unwrap();

        assert!(read_text(&path, None, None, None).is_err());
    }

    #[test]
    fn excerpt_limit_is_char_safe() {
        let excerpt = limit_excerpt("aé日b", 3);
        assert_eq!(excerpt.text, "aé日");
        assert!(excerpt.truncated);
    }

    #[test]
    fn active_excerpt_reads_plain_text_with_limit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "abcdef").unwrap();

        let excerpt = read_active_excerpt(&path, None, 4).unwrap();
        assert_eq!(
            excerpt,
            TextExcerpt {
                text: "abcd".to_string(),
                truncated: true,
            }
        );
    }

    #[test]
    fn strict_page_text_preserves_whitespace_between_segments() {
        let content = ExtractedContent {
            text: "page one\npage two".to_string(),
            source_map: SourceMap {
                segments: vec![
                    SourceSegment {
                        text_range: ByteRange { start: 0, end: 4 },
                        origin: SourceOrigin::PdfPage {
                            page: 1,
                            bbox: None,
                        },
                    },
                    SourceSegment {
                        text_range: ByteRange { start: 5, end: 8 },
                        origin: SourceOrigin::PdfPage {
                            page: 1,
                            bbox: None,
                        },
                    },
                    SourceSegment {
                        text_range: ByteRange { start: 9, end: 13 },
                        origin: SourceOrigin::PdfPage {
                            page: 2,
                            bbox: None,
                        },
                    },
                    SourceSegment {
                        text_range: ByteRange { start: 14, end: 17 },
                        origin: SourceOrigin::PdfPage {
                            page: 2,
                            bbox: None,
                        },
                    },
                ],
            },
            metadata: FileMetadata {
                path: "test.pdf".into(),
                size_bytes: 0,
                mime: Some("application/pdf".into()),
                title: None,
                page_count: Some(2),
            },
        };

        assert_eq!(page_text_strict(&content, 1).unwrap(), "page one");
        assert_eq!(page_text_strict(&content, 2).unwrap(), "page two");
        assert_eq!(
            page_range_text_strict(&content, 1, 2).unwrap(),
            "page one\npage two"
        );
        assert_eq!(
            page_range_text_strict(&content, 2, 1).unwrap(),
            "page one\npage two"
        );
    }
}
