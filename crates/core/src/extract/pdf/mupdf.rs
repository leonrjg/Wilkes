use std::path::Path;
use std::sync::Arc;

use mupdf::text_page::TextBlockType;
use mupdf::{DestinationKind, Document, MetadataName, TextPageFlags};
use tracing::{info, trace, warn};

use crate::extract::image::{
    self, AnalysisContext, DiscoveredImage, ImageAnalyzer, NativeTextOnPage,
};
use crate::types::{
    BoundingBox, DeclaredOutline, ExtractedContent, ExtractedImage, ExtractionDiagnostics,
    FileMetadata, ImageTransform, OutlineAnchor, OutlineEntry, SourceMap, SourceOrigin,
};

use super::backend::PdfBackend;
use super::sanitize::{self, Block, Line, Page, Reading, Word};

#[derive(Default)]
pub(super) struct MuPdfBackend {
    /// The configured image analyzer, or none. One analyzer per extractor,
    /// and one extractor per registry, so every consumer of a rendition sees
    /// the same enrichment — an analyzer configured per call site is how two
    /// consumers end up disagreeing about what a document says.
    analyzer: Option<Arc<dyn ImageAnalyzer>>,
}

impl MuPdfBackend {
    pub(super) fn new(analyzer: Option<Arc<dyn ImageAnalyzer>>) -> Self {
        Self { analyzer }
    }
}

impl PdfBackend for MuPdfBackend {
    fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent> {
        let document = read_document(path, self.analyzer.as_deref())?;
        let size_bytes = std::fs::metadata(path)?.len();

        Ok(ExtractedContent {
            text: document.reading.text,
            source_map: SourceMap {
                segments: document.reading.segments,
            },
            metadata: FileMetadata {
                path: path.to_path_buf(),
                size_bytes,
                mime: Some("application/pdf".into()),
                title: document.title,
                page_count: Some(document.page_count),
            },
            images: document.images,
        })
    }

    fn outline(&self, path: &Path) -> anyhow::Result<DeclaredOutline> {
        let document = read_document(path, self.analyzer.as_deref())?;
        let anchors = PageAnchors::new(&document.reading);
        let mut entries = Vec::new();
        flatten_outline(
            &document.doc.outlines()?,
            0,
            &document.reading,
            &anchors,
            &mut entries,
        );
        // Surfaced per document as well as per entry: an outline answered
        // mostly by rung 3 is an outline still snapped to pages, and that is a
        // fact about this document that has to be visible where it is produced
        // rather than inferred later from sections that start early.
        if !entries.is_empty() {
            let rung = |anchor| entries.iter().filter(|e| e.anchor == anchor).count();
            info!(
                "outline of {:?}: {} entries, {} by destination coordinate, {} by title, {} page-only",
                path,
                entries.len(),
                rung(OutlineAnchor::DestinationCoordinate),
                rung(OutlineAnchor::TitleMatch),
                rung(OutlineAnchor::Page),
            );
        }
        Ok(DeclaredOutline {
            entries,
            diagnostics: document.diagnostics,
        })
    }
}

/// One PDF, read once: the sanitized reading, the metadata, and the open
/// document the bookmark tree still has to be asked for.
struct PdfDocument {
    doc: Document,
    reading: Reading,
    page_count: u32,
    title: Option<String>,
    diagnostics: ExtractionDiagnostics,
    images: Vec<ExtractedImage>,
}

/// Read a PDF into the one reading Wilkes has of it.
///
/// Extraction and outline reading share this because they have to: a byte
/// offset into the reading is only meaningful against the reading it indexes,
/// and there is exactly one of those. Asking for the outline therefore costs
/// what extraction costs — the price of the offsets being real rather than a
/// page number wearing an offset's clothes.
fn read_document(
    path: &Path,
    analyzer: Option<&dyn ImageAnalyzer>,
) -> anyhow::Result<PdfDocument> {
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

    let mut pages = Vec::with_capacity(page_count as usize);
    let mut discovered: Vec<DiscoveredImage> = Vec::new();
    for i in 0..page_count as i32 {
        let page = doc.load_page(i)?;
        let height = page.bounds().map(|bounds| bounds.y1 - bounds.y0)?;
        // ACCURATE_BBOXES produces tighter per-character quads.
        // PRESERVE_IMAGES adds the page's image blocks to the same block list,
        // in the order the page draws them — which is the only discovery
        // signal this phase needs, and the only thing that establishes where
        // an image sits relative to the text around it.
        let text_page = page.to_text_page(
            TextPageFlags::ACCURATE_BBOXES | TextPageFlags::PRESERVE_IMAGES,
        )?;
        pages.push(extract_page_words(
            &text_page,
            (i + 1) as u32,
            height,
            &mut discovered,
        ));
    }

    let mut diagnostics = ExtractionDiagnostics::default();
    let context = AnalysisContext::new(native_words(&pages));
    let mut images = image::analyze(&discovered, &context, analyzer, &mut diagnostics);
    // The pixels have done their work. Dropping them here rather than at the
    // end of extraction keeps a document's worth of decoded artwork out of
    // memory while the reading is being built.
    drop(discovered);
    let reading = sanitize::sanitize(pages, &mut images, &mut diagnostics);
    if diagnostics.ambiguous_column_pages > 0
        || diagnostics.relocated_marginalia_blocks > 0
        || diagnostics.removed_furniture_runs > 0
    {
        info!(
            "sanitized {:?}: {} pages, {} with one body column, {} ambiguous, \
             {} marginalia blocks relocated, {} furniture runs removed, \
             {} wrap hyphens joined, {} kept",
            path,
            diagnostics.pages,
            diagnostics.body_column_pages,
            diagnostics.ambiguous_column_pages,
            diagnostics.relocated_marginalia_blocks,
            diagnostics.removed_furniture_runs,
            diagnostics.joined_wrap_hyphens,
            diagnostics.kept_wrap_hyphens,
        );
    }

    if diagnostics.native_images_found > 0 {
        info!(
            "images in {:?}: {} found, {} analyzed, {} skipped by a technical limit, \
             {} transcribed, {} recognition failures, {} regions accepted, \
             {} below the threshold, {} already native text, \
             {} described, {} description failures, {} with no describer configured",
            path,
            diagnostics.native_images_found,
            diagnostics.native_images_analyzed,
            diagnostics.native_images_skipped_technical_limit,
            diagnostics.images_ocr_succeeded,
            diagnostics.images_ocr_failed,
            diagnostics.ocr_regions_accepted,
            diagnostics.ocr_regions_rejected_low_confidence,
            diagnostics.ocr_regions_deduplicated_against_native_text,
            diagnostics.images_description_succeeded,
            diagnostics.images_description_failed,
            diagnostics.images_description_not_configured,
        );
    }

    Ok(PdfDocument {
        doc,
        reading,
        page_count,
        title,
        diagnostics,
        images,
    })
}

