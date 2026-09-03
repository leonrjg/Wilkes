use std::path::Path;
use std::sync::Arc;

use mupdf::text_page::TextBlockType;
use mupdf::{DestinationKind, Document, MetadataName, TextPageFlags};
use tracing::{debug, info, trace, warn};

use crate::extract::image::{
    self, AnalysisContext, DiscoveredImage, ImageAnalyzer, NativeImage, NativeTextOnPage,
};
use crate::types::{
    BoundingBox, DeclaredOutline, ExtractedContent, ExtractedImage, ExtractionDiagnostics,
    FileMetadata, ImageTransform, OutlineAnchor, OutlineEntry, RegionOrigin, SourceMap,
    SourceOrigin,
};

use super::backend::PdfBackend;
use super::sanitize::{self, Block, Line, Page, Reading, Word};
use super::typeset;

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
fn read_document(path: &Path, analyzer: Option<&dyn ImageAnalyzer>) -> anyhow::Result<PdfDocument> {
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

    // Typeset routing costs a render and a recognizer call per region, so it
    // is not done at all when nothing would read the result. Without an
    // analyzer the reading is the page's own glyphs, which is what it has
    // always been.
    // What marks out the formulas and tables a page typesets, or nothing.
    // Asked of the analyzer rather than decided here, and asked once: a
    // configuration with no detector must not pay a page render per page to
    // be told nothing, and there is no second way to answer the question —
    // the font and rule heuristics that used to answer it are gone.
    let detector = analyzer.and_then(|analyzer| analyzer.layout());
    // Asked once per document, before a single pixmap is built. An analyzer
    // that does not read embedded rasters must not be handed a document's
    // worth of decoded artwork to ignore — the decode is the cost this
    // question exists to avoid, and it is paid at discovery, not at analysis.
    //
    // With no analyzer the rasters are still decoded and digested, because
    // that is what a reading with no enrichment has always contained: the
    // images found, counted and identified, and the diagnostics saying the
    // analysis did not run.
    let read_embedded = analyzer.is_none_or(|analyzer| analyzer.reads_embedded_images());
    let mut pages = Vec::with_capacity(page_count as usize);
    let mut discovered: Vec<DiscoveredImage> = Vec::new();
    let mut diagnostics = ExtractionDiagnostics::default();
    let mut pending: Vec<typeset::TypesetRegion> = Vec::new();
    // Every class the detector named, whether or not anything routes it. A
    // class this build reads nothing for is still a fact about the document —
    // "this page holds forty inline formulas we do not read" and "this page
    // holds no mathematics" are different, and only one of them is a gap.
    // Class name to (count, whether anything reads it). The second is taken
    // from the region rather than re-derived from the name: one place decides
    // what a class means, and it is the detector's own mapping.
    let mut detected: std::collections::BTreeMap<&'static str, (u32, bool)> = Default::default();
    // Visibility only: where the time before recognition goes. Two stages,
    // because they are paid for different things — a page render and a
    // detection per page of the document, and a crop render per region the
    // detector marked out.
    let mut text_layer = std::time::Duration::ZERO;
    let mut render_and_detect = std::time::Duration::ZERO;
    let mut crop_render = std::time::Duration::ZERO;
    // What each page's own glyphs said, kept per page so the detector's
    // regions can be reconciled against the words they sit among once the
    // whole document has been detected.
    let mut surveys: Vec<(BoundingBox, typeset::PageSurvey)> =
        Vec::with_capacity(page_count as usize);
    for i in 0..page_count as i32 {
        let page = doc.load_page(i)?;
        let bounds = page.bounds()?;
        let height = bounds.y1 - bounds.y0;
        // ACCURATE_BBOXES produces tighter per-character quads.
        // PRESERVE_IMAGES adds the page's image blocks to the same block list,
        // in the order the page draws them — which is the only discovery
        // signal this phase needs, and the only thing that establishes where
        // an image sits relative to the text around it.
        let words_started = std::time::Instant::now();
        let text_page =
            page.to_text_page(TextPageFlags::ACCURATE_BBOXES | TextPageFlags::PRESERVE_IMAGES)?;
        let mut survey = typeset::PageSurvey {
            words: Vec::new(),
            drawn: Vec::new(),
        };
        pages.push(extract_page_words(
            &text_page,
            (i + 1) as u32,
            height,
            &mut discovered,
            &mut survey,
            read_embedded,
        ));
        text_layer += words_started.elapsed();

        let page_box = BoundingBox {
            x: bounds.x0,
            y: bounds.y0,
            width: bounds.x1 - bounds.x0,
            height,
        };
        surveys.push((page_box, survey));
    }

    // One detection for the whole document, because a detector reached through
    // the worker is *started* by this call: putting it in the page loop above
    // is what let a cancel be outrun, one respawned worker per page.
    //
    // The rendering stays here — it is mupdf's, and mupdf is this process's.
    // What the detector gets is a way to ask for page `n`, pulled one page at
    // a time so a book never has more than one page of pixels in memory: at
    // 1600 square that is eight megabytes a page, and a four-hundred-page book
    // rendered up front would be three gigabytes held to save writing them out
    // one at a time. Only the detection crosses.
    //
    // The cost of pulling is that a page is loaded twice — once above for its
    // glyphs, once here for its pixels. That is a page-dictionary parse, not a
    // content-stream run, and it is the cheaper half of the alternative:
    // holding every page of the document open across both passes. It lands in
    // the `page render + detection` figure the stage log reports below.
    if let Some(detector) = detector {
        let detect_started = std::time::Instant::now();
        let mut renders_failed = 0u32;
        let side = detector.input_side();
        let detected_pages = {
            let mut render = |index: usize| -> anyhow::Result<::image::RgbImage> {
                // A render failure is this page's, not the document's: the
                // reading keeps the page's own glyphs, which is what it would
                // have had anyway, and the log says which page and why.
                // Bailing would throw away a whole book over one page that
                // would not rasterize.
                let rendered = doc
                    .load_page(index as i32)
                    .map_err(anyhow::Error::from)
                    .and_then(|page| typeset::render_page(&page, side));
                if let Err(error) = &rendered {
                    warn!("page render on page {} of {:?}: {error:#}", index + 1, path);
                    renders_failed += 1;
                }
                rendered
            };
            detector.detect_document(page_count as usize, &mut render)
        };
        diagnostics.layout_pages_failed += renders_failed;
        render_and_detect += detect_started.elapsed();

        match detected_pages {
            Ok(per_page) => {
                debug!(
                    "{} page(s) detected in {} ms",
                    per_page.len(),
                    detect_started.elapsed().as_millis(),
                );
                for (index, found) in per_page.into_iter().enumerate() {
                    let (page_box, survey) = &surveys[index];
                    for region in &found {
                        let entry = detected.entry(region.label).or_insert((0, false));
                        entry.0 += 1;
                        entry.1 = region.kind.is_some();
                    }
                    pending.extend(typeset::regions(
                        (index + 1) as u32,
                        page_box,
                        &found,
                        survey,
                    ));
                }
            }
            // One request, one outcome: a detection that failed failed for the
            // document, and the reading falls back to the pages' own glyphs —
            // which is what it would have had with no detector at all.
            Err(error) => {
                warn!("layout detection on {:?}: {error:#}", path);
                // Every page went undetected, not just the ones that would not
                // rasterize. Assigned rather than added to, so the pages
                // already counted as render failures are not counted twice.
                diagnostics.layout_pages_failed = page_count;
            }
        }
    }

    if detector.is_some() {
        report_detections(path, &detected, &mut diagnostics);
    }

    let mut budgeted = typeset::counted(pending, &mut diagnostics);
    // The regions that speak for no word of their page, and where the page's
    // own geometry put each of them. Nothing can be marked for these here —
    // there is no word to mark — so the place travels to
    // `sanitize::supersede_typeset_regions`, which is where a word is made for
    // them if their reading is admitted.
    let mut anchored: Vec<(usize, typeset::Anchor)> = Vec::new();
    // One page loaded per page that has regions, and its regions rendered
    // together: loading a page is cheap and doing it per region would still be
    // an avoidable repetition of the parse.
    budgeted.sort_by_key(|region| region.page);
    let mut at = 0usize;
    while at < budgeted.len() {
        let page_number = budgeted[at].page;
        let end = budgeted[at..]
            .iter()
            .position(|region| region.page != page_number)
            .map_or(budgeted.len(), |offset| at + offset);
        let page = doc.load_page(page_number as i32 - 1)?;
        let crop_started = std::time::Instant::now();
        let placed = typeset::discover(&page, &budgeted[at..end], &mut discovered);
        crop_render += crop_started.elapsed();
        for (region, image_index) in budgeted[at..end].iter().zip(placed) {
            // The words this region speaks for learn which image will speak
            // for them. Whether it actually does is settled after recognition,
            // in `sanitize::supersede_typeset_regions` — as is the whole of
            // the anchored case, which is the same thing with the word made
            // rather than found.
            match region.anchor {
                Some(anchor) => anchored.push((image_index, anchor)),
                None => {
                    for (block, line, word) in &region.words {
                        if let Some(word) = pages[page_number as usize - 1]
                            .blocks
                            .get_mut(*block)
                            .and_then(|block| block.lines.get_mut(*line))
                            .and_then(|line| line.words.get_mut(*word))
                        {
                            word.typeset = Some(image_index);
                        }
                    }
                }
            }
        }
        at = end;
    }

    // Visibility only: the cost of everything that happens before a recognizer
    // is asked anything, so the recognizers' own reported times can be read
    // against the rest of the read.
    info!(
        "stages of {:?}: text layer {:.1}s over {page_count} page(s), \
         page render + detection {:.1}s, crop render {:.1}s over {} region(s)",
        path,
        text_layer.as_secs_f32(),
        render_and_detect.as_secs_f32(),
        crop_render.as_secs_f32(),
        budgeted.len(),
    );

    let context = AnalysisContext::new(native_words(&pages));
    // Said before the work rather than after it. Analysis is the slow part of
    // reading a document — minutes per figure on a CPU — and a reader watching
    // the log needs to know both that it is about to happen and whether an
    // analyzer exists to do it. Silence here is indistinguishable from a
    // runtime that has no analyzer attached, which is a thing that has
    // happened and was invisible for exactly this reason.
    if !discovered.is_empty() {
        let typeset = discovered
            .iter()
            .filter(|found| found.origin == RegionOrigin::Typeset)
            .count();
        match analyzer {
            Some(analyzer) => info!(
                "images in {:?}: analyzing {} ({} embedded, {} typeset) with {}",
                path,
                discovered.len(),
                discovered.len() - typeset,
                typeset,
                analyzer.identity()
            ),
            None => info!(
                "images in {:?}: {} found, no analyzer configured — not enriching",
                path,
                discovered.len()
            ),
        }
    }
    let mut images = image::analyze(&discovered, &context, analyzer, &mut diagnostics);
    // The pixels have done their work. Dropping them here rather than at the
    // end of extraction keeps a document's worth of decoded artwork out of
    // memory while the reading is being built.
    drop(discovered);
    let reading = sanitize::sanitize(pages, &mut images, &anchored, &mut diagnostics);
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

    if diagnostics.typeset_regions_found > 0 {
        info!(
            "typeset regions in {:?}: {} found and read ({} over no glyph of the page at all), \
             {} admitted and now standing in place of the page's own glyphs",
            path,
            diagnostics.typeset_regions_found,
            diagnostics.typeset_regions_anchored,
            diagnostics.typeset_regions_superseded_native_text,
        );
    }

    if diagnostics.native_images_found > 0 {
        info!(
            "images in {:?}: {} found, {} analyzed, {} skipped by a technical limit, \
             {} transcribed, {} recognition failures, {} regions accepted, \
             {} below the threshold, {} already native text, \
             {} formulas ({} invalid LaTeX), {} tables ({} malformed), \
             {} charts ({} malformed), {} with no text to read, \
             {} of no known kind, \
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
            diagnostics.formulas_accepted,
            diagnostics.formulas_rejected_invalid_latex,
            diagnostics.tables_accepted,
            diagnostics.tables_rejected_malformed,
            diagnostics.charts_accepted,
            diagnostics.charts_rejected_malformed,
            diagnostics.regions_marked_not_text,
            diagnostics.regions_unroutable,
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
    surveyed: &mut typeset::PageSurvey,
    read_embedded: bool,
) -> Page {
    let mut blocks = Vec::new();
    for block in text_page.blocks() {
        if block.r#type() == TextBlockType::Image {
            // Where the page draws a picture, whether or not discovery keeps
            // it: an area a picture occupies is an area the prose does not,
            // and that is true of a picture whose pixels could not be read.
            let bounds = block.bounds();
            surveyed.drawn.push(BoundingBox {
                x: bounds.x0,
                y: bounds.y0,
                width: (bounds.x1 - bounds.x0).max(0.0),
                height: (bounds.y1 - bounds.y0).max(0.0),
            });
            if let Some(found) = discover_image(&block, page_num, discovered.len(), read_embedded) {
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
                let Some(c) = ch.char() else { continue };

                if c.is_whitespace() {
                    flush(&mut out, &mut word_chars, &mut bbox);
                    out.push_space(c);
                    continue;
                }

                word_chars.push(c);

                // An axis-aligned rect from the character's bounding quad.
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
                    bbox = Some(match &bbox {
                        Some(existing) => existing.merge(&next),
                        None => next,
                    });
                }
            }

            // End of line: flush any trailing word. The line itself becomes a
            // newline when the reading is rendered.
            flush(&mut out, &mut word_chars, &mut bbox);
            // Addressed by the indices this reading uses, not by MuPDF's: an
            // image block and an empty block both shift them, and a region
            // that marked out the wrong word would silently remove text it
            // never covered.
            for (index, out_word) in out.words.iter().enumerate() {
                if let Some(bbox) = &out_word.bbox {
                    surveyed.words.push(typeset::WordBox {
                        block: blocks.len(),
                        line: lines.len(),
                        word: index,
                        bbox: bbox.clone(),
                    });
                }
            }
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

/// Say what the detector found, and count what nothing reads.
///
/// At info rather than debug, and always when a detector ran, because the
/// silence this replaces was the whole of the last failure: a document whose
/// mathematics went unrecognized read exactly like a document with no
/// mathematics in it. The class names are the evidence and they are only here.
fn report_detections(
    path: &Path,
    detected: &std::collections::BTreeMap<&'static str, (u32, bool)>,
    diagnostics: &mut ExtractionDiagnostics,
) {
    if detected.is_empty() {
        return;
    }
    let (mut routed, mut unrouted) = (Vec::new(), Vec::new());
    for (label, (count, is_routed)) in detected {
        diagnostics.layout_regions_detected += count;
        if *is_routed {
            routed.push(format!("{count} {label}"));
        } else {
            diagnostics.layout_regions_not_routed += count;
            unrouted.push(format!("{count} {label}"));
        }
    }
    info!(
        "layout of {:?}: routed {}; detected and not read {}",
        path,
        if routed.is_empty() {
            "nothing".to_string()
        } else {
            routed.join(", ")
        },
        if unrouted.is_empty() {
            "nothing".to_string()
        } else {
            unrouted.join(", ")
        },
    );
}

/// Read one native image block: where the page put it, and its pixels.
///
/// Everything mechanical. No caption is looked for, no neighbouring text is
/// consulted, and nothing decides whether this is a figure — a repeated logo
/// arrives here exactly as a diagram does, and it is the recognizer finding no
/// text in it that keeps it out of the reading.
///
/// `read_embedded` is the one thing here that is a configuration rather than a
/// property of the page, and it decides exactly one thing: whether the pixels
/// are decoded. The block is still found, still placed in the reading order,
/// and still reported — withholding it from the recognizer is not the same as
/// pretending the page does not draw it.
fn discover_image(
    block: &mupdf::TextBlock<'_>,
    page: u32,
    ordinal: usize,
    read_embedded: bool,
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
        origin: RegionOrigin::Embedded,
        bbox,
        transform,
        decoded: None,
        rejected: None,
        withheld_by_scope: false,
        // Nothing has classified an embedded raster before it is read. What
        // is in it is the recognizer's answer, not a routing input.
        kind: None,
    };

    if !read_embedded {
        found.withheld_by_scope = true;
        return Some(found);
    }

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
        typeset: None,
    });
}

