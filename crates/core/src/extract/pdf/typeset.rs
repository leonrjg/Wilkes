//! Regions the page *draws* rather than embeds, found from the page's own
//! typography.
//!
//! [`super::mupdf`] discovers embedded rasters, which is what the recognizer
//! has been fed until now. A LaTeX-typeset formula is not one of those: it is
//! glyphs from a math font placed by the page, so MuPDF reports it as text and
//! the reading gets the flattened glyph run — `ci = ai ⊕bi` — which is not
//! mathematics and which no consumer can parse back into any. This module is
//! the missing head of that path: it marks out the areas of a page that are
//! formulas and ruled tables, so the same recognizer, the same admission rules
//! and the same serialization can read them.
//!
//! ## The signal is the document's, not a guess about content
//!
//! Two structural facts are read off the page, and nothing else:
//!
//! 1. **Which font drew each glyph.** A typesetter that sets mathematics
//!    switches to a math font to do it — `CMMI`/`CMSY`/`CMEX` under TeX,
//!    `LatinModernMath` or `STIXTwoMath` under unicode-math, `Cambria Math`
//!    under Word. That switch is a declaration in the file, not an inference
//!    from the characters, which is what makes it usable: `n` in body text and
//!    `n` in an equation are the same character and different fonts.
//! 2. **Where the page drew rules.** A ruled table is bounded by thin wide
//!    filled rectangles, and MuPDF hands them over as vector blocks once it is
//!    asked to collect them.
//!
//! Neither is a layout *model*. FIGURE.md's phase three names a small ONNX
//! layout detector for this job; what is built here is the cheaper thing that
//! the document already tells us, and it is deliberately narrow. Its failures
//! are stated in [`formula_lines`] and [`table_regions`].
//!
//! ## Everything it finds fails safe
//!
//! A region marked out here is only a decision to *spend a recognizer call*.
//! Whether its answer replaces the page's glyphs is decided afterwards by the
//! same admission rules every other region meets — a formula on whether its
//! LaTeX closes, a table on whether it is rectangular. A false positive costs
//! time and changes no bytes: the region is refused and the glyph run the page
//! drew stays exactly where it was.

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
pub(super) const ROUTING_VERSION: &str = "typeset-routing-v2";

/// The share of a line's glyphs that must come from a math font for the line
/// to be part of a formula.
///
/// Set to separate *display* mathematics from prose that mentions a variable,
/// which is the separation that matters: a display equation is almost entirely
/// math-font glyphs, and a sentence like "for all i = 1, ..., n we have" is
/// two of them in twenty-five. On the reported case the two sides of the band
/// are roughly 0.9 (`ci = ai ⊕bi`, six of seven) and 0.08 (the sentence
/// above), and 0.5 sits between them with room on both sides.
///
/// Inline mathematics is deliberately *not* reachable at any threshold this
/// rule can express, because the unit is the line and an inline formula shares
/// its line with the prose around it. That is a cost decision as much as a
/// detection one: recognition is tens of seconds a region, and a document with
/// four hundred inline symbols cannot pay four hundred of those.
const MATH_GLYPH_SHARE: f32 = 0.5;

/// The fewest glyphs a line may have and still be a formula. A one- or
/// two-glyph line drawn in a math font is a list bullet or a stray symbol far
/// more often than it is an equation, and it is never an equation worth a
/// recognizer call.
const MIN_FORMULA_GLYPHS: usize = 3;

/// How far a line's glyph sizes, or its baselines, must spread before the line
/// counts as having carried structure that flattening destroyed. Points.
///
/// Half a point is comfortably below a real subscript — typeset at around
/// seventy percent of the body size, so three points apart on ten-point text —
/// and comfortably above the jitter of a typesetter placing glyphs on one
/// baseline. Measured as a spread rather than as a count of distinct values,
/// because two glyphs a hundredth of a point apart are on the same baseline
/// however they round.
const STRUCTURE_SPREAD_POINTS: f32 = 0.5;

/// Two formula lines join into one region when the gap between them is under
/// this many line heights. An aligned multi-line equation is one formula, and
/// reading its halves separately would produce two expressions neither of
/// which is what the page shows.
const LINE_GAP_FACTOR: f32 = 1.6;

/// A vector block is a rule if it is no thicker than this, in points. A
/// `\hline` or a booktabs rule is well under a point; anything thicker is a
/// filled box, a frame or a bar in a chart.
const RULE_MAX_THICKNESS: f32 = 2.5;

/// ...and no narrower than this. Half an inch: narrower marks are underlines,
/// fraction bars and radical rules, and a fraction bar is a formula's business
/// rather than a table's.
const RULE_MIN_WIDTH: f32 = 36.0;