/// Every word the pages draw as their own glyphs, for the deduplication rule.
///
/// Read from the pages as MuPDF gave them, before sanitation moves anything:
/// the question is where the document *drew* a word, and relocation changes
/// where a word is read, never where it was drawn.
fn native_words(pages: &[Page]) -> Vec<NativeTextOnPage> {
    let mut words = Vec::new();
    for page in pages {
        for block in &page.blocks {
            for line in &block.lines {
                for word in &line.words {
                    if let Some(bbox) = &word.bbox {
                        words.push(NativeTextOnPage {
                            page: page.number,
                            text: word.text.clone(),
                            bbox: bbox.clone(),
                        });
                    }
                }
            }
        }
    }
    words
}

/// Where each page's segments sit in the reading. Marginalia are relocated
/// within their own page, never across one, so a page's segments stay
/// contiguous and a page's text is a slice.
struct PageAnchors {
    /// `(page, first segment index, last segment index + 1)`, ascending.
    pages: Vec<(u32, usize, usize)>,
}

impl PageAnchors {
    fn new(reading: &Reading) -> Self {
        let mut pages: Vec<(u32, usize, usize)> = Vec::new();
        for (index, segment) in reading.segments.iter().enumerate() {
            let SourceOrigin::PdfPage { page, .. } = segment.origin else {
                continue;
            };
            match pages.last_mut() {
                Some(last) if last.0 == page => last.2 = index + 1,
                _ => pages.push((page, index, index + 1)),
            }
        }
        Self { pages }
    }

    fn segments(&self, page: u32) -> Option<(usize, usize)> {
        self.pages
            .iter()
            .find(|(at, _, _)| *at == page)
            .map(|(_, start, end)| (*start, *end))
    }
}

/// Resolve one bookmark's position in the reading, and say which rung of the
/// ladder answered.
///
/// 1. The destination's own vertical coordinate, resolved to the first word at
///    or below it.
/// 2. The bookmark title, found in the destination page's text. Destinations
///    that carry no coordinate (`Fit`, `FitB`) start here.
/// 3. Nothing. The entry keeps its page and gets no offset — a bookmark whose
///    title was renumbered, restyled or set as an image marks a position
///    Wilkes cannot see, and guessing one would put a section boundary
///    wherever the guess landed.
fn anchor_entry(
    reading: &Reading,
    anchors: &PageAnchors,
    page: u32,
    kind: DestinationKind,
    title: &str,
) -> (Option<usize>, OutlineAnchor) {
    let Some((first, end)) = anchors.segments(page) else {
        return (None, OutlineAnchor::Page);
    };

    if let Some(top) = destination_top(kind) {
        let below = reading.segments[first..end].iter().find(|segment| {
            match &segment.origin {
                SourceOrigin::PdfPage { bbox, .. } => bbox
                    .as_ref()
                    // A word entirely above the destination ends above it; one
                    // the destination lands on or inside does not.
                    .is_some_and(|bbox| bbox.y + bbox.height > top + DESTINATION_EPSILON),
                _ => false,
            }
        });
        if let Some(segment) = below {
            return (
                Some(segment.text_range.start),
                OutlineAnchor::DestinationCoordinate,
            );
        }
    }

    let page_start = reading.segments[first].text_range.start;
    let page_end = reading.segments[end - 1].text_range.end;
    if let Some(offset) = title_offset(&reading.text[page_start..page_end], title) {
        let absolute = page_start + offset;
        // Snap back to the start of the word the match begins inside, so a
        // section begins at a word and not in the middle of one.
        let start = reading.segments[first..end]
            .iter()
            .find(|segment| segment.text_range.end > absolute)
            .map_or(absolute, |segment| segment.text_range.start.min(absolute));
        return (Some(start), OutlineAnchor::TitleMatch);
    }

    (None, OutlineAnchor::Page)
}

