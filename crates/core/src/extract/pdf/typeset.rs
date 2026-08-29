//! Pages the recognizer should read, and the renders it reads them from.
//!
//! [`super::mupdf`] discovers the rasters a PDF embeds, which is what the
//! recognizer was fed until this module existed. A typeset formula is not one
//! of those: it is glyphs from a math font placed by the page, so MuPDF
//! reports it as text and the reading gets the flattened run — `ci = ai ⊕bi`,
//! which is not mathematics and which no consumer can parse back into any.
//!
//! ## Why this module no longer delimits anything
//!
//! It used to. Formula extents were reconstructed from MuPDF's text lines by
//! font share, baseline clustering, glyph-size spread, gap thresholds, margin
//! clamping and a stacking rule, and every one of those failed in a different
//! way on the first document it met:
//!
//! - a region stopped at its seed and cropped through `(mod q)`, so the
//!   recognizer was shown half a glyph and invented `( n_{0} )`;
//! - `y_B^{x_A}(mod q)` arrived as three fragments of two, two and four
//!   glyphs, none of which could seed a region, so the formula was never read;
//! - `w ∈ GF(q)` lost its parentheses outright, because this document draws
//!   them as *vector paths* — no glyph measurement can see them at all.
//!
//! The pattern is one mistake, not three. MuPDF's text lines are a lossy,
//! typesetter-dependent decomposition of what the page draws: they split one
//! expression into arbitrary fragments and drop every mark that is not a
//! glyph. Fraction bars, radicals, big operators and matrix rules are queued
//! behind the three above. Reassembling an expression from that decomposition
//! is a losing position, and each heuristic was a patch on the position rather
//! than a move out of it.
//!
//! ## What it does instead
//!
//! The recognizer is a document parser. It already returns `<formula>`,
//! `<otsl>` and `<chart>` elements *with coordinates*, so it already answers
//! "where does this formula begin and end" — better than any of the rules
//! above, and from a rendering in which vector parentheses are simply visible.
//!
//! So this module keeps the one job the typography survey is good at — a
//! cheap gate on which pages are worth a call — and hands the recognizer the
//! whole page. Measured on the reported document, that costs about what the
//! crops cost: 40 hand-cut crops came to 364 vision tiles against 403 for one
//! call on each of the 31 pages carrying mathematics, because the tiler
//! quantizes to whole 512-pixel tiles and no crop can cost less than five of
//! them however small the formula is. Cost being a wash is what makes the
//! choice: the same money buys either six heuristics or none.
//!
//! ## Ownership is unchanged
//!
//! A page render is not a second extraction of the document. Prose the
//! recognizer reads off it is refused by the rule that was already there —
//! [`crate::types::RegionKind::supersedes_native_glyphs`] admits only a
//! formula, a table or a chart from a typeset origin — so MuPDF remains the
//! sole owner of the document's prose by construction rather than by our
//! declining to look at the page.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use mupdf::device::NativeDevice;
use mupdf::text_page::TextBlockType;
use mupdf::{ColorParams, Colorspace, Device, IRect, Matrix, Text};
use tracing::warn;

use crate::extract::image::{self, DiscoveredImage, NativeImage};
use crate::types::{BoundingBox, ImageTransform, Point, RegionOrigin};

// ── The pinned recipe ────────────────────────────────────────────────────────

/// Bumped when anything in this module changes what is marked out or how it is
/// rendered. Part of the enrichment recipe an extractor declares, so a reading
/// produced under different routing is never mistaken for one produced under
/// this.
pub(super) const ROUTING_VERSION: &str = "typeset-routing-v5";

/// What the longest edge of a rendered page is aimed at, in pixels.
///
/// The recognizer fits the longest edge to 2048 before tiling, so rendering
/// there hands it the resolution it will actually use: below it the render is
/// upsampled and detail is invented, above it the pixels are paid for and
/// thrown away.
const TARGET_LONGEST_PX: f32 = 2048.0;

