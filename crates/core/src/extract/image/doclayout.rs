//! The layout detector: what a page holds, and where.
//!
//! PP-DocLayoutV2 as an ONNX graph, run on the `ort` already in the tree.
//! It answers one question — for each area of a rendered page, which of
//! twenty-five document classes it is — and it answers it from the picture,
//! which is what makes it the right instrument for a question the file itself
//! does not declare.
//!
//! ## What it replaced
//!
//! Until now [`crate::extract::pdf`] marked out formulas by reading the font
//! each glyph was drawn in and tables by finding the rules the page stroked.
//! Both worked on the documents they were written against and neither
//! generalized: the font rule needed a list of face names and could not see
//! inline mathematics at all, because its unit was the line and an inline
//! formula shares its line with prose; the rule stack found booktabs tables
//! and missed every unruled one. Those were the heuristics. They are gone —
//! this is the one mechanism, and when it is not installed nothing is marked
//! out rather than something being guessed.
//!
//! ## How a page is looked at
//!
//! Five forward passes, all of them 800x800, because that is the only input
//! size the graph has. What differs is how much page is in one:
//!
//! - **The page pass.** The 1600x1600 render the host hands over, downsampled
//!   back to 800. Every class is read from this one, and it is the *only*
//!   source for the twenty-three that are not formulas: a table, a figure
//!   title or a header is a judgement about a page, and a quarter of a page is
//!   not a page.
//! - **Four formula tiles.** The same render cut into 800x800 quarters and fed
//!   at its own resolution, with only `display_formula` and `inline_formula`
//!   kept. An expression that was 6 px wide in the old input is 12 px wide
//!   here, which is the whole of the idea: an inline expression has to reach
//!   the detector at a size it can propose.
//!
//! What comes back is reconciled here — boxes two tiles both saw are one box,
//! the two halves of an expression a seam cut are one box, and an area
//! proposed as both formula classes is one crop.
//!
//! ## What it costs
//!
//! One 1600 px page render and five forward passes. Measured at 1,141 ms a
//! page over the 28-page `formula_recall` fixture on this project's reference
//! machine, against 236 ms for the single whole-page pass it replaced — so
//! roughly forty-five minutes for a 2,300-page library rather than nine,
//! against the hours the recognizer then takes on what was marked out. It
//! buys, against that baseline at each one's own operating point:
//!
//! ```text
//! kind      labels   800px whole, 0.50     1600px + 4 tiles, 0.44
//!                    rec  cov  prec        rec  cov  prec
//! inline      782    37%  49%   80%        45%  61%   71%
//! display      70    81%  94%   88%        86%  94%   82%
//! ```
//!
//! and by how wide the expression is in the graph's own 800 px square, which
//! is where the gain is (recall / coverage):
//!
//! ```text
//! width     labels   800px whole     1600px + 4 tiles
//!    <8px      209   0% /  10%       0% /  14%
//!  8-16px      178  29% /  40%      30% /  49%
//! 16-32px      119  55% /  72%      65% /  88%
//! 32-64px      175  54% /  70%      76% /  92%
//! 64-128px     107  74% /  81%      81% /  92%
//!  >=128px      64  88% /  92%      94% /  95%
//! ```
//!
//! ### What in that is the tiles
//!
//! Less than the headline. Three changes are stacked in it, and they were
//! measured apart, all at 0.44 so the threshold is not one of the variables:
//!
//! ```text
//!                                    ms/page   inline cov   inline prec
//! 800 px, rendered direct               236        52%          78%
//! 1600 px, sampled back down to 800     230        56%          74%
//! 1600 px + four formula tiles         1141        61%          71%
//! ```
//!
//! Rendering at 1600 and taking every second pixel is four points of coverage
//! for *nothing* — it is marginally faster than rasterizing at 800, because
//! mupdf's antialiasing is the expensive part and nearest-neighbour sampling
//! of a sharp raster keeps thin strokes a filtered reduction smears away. It
//! also lifts display recall 81% to 86%, which is the one number the tiles do
//! not touch. The four tiles are the other five points, and they are what the
//! 900 ms buys.
//!
//! ### What of that reaches the recognizer
//!
//! Less again, and for a reason that is not this module's. A detection becomes
//! a crop only if `extract::pdf::typeset::regions` finds it a place in the
//! page's reading, and the same fixture scored one stage further down — at the
//! crop, not the proposal — reads (needs_recognizer labels, of 555):
//!
//! ```text
//!                          proposed   cropped   cleanly   crops/page
//! 800 px, whole page          79%       78%       63%        15.6
//! 1600 px, whole page         83%       82%       66%        16.8
//! 1600 px + four tiles        90%       88%       71%        18.3
//! ```
//!
//! The tiles' gain survives the stage nearly whole — eleven points proposed,
//! ten points cropped — so the 900 ms buys what it looked like it bought.
//!
//! The gap between the two columns was seventeen points until a region was
//! allowed to speak for no words at all: 88% of what went missing there were
//! expressions the page *draws* rather than sets, with no glyph run underneath
//! for a region's reading to replace. Those are anchored into the reading now
//! rather than dropped, which is where the second column's fifteen points came
//! from and why a page costs three more crops. What is left is two points, and
//! about half of it is the nesting this module leaves open — see
//! [`ABSORB_SHARE`].
//!
//! ### What nothing here fixes
//!
//! Under 8 px: 209 labels, 0% recall before and after, at every threshold, at
//! 3x as well as 2x. Those are lone italic variables — `X`, `n`, `p` — and the
//! detector does not propose them because it does not hold them to be
//! formulas, not because they were too small to see. Resolution was the wrong
//! hypothesis for that quarter of the fixture's inline labels, and the next
//! thing tried should not be more of it.
//!
//! The graph is 204 MB of fp32.
//!
//! ## What it does not decide
//!
//! Whether a marked-out area's reading reaches the document. That is settled
//! afterwards by the same admission rules as before: a formula on whether its
//! LaTeX parses, a table on whether it is rectangular, and — for every typeset
//! region — on whether the kind is one worth displacing the page's own glyphs
//! for. A false positive here still costs time and changes no bytes.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use image::RgbImage;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use tracing::debug;

use super::LayoutRegion;
use crate::types::{BoundingBox, RegionKind};

/// The detector's id, as a reading records it.
pub const MODEL_ID: &str = "PP-DocLayoutV2";

const REPO: &str = "alex-dinh/PP-DocLayoutV2-ONNX";

/// Pinned to a commit rather than to `main`. The graph's class order *is* the
/// contract for [`LABELS`], and a branch that moved under us would silently
/// re-label every region in the library.
const REVISION: &str = "5e30a2650d087e23af3a8084d42bd30d135af771";

const GRAPH: &str = "PP-DocLayoutV2.onnx";
const CONFIG: &str = "config.json";

pub const ARTIFACTS: &[&str] = &[GRAPH, CONFIG];

/// Size and digest of each artifact, in `ARTIFACTS` order. Read from the
/// pinned revision; a file that does not match is deleted rather than used.
const DIGESTS: &[(u64, &str)] = &[
    (
        213_963_712,
        "2009fcb35e64085ab9f6f2b27aca550edc29a040a24f7d6a0f05b74a2f804860",
    ),
    (
        4_535,
        "e1a25a0cbdc59c3668bc96467ecca6b9747eca29ff26d90282f452adc89df21f",
    ),
];

/// The square the graph declares. Every forward pass is exactly this, and no
/// pass is ever anything else.
///
/// The model's own `Preprocess` block says `Resize` to 800x800 with
/// `keep_ratio: false`, so the page is stretched into the square rather than
/// letterboxed and the map back is two independent scales. Following the
/// model's preprocessing rather than improving on it: a detector fed pixels
/// laid out differently from its training is a detector answering a different
/// question.
///
/// It is no longer the size a *page* is rasterized to; that is
/// [`RENDER_SIDE`], and the two are separate because how much page reaches the
/// graph in one square is a choice, while the square is not.
const SIDE: u32 = 800;

/// The square a *page* is rasterized to, which is no longer the graph's own.
///
/// The graph still sees 800x800 and only ever sees 800x800. What changed is
/// how much page is in one of those squares: at 2x the host hands over a
/// 1600x1600 render, the whole-page pass downsamples it back to 800 for the
/// classes that need page context, and the formula classes additionally get
/// four 800x800 tiles of the render at its own resolution — so an inline
/// expression that was 6 px wide in the old input is 12 px wide in the input
/// the formula pass actually sees.
///
/// 2x and not 3x, and 2x2 and not 3x3. Three tilings were measured on the
/// 28-page fixture, each read at *its own* loosest threshold that still holds
/// inline precision at 70% — comparing them at one shared threshold would be
/// comparing two of them at an operating point they would not have chosen:
///
/// ```text
/// render/stride   passes   ms/page   thr    inline cov   inline prec
/// 1600 / 800         5       1141    0.44       61%          71%
/// 1600 / 400        10       2296    0.46       60%          70%
/// 2400 / 800        10       2308    0.46       60%          70%
/// ```
///
/// The cheapest of the three is also the best of the three. Doubling the
/// passes either way *loses* a point: a 3x3 tiling of the same render pays
/// nine passes to avoid the seams that [`joins`] already puts back together,
/// and a 3x render pays nine passes for pixels the graph does not use. And
/// under 8 px — the bucket the extra resolution was for — 3x recalls nothing
/// 2x did not, which is the measurement saying the remaining misses are not a
/// resolution problem at all. See the module header.
const RENDER_SIDE: u32 = 1600;