/// Half a point: enough to absorb the rounding a destination coordinate
/// carries, far less than the height of a line.
const DESTINATION_EPSILON: f32 = 0.5;

/// The vertical coordinate a destination carries, in the same space as the
/// word boxes.
///
/// `mupdf` resolves a destination through the page's own transform
/// (`pdf_page_obj_transform`, applied in `populate_destination`), so what
/// arrives here is already MuPDF page space: origin top-left, y increasing
/// downward — the space `extract_page_words` records boxes in. That is
/// asserted by `a_destination_coordinate_anchors_the_entry_where_it_points`
/// against a PDF whose bookmark's user-space `y` and page-space `y` differ, so
/// a `mupdf` that stopped normalizing would fail the test rather than move
/// every heading in the corpus.
fn destination_top(kind: DestinationKind) -> Option<f32> {
    match kind {
        DestinationKind::XYZ { top, .. } => top.filter(|top| top.is_finite()),
        DestinationKind::FitH { top } | DestinationKind::FitBH { top } => {
            top.is_finite().then_some(top)
        }
        DestinationKind::FitR { top, .. } => top.is_finite().then_some(top),
        DestinationKind::Fit
        | DestinationKind::FitB
        | DestinationKind::FitV { .. }
        | DestinationKind::FitBV { .. } => None,
    }
}

/// The first occurrence of `title` in `text`, matched the way literal PDF
/// search matches — the same normalization, because the question is the same
/// one: does this string appear on this page, ignoring how the page set it.
fn title_offset(text: &str, title: &str) -> Option<usize> {
    use grep_matcher::Matcher;

    let projection = crate::search::pdf_projection::PdfSearchProjection::new(text);
    let matcher = crate::search::pdf_projection::literal_matcher(title, false).ok()?;
    let found = matcher.find(projection.as_bytes()).ok()??;
    projection
        .raw_range(crate::types::ByteRange {
            start: found.start(),
            end: found.end(),
        })
        .map(|range| range.start)
}

/// Depth-first flattening of the bookmark tree into reading order.
///
/// Entries whose destination does not resolve to a page are dropped rather
/// than kept with a missing locator: a bookmark pointing at an external URL or
/// a named destination this document no longer contains marks no position in
/// the text, and a consumer segmenting by these entries would place a section
/// boundary wherever it guessed. `mupdf` reports the page 0-based; extraction
/// numbers pages from one (`extract_page_words`), and the two must agree.
fn flatten_outline(
    outlines: &[mupdf::Outline],
    level: u32,
    reading: &Reading,
    anchors: &PageAnchors,
    out: &mut Vec<OutlineEntry>,
) {
    for outline in outlines {
        if let Some(dest) = &outline.dest {
            let title = outline.title.trim();
            if !title.is_empty() {
                let page = dest.loc.page_number + 1;
                let (byte_offset, anchor) = anchor_entry(reading, anchors, page, dest.kind, title);
                out.push(OutlineEntry {
                    title: title.to_string(),
                    level,
                    page: Some(page),
                    byte_offset,
                    anchor,
                });
            }
        }
        flatten_outline(&outline.down, level + 1, reading, anchors, out);
    }
}

/// Walk every character in `text_page` in document order and build the page's
/// blocks, lines and whitespace-delimited words, each with the merged
/// character bounding box.
///
/// Bounding boxes are in MuPDF page space: origin top-left, y increases
/// downward.  The frontend's highlight overlay uses these coordinates directly.
///
/// This produces the page as the page is; [`sanitize`](super::sanitize) turns
/// it into the document's reading.
fn extract_page_words(
    text_page: &mupdf::TextPage,
    page_num: u32,
    height: f32,
    discovered: &mut Vec<DiscoveredImage>,
) -> Page {
    let mut blocks = Vec::new();

    for block in text_page.blocks() {
        if block.r#type() == TextBlockType::Image {
            if let Some(found) = discover_image(&block, page_num, discovered.len()) {
                blocks.push(Block {
                    lines: Vec::new(),
                    image: Some(discovered.len()),
                });
                discovered.push(found);
            }
            continue;
        }
        let mut lines = Vec::new();
        for line in block.lines() {
            let mut out = Line::default();
            let mut word_chars = String::new();
            let mut bbox: Option<BoundingBox> = None;

            for ch in line.chars() {
                let c = match ch.char() {
                    Some(c) => c,
                    None => continue,
                };

                if c.is_whitespace() {
                    flush(&mut out, &mut word_chars, &mut bbox);
                    out.push_space(c);
                    continue;
                }

                word_chars.push(c);

                // Derive an axis-aligned rect from the character's bounding quad.
                let q = ch.quad();
                let x1 = q.ul.x.min(q.ll.x);
                let y1 = q.ul.y.min(q.ur.y);
                let x2 = q.ur.x.max(q.lr.x);
                let y2 = q.ll.y.max(q.lr.y);

                if x2 > x1 && y2 > y1 {
                    let next = BoundingBox {
                        x: x1,
                        y: y1,
                        width: x2 - x1,
                        height: y2 - y1,
                    };
                    bbox = Some(match bbox {
                        Some(existing) => existing.merge(&next),
                        None => next,
                    });
                }
            }

            // End of line: flush any trailing word. The line itself becomes a
            // newline when the reading is rendered.
            flush(&mut out, &mut word_chars, &mut bbox);
            lines.push(out);
        }
        if !lines.is_empty() {
            blocks.push(Block { lines, image: None });
        }
    }

    Page {
        number: page_num,
        height,
        blocks,
    }
}