/// The most pages one document may spend recognizer calls on.
///
/// A runaway bound, not a policy. Recognition is tens of seconds a page on a
/// CPU — around thirteen vision tiles for an A4 page, and roughly two seconds
/// a tile on an M1 — so a document is minutes per hundred pages and this cap
/// is where a single file stops being able to occupy the machine indefinitely.
/// It is set past any book somebody reads rather than at a comfortable wait,
/// because a cap that truncates ordinary documents is a silent quality
/// setting wearing a safety limit's clothes.
///
/// What it drops is counted in
/// [`crate::types::ExtractionDiagnostics::typeset_regions_over_budget`] and
/// logged, because a bounded run that reports nothing dropped reads exactly
/// like a document that had nothing more to find.
const MAX_PAGES_PER_DOCUMENT: usize = 500;

/// Trim a document's pages to what it may spend, reporting what it cost.
pub(super) fn within_budget(
    pages: Vec<u32>,
    diagnostics: &mut crate::types::ExtractionDiagnostics,
) -> Vec<u32> {
    diagnostics.typeset_pages_read = pages.len() as u32;
    if pages.len() <= MAX_PAGES_PER_DOCUMENT {
        return pages;
    }
    let dropped = pages.len() - MAX_PAGES_PER_DOCUMENT;
    diagnostics.typeset_regions_over_budget = dropped as u32;
    warn!(
        "typeset routing: {} pages to read, {MAX_PAGES_PER_DOCUMENT} rendered, \
         {dropped} left unread — the per-document budget was reached, and those \
         pages keep the reading they already had",
        pages.len()
    );
    pages.into_iter().take(MAX_PAGES_PER_DOCUMENT).collect()
}

/// Break each recognized page render into one image per region it found.
///
/// A page render is a means, not a record. What belongs in the reading is the
/// formulas and tables the recognizer marked out on it, each at its own place
/// on the page and in its own position in the reading — a page carrying three
/// formulas contributes three, in the order the page draws them, not one block
/// of three at whichever position the page happened to start.
///
/// So the render is expanded here into an image per accepted region, carrying
/// that region's own page rectangle, and the render itself is dropped. Every
/// consumer downstream — placement, the source map, chunking, export — then
/// sees exactly what it saw when regions were cut by hand, and none of them
/// had to learn that a page is now a thing.
///
/// The digest stays the page's, because the page is what was analyzed and the
/// annotation cache is addressed by the thing that was analyzed. Caching
/// happens before this split, at the level of the call that was actually made.
pub(super) fn split_page_regions(
    images: Vec<crate::types::ExtractedImage>,
    diagnostics: &mut crate::types::ExtractionDiagnostics,
) -> Vec<crate::types::ExtractedImage> {
    let mut out = Vec::with_capacity(images.len());
    for image in images {
        if image.origin != RegionOrigin::Typeset {
            out.push(image);
            continue;
        }
        for (ordinal, region) in image.accepted_ocr().enumerate() {
            let bbox = crate::extract::image::serialize::polygon_hull(
                &region.page_polygon,
                &image.bbox,
            );
            let pixels = hull_of_points(&region.polygon_within_image);
            diagnostics.typeset_regions_found += 1;
            out.push(crate::types::ExtractedImage {
                id: format!("p{}-v{ordinal}", image.page),
                page: image.page,
                origin: RegionOrigin::Typeset,
                transform: ImageTransform {
                    a: bbox.width,
                    b: 0.0,
                    c: 0.0,
                    d: bbox.height,
                    e: bbox.x,
                    f: bbox.y,
                },
                bbox,
                pixel_width: pixels.width.max(0.0) as u32,
                pixel_height: pixels.height.max(0.0) as u32,
                image_sha256: image.image_sha256.clone(),
                reading_range: None,
                ocr_regions: vec![region.clone()],
                description: None,
                analyzer_identity: image.analyzer_identity.clone(),
                status: image.status.clone(),
            });
        }
    }
    out
}