/// How far apart the formula tiles start, in render pixels.
///
/// `SIDE` is a 2x2 tiling with no overlap: four passes, and an expression a
/// seam cuts is put back together by [`joins`] rather than by an overlapping
/// tile that happened to contain it — which is the cheaper of the two ways to
/// not lose it, by the table on [`RENDER_SIDE`].
const TILE_STRIDE: u32 = SIDE;

/// How the whole-page pass gets from [`RENDER_SIDE`] down to [`SIDE`].
///
/// Named as a constant, and named again in [`identity`], because it changes
/// what the detector sees and therefore what it says — see [`cut`] for the
/// measurement. A resampling filter is not the sort of thing that looks like
/// part of a recipe, which is exactly why it is written into one: changing it
/// silently would leave a library half-read one way and half the other.
const DOWNSAMPLE: image::imageops::FilterType = image::imageops::FilterType::Nearest;

/// [`DOWNSAMPLE`] as the identity spells it.
const DOWNSAMPLE_NAME: &str = "nearest";

/// The score a detection must reach to be believed.
///
/// The model's own `draw_threshold`. Not tuned here — a threshold this module
/// picked for itself would be exactly the kind of unexplained constant this
/// detector exists to remove. It governs every class but the two formula
/// classes, which have their own; see [`FORMULA_THRESHOLD`].
pub const SCORE_THRESHOLD: f32 = 0.5;

/// The score a `display_formula` or `inline_formula` detection must reach.
///
/// Lower than [`SCORE_THRESHOLD`] and picked by measurement rather than
/// inherited: the model's `draw_threshold` is one number for twenty-five
/// classes, and the two this pipeline actually routes are the two it is least
/// confident about.
///
/// Swept on the fixture against the cost of being wrong — a false positive is
/// a 0.3 s Texify call and a chance that admission accepts a wrong reading of
/// prose — and set at the loosest point that still holds inline precision at
/// the 70% floor. On the production recipe, inline coverage and precision run
/// 58%/73% at 0.50, 59%/72% at 0.46, 61%/71% at 0.44, 62%/69% at 0.42 and
/// 63%/67% at 0.40. 0.44 is the last point on the floor; below it nearly a
/// third of the crops the recognizer is handed are prose.
pub const FORMULA_THRESHOLD: f32 = 0.44;

/// The score the whole-page pass must reach on a formula class before the four
/// formula tiles are run at all.
///
/// The tiles are four fifths of the detector's cost and they exist for one
/// thing: an inline expression too small for the whole-page pass to propose
/// *at its own threshold*. A page with no mathematics on it pays for them and
/// gets nothing, and a book is mostly such pages.
///
/// The number is not [`FORMULA_THRESHOLD`] and could not be: the pages the
/// tiles are for are exactly the pages the whole-page pass does not believe.
/// What it is, is the floor below which the whole-page pass is saying nothing
/// at all — the graph returns every one of its queries with a score, and on a
/// page that draws no mathematics the best a formula query manages is a small
/// number rather than zero. So the gate reads the *unthresholded* best formula
/// score off the pass it has already paid for, which is
/// [`best_formula_score`], and compares it to this.
///
/// Set from the 28-page `formula_recall` fixture, `perf_profile gate`, which
/// prints [`best_formula_score`] for every page beside what the page holds.
/// The two populations do not overlap:
///
/// ```text
///                             pages   lowest   highest
/// carries a formula label        25   0.3884    0.9664
/// carries none                    3   0.0312    0.0519
/// ```
///
/// The single labelled page anywhere near the floor is `MMET02-01_E#21` at
/// 0.3884 — the page the whole-page pass believes *least*, and the one the
/// tiles matter most for. Every other labelled page is above 0.8485. So there
/// is a clear gap, 0.0519 to 0.3884, and the floor is its geometric middle
/// rounded to two places: 2.9x the highest unlabelled page, 0.39x the lowest
/// labelled one.
///
/// The middle and not the edge, because the two errors are not symmetric. A
/// floor too high closes the gate on a page that has mathematics and costs
/// reach, which is the thing the tiles were bought for; a floor too low leaves
/// a prose page paying 897 ms, which is only the status quo. Where the gap is
/// this wide there is room for both margins, and the log-middle is what takes
/// them in proportion rather than in points.
///
/// On the fixture the gate is closed on exactly the three unlabelled pages and
/// open on all twenty-five labelled ones, so reach is unchanged — measured,
/// not argued: `formula_recall` reads 88% cropped with the gate as without it.
pub const TILE_GATE_FLOOR: f32 = 0.15;

/// The best score any formula query carries in one pass's raw rows.
///
/// Raw, at no threshold: the graph returns every query it has and the gate's
/// whole point is to read a page the *threshold* rejects. Zero when the pass
/// proposed no formula class at all, which is a closed gate.
///
/// Public because the probe that sets [`TILE_GATE_FLOOR`] reads exactly this
/// number off exactly these rows, and a probe with its own copy of it would
/// be setting a floor for a gate nobody runs.
pub fn best_formula_score(rows: &[f32]) -> f32 {
    let mut best = 0f32;
    for row in rows.as_chunks::<DETECTION_COLUMNS>().0 {
        let (class, score) = (row[0], row[1]);
        if score.is_nan() {
            continue;
        }
        let Some(label) = usize::try_from(class as i64)
            .ok()
            .and_then(|index| LABELS.get(index))
        else {
            continue;
        };
        if is_formula(label) && score > best {
            best = score;
        }
    }
    best
}

/// Whether a class is one the formula passes exist for.
///
/// The one place this build says which classes get the extra resolution. Both
/// route to [`RegionKind::Formula`] and both are what the tiling is for, so
/// this is stated once rather than matched at each of the four sites that ask.
pub fn is_formula(label: &str) -> bool {
    matches!(label, "display_formula" | "inline_formula")
}

/// The score a detection of `label` must reach to be believed.
fn threshold_for(label: &str, formula_threshold: f32) -> f32 {
    if is_formula(label) {
        formula_threshold
    } else {
        SCORE_THRESHOLD
    }
}

/// PP-DocLayoutV2's classes, in the order its `config.json` lists them.
///
/// The index *is* the class id the graph returns, so this order is a contract
/// with the pinned revision and not a convenience. Kept in full, including the
/// classes nothing routes, because a detection this build cannot name is a
/// fact about the build and is reported as one.
pub const LABELS: [&str; 25] = [
    "abstract",
    "algorithm",
    "aside_text",
    "chart",
    "content",
    "display_formula",
    "doc_title",
    "figure_title",
    "footer",
    "footer_image",
    "footnote",
    "formula_number",
    "header",
    "header_image",
    "image",
    "inline_formula",
    "number",
    "paragraph_title",
    "reference",
    "reference_content",
    "seal",
    "table",
    "text",
    "vertical_text",
    "vision_footnote",
];

/// What a class means to Wilkes, or nothing.
///
/// Only the kinds that are *worth displacing a page's own glyphs for* map to
/// something — see [`RegionKind::supersedes_native_glyphs`]. A class that maps
/// to `None` is still counted; it is simply an area the page already tells us
/// about better than a recognizer would.
///
/// Both formula classes map to [`RegionKind::Formula`]. `inline_formula` mapped
/// to nothing for as long as supersession replaced whole *lines* — an inline
/// expression shares its line with prose, so reading one meant either losing
/// the sentence or reading it twice. A region owns words now, so an inline
/// formula is spliced into the sentence where the page drew it.
pub fn kind_of(label: &str) -> Option<RegionKind> {
    match label {
        // Both formulas, and both routed. An inline expression was reported
        // and not read for as long as supersession replaced whole lines: it
        // shares its line with prose, so reading it meant either losing the
        // sentence or reading it twice. Regions own words now, so it is
        // spliced into the sentence where the page drew it.
        "display_formula" | "inline_formula" => Some(RegionKind::Formula),
        "table" => Some(RegionKind::Table),
        "chart" => Some(RegionKind::Chart),
        _ => None,
    }
}

/// The static label equal to `label`, or `None` when this detector has no such
/// class.
///
/// The crossing back from a worker's reply into the detector's own vocabulary.
/// A [`LayoutRegion`](super::LayoutRegion)'s label is a `&'static str` out of
/// [`LABELS`] because the diagnostics count classes by identity; a string off a
/// pipe is not one of those until it has been found here.
pub fn label_of(label: &str) -> Option<&'static str> {
    LABELS.iter().copied().find(|known| *known == label)
}

/// What one detection does: how big a page it is handed, how that page is cut
/// into the squares the graph sees, and what a formula detection must score.
///
/// A struct and not four constants read from inside the detection code,
/// because the probe measures other recipes than the one production runs and
/// there is to be one detection path, not a production one and a probe one.
/// Production always passes [`Recipe::PRODUCTION`]; nothing under `src/`
/// constructs any other.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Recipe {
    /// The square the host rasterizes a page to. Always a multiple of [`SIDE`].
    pub render_side: u32,
    /// How far apart the formula tiles start, in render pixels. `None` runs
    /// the whole-page pass only — the recipe as it was before tiling, kept
    /// reachable so the probe can print the baseline from the same code.
    pub tile_stride: Option<u32>,
    /// What a `display_formula` or `inline_formula` detection must score.
    pub formula_threshold: f32,
    /// What the whole-page pass must already have seen for the formula tiles
    /// to be run at all — [`TILE_GATE_FLOOR`], or `None` to tile every page
    /// unconditionally, which is what every recipe measured before the gate
    /// existed did.
    ///
    /// A field of the recipe and not a constant read inside the detection
    /// code, for the same reason the tiling is: the probe measures the gated
    /// recipe against the ungated one, and there is to be one detection path.
    pub tile_gate: Option<f32>,
}

