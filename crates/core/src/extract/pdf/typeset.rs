//! Regions the page *draws* rather than embeds, marked out by a layout model.
//!
//! [`super::mupdf`] discovers embedded rasters, which is what the recognizer
//! was fed until this module existed. A LaTeX-typeset formula is not one of
//! those: it is glyphs from a math font placed by the page, so MuPDF reports
//! it as text and the reading gets the flattened glyph run — `ci = ai ⊕bi` —
//! which is not mathematics and which no consumer can parse back into any.
//! This module is the head of that path: it marks out the areas of a page that
//! are formulas, tables and charts, so the same recognizer, the same admission
//! rules and the same serialization can read them.
//!
//! ## What decides, and what used to
//!
//! A layout detector — [`crate::extract::image::doclayout`] — looks at a
//! render of the page and names each area. That is the whole of the decision.
//!
//! It replaced two rules that read the file instead of the picture. Formulas
//! were found by the *font* each glyph was drawn in, against a list of face
//! names; tables were found by the *rules* the page stroked, three or more
//! stacked and sharing a column. Each came with its own constants — a share of
//! a line's glyphs, a minimum glyph count, a baseline spread, two gap factors,
//! a rule thickness, a minimum width, a span — and each was tuned on the
//! documents that prompted it.
//!
//! They are gone, and the reasons are worth keeping:
//!
//! - **The font rule could not see inline mathematics at all.** Its unit was
//!   the line and its test was a share of that line's glyphs, so an expression
//!   sharing its line with prose was unreachable *at any threshold*. That is
//!   not a tuning failure, it is the shape of the rule.
//! - **The font rule needed a registry.** A publisher setting its mathematics
//!   in a house face named nothing like `CMMI` yielded no formulas and read
//!   exactly like a document with no mathematics in it.
//! - **The rule stack found booktabs tables and missed unruled ones**, and
//!   fired on framed asides that are not tables at all.
//!
//! What is left here is geometry, and geometry only: where a detected area
//! sits on the page, which of the page's own lines it covers, and how to
//! render it for the recognizer. Nothing in this module decides what an area
//! *contains*.
//!
//! ## Everything it finds still fails safe
//!
//! A region marked out here is only a decision to *spend a recognizer call*.
//! Whether its answer replaces the page's glyphs is settled afterwards by the
//! same admission rules every other region meets — a formula on whether its
//! LaTeX closes, a table on whether it is rectangular. A false positive costs
//! time and changes no bytes: the region is refused and the glyph run the page
//! drew stays exactly where it was.

use mupdf::{Colorspace, Device, IRect, Matrix};
use tracing::warn;

use crate::extract::image::{self, DiscoveredImage, LayoutRegion, NativeImage};
use crate::types::{BoundingBox, ImageTransform, RegionKind, RegionOrigin};

// ── The pinned recipe ────────────────────────────────────────────────────────

/// Bumped when anything in this module changes what is marked out or how it is
/// rendered. Part of the enrichment recipe an extractor declares, so a reading
/// produced under different routing is never mistaken for one produced under
/// this. The detector's own identity joins it separately, through the
/// analyzer.
///
/// v7 is the layout detector replacing the font and rule heuristics. v8 marks
/// out *words* rather than lines, which both tightens every crop to the glyphs
/// it replaces and makes an expression inside a sentence routable at all.
pub(super) const ROUTING_VERSION: &str = "typeset-routing-v8";

/// How much of a page word must fall inside a detected area for the area to
/// speak for that word.
///
/// Not a tuning knob for *what is a formula* — the detector answers that. This
/// is the containment test that decides which glyph runs leave the reading
/// when the area's reading is admitted, and it is a majority: a word more than
/// half inside a detected formula is part of it, and one that merely brushes
/// its edge is not. The alternative, requiring total containment, loses the
/// last word of every region whose box the detector drew a point tight.
const WORD_INSIDE_SHARE: f32 = 0.5;

/// White space left around a region when it is rendered, in points.
///
/// A recognizer meets a page with margins; a crop shaved to the ink is not
/// something it was trained on, and the first and last glyph sit against the
/// edge where a tiler will cut them.
const MARGIN_POINTS: f32 = 8.0;

/// What the longest edge of a rendered region is aimed at, in pixels.
///
/// A display formula is a few hundred points wide, so this lands the render
/// between four and eight times page scale — enough that a subscript survives
/// the resize a recognizer does on the way in.
const TARGET_LONGEST_PX: f32 = 1600.0;

