//! How image enrichment is written into the canonical reading.
//!
//! One owner for the labels, the separators and the order, because the bytes
//! this module produces are part of the extraction recipe: change them and
//! every managed document's rendition hash changes, so they cannot be
//! reinvented at three call sites.
//!
//! The labels are not decoration. `Image embedded text:` marks a
//! transcription — bytes a reader may quote as words that appear in the
//! document — and `Image description:` marks a generated claim about what the
//! picture shows, which nobody may quote as the author's. Exact search shows
//! the distinction, exports carry it without private metadata, and a language
//! model reading the export is told which is which in the text itself rather
//! than being trusted to guess.

use std::collections::HashMap;

use tracing::warn;

use crate::types::{
    BoundingBox, ExtractedContent, ExtractedImage, ImageOcrRegion, Point, ReadingRegion,
    RegionKind, RegionOrigin, SourceOrigin, SupersededArea, TextProvenance,
};

/// Bumped whenever these bytes change for the same analysis — a new label, a
/// new separator, a new order. Part of the extraction recipe.
///
/// v2: one label per recognized kind. A reading written under v1 labelled a
/// transcribed table `Image embedded text:` — the same bytes under a claim
/// that is no longer made, which is exactly what a recipe version exists to
/// keep apart.
///
/// v3: one label per kind *and origin*. A formula the page typeset and Wilkes
/// re-read is not embedded in anything, and the bytes that stand in place of
/// its glyph run may not say it was.
pub const SERIALIZATION_VERSION: &str = "image-enrichment-v3";

/// The label prose is transcribed under. The other kinds' labels are on
/// [`RegionKind::label`], where the exhaustive match keeps every kind from
/// reaching the reading unlabelled; this name is kept because callers outside
/// this module ask for the transcription label by it.
pub const OCR_LABEL: &str = RegionKind::Text.label(RegionOrigin::Embedded);
pub const DESCRIPTION_LABEL: &str = "Image description:";

/// Between two spotted regions. A separator rather than a newline: the regions
/// of one figure are one list, and a reader scanning the reading should see
/// them as one.
const REGION_SEPARATOR: &str = "; ";

/// One run of the enrichment block, and what it is.
///
/// The pieces of a block concatenate to the block, with no bytes in between,
/// so the renderer can write them one after another and give each its own
/// source segment. That is what makes "every inserted byte has a page locator
/// and truthful provenance" a property of the code rather than an aspiration:
/// there is nowhere for an uncovered byte to hide.
#[derive(Clone, Debug)]
pub struct Piece {
    pub text: String,
    /// The page region these bytes describe. A transcribed region names its
    /// own polygon's hull; the labels and separators name the whole image,
    /// because that is the truthful answer for a byte Wilkes wrote itself.
    pub bbox: BoundingBox,
    pub provenance: TextProvenance,
}

/// The axis-aligned hull of a page polygon.
///
/// The polygon is the precise locator and lives on the region; this is what
/// the source map can carry, since a [`crate::types::SourceOrigin`] holds a
/// rectangle. Reducing to the hull loses tightness on a rotated label and
/// nothing else — the hull still contains the text and nothing but the region
/// around it.
pub fn polygon_hull(polygon: &[Point], fallback: &BoundingBox) -> BoundingBox {
    let mut points = polygon.iter();
    let Some(first) = points.next() else {
        return fallback.clone();
    };
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
    for point in points {
        x0 = x0.min(point.x);
        y0 = y0.min(point.y);
        x1 = x1.max(point.x);
        y1 = y1.max(point.y);
    }
    BoundingBox {
        x: x0,
        y: y0,
        width: (x1 - x0).max(0.0),
        height: (y1 - y0).max(0.0),
    }
}

/// One labelled run of the reading: the regions it covers, all of one kind.
struct Block<'a> {
    kind: RegionKind,
    regions: Vec<&'a ImageOcrRegion>,
}

/// Whether this kind's body begins its own line rather than following its
/// label.
fn starts_a_line(kind: RegionKind) -> bool {
    matches!(
        kind,
        RegionKind::Table | RegionKind::Chart | RegionKind::Code
    )
}

