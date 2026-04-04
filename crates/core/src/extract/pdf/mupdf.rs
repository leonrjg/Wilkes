use std::path::Path;

use mupdf::{Document, MetadataName, TextPageFlags};

use crate::types::{
    BoundingBox, ByteRange, ExtractedContent, FileMetadata, SourceMap, SourceOrigin, SourceSegment,
};

use super::backend::PdfBackend;

pub(super) struct MuPdfBackend;

impl PdfBackend for MuPdfBackend {
    fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent> {
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path"))?;

        let doc = Document::open(path_str)?;
        let page_count = doc.page_count()? as u32;

        let title = doc
            .metadata(MetadataName::Title)
            .ok()
            .filter(|s| !s.is_empty());

        let mut text = String::new();
        let mut segments: Vec<SourceSegment> = Vec::new();

        for i in 0..page_count as i32 {
            let page = doc.load_page(i)?;
            // ACCURATE_BBOXES produces tighter per-character quads.
            let text_page = page.to_text_page(TextPageFlags::ACCURATE_BBOXES)?;
            extract_page_words(&text_page, (i + 1) as u32, &mut text, &mut segments);
            if !text.ends_with('\n') {
                text.push('\n');
            }
        }

        let size_bytes = std::fs::metadata(path)?.len();

        Ok(ExtractedContent {
            text: text.clone(),
            source_map: SourceMap { segments },
            metadata: FileMetadata {
                path: path.to_path_buf(),
                size_bytes,
                mime: Some("application/pdf".into()),
                title,
                page_count: Some(page_count),
            },
        })
    }
}

/// Walk every character in `text_page` in document order, build
/// whitespace-delimited words, append them to `text`, and record a
/// `SourceSegment` per word with the merged character bounding box.
///
/// Bounding boxes are in MuPDF page space: origin top-left, y increases
/// downward.  The frontend's highlight overlay uses these coordinates directly.
fn extract_page_words(
    text_page: &mupdf::TextPage,
    page_num: u32,
    text: &mut String,
    segments: &mut Vec<SourceSegment>,
) {
    let mut word_chars = String::new();
    let mut word_start: usize = 0;
    let mut bbox_min_x = f32::MAX;
    let mut bbox_min_y = f32::MAX;
    let mut bbox_max_x = f32::MIN;
    let mut bbox_max_y = f32::MIN;
    let mut has_bbox = false;

    let flush = |word_chars: &mut String,
                     word_start: usize,
                     has_bbox: bool,
                     bbox_min_x: f32,
                     bbox_min_y: f32,
                     bbox_max_x: f32,
                     bbox_max_y: f32,
                     text: &mut String,
                     segments: &mut Vec<SourceSegment>| {
        if word_chars.is_empty() {
            return;
        }
        let start = word_start;
        text.push_str(word_chars);
        let end = text.len();
        word_chars.clear();

        let bbox = if has_bbox {
            Some(BoundingBox {
                x: bbox_min_x,
                y: bbox_min_y,
                width: (bbox_max_x - bbox_min_x).max(0.0),
                height: (bbox_max_y - bbox_min_y).max(0.0),
            })
        } else {
            None
        };
        segments.push(SourceSegment {
            text_range: ByteRange { start, end },
            origin: SourceOrigin::PdfPage { page: page_num, bbox },
        });
    };

    for block in text_page.blocks() {
        for line in block.lines() {
            for ch in line.chars() {
                let c = match ch.char() {
                    Some(c) => c,
                    None => continue,
                };

                if c.is_whitespace() {
                    flush(
                        &mut word_chars,
                        word_start,
                        has_bbox,
                        bbox_min_x,
                        bbox_min_y,
                        bbox_max_x,
                        bbox_max_y,
                        text,
                        segments,
                    );
                    has_bbox = false;
                    bbox_min_x = f32::MAX;
                    bbox_min_y = f32::MAX;
                    bbox_max_x = f32::MIN;
                    bbox_max_y = f32::MIN;
                    text.push(c);
                } else {
                    if word_chars.is_empty() {
                        word_start = text.len();
                    }
                    word_chars.push(c);

                    // Derive an axis-aligned rect from the character's bounding quad.
                    let q = ch.quad();
                    let x1 = q.ul.x.min(q.ll.x);
                    let y1 = q.ul.y.min(q.ur.y);
                    let x2 = q.ur.x.max(q.lr.x);
                    let y2 = q.ll.y.max(q.lr.y);

                    if x2 > x1 && y2 > y1 {
                        if has_bbox {
                            bbox_min_x = bbox_min_x.min(x1);
                            bbox_min_y = bbox_min_y.min(y1);
                            bbox_max_x = bbox_max_x.max(x2);
                            bbox_max_y = bbox_max_y.max(y2);
                        } else {
                            bbox_min_x = x1;
                            bbox_min_y = y1;
                            bbox_max_x = x2;
                            bbox_max_y = y2;
                            has_bbox = true;
                        }
                    }
                }
            }

            // End of line: flush any trailing word and emit a newline so the
            // next line starts on a fresh offset in the text buffer.
            flush(
                &mut word_chars,
                word_start,
                has_bbox,
                bbox_min_x,
                bbox_min_y,
                bbox_max_x,
                bbox_max_y,
                text,
                segments,
            );
            has_bbox = false;
            bbox_min_x = f32::MAX;
            bbox_min_y = f32::MAX;
            bbox_max_x = f32::MIN;
            bbox_max_y = f32::MIN;
            text.push('\n');
        }
    }

    // Flush any word left over after the last block (no trailing whitespace).
    flush(
        &mut word_chars,
        word_start,
        has_bbox,
        bbox_min_x,
        bbox_min_y,
        bbox_max_x,
        bbox_max_y,
        text,
        segments,
    );
}