/// The fewest rules that make a table. Three is the booktabs shape — top rule,
/// mid rule, bottom rule — and it is the smallest count that distinguishes a
/// table from a section divider or a header underline, both of which come
/// singly or in pairs.
const TABLE_MIN_RULES: usize = 3;

/// Two rules belong to the same table when they share this much of the
/// narrower one's width.
const RULE_X_OVERLAP: f32 = 0.6;

/// Rules further apart than this many points are not the same table.
const RULE_MAX_SPAN: f32 = 520.0;

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

/// The most regions one document may spend recognizer calls on.
///
/// Recognition is tens of seconds a region on a CPU, and a mathematics
/// textbook has more display equations than a reader will wait for. This is a
/// cost bound and not a correctness one: what it drops is counted in
/// [`crate::types::ExtractionDiagnostics::typeset_regions_over_budget`] and
/// logged, because a bounded run that reports nothing dropped reads exactly
/// like a document that had nothing more to find.
const MAX_REGIONS_PER_DOCUMENT: usize = 250;

// ── The survey ───────────────────────────────────────────────────────────────

/// What one page's drawing operations said about itself.
#[derive(Default)]
pub(super) struct Survey {
    /// The page-space origin of every glyph drawn in a math font.
    math_origins: Vec<Point>,
    /// Thin wide rectangles the page filled or stroked.
    rules: Vec<BoundingBox>,
    /// Every face the page drew text with, and how many glyphs each drew.
    ///
    /// Collected whether or not the face is mathematics, because the useful
    /// question when a document yields no formulas is *what it does draw
    /// with*. Without this, "this document has no mathematics" and "this
    /// build did not recognize the face its mathematics is set in" are the
    /// same silence — which is exactly how `DBAMWK+Formula` went unnoticed.
    pub(super) faces: BTreeMap<String, usize>,
}

/// A line of the page, as this module needs it: where it sits and how much of
/// it is mathematics.
pub(super) struct SurveyedLine {
    /// Index of the block this line belongs to, in the page's block list.
    pub block: usize,
    /// Index of the line within that block.
    pub line: usize,
    pub bbox: BoundingBox,
    pub glyphs: usize,
    pub math_glyphs: usize,
    /// Whether the page drew this line's glyphs at more than one size or on
    /// more than one baseline — a subscript, a superscript, a stacked
    /// fraction, the limits on a sum.
    ///
    /// This is the damage the whole feature exists to repair, measured
    /// directly. Reading a line out of the page's own drawing flattens it: `c`
    /// with a subscript `i` becomes `ci`, and the structure that said which
    /// was which is gone. A line with no such structure flattens to itself and
    /// has nothing to recover — `mod n` reads `mod n` either way.
    pub structure_flattened: bool,
}

impl SurveyedLine {
    /// Whether this line is a formula worth reading again.
    ///
    /// Three conditions, and the third is the one that matters. The
    /// typography says the line is mathematics; it is long enough not to be a
    /// stray symbol; and flattening it destroyed something.
    ///
    /// That last condition replaced a length floor, which was the wrong
    /// question asked in the wrong units. On the document that prompted this,
    /// a floor kept `ETAOINSRHDLUCMFYWGPBVKXQJZ` — a cipher alphabet, twenty-
    /// six glyphs, nothing to recover and a transcription that could only
    /// damage it — and dropped `c = me` at four. The structure test gets both
    /// right, and it does so by asking what the feature is for rather than by
    /// counting characters.
    fn is_formula(&self) -> bool {
        self.glyphs >= MIN_FORMULA_GLYPHS
            && (self.math_glyphs as f32) >= (self.glyphs as f32) * MATH_GLYPH_SHARE
            && self.structure_flattened
    }
}

/// One area of one page to render and read, and the lines it stands in place
/// of.
pub(super) struct TypesetRegion {
    pub page: u32,
    /// The area to render, already carrying its margin.
    pub bbox: BoundingBox,
    /// The surveyed lines this region covers, as `(block, line)` pairs. These
    /// are the page's own glyphs, and they leave the reading only if the
    /// recognizer's answer for this region is admitted.
    pub lines: Vec<(usize, usize)>,
}

impl Survey {
    /// How many of this line's glyphs the page drew in a math font.
    ///
    /// Counted by containment rather than by matching glyph to glyph: a glyph
    /// origin sits on the baseline at the left edge of its own glyph, so it
    /// falls inside the line box that glyph belongs to, and a line box is a
    /// narrow band that no neighbouring line's baseline reaches into. Matching
    /// individual glyphs would be exact — MuPDF computes both origins the same
    /// way — and would also break on the ligature expansion that gives a
    /// ligature's second half an interpolated origin.
    fn math_glyphs_within(&self, bbox: &BoundingBox) -> usize {
        self.math_origins
            .iter()
            .filter(|origin| {
                origin.x >= bbox.x
                    && origin.x <= bbox.x + bbox.width
                    && origin.y >= bbox.y
                    && origin.y <= bbox.y + bbox.height
            })
            .count()
    }