/// Whether an image's enrichment block stands apart from the prose around it,
/// so a passage boundary belongs on either side of it.
///
/// Two questions the reading used to answer with one flag: *does this open a
/// line of its own*, and *is this a unit no passage may be cut across*. They
/// agree for a picture the document embeds — its label, transcription and
/// description are Wilkes' account of something drawn beside the text, and the
/// prose on either side is not the sentence it interrupts.
///
/// They disagree for an expression the page typeset. It opens a line because
/// that is how the page set it, but its reading stands *in place of* the
/// document's own glyphs ([`RegionKind::supersedes_native_glyphs`]): a display
/// formula is a constituent of the sentence that introduces it, not a block
/// beside it. A seam there strands the clause that introduces it — "…as the
/// bit sequence", "expressed as a formula:", "while Bob calculates" — in a
/// passage of its own, and takes the overlap window with it, so neither half
/// can reach the other. Measured on the corpus that prompted this: one
/// definition and its three displayed formulas came out as six passages
/// totalling 659 bytes under a 600-character window, and the formulas were
/// then cited by nothing.
///
/// `starts_a_line` asks the same question of a kind, and a table, a chart or a
/// code listing is line-structured whatever set it — those keep the boundary a
/// formula loses. Answering it in two places is how the two questions came
/// apart in the first place, so this is the one owner and the chunker's
/// `structural_runs` is its one reader.
pub fn is_structural_block(image: &ExtractedImage) -> bool {
    image.origin != RegionOrigin::Typeset
        || image
            .accepted_ocr()
            .any(|region| starts_a_line(region.kind))
}

/// Group accepted regions into labelled blocks, in the order the recognizer
/// emitted them.
///
/// Consecutive prose regions are one list, as they have always been. Every
/// other kind is a block of its own: two LaTeX expressions joined by `; ` are
/// not a longer expression, and two Markdown tables run together are not a
/// bigger table. Emission order is never disturbed to gather same-kind
/// regions — that would be a layout decision, and reordering the reading is
/// not this module's to make.
fn blocks<'a>(accepted: &[&'a ImageOcrRegion]) -> Vec<Block<'a>> {
    let mut blocks: Vec<Block<'a>> = Vec::new();
    for region in accepted {
        let joinable = region.kind == RegionKind::Text;
        match blocks.last_mut() {
            Some(last) if joinable && last.kind == region.kind => last.regions.push(region),
            _ => blocks.push(Block {
                kind: region.kind,
                regions: vec![region],
            }),
        }
    }
    blocks
}