impl Recipe {
    /// What extraction runs, and the only recipe named in [`identity`].
    pub const PRODUCTION: Self = Self {
        render_side: RENDER_SIDE,
        tile_stride: Some(TILE_STRIDE),
        formula_threshold: FORMULA_THRESHOLD,
        tile_gate: Some(TILE_GATE_FLOOR),
    };

    /// One 800x800 pass over the whole page and nothing else: the detector as
    /// it was before the formula tiles.
    pub const fn whole_page(formula_threshold: f32) -> Self {
        Self {
            render_side: SIDE,
            tile_stride: None,
            formula_threshold,
            // Nothing to gate: there are no tiles to withhold.
            tile_gate: None,
        }
    }

    /// The windows one detection looks through, in render-square pixels.
    ///
    /// The first is always the whole page, downsampled to the graph's square;
    /// it is the only one whose non-formula classes are kept, because a table
    /// or a figure title is a judgement about the page and a tile is not a
    /// page. The rest are formula tiles at the render's own resolution.
    pub fn windows(&self) -> Vec<Window> {
        let mut windows = vec![Window {
            x: 0,
            y: 0,
            side: self.render_side,
            whole_page: true,
        }];
        let Some(stride) = self.tile_stride else {
            return windows;
        };
        if self.render_side <= SIDE || stride == 0 {
            // Tiling a square that is already the graph's own would run the
            // same pass twice and call the second one a tile.
            return windows;
        }
        for y in tile_offsets(self.render_side, stride) {
            for x in tile_offsets(self.render_side, stride) {
                windows.push(Window {
                    x,
                    y,
                    side: SIDE,
                    whole_page: false,
                });
            }
        }
        windows
    }

    /// The recipe as it enters an identity string, and as the probe heads a
    /// table with. Derived, so the host answers it with no worker up.
    pub fn name(&self) -> String {
        let tiling = match self.tile_stride {
            Some(stride) if self.render_side > SIDE && stride > 0 => {
                format!("{SIDE}/{stride}")
            }
            _ => "none".to_string(),
        };
        // Named, because it decides whether a page is looked at four more
        // times and therefore what the detector says about it. A reading taken
        // with the gate closed on a page is not the reading the ungated recipe
        // would have produced there.
        let gate = match self.tile_gate {
            Some(floor)
                if matches!(self.tile_stride, Some(stride) if stride > 0)
                    && self.render_side > SIDE =>
            {
                format!("{floor}")
            }
            _ => "none".to_string(),
        };
        format!(
            "{}px-{DOWNSAMPLE_NAME}+tiles-{tiling}+tilegate-{gate}\
             +score-{SCORE_THRESHOLD}+formula-{}",
            self.render_side, self.formula_threshold
        )
    }
}

/// Where a tile's left (or top) edge sits, in render pixels.
///
/// The last offset is pinned to the far edge rather than left where the stride
/// happened to stop: a strip of page that no tile covered would be a strip
/// where the formula classes silently went back to whole-page resolution.
fn tile_offsets(render_side: u32, stride: u32) -> Vec<u32> {
    let last = render_side - SIDE;
    let mut offsets = Vec::new();
    let mut at = 0;
    while at <= last {
        offsets.push(at);
        at += stride;
    }
    if offsets.last() != Some(&last) {
        offsets.push(last);
    }
    offsets
}

/// One forward pass's view of the render square.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    /// Top-left corner in render pixels.
    pub x: u32,
    pub y: u32,
    /// The window's side in render pixels. [`SIDE`] for a tile, which is then
    /// fed to the graph unscaled; the whole render square for the page pass,
    /// which is downsampled to [`SIDE`] first.
    pub side: u32,
    /// Whether this pass's non-formula classes are kept.
    pub whole_page: bool,
}

impl Window {
    /// Where a point in this pass's 800 px input lands in the render square.
    fn to_render(self, at: f32) -> f32 {
        at * (self.side as f32 / SIDE as f32)
    }
}

/// One forward pass: what the graph returned, and where it was looking.
///
/// Held rather than decoded on the spot so a probe sweeping operating points
/// decodes the same passes at each one. Re-running the graph per threshold
/// would measure a cost production does not pay.
pub struct Pass {
    pub window: Window,
    pub rows: Vec<f32>,
}

/// The detector's recipe, as it enters the extraction identity.
///
/// Everything that changes what [`decode`] returns is named here, because this
/// string is what makes a stored reading be taken again. The recipe's geometry
/// and its two thresholds through [`Recipe::name`]; the constants the merge
/// reconciles the passes with through [`merge_rule`] — those decide which of
/// two boxes over one expression the recognizer is handed, which is a different
/// crop and a different reading, and until now they were unnamed.
pub fn identity() -> String {
    format!(
        "{MODEL_ID}+{REPO}@{REVISION}+{}+{}+labels-{}",
        Recipe::PRODUCTION.name(),
        merge_rule(),
        LABELS.len()
    )
}

/// How the passes are reconciled, as the identity spells it.
///
/// Derived from the constants alone, so the host answers it with no worker up
/// — the same rule [`Recipe::name`] is held to.
fn merge_rule() -> String {
    format!(
        "merge-iou-{MERGE_IOU}+absorb-{ABSORB_SHARE}+seam-{SEAM_EPS}px/{SEAM_OVERLAP}+cross-{CROSS_CLASS_IOU}"
    )
}

pub fn install_dir(model_dir: &Path) -> PathBuf {
    model_dir.join("layout").join(MODEL_ID)
}

pub fn is_installed(model_dir: &Path) -> bool {
    let dir = install_dir(model_dir);
    ARTIFACTS.iter().all(|name| dir.join(name).is_file())
}

pub fn footprint_bytes() -> u64 {
    DIGESTS.iter().map(|(size, _)| size).sum()
}

/// What the detector is, where it came from, and under what terms.
pub fn inventory() -> crate::types::RecognizerInventory {
    crate::types::RecognizerInventory {
        name: MODEL_ID.to_string(),
        repo: REPO.to_string(),
        revision: REVISION.to_string(),
        license: "Apache-2.0".to_string(),
        license_url: "https://huggingface.co/PaddlePaddle/PP-DocLayoutV2".to_string(),
        derived_from: vec![
            "PP-DocLayoutV2 (Apache-2.0, PaddlePaddle)".to_string(),
            "RT-DETR detection head (Apache-2.0, PaddlePaddle)".to_string(),
            format!("ONNX export by {REPO}"),
        ],
        artifacts: ARTIFACTS
            .iter()
            .zip(DIGESTS)
            .map(
                |(filename, (size_bytes, sha256))| crate::types::InventoriedArtifact {
                    filename: (*filename).to_string(),
                    size_bytes: *size_bytes,
                    sha256: (*sha256).to_string(),
                },
            )
            .collect(),
        footprint_bytes: footprint_bytes(),
    }
}

/// Fetch the detector's artifacts into `model_dir`, checking each against the
/// size and digest declared above.
pub fn install(
    model_dir: &Path,
    progress: Option<crate::models::progress::ProgressTx>,
) -> Result<()> {
    use hf_hub::api::sync::ApiBuilder;

    let dir = install_dir(model_dir);
    std::fs::create_dir_all(&dir).context("could not create the detector directory")?;

    let api = ApiBuilder::new()
        .with_progress(false)
        .build()
        .context("could not reach the model hub")?
        .repo(hf_hub::Repo::with_revision(
            REPO.to_string(),
            hf_hub::RepoType::Model,
            REVISION.to_string(),
        ));

    let reporter = progress.map(crate::models::hf_hub::HfProgressReporter::new);
    for (filename, (size_bytes, sha256)) in ARTIFACTS.iter().zip(DIGESTS) {
        let target = dir.join(filename);
        if target.is_file() && super::verify_artifact(&target, *size_bytes, sha256).is_ok() {
            continue;
        }
        let fetched = match reporter.clone() {
            Some(reporter) => api.download_with_progress(filename, reporter),
            None => api.download(filename),
        }
        .with_context(|| format!("could not download {filename} from {REPO}"))?;
        std::fs::copy(&fetched, &target)
            .with_context(|| format!("could not place {filename} under {}", dir.display()))?;
        // A file that does not match is removed rather than left where
        // `is_installed` would count it: a half-installed detector that reads
        // as installed is worse than one that reads as absent.
        if let Err(error) = super::verify_artifact(&target, *size_bytes, sha256) {
            let _ = std::fs::remove_file(&target);
            return Err(error);
        }
    }
    Ok(())
}

/// The graph's declared inputs. `scale_factor` is fed as identity so the boxes
/// come back in the square's own coordinates and the map back to the page
/// stays here, where a test can check it.
pub const INPUT_IMAGE: &str = "image";
pub const INPUT_SCALE: &str = "scale_factor";
pub const INPUT_SHAPE: &str = "im_shape";