/// Read one native image block: where the page put it, and its pixels.
///
/// Everything mechanical. No caption is looked for, no neighbouring text is
/// consulted, and nothing decides whether this is a figure — a repeated logo
/// arrives here exactly as a diagram does, and it is the recognizer finding no
/// text in it that keeps it out of the reading.
fn discover_image(
    block: &mupdf::TextBlock<'_>,
    page: u32,
    ordinal: usize,
) -> Option<DiscoveredImage> {
    let bounds = block.bounds();
    let bbox = BoundingBox {
        x: bounds.x0,
        y: bounds.y0,
        width: (bounds.x1 - bounds.x0).max(0.0),
        height: (bounds.y1 - bounds.y0).max(0.0),
    };
    let ctm = block.ctm()?;
    let transform = ImageTransform {
        a: ctm.a,
        b: ctm.b,
        c: ctm.c,
        d: ctm.d,
        e: ctm.e,
        f: ctm.f,
    };
    let id = format!("p{page}-i{ordinal}");

    let mut found = DiscoveredImage {
        id: id.clone(),
        page,
        bbox,
        transform,
        decoded: None,
        rejected: None,
    };

    let Some(image) = block.image() else {
        found.rejected = Some("image block carries no image".to_string());
        return Some(found);
    };
    // Ask the technical limits before decoding: the point of a decode limit is
    // not to decode the thing.
    if let Some(reason) = image::technical_limit(image.width(), image.height()) {
        found.rejected = Some(reason);
        return Some(found);
    }
    let pixmap = match image.to_pixmap() {
        Ok(pixmap) => pixmap,
        Err(error) => {
            warn!("image {id}: decode failed: {error}");
            found.rejected = Some(format!("decode failed: {error}"));
            return Some(found);
        }
    };
    match image::decode(
        pixmap.width(),
        pixmap.height(),
        pixmap.n() as usize,
        pixmap.stride() as usize,
        pixmap.samples(),
    ) {
        Ok(decoded) => found.decoded = Some(decoded),
        Err(reason) => {
            warn!("image {id}: {reason}");
            found.rejected = Some(reason);
        }
    }
    Some(found)
}