/// The enrichment block for one image, or an empty list when the image
/// contributed nothing to the reading.
///
/// Nothing is written for an image whose recognizer accepted no text and
/// whose description is absent — which is the ordinary outcome for a
/// repeated logo, and the reason this phase can enumerate images
/// mechanically without a figure classifier. That the image was *looked at*
/// is recorded in diagnostics, where a reader can tell "no text in it" from
/// "never analyzed"; the reading itself stays as the author wrote it.
pub fn enrichment_pieces(image: &ExtractedImage) -> Vec<Piece> {
    let accepted: Vec<&ImageOcrRegion> = image.accepted_ocr().collect();
    let description = image
        .description
        .as_ref()
        .filter(|description| !description.description.trim().is_empty());
    if accepted.is_empty() && description.is_none() {
        return Vec::new();
    }

    let mut pieces = Vec::new();
    let whole = |text: &str, provenance: TextProvenance| Piece {
        text: text.to_string(),
        bbox: image.bbox.clone(),
        provenance,
    };
    let structural_ocr = |kind: RegionKind| TextProvenance::ImageOcr {
        image_id: image.id.clone(),
        confidence: None,
        kind,
    };

    for (index, block) in blocks(&accepted).into_iter().enumerate() {
        if index > 0 {
            pieces.push(whole("\n", structural_ocr(block.kind)));
        }
        let kind = block.kind;
        // A table and a code listing are line-structured — a Markdown table
        // that does not start at the beginning of a line is not a Markdown
        // table — so their label ends its own line. A phrase or an inline
        // formula follows its label.
        pieces.push(whole(
            &format!(
                "{}{}",
                kind.label(image.origin),
                if starts_a_line(kind) { "\n" } else { " " }
            ),
            structural_ocr(kind),
        ));
        for (index, region) in block.regions.iter().enumerate() {
            if index > 0 {
                pieces.push(whole(REGION_SEPARATOR, structural_ocr(kind)));
            }
            pieces.push(Piece {
                text: region.text.clone(),
                bbox: polygon_hull(&region.page_polygon, &image.bbox),
                provenance: TextProvenance::ImageOcr {
                    image_id: image.id.clone(),
                    confidence: Some(region.confidence),
                    kind,
                },
            });
        }
        // A terminator, so the block reads as a sentence to an embedder and a
        // language model rather than running into whatever follows. Omitted
        // when the last region already ends in one, which is the common case
        // for a transcribed caption and is always the case for a table.
        let ends_closed = starts_a_line(kind)
            || block
                .regions
                .last()
                .and_then(|region| region.text.chars().next_back())
                .is_some_and(|c| matches!(c, '.' | '!' | '?' | ':' | ';'));
        pieces.push(whole(
            if ends_closed { "\n" } else { ".\n" },
            structural_ocr(kind),
        ));
    }

    if let Some(description) = description {
        let provenance = TextProvenance::ImageDescription {
            image_id: image.id.clone(),
            analyzer_id: image.analyzer_identity.clone(),
        };
        if !accepted.is_empty() {
            pieces.push(whole("\n", provenance.clone()));
        }
        pieces.push(whole(&format!("{DESCRIPTION_LABEL} "), provenance.clone()));
        pieces.push(whole(description.description.trim(), provenance.clone()));
        pieces.push(whole("\n", provenance));
    }

    pieces
}

/// What an area the page typeset *inside a line* contributes to the reading.
///
/// The region's own text and nothing around it. No label, because `Page
/// formula:` in the middle of a sentence is Wilkes talking over the document;
/// no terminator, because the sentence has one; no newline, because the
/// expression is in the line rather than beside it. What is left is exactly
/// the bytes that stand in place of the words the page drew there, which is
/// also what makes them resolvable — every piece here carries a confidence, so
/// [`reading_regions`] answers for them without a second rule.
///
/// Several accepted regions on one area are joined with a space: they were one
/// expression to the page, and a newline would put a line break inside a
/// sentence.
pub fn inline_pieces(image: &ExtractedImage) -> Vec<Piece> {
    let accepted: Vec<&ImageOcrRegion> = image.accepted_ocr().collect();
    let mut pieces = Vec::new();
    for (index, region) in accepted.iter().enumerate() {
        if index > 0 {
            pieces.push(Piece {
                text: " ".to_string(),
                bbox: image.bbox.clone(),
                provenance: TextProvenance::ImageOcr {
                    image_id: image.id.clone(),
                    confidence: None,
                    kind: region.kind,
                },
            });
        }
        pieces.push(Piece {
            text: region.text.clone(),
            bbox: polygon_hull(&region.page_polygon, &image.bbox),
            provenance: TextProvenance::ImageOcr {
                image_id: image.id.clone(),
                confidence: Some(region.confidence),
                kind: region.kind,
            },
        });
    }
    pieces
}