fn hull_of_points(points: &[Point]) -> BoundingBox {
    let Some(first) = points.first() else {
        return BoundingBox { x: 0.0, y: 0.0, width: 0.0, height: 0.0 };
    };
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
    for point in points {
        x0 = x0.min(point.x);
        y0 = y0.min(point.y);
        x1 = x1.max(point.x);
        y1 = y1.max(point.y);
    }
    BoundingBox { x: x0, y: y0, width: x1 - x0, height: y1 - y0 }
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Draw one whole page for the recognizer, and say what page area the pixels
/// cover.
///
/// The whole page and not a crop, for the reason in the module documentation.
/// Nothing is cut out, so nothing can be cut *through*: the recognizer sees
/// every mark the page draws, including the ones that are not glyphs, and
/// decides for itself where an expression begins and ends.
fn render(page: &mupdf::Page) -> anyhow::Result<(NativeImage, BoundingBox)> {
    let bounds = page.bounds()?;
    let (width, height) = (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
    anyhow::ensure!(width > 0.0 && height > 0.0, "page has no area");
    let scale = TARGET_LONGEST_PX / width.max(height);

    let rect = IRect {
        x0: (bounds.x0 * scale).floor() as i32,
        y0: (bounds.y0 * scale).floor() as i32,
        x1: (bounds.x1 * scale).ceil() as i32,
        y1: (bounds.y1 * scale).ceil() as i32,
    };
    let (pixels_wide, pixels_high) =
        ((rect.x1 - rect.x0) as u32, (rect.y1 - rect.y0) as u32);
    if let Some(reason) = image::technical_limit(pixels_wide, pixels_high) {
        anyhow::bail!("page {pixels_wide}x{pixels_high} at scale {scale:.1}: {reason}");
    }

    let mut pixmap = mupdf::Pixmap::new_with_rect(&Colorspace::device_rgb(), rect, false)?;
    // The page paints only what it draws; everything else has to start as
    // paper rather than as whatever the allocation held.
    pixmap.clear_with(0xff)?;
    let device = Device::from_pixmap(&pixmap)?;
    page.run(&device, &Matrix::new_scale(scale, scale))?;
    drop(device);

    let decoded = image::decode(
        pixmap.width(),
        pixmap.height(),
        pixmap.n() as usize,
        pixmap.stride() as usize,
        pixmap.samples(),
    )
    .map_err(|reason| anyhow::anyhow!("page did not decode: {reason}"))?;

    // Derived back from the pixel grid, so the transform recorded for the
    // render is an exact map from a pixel of it to a point of the page — which
    // is what places every region the recognizer finds.
    Ok((
        decoded,
        BoundingBox {
            x: rect.x0 as f32 / scale,
            y: rect.y0 as f32 / scale,
            width: pixels_wide as f32 / scale,
            height: pixels_high as f32 / scale,
        },
    ))
}

/// Render one page and hand it to discovery as the image it now is.
///
/// The id says which page it is and that it is a render rather than something
/// the PDF embedded, because the id is what
/// [`crate::types::TextProvenance`] carries and a reader resolving it is
/// entitled to know which kind of thing it names.
pub(super) fn discover_page(
    page: &mupdf::Page,
    page_number: u32,
    discovered: &mut Vec<DiscoveredImage>,
) {
    let id = format!("p{page_number}-page");
    let (decoded, covered, rejected) = match render(page) {
        Ok((decoded, covered)) => (Some(decoded), covered, None),
        Err(error) => {
            warn!("typeset page {id}: {error:#}");
            (
                None,
                BoundingBox { x: 0.0, y: 0.0, width: 0.0, height: 0.0 },
                Some(format!("{error:#}")),
            )
        }
    };
    discovered.push(DiscoveredImage {
        id,
        page: page_number,
        origin: RegionOrigin::Typeset,
        bbox: covered.clone(),
        // The render maps the unit square onto exactly the page rectangle it
        // drew, upright and unrotated, because that is how it was asked for.
        transform: ImageTransform {
            a: covered.width,
            b: 0.0,
            c: 0.0,
            d: covered.height,
            e: covered.x,
            f: covered.y,
        },
        decoded,
        rejected,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ExtractedImage, ExtractionDiagnostics, ImageAnalysisStatus, ImageOcrRegion, OcrAdmission,
        RegionKind,
    };

    // ── The font signal ──────────────────────────────────────────────────

    #[test]
    fn tex_and_opentype_math_families_are_math() {
        for name in [
            "CMMI10",
            "ABCDEF+CMMI10",
            "CMSY7",
            "CMEX10",
            "MSAM10",
            "LMMathItalic12",
            "XYZABC+LatinModernMath-Regular",
            "STIXTwoMath-Regular",
            "Cambria Math",
            "TeXGyrePagella-Math",
            // A publisher rolling its own and naming it for the job. This is
            // the face the reported document sets its mathematics in.
            "DBAMWK+Formula",
            "Formula",
            "EquationFont",
        ] {
            assert!(is_math_font(name), "{name} should be a math font");
        }
    }

    #[test]
    fn text_families_are_not_math() {
        for name in [
            "CMR10",
            "ABCDEF+CMR10",
            "NimbusRomNo9L-Regu",
            "Times New Roman",
            "Helvetica",
            "LMRoman10-Regular",
            "SFRM1000",
            "Arial-BoldMT",
            "DBAMWK+SourceSans",
            "DBAMWK+SourceSansBold",
            "DBAMWK+SourceCode",
            // A real text typeface whose name starts like "formula".
            "Formata-Regular",
        ] {
            assert!(!is_math_font(name), "{name} should not be a math font");
        }
    }

    // ── The gate ─────────────────────────────────────────────────────────

    fn survey_of(math_glyphs: usize, rules: usize) -> Survey {
        Survey {
            math_origins: (0..math_glyphs)
                .map(|n| Point { x: n as f32, y: 0.0 })
                .collect(),
            faces: BTreeMap::new(),
            rules: (0..rules)
                .map(|n| BoundingBox {
                    x: 100.0,
                    y: 100.0 + n as f32 * 20.0,
                    width: 300.0,
                    height: 0.4,
                })
                .collect(),
        }
    }

    /// The gate decides only whether to look. A page carrying an expression
    /// is worth a call; a page mentioning a variable is not.
    #[test]
    fn a_page_is_read_for_its_mathematics_or_for_its_rules() {
        assert!(worth_reading(&survey_of(MIN_MATH_GLYPHS_PER_PAGE, 0)));
        assert!(!worth_reading(&survey_of(MIN_MATH_GLYPHS_PER_PAGE - 1, 0)));
        assert!(
            worth_reading(&survey_of(0, MIN_RULES_PER_PAGE)),
            "a ruled table carries no mathematics and is still worth reading"
        );
        assert!(!worth_reading(&survey_of(0, MIN_RULES_PER_PAGE - 1)));
        assert!(!worth_reading(&Survey::default()));
    }

    // ── The budget ───────────────────────────────────────────────────────

    /// A bounded run that reports nothing dropped reads exactly like a
    /// document that had nothing more to find.
    #[test]
    fn the_budget_reports_what_it_dropped() {
        let mut diagnostics = ExtractionDiagnostics::default();
        let kept = within_budget(
            (0..MAX_PAGES_PER_DOCUMENT as u32 + 7).collect(),
            &mut diagnostics,
        );
        assert_eq!(kept.len(), MAX_PAGES_PER_DOCUMENT);
        assert_eq!(
            diagnostics.typeset_pages_read,
            MAX_PAGES_PER_DOCUMENT as u32 + 7
        );
        assert_eq!(diagnostics.typeset_regions_over_budget, 7);
    }

    #[test]
    fn a_document_within_the_budget_drops_nothing() {
        let mut diagnostics = ExtractionDiagnostics::default();
        assert!(within_budget(Vec::new(), &mut diagnostics).is_empty());
        assert_eq!(diagnostics.typeset_regions_over_budget, 0);
    }

    // ── Splitting a read page ────────────────────────────────────────────

    fn region(kind: RegionKind, admission: OcrAdmission, x: f32, y: f32) -> ImageOcrRegion {
        ImageOcrRegion {
            kind,
            text: "E = mc^{2}".to_string(),
            confidence: 0.9,
            polygon_within_image: vec![
                Point { x: x * 4.0, y: y * 4.0 },
                Point { x: x * 4.0 + 200.0, y: y * 4.0 + 40.0 },
            ],
            page_polygon: vec![
                Point { x, y },
                Point { x: x + 50.0, y },
                Point { x: x + 50.0, y: y + 10.0 },
                Point { x, y: y + 10.0 },
            ],
            admission,
        }
    }

    fn read_page(regions: Vec<ImageOcrRegion>) -> ExtractedImage {
        ExtractedImage {
            id: "p7-page".into(),
            page: 7,
            origin: RegionOrigin::Typeset,
            bbox: BoundingBox { x: 0.0, y: 0.0, width: 595.0, height: 841.0 },
            transform: ImageTransform {
                a: 595.0,
                b: 0.0,
                c: 0.0,
                d: 841.0,
                e: 0.0,
                f: 0.0,
            },
            pixel_width: 1448,
            pixel_height: 2048,
            image_sha256: "page-digest".into(),
            reading_range: None,
            ocr_regions: regions,
            description: None,
            analyzer_identity: "test".into(),
            status: ImageAnalysisStatus::Complete,
        }
    }

    /// A page carrying three formulas contributes three images, each with its
    /// own page rectangle — not one block of three at whichever position the
    /// page happened to start.
    #[test]
    fn a_read_page_becomes_one_image_per_region_it_found() {
        let mut diagnostics = ExtractionDiagnostics::default();
        let split = split_page_regions(
            vec![read_page(vec![
                region(RegionKind::Formula, OcrAdmission::Accepted, 100.0, 200.0),
                region(RegionKind::Table, OcrAdmission::Accepted, 100.0, 400.0),
            ])],
            &mut diagnostics,
        );

        assert_eq!(split.len(), 2, "the page itself is not a record");
        assert!(split.iter().all(|image| image.page == 7));
        assert_eq!(split[0].id, "p7-v0");
        assert_eq!(split[1].id, "p7-v1");
        assert!((split[0].bbox.y - 200.0).abs() < 0.01, "{:?}", split[0].bbox);
        assert!((split[1].bbox.y - 400.0).abs() < 0.01, "{:?}", split[1].bbox);
        assert_eq!(split[0].ocr_regions.len(), 1, "one region each");
        assert_eq!(diagnostics.typeset_regions_found, 2);
    }

    /// The digest stays the page's: the page is what was analyzed, and the
    /// annotation cache is addressed by the thing that was analyzed.
    #[test]
    fn a_split_region_carries_the_digest_of_the_page_that_was_read() {
        let mut diagnostics = ExtractionDiagnostics::default();
        let split = split_page_regions(
            vec![read_page(vec![region(
                RegionKind::Formula,
                OcrAdmission::Accepted,
                100.0,
                200.0,
            )])],
            &mut diagnostics,
        );
        assert_eq!(split[0].image_sha256, "page-digest");
    }

    /// A refused region is not a region. Prose the recognizer read off the
    /// page reaches here already refused by the kind rule, and contributes
    /// nothing — which is what keeps a page render from being a second
    /// extraction of the document.
    #[test]
    fn a_refused_region_contributes_no_image() {
        let mut diagnostics = ExtractionDiagnostics::default();
        let split = split_page_regions(
            vec![read_page(vec![
                region(
                    RegionKind::Text,
                    OcrAdmission::DeduplicatedAgainstNativeText,
                    100.0,
                    200.0,
                ),
                region(
                    RegionKind::Formula,
                    OcrAdmission::RejectedInvalidLatex,
                    100.0,
                    300.0,
                ),
            ])],
            &mut diagnostics,
        );
        assert!(split.is_empty());
        assert_eq!(diagnostics.typeset_regions_found, 0);
    }

    /// An embedded picture passes through untouched: it is not a page render
    /// and has nothing to split.
    #[test]
    fn an_embedded_image_is_left_alone() {
        let mut diagnostics = ExtractionDiagnostics::default();
        let mut embedded = read_page(vec![region(
            RegionKind::Text,
            OcrAdmission::Accepted,
            10.0,
            20.0,
        )]);
        embedded.origin = RegionOrigin::Embedded;
        embedded.id = "p7-i0".into();
        let split = split_page_regions(vec![embedded], &mut diagnostics);
        assert_eq!(split.len(), 1);
        assert_eq!(split[0].id, "p7-i0");
        assert_eq!(diagnostics.typeset_regions_found, 0);
    }
}