/// Columns of the detection tensor. Six are the detection; the last two are
/// the reading-order outputs V2 adds, which nothing here consumes.
const DETECTION_COLUMNS: usize = 8;

/// PP-DocLayoutV2, addressed.
pub struct DocLayout {
    /// Where the graph is and what to load it with, kept so the session can be
    /// dropped and rebuilt: [`super::LayoutModel::release`] is a promise that
    /// a later `detect` still works.
    path: PathBuf,
    threads: usize,
    /// `Session::run` needs `&mut`, and the detector is shared across the
    /// extraction of one document; one page at a time is also what the cost
    /// model assumes. `None` after a release, and until the first page.
    session: Mutex<Option<Session>>,
}

impl DocLayout {
    /// Check the graph is here and note how to load it. Loading is deferred to
    /// the first page — a runtime that attaches an analyzer and then indexes
    /// nothing should not pay 204 MB for it.
    pub fn load(model_dir: &Path, threads: usize) -> Result<Self> {
        let path = install_dir(model_dir).join(GRAPH);
        anyhow::ensure!(
            path.is_file(),
            "the layout detector is not installed: {} is missing",
            path.display()
        );
        Ok(Self {
            path,
            threads,
            session: Mutex::new(None),
        })
    }

    fn open(&self) -> Result<Session> {
        Session::builder()
            .map_err(|e| anyhow::anyhow!("could not start an ONNX session builder: {e}"))?
            .with_intra_threads(self.threads)
            .map_err(|e| anyhow::anyhow!("could not set the session thread count: {e}"))?
            .commit_from_file(&self.path)
            .map_err(|e| anyhow::anyhow!("could not load {}: {e}", self.path.display()))
    }

    /// The size a page must be rendered at to be detected on.
    ///
    /// No longer the graph's own square: the host renders at [`RENDER_SIDE`]
    /// and this module cuts that into the squares the graph sees.
    pub const fn input_side() -> u32 {
        Recipe::PRODUCTION.render_side
    }

    /// The square the graph itself is fed, whatever the page was rendered at.
    ///
    /// Public because a probe that reports recall by how big an expression is
    /// *to the detector* has to measure it in this space, and it must not
    /// grow its own copy of the number.
    pub const fn graph_side() -> u32 {
        SIDE
    }
}

/// Turn one rendered page into the model's input tensor: RGB, CHW, scaled to
/// 0..1.
///
/// The model's `NormalizeImage` step declares mean 0, std 1 and `norm_type:
/// none`, which in PaddleDetection's preprocessing is the identity after the
/// 1/255 scaling. Written out rather than folded away so the next reader can
/// check it against the config.
/// `pub` so a probe measuring an execution provider feeds the graph exactly
/// what production feeds it. Pure, and holds no state.
pub fn to_tensor(page: &RgbImage) -> Result<Tensor<f32>> {
    let (w, h) = (page.width() as usize, page.height() as usize);
    anyhow::ensure!(
        w == SIDE as usize && h == SIDE as usize,
        "the detector wants a {SIDE}x{SIDE} render, got {w}x{h}"
    );
    let mut chw = vec![0f32; 3 * w * h];
    for (x, y, pixel) in page.enumerate_pixels() {
        let (x, y) = (x as usize, y as usize);
        for channel in 0..3 {
            chw[channel * w * h + y * w + x] = f32::from(pixel.0[channel]) / 255.0;
        }
    }
    Tensor::from_array((vec![1i64, 3, h as i64, w as i64], chw))
        .context("could not build the detector's input tensor")
}

impl super::LayoutModel for DocLayout {
    fn identity(&self) -> String {
        identity()
    }

    fn input_side(&self) -> u32 {
        Recipe::PRODUCTION.render_side
    }

    fn release(&self) {
        let dropped = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .is_some();
        if dropped {
            debug!("layout detector released; the next page reloads it");
        }
    }

    /// The loop over the document's pages, on the side of the pipe the graph
    /// is resident on. One load, every page, one killable unit.
    ///
    /// A page is rendered, detected on and dropped before the next is asked
    /// for, so a book costs one page of pixels rather than a book of them.
    fn detect_document(
        &self,
        page_count: usize,
        render: &mut dyn FnMut(usize) -> Result<RgbImage>,
    ) -> Result<Vec<Vec<LayoutRegion>>> {
        let recipe = Recipe::PRODUCTION;
        let mut found = Vec::with_capacity(page_count);
        for index in 0..page_count {
            // A page that would not rasterize holds no regions. Not an error:
            // the caller has already said so in its own log, and failing the
            // document would throw away a book over one page.
            let Ok(page) = render(index) else {
                found.push(Vec::new());
                continue;
            };
            found.push(decode(&self.passes(&page, &recipe)?, &recipe));
        }
        Ok(found)
    }
}

impl DocLayout {
    /// Run every pass `recipe` calls for over one rendered page.
    ///
    /// The one entry to the graph. Production reaches it through
    /// [`LayoutModel::detect_document`](super::LayoutModel::detect_document) with
    /// [`Recipe::PRODUCTION`]; the probe reaches it with the recipes it is
    /// comparing. Neither has a second path, which is the point: a measured
    /// recipe that production could not run would be a measurement of nothing.
    ///
    /// Returns the passes undecoded, so a probe can decode the same passes at
    /// several thresholds for the price of one detection.
    pub fn passes(&self, page: &RgbImage, recipe: &Recipe) -> Result<Vec<Pass>> {
        let side = recipe.render_side;
        anyhow::ensure!(
            page.width() == side && page.height() == side,
            "this recipe wants a {side}x{side} render, got {}x{}",
            page.width(),
            page.height()
        );
        let windows = recipe.windows();
        debug!(
            "detecting with {} over a {side}px render, {} passes",
            recipe.name(),
            windows.len()
        );
        let mut passes: Vec<Pass> = Vec::with_capacity(windows.len());
        for window in windows {
            // The gate: asked once, between the pass every page pays for and
            // the four this page may not need. The whole-page window is the
            // first and only the first — see [`Recipe::windows`] — so "one
            // pass held" is "the tiles are next", not a re-derivation of it.
            if passes.len() == 1 && !window.whole_page {
                if let Some(floor) = recipe.tile_gate {
                    let best = best_formula_score(&passes[0].rows);
                    if best < floor {
                        debug!(
                            "the whole-page pass scored {best:.4} on the formula classes, \
                             below the {floor} tile gate; the formula tiles are not run"
                        );
                        break;
                    }
                }
            }
            let input = cut(page, &window);
            let rows = self.run(&input)?;
            passes.push(Pass { window, rows });
        }
        Ok(passes)
    }

    /// One forward pass over one 800x800 input: the detection tensor,
    /// verbatim, before anything is believed or dropped.
    ///
    /// The graph returns every one of its queries and the score cut is applied
    /// afterwards by [`decode`], so this is the one place a caller can see
    /// what the detector proposed *below* the threshold.
    fn run(&self, page: &RgbImage) -> Result<Vec<f32>> {
        let image = to_tensor(page)?;
        let unit = Tensor::from_array((vec![1i64, 2], vec![1.0f32, 1.0]))
            .context("could not build the scale tensor")?;
        let shape = Tensor::from_array((vec![1i64, 2], vec![SIDE as f32, SIDE as f32]))
            .context("could not build the shape tensor")?;

        let mut held = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.is_none() {
            *held = Some(self.open()?);
        }
        let session = held.as_mut().expect("the session was just opened");
        let feed: Vec<(String, SessionInputValue)> = session
            .inputs()
            .iter()
            .map(|input| input.name().to_string())
            .map(|name| -> Result<(String, SessionInputValue)> {
                let value: SessionInputValue = match name.as_str() {
                    INPUT_IMAGE => image.clone().into(),
                    INPUT_SCALE => unit.clone().into(),
                    INPUT_SHAPE => shape.clone().into(),
                    other => anyhow::bail!(
                        "the detector declares an input this build does not know: {other}"
                    ),
                };
                Ok((name, value))
            })
            .collect::<Result<_>>()?;

        let outputs = session
            .run(feed)
            .map_err(|e| anyhow::anyhow!("the layout detector failed: {e}"))?;
        let (shape, rows) = outputs[0].try_extract_tensor::<f32>().map_err(|e| {
            anyhow::anyhow!("the detector returned a tensor this build cannot read: {e}")
        })?;
        anyhow::ensure!(
            shape.len() == 2 && shape[1] as usize == DETECTION_COLUMNS,
            "the detector returned {shape:?}, {DETECTION_COLUMNS} columns expected"
        );

        Ok(rows.to_vec())
    }
}