/// The stretches of a reading that stand in place of a page's own glyph run.
///
/// The inverse of [`enrichment_pieces`], read back off the finished reading
/// rather than recomputed: the source map is where the reading records what
/// each of its bytes is, and a second traversal of the images would be a
/// second opinion about bytes that are already written.
///
/// Two conditions, and no more:
///
/// - The bytes are a *region's*, not the block's framing. A piece with no
///   confidence is a label or a separator — [`TextProvenance::ImageOcr`] says
///   so — and `Page formula:` is Wilkes' word about the area, not the
///   document's own.
/// - The area was one the page typeset. An embedded picture's transcription
///   supersedes nothing: the page draws pixels there, the reading adds an
///   account of them, and both stand.
///
/// Nothing here re-tests [`RegionKind::supersedes_native_glyphs`]. Admission
/// already refuses any typeset region whose kind is not worth displacing a
/// glyph run for, so a typeset region that reached the reading is one that
/// displaced one; asking again would be this module's own copy of a rule that
/// has an owner.
pub fn reading_regions(content: &ExtractedContent) -> Vec<ReadingRegion> {
    let typeset: HashMap<&str, &ExtractedImage> = content
        .images
        .iter()
        .filter(|image| image.origin == RegionOrigin::Typeset)
        .map(|image| (image.id.as_str(), image))
        .collect();

    content
        .source_map
        .segments
        .iter()
        .filter_map(|segment| {
            let SourceOrigin::PdfPage { page, .. } = &segment.origin else {
                return None;
            };
            let (area_id, bbox) = match &segment.provenance {
                TextProvenance::ImageOcr {
                    image_id,
                    confidence: Some(_),
                    ..
                } => {
                    let image = typeset.get(image_id.as_str())?;
                    (image.id.clone(), image.bbox.clone())
                }
                _ => return None,
            };
            Some(ReadingRegion {
                area_id,
                page: *page,
                bbox,
                text_range: segment.text_range.clone(),
            })
        })
        .collect()
}