    /// Whether this page offered anything to mark a region out from. The
    /// faces are not part of the answer: they are collected for the report
    /// and a page drawn entirely in body text offers nothing.
    pub(super) fn is_empty(&self) -> bool {
        self.math_origins.is_empty() && self.rules.is_empty()
    }
}

/// Which fonts a document reserves for mathematics.
///
/// Matched on the family, after the subset tag an embedded font carries
/// (`ABCDEF+CMMI10`) and after punctuation and case are folded away. The list
/// is the TeX math families, the OpenType math fonts unicode-math ships with,
/// and the two Microsoft uses — plus anything whose family name simply
/// contains "math", which is the convention every OpenType math font follows
/// and which covers the ones not named here.
///
/// What it misses is a document that sets mathematics in a text font: some
/// publishers' PDFs re-encode everything into a single subsetted face, and
/// there is then no signal in the file to find. Nothing here guesses at that
/// case — a formula that leaves no typographic trace is left as the glyph run
/// it always was, which is today's behaviour.
fn is_math_font(name: &str) -> bool {
    let family: String = name
        .rsplit('+')
        .next()
        .unwrap_or(name)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    // Trailing design sizes: cmmi10, cmsy7, lmmathitalic12.
    let stem = family.trim_end_matches(|c: char| c.is_ascii_digit());

    // A face named for the job it does. Every OpenType math font follows the
    // first convention, and a publisher rolling its own follows one of the
    // others — the course book that prompted this ships `DBAMWK+Formula`,
    // which is as explicit a declaration as `LatinModernMath` and was missed
    // by a list that only knew TeX's names.
    if ["math", "formula", "equation"].iter().any(|word| stem.contains(word)) {
        return true;
    }
    const FAMILIES: &[&str] = &[
        // Computer Modern and Latin Modern maths.
        "cmmi", "cmsy", "cmex", "lmmi", "lmsy", "lmex",
        // AMS symbols, Euler, script, stmaryrd, wasy, blackboard.
        "msam", "msbm", "eufm", "eufb", "eurm", "eurb", "rsfs", "stmary", "wasy", "bbold",
        // Times/Symbol-based maths from older typesetters.
        "cmbsy", "cmmib", "esint", "mtextra", "symbol",
    ];
    FAMILIES.contains(&stem)
}

/// The device that answers "which font drew this glyph".
///
/// A device rather than the structured-text page because the safe structured-
/// text API exposes a character's box, size and origin but not its font, and
/// the font is the whole signal. Running the page a second time costs a page
/// parse — milliseconds against the tens of seconds a single recognizer call
/// takes — and buys the answer without reaching past the wrapper into raw
/// MuPDF structures.
#[derive(Default)]
struct Drawn {
    math_origins: Vec<Point>,
    faces: BTreeMap<String, usize>,
}

struct FontProbe(Rc<RefCell<Drawn>>);

impl FontProbe {
    /// A text-drawing operation, whichever entry point delivered it.
    ///
    /// `ctm` is the accumulated transform, and a glyph's item coordinates are
    /// its origin before it — MuPDF composes exactly this to place the glyph —
    /// so the origin in page space is the item mapped through `ctm`.
    ///
    /// Word spaces are skipped. A space is an item like any other and would be
    /// counted as a math glyph while the line it sits on counts only its
    /// visible characters, which put the share above one — harmless against a
    /// threshold, and a number that cannot be read.
    fn collect(&mut self, text: &Text, ctm: Matrix) {
        for span in text.spans() {
            let font = span.font();
            let name = font.name();
            let math = is_math_font(name);
            let mut drawn = self.0.borrow_mut();
            let mut glyphs = 0usize;
            for item in span.items() {
                if char::from_u32(item.ucs() as u32).is_some_and(char::is_whitespace) {
                    continue;
                }
                glyphs += 1;
                if !math {
                    continue;
                }
                let (x, y) = (item.x(), item.y());
                drawn.math_origins.push(Point {
                    x: ctm.a * x + ctm.c * y + ctm.e,
                    y: ctm.b * x + ctm.d * y + ctm.f,
                });
            }
            *drawn.faces.entry(name.to_string()).or_default() += glyphs;
        }
    }
}

impl NativeDevice for FontProbe {
    fn fill_text(
        &mut self,
        text: &Text,
        ctm: Matrix,
        _color_space: &Colorspace,
        _color: &[f32],
        _alpha: f32,
        _cp: ColorParams,
    ) {
        self.collect(text, ctm);
    }

    fn stroke_text(
        &mut self,
        text: &Text,
        _stroke_state: &mupdf::StrokeState,
        ctm: Matrix,
        _color_space: &Colorspace,
        _color: &[f32],
        _alpha: f32,
        _cp: ColorParams,
    ) {
        self.collect(text, ctm);
    }