/// The input for one window: the tile it names, at the graph's own square.
///
/// A tile is cut and handed over unscaled — the whole reason for the render
/// being bigger than the graph's square is that a tile keeps its pixels. The
/// whole-page window is the only one resampled, and it is resampled down.
///
/// Nearest, which is to say every second pixel of the 1600. Four filters were
/// measured on the fixture, whole-page pass only, at 0.44 (inline coverage /
/// precision, and display recall):
///
/// ```text
/// nearest      56% / 74%   86%      catmull-rom  55% / 73%   79%
/// lanczos3     55% / 73%   79%      triangle     52% / 74%   77%
/// gaussian     49% / 74%   79%      (mupdf at 800 direct: 52% / 78%, 80%)
/// ```
///
/// Which is the opposite of the expected order, and the reason is that a
/// filtered reduction of 9 pt type is a reduction of mostly stroke: averaging
/// two pixels of a one-pixel stem with two of paper leaves grey where the page
/// drew ink, and the detector is looking for exactly that ink. Point-sampling
/// a raster mupdf already antialiased at twice the size keeps the stems and
/// keeps them black. It also beats rasterizing at 800 outright, on both
/// classes, which is why the whole-page pass reads a downsample rather than
/// the host being asked for a second render.
///
/// Not an argument from resampling theory, and not offered as one: it is what
/// the fixture said, and the table is here so the next person can disagree
/// with the measurement rather than with the reasoning.
pub fn cut(page: &RgbImage, window: &Window) -> RgbImage {
    let tile = image::imageops::crop_imm(page, window.x, window.y, window.side, window.side);
    if window.side == SIDE {
        return tile.to_image();
    }
    image::imageops::resize(&*tile, SIDE, SIDE, DOWNSAMPLE)
}

/// A detection as it comes off one pass, in render-square pixels.
///
/// Kept in render pixels rather than page fractions until the merge is over:
/// "these two fragments abut on a tile seam" is a statement about pixels, and
/// a tolerance expressed in fractions of a page would mean something different
/// on every page size.
#[derive(Clone, Copy, Debug)]
struct Proposal {
    label: &'static str,
    score: f32,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    /// Which of this proposal's sides sit on an edge of its own window that is
    /// *interior* to the page — the sides where the box may be a fragment of
    /// something the tile cut in half. Never set for the whole-page window,
    /// whose every edge is the page's own.
    cut_left: bool,
    cut_right: bool,
    cut_top: bool,
    cut_bottom: bool,
}

impl Proposal {
    fn area(&self) -> f32 {
        (self.x1 - self.x0).max(0.0) * (self.y1 - self.y0).max(0.0)
    }

    fn clipped(&self) -> bool {
        self.cut_left || self.cut_right || self.cut_top || self.cut_bottom
    }

    fn intersection(&self, other: &Self) -> f32 {
        let w = (self.x1.min(other.x1) - self.x0.max(other.x0)).max(0.0);
        let h = (self.y1.min(other.y1) - self.y0.max(other.y0)).max(0.0);
        w * h
    }

    fn iou(&self, other: &Self) -> f32 {
        let overlap = self.intersection(other);
        let union = self.area() + other.area() - overlap;
        if union <= 0.0 {
            0.0
        } else {
            overlap / union
        }
    }

    /// Take `other` in, so this proposal is the group's box.
    ///
    /// The union's sides are *not* the intersection of the members' flags. A
    /// side of the union is placed by whichever member reached furthest out,
    /// and whether that side sits on a tile boundary is that member's fact
    /// alone. Asking every member would let a whole-page proposal that stopped
    /// short of the seam talk the fragment that reached it out of being cut
    /// there — and then the fragment on the *other* side of the seam has
    /// nothing to join, so the expression is read in halves, which is the one
    /// thing [`joins`] exists to prevent.
    fn absorb(&mut self, other: &Self) {
        let x0 = self.x0.min(other.x0);
        let y0 = self.y0.min(other.y0);
        let x1 = self.x1.max(other.x1);
        let y1 = self.y1.max(other.y1);
        self.cut_left = placed_by(x0, self.x0, self.cut_left, other.x0, other.cut_left);
        self.cut_top = placed_by(y0, self.y0, self.cut_top, other.y0, other.cut_top);
        self.cut_right = placed_by(x1, self.x1, self.cut_right, other.x1, other.cut_right);
        self.cut_bottom = placed_by(y1, self.y1, self.cut_bottom, other.y1, other.cut_bottom);
        (self.x0, self.y0, self.x1, self.y1) = (x0, y0, x1, y1);
        self.score = self.score.max(other.score);
    }
}

/// Whether the member that placed a side of a union was cut at it.
///
/// `at` is the coordinate the union takes, so it is bitwise one of `mine` and
/// `theirs` — it was produced by `min` or `max` of exactly those two. A member
/// the union reached past says nothing about that side; two members that both
/// placed it are asked as an "either", because one of them having seen a tile
/// edge there is the evidence.
fn placed_by(at: f32, mine: f32, mine_cut: bool, theirs: f32, theirs_cut: bool) -> bool {
    (mine == at && mine_cut) || (theirs == at && theirs_cut)
}

/// Two proposals of the same class overlapping this much are the same thing
/// seen from two tiles.
const MERGE_IOU: f32 = 0.5;

/// How much of a *clipped* proposal has to lie inside a larger one of the same
/// class for it to be that one's fragment rather than its own expression.
///
/// Only clipped proposals are absorbed this way: a fragment is a piece of
/// whatever box contains it, and an unclipped small box is an expression in
/// its own right. So a whole-page `inline_formula` that swallowed a line does
/// *not* eat the correct small detections inside it — both go forward.
///
/// What then happens to them is not settled here, and this comment used to
/// claim it was. `extract::pdf::typeset::regions` gives the page's words to
/// the largest box first, and a box left owning none is kept only where
/// nothing already speaks for its area — so on such a line the container is
/// what gets cropped and the small ones inside it do not. Measured on the
/// 28-page `formula_recall` fixture under the production recipe, that was 5 of
/// the 555 needs_recognizer labels — about a point of coverage — against 83
/// lost because nothing is typeset under them at all. A merge rule that
/// dropped a box containing two or more unclipped ones was written and
/// measured against exactly that: it moved the labels a crop reaches from 404
/// to 402 and the crops from 15.0 to 15.2 a page, so it is not in the tree.
/// The nesting is decided once, downstream, by which box owns the words and,
/// where there are none, by which box owns the area.
const ABSORB_SHARE: f32 = 0.8;

/// How close, in render pixels, two fragments' facing edges must be to count
/// as the two halves of something a tile seam cut.
const SEAM_EPS: f32 = 2.0;

/// How much of the shorter side two seam fragments must share along the seam.
const SEAM_OVERLAP: f32 = 0.5;

/// Two formula proposals of *different* classes overlapping this much are one
/// crop and one Texify call, not two.
const CROSS_CLASS_IOU: f32 = 0.6;

/// How much of the shorter span `a0..a1` and `b0..b1` share.
fn span_overlap(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    let overlap = (a1.min(b1) - a0.max(b0)).max(0.0);
    let shorter = (a1 - a0).min(b1 - b0);
    if shorter <= 0.0 {
        0.0
    } else {
        overlap / shorter
    }
}

/// Whether `a` and `b` are the two halves of one expression a tile seam cut.
///
/// Both have to be clipped, on the facing sides, at the same coordinate. A
/// formula that merely ends where another begins is not joined: the evidence
/// for joining is that a tile boundary is there, not that two boxes are close.
fn seam_adjacent(a: &Proposal, b: &Proposal) -> bool {
    let vertical = (a.cut_right && b.cut_left && (a.x1 - b.x0).abs() <= SEAM_EPS)
        || (b.cut_right && a.cut_left && (b.x1 - a.x0).abs() <= SEAM_EPS);
    if vertical && span_overlap(a.y0, a.y1, b.y0, b.y1) >= SEAM_OVERLAP {
        return true;
    }
    let horizontal = (a.cut_bottom && b.cut_top && (a.y1 - b.y0).abs() <= SEAM_EPS)
        || (b.cut_bottom && a.cut_top && (b.y1 - a.y0).abs() <= SEAM_EPS);
    horizontal && span_overlap(a.x0, a.x1, b.x0, b.x1) >= SEAM_OVERLAP
}

/// Whether two same-class proposals are one expression seen twice.
fn joins(a: &Proposal, b: &Proposal) -> bool {
    if a.label != b.label {
        return false;
    }
    if a.iou(b) >= MERGE_IOU || seam_adjacent(a, b) {
        return true;
    }
    // A tile that caught the left third of an expression and a tile that
    // caught the whole of it overlap far too little to be one box by IoU, and
    // are one box.
    let (smaller, larger) = if a.area() <= b.area() { (a, b) } else { (b, a) };
    smaller.clipped()
        && smaller.area() > 0.0
        && smaller.intersection(larger) / smaller.area() >= ABSORB_SHARE
}

/// Fold every group of proposals that describe one expression into one box.
///
/// The union and not the highest-scoring member: the reason two tiles saw the
/// same formula differently is usually that one of them could only see part of
/// it, and the part is not what a recognizer should be handed.
fn merge(mut pool: Vec<Proposal>) -> Vec<Proposal> {
    let mut merged: Vec<Proposal> = Vec::with_capacity(pool.len());
    // Highest score first, so a group is grown around the proposal the
    // detector was surest of and the result does not depend on pass order.
    pool.sort_by(|a, b| b.score.total_cmp(&a.score));
    for proposal in pool {
        // Repeated to a fixed point: absorbing one proposal grows the box,
        // and a grown box can reach a group that was separate a moment ago.
        // Without this the result would depend on which order the groups were
        // discovered in, which is a tile-order dependency in disguise.
        let mut candidate = proposal;
        while let Some(at) = merged.iter().position(|kept| joins(kept, &candidate)) {
            let absorbed = merged.swap_remove(at);
            candidate.absorb(&absorbed);
        }
        merged.push(candidate);
    }
    merged
}