fn flush(line: &mut Line, word_chars: &mut String, bbox: &mut Option<BoundingBox>) {
    if word_chars.is_empty() {
        *bbox = None;
        return;
    }
    line.push_word(Word {
        text: std::mem::take(word_chars),
        bbox: bbox.take(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ByteRange;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_mupdf_backend_invalid_file() {
        let backend = MuPdfBackend::default();
        let dir = tempdir().unwrap();
        let path = dir.path().join("invalid.pdf");
        fs::write(&path, "not a pdf").unwrap();

        let result = backend.extract(&path);
        assert!(result.is_err());
    }

    #[test]
    fn test_mupdf_backend_non_existent_file() {
        let backend = MuPdfBackend::default();
        let path = Path::new("non_existent.pdf");
        let result = backend.extract(path);
        assert!(result.is_err());
    }

    #[test]
    fn test_mupdf_backend_extract_valid_pdf() {
        let backend = MuPdfBackend::default();
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

    /// One page, three lines, and two bookmarks: one `XYZ` destination whose
    /// user-space `y` (420) and page-space `y` (180) are different numbers and
    /// land on different lines, and one `Fit` destination that carries no
    /// coordinate at all.
    const COORDINATE_OUTLINED_PDF_BASE64: &str = "JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgL091dGxpbmVzIDUgMCBSIC9QYWdlTW9kZSAvVXNlT3V0bGluZXMgPj4KZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4KZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCAyMDAgNjAwXSAvQ29udGVudHMgNCAwIFIgL1Jlc291cmNlcyA8PCAvRm9udCA8PCAvRjEgNiAwIFIgPj4gPj4gPj4KZW5kb2JqCjQgMCBvYmoKPDwgL0xlbmd0aCAxMjkgPj4Kc3RyZWFtCkJUIC9GMSAxMiBUZiAyMCA1NTAgVGQgKEFscGhhIGhlYWRpbmcpIFRqIEVUCkJUIC9GMSAxMiBUZiAyMCA0MDAgVGQgKEJldGEgbWlkZGxlKSBUaiBFVApCVCAvRjEgMTIgVGYgMjAgMjAwIFRkIChHYW1tYSB0YWlsKSBUaiBFVAplbmRzdHJlYW0KZW5kb2JqCjUgMCBvYmoKPDwgL1R5cGUgL091dGxpbmVzIC9GaXJzdCA3IDAgUiAvTGFzdCA4IDAgUiAvQ291bnQgMiA+PgplbmRvYmoKNiAwIG9iago8PCAvVHlwZSAvRm9udCAvU3VidHlwZSAvVHlwZTEgL0Jhc2VGb250IC9IZWx2ZXRpY2EgPj4KZW5kb2JqCjcgMCBvYmoKPDwgL1RpdGxlIChNaWRkbGUpIC9QYXJlbnQgNSAwIFIgL0Rlc3QgWzMgMCBSIC9YWVogMjAgNDIwIDBdIC9OZXh0IDggMCBSID4+CmVuZG9iago4IDAgb2JqCjw8IC9UaXRsZSAoR2FtbWEgdGFpbCkgL1BhcmVudCA1IDAgUiAvRGVzdCBbMyAwIFIgL0ZpdF0gL1ByZXYgNyAwIFIgPj4KZW5kb2JqCnhyZWYKMCA5CjAwMDAwMDAwMDAgNjU1MzUgZiAKMDAwMDAwMDAwOSAwMDAwMCBuIAowMDAwMDAwMDk3IDAwMDAwIG4gCjAwMDAwMDAxNTQgMDAwMDAgbiAKMDAwMDAwMDI4MCAwMDAwMCBuIAowMDAwMDAwNDYwIDAwMDAwIG4gCjAwMDAwMDA1MzEgMDAwMDAgbiAKMDAwMDAwMDYwMSAwMDAwMCBuIAowMDAwMDAwNjkyIDAwMDAwIG4gCnRyYWlsZXIKPDwgL1NpemUgOSAvUm9vdCAxIDAgUiA+PgpzdGFydHhyZWYKNzc4CiUlRU9GCg==";

    /// One page, a line of text, a 4x2 RGB image placed at a known matrix,
    /// and a caption below it. Hand-built so the thing under test — where the
    /// image is and how big — is visible in the source that asserts about it.
    const IMAGE_PDF_BASE64: &str = "JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4KZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4KZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCAyMDAgMzAwXSAvQ29udGVudHMgNCAwIFIgL1Jlc291cmNlcyA8PCAvRm9udCA8PCAvRjEgNiAwIFIgPj4gL1hPYmplY3QgPDwgL0ltMCA1IDAgUiA+PiA+PiA+PgplbmRvYmoKNCAwIG9iago8PCAvTGVuZ3RoIDEzMiA+PgpzdHJlYW0KQlQgL0YxIDEyIFRmIDIwIDI1MCBUZCAoQWJvdmUgdGhlIHBpY3R1cmUpIFRqIEVUCnEgMTYwIDAgMCA4MCAyMCAxMDAgY20gL0ltMCBEbyBRCkJUIC9GMSAxMiBUZiAyMCA2MCBUZCAoRmlndXJlIDE6IGEgY2FwdGlvbikgVGogRVQKZW5kc3RyZWFtCmVuZG9iago1IDAgb2JqCjw8IC9UeXBlIC9YT2JqZWN0IC9TdWJ0eXBlIC9JbWFnZSAvV2lkdGggNCAvSGVpZ2h0IDIgL0NvbG9yU3BhY2UgL0RldmljZVJHQiAvQml0c1BlckNvbXBvbmVudCA4IC9MZW5ndGggMjQgPj4Kc3RyZWFtCv8AAAD/AAAA////AAAAAEBAQICAgP///wplbmRzdHJlYW0KZW5kb2JqCjYgMCBvYmoKPDwgL1R5cGUgL0ZvbnQgL1N1YnR5cGUgL1R5cGUxIC9CYXNlRm9udCAvSGVsdmV0aWNhID4+CmVuZG9iagp4cmVmCjAgNwowMDAwMDAwMDAwIDY1NTM1IGYgCjAwMDAwMDAwMDkgMDAwMDAgbiAKMDAwMDAwMDA1OCAwMDAwMCBuIAowMDAwMDAwMTE1IDAwMDAwIG4gCjAwMDAwMDAyNjcgMDAwMDAgbiAKMDAwMDAwMDQ0OSAwMDAwMCBuIAowMDAwMDAwNjE2IDAwMDAwIG4gCnRyYWlsZXIKPDwgL1NpemUgNyAvUm9vdCAxIDAgUiA+PgpzdGFydHhyZWYKNjg2CiUlRU9GCg==";

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

        let outline = MuPdfBackend::default().outline(&path).expect("outline reads").entries;
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
    }

    /// The same numbering extraction uses, checked against extraction itself
    /// rather than asserted twice: a bookmark on page 1 must land in the text
    /// that `extract` attributes to page 1.
    #[test]
    fn outline_pages_agree_with_the_pages_extraction_reports() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "outlined.pdf", OUTLINED_PDF_BASE64);

        let content = MuPdfBackend::default().extract(&path).expect("extracts");
        let outline = MuPdfBackend::default().outline(&path).expect("outline reads").entries;

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

    /// Rung 1, and the coordinate-space assumption it rests on.
    ///
    /// The bookmark's `y` is 420 in PDF user space, which is 180 in MuPDF page
    /// space, and the two pick different lines of this page — 420 is below all
    /// three lines, 180 is just above the middle one. Landing on the middle
    /// line is therefore proof that `mupdf` hands us page space, not merely
    /// that the entry resolved to something.
    #[test]
    fn a_destination_coordinate_anchors_the_entry_where_it_points() {
        let dir = tempdir().unwrap();
        let path = write_pdf(
            dir.path(),
            "coordinates.pdf",
            COORDINATE_OUTLINED_PDF_BASE64,
        );

        let content = MuPdfBackend::default().extract(&path).expect("extracts");
        let outline = MuPdfBackend::default().outline(&path).expect("outline reads").entries;

        let middle = outline.iter().find(|e| e.title == "Middle").expect("entry");
        assert_eq!(middle.anchor, OutlineAnchor::DestinationCoordinate);
        let offset = middle.byte_offset.expect("anchored to a position");
        assert!(
            content.text[offset..].starts_with("Beta middle"),
            "anchored at {:?}",
            &content.text[offset..]
        );
    }

    /// Rung 2: a `Fit` destination carries no coordinate, so the title has to
    /// find itself on the page.
    #[test]
    fn a_destination_without_a_coordinate_falls_back_to_the_title() {
        let dir = tempdir().unwrap();
        let path = write_pdf(
            dir.path(),
            "coordinates.pdf",
            COORDINATE_OUTLINED_PDF_BASE64,
        );

        let content = MuPdfBackend::default().extract(&path).expect("extracts");
        let outline = MuPdfBackend::default().outline(&path).expect("outline reads").entries;

        let tail = outline
            .iter()
            .find(|e| e.title == "Gamma tail")
            .expect("entry");
        assert_eq!(tail.anchor, OutlineAnchor::TitleMatch);
        let offset = tail.byte_offset.expect("anchored to a position");
        assert!(
            content.text[offset..].starts_with("Gamma tail"),
            "anchored at {:?}",
            &content.text[offset..]
        );
    }

    /// Rung 3: a bookmark whose title is nowhere on its page and whose
    /// destination is coordinate-less keeps its page and gets no offset.
    #[test]
    fn a_bookmark_that_resolves_to_nothing_degrades_to_its_page() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "outlined.pdf", OUTLINED_PDF_BASE64);

        let outline = MuPdfBackend::default().outline(&path).expect("outline reads").entries;
        let chapter_one = outline
            .iter()
            .find(|e| e.title == "Chapter One")
            .expect("entry");
        assert_eq!(chapter_one.anchor, OutlineAnchor::Page);
        assert_eq!(chapter_one.byte_offset, None);
        assert_eq!(chapter_one.page, Some(1));
    }

    #[test]
    fn a_pdf_without_bookmarks_declares_no_outline() {
        let dir = tempdir().unwrap();
        // The plain single-page PDF the extraction test uses.
        let path = write_pdf(dir.path(), "plain.pdf", "JVBERi0xLjQKMSAwIG9iago8PAovVHlwZSAvQ2F0YWxvZwovUGFnZXMgMiAwIFIKPj4KZW5kb2JqCjIgMCBvYmoKPDwKL1R5cGUgL1BhZ2VzCi9LaWRzIFszIDAgUl0KL0NvdW50IDEKPj4KZW5kb2JqCjMgMCBvYmoKPDwKL1R5cGUgL1BhZ2UKL1BhcmVudCAyIDAgUgovTWVkaWFCb3ggWzAgMCAzMDAgMTQ0XQovQ29udGVudHMgNCAwIFIKL1Jlc291cmNlcyA8PAovRm9udCA8PAovRjEgPDwKL1R5cGUgL0ZvbnQKL1N1YnR5cGUgL1R5cGUxCi9CYXNlRm9udCAvSGVsdmV0aWNhCj4+Cj4+Cj4+Cj4+CjBlbmRvYmoKNCAwIG9iago8PAovTGVuZ3RoIDQxCj4+CnN0cmVhbQpCVAovRjEgMTggVGYKMCBldAo1MCA1MCBUZAooSGVsbG8gV29ybGQpIFRqCkVUCmVuZHN0cmVhbQplbmRvYmoKeHJlZgowIDUKMDAwMDAwMDAwMCA2NTUzNSBmIAowMDAwMDAwMDA5IDAwMDAwIG4gCjAwMDAwMDAwNTYgMDAwMDAgbiAKMDAwMDAwMDExMSAwMDAwMCBuIAowMDAwMDAwMjgyIDAwMDAwIG4gCnRyYWlsZXIKPDwKL1NpemUgNQovUm9vdCAxIDAgUgo+PgpzdGFydHhyZWYKMzcyCiUlRU9GCg==");

        assert!(MuPdfBackend::default()
            .outline(&path)
            .expect("outline reads")
            .entries
            .is_empty());
    }

    #[test]
    fn an_unreadable_file_is_an_error_and_not_an_empty_outline() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("invalid.pdf");
        fs::write(&path, "not a pdf").unwrap();
        assert!(MuPdfBackend::default().outline(&path).is_err());
    }

    // ── Native images ───────────────────────────────────────────────────

    /// Discovery is mechanical: the block MuPDF exposes, its placement and
    /// its pixels. No caption is matched, and the caption in this fixture is
    /// there to prove it is not consulted.
    #[test]
    fn a_native_image_block_is_found_with_its_placement_and_pixels() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "image.pdf", IMAGE_PDF_BASE64);

        let content = MuPdfBackend::default().extract(&path).expect("extracts");
        assert_eq!(content.images.len(), 1, "one image block on the page");
        let image = &content.images[0];

        assert_eq!(image.id, "p1-i0");
        assert_eq!(image.page, 1);
        assert_eq!((image.pixel_width, image.pixel_height), (4, 2));
        assert!(!image.image_sha256.is_empty(), "the pixels were digested");

        // Placed by `160 0 0 80 20 100 cm` on a 300-point-tall page, so the
        // box is 160 x 80 with its top 300 - 180 = 120 from the top edge.
        assert!((image.bbox.width - 160.0).abs() < 1.0, "{:?}", image.bbox);
        assert!((image.bbox.height - 80.0).abs() < 1.0, "{:?}", image.bbox);
        assert!((image.bbox.x - 20.0).abs() < 1.0, "{:?}", image.bbox);
        assert!((image.bbox.y - 120.0).abs() < 1.0, "{:?}", image.bbox);
    }

    /// The transform is what turns a position inside the image into a
    /// position on the page. Checked at the corners, because a flip that put
    /// the first pixel row at the bottom would still produce a plausible
    /// bounding box and an upside-down highlight.
    #[test]
    fn the_transform_maps_image_pixels_onto_the_page() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "image.pdf", IMAGE_PDF_BASE64);

        let content = MuPdfBackend::default().extract(&path).expect("extracts");
        let image = &content.images[0];
        let (width, height) = (image.pixel_width, image.pixel_height);

        let top_left = image.transform.pixel_to_page(0.0, 0.0, width, height);
        let bottom_right = image
            .transform
            .pixel_to_page(width as f32, height as f32, width, height);

        assert!(
            (top_left.x - image.bbox.x).abs() < 1.0 && (top_left.y - image.bbox.y).abs() < 1.0,
            "the first pixel is at the box's top-left, got {top_left:?} for {:?}",
            image.bbox
        );
        assert!(
            (bottom_right.x - (image.bbox.x + image.bbox.width)).abs() < 1.0
                && (bottom_right.y - (image.bbox.y + image.bbox.height)).abs() < 1.0,
            "the last pixel is at the box's bottom-right, got {bottom_right:?} for {:?}",
            image.bbox
        );
    }

    /// With no analyzer the reading is the document's own text, unchanged,
    /// and the image is counted rather than described. "No recognizer here"
    /// and "no text in the picture" are different facts and read differently.
    #[test]
    fn without_an_analyzer_the_reading_is_unchanged_and_the_image_is_counted() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "image.pdf", IMAGE_PDF_BASE64);

        let content = MuPdfBackend::default().extract(&path).expect("extracts");
        assert!(content.text.contains("Above the picture"));
        assert!(content.text.contains("Figure 1: a caption"));
        assert!(!content.text.contains("Image embedded text:"));
        assert!(content.images[0].reading_range.is_none());
        assert!(matches!(
            content.images[0].status,
            crate::types::ImageAnalysisStatus::Partial { .. }
        ));

        let diagnostics = MuPdfBackend::default()
            .outline(&path)
            .expect("outline reads")
            .diagnostics;
        assert_eq!(diagnostics.native_images_found, 1);
        assert_eq!(diagnostics.native_images_analyzed, 0);
    }

    /// Enrichment is written where the page drew the picture — between the
    /// line above it and the caption below — and not after the text that
    /// looks like a caption, because no caption was looked for.
    #[test]
    fn enrichment_lands_at_the_image_block_rather_than_near_a_caption() {
        use crate::extract::image::{AnalysisContext, DiscoveredImage, ImageAnalyzer};
        use crate::types::{ImageDescription, ImageOcrRegion, OcrAdmission, Point};

        struct Fixed;
        impl ImageAnalyzer for Fixed {
            fn identity(&self) -> String {
                "fixed-analyzer-v1".to_string()
            }
            fn analyze(
                &self,
                images: &mut [ExtractedImage],
                _discovered: &[DiscoveredImage],
                _context: &AnalysisContext,
                diagnostics: &mut ExtractionDiagnostics,
            ) {
                for image in images {
                    diagnostics.native_images_analyzed += 1;
                    diagnostics.ocr_regions_accepted += 1;
                    image.analyzer_identity = self.identity();
                    image.ocr_regions = vec![ImageOcrRegion {
                        text: "Knowledge base".to_string(),
                        confidence: 0.9,
                        polygon_within_image: vec![Point { x: 0.0, y: 0.0 }],
                        page_polygon: vec![
                            Point { x: 30.0, y: 130.0 },
                            Point { x: 90.0, y: 130.0 },
                            Point { x: 90.0, y: 150.0 },
                            Point { x: 30.0, y: 150.0 },
                        ],
                        admission: OcrAdmission::Accepted,
                    }];
                    image.description = Some(ImageDescription {
                        description: "Four coloured squares over four grey ones.".to_string(),
                        relationships: Vec::new(),
                    });
                }
            }
        }

        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "image.pdf", IMAGE_PDF_BASE64);
        let backend = MuPdfBackend::new(Some(Arc::new(Fixed)));
        let content = backend.extract(&path).expect("extracts");

        let above = content.text.find("Above the picture").expect("text above");
        let block = content
            .text
            .find("Image embedded text: Knowledge base.")
            .expect("the transcription is in the reading");
        let caption = content.text.find("Figure 1:").expect("the caption");
        assert!(above < block && block < caption, "{:?}", content.text);
        assert!(content
            .text
            .contains("Image description: Four coloured squares over four grey ones."));

        // The author's own lines are neither moved nor duplicated.
        assert_eq!(content.text.matches("Figure 1: a caption").count(), 1);
        assert_eq!(content.text.matches("Above the picture").count(), 1);

        // The image knows where its block landed, and the bytes there are it.
        let range = content.images[0]
            .reading_range
            .clone()
            .expect("the block has a range");
        assert!(content.text[range.start..range.end].starts_with("Image embedded text:"));
    }

    /// Every byte the enrichment inserted resolves to a page and says what it
    /// is. The transcription resolves to its own polygon, not to the whole
    /// image, which is what lets exact search highlight the label.
    #[test]
    fn every_inserted_byte_has_a_locator_and_truthful_provenance() {
        use crate::extract::image::{AnalysisContext, DiscoveredImage, ImageAnalyzer};
        use crate::types::{ImageOcrRegion, OcrAdmission, Point, TextProvenance};

        struct Spotter;
        impl ImageAnalyzer for Spotter {
            fn identity(&self) -> String {
                "spotter-v1".to_string()
            }
            fn analyze(
                &self,
                images: &mut [ExtractedImage],
                _discovered: &[DiscoveredImage],
                _context: &AnalysisContext,
                _diagnostics: &mut ExtractionDiagnostics,
            ) {
                for image in images {
                    image.analyzer_identity = self.identity();
                    image.ocr_regions = vec![ImageOcrRegion {
                        text: "Expert knowledge".to_string(),
                        confidence: 0.87,
                        polygon_within_image: vec![Point { x: 0.0, y: 0.0 }],
                        page_polygon: vec![
                            Point { x: 40.0, y: 130.0 },
                            Point { x: 100.0, y: 130.0 },
                            Point { x: 100.0, y: 150.0 },
                            Point { x: 40.0, y: 150.0 },
                        ],
                        admission: OcrAdmission::Accepted,
                    }];
                }
            }
        }

        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "image.pdf", IMAGE_PDF_BASE64);
        let content = MuPdfBackend::new(Some(Arc::new(Spotter)))
            .extract(&path)
            .expect("extracts");

        let range = content.images[0].reading_range.clone().expect("a range");
        let inserted: Vec<&crate::types::SourceSegment> = content
            .source_map
            .segments
            .iter()
            .filter(|segment| {
                segment.text_range.start >= range.start && segment.text_range.end <= range.end
            })
            .collect();

        // The segments tile the block: nothing inserted is left unattributed.
        let mut cursor = range.start;
        for segment in &inserted {
            assert_eq!(segment.text_range.start, cursor, "a gap in the block");
            cursor = segment.text_range.end;
            assert!(matches!(
                segment.provenance,
                TextProvenance::ImageOcr { .. } | TextProvenance::ImageDescription { .. }
            ));
            assert!(matches!(
                segment.origin,
                SourceOrigin::PdfPage { page: 1, bbox: Some(_) }
            ));
        }
        assert_eq!(cursor, range.end, "the block ends where the segments do");

        let label = content
            .source_map
            .resolve(
                content
                    .text
                    .find("Expert knowledge")
                    .expect("the label is in the reading"),
            )
            .expect("it resolves");
        let SourceOrigin::PdfPage { bbox: Some(bbox), .. } = label else {
            panic!("expected a page locator, got {label:?}");
        };
        // The region's polygon, not the image's box.
        assert!((bbox.x - 40.0).abs() < 0.01 && (bbox.width - 60.0).abs() < 0.01, "{bbox:?}");
    }

    /// The document this feature was specified against, when it is present.
    ///
    /// Gated on an environment variable rather than checked in: the corpus is
    /// a user's library, not a fixture. Run it with
    /// `WILKES_SAMPLE_PDF=<path> cargo test -- --ignored`.
    #[test]
    #[ignore = "needs a local corpus document"]
    fn the_sample_documents_diagram_is_found_as_a_native_image() {
        let Ok(path) = std::env::var("WILKES_SAMPLE_PDF") else {
            return;
        };
        let content = MuPdfBackend::default()
            .extract(std::path::Path::new(&path))
            .expect("extracts");
        eprintln!("{} images found", content.images.len());
        for image in content.images.iter().take(40) {
            eprintln!(
                "  {} page {} {}x{}px box {:.0},{:.0} {:.0}x{:.0} {:?}",
                image.id,
                image.page,
                image.pixel_width,
                image.pixel_height,
                image.bbox.x,
                image.bbox.y,
                image.bbox.width,
                image.bbox.height,
                image.status,
            );
        }
        let expected = content
            .images
            .iter()
            .find(|image| image.page == 18)
            .expect("page 18 draws the expert-system diagram");
        assert_eq!((expected.pixel_width, expected.pixel_height), (1559, 499));
    }

    /// Source-map totality on a real extraction: the segments tile the
    /// reading's words in order, and nothing but whitespace lies between them.
    #[test]
    fn every_word_of_the_reading_resolves_to_a_page_position() {
        let dir = tempdir().unwrap();
        let path = write_pdf(
            dir.path(),
            "coordinates.pdf",
            COORDINATE_OUTLINED_PDF_BASE64,
        );

        let content = MuPdfBackend::default().extract(&path).expect("extracts");
        let mut previous = ByteRange { start: 0, end: 0 };
        for segment in &content.source_map.segments {
            assert!(segment.text_range.start >= previous.end, "segments overlap");
            assert!(segment.text_range.end <= content.text.len());
            assert!(content.text[previous.end..segment.text_range.start]
                .chars()
                .all(char::is_whitespace));
            assert!(content
                .source_map
                .resolve(segment.text_range.start)
                .is_some());
            previous = segment.text_range.clone();
        }
        assert!(content.text[previous.end..]
            .chars()
            .all(char::is_whitespace));
    }
}