/// Resolve stored regions against the reading they were cut from.
///
/// The pieces of one area are one thing to a reader — two formulas the page
/// set as one displayed block, or a label and its rows — so they are joined,
/// with the newline the reading itself puts between two blocks of the same
/// area. Consecutive pieces share an area because they were written in
/// reading order and are read back in it.
///
/// A range that does not resolve is dropped and said so. It means the stored
/// text and the stored positions have come apart, which is a fact worth a log
/// line: silently serving the neighbouring bytes would put an arbitrary
/// fragment of the document on a reader's clipboard.
pub fn superseded_areas(full_text: &str, regions: &[ReadingRegion]) -> Vec<SupersededArea> {
    let mut areas: Vec<(&str, SupersededArea)> = Vec::new();

    for (index, region) in regions.iter().enumerate() {
        let Some(text) = full_text.get(region.text_range.start..region.text_range.end) else {
            warn!(
                "reading region {index} of area {} does not resolve against the stored text \
                 ({}..{} of {} bytes)",
                region.area_id,
                region.text_range.start,
                region.text_range.end,
                full_text.len()
            );
            continue;
        };
        match areas.last_mut() {
            Some((area_id, area)) if *area_id == region.area_id => {
                area.text.push('\n');
                area.text.push_str(text);
            }
            _ => areas.push((
                region.area_id.as_str(),
                SupersededArea {
                    page: region.page,
                    bbox: region.bbox.clone(),
                    text: text.to_string(),
                },
            )),
        }
    }

    areas
        .into_iter()
        .map(|(_, area)| area)
        .filter(|area| !area.text.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ImageAnalysisStatus, ImageDescription, ImageTransform, OcrAdmission};

    fn image(regions: Vec<ImageOcrRegion>, description: Option<&str>) -> ExtractedImage {
        ExtractedImage {
            id: "p18-i0".into(),
            page: 18,
            origin: RegionOrigin::Embedded,
            bbox: BoundingBox {
                x: 10.0,
                y: 20.0,
                width: 300.0,
                height: 100.0,
            },
            transform: ImageTransform {
                a: 300.0,
                b: 0.0,
                c: 0.0,
                d: 100.0,
                e: 10.0,
                f: 20.0,
            },
            pixel_width: 1559,
            pixel_height: 499,
            image_sha256: "digest".into(),
            reading_range: None,
            reading_block: None,
            reading_anchor: None,
            ocr_regions: regions,
            description: description.map(|description| ImageDescription {
                description: description.to_string(),
            }),
            analyzer_identity: "analyzer-v1".into(),
            status: ImageAnalysisStatus::Complete,
        }
    }

    fn region(text: &str, confidence: f32, admission: OcrAdmission) -> ImageOcrRegion {
        kinded(RegionKind::Text, text, confidence, admission)
    }

    fn kinded(
        kind: RegionKind,
        text: &str,
        confidence: f32,
        admission: OcrAdmission,
    ) -> ImageOcrRegion {
        ImageOcrRegion {
            kind,
            text: text.to_string(),
            confidence,
            polygon_within_image: Vec::new(),
            page_polygon: vec![
                Point { x: 40.0, y: 30.0 },
                Point { x: 90.0, y: 30.0 },
                Point { x: 90.0, y: 50.0 },
                Point { x: 40.0, y: 50.0 },
            ],
            admission,
        }
    }

    fn rendered(pieces: &[Piece]) -> String {
        pieces.iter().map(|piece| piece.text.as_str()).collect()
    }

    /// A reading with one image's enrichment written into it, exactly as
    /// `sanitize::render` writes it: one segment per piece, in order.
    fn reading(images: Vec<ExtractedImage>) -> ExtractedContent {
        let mut text = String::new();
        let mut segments = Vec::new();
        for image in &images {
            for piece in enrichment_pieces(image) {
                let start = text.len();
                text.push_str(&piece.text);
                segments.push(crate::types::SourceSegment {
                    text_range: crate::types::ByteRange {
                        start,
                        end: text.len(),
                    },
                    origin: SourceOrigin::PdfPage {
                        page: image.page,
                        bbox: Some(piece.bbox.clone()),
                    },
                    provenance: piece.provenance.clone(),
                });
            }
        }
        ExtractedContent {
            text,
            source_map: crate::types::SourceMap { segments },
            metadata: crate::types::FileMetadata {
                path: "doc.pdf".into(),
                size_bytes: 0,
                mime: None,
                title: None,
                page_count: None,
            },
            images,
        }
    }

    fn typeset(kind: RegionKind, text: &str) -> ExtractedImage {
        let mut image = image(vec![kinded(kind, text, 0.95, OcrAdmission::Accepted)], None);
        image.origin = RegionOrigin::Typeset;
        image
    }

    /// The area the reading speaks for is the one whose glyph run it replaced
    /// -- the region as it was marked out -- and the bytes are the region's
    /// own, without the label standing in front of them.
    #[test]
    fn a_typeset_region_is_the_area_and_its_own_bytes() {
        let content = reading(vec![typeset(
            RegionKind::Formula,
            "y_{B} = w^{x_{B}} \\bmod q",
        )]);
        let regions = reading_regions(&content);

        assert_eq!(regions.len(), 1, "{regions:?}");
        assert_eq!(regions[0].area_id, "p18-i0");
        assert_eq!(regions[0].page, 18);
        assert_eq!(regions[0].bbox, content.images[0].bbox);

        let areas = superseded_areas(&content.text, &regions);
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].text, "y_{B} = w^{x_{B}} \\bmod q");
        assert!(content.text.contains("Page formula:"), "{}", content.text);
        assert!(!areas[0].text.contains("Page formula:"));
    }

    /// A picture's transcription supersedes nothing: the page draws pixels
    /// there, and the reading adds an account of them beside what the page
    /// still draws.
    #[test]
    fn an_embedded_image_speaks_for_no_area() {
        let content = reading(vec![image(
            vec![kinded(
                RegionKind::Formula,
                "E = mc^2",
                0.95,
                OcrAdmission::Accepted,
            )],
            Some("A blackboard."),
        )]);

        assert!(reading_regions(&content).is_empty());
    }

    /// The framing Wilkes writes around a transcription -- its label, the
    /// separators, the terminator -- belongs to the block and to no region of
    /// the page, and `confidence: None` is how the reading says so.
    #[test]
    fn the_blocks_framing_is_not_part_of_any_area() {
        let content = reading(vec![typeset(RegionKind::Table, "| a | b |\n| - | - |")]);
        let regions = reading_regions(&content);

        for region in &regions {
            let bytes = &content.text[region.text_range.start..region.text_range.end];
            assert!(!bytes.contains("Page table:"), "{bytes:?}");
            assert!(!bytes.trim().is_empty());
        }
        assert_eq!(
            superseded_areas(&content.text, &regions)[0].text,
            "| a | b |\n| - | - |"
        );
    }

    /// Two regions of one area are one thing to a reader, and are joined the
    /// way the reading separates the blocks they were written as.
    #[test]
    fn the_pieces_of_one_area_are_read_back_as_one() {
        let mut image = typeset(RegionKind::Formula, "x = 1");
        image.ocr_regions.push(kinded(
            RegionKind::Formula,
            "y = 2",
            0.9,
            OcrAdmission::Accepted,
        ));
        let content = reading(vec![image]);

        let areas = superseded_areas(&content.text, &reading_regions(&content));
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].text, "x = 1\ny = 2");
    }

    /// Stored positions and stored text can only come apart through damage,
    /// and serving the neighbouring bytes would put an arbitrary fragment of
    /// the document on a reader's clipboard.
    #[test]
    fn a_range_that_does_not_resolve_is_dropped() {
        let content = reading(vec![typeset(RegionKind::Formula, "x = 1")]);
        let mut regions = reading_regions(&content);
        regions[0].text_range.end = content.text.len() + 100;

        assert!(superseded_areas(&content.text, &regions).is_empty());
    }

    #[test]
    fn an_image_with_nothing_to_say_writes_nothing() {
        assert!(enrichment_pieces(&image(Vec::new(), None)).is_empty());
        assert!(enrichment_pieces(&image(
            vec![region("noise", 0.1, OcrAdmission::RejectedLowConfidence)],
            None
        ))
        .is_empty());
    }

    #[test]
    fn transcription_and_description_are_labelled_separately() {
        let pieces = enrichment_pieces(&image(
            vec![
                region("Non-expert", 0.9, OcrAdmission::Accepted),
                region("Knowledge base", 0.8, OcrAdmission::Accepted),
            ],
            Some("A non-expert consults a knowledge base."),
        ));
        assert_eq!(
            rendered(&pieces),
            "Image embedded text: Non-expert; Knowledge base.\n\
             \n\
             Image description: A non-expert consults a knowledge base.\n"
        );
    }

    /// The property the renderer relies on: the pieces tile the block, so
    /// every byte of it gets a segment.
    #[test]
    fn every_piece_carries_provenance_and_a_region() {
        let pieces = enrichment_pieces(&image(
            vec![region("Knowledge base", 0.8, OcrAdmission::Accepted)],
            Some("A diagram."),
        ));
        assert!(pieces.iter().all(|piece| !piece.text.is_empty()));
        assert!(pieces
            .iter()
            .all(|piece| piece.provenance != TextProvenance::Native));
        let transcribed = pieces
            .iter()
            .find(|piece| piece.text == "Knowledge base")
            .expect("the transcribed region is its own piece");
        // The region's own polygon, not the whole image.
        assert_eq!(transcribed.bbox.x, 40.0);
        assert_eq!(transcribed.bbox.width, 50.0);
    }

    /// Every kind reaches the reading under its own label, in the order the
    /// recognizer emitted them, and a kind that is not prose is a block of
    /// its own rather than an item in a list.
    #[test]
    fn each_kind_is_written_under_its_own_label() {
        let table = "| a | b |\n| --- | --- |\n| 1 | 2 |";
        let pieces = enrichment_pieces(&image(
            vec![
                kinded(RegionKind::Text, "Figure 3", 0.9, OcrAdmission::Accepted),
                kinded(
                    RegionKind::Formula,
                    "E = mc^{2}",
                    0.9,
                    OcrAdmission::Accepted,
                ),
                kinded(RegionKind::Table, table, 0.9, OcrAdmission::Accepted),
                kinded(RegionKind::Chart, table, 0.9, OcrAdmission::Accepted),
                kinded(RegionKind::Code, "let x = 1;", 0.9, OcrAdmission::Accepted),
            ],
            None,
        ));
        assert_eq!(
            rendered(&pieces),
            "Image embedded text: Figure 3.\n\
             \n\
             Image embedded formula: E = mc^{2}.\n\
             \n\
             Image embedded table:\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\
             \n\
             Image transcribed chart:\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\
             \n\
             Image embedded code:\nlet x = 1;\n"
        );
    }

    /// A chart is Wilkes' reconstruction of what a picture depicts, not a
    /// quotation of ruled cells, and the label is the only place a consumer
    /// learns that. No other kind may claim the same words.
    #[test]
    fn a_chart_is_labelled_a_transcription_and_a_table_is_not() {
        let embedded = RegionOrigin::Embedded;
        assert_eq!(
            RegionKind::Chart.label(embedded),
            "Image transcribed chart:"
        );
        assert!(!RegionKind::Chart.label(embedded).contains("embedded"));
        assert!(RegionKind::Table.label(embedded).contains("embedded"));
    }

    /// A formula the page typeset is not embedded in anything: the bytes
    /// stand in place of the glyph run the page drew, and the label may not
    /// claim the document carries a picture of it.
    #[test]
    fn a_typeset_region_is_never_labelled_embedded() {
        for kind in RegionKind::ALL {
            let label = kind.label(RegionOrigin::Typeset);
            assert!(!label.contains("embedded"), "{label}");
            assert!(!label.contains("Image"), "{label}");
            assert_ne!(label, kind.label(RegionOrigin::Embedded));
        }
    }

    /// The mechanism behind "a fifth kind cannot appear without a label": the
    /// match in `label` is exhaustive, so a new kind does not compile until it
    /// has one, and no two kinds share.
    #[test]
    fn every_kind_has_a_distinct_label() {
        let labels: Vec<&str> = [RegionOrigin::Embedded, RegionOrigin::Typeset]
            .into_iter()
            .flat_map(|origin| RegionKind::ALL.iter().map(move |kind| kind.label(origin)))
            .collect();
        assert_eq!(labels.len(), RegionKind::ALL.len() * 2);
        for (index, label) in labels.iter().enumerate() {
            assert!(label.ends_with(':'), "{label}");
            assert!(!labels[..index].contains(label), "{label} is used twice");
        }
    }

    /// Two prose regions are one list; two formulas are two blocks. Joining
    /// LaTeX with `; ` would produce an expression neither region contains.
    #[test]
    fn prose_regions_share_a_block_and_formulas_do_not() {
        let pieces = enrichment_pieces(&image(
            vec![
                kinded(RegionKind::Text, "Non-expert", 0.9, OcrAdmission::Accepted),
                kinded(RegionKind::Text, "User", 0.9, OcrAdmission::Accepted),
                kinded(RegionKind::Formula, "a^{2}", 0.9, OcrAdmission::Accepted),
                kinded(RegionKind::Formula, "b^{2}", 0.9, OcrAdmission::Accepted),
            ],
            None,
        ));
        assert_eq!(
            rendered(&pieces),
            "Image embedded text: Non-expert; User.\n\
             \n\
             Image embedded formula: a^{2}.\n\
             \n\
             Image embedded formula: b^{2}.\n"
        );
    }

    /// Every byte of a transcription says what kind of claim it is, including
    /// the label and the separators Wilkes wrote itself.
    #[test]
    fn provenance_names_the_kind_of_every_transcribed_byte() {
        let pieces = enrichment_pieces(&image(
            vec![kinded(
                RegionKind::Formula,
                "E = mc^{2}",
                0.9,
                OcrAdmission::Accepted,
            )],
            None,
        ));
        assert!(pieces.iter().all(|piece| matches!(
            piece.provenance,
            TextProvenance::ImageOcr {
                kind: RegionKind::Formula,
                ..
            }
        )));
    }

    /// Rejected and deduplicated regions stay out of the reading; they are
    /// visible in diagnostics and on the image, which is where a missing
    /// label should be looked for.
    #[test]
    fn only_accepted_regions_reach_the_reading() {
        let pieces = enrichment_pieces(&image(
            vec![
                region("Expert system", 0.9, OcrAdmission::Accepted),
                region("blurred", 0.2, OcrAdmission::RejectedLowConfidence),
                region("Figure 3", 0.9, OcrAdmission::DeduplicatedAgainstNativeText),
            ],
            None,
        ));
        assert_eq!(rendered(&pieces), "Image embedded text: Expert system.\n");
    }
}