    // `clip_text` and `ignore_text` are deliberately not collected: the first
    // paints through a mask and the second is the invisible text layer a
    // scanner writes under its own image. Neither is a glyph a reader sees,
    // and a formula is something the page shows.
}

/// Read one page's typography: which glyphs are mathematics and where the
/// rules are.
///
/// The text page is the one extraction already built, so the rules come from
/// the same object the reading does. The font probe is a second run of the
/// page, for the reason on [`FontProbe`].
pub(super) fn survey(page: &mupdf::Page, text_page: &mupdf::TextPage) -> Survey {
    let collected = Rc::new(RefCell::new(Drawn::default()));
    match Device::from_native(FontProbe(Rc::clone(&collected))) {
        Ok(device) => {
            if let Err(error) = page.run(&device, &Matrix::IDENTITY) {
                // A page that will not run is a page with no typographic
                // signal, not a failed extraction: the reading is already
                // built from the structured text and stands without this.
                warn!("typeset survey: page would not run: {error}");
            }
        }
        Err(error) => warn!("typeset survey: no device: {error}"),
    }

    let mut rules = Vec::new();
    for block in text_page.blocks() {
        if block.r#type() != TextBlockType::Vector {
            continue;
        }
        let bounds = block.bounds();
        let (width, height) = (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
        if height > RULE_MAX_THICKNESS || width < RULE_MIN_WIDTH {
            continue;
        }
        rules.push(BoundingBox {
            x: bounds.x0,
            y: bounds.y0,
            width,
            height,
        });
    }

    // Taken by clone rather than by unwrapping the handle: whether MuPDF has
    // finished with its copy of the device is its business, and a failed
    // unwrap here would return an empty survey — which reads exactly like a
    // page with no mathematics on it.
    let drawn = collected.borrow();
    Survey {
        math_origins: drawn.math_origins.clone(),
        faces: drawn.faces.clone(),
        rules,
    }
}

/// Measure one line against the survey.
///
/// `sizes` and `baselines` are the font size and the baseline `y` of every
/// glyph on the line, in the order they were drawn; their spread is what says
/// whether the line carried structure the reading flattened.
pub(super) fn surveyed_line(
    survey: &Survey,
    block: usize,
    line: usize,
    bbox: BoundingBox,
    glyphs: usize,
    sizes: &[f32],
    baselines: &[f32],
) -> SurveyedLine {
    let math_glyphs = survey.math_glyphs_within(&bbox);
    SurveyedLine {
        block,
        line,
        bbox,
        glyphs,
        math_glyphs,
        structure_flattened: spread(sizes) > STRUCTURE_SPREAD_POINTS
            || spread(baselines) > STRUCTURE_SPREAD_POINTS,
    }
}

fn spread(values: &[f32]) -> f32 {
    let (mut low, mut high) = (f32::INFINITY, f32::NEG_INFINITY);
    for value in values {
        low = low.min(*value);
        high = high.max(*value);
    }
    if low > high {
        return 0.0;
    }
    high - low
}

// ── Marking out regions ─────────────────────────────────────────────────────

/// The formula regions of one page: runs of adjacent lines the typography says
/// are mathematics.
///
/// Adjacent lines join because an aligned equation is one formula and its
/// halves are not two. Adjacency is vertical and measured in line heights, so
/// a second equation further down the page is its own region.
///
/// What this does not find: inline mathematics, for the reason on
/// [`MATH_GLYPH_SHARE`]; an equation number set in a text font beside a
/// display equation, which sits on the same line and lowers its share; and any
/// formula in a document that sets mathematics without switching fonts.
fn formula_lines(lines: &[SurveyedLine], taken: &[bool]) -> Vec<Vec<usize>> {
    let mut regions: Vec<Vec<usize>> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if taken[index] || !line.is_formula() {
            continue;
        }
        let joins = regions.last().is_some_and(|last: &Vec<usize>| {
            let previous = &lines[*last.last().expect("a region has a line")];
            // Consecutive in the survey, and close enough vertically that the
            // page set them as one displayed block.
            *last.last().expect("a region has a line") + 1 == index
                && line.bbox.y - (previous.bbox.y + previous.bbox.height)
                    < previous.bbox.height.max(1.0) * LINE_GAP_FACTOR
        });
        match (joins, regions.last_mut()) {
            (true, Some(last)) => last.push(index),
            _ => regions.push(vec![index]),
        }
    }
    regions
}