/// The pixels of one embedded image, found again in the document it came from.
///
/// The ids extraction assigns are positional — `p{page}-i{ordinal}`, where the
/// ordinal counts image blocks across the document in the order the pages draw
/// them — so finding one again is the same walk under the same flags, counting
/// the same blocks. It deliberately reuses [`discover_image`] rather than
/// reimplementing the decode: an id resolved by one rule and decoded by
/// another would be a picture nobody could check.
///
/// `None` when the walk ends without that id, which means the document is not
/// the one the rendition was extracted from. The caller has a digest and can
/// say so; this function does not guess.
pub fn decode_embedded_image(path: &Path, area_id: &str) -> anyhow::Result<Option<NativeImage>> {
    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path"))?;
    let doc = Document::open(path_str)?;
    let page_count = doc.page_count()?;

    let mut ordinal = 0usize;
    for index in 0..page_count {
        let page = doc.load_page(index)?;
        // The same flags discovery used. ACCURATE_BBOXES does not change which
        // blocks arrive, but PRESERVE_IMAGES decides whether any image blocks
        // do at all, and the ordinals are a count of them.
        let text_page =
            page.to_text_page(TextPageFlags::ACCURATE_BBOXES | TextPageFlags::PRESERVE_IMAGES)?;
        for block in text_page.blocks() {
            if block.r#type() != TextBlockType::Image {
                continue;
            }
            // Decoded, always. `read_embedded` is a scope setting for a
            // reading — whether a document's rasters are worth a recognizer —
            // and this is not a reading: the caller has named one picture and
            // asked for its pixels. Threading the setting in here would make
            // this return `None` for a picture that is present, which the
            // caller reads as "not this document".
            let Some(found) = discover_image(&block, index as u32 + 1, ordinal, true) else {
                continue;
            };
            ordinal += 1;
            if found.id != area_id {
                continue;
            }
            if let Some(reason) = found.rejected {
                anyhow::bail!("image {area_id} cannot be decoded: {reason}");
            }
            return Ok(found.decoded);
        }
    }
    Ok(None)
}