/// The render scale is clamped into this band: below it a wide table would be
/// read from too few pixels, above it a small formula would cost megabytes to
/// say the same thing.
const MIN_RENDER_SCALE: f32 = 2.0;
const MAX_RENDER_SCALE: f32 = 8.0;

/// The most lopsided a rendered region may be before it is padded out with
/// white.
///
/// A recognizer resizes what it is given onto a fixed grid. A 20:1 strip
/// arriving at a 4:1 grid is not cropped, it is *squashed* — every glyph
/// stretched five times vertically — and that is a distortion the model never
/// saw in training. Padding costs blank pixels and preserves the shapes, which
/// is the trade worth making.
///
/// The pad is *white*, not more of the page. A display formula padded with
/// page content would arrive carrying the paragraphs above and below it, and
/// the recognizer would read those too — prose the reading already has, which
/// would then be inserted a second time. The region is what was routed, and
/// the render shows the region and nothing else.
const MAX_ASPECT: f32 = 4.0;

// ── What is marked out ───────────────────────────────────────────────────────

/// One word of one page, as this module needs it: where it sits, and nothing
/// else.
///
/// A *word* and not a line. Supersession is geometric — a detected area speaks
/// for the glyphs it covers — and at line granularity the smallest thing an
/// area can speak for is a whole line. That is right for a display formula,
/// which is a line, and wrong for an expression inside a sentence, which is
/// four words in the middle of one. Measured before this was a word: an inline
/// formula's box, grown by the render margin, reached into the words either
/// side of it, and the recognizer was handed `th √n ∈ ℕ. T` and read the prose
/// as mathematics.
#[derive(Clone, Debug)]
pub(super) struct WordBox {
    /// Index of the block this word belongs to, in the page's block list.
    pub block: usize,
    /// Index of the line within that block.
    pub line: usize,
    /// Index of the word within that line.
    pub word: usize,
    pub bbox: BoundingBox,
}

impl WordBox {
    fn right(&self) -> f32 {
        self.bbox.x + self.bbox.width
    }

    fn bottom(&self) -> f32 {
        self.bbox.y + self.bbox.height
    }
}

/// One area of one page to render and read, and the words it stands in place
/// of.
pub(super) struct TypesetRegion {
    pub page: u32,
    /// What the detector called this area. Carried through to discovery, where
    /// it decides which recognizer reads the region — a formula and a page are
    /// different inputs.
    pub kind: RegionKind,
    /// The area to render, already carrying its margin.
    pub bbox: BoundingBox,
    /// The page's own words this region covers, as `(block, line, word)`.
    /// These are the page's glyphs, and they leave the reading only if the
    /// recognizer's answer for this region is admitted.
    pub words: Vec<(usize, usize, usize)>,
}

/// Turn one page's detections into the regions that will be rendered and read.
///
/// Three things happen, in order, and none of them is a judgement about
/// content:
///
/// 1. Detections the build routes nothing for are dropped. `kind` is `None`
///    for every class whose reading would be refused on arrival — see
///    [`crate::extract::image::doclayout::kind_of`] — and rendering those
///    would be a recognizer call whose answer is discarded by rule.
/// 2. The remaining boxes are put into page coordinates and matched to the
///    page's own lines by containment.
/// 3. An area covering no line *of its own* is dropped. Not because it is
///    wrong — the detector may well have found a picture the page draws — but
///    because there is no glyph run for its reading to stand in place of, and
///    this path's whole contract is that a typeset region replaces one.
///
/// A line belongs to at most one region. The detector's classes nest — a
/// display formula inside a table, a formula number beside one — and two
/// regions claiming the same glyph run would mean one of them reading a line
/// that has already left the document with the other. Largest first, so the
/// enclosing structure owns what it encloses: a formula inside a table cell is
/// better read as part of the table than as a formula the table then reads
/// again.
pub(super) fn regions(
    page: u32,
    bounds: &BoundingBox,
    detections: &[LayoutRegion],
    words: &[WordBox],
) -> Vec<TypesetRegion> {
    let mut order: Vec<&LayoutRegion> = detections
        .iter()
        .filter(|detection| detection.kind.is_some())
        .collect();
    order.sort_by(|a, b| {
        let area = |d: &LayoutRegion| d.bbox.width * d.bbox.height;
        area(b)
            .total_cmp(&area(a))
            // Ties broken by score, so the order does not depend on how the
            // graph happened to number two boxes of the same size.
            .then(b.score.total_cmp(&a.score))
    });

    let mut claimed = vec![false; words.len()];
    let mut found = Vec::new();
    for detection in order {
        let area = BoundingBox {
            x: bounds.x + detection.bbox.x * bounds.width,
            y: bounds.y + detection.bbox.y * bounds.height,
            width: detection.bbox.width * bounds.width,
            height: detection.bbox.height * bounds.height,
        };
        let owned: Vec<usize> = words
            .iter()
            .enumerate()
            .filter(|(index, word)| {
                !claimed[*index] && share_inside(&word.bbox, &area) >= WORD_INSIDE_SHARE
            })
            .map(|(index, _)| index)
            .collect();
        if owned.is_empty() {
            continue;
        }
        for index in &owned {
            claimed[*index] = true;
        }
        // The area the recognizer sees is the union of the words it speaks
        // for, and no more.
        //
        // The detector's own box is deliberately *not* unioned in. It is drawn
        // on an 800x800 render of the page and lands a point or two wide of
        // the ink either side; at line granularity that slack fell in the
        // margin and cost nothing, and at word granularity it is the
        // neighbouring word. What the region replaces is these words, so what
        // it shows is these words.
        let hull = owned
            .iter()
            .map(|index| words[*index].bbox.clone())
            .reduce(|hull, bbox| union(&hull, &bbox))
            .expect("a region owns at least one word");
        found.push(TypesetRegion {
            page,
            kind: detection
                .kind
                .expect("kind-less detections were filtered out"),
            bbox: with_margin_clear_of(&hull, words, &owned),
            words: owned
                .iter()
                .map(|index| (words[*index].block, words[*index].line, words[*index].word))
                .collect(),
        });
    }
    found
}

