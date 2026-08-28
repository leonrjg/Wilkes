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
///
/// An image's enrichment block is a structural unit. The splitter is run
/// separately on the text before it, on the block itself, and on the text
/// after — so a boundary falls immediately before and immediately after the
/// block, its transcription and description stay in one passage whenever they
/// fit, and no overlap window drags unrelated body prose across the seam. A
/// block too large for one chunk is split inside itself, by the same splitter,
/// and every piece still resolves to the same image.
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

    let mut spans: Vec<ByteRange> = Vec::new();
    for run in structural_runs(content) {
        let text = &content.text[run.start..run.end];
        let base = text.as_ptr() as usize;
        // chunks() returns subslices of the original — pointer diff gives the
        // byte offset. Whitespace-only chunks carry no passage, so their bytes
        // are absorbed by the preceding chunk's range below instead of forming
        // a chunk of their own.
        spans.extend(
            splitter
                .chunks(text)
                .filter(|chunk_str| !chunk_str.trim().is_empty())
                .map(|chunk_str| {
                    let offset = chunk_str.as_ptr() as usize - base + run.start;
                    ByteRange {
                        start: offset,
                        end: offset + chunk_str.len(),
                    }
                }),
        );
    }

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

/// The runs the splitter is applied to, in order, tiling the whole text.
///
/// One run per image-enrichment block and one for each stretch of document
/// between them. A block whose range is malformed or out of order is left in
/// the surrounding run rather than made into a boundary: the enrichment is
/// still in the text either way, and a bad range must not be able to drop or
/// duplicate bytes.
fn structural_runs(content: &ExtractedContent) -> Vec<ByteRange> {
    let mut blocks: Vec<ByteRange> = content
        .images
        .iter()
        .filter_map(|image| image.reading_range.clone())
        .filter(|range| {
            range.start < range.end
                && range.end <= content.text.len()
                && content.text.is_char_boundary(range.start)
                && content.text.is_char_boundary(range.end)
        })
        .collect();
    blocks.sort_by_key(|range| range.start);

    let mut runs: Vec<ByteRange> = Vec::new();
    let mut cursor = 0usize;
    for block in blocks {
        if block.start < cursor {
            continue;
        }
        if block.start > cursor {
            runs.push(ByteRange {
                start: cursor,
                end: block.start,
            });
        }
        cursor = block.end;
        runs.push(block);
    }
    if cursor < content.text.len() {
        runs.push(ByteRange {
            start: cursor,
            end: content.text.len(),
        });
    }
    if runs.is_empty() {
        runs.push(ByteRange {
            start: 0,
            end: content.text.len(),
        });
    }
    runs
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
    use crate::types::{
        BoundingBox, ExtractedImage, FileMetadata, ImageAnalysisStatus, ImageTransform, SourceMap,
        SourceSegment,
    };

    /// A rendition holding one image whose enrichment occupies `start..end`.
    fn image_content(text: String, start: usize, end: usize) -> ExtractedContent {
        let byte_len = text.len();
        ExtractedContent {
            text,
            source_map: SourceMap {
                segments: vec![SourceSegment {
                    text_range: ByteRange {
                        start: 0,
                        end: byte_len,
                    },
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: None,
                    },
                    provenance: Default::default(),
                }],
            },
            metadata: FileMetadata {
                path: PathBuf::from("doc.pdf"),
                size_bytes: byte_len as u64,
                mime: Some("application/pdf".into()),
                title: None,
                page_count: Some(1),
            },
            images: vec![ExtractedImage {
                id: "p1-i0".into(),
                page: 1,
                bbox: BoundingBox {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 50.0,
                },
                transform: ImageTransform {
                    a: 100.0,
                    b: 0.0,
                    c: 0.0,
                    d: 50.0,
                    e: 0.0,
                    f: 0.0,
                },
                pixel_width: 100,
                pixel_height: 50,
                image_sha256: "digest".into(),
                reading_range: (start < end).then_some(ByteRange { start, end }),
                ocr_regions: Vec::new(),
                description: None,
                analyzer_identity: "analyzer-v1".into(),
                status: ImageAnalysisStatus::Complete,
            }],
        }
    }

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

    /// An enrichment block is a chunk of its own at ordinary settings: a
    /// boundary immediately before it, one immediately after, and the
    /// transcription and description together in between.
    #[test]
    fn an_image_block_is_a_chunk_of_its_own() {
        let before = "The system has three parts, described below. ".repeat(3);
        let block = "Image embedded text: Non-expert; User interface; Inference engine.\n\n\
                     Image description: A non-expert reaches an inference engine through a \
                     user interface.\n";
        let after = "The knowledge base is populated by an expert. ".repeat(3);
        let text = format!("{before}\n{block}{after}");
        let start = text.find("Image embedded text:").expect("the block is there");
        let content = image_content(text.clone(), start, start + block.len());

        let chunks = chunk_content(&content, PathBuf::from("doc.pdf"), 400, 60);

        let block_chunks: Vec<&Chunk> = chunks
            .iter()
            .filter(|chunk| chunk.text.contains("Image embedded text:"))
            .collect();
        assert_eq!(block_chunks.len(), 1, "the block is not split");
        let passage = block_chunks[0];
        assert!(
            passage.text.contains("Image description:"),
            "transcription and description stay together: {:?}",
            passage.text
        );
        assert!(
            !passage.text.contains("three parts") && !passage.text.contains("populated by"),
            "no body prose crosses the boundary: {:?}",
            passage.text
        );
    }

    /// The invariant every consumer of the chunk export rests on, with an
    /// image block in the text: the chunks still rebuild the reading byte for
    /// byte, and every byte belongs to one.
    #[test]
    fn chunks_around_an_image_block_still_reconstruct_the_reading() {
        let before = "Body prose that runs on for a while. ".repeat(8);
        let block = "Image embedded text: Alpha; Beta; Gamma.\n\nImage description: Three \
                     labelled boxes joined by arrows.\n";
        let after = "More body prose following the figure. ".repeat(8);
        let text = format!("{before}\n{block}{after}");
        let start = text.find("Image embedded text:").expect("the block is there");
        let content = image_content(text.clone(), start, start + block.len());

        let chunks = chunk_content(&content, PathBuf::from("doc.pdf"), 200, 30);

        assert_eq!(chunks.first().expect("a first chunk").byte_range.start, 0);
        assert_eq!(chunks.last().expect("a last chunk").byte_range.end, text.len());
        ensure_chunks_reconstruct(
            &text,
            chunks
                .iter()
                .map(|chunk| (&chunk.byte_range, chunk.text.as_str())),
        )
        .expect("chunks rebuild the reading");
    }

    /// A block larger than the window is split inside itself rather than
    /// swallowing the prose around it, and every piece is still the image's.
    #[test]
    fn an_oversized_block_splits_inside_itself() {
        let block = format!(
            "Image embedded text: {}\n",
            (0..40)
                .map(|index| format!("Label number {index}"))
                .collect::<Vec<_>>()
                .join("; ")
        );
        let text = format!("Before the figure.\n{block}After the figure.\n");
        let start = text.find("Image embedded text:").expect("the block is there");
        let content = image_content(text.clone(), start, start + block.len());

        let chunks = chunk_content(&content, PathBuf::from("doc.pdf"), 150, 20);
        let pieces: Vec<&Chunk> = chunks
            .iter()
            .filter(|chunk| chunk.byte_range.start >= start && chunk.byte_range.end <= start + block.len())
            .collect();
        assert!(pieces.len() > 1, "the block did not split");
        assert!(
            chunks
                .iter()
                .all(|chunk| !(chunk.text.contains("Label number") && chunk.text.contains("After the figure"))),
            "a piece of the block swallowed the prose after it"
        );
        ensure_chunks_reconstruct(
            &text,
            chunks
                .iter()
                .map(|chunk| (&chunk.byte_range, chunk.text.as_str())),
        )
        .expect("chunks rebuild the reading");
    }

    /// A range that does not describe a place in the text cannot be allowed to
    /// drop or duplicate bytes; it simply stops being a boundary.
    #[test]
    fn a_malformed_image_range_is_ignored_rather_than_obeyed() {
        let text = "Alpha beta gamma delta epsilon. ".repeat(6);
        let mut content = image_content(text.clone(), 0, 0);
        content.images[0].reading_range = Some(ByteRange {
            start: 40,
            end: text.len() + 100,
        });

        let chunks = chunk_content(&content, PathBuf::from("doc.pdf"), 80, 10);
        ensure_chunks_reconstruct(
            &text,
            chunks
                .iter()
                .map(|chunk| (&chunk.byte_range, chunk.text.as_str())),
        )
        .expect("chunks rebuild the reading");
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