/// Rasterize one area of one page, for a region the page typeset rather than
/// embedded.
///
/// There is no block to find for these: the picture is the page's own drawing,
/// and the bbox recorded at extraction is the whole address. Rendering it again
/// is what produced the pixels in the first place.
pub fn render_page_area(path: &Path, page: u32, bbox: &BoundingBox) -> anyhow::Result<NativeImage> {
    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path"))?;
    let doc = Document::open(path_str)?;
    anyhow::ensure!(page >= 1, "pages are 1-based; got {page}");
    let loaded = doc.load_page(page as i32 - 1)?;
    let (rendered, _) = typeset::render(&loaded, bbox)?;
    Ok(rendered)
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

    /// One page, three lines of text, the middle one set in `CMMI10` — the
    /// Computer Modern math italic a TeX document sets a display equation in.
    /// Hand-built so the signal under test, which font drew which line, is
    /// visible in the source that asserts about it.
    ///
    /// The equation is drawn the way a typesetter draws one: base glyphs on
    /// the baseline at twelve points, subscripts three points lower at eight.
    /// That structure is what the reading flattens away, and it is what the
    /// routing rule looks for — a fixture without it would not be a display
    /// equation and would rightly not be marked out. It is also long against a
    /// short page, which is what makes the padding rule reachable from here.
    const MATH_PDF_BASE64: &str = "JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4KZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4KZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCA0MDAgMzAwXSAvQ29udGVudHMgNCAwIFIgL1Jlc291cmNlcyA8PCAvRm9udCA8PCAvRjEgNSAwIFIgL0YyIDYgMCBSID4+ID4+ID4+CmVuZG9iago0IDAgb2JqCjw8IC9MZW5ndGggMTAwOSA+PgpzdHJlYW0KQlQgL0YxIDExIFRmIDIwIDI1MCBUZCAod2hpY2ggaXMgb2J0YWluZWQgYnkgYml0d2lzZSBhZGRpdGlvbiBvZiBhIGFuZCBiKSBUaiBFVApCVCAvRjIgMTIgVGYgNDAuMCAyMzAuMCBUZCAoYykgVGogRVQKQlQgL0YyIDggVGYgNDcuMCAyMjcuMCBUZCAoaSkgVGogRVQKQlQgL0YyIDEyIFRmIDU0LjAgMjMwLjAgVGQgKD0pIFRqIEVUCkJUIC9GMiAxMiBUZiA2NS4wIDIzMC4wIFRkIChhKSBUaiBFVApCVCAvRjIgOCBUZiA3Mi4wIDIyNy4wIFRkIChpKSBUaiBFVApCVCAvRjIgMTIgVGYgNzkuMCAyMzAuMCBUZCAoKykgVGogRVQKQlQgL0YyIDEyIFRmIDkwLjAgMjMwLjAgVGQgKGIpIFRqIEVUCkJUIC9GMiA4IFRmIDk3LjAgMjI3LjAgVGQgKGkpIFRqIEVUCkJUIC9GMiAxMiBUZiAxMDQuMCAyMzAuMCBUZCAoKykgVGogRVQKQlQgL0YyIDEyIFRmIDExNS4wIDIzMC4wIFRkIChkKSBUaiBFVApCVCAvRjIgOCBUZiAxMjIuMCAyMjcuMCBUZCAoaSkgVGogRVQKQlQgL0YyIDEyIFRmIDEyOS4wIDIzMC4wIFRkICgrKSBUaiBFVApCVCAvRjIgMTIgVGYgMTQwLjAgMjMwLjAgVGQgKGUpIFRqIEVUCkJUIC9GMiA4IFRmIDE0Ny4wIDIyNy4wIFRkIChpKSBUaiBFVApCVCAvRjIgMTIgVGYgMTU0LjAgMjMwLjAgVGQgKCspIFRqIEVUCkJUIC9GMiAxMiBUZiAxNjUuMCAyMzAuMCBUZCAoZikgVGogRVQKQlQgL0YyIDggVGYgMTcyLjAgMjI3LjAgVGQgKGkpIFRqIEVUCkJUIC9GMiAxMiBUZiAxNzkuMCAyMzAuMCBUZCAoKykgVGogRVQKQlQgL0YyIDEyIFRmIDE5MC4wIDIzMC4wIFRkIChnKSBUaiBFVApCVCAvRjIgOCBUZiAxOTcuMCAyMjcuMCBUZCAoaSkgVGogRVQKQlQgL0YyIDEyIFRmIDIwNC4wIDIzMC4wIFRkICgrKSBUaiBFVApCVCAvRjIgMTIgVGYgMjE1LjAgMjMwLjAgVGQgKGgpIFRqIEVUCkJUIC9GMiA4IFRmIDIyMi4wIDIyNy4wIFRkIChpKSBUaiBFVApCVCAvRjEgMTEgVGYgMjAgMjEwIFRkIChhbmQgdGhlIGRpc2N1c3Npb24gY29udGludWVzIGFmdGVyd2FyZHMpIFRqIEVUCmVuZHN0cmVhbQplbmRvYmoKNSAwIG9iago8PCAvVHlwZSAvRm9udCAvU3VidHlwZSAvVHlwZTEgL0Jhc2VGb250IC9IZWx2ZXRpY2EgPj4KZW5kb2JqCjYgMCBvYmoKPDwgL1R5cGUgL0ZvbnQgL1N1YnR5cGUgL1R5cGUxIC9CYXNlRm9udCAvQ01NSTEwID4+CmVuZG9iagp4cmVmCjAgNwowMDAwMDAwMDAwIDY1NTM1IGYgCjAwMDAwMDAwMDkgMDAwMDAgbiAKMDAwMDAwMDA1OCAwMDAwMCBuIAowMDAwMDAwMTE1IDAwMDAwIG4gCjAwMDAwMDAyNTEgMDAwMDAgbiAKMDAwMDAwMTMxMSAwMDAwMCBuIAowMDAwMDAxMzgxIDAwMDAwIG4gCnRyYWlsZXIKPDwgL1NpemUgNyAvUm9vdCAxIDAgUiA+PgpzdGFydHhyZWYKMTQ0OAolJUVPRgo=";

    /// What `MATH_PDF_BASE64`'s equation reads as when nothing routes it: the
    /// subscripts flattened into the line, their size and offset gone, and the
    /// word spacing left over from where they sat. Not mathematics, and not
    /// something a consumer can parse back into any — which is the whole
    /// reason for the feature these tests cover.
    const NATIVE_EQUATION: &str = "c i = a i + bi + di + e i + f i + gi + hi";

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

        let outline = MuPdfBackend::default()
            .outline(&path)
            .expect("outline reads")
            .entries;
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
        let outline = MuPdfBackend::default()
            .outline(&path)
            .expect("outline reads")
            .entries;

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
        let outline = MuPdfBackend::default()
            .outline(&path)
            .expect("outline reads")
            .entries;

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
        let outline = MuPdfBackend::default()
            .outline(&path)
            .expect("outline reads")
            .entries;

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

        let outline = MuPdfBackend::default()
            .outline(&path)
            .expect("outline reads")
            .entries;
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

    /// The pixels are not stored, so serving a figure means finding it again
    /// in the document it came from. The ids are positional, so "finding it
    /// again" is the same walk under the same flags — and the digest recorded
    /// at extraction is what proves the walk landed on the same picture.
    #[test]
    fn a_retained_figure_re_derives_to_the_pixels_that_were_analyzed() {
        use crate::types::RetainedImage;

        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "image.pdf", IMAGE_PDF_BASE64);
        let content = MuPdfBackend::default().extract(&path).expect("extracts");
        let retained = RetainedImage::of(&content.images[0]);
        assert!(
            !retained.image_sha256.is_empty(),
            "the fixture's image decodes, so it has a digest to check against"
        );

        let rendered = crate::figure::render_figure(&path, &retained, None).expect("re-derives");
        assert_eq!(
            (rendered.source_width, rendered.source_height),
            (retained.pixel_width, retained.pixel_height),
            "the same picture, at the size it was analyzed"
        );
        assert_eq!(&rendered.png[1..4], b"PNG");

        // A digest that does not match is a refusal, not a fallback: it means
        // the source and the rendition have come apart, and the wrong picture
        // is worse than none.
        let tampered = RetainedImage {
            image_sha256: "0".repeat(64),
            ..retained.clone()
        };
        let error = crate::figure::render_figure(&path, &tampered, None)
            .expect_err("a mismatch is refused");
        assert!(
            error.to_string().contains("come apart"),
            "unexpected error: {error}"
        );

        // An id this document does not draw is the same kind of mistake, and
        // is reported rather than answered with a neighbouring picture.
        let absent = RetainedImage {
            id: "p9-i9".to_string(),
            ..retained.clone()
        };
        assert!(crate::figure::render_figure(&path, &absent, None).is_err());
    }

    /// Downscaling happens after the digest is checked, so a caller asking for
    /// a smaller copy still gets one the rendition can vouch for.
    #[test]
    fn a_max_edge_shrinks_the_copy_and_not_the_check() {
        use crate::types::RetainedImage;

        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "image.pdf", IMAGE_PDF_BASE64);
        let content = MuPdfBackend::default().extract(&path).expect("extracts");
        let retained = RetainedImage::of(&content.images[0]);

        let rendered = crate::figure::render_figure(&path, &retained, Some(2)).expect("renders");
        assert_eq!(rendered.width.max(rendered.height), 2);
        assert_eq!(
            (rendered.source_width, rendered.source_height),
            (retained.pixel_width, retained.pixel_height),
            "and still reports what it verified"
        );
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
        let bottom_right =
            image
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

    /// An image nothing was established about still has a place in the
    /// reading. Discovery is the page's geometry, not a recognizer's opinion,
    /// so the anchor is written whether or not a byte was — it is what links
    /// the picture to the passage it was drawn into.
    #[test]
    fn an_unanalyzed_image_is_still_anchored_where_the_page_drew_it() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "image.pdf", IMAGE_PDF_BASE64);

        let content = MuPdfBackend::default().extract(&path).expect("extracts");
        let image = &content.images[0];

        assert!(
            image.reading_range.is_none(),
            "nothing was written for this image"
        );
        let anchor = image
            .reading_anchor
            .expect("but the page still drew it somewhere");
        assert!(
            anchor <= content.text.len() && content.text.is_char_boundary(anchor),
            "anchor {anchor} is not a position in a {}-byte reading",
            content.text.len()
        );

        let above = content
            .text
            .find("Above the picture")
            .expect("the fixture sets text above the image");
        let caption = content
            .text
            .find("Figure 1: a caption")
            .expect("and a caption below it");
        assert!(
            above < anchor && anchor <= caption,
            "the anchor should fall between the text above the picture ({above}) \
             and the caption below it ({caption}), got {anchor}"
        );
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
            fn layout(&self) -> Option<&dyn crate::extract::image::LayoutModel> {
                None
            }
            fn release(&self) {}
            fn reads_embedded_images(&self) -> bool {
                true
            }
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
                        kind: Default::default(),
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
            fn layout(&self) -> Option<&dyn crate::extract::image::LayoutModel> {
                None
            }
            fn release(&self) {}
            fn reads_embedded_images(&self) -> bool {
                true
            }
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
                        kind: Default::default(),
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
                SourceOrigin::PdfPage {
                    page: 1,
                    bbox: Some(_)
                }
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
        let SourceOrigin::PdfPage {
            bbox: Some(bbox), ..
        } = label
        else {
            panic!("expected a page locator, got {label:?}");
        };
        // The region's polygon, not the image's box.
        assert!(
            (bbox.x - 40.0).abs() < 0.01 && (bbox.width - 60.0).abs() < 0.01,
            "{bbox:?}"
        );
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

    // ── Typeset regions ─────────────────────────────────────────────────

    /// The recognizer, standing in for one: it reads every typeset region as
    /// the same LaTeX, and reads nothing in an embedded picture. Enough to
    /// assert what *routing* did, which is what these tests are about.
    struct FormulaReader {
        latex: Option<&'static str>,
        /// What the configured scope would answer. Routing and scope are
        /// independent — one decides which areas are marked out, the other
        /// which of them are decoded — and the tests below hold them apart.
        reads_embedded: bool,
        /// What marks the areas out. `None` is a runtime with no detector
        /// installed, which marks out nothing at all.
        detector: Option<StubLayout>,
    }

    impl FormulaReader {
        fn new(latex: Option<&'static str>) -> Self {
            Self {
                latex,
                reads_embedded: true,
                detector: Some(StubLayout),
            }
        }
    }

    /// A layout detector standing in for one: it marks out a fixed rectangle
    /// of every page, as a fraction of it.
    ///
    /// The whole of what the real detector contributes is *where* to look, and
    /// a test that ran the real one would be testing PP-DocLayoutV2 rather
    /// than this pipeline. The rectangle below is the display equation of
    /// `MATH_PDF_BASE64`, worked out from its content stream: on a 400 x 300
    /// page the base glyphs sit on a baseline at PDF y 230 at twelve points
    /// with the subscripts three points under them at eight, so the equation
    /// occupies roughly y 58..78 measured down from the top, and x 35..235.
    /// The prose lines — baselines at 250 and 210 — are outside it, so a
    /// region that swallowed them fails the tests here rather than passing
    /// them quietly.
    #[derive(Default)]
    struct StubLayout;

    impl StubLayout {
        const LABEL: &'static str = "display_formula";
    }

    impl crate::extract::image::LayoutModel for StubLayout {
        fn input_side(&self) -> u32 {
            800
        }
        fn release(&self) {}
        fn identity(&self) -> String {
            "stub-layout-v1".to_string()
        }

        fn detect_document(
            &self,
            page_count: usize,
            render: &mut dyn FnMut(usize) -> anyhow::Result<::image::RgbImage>,
        ) -> anyhow::Result<Vec<Vec<crate::extract::image::LayoutRegion>>> {
            Ok((0..page_count)
                .map(|index| {
                    // Pulled, so the test exercises the same page-by-page
                    // render the real detectors ask for.
                    let _ = render(index);
                    vec![crate::extract::image::LayoutRegion {
                        label: Self::LABEL,
                        kind: crate::extract::image::doclayout::kind_of(Self::LABEL),
                        score: 0.95,
                        bbox: BoundingBox {
                            x: 35.0 / 400.0,
                            y: 58.0 / 300.0,
                            width: 200.0 / 400.0,
                            height: 20.0 / 300.0,
                        },
                    }]
                })
                .collect())
        }
    }

    impl crate::extract::image::ImageAnalyzer for FormulaReader {
        fn release(&self) {}
        fn layout(&self) -> Option<&dyn crate::extract::image::LayoutModel> {
            self.detector
                .as_ref()
                .map(|detector| detector as &dyn crate::extract::image::LayoutModel)
        }
        fn reads_embedded_images(&self) -> bool {
            self.reads_embedded
        }
        fn identity(&self) -> String {
            "formula-reader-v1".to_string()
        }

        fn analyze(
            &self,
            images: &mut [ExtractedImage],
            _discovered: &[DiscoveredImage],
            _context: &crate::extract::image::AnalysisContext,
            diagnostics: &mut ExtractionDiagnostics,
        ) {
            use crate::types::{ImageOcrRegion, OcrAdmission, Point, RegionKind};
            for image in images {
                diagnostics.native_images_analyzed += 1;
                image.analyzer_identity = self.identity();
                let (Some(latex), RegionOrigin::Typeset) = (self.latex, image.origin) else {
                    continue;
                };
                image.ocr_regions = vec![ImageOcrRegion {
                    kind: RegionKind::Formula,
                    text: latex.to_string(),
                    confidence: 0.95,
                    polygon_within_image: vec![Point { x: 0.0, y: 0.0 }],
                    page_polygon: vec![Point {
                        x: image.bbox.x,
                        y: image.bbox.y,
                    }],
                    admission: OcrAdmission::Accepted,
                }];
            }
        }
    }

    /// The reported failure, end to end: a display line the page set in a
    /// math font is marked out, rendered, read as LaTeX, and the LaTeX takes
    /// the glyph run's place in the canonical reading. One owner for those
    /// bytes — the flattened `ci = ai + bi` is *gone*, not sitting beside its
    /// own transcription.
    #[test]
    fn a_math_font_line_is_read_as_latex_and_replaces_the_glyph_run() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "math.pdf", MATH_PDF_BASE64);
        let backend = MuPdfBackend::new(Some(Arc::new(FormulaReader::new(Some(
            "c_i = a_i \\oplus b_i",
        )))));
        let content = backend.extract(&path).expect("extracts");

        let typeset: Vec<&ExtractedImage> = content
            .images
            .iter()
            .filter(|image| image.origin == RegionOrigin::Typeset)
            .collect();
        assert_eq!(
            typeset.len(),
            1,
            "the display line is one region: {:?}",
            content
                .images
                .iter()
                .map(|i| (&i.id, i.origin))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            typeset[0].id, "p1-v0",
            "typeset ids are told apart from embedded ones"
        );
        assert!(
            typeset[0].pixel_width > 0 && typeset[0].pixel_height > 0,
            "it was rendered"
        );

        assert!(
            content
                .text
                .contains("Page formula: c_i = a_i \\oplus b_i."),
            "the LaTeX is in the reading under a label that does not say embedded: {:?}",
            content.text
        );
        assert!(
            !content.text.contains(NATIVE_EQUATION),
            "the glyph run the page drew has left the reading: {:?}",
            content.text
        );
        // The subscripts left with the line they were on. Two claims on one
        // glyph run is what supersession exists to prevent, and a reading
        // surface offered both would have to choose between them.
        let areas = crate::extract::image::serialize::superseded_areas(
            &content.text,
            &crate::extract::image::serialize::reading_regions(&content),
        );
        assert_eq!(areas.len(), 1, "one claim on the area, not nine: {areas:?}");
        assert_eq!(areas[0].text, "c_i = a_i \\oplus b_i");
        assert!(
            content
                .text
                .contains("which is obtained by bitwise addition")
                && content
                    .text
                    .contains("and the discussion continues afterwards"),
            "the prose around it is untouched: {:?}",
            content.text
        );
    }

    /// The rendered crop shows the region and nothing else.
    ///
    /// A display formula is a sliver, and it is padded out so a recognizer's
    /// resize does not squash every glyph in it. If that pad were more of the
    /// page rather than paper, the crop would carry the paragraphs above and
    /// below — and the recognizer would read prose the reading already has,
    /// which would then be written into it a second time. So: ink only where
    /// the region is.
    #[test]
    fn the_render_pads_a_sliver_with_paper_and_not_with_the_rest_of_the_page() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct Capture {
            seen: Mutex<Vec<(u32, u32, Vec<u32>)>>,
            detector: StubLayout,
        }

        impl crate::extract::image::ImageAnalyzer for Capture {
            fn release(&self) {}
            fn layout(&self) -> Option<&dyn crate::extract::image::LayoutModel> {
                Some(&self.detector)
            }
            fn reads_embedded_images(&self) -> bool {
                true
            }
            fn identity(&self) -> String {
                "capture-v1".to_string()
            }
            fn analyze(
                &self,
                _images: &mut [ExtractedImage],
                discovered: &[DiscoveredImage],
                _context: &crate::extract::image::AnalysisContext,
                _diagnostics: &mut ExtractionDiagnostics,
            ) {
                for found in discovered {
                    let Some(decoded) = &found.decoded else {
                        continue;
                    };
                    let pixels = &decoded.pixels;
                    // Rows with any ink in them.
                    let inked = (0..pixels.height())
                        .filter(|y| {
                            (0..pixels.width())
                                .any(|x| pixels.get_pixel(x, *y).0.iter().any(|c| *c < 200))
                        })
                        .collect();
                    self.seen.lock().expect("not poisoned").push((
                        pixels.width(),
                        pixels.height(),
                        inked,
                    ));
                }
            }
        }

        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "math.pdf", MATH_PDF_BASE64);
        let capture = Arc::new(Capture::default());
        MuPdfBackend::new(Some(capture.clone()))
            .extract(&path)
            .expect("extracts");

        let seen = capture.seen.lock().expect("not poisoned");
        assert_eq!(seen.len(), 1, "one region was rendered");
        let (width, height, inked) = &seen[0];
        assert!(
            (*width as f32 / *height as f32) <= 4.5,
            "the sliver was padded: {width}x{height}"
        );

        // The padding exists to fit a recognizer's tiler, and the tiler is the
        // only thing that can say whether it did. A canvas a hair under the
        // bound is rounded up to a second row of tiles — the same picture for
        // nearly twice the prefill — and no assertion inside the renderer can
        // see that, because the rounding happens on the other side of the
        // boundary. This is the one place the two meet.
        #[cfg(feature = "recognize-onnx")]
        {
            let (_, _, cols, rows) =
                crate::extract::image::granite_docling::tile_grid(*width, *height);
            assert_eq!(
                rows, 1,
                "a padded sliver costs one row of tiles, not two: {width}x{height} \
                 tiled {cols}x{rows}"
            );
        }
        assert!(!inked.is_empty(), "the equation is in the crop");

        // Every inked row is in the middle band. The pad above and below is
        // paper, and the prose lines the page draws there are not in it.
        let (first, last) = (inked[0], inked[inked.len() - 1]);
        assert!(
            first > 0 && last < height - 1,
            "ink reaches the edge of the pad: rows {first}..={last} of {height}"
        );
        let band = last - first + 1;
        assert!(
            band < height / 2,
            "the ink is one line, not the page: {band} of {height} rows"
        );
    }

    /// The default scope withholds the embedded rasters and keeps the typeset
    /// routing, which is the whole point of separating them: the formula the
    /// page draws is still marked out, rendered and read, because it is not a
    /// picture the document embedded — it is the only account of that area
    /// there is.
    #[test]
    fn withholding_embedded_rasters_leaves_typeset_routing_running() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "math.pdf", MATH_PDF_BASE64);
        let backend = MuPdfBackend::new(Some(Arc::new(FormulaReader {
            latex: Some("c_i = a_i \\oplus b_i"),
            reads_embedded: false,
            detector: Some(StubLayout),
        })));
        let content = backend.extract(&path).expect("extracts");

        assert!(
            content
                .images
                .iter()
                .any(|image| image.origin == RegionOrigin::Typeset),
            "the display line is still marked out: {:?}",
            content
                .images
                .iter()
                .map(|i| (&i.id, i.origin))
                .collect::<Vec<_>>()
        );
        assert!(
            content
                .text
                .contains("Page formula: c_i = a_i \\oplus b_i."),
            "and still read: {:?}",
            content.text
        );
        assert!(
            !content.text.contains(NATIVE_EQUATION),
            "and still replaces the glyph run: {:?}",
            content.text
        );
    }

    /// A region the recognizer had no admissible answer for keeps the page's
    /// own glyphs. That is what makes a wrongly marked-out region cost time
    /// and no bytes.
    #[test]
    fn a_region_with_no_admitted_reading_leaves_the_page_s_glyphs_alone() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "math.pdf", MATH_PDF_BASE64);
        let backend = MuPdfBackend::new(Some(Arc::new(FormulaReader::new(None))));
        let content = backend.extract(&path).expect("extracts");

        assert!(
            content.text.contains(NATIVE_EQUATION),
            "the glyph run is still the reading: {:?}",
            content.text
        );
        assert!(!content.text.contains("Page formula:"));
    }

    /// Without an analyzer nothing is surveyed, nothing is rendered, and the
    /// reading is the document's own text — the behaviour every reading
    /// produced before typeset routing existed had.
    #[test]
    fn without_an_analyzer_no_region_is_marked_out_at_all() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "math.pdf", MATH_PDF_BASE64);
        let content = MuPdfBackend::default().extract(&path).expect("extracts");
        assert!(content.text.contains(NATIVE_EQUATION));
        assert!(content.images.is_empty());

        let diagnostics = MuPdfBackend::default()
            .outline(&path)
            .expect("outline reads")
            .diagnostics;
        assert_eq!(diagnostics.typeset_regions_found, 0);
    }

    /// The counters separate "found" from "took the page's place": a region
    /// that was marked out and refused is not one that changed the reading.
    #[test]
    fn the_diagnostics_separate_regions_found_from_regions_that_superseded() {
        let dir = tempdir().unwrap();
        let path = write_pdf(dir.path(), "math.pdf", MATH_PDF_BASE64);

        let read = |latex| {
            MuPdfBackend::new(Some(Arc::new(FormulaReader::new(latex))))
                .outline(&path)
                .expect("outline reads")
                .diagnostics
        };

        let admitted = read(Some("c_i = a_i \\oplus b_i"));
        assert_eq!(admitted.typeset_regions_found, 1);
        assert_eq!(admitted.typeset_regions_superseded_native_text, 1);
        // A typeset region is not one of the document's own images.
        assert_eq!(admitted.native_images_found, 0);

        let refused = read(None);
        assert_eq!(refused.typeset_regions_found, 1);
        assert_eq!(refused.typeset_regions_superseded_native_text, 0);
    }
}