/// The table regions of one page: stacks of rules, and whatever lines they
/// enclose.
///
/// Rules are grouped when they share most of their width and lie within one
/// page's worth of vertical span; three of them is the booktabs shape. The
/// region is the union of the rules with every line whose box falls between
/// the topmost and the bottommost, which is what makes the caption above a
/// table stay out of it and the body rows come in.
///
/// What this does not find: a table ruled only on its outer frame, and an
/// unruled table — a tabular set with white space alone leaves no vector trace
/// at all, and finding it is what a layout model would be for.
fn table_regions(rules: &[BoundingBox], lines: &[SurveyedLine]) -> Vec<(BoundingBox, Vec<usize>)> {
    let mut ordered: Vec<&BoundingBox> = rules.iter().collect();
    ordered.sort_by(|a, b| a.y.total_cmp(&b.y));

    let mut groups: Vec<Vec<&BoundingBox>> = Vec::new();
    for rule in ordered {
        let joined = groups.iter_mut().find(|group| {
            let first = group[0];
            let overlap = (first.x + first.width).min(rule.x + rule.width) - first.x.max(rule.x);
            overlap >= first.width.min(rule.width) * RULE_X_OVERLAP
                && rule.y - first.y <= RULE_MAX_SPAN
        });
        match joined {
            Some(group) => group.push(rule),
            None => groups.push(vec![rule]),
        }
    }

    groups
        .into_iter()
        .filter(|group| group.len() >= TABLE_MIN_RULES)
        .map(|group| {
            let hull = group
                .iter()
                .skip(1)
                .fold((*group[0]).clone(), |hull, rule| hull.merge(rule));
            let covered = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| {
                    let centre = line.bbox.y + line.bbox.height / 2.0;
                    centre >= hull.y && centre <= hull.y + hull.height
                })
                // A rule stack with no text between the rules is a set of
                // dividers, not a table.
                .map(|(index, _)| index)
                .collect::<Vec<usize>>();
            (hull, covered)
        })
        .filter(|(_, covered)| !covered.is_empty())
        .collect()
}

/// Everything on one page worth rendering for the recognizer.
///
/// Tables are marked out first and take their lines with them, because a
/// table's cells may themselves be mathematics: the same lines cannot belong
/// to two regions, and the table is the larger truth about them.
pub(super) fn regions(page: u32, survey: &Survey, lines: &[SurveyedLine]) -> Vec<TypesetRegion> {
    let mut taken = vec![false; lines.len()];
    let mut found: Vec<TypesetRegion> = Vec::new();

    for (hull, covered) in table_regions(&survey.rules, lines) {
        for index in &covered {
            taken[*index] = true;
        }
        found.push(TypesetRegion {
            page,
            bbox: with_margin(&hull),
            lines: covered
                .iter()
                .map(|index| (lines[*index].block, lines[*index].line))
                .collect(),
        });
    }

    for group in formula_lines(lines, &taken) {
        let hull = group
            .iter()
            .skip(1)
            .fold(lines[group[0]].bbox.clone(), |hull, index| {
                hull.merge(&lines[*index].bbox)
            });
        found.push(TypesetRegion {
            page,
            bbox: with_margin(&hull),
            lines: group
                .iter()
                .map(|index| (lines[*index].block, lines[*index].line))
                .collect(),
        });
    }

    // Reading order, so a document's regions are numbered down the page
    // whichever rule marked them out.
    found.sort_by(|a, b| a.bbox.y.total_cmp(&b.bbox.y));
    found
}

fn with_margin(bbox: &BoundingBox) -> BoundingBox {
    BoundingBox {
        x: bbox.x - MARGIN_POINTS,
        y: bbox.y - MARGIN_POINTS,
        width: bbox.width + MARGIN_POINTS * 2.0,
        height: bbox.height + MARGIN_POINTS * 2.0,
    }
}