/// How much of `word` falls inside `area`, as a share of the word's own area.
fn share_inside(word: &BoundingBox, area: &BoundingBox) -> f32 {
    let own = word.width * word.height;
    if own <= 0.0 {
        return 0.0;
    }
    let width = (word.x + word.width).min(area.x + area.width) - word.x.max(area.x);
    let height = (word.y + word.height).min(area.y + area.height) - word.y.max(area.y);
    if width <= 0.0 || height <= 0.0 {
        return 0.0;
    }
    (width * height) / own
}

fn union(a: &BoundingBox, b: &BoundingBox) -> BoundingBox {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    BoundingBox {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

/// Draw one whole page at the size the detector declares.
///
/// Stretched into the square rather than letterboxed, because the detector's
/// own preprocessing says `keep_ratio: false` and a model fed pixels laid out
/// differently from its training is a model answering a different question.
/// The map back to page points is therefore two independent scales, and it is
/// done by [`regions`] from the page's own bounds.
pub(super) fn render_page(page: &mupdf::Page, side: u32) -> anyhow::Result<::image::RgbImage> {
    let bounds = page.bounds()?;
    let (width, height) = (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
    anyhow::ensure!(
        width > 0.0 && height > 0.0,
        "a page with no extent cannot be detected on"
    );
    let canvas = IRect {
        x0: 0,
        y0: 0,
        x1: side as i32,
        y1: side as i32,
    };
    let mut pixmap = mupdf::Pixmap::new_with_rect(&Colorspace::device_rgb(), canvas, false)?;
    // Paper first: the page paints only what it draws.
    pixmap.clear_with(0xff)?;
    let device = Device::from_pixmap(&pixmap)?;
    page.run(
        &device,
        &Matrix::new_scale(side as f32 / width, side as f32 / height),
    )?;
    drop(device);

    let decoded = image::decode(
        pixmap.width(),
        pixmap.height(),
        pixmap.n() as usize,
        pixmap.stride() as usize,
        pixmap.samples(),
    )
    .map_err(|reason| anyhow::anyhow!("the page did not render for detection: {reason}"))?;
    Ok(decoded.pixels)
}

/// Grow the hull by as much margin as can be had without showing a word
/// the region does not own.
///
/// A recognizer meets a page with margins, so a crop shaved to the ink is not
/// what it was trained on — but a crop carrying a *sliver of the next
/// expression* is worse than either, because half a glyph is an invitation to
/// invent the rest of one. So the margin is as much as can be had without
/// showing anything the region does not speak for.
///
/// At word granularity this is the whole of the crop's right-hand boundary
/// rather than a second line of defence: an inline expression has a word four
/// points to its left and another four points to its right, so the margin it
/// gets is whatever fits between them, which is usually none.
fn with_margin_clear_of(hull: &BoundingBox, words: &[WordBox], owned: &[usize]) -> BoundingBox {
    let (mut left, mut right) = (MARGIN_POINTS, MARGIN_POINTS);
    let (mut top, mut bottom) = (MARGIN_POINTS, MARGIN_POINTS);
    let (hull_right, hull_bottom) = (hull.x + hull.width, hull.y + hull.height);

    for (index, word) in words.iter().enumerate() {
        if owned.contains(&index) {
            continue;
        }
        if word.bbox.y < hull_bottom && hull.y < word.bottom() {
            if word.right() <= hull.x {
                left = left.min(hull.x - word.right());
            }
            if word.bbox.x >= hull_right {
                right = right.min(word.bbox.x - hull_right);
            }
        }
        if word.bbox.x < hull_right && hull.x < word.right() {
            if word.bottom() <= hull.y {
                top = top.min(hull.y - word.bottom());
            }
            if word.bbox.y >= hull_bottom {
                bottom = bottom.min(word.bbox.y - hull_bottom);
            }
        }
    }

    let (left, right) = (left.max(0.0), right.max(0.0));
    let (top, bottom) = (top.max(0.0), bottom.max(0.0));
    BoundingBox {
        x: hull.x - left,
        y: hull.y - top,
        width: hull.width + left + right,
        height: hull.height + top + bottom,
    }
}

/// Record how many regions a document marked out. Every one of them is
/// rendered and read.
///
/// There used to be a cap here — the first 250 regions of a document, the rest
/// discovered and dropped. It was a cost bound written when every region meant
/// tens of seconds of CPU recognition, and it did the one thing a cost bound
/// must not: it truncated by page order, so a mathematics textbook lost the
/// formulas in its later chapters and kept the ones in its first. A reader
/// searching that book would find its early equations and not its late ones,
/// with nothing in the reading to say why.
///
/// The cost it was bounding is now bounded where it belongs: by
/// [`crate::types::ImageScope`], which is a choice the user makes about the
/// whole library, rather than by a silent truncation of one document.
pub(super) fn counted(
    regions: Vec<TypesetRegion>,
    diagnostics: &mut crate::types::ExtractionDiagnostics,
) -> Vec<TypesetRegion> {
    diagnostics.typeset_regions_found = regions.len() as u32;
    regions
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// The scale one region is drawn at.
fn render_scale(bbox: &BoundingBox) -> f32 {
    let longest = bbox.width.max(bbox.height).max(1.0);
    (TARGET_LONGEST_PX / longest).clamp(MIN_RENDER_SCALE, MAX_RENDER_SCALE)
}

/// Draw one region of one page, and hand back what the recognizer will read
/// together with the page rectangle those pixels cover.
///
/// The returned rectangle is the page area the *canvas* covers, derived back
/// from the pixel grid so that the transform recorded for the render is an
/// exact map from a pixel of it to a point of the page. A recognized region's
/// polygon therefore lands where the page draws it, which is what every
/// consumer of the source map resolves.
///
/// Padding is applied to the pixel rectangle and not to the page rectangle.
/// Doing it in page space and rounding to pixels afterwards was the same idea
/// and cost twice the model: see [`pad_to_aspect`].
fn render(page: &mupdf::Page, bbox: &BoundingBox) -> anyhow::Result<(NativeImage, BoundingBox)> {
    let scale = render_scale(bbox);
    let scaled = |rect: &BoundingBox| IRect {
        x0: (rect.x * scale).floor() as i32,
        y0: (rect.y * scale).floor() as i32,
        x1: ((rect.x + rect.width) * scale).ceil() as i32,
        y1: ((rect.y + rect.height) * scale).ceil() as i32,
    };

    let canvas = pad_to_aspect(scaled(bbox));
    let (width, height) = (
        (canvas.x1 - canvas.x0) as u32,
        (canvas.y1 - canvas.y0) as u32,
    );
    if let Some(reason) = image::technical_limit(width, height) {
        anyhow::bail!("region {width}x{height} at scale {scale:.1}: {reason}");
    }

    let mut pixmap = mupdf::Pixmap::new_with_rect(&Colorspace::device_rgb(), canvas, false)?;
    // The page paints only what it draws; everything else has to start as
    // paper rather than as whatever the allocation held.
    pixmap.clear_with(0xff)?;
    // Clipped to the region and not to the canvas, so the pad stays paper.
    // The whole page is run and MuPDF keeps what falls inside the clip —
    // which is how the pad ends up white without a second image to compose.
    let device = Device::from_pixmap_with_clip(&pixmap, scaled(bbox))?;
    page.run(&device, &Matrix::new_scale(scale, scale))?;
    drop(device);

    let decoded = image::decode(
        pixmap.width(),
        pixmap.height(),
        pixmap.n() as usize,
        pixmap.stride() as usize,
        pixmap.samples(),
    )
    .map_err(|reason| anyhow::anyhow!("region did not decode: {reason}"))?;

    Ok((
        decoded,
        BoundingBox {
            x: canvas.x0 as f32 / scale,
            y: canvas.y0 as f32 / scale,
            width: width as f32 / scale,
            height: height as f32 / scale,
        },
    ))
}

/// Grow a rendered region until it is no more lopsided than [`MAX_ASPECT`],
/// for the reason given there. In pixels, symmetric, and never shrinking.
///
/// The rounding is the whole point of doing this here. A recognizer's tiler
/// takes the *pixel* dimensions and rounds them up to whole tiles, so a canvas
/// a hair under the bound is charged for a second row of them — measured, an
/// 1409x353 crop is 4x2 tiles where 1409x352 is 4x1, the same picture for
/// nearly twice the prefill. Padding in page points and rounding to pixels
/// afterwards could land on either side of that; padding the pixels lands on
/// the right one by construction.
///
/// Hence `floor` on the derived edge: it makes the padded ratio meet or pass
/// the bound rather than fall a fraction short of it. `max` against the
/// region's own edge keeps that from ever shrinking the region, which the
/// clip would then crop.
fn pad_to_aspect(region: IRect) -> IRect {
    let (width, height) = (region.x1 - region.x0, region.y1 - region.y0);
    let (padded_width, padded_height) = if width as f32 > height as f32 * MAX_ASPECT {
        (
            width,
            ((width as f32 / MAX_ASPECT).floor() as i32).max(height),
        )
    } else if height as f32 > width as f32 * MAX_ASPECT {
        (
            ((height as f32 / MAX_ASPECT).floor() as i32).max(width),
            height,
        )
    } else {
        (width, height)
    };
    let x0 = region.x0 - (padded_width - width) / 2;
    let y0 = region.y0 - (padded_height - height) / 2;
    IRect {
        x0,
        y0,
        x1: x0 + padded_width,
        y1: y0 + padded_height,
    }
}

/// Render each region of one page and hand it to discovery as the image it now
/// is.
///
/// The ordinal numbers typeset regions apart from embedded ones — `p3-v0`
/// beside `p3-i0` — because the id is what [`crate::types::TextProvenance`]
/// carries and a reader resolving it is entitled to know which kind of thing
/// it names.
/// Returns, for each region in order, the index it was given in `discovered`,
/// so the lines it speaks for can be pointed at it.
pub(super) fn discover(
    page: &mupdf::Page,
    found: &[TypesetRegion],
    discovered: &mut Vec<DiscoveredImage>,
) -> Vec<usize> {
    let mut placed = Vec::with_capacity(found.len());
    for (ordinal, region) in found.iter().enumerate() {
        let id = format!("p{}-v{ordinal}", region.page);
        let (decoded, covered, rejected) = match render(page, &region.bbox) {
            Ok((decoded, covered)) => (Some(decoded), covered, None),
            Err(error) => {
                warn!("typeset region {id}: {error:#}");
                (None, region.bbox.clone(), Some(format!("{error:#}")))
            }
        };
        placed.push(discovered.len());
        discovered.push(DiscoveredImage {
            id,
            page: region.page,
            origin: RegionOrigin::Typeset,
            bbox: covered.clone(),
            // The render maps the unit square onto exactly the page rectangle
            // it drew, upright and unrotated, because that is how it was
            // asked for.
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
            // A typeset region only exists because something is going to read
            // it: the detector marked it out, it covers a line of the page's
            // own, and the page was rendered for it. There is no scope under
            // which one is withheld.
            withheld_by_scope: false,
            kind: Some(region.kind),
        });
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One word of one line. The tests below mostly stand a whole line up as
    /// a single word, which is what a display formula's line is once the
    /// detector has drawn a box around it.
    fn word(block: usize, index: usize, x: f32, y: f32, w: f32, h: f32) -> WordBox {
        WordBox {
            block,
            line: index,
            word: 0,
            bbox: BoundingBox {
                x,
                y,
                width: w,
                height: h,
            },
        }
    }

    fn nth(mut boxed: WordBox, index: usize) -> WordBox {
        boxed.word = index;
        boxed
    }

    #[allow(clippy::too_many_arguments)]
    fn detected(kind: Option<RegionKind>, x: f32, y: f32, w: f32, h: f32) -> LayoutRegion {
        LayoutRegion {
            label: "display_formula",
            kind,
            score: 0.9,
            bbox: BoundingBox {
                x,
                y,
                width: w,
                height: h,
            },
        }
    }

    /// A page 600 x 800 points, so a detection at a tenth of the page is at 60
    /// and 80 points.
    fn page_bounds() -> BoundingBox {
        BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 600.0,
            height: 800.0,
        }
    }

    // ── Marking out regions ─────────────────────────────────────────────────

    /// The detector answers in fractions of the page and the page's own lines
    /// are in points. Getting this map wrong would put every region somewhere
    /// the page does not draw.
    #[test]
    fn a_detection_lands_where_the_page_draws_it() {
        let lines = vec![word(0, 0, 100.0, 200.0, 300.0, 12.0)];
        let found = regions(
            3,
            &page_bounds(),
            &[detected(Some(RegionKind::Formula), 0.1, 0.24, 0.6, 0.03)],
            &lines,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].page, 3);
        assert_eq!(found[0].words, vec![(0, 0, 0)]);
        // The detection at 0.24 x 800 = 192 finds the word at 200, and what
        // gets rendered is the *word* plus its margin — not the detector's
        // box, which is drawn on an 800x800 render and lands wide of the ink.
        assert_eq!(
            found[0].bbox.y,
            200.0 - MARGIN_POINTS,
            "{:?}",
            found[0].bbox
        );
        assert_eq!(
            found[0].bbox.x,
            100.0 - MARGIN_POINTS,
            "{:?}",
            found[0].bbox
        );
        assert_eq!(
            found[0].bbox.width,
            300.0 + 2.0 * MARGIN_POINTS,
            "{:?}",
            found[0].bbox
        );
    }

    /// An expression inside a sentence claims the words it covers and no
    /// others, and the crop it renders stops short of the words either side.
    /// Before regions were words, the detector's box plus the render margin
    /// reached into both neighbours and the recognizer read the prose as
    /// mathematics.
    #[test]
    fn an_inline_detection_claims_its_words_and_not_the_sentence() {
        // `with √n ∈ ℕ. Thus` — five words on one line, the middle three of
        // them the expression.
        let lines = vec![
            nth(word(0, 0, 100.0, 200.0, 20.0, 10.0), 0),
            nth(word(0, 0, 124.0, 200.0, 12.0, 10.0), 1),
            nth(word(0, 0, 140.0, 200.0, 8.0, 10.0), 2),
            nth(word(0, 0, 152.0, 200.0, 12.0, 10.0), 3),
            nth(word(0, 0, 168.0, 200.0, 24.0, 10.0), 4),
        ];
        // A box around the three middle words, drawn a point wide of them on
        // each side as the detector's own boxes are.
        let found = regions(
            1,
            &page_bounds(),
            &[detected(
                Some(RegionKind::Formula),
                123.0 / 600.0,
                199.0 / 800.0,
                42.0 / 600.0,
                12.0 / 800.0,
            )],
            &lines,
        );

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].words, vec![(0, 0, 1), (0, 0, 2), (0, 0, 3)]);
        assert!(
            found[0].bbox.x >= 120.0,
            "the word before it is not in the crop: {:?}",
            found[0].bbox
        );
        assert!(
            found[0].bbox.x + found[0].bbox.width <= 168.0,
            "nor the word after it: {:?}",
            found[0].bbox
        );
    }

    /// A class the build routes nothing for never becomes a region. Rendering
    /// one would be a recognizer call whose answer the admission rules discard.
    #[test]
    fn a_detection_with_no_kind_is_not_rendered() {
        let lines = vec![word(0, 0, 100.0, 200.0, 300.0, 12.0)];
        assert!(regions(
            1,
            &page_bounds(),
            &[detected(None, 0.1, 0.24, 0.6, 0.03)],
            &lines
        )
        .is_empty());
    }

    /// A detected area with no glyph run under it has nothing to stand in
    /// place of, which is this path's whole contract.
    #[test]
    fn a_detection_over_no_line_is_dropped() {
        let lines = vec![word(0, 0, 100.0, 700.0, 300.0, 12.0)];
        assert!(regions(
            1,
            &page_bounds(),
            &[detected(Some(RegionKind::Formula), 0.1, 0.24, 0.6, 0.03)],
            &lines,
        )
        .is_empty());
    }

    /// A line more than half inside the box belongs to it; one that merely
    /// brushes the edge does not. The line the detector drew tight around
    /// still leaves with its region.
    #[test]
    fn a_line_is_claimed_by_majority_and_not_by_touching() {
        let lines = vec![
            // Squarely inside.
            word(0, 0, 100.0, 200.0, 300.0, 12.0),
            // Overlapping the bottom edge by two points of twelve.
            word(0, 1, 100.0, 222.0, 300.0, 12.0),
        ];
        let found = regions(
            1,
            &page_bounds(),
            &[detected(Some(RegionKind::Formula), 0.1, 0.24, 0.6, 0.03)],
            &lines,
        );
        assert_eq!(found[0].words, vec![(0, 0, 0)], "the grazed line stays");
    }

    /// The rendered area is the union of the detector's box and every line it
    /// speaks for: reading a region that does not show the whole of what it
    /// replaces is how half an equation gets into a document.
    #[test]
    fn the_region_grows_to_cover_the_lines_it_replaces() {
        // A line running well right of the detected box, mostly inside it
        // vertically and horizontally overlapping by more than half.
        let lines = vec![word(0, 0, 100.0, 200.0, 420.0, 12.0)];
        let found = regions(
            1,
            &page_bounds(),
            &[detected(Some(RegionKind::Formula), 0.1, 0.24, 0.6, 0.03)],
            &lines,
        );
        let right = found[0].bbox.x + found[0].bbox.width;
        assert!(right >= 520.0, "the region reaches the line's end: {right}");
    }

    #[test]
    fn every_line_of_a_multi_line_area_leaves_together() {
        let lines = vec![
            word(0, 0, 100.0, 200.0, 300.0, 12.0),
            word(0, 1, 100.0, 214.0, 300.0, 12.0),
            word(1, 0, 100.0, 400.0, 300.0, 12.0),
        ];
        let found = regions(
            1,
            &page_bounds(),
            &[detected(Some(RegionKind::Formula), 0.1, 0.24, 0.6, 0.05)],
            &lines,
        );
        assert_eq!(found[0].words, vec![(0, 0, 0), (0, 1, 0)]);
    }

    /// The detector's classes nest: a display formula inside a table, a
    /// formula number beside one. Two regions claiming the same glyph run
    /// would mean one of them reading a line that has already left the
    /// document with the other, so the enclosing structure claims first.
    #[test]
    fn a_line_belongs_to_one_region_and_the_larger_structure_claims_it() {
        let lines = vec![
            word(0, 0, 100.0, 200.0, 300.0, 12.0),
            word(0, 1, 100.0, 214.0, 300.0, 12.0),
        ];
        let table = LayoutRegion {
            label: "table",
            kind: Some(RegionKind::Table),
            score: 0.9,
            bbox: BoundingBox {
                x: 0.1,
                y: 0.24,
                width: 0.6,
                height: 0.06,
            },
        };
        // A formula inside the table's first row.
        let formula = detected(Some(RegionKind::Formula), 0.15, 0.245, 0.4, 0.02);

        for order in [
            vec![table.clone(), formula.clone()],
            vec![formula.clone(), table.clone()],
        ] {
            let found = regions(1, &page_bounds(), &order, &lines);
            assert_eq!(found.len(), 1, "the enclosed region is not also read");
            assert_eq!(found[0].words, vec![(0, 0, 0), (0, 1, 0)]);
        }
    }

    /// Two areas that do not overlap are both read: the claim is per line, not
    /// a first-past-the-post over the page.
    #[test]
    fn two_separate_areas_are_both_marked_out() {
        let lines = vec![
            word(0, 0, 100.0, 200.0, 300.0, 12.0),
            word(1, 0, 100.0, 500.0, 300.0, 12.0),
        ];
        let found = regions(
            1,
            &page_bounds(),
            &[
                detected(Some(RegionKind::Formula), 0.1, 0.24, 0.6, 0.03),
                detected(Some(RegionKind::Formula), 0.1, 0.615, 0.6, 0.03),
            ],
            &lines,
        );
        assert_eq!(found.len(), 2);
    }

    // ── The margin ──────────────────────────────────────────────────────────

    #[test]
    fn a_region_with_room_keeps_its_whole_margin() {
        let hull = BoundingBox {
            x: 100.0,
            y: 200.0,
            width: 300.0,
            height: 12.0,
        };
        let padded = with_margin_clear_of(&hull, &[], &[]);
        assert_eq!(padded.x, 100.0 - MARGIN_POINTS);
        assert_eq!(padded.width, 300.0 + 2.0 * MARGIN_POINTS);
    }

    /// The margin is cut back rather than reaching into a line the region does
    /// not speak for: half a neighbouring glyph is an invitation to invent the
    /// rest of one.
    #[test]
    fn the_margin_is_cut_back_rather_than_reaching_into_a_line_it_does_not_own() {
        let hull = BoundingBox {
            x: 100.0,
            y: 200.0,
            width: 300.0,
            height: 12.0,
        };
        // Three points above the hull, overlapping it horizontally.
        let neighbour = word(0, 0, 100.0, 185.0, 300.0, 12.0);
        let padded = with_margin_clear_of(&hull, std::slice::from_ref(&neighbour), &[]);
        assert_eq!(padded.y, 200.0 - 3.0);
        assert_eq!(padded.x, 100.0 - MARGIN_POINTS, "sideways is untouched");
    }

    #[test]
    fn a_line_the_region_owns_does_not_cut_its_margin() {
        let hull = BoundingBox {
            x: 100.0,
            y: 200.0,
            width: 300.0,
            height: 12.0,
        };
        let owned = word(0, 0, 100.0, 185.0, 300.0, 12.0);
        let padded = with_margin_clear_of(&hull, std::slice::from_ref(&owned), &[0]);
        assert_eq!(padded.y, 200.0 - MARGIN_POINTS);
    }

    // ── Rendering ───────────────────────────────────────────────────────────

    #[test]
    fn a_sliver_is_padded_rather_than_left_to_be_squashed() {
        let padded = pad_to_aspect(IRect {
            x0: 0,
            y0: 0,
            x1: 1600,
            y1: 80,
        });
        let (width, height) = (padded.x1 - padded.x0, padded.y1 - padded.y0);
        assert_eq!(width, 1600, "the long edge is never grown");
        assert!(
            width as f32 <= height as f32 * MAX_ASPECT,
            "{width}x{height} is still past the bound"
        );
    }

    /// A canvas a hair under the bound is charged for a whole extra row of
    /// tiles, so the padded edge must meet or pass it rather than fall short.
    #[test]
    fn a_padded_sliver_never_lands_just_under_the_bound() {
        for width in [1409, 1600, 801, 999] {
            let padded = pad_to_aspect(IRect {
                x0: 0,
                y0: 0,
                x1: width,
                y1: 40,
            });
            let (w, h) = (padded.x1 - padded.x0, padded.y1 - padded.y0);
            // At or *past* the bound, never a hair under it: the derived
            // edge is floored precisely so the ratio does not round down
            // into a second row of tiles.
            assert!(
                w as f32 >= h as f32 * MAX_ASPECT,
                "{w}x{h} rounded to just under the bound"
            );
        }
    }

    #[test]
    fn padding_never_shrinks_the_region() {
        let region = IRect {
            x0: 10,
            y0: 20,
            x1: 210,
            y1: 60,
        };
        let padded = pad_to_aspect(region);
        assert!(padded.x1 - padded.x0 >= region.x1 - region.x0);
        assert!(padded.y1 - padded.y0 >= region.y1 - region.y0);
    }

    #[test]
    fn a_region_already_within_the_aspect_bound_is_not_padded() {
        let region = IRect {
            x0: 0,
            y0: 0,
            x1: 400,
            y1: 200,
        };
        let padded = pad_to_aspect(region);
        assert_eq!(padded.x1 - padded.x0, 400);
        assert_eq!(padded.y1 - padded.y0, 200);
    }

    #[test]
    fn the_render_scale_stays_inside_its_band() {
        let of = |width: f32, height: f32| {
            render_scale(&BoundingBox {
                x: 0.0,
                y: 0.0,
                width,
                height,
            })
        };
        assert_eq!(of(4.0, 4.0), MAX_RENDER_SCALE, "a tiny region is capped");
        assert_eq!(of(2000.0, 40.0), MIN_RENDER_SCALE, "a huge one is floored");
        let ordinary = of(400.0, 16.0);
        assert!(ordinary > MIN_RENDER_SCALE && ordinary < MAX_RENDER_SCALE);
    }

    // ── What the document marked out ─────────────────────────────────────────

    /// Every region a document marks out is rendered and read. There is no
    /// cap: the count is a report, not a gate.
    #[test]
    fn every_region_a_document_marks_out_is_kept_and_counted() {
        let region = || TypesetRegion {
            page: 1,
            kind: RegionKind::Formula,
            bbox: BoundingBox {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            words: Vec::new(),
        };
        let mut diagnostics = crate::types::ExtractionDiagnostics::default();
        let kept = counted((0..417).map(|_| region()).collect(), &mut diagnostics);
        assert_eq!(kept.len(), 417, "nothing is dropped");
        assert_eq!(diagnostics.typeset_regions_found, 417);
    }

    #[test]
    fn a_document_that_marks_out_nothing_counts_nothing() {
        let mut diagnostics = crate::types::ExtractionDiagnostics::default();
        let kept = counted(Vec::new(), &mut diagnostics);
        assert!(kept.is_empty());
        assert_eq!(diagnostics.typeset_regions_found, 0);
    }
}
