use std::path::Path;

use mupdf::{Document, MetadataName, TextPageFlags};
use tracing::trace;

use crate::types::{
    BoundingBox, ByteRange, ExtractedContent, FileMetadata, OutlineEntry, SourceMap, SourceOrigin,
    SourceSegment,
};

use super::backend::PdfBackend;

pub(super) struct MuPdfBackend;

impl PdfBackend for MuPdfBackend {
    fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent> {
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path"))?;

        // Log before any mupdf FFI call so a C-level abort leaves a breadcrumb.
        trace!("mupdf: opening {:?}", path);
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

    fn outline(&self, path: &Path) -> anyhow::Result<Vec<OutlineEntry>> {
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path"))?;
        trace!("mupdf: reading outline of {:?}", path);
        let doc = Document::open(path_str)?;
        let mut entries = Vec::new();
        flatten_outline(&doc.outlines()?, 0, &mut entries);
        Ok(entries)
    }
}

/// Depth-first flattening of the bookmark tree into reading order.
///
/// Entries whose destination does not resolve to a page are dropped rather
/// than kept with a missing locator: a bookmark pointing at an external URL or
/// a named destination this document no longer contains marks no position in
/// the text, and a consumer segmenting by these entries would place a section
/// boundary wherever it guessed. `mupdf` reports the page 0-based; extraction
/// numbers pages from one (`extract_page_words`), and the two must agree.
fn flatten_outline(outlines: &[mupdf::Outline], level: u32, out: &mut Vec<OutlineEntry>) {
    for outline in outlines {
        if let Some(dest) = &outline.dest {
            let title = outline.title.trim();
            if !title.is_empty() {
                out.push(OutlineEntry {
                    title: title.to_string(),
                    level,
                    page: Some(dest.loc.page_number + 1),
                    byte_offset: None,
                });
            }
        }
        flatten_outline(&outline.down, level + 1, out);
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
            origin: SourceOrigin::PdfPage {
                page: page_num,
                bbox,
            },
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_mupdf_backend_invalid_file() {
        let backend = MuPdfBackend;
        let dir = tempdir().unwrap();
        let path = dir.path().join("invalid.pdf");
        fs::write(&path, "not a pdf").unwrap();

        let result = backend.extract(&path);
        assert!(result.is_err());
    }

    #[test]
    fn test_mupdf_backend_non_existent_file() {
        let backend = MuPdfBackend;
        let path = Path::new("non_existent.pdf");
        let result = backend.extract(path);
        assert!(result.is_err());
    }

    #[test]
    fn test_mupdf_backend_extract_valid_pdf() {
        let backend = MuPdfBackend;
        let dir = tempdir().unwrap();
        let path = dir.path().join("valid.pdf");

        // Minimal valid PDF with "Hello World"
        let pdf_base64 = "JVBERi0xLjQKMSAwIG9iago8PAovVHlwZSAvQ2F0YWxvZwovUGFnZXMgMiAwIFIKPj4KZW5kb2JqCjIgMCBvYmoKPDwKL1R5cGUgL1BhZ2VzCi9LaWRzIFszIDAgUl0KL0NvdW50IDEKPj4KZW5kb2JqCjMgMCBvYmoKPDwKL1R5cGUgL1BhZ2UKL1BhcmVudCAyIDAgUgovTWVkaWFCb3ggWzAgMCAzMDAgMTQ0XQovQ29udGVudHMgNCAwIFIKL1Jlc291cmNlcyA8PAovRm9udCA8PAovRjEgPDwKL1R5cGUgL0ZvbnQKL1N1YnR5cGUgL1R5cGUxCi9CYXNlRm9udCAvSGVsdmV0aWNhCj4+Cj4+Cj4+Cj4+CjBlbmRvYmoKNCAwIG9iago8PAovTGVuZ3RoIDQxCj4+CnN0cmVhbQpCVAovRjEgMTggVGYKMCBldAo1MCA1MCBUZAooSGVsbG8gV29ybGQpIFRqCkVUCmVuZHN0cmVhbQplbmRvYmoKeHJlZgowIDUKMDAwMDAwMDAwMCA2NTUzNSBmIAowMDAwMDAwMDA5IDAwMDAwIG4gCjAwMDAwMDAwNTYgMDAwMDAgbiAKMDAwMDAwMDExMSAwMDAwMCBuIAowMDAwMDAwMjgyIDAwMDAwIG4gCnRyYWlsZXIKPDwKL1NpemUgNQovUm9vdCAxIDAgUgo+PgpzdGFydHhyZWYKMzcyCiUlRU9GCg==";
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let pdf_bytes = STANDARD.decode(pdf_base64).unwrap();
        fs::write(&path, &pdf_bytes).unwrap();

        let content = backend.extract(&path).expect("Should extract valid PDF");

        assert!(content.text.contains("Hello"));
        assert!(content.text.contains("World"));
        assert_eq!(content.metadata.page_count, Some(1));
        assert_eq!(content.metadata.mime.as_deref(), Some("application/pdf"));
        assert!(!content.source_map.segments.is_empty());
    }

    /// Two chapters and one nested section, destinations on pages 1 and 2.
    /// Hand-built rather than fixtured so the bookmark tree — the thing under
    /// test — is visible in the source that asserts about it.
    const OUTLINED_PDF_BASE64: &str = "JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgL091dGxpbmVzIDUgMCBSIC9QYWdlTW9kZSAvVXNlT3V0bGluZXMgPj4KZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUiA2IDAgUl0gL0NvdW50IDIgPj4KZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCAyMDAgMjAwXSAvQ29udGVudHMgNCAwIFIgL1Jlc291cmNlcyA8PCAvRm9udCA8PCAvRjEgOSAwIFIgPj4gPj4gPj4KZW5kb2JqCjQgMCBvYmoKPDwgL0xlbmd0aCA0NSA+PgpzdHJlYW0KQlQgL0YxIDEyIFRmIDIwIDEwMCBUZCAoQWxwaGEgcGFnZSBvbmUpIFRqIEVUCmVuZHN0cmVhbQplbmRvYmoKNSAwIG9iago8PCAvVHlwZSAvT3V0bGluZXMgL0ZpcnN0IDcgMCBSIC9MYXN0IDggMCBSIC9Db3VudCAyID4+CmVuZG9iago2IDAgb2JqCjw8IC9UeXBlIC9QYWdlIC9QYXJlbnQgMiAwIFIgL01lZGlhQm94IFswIDAgMjAwIDIwMF0gL0NvbnRlbnRzIDEwIDAgUiAvUmVzb3VyY2VzIDw8IC9Gb250IDw8IC9GMSA5IDAgUiA+PiA+PiA+PgplbmRvYmoKNyAwIG9iago8PCAvVGl0bGUgKENoYXB0ZXIgT25lKSAvUGFyZW50IDUgMCBSIC9EZXN0IFszIDAgUiAvRml0XSAvTmV4dCA4IDAgUiAvRmlyc3QgMTEgMCBSIC9MYXN0IDExIDAgUiAvQ291bnQgMSA+PgplbmRvYmoKOCAwIG9iago8PCAvVGl0bGUgKENoYXB0ZXIgVHdvKSAvUGFyZW50IDUgMCBSIC9EZXN0IFs2IDAgUiAvRml0XSAvUHJldiA3IDAgUiA+PgplbmRvYmoKOSAwIG9iago8PCAvVHlwZSAvRm9udCAvU3VidHlwZSAvVHlwZTEgL0Jhc2VGb250IC9IZWx2ZXRpY2EgPj4KZW5kb2JqCjEwIDAgb2JqCjw8IC9MZW5ndGggNDQgPj4Kc3RyZWFtCkJUIC9GMSAxMiBUZiAyMCAxMDAgVGQgKEJldGEgcGFnZSB0d28pIFRqIEVUCmVuZHN0cmVhbQplbmRvYmoKMTEgMCBvYmoKPDwgL1RpdGxlIChTZWN0aW9uIDEuMSkgL1BhcmVudCA3IDAgUiAvRGVzdCBbNiAwIFIgL0ZpdF0gPj4KZW5kb2JqCnhyZWYKMCAxMgowMDAwMDAwMDAwIDY1NTM1IGYgCjAwMDAwMDAwMDkgMDAwMDAgbiAKMDAwMDAwMDA5NyAwMDAwMCBuIAowMDAwMDAwMTYwIDAwMDAwIG4gCjAwMDAwMDAyODYgMDAwMDAgbiAKMDAwMDAwMDM4MSAwMDAwMCBuIAowMDAwMDAwNDUyIDAwMDAwIG4gCjAwMDAwMDA1NzkgMDAwMDAgbiAKMDAwMDAwMDcwMiAwMDAwMCBuIAowMDAwMDAwNzg5IDAwMDAwIG4gCjAwMDAwMDA4NTkgMDAwMDAgbiAKMDAwMDAwMDk1NCAwMDAwMCBuIAp0cmFpbGVyCjw8IC9TaXplIDEyIC9Sb290IDEgMCBSID4+CnN0YXJ0eHJlZgoxMDMwCiUlRU9GCg==";

    fn write_pdf(dir: &std::path::Path, name: &str, base64: &str) -> std::path::PathBuf {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let path = dir.join(name);
        fs::write(&path, STANDARD.decode(base64).unwrap()).unwrap();
        path
    }

    #[test]
    fn reads_the_bookmark_tree_in_reading_order_with_one_based_pages() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "outlined.pdf", OUTLINED_PDF_BASE64);

        let outline = MuPdfBackend.outline(&path).expect("outline reads");
        let seen: Vec<(&str, u32, Option<u32>)> = outline
            .iter()
            .map(|e| (e.title.as_str(), e.level, e.page))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("Chapter One", 0, Some(1)),
                // Depth-first: the nested section follows its parent, before
                // the parent's sibling, which is the order a reader meets them.
                ("Section 1.1", 1, Some(2)),
                ("Chapter Two", 0, Some(2)),
            ]
        );
        // A page locator, never a byte offset — extraction paginates PDFs.
        assert!(outline.iter().all(|e| e.byte_offset.is_none()));
    }

    /// The same numbering extraction uses, checked against extraction itself
    /// rather than asserted twice: a bookmark on page 1 must land in the text
    /// that `extract` attributes to page 1.
    #[test]
    fn outline_pages_agree_with_the_pages_extraction_reports() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "outlined.pdf", OUTLINED_PDF_BASE64);

        let content = MuPdfBackend.extract(&path).expect("extracts");
        let outline = MuPdfBackend.outline(&path).expect("outline reads");

        let chapter_two = outline
            .iter()
            .find(|e| e.title == "Chapter Two")
            .expect("second chapter");
        let page = chapter_two.page.expect("resolved page");
        let first_on_page = content
            .source_map
            .segments
            .iter()
            .find(|segment| {
                matches!(segment.origin, SourceOrigin::PdfPage { page: at, .. } if at == page)
            })
            .expect("extraction attributed text to that page");
        assert!(
            content.text[first_on_page.text_range.start..].starts_with("Beta"),
            "page {page} starts the second chapter's text"
        );
    }

    #[test]
    fn a_pdf_without_bookmarks_declares_no_outline() {
        let dir = tempdir().unwrap();
        // The plain single-page PDF the extraction test uses.
        let path = write_pdf(dir.path(), "plain.pdf", "JVBERi0xLjQKMSAwIG9iago8PAovVHlwZSAvQ2F0YWxvZwovUGFnZXMgMiAwIFIKPj4KZW5kb2JqCjIgMCBvYmoKPDwKL1R5cGUgL1BhZ2VzCi9LaWRzIFszIDAgUl0KL0NvdW50IDEKPj4KZW5kb2JqCjMgMCBvYmoKPDwKL1R5cGUgL1BhZ2UKL1BhcmVudCAyIDAgUgovTWVkaWFCb3ggWzAgMCAzMDAgMTQ0XQovQ29udGVudHMgNCAwIFIKL1Jlc291cmNlcyA8PAovRm9udCA8PAovRjEgPDwKL1R5cGUgL0ZvbnQKL1N1YnR5cGUgL1R5cGUxCi9CYXNlRm9udCAvSGVsdmV0aWNhCj4+Cj4+Cj4+Cj4+CjBlbmRvYmoKNCAwIG9iago8PAovTGVuZ3RoIDQxCj4+CnN0cmVhbQpCVAovRjEgMTggVGYKMCBldAo1MCA1MCBUZAooSGVsbG8gV29ybGQpIFRqCkVUCmVuZHN0cmVhbQplbmRvYmoKeHJlZgowIDUKMDAwMDAwMDAwMCA2NTUzNSBmIAowMDAwMDAwMDA5IDAwMDAwIG4gCjAwMDAwMDAwNTYgMDAwMDAgbiAKMDAwMDAwMDExMSAwMDAwMCBuIAowMDAwMDAwMjgyIDAwMDAwIG4gCnRyYWlsZXIKPDwKL1NpemUgNQovUm9vdCAxIDAgUgo+PgpzdGFydHhyZWYKMzcyCiUlRU9GCg==");

        assert!(MuPdfBackend.outline(&path).expect("outline reads").is_empty());
    }

    #[test]
    fn an_unreadable_file_is_an_error_and_not_an_empty_outline() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("invalid.pdf");
        fs::write(&path, "not a pdf").unwrap();
        assert!(MuPdfBackend.outline(&path).is_err());
    }
}
