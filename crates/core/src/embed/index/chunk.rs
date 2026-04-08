use std::path::PathBuf;

use text_splitter::{ChunkConfig, TextSplitter};

use crate::types::{ByteRange, ExtractedContent, SourceOrigin};

// ── Chunk ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Chunk {
    pub text: String,
    /// Byte range into `ExtractedContent.text`.
    pub byte_range: ByteRange,
    /// Resolved source position.
    pub origin: SourceOrigin,
    pub file_path: PathBuf,
}

// ── Chunker ───────────────────────────────────────────────────────────────────

/// Split `content` into overlapping chunks, resolving each chunk's `SourceOrigin`
/// from the embedded `SourceMap`.
///
/// `window_chars` is the target chunk size in characters (~256 tokens at 1200).
/// `overlap_chars` is the overlap between adjacent chunks.
pub fn chunk_content(
    content: &ExtractedContent,
    file_path: PathBuf,
    window_chars: usize,
    overlap_chars: usize,
) -> Vec<Chunk> {
    if content.text.is_empty() {
        return Vec::new();
    }

    let config = ChunkConfig::new(window_chars)
        .with_overlap(overlap_chars)
        .expect("overlap must be smaller than chunk size");
    let splitter = TextSplitter::new(config);

    let base = content.text.as_ptr() as usize;

    splitter
        .chunks(&content.text)
        .filter_map(|chunk_str| {
            let text = chunk_str.trim().to_string();
            if text.is_empty() {
                return None;
            }
            // chunks() returns subslices of the original — pointer diff gives the byte offset.
            let offset = chunk_str.as_ptr() as usize - base;
            let byte_range = ByteRange {
                start: offset,
                end: offset + chunk_str.len(),
            };
            let origin = content
                .source_map
                .resolve_range(byte_range.clone())
                .or_else(|| {
                    // Chunk start may land on a gap (e.g. inter-page whitespace in PDFs).
                    // Walk forward to the first byte that resolves.
                    (1..chunk_str.len()).find_map(|i| {
                        content.source_map.resolve_range(ByteRange {
                            start: offset + i,
                            end: offset + chunk_str.len(),
                        })
                    })
                })
                .unwrap_or_else(|| {
                    let line = content.text[..offset]
                        .bytes()
                        .filter(|&b| b == b'\n')
                        .count() as u32
                        + 1;
                    SourceOrigin::TextFile { line, col: 0 }
                });
            Some(Chunk {
                text,
                byte_range,
                origin,
                file_path: file_path.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileMetadata, SourceMap, SourceSegment};

    #[test]
    fn test_chunk_content_simple() {
        let content = ExtractedContent {
            text: "Hello world. This is a test string for chunking. It should be split."
                .to_string(),
            source_map: SourceMap {
                segments: vec![SourceSegment {
                    text_range: ByteRange { start: 0, end: 70 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                }],
            },
            metadata: FileMetadata {
                path: PathBuf::from("test.txt"),
                size_bytes: 70,
                mime: None,
                title: None,
                page_count: None,
            },
        };

        // window_chars = 20, overlap = 5
        let chunks = chunk_content(&content, PathBuf::from("test.txt"), 20, 5);

        assert!(!chunks.is_empty());
        for chunk in &chunks {
            assert!(!chunk.text.is_empty());
            assert_eq!(chunk.file_path, PathBuf::from("test.txt"));
            match chunk.origin {
                SourceOrigin::TextFile { line, .. } => assert_eq!(line, 1),
                _ => panic!("Expected TextFile origin"),
            }
        }
    }

    #[test]
    fn test_chunk_empty_content() {
        let content = ExtractedContent {
            text: "".to_string(),
            source_map: SourceMap { segments: vec![] },
            metadata: FileMetadata {
                path: PathBuf::from("test.txt"),
                size_bytes: 0,
                mime: None,
                title: None,
                page_count: None,
            },
        };

        let chunks = chunk_content(&content, PathBuf::from("test.txt"), 100, 10);
        assert!(chunks.is_empty());
    }
}