/// Turn every pass of one page into regions, in page fractions.
///
/// Separated from the session so it can be tested against rows written by
/// hand: the geometry, the thresholding and the merge are where a mistake
/// would be invisible, and none of them needs a 204 MB graph to exercise.
///
/// Non-formula classes are taken from the whole-page pass alone and are
/// untouched by any of this — a table is a judgement about a page, and a tile
/// is not a page. The formula classes are taken from every pass, at
/// `recipe.formula_threshold`, and reconciled.
pub fn decode(passes: &[Pass], recipe: &Recipe) -> Vec<LayoutRegion> {
    let render = recipe.render_side as f32;
    let mut regions = Vec::new();
    let mut formulas = Vec::new();

    for pass in passes {
        let window = pass.window;
        let (ox, oy) = (window.x as f32, window.y as f32);
        let far = (window.x + window.side) as f32;
        let low = (window.y + window.side) as f32;
        for row in pass.rows.as_chunks::<DETECTION_COLUMNS>().0 {
            let (class, score) = (row[0], row[1]);
            if score.is_nan() {
                continue;
            }
            // The graph returns every query, thresholded here rather than
            // inside; a negative or out-of-range class id is a graph this
            // build does not understand, and is dropped loudly rather than
            // indexed with.
            let Some(label) = usize::try_from(class as i64)
                .ok()
                .and_then(|index| LABELS.get(index))
            else {
                debug!("layout detector returned class {class}, which this build has no name for");
                continue;
            };
            let formula = is_formula(label);
            if !formula && !window.whole_page {
                continue;
            }
            if score < threshold_for(label, recipe.formula_threshold) {
                continue;
            }
            // Into the render square, then clamped to the window: a box the
            // graph ran off the edge of its input says nothing about the page
            // outside that input, and the clamp is what makes "this side sits
            // on the tile's edge" answerable below.
            let x0 = (ox + window.to_render(row[2].min(row[4]))).clamp(ox, far);
            let y0 = (oy + window.to_render(row[3].min(row[5]))).clamp(oy, low);
            let x1 = (ox + window.to_render(row[2].max(row[4]))).clamp(ox, far);
            let y1 = (oy + window.to_render(row[3].max(row[5]))).clamp(oy, low);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            if !formula {
                regions.push(region(label, score, x0, y0, x1, y1, render));
                continue;
            }
            formulas.push(Proposal {
                label,
                score,
                x0,
                y0,
                x1,
                y1,
                // An edge of the render square is the page's own edge, and a
                // box that ends there is not a fragment of anything.
                cut_left: window.x > 0 && (x0 - ox).abs() <= SEAM_EPS,
                cut_right: far < render && (far - x1).abs() <= SEAM_EPS,
                cut_top: window.y > 0 && (y0 - oy).abs() <= SEAM_EPS,
                cut_bottom: low < render && (low - y1).abs() <= SEAM_EPS,
            });
        }
    }

    let mut merged = merge(formulas);
    // Across the two formula classes now: an area proposed as both is one
    // crop and one recognizer call, and the class the detector scored higher
    // is the one it keeps. Suppression and not a union, because a small inline
    // expression inside a large display block is two real regions.
    merged.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Proposal> = Vec::with_capacity(merged.len());
    for proposal in merged {
        if kept
            .iter()
            .any(|other| other.label != proposal.label && other.iou(&proposal) >= CROSS_CLASS_IOU)
        {
            continue;
        }
        kept.push(proposal);
    }

    regions.extend(
        kept.into_iter()
            .map(|p| region(p.label, p.score, p.x0, p.y0, p.x1, p.y1, render)),
    );
    regions
}

