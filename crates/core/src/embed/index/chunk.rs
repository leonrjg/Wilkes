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
/// The returned ranges tile `content.text` with no holes: the first starts at
/// byte 0, the last ends at `content.text.len()`, and every boundary either
/// overlaps its neighbour or meets it exactly. The splitter drops whitespace at
/// split points, so those bytes are handed to the preceding chunk rather than
/// left owned by nobody — a chunk's `text` is exactly the slice its range names,
/// which is what lets a consumer rebuild the whole extracted rendition (and its
/// `extracted_content_sha256`) from the chunks alone.
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

    // chunks() returns subslices of the original — pointer diff gives the byte offset.
    // Whitespace-only chunks carry no passage, so their bytes are absorbed by the
    // preceding chunk's range below instead of forming a chunk of their own.
    let mut spans: Vec<ByteRange> = splitter
        .chunks(&content.text)
        .filter(|chunk_str| !chunk_str.trim().is_empty())
        .map(|chunk_str| {
            let offset = chunk_str.as_ptr() as usize - base;
            ByteRange {
                start: offset,
                end: offset + chunk_str.len(),
            }
        })
        .collect();

    let Some(first) = spans.first_mut() else {
        return Vec::new();
    };
    first.start = 0;
    for index in 0..spans.len().saturating_sub(1) {
        let next_start = spans[index + 1].start;
        if spans[index].end < next_start {
            spans[index].end = next_start;
        }
    }
    spans.last_mut().expect("spans is non-empty").end = content.text.len();

    spans
        .into_iter()
        .map(|byte_range| {
            let text = content.text[byte_range.start..byte_range.end].to_string();
            let origin = content
                .source_map
                .resolve_range(byte_range.clone())
                .or_else(|| {
                    // Chunk start may land on a gap (e.g. inter-page whitespace in PDFs).
                    // Walk forward to the first byte that resolves.
                    (byte_range.start + 1..byte_range.end)
                        .filter(|index| content.text.is_char_boundary(*index))
                        .find_map(|start| {
                            content.source_map.resolve_range(ByteRange {
                                start,
                                end: byte_range.end,
                            })
                        })
                })
                .unwrap_or_else(|| {
                    let line = content.text[..byte_range.start]
                        .bytes()
                        .filter(|&b| b == b'\n')
                        .count() as u32
                        + 1;
                    SourceOrigin::TextFile { line, col: 0 }
                });
            Chunk {
                text,
                byte_range,
                origin,
                file_path: file_path.clone(),
            }
        })
        .collect()
}

/// Verify that ordinal-ordered `chunks` rebuild `text` byte for byte: each
/// chunk's text is exactly the slice its range names, the ranges leave no hole,
/// and the last one reaches the end. A consumer holding only the chunks can then
/// recompute the extracted-content hash, which is the whole point of publishing
/// stable byte ranges alongside the text.
///
/// An empty chunk list is vacuously covering: a document with nothing to embed
/// contributes no chunks, and callers that require content (managed admission)
/// refuse it on that separate ground.
pub fn ensure_chunks_reconstruct<'a>(
    text: &str,
    chunks: impl IntoIterator<Item = (&'a ByteRange, &'a str)>,
) -> anyhow::Result<()> {
    let mut owned_through = 0usize;
    let mut any = false;
    for (byte_range, chunk_text) in chunks {
        any = true;
        anyhow::ensure!(
            byte_range.start <= byte_range.end && byte_range.end <= text.len(),
            "chunk byte range {}..{} falls outside the {}-byte extracted content",
            byte_range.start,
            byte_range.end,
            text.len()
        );
        anyhow::ensure!(
            text.get(byte_range.start..byte_range.end) == Some(chunk_text),
            "chunk text is not the extracted content at bytes {}..{}",
            byte_range.start,
            byte_range.end
        );
        anyhow::ensure!(
            byte_range.start <= owned_through,
            "extracted content bytes {}..{} belong to no chunk",
            owned_through,
            byte_range.start
        );
        owned_through = owned_through.max(byte_range.end);
    }
    anyhow::ensure!(
        !any || owned_through == text.len(),
        "extracted content bytes {}..{} belong to no chunk",
        owned_through,
        text.len()
    );
    Ok(())
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
                    provenance: Default::default(),
                }],
            },
            metadata: FileMetadata {
                path: PathBuf::from("test.txt"),
                size_bytes: 70,
                mime: None,
                title: None,
                page_count: None,
            },
            images: Vec::new(),
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
            images: Vec::new(),
        };

        let chunks = chunk_content(&content, PathBuf::from("test.txt"), 100, 10);
        assert!(chunks.is_empty());
    }

    /// The whitespace the splitter drops at a split point (and any trailing
    /// newline) still belongs to a chunk, so the chunks rebuild the extracted
    /// text byte for byte.
    #[test]
    fn chunks_tile_the_extracted_text_across_dropped_whitespace() {
        let text = format!(
            "\n\n{}\n{}\n\n{}\n",
            "Data protection begins with a lawful basis. ".repeat(4),
            "Contents . . . . . . . . . . . . . . . . 7".repeat(3),
            "Cyber security controls follow from the threat model. ".repeat(4),
        );
        let byte_len = text.len();
        let content = ExtractedContent {
            text,
            source_map: SourceMap {
                segments: vec![SourceSegment {
                    text_range: ByteRange {
                        start: 0,
                        end: byte_len,
                    },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                    provenance: Default::default(),
                }],
            },
            metadata: FileMetadata {
                path: PathBuf::from("doc.txt"),
                size_bytes: byte_len as u64,
                mime: None,
                title: None,
                page_count: None,
            },
            images: Vec::new(),
        };

        let chunks = chunk_content(&content, PathBuf::from("doc.txt"), 120, 20);

        assert!(chunks.len() > 1, "expected a multi-chunk document");
        assert_eq!(chunks.first().expect("first chunk").byte_range.start, 0);
        assert_eq!(chunks.last().expect("last chunk").byte_range.end, byte_len);
        ensure_chunks_reconstruct(
            &content.text,
            chunks
                .iter()
                .map(|chunk| (&chunk.byte_range, chunk.text.as_str())),
        )
        .expect("chunks rebuild the extracted text");

        let mut rebuilt = String::new();
        for chunk in &chunks {
            let already = rebuilt.len().saturating_sub(chunk.byte_range.start);
            rebuilt.push_str(&chunk.text[already..]);
        }
        assert_eq!(rebuilt, content.text);
    }

    #[test]
    fn a_hole_between_chunks_is_reported_rather_than_published() {
        let text = "alpha\nbeta";
        let ranges = [
            (ByteRange { start: 0, end: 5 }, "alpha"),
            (ByteRange { start: 6, end: 10 }, "beta"),
        ];
        let error =
            ensure_chunks_reconstruct(text, ranges.iter().map(|(range, text)| (range, *text)))
                .expect_err("the newline belongs to no chunk");
        assert!(
            error.to_string().contains("belong to no chunk"),
            "unexpected error: {error}"
        );
    }
}