/// Say what the document draws with, and which of it was read as mathematics.
///
/// At info when nothing was — because that is the case a reader needs to see.
/// A document whose mathematics is set in a face this build does not know
/// yields no formulas and looks exactly like a document with no mathematics
/// in it, and the difference is the whole of whether this feature worked. The
/// face names are the evidence, and they are only in the file.
pub(super) fn report_faces(path: &std::path::Path, faces: &BTreeMap<String, usize>) {
    if faces.is_empty() {
        return;
    }
    let mut ordered: Vec<(&String, &usize)> = faces.iter().collect();
    ordered.sort_by_key(|(_, glyphs)| std::cmp::Reverse(**glyphs));
    let named = |mathematics: bool| {
        ordered
            .iter()
            .filter(|(name, _)| is_math_font(name) == mathematics)
            .map(|(name, glyphs)| format!("{name} ({glyphs})"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mathematics = named(true);
    if mathematics.is_empty() {
        tracing::info!(
            "typeset routing in {:?}: no face is known to this build as mathematics, so no \
             formula was marked out. The document draws with: {}",
            path,
            named(false)
        );
    } else {
        tracing::debug!(
            "typeset routing in {:?}: mathematics is set in {mathematics}; the rest is {}",
            path,
            named(false)
        );
    }
}

/// Trim a document's regions to what it may spend, reporting what it cost.
pub(super) fn within_budget(
    regions: Vec<TypesetRegion>,
    diagnostics: &mut crate::types::ExtractionDiagnostics,
) -> Vec<TypesetRegion> {
    diagnostics.typeset_regions_found = regions.len() as u32;
    if regions.len() <= MAX_REGIONS_PER_DOCUMENT {
        return regions;
    }
    let dropped = regions.len() - MAX_REGIONS_PER_DOCUMENT;
    diagnostics.typeset_regions_over_budget = dropped as u32;
    warn!(
        "typeset routing: {} regions found, {MAX_REGIONS_PER_DOCUMENT} rendered, \
         {dropped} left unread — the per-document budget was reached, and those \
         areas keep the page's own glyphs in the reading",
        regions.len()
    );
    regions.into_iter().take(MAX_REGIONS_PER_DOCUMENT).collect()
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
    let (width, height) = ((canvas.x1 - canvas.x0) as u32, (canvas.y1 - canvas.y0) as u32);
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
        (width, ((width as f32 / MAX_ASPECT).floor() as i32).max(height))
    } else if height as f32 > width as f32 * MAX_ASPECT {
        (((height as f32 / MAX_ASPECT).floor() as i32).max(width), height)
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
        });
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A surveyed line that carried structure — the ordinary case for a
    /// display equation, whose subscripts are what flattening destroyed.
    fn line(block: usize, index: usize, y: f32, glyphs: usize, math: usize) -> SurveyedLine {
        flat_line(block, index, y, glyphs, math, true)
    }

    fn flat_line(
        block: usize,
        index: usize,
        y: f32,
        glyphs: usize,
        math: usize,
        structure_flattened: bool,
    ) -> SurveyedLine {
        SurveyedLine {
            block,
            line: index,
            bbox: BoundingBox {
                x: 100.0,
                y,
                width: 300.0,
                height: 12.0,
            },
            glyphs,
            math_glyphs: math,
            structure_flattened,
        }
    }

    fn rule(y: f32, x: f32, width: f32) -> BoundingBox {
        BoundingBox {
            x,
            y,
            width,
            height: 0.4,
        }
    }

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
            // the face the reported document sets its mathematics in, and the
            // case that the TeX-only list missed.
            "DBAMWK+Formula",
            "Formula",
            "EquationFont",
        ] {
            assert!(is_math_font(name), "{name} should be a math font");
        }
    }

    /// The text fonts a document sets its prose in, including the ones whose
    /// names sit next to a math family in the same TeX distribution.
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
            // The rest of the reported document's faces, which must not be
            // dragged in by a rule loose enough to catch its `Formula`.
            "DBAMWK+SourceSans",
            "DBAMWK+SourceSansBold",
            "DBAMWK+SourceCode",
            // A real text typeface whose name starts like "formula".
            "Formata-Regular",
        ] {
            assert!(!is_math_font(name), "{name} should not be a math font");
        }
    }

    // ── Formula lines ────────────────────────────────────────────────────

    /// The reported case: a display line that is almost all math glyphs is a
    /// formula, and the sentence above it — which mentions two variables — is
    /// not.
    #[test]
    fn a_display_line_is_a_formula_and_a_sentence_mentioning_variables_is_not() {
        let lines = vec![
            line(0, 0, 100.0, 25, 2), // "for all i = 1, ..., n we have"
            line(0, 1, 130.0, 8, 7),  // "ci = ai ⊕ bi"
        ];
        let regions = formula_lines(&lines, &[false, false]);
        assert_eq!(regions.len(), 1, "one formula on the page");
        assert_eq!(regions[0], vec![1]);
    }

    /// The rule that decides whether a line is worth reading again is not its
    /// length but whether flattening it lost anything. `mod n` reads `mod n`
    /// either way; `ci = ai ⊕bi` lost two subscripts.
    #[test]
    fn a_line_that_flattened_to_itself_is_not_read_again() {
        let flat = flat_line(0, 0, 100.0, 8, 8, false);
        assert!(!flat.is_formula(), "nothing was destroyed, so nothing to repair");
        let structured = flat_line(0, 0, 100.0, 8, 8, true);
        assert!(structured.is_formula());
    }

    /// A long run in a math face with nothing stacked in it is still not a
    /// formula. The document that prompted this sets a cipher alphabet —
    /// twenty-six letters, no structure — in the same face as its equations,
    /// and a transcription of it could only be worse than the glyphs.
    #[test]
    fn a_long_flat_run_in_a_math_face_is_not_a_formula() {
        assert!(!flat_line(0, 0, 100.0, 26, 26, false).is_formula());
    }

    /// The spread is measured, not the count of distinct values: two glyphs a
    /// hundredth of a point apart sit on one baseline however they round.
    #[test]
    fn jitter_along_one_baseline_is_not_structure() {
        let survey = Survey::default();
        let bbox = BoundingBox { x: 0.0, y: 0.0, width: 100.0, height: 12.0 };
        let jittered = surveyed_line(
            &survey,
            0,
            0,
            bbox.clone(),
            4,
            &[10.0, 10.0, 10.01, 10.0],
            &[100.0, 100.02, 100.0, 99.99],
        );
        assert!(!jittered.structure_flattened);

        let subscripted = surveyed_line(
            &survey,
            0,
            0,
            bbox,
            4,
            &[10.0, 7.0, 10.0, 7.0],
            &[100.0, 102.0, 100.0, 102.0],
        );
        assert!(subscripted.structure_flattened);
    }

    #[test]
    fn a_line_too_short_to_be_an_equation_is_not_one() {
        let lines = vec![line(0, 0, 100.0, 2, 2)];
        assert!(formula_lines(&lines, &[false]).is_empty());
    }

    /// An aligned equation is one formula. Reading its halves apart would
    /// produce two expressions, neither of which is what the page shows.
    #[test]
    fn adjacent_formula_lines_are_one_region() {
        let lines = vec![
            line(0, 0, 100.0, 8, 8),
            line(0, 1, 116.0, 8, 8),
            line(0, 2, 132.0, 8, 8),
        ];
        let regions = formula_lines(&lines, &[false; 3]);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], vec![0, 1, 2]);
    }

    /// A second equation further down the page is its own region: the gap
    /// says the page set them as two displayed blocks.
    #[test]
    fn formulas_separated_by_prose_are_separate_regions() {
        let lines = vec![
            line(0, 0, 100.0, 8, 8),
            line(0, 1, 140.0, 30, 1),
            line(0, 2, 180.0, 8, 8),
        ];
        let regions = formula_lines(&lines, &[false; 3]);
        assert_eq!(regions.len(), 2);
    }

    #[test]
    fn a_line_already_taken_by_a_table_is_not_also_a_formula() {
        let lines = vec![line(0, 0, 100.0, 8, 8)];
        assert!(formula_lines(&lines, &[true]).is_empty());
    }

    // ── Table rules ──────────────────────────────────────────────────────

    #[test]
    fn three_stacked_rules_with_rows_between_them_are_a_table() {
        let rules = vec![
            rule(100.0, 100.0, 300.0),
            rule(130.0, 100.0, 300.0),
            rule(200.0, 100.0, 300.0),
        ];
        let lines = vec![line(0, 0, 110.0, 12, 0), line(0, 1, 150.0, 14, 0)];
        let found = table_regions(&rules, &lines);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, vec![0, 1], "both rows are inside the table");
        assert!(found[0].0.height >= 100.0, "{:?}", found[0].0);
    }

    /// A section divider and a header underline come singly or in pairs.
    #[test]
    fn fewer_than_three_rules_are_not_a_table() {
        let rules = vec![rule(100.0, 100.0, 300.0), rule(130.0, 100.0, 300.0)];
        let lines = vec![line(0, 0, 110.0, 12, 0)];
        assert!(table_regions(&rules, &lines).is_empty());
    }

    #[test]
    fn a_rule_stack_enclosing_no_text_is_not_a_table() {
        let rules = vec![
            rule(100.0, 100.0, 300.0),
            rule(130.0, 100.0, 300.0),
            rule(160.0, 100.0, 300.0),
        ];
        assert!(table_regions(&rules, &[]).is_empty());
    }

    /// Rules in two different columns are two different things, whatever
    /// their count.
    #[test]
    fn rules_that_do_not_share_a_column_are_not_one_table() {
        let rules = vec![
            rule(100.0, 60.0, 200.0),
            rule(130.0, 60.0, 200.0),
            rule(160.0, 330.0, 200.0),
        ];
        let lines = vec![line(0, 0, 110.0, 12, 0)];
        assert!(table_regions(&rules, &lines).is_empty());
    }

    // ── Regions ──────────────────────────────────────────────────────────

    /// A table's cells may themselves be mathematics. The same lines cannot
    /// belong to two regions, and the table is the larger truth about them.
    #[test]
    fn a_table_claims_its_lines_before_the_formula_rule_sees_them() {
        let survey = Survey {
            math_origins: Vec::new(),
            faces: BTreeMap::new(),
            rules: vec![
                rule(100.0, 100.0, 300.0),
                rule(130.0, 100.0, 300.0),
                rule(200.0, 100.0, 300.0),
            ],
        };
        let lines = vec![line(0, 0, 110.0, 8, 8), line(0, 1, 150.0, 8, 8)];
        let found = regions(3, &survey, &lines);
        assert_eq!(found.len(), 1, "one region, not a table and two formulas");
        assert_eq!(found[0].lines, vec![(0, 0), (0, 1)]);
        assert_eq!(found[0].page, 3);
    }

    #[test]
    fn a_region_carries_a_margin_around_what_it_covers() {
        let survey = Survey {
            math_origins: Vec::new(),
            faces: BTreeMap::new(),
            rules: Vec::new(),
        };
        let lines = vec![line(0, 0, 100.0, 8, 8)];
        let found = regions(1, &survey, &lines);
        assert_eq!(found.len(), 1);
        assert!(found[0].bbox.x < 100.0 && found[0].bbox.y < 100.0);
        assert!(found[0].bbox.width > 300.0 && found[0].bbox.height > 12.0);
    }

    // ── Geometry handed to the recognizer ────────────────────────────────

    fn rect(width: i32, height: i32) -> IRect {
        IRect {
            x0: 100,
            y0: 200,
            x1: 100 + width,
            y1: 200 + height,
        }
    }

    fn ratio(r: IRect) -> f32 {
        (r.x1 - r.x0) as f32 / (r.y1 - r.y0) as f32
    }

    /// A display formula is a sliver, and a recognizer that resizes it onto a
    /// square-ish grid would stretch every glyph. Padding is what keeps the
    /// shapes.
    #[test]
    fn a_sliver_is_padded_rather_than_left_to_be_squashed() {
        let padded = pad_to_aspect(rect(1600, 56));
        assert!((ratio(padded) - MAX_ASPECT).abs() < 0.05, "{padded:?}");
        // Symmetric: the formula stays where it was, centred in the pad.
        assert_eq!(padded.x0, 100, "the long edge is untouched");
        assert_eq!(
            (padded.y0 + padded.y1) / 2,
            200 + 28,
            "the region stays centred"
        );
    }

    /// The property that costs tiles. A recognizer rounds the canvas up to
    /// whole tiles, so a ratio a hair *under* the bound buys a second row of
    /// them — the same picture for nearly twice the prefill. Padding must
    /// never land there, at any size.
    #[test]
    fn a_padded_sliver_never_lands_just_under_the_bound() {
        for width in [401, 999, 1409, 1410, 1411, 1412, 2825, 4097] {
            for height in [17, 41, 100, 225, 353] {
                let padded = pad_to_aspect(rect(width, height));
                let ratio = ratio(padded);
                let lopsided = width as f32 > height as f32 * MAX_ASPECT
                    || height as f32 > width as f32 * MAX_ASPECT;
                assert!(
                    !lopsided || ratio >= MAX_ASPECT || ratio <= 1.0 / MAX_ASPECT,
                    "{width}x{height} padded to {padded:?}, ratio {ratio}"
                );
            }
        }
    }

    /// Padding grows the canvas and never crops it: the region has to survive
    /// whole, or the clip would cut the formula it was marked out for.
    #[test]
    fn padding_never_shrinks_the_region() {
        for (width, height) in [(1600, 56), (56, 1600), (300, 200), (1, 1), (4000, 3)] {
            let region = rect(width, height);
            let padded = pad_to_aspect(region);
            assert!(
                padded.x0 <= region.x0
                    && padded.y0 <= region.y0
                    && padded.x1 >= region.x1
                    && padded.y1 >= region.y1,
                "{width}x{height}: {padded:?} does not contain {region:?}"
            );
        }
    }

    #[test]
    fn a_region_already_within_the_aspect_bound_is_not_padded() {
        let region = rect(300, 200);
        assert_eq!(pad_to_aspect(region), region);
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

    // ── The budget ───────────────────────────────────────────────────────

    /// A bounded run that reports nothing dropped reads exactly like a
    /// document that had nothing more to find.
    #[test]
    fn the_budget_reports_what_it_dropped() {
        let region = || TypesetRegion {
            page: 1,
            bbox: BoundingBox {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            lines: Vec::new(),
        };
        let mut diagnostics = crate::types::ExtractionDiagnostics::default();
        let kept = within_budget(
            (0..MAX_REGIONS_PER_DOCUMENT + 7).map(|_| region()).collect(),
            &mut diagnostics,
        );
        assert_eq!(kept.len(), MAX_REGIONS_PER_DOCUMENT);
        assert_eq!(
            diagnostics.typeset_regions_found,
            MAX_REGIONS_PER_DOCUMENT as u32 + 7
        );
        assert_eq!(diagnostics.typeset_regions_over_budget, 7);
    }

    #[test]
    fn a_document_within_the_budget_drops_nothing() {
        let mut diagnostics = crate::types::ExtractionDiagnostics::default();
        let kept = within_budget(Vec::new(), &mut diagnostics);
        assert!(kept.is_empty());
        assert_eq!(diagnostics.typeset_regions_over_budget, 0);
    }
}