/// One proposal, in the page fractions a region is recorded in.
fn region(
    label: &'static str,
    score: f32,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    render: f32,
) -> LayoutRegion {
    let clamp = |v: f32| (v / render).clamp(0.0, 1.0);
    let (x0, y0) = (clamp(x0), clamp(y0));
    let (x1, y1) = (clamp(x1), clamp(y1));
    LayoutRegion {
        label,
        kind: kind_of(label),
        score,
        bbox: BoundingBox {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Class ids the tests name, so a reader does not have to count [`LABELS`].
    const DISPLAY: f32 = 5.0;
    const INLINE: f32 = 15.0;
    const TABLE: f32 = 21.0;

    fn row(class: f32, score: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> [f32; 8] {
        [class, score, x0, y0, x1, y1, 0.0, 0.0]
    }

    /// The detector as it was before the tiles: one 800 px pass over the page.
    fn baseline() -> Recipe {
        Recipe::whole_page(SCORE_THRESHOLD)
    }

    /// One whole-page pass carrying `rows`.
    fn whole(rows: &[f32]) -> Vec<Pass> {
        vec![Pass {
            window: Window {
                x: 0,
                y: 0,
                side: SIDE,
                whole_page: true,
            },
            rows: rows.to_vec(),
        }]
    }

    /// One tile of a `render_side` square, carrying `rows` in its own pixels.
    fn tile(x: u32, y: u32, rows: &[f32]) -> Pass {
        Pass {
            window: Window {
                x,
                y,
                side: SIDE,
                whole_page: false,
            },
            rows: rows.to_vec(),
        }
    }

    /// The 2x tiled recipe with the formula cut wherever a test wants it.
    fn tiled(formula_threshold: f32) -> Recipe {
        Recipe {
            render_side: 2 * SIDE,
            tile_stride: Some(SIDE),
            formula_threshold,
            // These tests hand `decode` the passes directly, so there is no
            // gate to apply: what is under test is the reconciliation.
            tile_gate: None,
        }
    }

    #[test]
    fn a_detection_becomes_a_fraction_of_the_page() {
        // 800 is the square, so half of it is half the page.
        let found = decode(
            &whole(&row(DISPLAY, 0.9, 200.0, 400.0, 600.0, 600.0)),
            &baseline(),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "display_formula");
        assert_eq!(found[0].kind, Some(RegionKind::Formula));
        assert_eq!(found[0].bbox.x, 0.25);
        assert_eq!(found[0].bbox.y, 0.5);
        assert_eq!(found[0].bbox.width, 0.5);
        assert_eq!(found[0].bbox.height, 0.25);
    }

    #[test]
    fn a_detection_below_the_threshold_is_not_believed() {
        let mut rows = Vec::new();
        rows.extend(row(TABLE, SCORE_THRESHOLD - 0.01, 0.0, 0.0, 100.0, 100.0));
        rows.extend(row(TABLE, SCORE_THRESHOLD, 0.0, 0.0, 200.0, 200.0));
        let found = decode(&whole(&rows), &baseline());
        assert_eq!(found.len(), 1, "the threshold is inclusive and only just");
        assert_eq!(
            found[0].bbox.width, 0.25,
            "the believed one is the survivor"
        );
    }

    /// The two classes this pipeline routes have their own cut, and it is not
    /// the one the other twenty-three are held to.
    #[test]
    fn the_formula_classes_are_thresholded_apart_from_every_other_class() {
        let recipe = Recipe::whole_page(0.3);
        let mut rows = Vec::new();
        rows.extend(row(INLINE, 0.35, 0.0, 0.0, 100.0, 100.0));
        rows.extend(row(DISPLAY, 0.35, 400.0, 400.0, 500.0, 500.0));
        rows.extend(row(TABLE, 0.35, 200.0, 0.0, 300.0, 100.0));
        let found = decode(&whole(&rows), &recipe);
        let labels: Vec<&str> = found.iter().map(|region| region.label).collect();
        assert!(labels.contains(&"inline_formula"), "{labels:?}");
        assert!(labels.contains(&"display_formula"), "{labels:?}");
        assert!(
            !labels.contains(&"table"),
            "a table at 0.35 is below SCORE_THRESHOLD and the formula cut is not its cut: {labels:?}"
        );
        // And the other way: at 0.6 the table is believed and the formulas
        // would be too, so this is the cut and not an ordering accident.
        let mut high = Vec::new();
        high.extend(row(TABLE, 0.6, 200.0, 0.0, 300.0, 100.0));
        assert_eq!(decode(&whole(&high), &recipe).len(), 1);
    }

    /// The graph returns every one of its queries, and the ones it does not
    /// believe carry boxes that run off the square or invert.
    #[test]
    fn a_box_running_past_the_page_is_clamped_and_an_empty_one_is_dropped() {
        let mut rows = Vec::new();
        rows.extend(row(TABLE, 0.9, -40.0, -10.0, 900.0, 810.0));
        rows.extend(row(TABLE, 0.9, 400.0, 400.0, 400.0, 500.0));
        let found = decode(&whole(&rows), &baseline());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bbox.x, 0.0);
        assert_eq!(found[0].bbox.y, 0.0);
        assert_eq!(found[0].bbox.width, 1.0);
        assert_eq!(found[0].bbox.height, 1.0);
    }

    /// Corners in either order describe the same rectangle.
    #[test]
    fn an_inverted_box_is_read_as_the_rectangle_it_describes() {
        let found = decode(
            &whole(&row(TABLE, 0.9, 600.0, 600.0, 200.0, 400.0)),
            &baseline(),
        );
        assert_eq!(found[0].bbox.x, 0.25);
        assert_eq!(found[0].bbox.y, 0.5);
    }

    #[test]
    fn a_class_this_build_has_no_name_for_is_dropped_rather_than_indexed_with() {
        let recipe = baseline();
        assert!(decode(&whole(&row(99.0, 0.9, 0.0, 0.0, 100.0, 100.0)), &recipe).is_empty());
        assert!(decode(&whole(&row(-1.0, 0.9, 0.0, 0.0, 100.0, 100.0)), &recipe).is_empty());
    }

    // -----------------------------------------------------------------------
    // The tiles
    // -----------------------------------------------------------------------

    /// 2x2 with no overlap, 3x3 with 50% overlap, and the far edge covered in
    /// both — a strip of page no tile reached would be a strip where the
    /// formula classes quietly went back to whole-page resolution.
    #[test]
    fn the_tiles_cover_the_render_square_and_the_page_pass_leads() {
        let windows = tiled(FORMULA_THRESHOLD).windows();
        assert_eq!(windows.len(), 1 + 4);
        assert!(windows[0].whole_page);
        assert_eq!(windows[0].side, 2 * SIDE);
        assert!(windows[1..].iter().all(|w| !w.whole_page && w.side == SIDE));
        let corners: Vec<(u32, u32)> = windows[1..].iter().map(|w| (w.x, w.y)).collect();
        assert_eq!(corners, vec![(0, 0), (800, 0), (0, 800), (800, 800)]);

        let overlapped = Recipe {
            tile_stride: Some(400),
            ..tiled(FORMULA_THRESHOLD)
        };
        assert_eq!(overlapped.windows().len(), 1 + 9);

        // A stride the render side is not a multiple of still reaches the far
        // edge, because the last offset is pinned there.
        assert_eq!(tile_offsets(2400, 700), vec![0, 700, 1400, 1600]);
        assert_eq!(*tile_offsets(2400, 700).last().unwrap(), 2400 - SIDE);

        // A render at the graph's own square has nothing to tile.
        assert_eq!(Recipe::whole_page(0.5).windows().len(), 1);
    }

    /// A tile's own pixels are page pixels at a known offset, and that is the
    /// whole of the map back.
    #[test]
    fn a_detection_in_a_tile_lands_where_the_tile_sits_on_the_page() {
        // Bottom-right tile of a 1600 square: its (200, 400) is the render's
        // (1000, 1200), which is (0.625, 0.75) of the page.
        let passes = vec![tile(
            800,
            800,
            &row(DISPLAY, 0.9, 200.0, 400.0, 600.0, 600.0),
        )];
        let found = decode(&passes, &tiled(0.5));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bbox.x, 0.625);
        assert_eq!(found[0].bbox.y, 0.75);
        assert_eq!(found[0].bbox.width, 0.25);
        assert_eq!(found[0].bbox.height, 0.125);
    }

    /// A tile is not a page, so nothing but a formula is believed from one.
    #[test]
    fn a_tile_speaks_only_for_the_formula_classes() {
        let passes = vec![tile(0, 0, &row(TABLE, 0.99, 100.0, 100.0, 700.0, 700.0))];
        assert!(decode(&passes, &tiled(0.5)).is_empty());
    }

    /// The seam case: a formula the 2x2 tiling cut in half comes out as one
    /// box spanning the cut, not as two fragments each ending at 800.
    #[test]
    fn a_formula_straddling_a_tile_seam_comes_back_as_one_box() {
        let passes = vec![
            // Runs to the right edge of the left tile...
            tile(0, 0, &row(INLINE, 0.9, 700.0, 300.0, 800.0, 340.0)),
            // ...and continues from the left edge of the right one.
            tile(800, 0, &row(INLINE, 0.8, 0.0, 300.0, 120.0, 340.0)),
        ];
        let found = decode(&passes, &tiled(0.5));
        assert_eq!(found.len(), 1, "two halves of one expression: {found:?}");
        assert_eq!(found[0].bbox.x, 700.0 / 1600.0);
        assert_eq!(found[0].bbox.x + found[0].bbox.width, 920.0 / 1600.0);
        assert_eq!(found[0].score, 0.9, "the surer half names the merge");
    }

    /// A merge must not lose the seam it still sits on.
    ///
    /// The whole-page pass proposes its own box for a straddling expression
    /// and, at half the resolution, it routinely stops short of the seam. That
    /// box overlaps the left fragment enough to be merged with it — and if the
    /// merge took the sides both members agreed on, the union would come out
    /// *uncut* at a right edge the left fragment placed exactly on the tile
    /// boundary. The fragment on the other side then has nothing to join, and
    /// one expression is handed to the recognizer as two halves.
    #[test]
    fn a_fragment_that_placed_the_seam_edge_keeps_its_cut_through_a_merge() {
        let passes = vec![
            Pass {
                window: Window {
                    x: 0,
                    y: 0,
                    side: 2 * SIDE,
                    whole_page: true,
                },
                // Render 675..775: the same expression, seen whole-page, and
                // stopping 25 px short of the seam at 800.
                rows: row(INLINE, 0.9, 337.5, 300.0, 387.5, 340.0).to_vec(),
            },
            // The left fragment, running to the seam and cut by it.
            tile(0, 0, &row(INLINE, 0.7, 700.0, 600.0, 800.0, 680.0)),
            // ...and the right half, from the tile the other side of it.
            tile(800, 0, &row(INLINE, 0.65, 0.0, 600.0, 100.0, 680.0)),
        ];
        let found = decode(&passes, &tiled(0.5));
        assert_eq!(
            found.len(),
            1,
            "the two halves were not rejoined: {found:?}"
        );
        assert_eq!(found[0].bbox.x, 675.0 / 1600.0);
        assert_eq!(found[0].bbox.x + found[0].bbox.width, 900.0 / 1600.0);
    }

    /// Two expressions that merely end where the other begins are two
    /// expressions. The evidence for joining is a tile boundary, not proximity.
    #[test]
    fn two_expressions_meeting_inside_one_tile_are_not_joined() {
        let mut rows = Vec::new();
        rows.extend(row(INLINE, 0.9, 300.0, 300.0, 400.0, 340.0));
        rows.extend(row(INLINE, 0.9, 400.0, 300.0, 500.0, 340.0));
        let found = decode(&vec![tile(0, 0, &rows)], &tiled(0.5));
        assert_eq!(found.len(), 2, "{found:?}");
    }

    /// The page's own edge is not a seam: a formula that ends at the bottom of
    /// the page is whole, and is not waiting for a fragment to join it.
    #[test]
    fn the_render_squares_own_edge_is_not_a_cut() {
        let passes = vec![
            tile(800, 800, &row(INLINE, 0.9, 700.0, 700.0, 800.0, 800.0)),
            tile(0, 0, &row(INLINE, 0.9, 10.0, 10.0, 40.0, 40.0)),
        ];
        let found = decode(&passes, &tiled(0.5));
        assert_eq!(found.len(), 2);
        // Nothing was stretched to the far corner by a phantom join.
        assert!(found.iter().all(|r| r.bbox.width < 0.1), "{found:?}");
    }

    /// Overlapping tiles see the same expression twice, and one of the two may
    /// have caught only the part inside its own tile. One crop comes out.
    #[test]
    fn the_same_expression_seen_from_two_overlapping_tiles_is_one_region() {
        let recipe = Recipe {
            tile_stride: Some(400),
            ..tiled(0.5)
        };
        let passes = vec![
            // The whole expression, comfortably inside the middle tile.
            tile(400, 0, &row(INLINE, 0.85, 300.0, 300.0, 500.0, 340.0)),
            // The same expression, clipped by the left tile's right edge.
            tile(0, 0, &row(INLINE, 0.7, 700.0, 300.0, 800.0, 340.0)),
        ];
        let found = decode(&passes, &recipe);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].bbox.x, 700.0 / 1600.0);
        assert_eq!(found[0].bbox.x + found[0].bbox.width, 900.0 / 1600.0);
    }

    /// A whole-page `inline_formula` that swallowed a line must not eat the
    /// tile detections inside it: those are not fragments, they are the
    /// expressions the tiling exists to find.
    ///
    /// Which of the three is then cropped is not decided here — see
    /// [`ABSORB_SHARE`]. `extract::pdf::typeset::regions` gives the line's
    /// words to the largest box, and the smaller ones inside it are dropped
    /// there for sitting inside an area already spoken for. A merge rule that
    /// dropped the container instead was measured against exactly this case
    /// and came out a label or two behind, so what this test asserts is what
    /// the tree does.
    #[test]
    fn a_large_box_does_not_absorb_the_unclipped_detections_inside_it() {
        let passes = vec![
            Pass {
                window: Window {
                    x: 0,
                    y: 0,
                    side: 2 * SIDE,
                    whole_page: true,
                },
                rows: row(INLINE, 0.9, 100.0, 150.0, 700.0, 175.0).to_vec(),
            },
            tile(0, 0, &row(INLINE, 0.8, 300.0, 310.0, 360.0, 340.0)),
            // Render (900, 310)..(960, 340): the same line, the next tile
            // along, and nowhere near that tile's edges either.
            tile(800, 0, &row(INLINE, 0.8, 100.0, 310.0, 160.0, 340.0)),
        ];
        let found = decode(&passes, &tiled(0.5));
        assert_eq!(found.len(), 3, "{found:?}");
    }

    /// One area proposed as both classes is one crop and one recognizer call.
    #[test]
    fn an_area_proposed_as_both_formula_classes_is_kept_once() {
        let mut rows = Vec::new();
        rows.extend(row(DISPLAY, 0.9, 200.0, 200.0, 600.0, 300.0));
        rows.extend(row(INLINE, 0.6, 205.0, 202.0, 595.0, 298.0));
        let found = decode(&whole(&rows), &Recipe::whole_page(0.5));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].label, "display_formula", "the surer class wins");
    }

    /// Only the kinds worth displacing a page's own glyphs are routed. Every
    /// other class is named and counted and reaches no recognizer.
    #[test]
    fn only_the_kinds_that_supersede_glyphs_are_routed() {
        for label in LABELS {
            if let Some(kind) = kind_of(label) {
                assert!(
                    kind.supersedes_native_glyphs(),
                    "{label} routes to {kind:?}, which would be refused on arrival"
                );
            }
        }
        assert_eq!(kind_of("display_formula"), Some(RegionKind::Formula));
        assert_eq!(kind_of("table"), Some(RegionKind::Table));
        assert_eq!(kind_of("chart"), Some(RegionKind::Chart));
        // Both formula classes route. An inline expression is a formula that
        // happens to share its line with prose, and a region owns words.
        assert_eq!(kind_of("inline_formula"), Some(RegionKind::Formula));
        // Prose is not routed: the page's own glyphs are the better evidence
        // for it, and admission would refuse the transcription anyway.
        assert_eq!(kind_of("text"), None);
        assert_eq!(kind_of("paragraph_title"), None);
        assert_eq!(kind_of("formula_number"), None);
    }

    // -----------------------------------------------------------------------
    // The tile gate
    // -----------------------------------------------------------------------

    /// The gate reads the *unthresholded* best formula score, because the page
    /// it exists to let through is the page the threshold rejects.
    #[test]
    fn the_gate_reads_the_best_formula_query_at_no_threshold() {
        let mut rows = Vec::new();
        // Well under `FORMULA_THRESHOLD`, and it is still what the gate sees.
        rows.extend(row(INLINE, 0.31, 0.0, 0.0, 100.0, 100.0));
        rows.extend(row(DISPLAY, 0.22, 200.0, 0.0, 300.0, 100.0));
        assert_eq!(best_formula_score(&rows), 0.31);
        // A non-formula class is not the gate's evidence, however sure of it
        // the detector is: the tiles read the formula classes and nothing else.
        let table = row(TABLE, 0.99, 0.0, 0.0, 100.0, 100.0).to_vec();
        assert_eq!(best_formula_score(&table), 0.0);
        // No rows, a class this build cannot name, and a NaN score are all
        // "the pass proposed no formula", which is a closed gate.
        assert_eq!(best_formula_score(&[]), 0.0);
        assert_eq!(
            best_formula_score(&row(99.0, 0.9, 0.0, 0.0, 10.0, 10.0)),
            0.0
        );
        assert_eq!(
            best_formula_score(&row(INLINE, f32::NAN, 0.0, 0.0, 10.0, 10.0)),
            0.0
        );
    }

    /// The floor sits inside the gap the fixture measured, and on the side of
    /// it that keeps a page with mathematics on the tiled path. The two
    /// numbers are the fixture's own, quoted in [`TILE_GATE_FLOOR`]; this is
    /// what stops the constant being moved without the measurement.
    #[test]
    fn the_floor_lies_between_the_two_populations_the_fixture_measured() {
        const LOWEST_LABELLED_PAGE: f32 = 0.3884;
        const HIGHEST_UNLABELLED_PAGE: f32 = 0.0519;
        assert!(
            TILE_GATE_FLOOR > HIGHEST_UNLABELLED_PAGE,
            "a floor at or below {HIGHEST_UNLABELLED_PAGE} gates no prose page and buys nothing"
        );
        assert!(
            TILE_GATE_FLOOR < LOWEST_LABELLED_PAGE,
            "a floor at or above {LOWEST_LABELLED_PAGE} takes MMET02-01_E#21 off the tiled path"
        );
        // And it is the production recipe that carries it. A recipe with the
        // constant defined and unused would be a gate nobody runs.
        assert_eq!(Recipe::PRODUCTION.tile_gate, Some(TILE_GATE_FLOOR));
        // The whole-page recipe has no tiles to withhold and says so.
        assert_eq!(Recipe::whole_page(0.5).tile_gate, None);
    }

    /// A closed gate is fewer passes and not fewer regions from the passes
    /// that ran: the whole-page pass still speaks for every class it always
    /// did, at the thresholds it always did.
    #[test]
    fn a_gated_page_still_decodes_its_whole_page_pass_in_full() {
        let mut rows = Vec::new();
        rows.extend(row(TABLE, 0.9, 200.0, 400.0, 600.0, 600.0));
        rows.extend(row(INLINE, 0.6, 100.0, 100.0, 200.0, 150.0));
        // The 1600 px production window, carrying its rows in graph pixels.
        let page = vec![Pass {
            window: Window {
                x: 0,
                y: 0,
                side: 2 * SIDE,
                whole_page: true,
            },
            rows,
        }];
        // Only the whole-page pass was run — the gate withheld the tiles — and
        // decode is handed exactly that.
        let found = decode(&page, &Recipe::PRODUCTION);
        let labels: Vec<&str> = found.iter().map(|region| region.label).collect();
        assert!(labels.contains(&"table"), "{labels:?}");
        assert!(labels.contains(&"inline_formula"), "{labels:?}");
    }

    /// The gate names itself in the recipe, and therefore in the identity: a
    /// page read with the tiles withheld is not the reading the ungated recipe
    /// would have produced there, and the identity is what makes the old one
    /// be taken again.
    #[test]
    fn the_gate_is_named_in_the_recipe_and_a_recipe_without_it_reads_differently() {
        let gated = Recipe::PRODUCTION;
        let ungated = Recipe {
            tile_gate: None,
            ..gated
        };
        assert!(
            gated
                .name()
                .contains(&format!("tilegate-{TILE_GATE_FLOOR}")),
            "{}",
            gated.name()
        );
        assert!(
            ungated.name().contains("tilegate-none"),
            "{}",
            ungated.name()
        );
        assert_ne!(gated.name(), ungated.name());
        // A recipe with no tiles reports no gate whatever the field holds:
        // there is nothing for a floor to withhold, and a name that claimed
        // otherwise would split one reading into two recipes.
        let untiled = Recipe {
            tile_stride: None,
            tile_gate: Some(TILE_GATE_FLOOR),
            ..gated
        };
        assert!(
            untiled.name().contains("tilegate-none"),
            "{}",
            untiled.name()
        );
    }

    #[test]
    fn the_identity_names_the_graph_its_revision_and_the_whole_recipe() {
        let id = identity();
        assert!(id.contains(MODEL_ID), "{id}");
        assert!(id.contains(REVISION), "{id}");
        assert!(id.contains(&SCORE_THRESHOLD.to_string()), "{id}");
        // The render side, the tiling and the formula cut are all in it: a
        // reading taken under one of these is not a reading taken under
        // another, and the identity is what makes the old ones be re-read.
        assert!(
            id.contains(&format!("{RENDER_SIDE}px-{DOWNSAMPLE_NAME}")),
            "{id}"
        );
        assert!(id.contains(&format!("tiles-{SIDE}/{TILE_STRIDE}")), "{id}");
        assert!(id.contains(&format!("tilegate-{TILE_GATE_FLOOR}")), "{id}");
        assert!(id.contains(&format!("formula-{FORMULA_THRESHOLD}")), "{id}");
        // And the merge's own constants. Each of them decides which of two
        // boxes over one expression comes out of `decode`, so a reading taken
        // under one set is not a reading taken under another.
        assert!(id.contains(&format!("merge-iou-{MERGE_IOU}")), "{id}");
        assert!(id.contains(&format!("absorb-{ABSORB_SHARE}")), "{id}");
        assert!(
            id.contains(&format!("seam-{SEAM_EPS}px/{SEAM_OVERLAP}")),
            "{id}"
        );
        assert!(id.contains(&format!("cross-{CROSS_CLASS_IOU}")), "{id}");
        assert_ne!(
            id,
            format!(
                "{MODEL_ID}+{REPO}@{REVISION}+{}+{}+labels-{}",
                Recipe::whole_page(SCORE_THRESHOLD).name(),
                merge_rule(),
                LABELS.len()
            ),
            "the tiled recipe must not read as the whole-page one"
        );
    }

    /// `input_side` is what the host renders at and `graph_side` is what the
    /// graph is fed. They were the same number and are not any more, and the
    /// two callers that ask are asking different questions.
    #[test]
    fn the_render_side_and_the_graph_side_are_different_questions() {
        assert_eq!(DocLayout::input_side(), RENDER_SIDE);
        assert_eq!(DocLayout::graph_side(), SIDE);
        assert_eq!(DocLayout::input_side() % DocLayout::graph_side(), 0);
    }

    /// The label order is the graph's class order. A test rather than a
    /// comment, because the one thing that would silently re-label a library
    /// is this list drifting from the pinned revision's `config.json`.
    #[test]
    fn the_label_order_is_the_pinned_revisions_own() {
        assert_eq!(LABELS.len(), 25);
        assert_eq!(LABELS[0], "abstract");
        assert_eq!(LABELS[3], "chart");
        assert_eq!(LABELS[5], "display_formula");
        assert_eq!(LABELS[15], "inline_formula");
        assert_eq!(LABELS[21], "table");
        assert_eq!(LABELS[22], "text");
        assert_eq!(LABELS[24], "vision_footnote");
    }
}
