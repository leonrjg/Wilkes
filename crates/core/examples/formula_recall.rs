//! How much of a page's mathematics does the layout detector actually find?
//!
//! `inline_probe` and `formula_probe` answered what happens *after* a formula
//! is marked out — whether a crop can be read. This answers the question in
//! front of that one, and the one the complaint is about: of the expressions a
//! page actually draws, how many does PP-DocLayoutV2 propose at all, and at
//! what score.
//!
//!     cargo run --release --example formula_recall -- <labels.json> \
//!         [--tiling 800:none] [--tiling 1600:800] \
//!         [--threshold 0.5] [--iou 0.5] [--per-page] [--dump-dir DIR] \
//!         [--texify FILE]
//!
//! With no `--tiling` given it runs two recipes and prints them side by side:
//! the whole-page baseline the detector had before the formula tiles, and
//! whatever [`doclayout::Recipe::PRODUCTION`] currently is. `--tiling R:S`
//! names another — a page rendered at R px, formula tiles every S px, or
//! `none` for the whole-page pass alone — and may be given repeatedly. Every
//! recipe named is detected and scored in the same run, over the same fixture
//! and the same matcher, because a before and an after taken from two runs are
//! two numbers and not a comparison.
//!
//! `--threshold` names an operating point for the two formula classes and may
//! be given repeatedly; with none given the table is printed at 0.5, 0.4, 0.3
//! and 0.2. The graph returns every query and the cut is applied in Rust, so a
//! threshold costs a decode and not a detection: every threshold named is
//! scored against the same passes, and eight of them cost what one costs.
//!
//! The page is rendered by [`typeset::render_page`] and the passes are run and
//! decoded by [`doclayout::DocLayout::passes`] and [`doclayout::decode`],
//! which are what extraction itself does. Reimplementing either would make
//! this a measurement of a detector nobody runs.
//!
//! It is then scored at two stages, and both are reported. The first is
//! `decode`: what the detector proposed. The second is what
//! [`typeset::regions`] makes of the same detections once they have claimed
//! the page's words — the crops that are actually rendered and paid 0.3 s of
//! Texify for, and a different set, because a detection whose words another
//! detection already claimed is dropped there and one that covers no word at
//! all is kept and anchored. Recall measured at the first and paid for at the
//! second is a number about nothing.
//!
//! `--texify FILE` adds a third stage, over one recipe only: it renders the
//! crops that cover no word of the page — the ones v9 anchors rather than
//! drops — with [`typeset::render`], reads each with the real recognizer, and
//! reports what came back beside how big the crop was and how much ink it
//! held. Nothing there rejects anything; it is a measurement of what admission
//! currently accepts, written out per crop to `FILE`.
//!
//! Loads its models in this process. The application is forbidden from doing
//! that — see the "no inference in the host process" invariant in `AGENTS.md`
//! — but a probe *is* the model's process, and Ctrl-C is the kill. Not
//! precedent for anything under `src/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use image::{Rgb, RgbImage};
use mupdf::text_page::TextBlockType;
use mupdf::{Colorspace, Device, Document, IRect, Matrix, TextPageFlags};

use wilkes_core::extract::image::doclayout::{self, DocLayout, Pass, Recipe};
use wilkes_core::extract::image::{decode, LayoutRegion};
use wilkes_core::extract::pdf::typeset::{self, PageSurvey, WordBox};
use wilkes_core::types::{BoundingBox, RegionKind};

/// The classes this probe counts as a formula proposal. `formula_number` is
/// deliberately not one: an equation tag is a label beside a formula, not the
/// formula, and counting it would flatter the numbers.
const FORMULA_CLASSES: [&str; 2] = ["inline_formula", "display_formula"];

const SWEEP: [f32; 4] = [0.5, 0.4, 0.3, 0.2];

/// How much of a label a detection of any formula class must cover for the
/// label to count as reached by the crop that detection would produce.
const COVERAGE: f32 = 0.5;

/// Pixels on the longest side of a dumped page. Big enough that a 9 pt inline
/// expression is legible in the dump, which is the whole point of the dump.
const DUMP_LONGEST_PX: f32 = 1700.0;

/// Label widths, in the graph's own pixels, that the size table splits on.
///
/// The graph is fed an 800 px square whatever the page was rendered at, so an
/// expression's size *there* is what the detector has to find it by — and it
/// is the number the page's own points do not tell you, because a 612 pt page
/// and a 595 pt page shrink by different factors.
///
/// Measured against [`DocLayout::graph_side`] and deliberately not against the
/// render side, so a bucket means the same thing under every recipe. Under the
/// tiled recipes an expression in this bucket does reach the graph larger than
/// the bucket says — that is the point of them — and a table whose rows moved
/// with the recipe would make the before and the after unreadable against each
/// other.
const SIZE_EDGES: [f32; 5] = [8.0, 16.0, 32.0, 64.0, 128.0];

// ---------------------------------------------------------------------------
// What the page's own text layer already says
// ---------------------------------------------------------------------------
//
// A recall number counts labels, and not every label is a loss. A lone italic
// `n` that the PDF records as the glyph `n` is read correctly by the text
// layer already; a crop of it handed to a recognizer would, at best, produce
// the same letter. What a recognizer is *for* is an expression the text layer
// reads wrongly or not at all — two glyphs whose sub/superscript structure the
// layer flattens, a symbol drawn as vector paths with no glyph record at all,
// a glyph whose font encoding names nothing.
//
// So every label is classified at run time from the document itself, never
// from the fixture's hand labels, and the tables are split on that class.

/// Pixels per point the ink check rasterizes a page at.
///
/// Four is enough that a 9 pt glyph's stroke is two or three pixels wide, so a
/// vector-drawn symbol leaves a countable blob, and small enough that a page
/// costs a few tens of megabytes to look at.
const INK_SCALE: f32 = 4.0;

/// A rendered pixel this dark or darker is ink. Mid-grey: page paper is 255
/// and a glyph's core is near 0, so only an antialiased edge sits near this.
const INK_LEVEL: u8 = 128;

/// How far a text-layer glyph's quad is grown, in points, before the ink
/// inside it is called accounted for.
///
/// A quad is where MuPDF says the character sits; the antialiased skirt of the
/// stroke lands a fraction of a point outside it, and counting that skirt as
/// unaccounted would make every glyph look like it had something drawn beside
/// it.
const GLYPH_MARGIN_POINTS: f32 = 0.5;

/// How many unaccounted dark pixels a label must hold for its ink to count as
/// present.
///
/// 25 pixels at [`INK_SCALE`] is about 1.6 pt² of ink — a quarter of what a
/// 9 pt italic letter puts on the page, and far more than the margin above
/// leaves behind. Below it a label is ink-free; a fraction bar, a radical or a
/// vector-drawn operator is well above it.
const INK_PIXELS: u32 = 25;

/// How much of a glyph's quad a label must cover for the glyph to count as
/// *in* the label, when the quad's centre is not.
///
/// The two tests together, because neither survives alone. The fixture's boxes
/// are drawn tight to the ink; MuPDF's quads are as tall as the line, ascender
/// to descender, whatever the letter — so a box around `w = .10` covers well
/// under half of each quad it holds, and an area test alone finds no glyphs in
/// it at all. A centre test alone is what fails on a box drawn around a
/// superscript, whose quad's centre sits down on the baseline outside it.
const GLYPH_INSIDE: f32 = 0.5;

/// What the text layer can and cannot do for one label.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Class {
    /// The text layer's reading of this label is wrong or absent.
    Needs,
    /// One mapped glyph, nothing drawn beside it: the layer has it already.
    Suffices,
    /// No glyphs and no ink. Not a class, a complaint — see `Reason::Blank`.
    Blank,
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Needs => "needs_recog",
            Class::Suffices => "text_layer",
            Class::Blank => "blank",
        }
    }
}

/// Why a label landed in its class. The first of these that holds, in this
/// order, so the counts partition the labels.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Reason {
    /// Two or more glyphs: whatever structure sits between them — a
    /// subscript, a fraction, an operator — the layer writes as a flat run.
    ManyGlyphs,
    /// A glyph the font encoding names nothing for. `texify.rs`'s own example.
    Unmapped,
    /// Ink and no glyphs at all: drawn as vector paths, with no glyph record.
    InkNoGlyphs,
    /// One glyph and ink it does not account for: something is drawn over or
    /// around the letter that the layer has no record of.
    OneGlyphInk,
    /// Neither glyph nor ink. Nothing is there to read, by either route.
    Blank,
    /// One mapped glyph, no unaccounted ink.
    Suffices,
}

impl Reason {
    fn name(self) -> &'static str {
        match self {
            Reason::ManyGlyphs => ">=2 glyphs",
            Reason::Unmapped => "unmapped glyph",
            Reason::InkNoGlyphs => "ink, 0 glyphs",
            Reason::OneGlyphInk => "1 glyph + ink",
            Reason::Blank => "no glyph, no ink",
            Reason::Suffices => "1 mapped glyph",
        }
    }

    fn class(self) -> Class {
        match self {
            Reason::ManyGlyphs | Reason::Unmapped | Reason::InkNoGlyphs | Reason::OneGlyphInk => {
                Class::Needs
            }
            Reason::Blank => Class::Blank,
            Reason::Suffices => Class::Suffices,
        }
    }
}

/// One label, as the document's own text layer answers for it.
#[derive(Clone, Copy)]
struct Reading {
    glyphs: usize,
    unmapped: usize,
    /// Glyph quads that touch the label at all, whether or not they are in it.
    /// Only ever reported, never classified on: it is what tells a label with
    /// nothing in it apart from a label whose glyphs the containment test
    /// refused.
    touching: usize,
    /// Dark pixels inside the label that no glyph quad accounts for.
    ink: u32,
    reason: Reason,
}

impl Reading {
    fn class(&self) -> Class {
        self.reason.class()
    }
}

/// One character of the page's text layer.
struct Glyph {
    quad: Rect,
    /// Whether the font encoding named a character for it.
    mapped: bool,
}

/// Read one page's labels against the page's own text layer and its ink.
///
/// The text layer is asked for through the same MuPDF call and the same flags
/// `extract::pdf::mupdf` reads a document with, so what is counted here is
/// what the reading would have held.
fn classify_page(page: &mupdf::Page, entry: &Entry) -> anyhow::Result<Vec<Reading>> {
    let text_page = page.to_text_page(TextPageFlags::ACCURATE_BBOXES)?;
    let mut glyphs: Vec<Glyph> = Vec::new();
    for block in text_page.blocks() {
        for line in block.lines() {
            for ch in line.chars() {
                // `None` is a code point MuPDF could not even form; U+FFFD is
                // the replacement character it substitutes when the font's
                // encoding names nothing. Both are a glyph on the page that
                // the reading cannot say the identity of.
                let character = ch.char();
                if character.is_some_and(char::is_whitespace) {
                    continue;
                }
                let mapped = character.is_some_and(|c| c != '\u{FFFD}');
                let q = ch.quad();
                let quad = Rect {
                    x0: q.ul.x.min(q.ll.x),
                    y0: q.ul.y.min(q.ur.y),
                    x1: q.ur.x.max(q.lr.x),
                    y1: q.ll.y.max(q.lr.y),
                };
                if quad.area() > 0.0 {
                    glyphs.push(Glyph { quad, mapped });
                }
            }
        }
    }

    let canvas = IRect {
        x0: 0,
        y0: 0,
        x1: (entry.page_width * INK_SCALE).ceil() as i32,
        y1: (entry.page_height * INK_SCALE).ceil() as i32,
    };
    let mut pixmap = mupdf::Pixmap::new_with_rect(&Colorspace::device_rgb(), canvas, false)?;
    pixmap.clear_with(0xff)?;
    let device = Device::from_pixmap(&pixmap)?;
    page.run(&device, &Matrix::new_scale(INK_SCALE, INK_SCALE))?;
    drop(device);
    let ink_page = decode(
        pixmap.width(),
        pixmap.height(),
        pixmap.n() as usize,
        pixmap.stride() as usize,
        pixmap.samples(),
    )
    .map_err(|reason| anyhow::anyhow!("the ink page did not decode: {reason}"))?
    .pixels;

    let mut out = Vec::with_capacity(entry.boxes.len());
    for label in &entry.boxes {
        let rect = Rect {
            x0: label.x0,
            y0: label.y0,
            x1: label.x1,
            y1: label.y1,
        };
        let mut inside = 0usize;
        let mut unmapped = 0usize;
        // Every glyph whose grown quad touches the label, whether or not it is
        // *in* the label: a neighbouring letter poking over the edge is ink
        // the text layer accounts for, and calling it unaccounted would make
        // every tight box look like it had something drawn beside it.
        let mut nearby: Vec<Rect> = Vec::new();
        for glyph in &glyphs {
            let grown = Rect {
                x0: glyph.quad.x0 - GLYPH_MARGIN_POINTS,
                y0: glyph.quad.y0 - GLYPH_MARGIN_POINTS,
                x1: glyph.quad.x1 + GLYPH_MARGIN_POINTS,
                y1: glyph.quad.y1 + GLYPH_MARGIN_POINTS,
            };
            if grown.intersection(&rect) > 0.0 {
                nearby.push(grown);
            }
            let centre = (
                (glyph.quad.x0 + glyph.quad.x1) / 2.0,
                (glyph.quad.y0 + glyph.quad.y1) / 2.0,
            );
            let centred = centre.0 >= rect.x0
                && centre.0 <= rect.x1
                && centre.1 >= rect.y0
                && centre.1 <= rect.y1;
            if centred || glyph.quad.intersection(&rect) >= GLYPH_INSIDE * glyph.quad.area() {
                inside += 1;
                if !glyph.mapped {
                    unmapped += 1;
                }
            }
        }

        let x0 = ((rect.x0 * INK_SCALE).floor().max(0.0)) as u32;
        let y0 = ((rect.y0 * INK_SCALE).floor().max(0.0)) as u32;
        let x1 = ((rect.x1 * INK_SCALE).ceil().max(0.0) as u32).min(ink_page.width());
        let y1 = ((rect.y1 * INK_SCALE).ceil().max(0.0) as u32).min(ink_page.height());
        let mut ink = 0u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let px = ink_page.get_pixel(x, y);
                if px[0] > INK_LEVEL && px[1] > INK_LEVEL && px[2] > INK_LEVEL {
                    continue;
                }
                let (px_x, px_y) = ((x as f32 + 0.5) / INK_SCALE, (y as f32 + 0.5) / INK_SCALE);
                let accounted = nearby.iter().any(|quad| {
                    px_x >= quad.x0 && px_x <= quad.x1 && px_y >= quad.y0 && px_y <= quad.y1
                });
                if !accounted {
                    ink += 1;
                }
            }
        }

        let reason = if inside >= 2 {
            Reason::ManyGlyphs
        } else if unmapped > 0 {
            Reason::Unmapped
        } else if inside == 0 && ink >= INK_PIXELS {
            Reason::InkNoGlyphs
        } else if inside == 1 && ink >= INK_PIXELS {
            Reason::OneGlyphInk
        } else if inside == 0 {
            Reason::Blank
        } else {
            Reason::Suffices
        };
        out.push(Reading {
            glyphs: inside,
            unmapped,
            touching: nearby.len(),
            ink,
            reason,
        });
    }
    Ok(out)
}

/// Classify every label in the fixture, once, before anything is detected.
fn classify_all(entries: &[Entry], names: &[String]) -> anyhow::Result<Vec<Vec<Reading>>> {
    let mut opened: Option<(PathBuf, Document)> = None;
    let mut out = Vec::with_capacity(entries.len());
    for (entry, name) in entries.iter().zip(names) {
        if opened
            .as_ref()
            .map(|(path, _)| *path != entry.pdf)
            .unwrap_or(true)
        {
            opened = Some((entry.pdf.clone(), Document::open(entry.pdf.as_path())?));
        }
        let document = &opened.as_ref().expect("a document was just opened").1;
        let page = document.load_page(entry.page)?;
        out.push(
            classify_page(&page, entry)
                .with_context(|| format!("{name}: could not read the text layer"))?,
        );
    }
    Ok(out)
}

/// Say what the classification found, and show the evidence the one tunable
/// constant rests on.
fn report_classes(entries: &[Entry], names: &[String], readings: &[Vec<Reading>]) {
    println!("══ what the page's own text layer already reads ══");
    println!(
        "needs_recognizer = (glyphs >= 2) or (an unmapped glyph) or (>= {INK_PIXELS} unaccounted \
         ink px with 0 glyphs)\n\
         {:18}or (1 glyph and >= {INK_PIXELS} unaccounted ink px); text_layer_suffices = exactly \
         one mapped glyph, no unaccounted ink",
        ""
    );
    println!(
        "ink at {INK_SCALE:.0} px/pt, a pixel is ink below {INK_LEVEL}, glyph quads grown \
         {GLYPH_MARGIN_POINTS} pt before their ink counts as accounted for;\n\
         a glyph is in a label when its quad's centre is, or when the label covers {:.0}% of \
         the quad\n",
        GLYPH_INSIDE * 100.0
    );

    let mut by_reason: BTreeMap<(&'static str, Reason), usize> = BTreeMap::new();
    let mut by_class: BTreeMap<(&'static str, Class), usize> = BTreeMap::new();
    for (entry, readings) in entries.iter().zip(readings) {
        for (label, reading) in entry.boxes.iter().zip(readings) {
            let kind = kind_name(&label.kind);
            *by_reason.entry((kind, reading.reason)).or_default() += 1;
            *by_class.entry((kind, reading.class())).or_default() += 1;
        }
    }

    println!(
        "{:<20} {:>8} {:>8} {:>8}",
        "reason", "inline", "display", "all"
    );
    for reason in [
        Reason::ManyGlyphs,
        Reason::Unmapped,
        Reason::InkNoGlyphs,
        Reason::OneGlyphInk,
        Reason::Blank,
        Reason::Suffices,
    ] {
        let inline = by_reason.get(&("inline", reason)).copied().unwrap_or(0);
        let display = by_reason.get(&("display", reason)).copied().unwrap_or(0);
        println!(
            "{:<20} {inline:>8} {display:>8} {:>8}",
            reason.name(),
            inline + display
        );
    }
    println!();
    println!(
        "{:<20} {:>8} {:>8} {:>8}",
        "class", "inline", "display", "all"
    );
    for class in [Class::Needs, Class::Suffices, Class::Blank] {
        let inline = by_class.get(&("inline", class)).copied().unwrap_or(0);
        let display = by_class.get(&("display", class)).copied().unwrap_or(0);
        println!(
            "{:<20} {inline:>8} {display:>8} {:>8}",
            class.name(),
            inline + display
        );
    }

    // The one number here that is a choice rather than a fact is INK_PIXELS,
    // and only the one-glyph labels are near it. Printing where they actually
    // fall says whether the cut is on a cliff or in a valley.
    let mut band = [0usize; 6];
    for (entry, readings) in entries.iter().zip(readings) {
        for reading in readings.iter().take(entry.boxes.len()) {
            if reading.glyphs != 1 {
                continue;
            }
            band[match reading.ink {
                0 => 0,
                1..=4 => 1,
                5..=12 => 2,
                13..=24 => 3,
                25..=99 => 4,
                _ => 5,
            }] += 1;
        }
    }
    println!(
        "\nunaccounted ink under the one-glyph labels (the only ones the {INK_PIXELS} px cut can move):"
    );
    for (index, edge) in ["0 px", "1-4", "5-12", "13-24", "25-99", ">=100"]
        .iter()
        .enumerate()
    {
        println!("  {edge:>6}: {}", band[index]);
    }

    let blank: Vec<String> = entries
        .iter()
        .zip(names)
        .zip(readings)
        .flat_map(|((entry, name), readings)| {
            entry
                .boxes
                .iter()
                .zip(readings)
                .filter(|(_, reading)| reading.reason == Reason::Blank)
                .map(|(label, reading)| {
                    format!(
                        "{name} ({:.1},{:.1})-({:.1},{:.1}) {:?}: {} glyph quads touch it, \
                         {} unaccounted ink px",
                        label.x0,
                        label.y0,
                        label.x1,
                        label.y1,
                        label.text,
                        reading.touching,
                        reading.ink
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    if !blank.is_empty() {
        println!(
            "\n{} label(s) hold neither a glyph nor ink — the fixture and the page disagree here, \
             and they are counted in neither class:",
            blank.len()
        );
        for line in &blank {
            println!("  {line}");
        }
    }
    println!();
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct Label {
    kind: String,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    #[serde(default)]
    text: String,
}

#[derive(serde::Deserialize)]
struct Entry {
    pdf: PathBuf,
    /// 0-based, as the fixture records it and as MuPDF's `load_page` wants it.
    page: i32,
    page_width: f32,
    page_height: f32,
    boxes: Vec<Label>,
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    fn area(&self) -> f32 {
        (self.x1 - self.x0).max(0.0) * (self.y1 - self.y0).max(0.0)
    }

    fn intersection(&self, other: &Rect) -> f32 {
        let w = (self.x1.min(other.x1) - self.x0.max(other.x0)).max(0.0);
        let h = (self.y1.min(other.y1) - self.y0.max(other.y0)).max(0.0);
        w * h
    }

    fn iou(&self, other: &Rect) -> f32 {
        let overlap = self.intersection(other);
        let union = self.area() + other.area() - overlap;
        if union <= 0.0 {
            0.0
        } else {
            overlap / union
        }
    }
}

/// A detection, mapped out of the detector's page fractions and back into the
/// page's own points — the space the fixture is written in and the space the
/// crop is later rendered from.
fn to_points(region: &LayoutRegion, width: f32, height: f32) -> Rect {
    Rect {
        x0: region.bbox.x * width,
        y0: region.bbox.y * height,
        x1: (region.bbox.x + region.bbox.width) * width,
        y1: (region.bbox.y + region.bbox.height) * height,
    }
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct Tally {
    labels: usize,
    detections: usize,
    matched: usize,
    covered: usize,
}

impl Tally {
    fn add(&mut self, other: &Tally) {
        self.labels += other.labels;
        self.detections += other.detections;
        self.matched += other.matched;
        self.covered += other.covered;
    }

    fn recall(&self) -> f32 {
        ratio(self.matched, self.labels)
    }

    fn coverage(&self) -> f32 {
        ratio(self.covered, self.labels)
    }

    fn precision(&self) -> f32 {
        ratio(self.matched, self.detections)
    }
}

fn ratio(part: usize, whole: usize) -> f32 {
    if whole == 0 {
        f32::NAN
    } else {
        part as f32 / whole as f32
    }
}

fn percent(value: f32) -> String {
    if value.is_nan() {
        "   -".to_string()
    } else {
        format!("{:>3.0}%", value * 100.0)
    }
}

/// What one detection sits over, once the labels are split by whether the
/// text layer already reads them.
///
/// Deliberately looser than the one-to-one matcher above, and for a different
/// question. The matcher asks whether an expression was *found*; this asks
/// what a crop would contain, and a crop that swallows half a label contains
/// that label whether or not the matcher gave it away to another detection.
/// The order is the order of usefulness: a detection over a label the text
/// layer cannot read is a useful hit however much else it covers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Over {
    Needs,
    Suffices,
    Nothing,
}

impl Over {
    const ALL: [Over; 3] = [Over::Needs, Over::Suffices, Over::Nothing];

    fn index(self) -> usize {
        match self {
            Over::Needs => 0,
            Over::Suffices => 1,
            Over::Nothing => 2,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Over::Needs => "over a needs_recognizer label",
            Over::Suffices => "over a text_layer_suffices label",
            Over::Nothing => "over nothing labelled",
        }
    }
}

/// One label, and everything the tables and the dump need to say about it.
struct LabelRow {
    rect: Rect,
    kind: &'static str,
    matched: bool,
    covered: bool,
    reading: Reading,
}

/// One page scored at one threshold.
struct PageScore {
    /// By kind: "inline", "display".
    by_kind: BTreeMap<&'static str, Tally>,
    /// By class and kind, for the split tables. The `detections` field of
    /// these is not filled: a detection belongs to a class by what it sits
    /// over, which is [`Over`] and is counted separately.
    by_class: BTreeMap<(Class, &'static str), Tally>,
    /// How many detections sat over each of the three things, in [`Over`]'s
    /// own order.
    over: [usize; 3],
    /// Which label each detection was matched to, for the dump. `None` is a
    /// detection nothing claimed.
    detections: Vec<(Rect, &'static str, f32, bool)>,
    /// Every label, in fixture order.
    labels: Vec<LabelRow>,
}

/// Which [`SIZE_EDGES`] bucket a label's width in detector pixels falls in.
fn bucket(width_px: f32) -> usize {
    SIZE_EDGES.iter().filter(|edge| width_px >= **edge).count()
}

fn bucket_name(index: usize) -> String {
    match index {
        0 => format!("     <{:.0}px", SIZE_EDGES[0]),
        n if n == SIZE_EDGES.len() => format!("   >={:.0}px", SIZE_EDGES[n - 1]),
        n => format!("{:>4.0}-{:.0}px", SIZE_EDGES[n - 1], SIZE_EDGES[n]),
    }
}

/// Greedy one-to-one matching by IoU, highest-scoring detection first.
///
/// Greedy and not Hungarian on purpose: the question is whether an expression
/// was found at all, and a matching that reassigned detections to squeeze out
/// one more pair would be measuring the matcher.
fn score(
    labels: &[Label],
    readings: &[Reading],
    found: &[(Rect, &'static str, f32)],
    iou_floor: f32,
) -> PageScore {
    let mut ranked: Vec<usize> = (0..found.len()).collect();
    ranked.sort_by(|a, b| {
        found[*b]
            .2
            .partial_cmp(&found[*a].2)
            .expect("no NaN scores")
    });

    let mut claimed_by: Vec<Option<usize>> = vec![None; labels.len()];
    let mut claims: Vec<Option<usize>> = vec![None; found.len()];
    for detection in ranked {
        let (rect, _, _) = &found[detection];
        let best = labels
            .iter()
            .enumerate()
            .filter(|(index, _)| claimed_by[*index].is_none())
            .map(|(index, label)| {
                let label_rect = Rect {
                    x0: label.x0,
                    y0: label.y0,
                    x1: label.x1,
                    y1: label.y1,
                };
                (index, rect.iou(&label_rect))
            })
            .filter(|(_, iou)| *iou >= iou_floor)
            .max_by(|a, b| a.1.partial_cmp(&b.1).expect("no NaN areas"));
        if let Some((index, _)) = best {
            claimed_by[index] = Some(detection);
            claims[detection] = Some(index);
        }
    }

    // The looser measure: any formula detection that swallows half a label is
    // a crop that still contains the expression, which is what the downstream
    // recognizer is handed.
    let mut by_kind: BTreeMap<&'static str, Tally> = BTreeMap::new();
    let mut by_class: BTreeMap<(Class, &'static str), Tally> = BTreeMap::new();
    let mut label_rows = Vec::new();
    for (index, label) in labels.iter().enumerate() {
        let rect = Rect {
            x0: label.x0,
            y0: label.y0,
            x1: label.x1,
            y1: label.y1,
        };
        let kind = kind_name(&label.kind);
        let matched = claimed_by[index].is_some();
        let covered = rect.area() > 0.0
            && found
                .iter()
                .any(|(d, _, _)| d.intersection(&rect) / rect.area() >= COVERAGE);
        let tally = by_kind.entry(kind).or_default();
        tally.labels += 1;
        tally.matched += usize::from(matched);
        tally.covered += usize::from(covered);
        let reading = readings[index];
        let tally = by_class.entry((reading.class(), kind)).or_default();
        tally.labels += 1;
        tally.matched += usize::from(matched);
        tally.covered += usize::from(covered);
        label_rows.push(LabelRow {
            rect,
            kind,
            matched,
            covered,
            reading,
        });
    }

    // What each detection sits over. A detection *contains* a label when it
    // covers at least [`COVERAGE`] of it — the crop this detection would
    // produce would hold that expression — or when the matcher gave that label
    // to it, which is the tighter test and is not implied by the looser one
    // when a detection is much smaller than its label.
    let mut over = [0usize; 3];
    for (detection, (rect, _, _)) in found.iter().enumerate() {
        let mut verdict = Over::Nothing;
        for (index, row) in label_rows.iter().enumerate() {
            let contains = row.rect.area() > 0.0
                && (rect.intersection(&row.rect) / row.rect.area() >= COVERAGE
                    || claims[detection] == Some(index));
            if !contains {
                continue;
            }
            match row.reading.class() {
                Class::Needs => {
                    verdict = Over::Needs;
                    break;
                }
                // A blank label is grouped here rather than with the true
                // false positives: the detector did sit over something the
                // fixture marked out, and calling that a false positive on
                // prose would be a stronger claim than the evidence. The
                // blank count is printed with the classes and is expected to
                // be zero.
                Class::Suffices | Class::Blank => verdict = Over::Suffices,
            }
        }
        over[verdict.index()] += 1;
    }

    // A detection is attributed to the kind of the label it claimed; an
    // unclaimed one is a false positive and is counted against the kind its
    // own class implies, so the two precisions add up to the whole.
    for (detection, (_, class, _)) in found.iter().enumerate() {
        let kind = match claims[detection] {
            Some(index) => kind_name(&labels[index].kind),
            None if *class == "inline_formula" => "inline",
            None => "display",
        };
        by_kind.entry(kind).or_default().detections += 1;
    }

    PageScore {
        by_kind,
        by_class,
        over,
        detections: found
            .iter()
            .enumerate()
            .map(|(index, (rect, class, confidence))| {
                (*rect, *class, *confidence, claims[index].is_some())
            })
            .collect(),
        labels: label_rows,
    }
}

fn kind_name(kind: &str) -> &'static str {
    match kind {
        "inline" => "inline",
        "display" => "display",
        other => panic!("the fixture holds a kind this probe does not know: {other}"),
    }
}

// ---------------------------------------------------------------------------
// What reaches the recognizer
// ---------------------------------------------------------------------------
//
// Everything above scores [`doclayout::decode`], which is what the detector
// *proposed*. It is not what Texify is handed. Between the two sits
// [`typeset::regions`]: it sorts the detections largest-area-first and lets
// each claim the page words it covers, and a word belongs to one region only.
// So a whole-page `inline_formula` drawn around a whole line takes that line's
// words, and the small precise boxes the tiles found inside it come out empty
// and never become a crop — a gain measured at `decode` can be given back
// here, before any pixels are rendered. A box that covers no word *and* sits
// inside nothing already claimed is the other case: the page drew that
// expression rather than setting it, and the box is kept and anchored into the
// reading.
//
// This runs the real function over word boxes built the way
// `extract_page_words` builds them, so what is counted is what would be
// cropped and paid 0.3 s of Texify for.

/// How much of a word must lie inside a crop for the crop to be reading it.
///
/// The same majority `typeset` claims words by — restated here because that
/// constant is private to the backend, and because this is a different
/// question anyway: `regions` asks which words a region *speaks for*, and this
/// asks which words are in the picture, whoever owns them.
const WORD_IN_CROP: f32 = 0.5;

/// How much of a crop's content may be page prose before the crop stops
/// counting as a clean read of the label inside it.
///
/// A crop is clean when the expression is what is in it. Half is generous —
/// a crop where every other word is prose is already a crop Texify will read
/// prose out of — and it is set there so the number cannot be accused of being
/// tuned to make the claiming stage look bad.
const CLEAN_PROSE_SHARE: f32 = 0.5;

/// Seconds of recognizer per crop, for the cost column. Measured by
/// `formula_probe`; the reason a crop count is a number anyone cares about.
const TEXIFY_SECONDS: f64 = 0.3;

/// One fixture page's survey and its own bounds, as [`typeset::regions`] wants
/// them.
struct PageWords {
    bounds: BoundingBox,
    survey: PageSurvey,
}

/// Close one word: the page's own segmentation, which is whitespace.
///
/// `flush` in `extract::pdf::mupdf`, and the survey beside it, character for
/// character: an empty run is not a word, a word with no drawable quad is a
/// word of the reading but not of the survey, and the word index counts words
/// of the reading either way.
fn flush_word(
    out: &mut Vec<WordBox>,
    block: usize,
    line: usize,
    word: &mut usize,
    chars: &mut usize,
    bbox: &mut Option<BoundingBox>,
) {
    if *chars == 0 {
        *bbox = None;
        return;
    }
    if let Some(bbox) = bbox.take() {
        out.push(WordBox {
            block,
            line,
            word: *word,
            bbox,
        });
    }
    *chars = 0;
    *word += 1;
}

/// The page's whitespace-delimited words, each with the merged box of the
/// characters in it, and where the page draws a picture.
///
/// The `(block, line, word)` triple a [`WordBox`] carries addresses a reading
/// this probe does not build, and nothing here reads it back: what
/// [`typeset::regions`] claims on is the box, and the box is the page's.
///
/// The picture areas are surveyed for the same reason extraction surveys them:
/// a detection that covers no word and sits inside one is part of that picture,
/// and `typeset::regions` drops it rather than anchoring a figure's
/// mathematics into the prose beside the figure.
fn page_words(text_page: &mupdf::TextPage) -> PageSurvey {
    let mut out = Vec::new();
    let mut drawn = Vec::new();
    for (block_index, block) in text_page.blocks().enumerate() {
        // An image block holds no words. It still numbers a block, exactly as
        // it does in the reading.
        if block.r#type() == TextBlockType::Image {
            let bounds = block.bounds();
            drawn.push(BoundingBox {
                x: bounds.x0,
                y: bounds.y0,
                width: (bounds.x1 - bounds.x0).max(0.0),
                height: (bounds.y1 - bounds.y0).max(0.0),
            });
            continue;
        }
        for (line_index, line) in block.lines().enumerate() {
            let mut word = 0usize;
            let mut chars = 0usize;
            let mut bbox: Option<BoundingBox> = None;
            for ch in line.chars() {
                // A code point MuPDF could not form is not a character of the
                // reading, and does not extend the word it fell in.
                let Some(c) = ch.char() else { continue };
                if c.is_whitespace() {
                    flush_word(
                        &mut out,
                        block_index,
                        line_index,
                        &mut word,
                        &mut chars,
                        &mut bbox,
                    );
                    continue;
                }
                chars += 1;
                let q = ch.quad();
                let x0 = q.ul.x.min(q.ll.x);
                let y0 = q.ul.y.min(q.ur.y);
                let x1 = q.ur.x.max(q.lr.x);
                let y1 = q.ll.y.max(q.lr.y);
                if x1 > x0 && y1 > y0 {
                    let next = BoundingBox {
                        x: x0,
                        y: y0,
                        width: x1 - x0,
                        height: y1 - y0,
                    };
                    bbox = Some(match &bbox {
                        Some(existing) => existing.merge(&next),
                        None => next,
                    });
                }
            }
            flush_word(
                &mut out,
                block_index,
                line_index,
                &mut word,
                &mut chars,
                &mut bbox,
            );
        }
    }
    PageSurvey { words: out, drawn }
}

/// Survey every fixture page's words, once, before anything is detected.
///
/// Under the flags `extract::pdf::mupdf` reads a document with, because the
/// words this stage claims have to be the words that stage would have.
fn survey_all(entries: &[Entry], names: &[String]) -> anyhow::Result<Vec<PageWords>> {
    let mut opened: Option<(PathBuf, Document)> = None;
    let mut out = Vec::with_capacity(entries.len());
    for (entry, name) in entries.iter().zip(names) {
        if opened
            .as_ref()
            .map(|(path, _)| *path != entry.pdf)
            .unwrap_or(true)
        {
            opened = Some((entry.pdf.clone(), Document::open(entry.pdf.as_path())?));
        }
        let document = &opened.as_ref().expect("a document was just opened").1;
        let page = document.load_page(entry.page)?;
        let bounds = page
            .bounds()
            .with_context(|| format!("{name}: the page reports no bounds"))?;
        let text_page = page
            .to_text_page(TextPageFlags::ACCURATE_BBOXES | TextPageFlags::PRESERVE_IMAGES)
            .with_context(|| format!("{name}: could not read the text layer"))?;
        out.push(PageWords {
            bounds: BoundingBox {
                x: bounds.x0,
                y: bounds.y0,
                width: bounds.x1 - bounds.x0,
                height: bounds.y1 - bounds.y0,
            },
            survey: page_words(&text_page),
        });
    }
    Ok(out)
}

/// What the claiming stage did to one recipe's detections, over the fixture.
#[derive(Default, Clone, Copy)]
struct Claiming {
    /// needs_recognizer labels seen.
    labels: usize,
    /// ...that a formula detection covered, at `decode`.
    decoded: usize,
    /// ...that a rendered Formula crop covers.
    reached: usize,
    /// ...reached by a crop holding that label and little else.
    clean: usize,
    /// ...covered at `decode` and reached by no crop: the loss this measures.
    lost: usize,
    /// The lost ones, by what became of them. The first that holds, in this
    /// order, so they partition [`Claiming::lost`].
    ///
    /// No page word is under the label at all: it is drawn, not typeset. Such
    /// a region is anchored rather than dropped, so what is left in this row
    /// is a drawn expression no formula crop reached for some *other* reason —
    /// it sits inside a picture the page draws, or inside a region already
    /// kept.
    lost_wordless: usize,
    /// A table or a chart claimed the words first. Its own reading covers the
    /// expression; a formula crop of it does not exist.
    lost_to_other_kind: usize,
    /// A formula crop is over the label but does not hold half of it: the
    /// words it kept are not the ones the label is drawn over.
    lost_crop_short: usize,
    /// Words under it, no larger region over it, and no crop: the detection
    /// that covered it was dropped for owning nothing of its own.
    lost_unclaimed: usize,
    /// Formula crops, and the pages they came off.
    crops: usize,
    pages: usize,
}

impl Claiming {
    fn add(&mut self, other: &Claiming) {
        self.labels += other.labels;
        self.decoded += other.decoded;
        self.reached += other.reached;
        self.clean += other.clean;
        self.lost += other.lost;
        self.lost_wordless += other.lost_wordless;
        self.lost_to_other_kind += other.lost_to_other_kind;
        self.lost_crop_short += other.lost_crop_short;
        self.lost_unclaimed += other.lost_unclaimed;
        self.crops += other.crops;
        self.pages += other.pages;
    }

    fn crops_per_page(&self) -> f64 {
        if self.pages == 0 {
            f64::NAN
        } else {
            self.crops as f64 / self.pages as f64
        }
    }
}

impl Rect {
    fn of(bbox: &BoundingBox) -> Rect {
        Rect {
            x0: bbox.x,
            y0: bbox.y,
            x1: bbox.x + bbox.width,
            y1: bbox.y + bbox.height,
        }
    }

    /// Whether a crop of `self` holds `other` — the same test the coverage
    /// column above uses, moved one stage down the pipeline.
    fn covers(&self, other: &Rect) -> bool {
        other.area() > 0.0 && self.intersection(other) / other.area() >= COVERAGE
    }
}

/// How much of what a crop holds is prose.
///
/// Deliberately generous to the crop: a word is prose only when it touches no
/// label at all. The fixture's boxes are drawn tight to the ink and a word box
/// is as tall as its line, so any share test between the two would count the
/// expression's own words as prose. The consequence is that "cleanly" is an
/// upper bound — the crops this calls clean include some that are not — and
/// the dirty count is therefore a floor.
fn prose_share(crop: &Rect, words: &[WordBox], labels: &[Label]) -> f32 {
    let (mut held, mut prose) = (0usize, 0usize);
    for word in words {
        let rect = Rect::of(&word.bbox);
        if rect.area() <= 0.0 || rect.intersection(crop) / rect.area() < WORD_IN_CROP {
            continue;
        }
        held += 1;
        let labelled = labels.iter().any(|label| {
            rect.intersection(&Rect {
                x0: label.x0,
                y0: label.y0,
                x1: label.x1,
                y1: label.y1,
            }) > 0.0
        });
        if !labelled {
            prose += 1;
        }
    }
    if held == 0 {
        // The crop of an expression the page drew as paths holds no word at
        // all, and none of what it holds is prose. Before a region could be
        // anchored this could not happen and the ratio was NaN, which the
        // caller read — correctly, then — as "not clean".
        return 0.0;
    }
    ratio(prose, held)
}

/// Run one page's detections through the real claiming stage and score the
/// crops it would produce.
fn claiming(
    entry: &Entry,
    survey: &PageWords,
    detections: &[LayoutRegion],
    rows: &[LabelRow],
) -> Claiming {
    let claimed = typeset::regions(
        (entry.page + 1) as u32,
        &survey.bounds,
        detections,
        &survey.survey,
    );
    let crops: Vec<Rect> = claimed
        .iter()
        .filter(|region| region.kind == RegionKind::Formula)
        .map(|region| Rect::of(&region.bbox))
        .collect();
    // The other routed kinds, for saying what became of a label no formula
    // crop covers: a table claims the words inside it before any formula
    // does, and reads them itself.
    let others: Vec<Rect> = claimed
        .iter()
        .filter(|region| region.kind != RegionKind::Formula)
        .map(|region| Rect::of(&region.bbox))
        .collect();
    // Once per crop rather than once per (crop, label): what a crop holds is
    // a fact about the crop.
    let prose: Vec<f32> = crops
        .iter()
        .map(|crop| prose_share(crop, &survey.survey.words, &entry.boxes))
        .collect();

    let mut tally = Claiming {
        crops: crops.len(),
        pages: 1,
        ..Claiming::default()
    };
    for (index, row) in rows.iter().enumerate() {
        if row.reading.class() != Class::Needs {
            continue;
        }
        tally.labels += 1;
        tally.decoded += usize::from(row.covered);
        let reached = crops.iter().any(|crop| crop.covers(&row.rect));
        tally.reached += usize::from(reached);
        if row.covered && !reached {
            tally.lost += 1;
            // Why, in the order the reasons exclude each other.
            let wordless = !survey
                .survey
                .words
                .iter()
                .any(|word| Rect::of(&word.bbox).intersection(&row.rect) > 0.0);
            if wordless {
                tally.lost_wordless += 1;
            } else if others.iter().any(|region| region.covers(&row.rect)) {
                tally.lost_to_other_kind += 1;
            } else if crops.iter().any(|crop| crop.intersection(&row.rect) > 0.0) {
                tally.lost_crop_short += 1;
            } else {
                tally.lost_unclaimed += 1;
            }
        }
        let clean = crops.iter().zip(&prose).any(|(crop, share)| {
            crop.covers(&row.rect)
                // Nothing else the recognizer would have to have read out of
                // the same crop...
                && !rows.iter().enumerate().any(|(other, candidate)| {
                    other != index
                        && candidate.reading.class() == Class::Needs
                        && crop.covers(&candidate.rect)
                })
                // ...and not mostly the sentence around it. A crop holding no
                // words at all is the crop of an expression the page drew
                // rather than set, and holds no prose by construction.
                && *share <= CLEAN_PROSE_SHARE
        });
        tally.clean += usize::from(clean);
    }
    tally
}

// ---------------------------------------------------------------------------
// What the recognizer makes of a crop with no glyphs under it
// ---------------------------------------------------------------------------
//
// The stage above counts crops. This one reads them, with the recognizer
// extraction runs and at the crop extraction renders — `typeset::render`, so
// the scale, the margin and the aspect padding are the production ones — and
// prints what came back beside how big the crop was and how much ink was in
// it.
//
// Only the *anchored* crops: the ones over no word of the page at all. Those
// are what v9 added, they are the ones whose reading nothing else supplies,
// and a crop the size of one drawn letter is the case a reader has to be able
// to judge. Nothing here rejects anything — admission is reported, not
// changed, and `plausible` below is a classifier for this table only.

/// Ink width, in points, under which a crop cannot be holding more than one
/// glyph.
///
/// A 9-10 pt body font sets a wide letter — `w`, `m` — about 8 pt across, and
/// two of them with the space between are over 14. Measured on the *ink* and
/// not on the crop, because the crop carries the render margin and a crop
/// width would say 12 pt of nothing at all.
const ONE_GLYPH_POINTS: f32 = 12.0;

/// What one anchored crop is, and what came back for it.
struct AnchoredRead {
    page: String,
    /// The crop as rendered: the page rectangle it covers, in points.
    crop: BoundingBox,
    pixels: (u32, u32),
    /// Dark pixels in the crop, and the tightest box around them in points.
    ink: u32,
    ink_width: f32,
    ink_height: f32,
    latex: String,
    confidence: f32,
    /// What `ocr::admit` would say: for a Formula from a typeset region with
    /// no native glyphs under it, exactly whether the LaTeX parses.
    admitted: bool,
}

impl AnchoredRead {
    fn one_glyph_sized(&self) -> bool {
        self.ink_width < ONE_GLYPH_POINTS
    }

    /// Whether the reading is one the crop could hold: a single letter or
    /// symbol, however the model dressed it up.
    ///
    /// Only meaningful for a one-glyph-sized crop, and only a classifier for
    /// the table below — it decides nothing. Font commands, spacing macros,
    /// braces and `~` are stripped; anything left that is not one character or
    /// one control sequence is a reading the crop cannot contain.
    fn reads_as_one_glyph(&self) -> bool {
        let mut text = self.latex.trim().to_string();
        for command in [
            "\\mathrm",
            "\\mathit",
            "\\mathbf",
            "\\mathsf",
            "\\mathcal",
            "\\text",
            "\\bf",
            "\\rm",
            "\\it",
            "\\,",
            "\\;",
            "\\:",
            "\\!",
            "\\ ",
        ] {
            text = text.replace(command, "");
        }
        let stripped: String = text
            .chars()
            .filter(|c| !matches!(c, '{' | '}' | '~' | ' ' | '$'))
            .collect();
        let mut chars = stripped.chars();
        match chars.next() {
            None => false,
            // One control sequence — `\alpha`, `\Sigma` — and nothing after
            // it.
            Some('\\') => chars.all(|c| c.is_ascii_alphabetic()),
            Some(_) => chars.next().is_none(),
        }
    }
}

/// The ink in a crop: how many pixels are not paper, and the tightest box
/// around them, in page points.
fn ink_of(crop: &RgbImage, covered: &BoundingBox) -> (u32, f32, f32) {
    let scale = if covered.width > 0.0 {
        crop.width() as f32 / covered.width
    } else {
        1.0
    };
    let (mut count, mut x0, mut y0, mut x1, mut y1) = (0u32, u32::MAX, u32::MAX, 0u32, 0u32);
    for (x, y, pixel) in crop.enumerate_pixels() {
        if pixel[0] > INK_LEVEL && pixel[1] > INK_LEVEL && pixel[2] > INK_LEVEL {
            continue;
        }
        count += 1;
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x + 1);
        y1 = y1.max(y + 1);
    }
    if count == 0 {
        return (0, 0.0, 0.0);
    }
    (count, (x1 - x0) as f32 / scale, (y1 - y0) as f32 / scale)
}

/// Render every anchored formula crop of one recipe and read it with Texify.
///
/// The crops come from [`typeset::regions`], the same call the stage above
/// scores, so this is the set extraction would anchor into the reading — not a
/// set assembled here from detections.
fn read_anchored(
    entries: &[Entry],
    names: &[String],
    survey: &[PageWords],
    run: &Detected,
    threshold: f32,
    raw: &Path,
) -> anyhow::Result<()> {
    use wilkes_core::extract::image::ocr::{latex_parses, OcrEngine};
    use wilkes_core::extract::image::texify::Texify;
    use wilkes_core::extract::pdf::typeset::render as render_region;

    let dir = model_dir();
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let texify = Texify::load(&dir, 1, threads)
        .with_context(|| format!("Texify is not loadable from {}", dir.display()))?;
    println!(
        "\n══ every anchored crop, read by the real recognizer ══\n\
         recipe {} · formula threshold {threshold:.2} · {}\n\
         anchored = the crop covers no word of the page; admitted = its LaTeX parses, which for \
         a\ntypeset Formula with no glyphs under it is the whole of `ocr::admit`",
        run.geometry.tag(),
        texify.identity()
    );

    let recipe = run.geometry.recipe(threshold);
    let mut reads: Vec<AnchoredRead> = Vec::new();
    let mut opened: Option<(PathBuf, Document)> = None;
    let started = Instant::now();
    for (((entry, name), passes), page_survey) in
        entries.iter().zip(names).zip(&run.passes).zip(survey)
    {
        let claimed = typeset::regions(
            (entry.page + 1) as u32,
            &page_survey.bounds,
            &doclayout::decode(passes, &recipe),
            &page_survey.survey,
        );
        let anchored: Vec<&typeset::TypesetRegion> = claimed
            .iter()
            .filter(|region| region.kind == RegionKind::Formula && region.anchor.is_some())
            .collect();
        if anchored.is_empty() {
            continue;
        }
        if opened
            .as_ref()
            .map(|(path, _)| *path != entry.pdf)
            .unwrap_or(true)
        {
            opened = Some((entry.pdf.clone(), Document::open(entry.pdf.as_path())?));
        }
        let document = &opened.as_ref().expect("a document was just opened").1;
        let page = document.load_page(entry.page)?;
        for region in anchored {
            let (decoded, covered) = render_region(&page, &region.bbox)
                .with_context(|| format!("{name}: an anchored crop did not render"))?;
            let (ink, ink_width, ink_height) = ink_of(&decoded.pixels, &covered);
            // One crop a call, so a decode that warns about its token cap
            // warns between two of this loop's own lines.
            let answer = texify
                .spot_batch(std::slice::from_ref(&decoded.pixels))
                .with_context(|| format!("{name}: Texify did not read an anchored crop"))?;
            let region_read = answer
                .first()
                .and_then(|recognition| recognition.regions.first());
            let (latex, confidence) = match region_read {
                Some(spotted) => (spotted.text.clone(), spotted.confidence),
                // The recognizer's own answer for "nothing to transcribe".
                None => (String::new(), 0.0),
            };
            reads.push(AnchoredRead {
                page: name.clone(),
                crop: covered,
                pixels: (decoded.pixels.width(), decoded.pixels.height()),
                ink,
                ink_width,
                ink_height,
                admitted: latex_parses(&latex),
                latex,
                confidence,
            });
        }
    }

    if reads.is_empty() {
        println!("  no anchored crop under this recipe");
        return Ok(());
    }
    println!(
        "  {} crops in {:?}\n",
        reads.len(),
        Duration::from_secs(started.elapsed().as_secs())
    );

    // The raw list, one line a crop, so every number above is checkable.
    let mut out = String::new();
    out.push_str(
        "page\tcrop_w_pt\tcrop_h_pt\tpx_w\tpx_h\tink_px\tink_w_pt\tink_h_pt\tconfidence\t\
         admitted\tone_glyph_crop\treads_as_one_glyph\tlatex\n",
    );
    for read in &reads {
        out.push_str(&format!(
            "{}\t{:.1}\t{:.1}\t{}\t{}\t{}\t{:.1}\t{:.1}\t{:.3}\t{}\t{}\t{}\t{}\n",
            read.page,
            read.crop.width,
            read.crop.height,
            read.pixels.0,
            read.pixels.1,
            read.ink,
            read.ink_width,
            read.ink_height,
            read.confidence,
            read.admitted,
            read.one_glyph_sized(),
            read.reads_as_one_glyph(),
            read.latex.replace(['\t', '\n'], " "),
        ));
    }
    std::fs::write(raw, out).with_context(|| format!("could not write {}", raw.display()))?;
    println!("  raw list: {}", raw.display());

    let (small, large): (Vec<&AnchoredRead>, Vec<&AnchoredRead>) =
        reads.iter().partition(|read| read.one_glyph_sized());
    println!(
        "\n  a crop is one-glyph-sized when its ink is under {ONE_GLYPH_POINTS:.0} pt wide; a \
         reading is\n  `glyph` when it is one letter or one control sequence under the font and \
         spacing\n  commands, and `other` when it is anything the crop cannot hold\n"
    );
    println!(
        "{:<18} {:>6} {:>9} {:>8} {:>8} {:>26}",
        "crops", "n", "admitted", "glyph", "other", "confidence  min/med/max"
    );
    for (label, group) in [("one-glyph-sized", &small), ("larger", &large)] {
        let glyph: Vec<&&AnchoredRead> = group
            .iter()
            .filter(|read| read.reads_as_one_glyph())
            .collect();
        let other: Vec<&&AnchoredRead> = group
            .iter()
            .filter(|read| !read.reads_as_one_glyph())
            .collect();
        println!(
            "{:<18} {:>6} {:>9} {:>8} {:>8}",
            label,
            group.len(),
            group.iter().filter(|read| read.admitted).count(),
            glyph.len(),
            other.len(),
        );
        for (kind, part) in [("glyph", &glyph), ("other", &other)] {
            if part.is_empty() {
                continue;
            }
            let mut scores: Vec<f32> = part.iter().map(|read| read.confidence).collect();
            scores.sort_by(f32::total_cmp);
            println!(
                "  {:<16} {:>6} {:>9} {:>8} {:>8}   {:.3} / {:.3} / {:.3}",
                format!("· {kind}"),
                part.len(),
                part.iter().filter(|read| read.admitted).count(),
                "",
                "",
                scores[0],
                scores[scores.len() / 2],
                scores[scores.len() - 1],
            );
        }
    }
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// The dump
// ---------------------------------------------------------------------------

const GREEN: Rgb<u8> = Rgb([0, 150, 0]);
const RED: Rgb<u8> = Rgb([220, 0, 0]);
const BLUE: Rgb<u8> = Rgb([0, 90, 220]);
const GREY: Rgb<u8> = Rgb([170, 170, 170]);

/// A 5x7 bitmap for the glyphs a dump needs. Not a font crate: the dump wants
/// four digits and a decimal point beside each box and nothing else, and a
/// dependency for that would outweigh the table.
fn glyph(c: char) -> Option<[u8; 7]> {
    Some(match c {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ],
        'i' => [
            0b00100, 0b00000, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        'd' => [
            0b00001, 0b00001, 0b01101, 0b10011, 0b10001, 0b10011, 0b01101,
        ],
        'x' => [
            0b00000, 0b00000, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001,
        ],
        _ => return None,
    })
}

fn draw_text(canvas: &mut RgbImage, x: i32, y: i32, text: &str, colour: Rgb<u8>) {
    let mut pen = x;
    for c in text.chars() {
        let Some(bits) = glyph(c) else {
            pen += 6;
            continue;
        };
        for (row, line) in bits.iter().enumerate() {
            for column in 0..5u32 {
                if line & (1 << (4 - column)) == 0 {
                    continue;
                }
                let px = pen + column as i32;
                let py = y + row as i32;
                if px >= 0
                    && py >= 0
                    && (px as u32) < canvas.width()
                    && (py as u32) < canvas.height()
                {
                    canvas.put_pixel(px as u32, py as u32, colour);
                }
            }
        }
        pen += 6;
    }
}

fn draw_rect(canvas: &mut RgbImage, rect: &Rect, scale: f32, colour: Rgb<u8>, weight: i32) {
    let (w, h) = (canvas.width() as i32, canvas.height() as i32);
    let x0 = (rect.x0 * scale).round() as i32;
    let y0 = (rect.y0 * scale).round() as i32;
    let x1 = (rect.x1 * scale).round() as i32;
    let y1 = (rect.y1 * scale).round() as i32;
    for t in 0..weight {
        for x in x0..=x1 {
            for y in [y0 - t, y1 + t] {
                if x >= 0 && y >= 0 && x < w && y < h {
                    canvas.put_pixel(x as u32, y as u32, colour);
                }
            }
        }
        for y in y0..=y1 {
            for x in [x0 - t, x1 + t] {
                if x >= 0 && y >= 0 && x < w && y < h {
                    canvas.put_pixel(x as u32, y as u32, colour);
                }
            }
        }
    }
}

/// The page at dump resolution, unstretched — the detector's own square is
/// unreadable, and the point of a dump is that a person can read it.
fn render_for_dump(page: &mupdf::Page, width: f32, height: f32) -> anyhow::Result<(RgbImage, f32)> {
    let scale = DUMP_LONGEST_PX / width.max(height);
    let canvas = IRect {
        x0: 0,
        y0: 0,
        x1: (width * scale).ceil() as i32,
        y1: (height * scale).ceil() as i32,
    };
    let mut pixmap = mupdf::Pixmap::new_with_rect(&Colorspace::device_rgb(), canvas, false)?;
    pixmap.clear_with(0xff)?;
    let device = Device::from_pixmap(&pixmap)?;
    page.run(&device, &Matrix::new_scale(scale, scale))?;
    drop(device);
    let decoded = decode(
        pixmap.width(),
        pixmap.height(),
        pixmap.n() as usize,
        pixmap.stride() as usize,
        pixmap.samples(),
    )
    .map_err(|reason| anyhow::anyhow!("the dump page did not decode: {reason}"))?;
    Ok((decoded.pixels, scale))
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

fn model_dir() -> PathBuf {
    std::env::var("PROBE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").expect("a home directory");
            PathBuf::from(home).join("Library/Application Support/app.wilkes/models")
        })
}

/// A short name for a fixture page, for the per-page rows and the dump files.
fn tag(entry: &Entry) -> String {
    let stem = entry
        .pdf
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "?".to_string());
    let short: String = stem.split_whitespace().next().unwrap_or("?").to_string();
    format!("{short}#{}", entry.page)
}

/// One recipe's geometry: what a page is rendered at and how it is tiled.
///
/// The threshold is not in here, because the whole point of holding passes is
/// that one detection is scored at every threshold in the sweep. Geometry is
/// what costs a forward pass.
#[derive(Clone, Copy, PartialEq)]
struct Geometry {
    render_side: u32,
    tile_stride: Option<u32>,
    /// The tile gate this geometry is detected under: production's floor
    /// wherever there are tiles to withhold, and `None` where there are not.
    /// Carried rather than read from `Recipe::PRODUCTION` at use, because a
    /// geometry that tiles and one that does not are gated differently and the
    /// table's rows have to say which they were.
    tile_gate: Option<f32>,
}

impl Geometry {
    /// A geometry gated the way production gates one of that shape.
    fn tiled(render_side: u32, tile_stride: Option<u32>) -> Self {
        Self {
            render_side,
            tile_stride,
            tile_gate: match tile_stride {
                Some(stride) if stride > 0 && render_side > DocLayout::graph_side() => {
                    Recipe::PRODUCTION.tile_gate
                }
                _ => None,
            },
        }
    }

    fn recipe(&self, formula_threshold: f32) -> Recipe {
        Recipe {
            render_side: self.render_side,
            tile_stride: self.tile_stride,
            formula_threshold,
            tile_gate: self.tile_gate,
        }
    }

    /// A short tag for a table row and a dump filename.
    fn tag(&self) -> String {
        match self.tile_stride {
            Some(stride) if self.render_side > DocLayout::graph_side() => {
                format!("{}px/{stride}", self.render_side)
            }
            _ => format!("{}px/whole", self.render_side),
        }
    }
}

/// `R:S` — a render side and a tile stride, or `none` for no tiling.
fn parse_tiling(text: &str) -> anyhow::Result<Geometry> {
    let (side, stride) = text
        .split_once(':')
        .with_context(|| format!("--tiling wants R:S, got {text}"))?;
    let render_side: u32 = side
        .parse()
        .with_context(|| format!("--tiling: {side} is not a render side"))?;
    let tile_stride = match stride {
        "none" | "whole" => None,
        other => Some(
            other
                .parse()
                .with_context(|| format!("--tiling: {other} is not a stride"))?,
        ),
    };
    anyhow::ensure!(
        render_side.is_multiple_of(doclayout::DocLayout::graph_side()),
        "--tiling: {render_side} is not a multiple of the graph's {} px square",
        doclayout::DocLayout::graph_side()
    );
    Ok(Geometry::tiled(render_side, tile_stride))
}

/// One recipe's detections over the whole fixture, undecoded.
struct Detected {
    geometry: Geometry,
    ms_per_page: f64,
    /// One entry per fixture page, in fixture order.
    passes: Vec<Vec<Pass>>,
}

/// One label the text layer cannot read that the detector did not propose.
struct Miss {
    page: String,
    pdf: PathBuf,
    page_index: i32,
    rect: Rect,
    kind: &'static str,
    text: String,
    reading: Reading,
    /// The label's width in the graph's own square.
    width_px: f32,
    /// The nearest formula detection on the page, by IoU, if there was one.
    nearest: Option<(f32, &'static str, f32)>,
}

/// What one (geometry, threshold) pair scored, for the closing comparison.
struct Scored {
    tag: String,
    threshold: f32,
    ms_per_page: f64,
    inline: Tally,
    display: Tally,
    /// What the same detections came to after [`typeset::regions`] claimed the
    /// page's words with them — the crops the recognizer is handed.
    claimed: Claiming,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let fixture = args.next().context(
        "usage: <labels.json> [--tiling R:S] [--threshold T] [--iou I] [--per-page] [--dump-dir DIR] [--texify FILE]",
    )?;
    let mut thresholds: Vec<f32> = Vec::new();
    let mut iou_floor = 0.5f32;
    let mut dump_dir: Option<PathBuf> = None;
    let mut miss_crops: Option<PathBuf> = None;
    let mut texify_raw: Option<PathBuf> = None;
    let mut per_page = false;
    let mut geometries: Vec<Geometry> = Vec::new();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            // Repeatable: choosing an operating point is reading one column
            // down a range of them, and each costs a decode, not a detection.
            "--threshold" => {
                let value = args.next().context("--threshold wants a number")?;
                thresholds.push(value.parse().context("--threshold wants a number")?);
            }
            "--iou" => {
                let value = args.next().context("--iou wants a number")?;
                iou_floor = value.parse().context("--iou wants a number")?;
            }
            "--tiling" => {
                let value = args.next().context("--tiling wants R:S")?;
                geometries.push(parse_tiling(&value)?);
            }
            "--per-page" => per_page = true,
            "--dump-dir" => {
                dump_dir = Some(PathBuf::from(
                    args.next().context("--dump-dir wants a directory")?,
                ));
            }
            // A whole page at 1700 px shows *where* a miss is; it does not
            // show what the expression is. These are the same misses at eight
            // times page scale, one file each, so a claim about what the
            // detector failed on can be checked by looking at it.
            "--miss-crops" => {
                miss_crops = Some(PathBuf::from(
                    args.next().context("--miss-crops wants a directory")?,
                ));
            }
            // The stage after the crop count: what the recognizer says about
            // the crops that cover no word. One recipe and one threshold only
            // — the answer is about a set of crops, and two recipes' crops are
            // two different sets.
            "--texify" => {
                texify_raw = Some(PathBuf::from(
                    args.next().context("--texify wants a file to write")?,
                ));
            }
            other => anyhow::bail!("unknown flag {other}"),
        }
    }
    if thresholds.is_empty() {
        thresholds = SWEEP.to_vec();
    }
    if texify_raw.is_some() {
        anyhow::ensure!(
            thresholds.len() == 1 && geometries.len() == 1,
            "--texify reads one recipe's crops: name exactly one --tiling and one --threshold"
        );
    }
    if geometries.is_empty() {
        // The baseline and what production runs, in one invocation. Two runs
        // would be two numbers rather than a comparison.
        geometries.push(Geometry::tiled(doclayout::DocLayout::graph_side(), None));
        let production = Recipe::PRODUCTION;
        let live = Geometry::tiled(production.render_side, production.tile_stride);
        if !geometries.contains(&live) {
            geometries.push(live);
        }
    }
    for dir in [&dump_dir, &miss_crops].into_iter().flatten() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }

    let entries: Vec<Entry> = serde_json::from_str(
        &std::fs::read_to_string(&fixture).with_context(|| format!("could not read {fixture}"))?,
    )
    .with_context(|| format!("{fixture} is not the fixture shape this probe reads"))?;
    anyhow::ensure!(!entries.is_empty(), "{fixture} holds no pages");

    let dir = model_dir();
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let detector = DocLayout::load(&dir, threads)
        .with_context(|| format!("the layout detector is not loadable from {}", dir.display()))?;
    println!("detector: {}", doclayout::identity());
    println!("fixture : {fixture}");
    println!(
        "matching: IoU >= {iou_floor:.2}, coverage >= {COVERAGE:.2} of the label's area, {threads} threads"
    );
    println!(
        "recipes : {}\n",
        geometries
            .iter()
            .map(Geometry::tag)
            .collect::<Vec<_>>()
            .join("  ")
    );

    // Named once, so the per-page rows, the dumps and the comparison all say
    // the same thing about the same page.
    let names: Vec<String> = entries.iter().map(tag).collect();

    // Once, before any detection: what the class of a label is has nothing to
    // do with which recipe was run, and asking per recipe would invite the two
    // answers to differ.
    let readings = classify_all(&entries, &names)?;
    report_classes(&entries, &names, &readings);

    // Also once: the page's words are the page's, and the claiming stage
    // scored below has to be given the words extraction would have had.
    let survey = survey_all(&entries, &names)?;

    let mut runs = Vec::new();
    for geometry in &geometries {
        runs.push(detect_all(&detector, &entries, &names, *geometry)?);
    }

    let mut comparison = Vec::new();
    for run in &runs {
        for threshold in &thresholds {
            comparison.push(run_at(
                &entries,
                &names,
                &readings,
                &survey,
                run,
                *threshold,
                iou_floor,
                per_page,
                dump_dir.as_deref(),
                miss_crops.as_deref(),
            )?);
        }
    }

    println!("══ every recipe, side by side ══");
    println!(
        "{:<14} {:>5} {:>8}   {:>7} {:>7} {:>7}   {:>7} {:>7} {:>7}",
        "recipe", "thr", "ms/page", "in.rec", "in.cov", "in.pre", "di.rec", "di.cov", "di.pre"
    );
    for row in &comparison {
        println!(
            "{:<14} {:>5.2} {:>8.0}   {:>7} {:>7} {:>7}   {:>7} {:>7} {:>7}",
            row.tag,
            row.threshold,
            row.ms_per_page,
            percent(row.inline.recall()),
            percent(row.inline.coverage()),
            percent(row.inline.precision()),
            percent(row.display.recall()),
            percent(row.display.coverage()),
            percent(row.display.precision()),
        );
    }

    // The same recipes, one stage further down: not what the detector
    // proposed but what `typeset::regions` would hand the recognizer. This is
    // the column the tiles have to be worth their milliseconds on.
    println!("\n══ every recipe, at the crop the recognizer is handed ══");
    println!(
        "over the needs_recognizer labels; a crop costs {TEXIFY_SECONDS:.1} s of Texify, and \
         `lost` is a label\na formula detection covered at decode that no crop covers"
    );
    println!(
        "{:<14} {:>5} {:>8} {:>9} {:>10} {:>10} {:>7}",
        "recipe", "thr", "crops/pg", "texify s/pg", "reached", "cleanly", "lost"
    );
    for row in &comparison {
        let claimed = row.claimed;
        println!(
            "{:<14} {:>5.2} {:>8.1} {:>9.1}   {:>5} {:>4} {:>5} {:>4} {:>7}",
            row.tag,
            row.threshold,
            claimed.crops_per_page(),
            claimed.crops_per_page() * TEXIFY_SECONDS,
            claimed.reached,
            percent(ratio(claimed.reached, claimed.labels)),
            claimed.clean,
            percent(ratio(claimed.clean, claimed.labels)),
            claimed.lost,
        );
    }
    println!();

    if let Some(raw) = &texify_raw {
        let run = runs.first().expect("one recipe was required above");
        let threshold = *thresholds
            .first()
            .expect("one threshold was required above");
        read_anchored(&entries, &names, &survey, run, threshold, raw)?;
    }
    Ok(())
}

/// Pixels per point a miss crop is rendered at, and how much page is kept
/// around the label so the expression can be seen in its sentence.
const MISS_SCALE: f32 = 8.0;
const MISS_CONTEXT_POINTS: f32 = 26.0;

/// How many of the worst misses are listed and written out.
const MISS_COUNT: usize = 16;

/// List the misses the text layer cannot cover for, worst first, and write a
/// legible crop of each.
///
/// Worst is most glyphs first: a label the layer flattens six glyphs of is a
/// longer expression lost than one it flattens two of, and the width breaks
/// the ties. Not "lowest score", because these have no score — the detector
/// proposed nothing here, which is the whole finding.
fn report_misses(
    misses: &mut [Miss],
    run: &Detected,
    threshold: f32,
    dir: Option<&Path>,
) -> anyhow::Result<()> {
    misses.sort_by(|a, b| {
        b.reading
            .glyphs
            .cmp(&a.reading.glyphs)
            .then(b.width_px.partial_cmp(&a.width_px).expect("no NaN widths"))
    });
    println!(
        "\nthe {} worst of {} needs_recognizer labels the detector proposed nothing for:",
        MISS_COUNT.min(misses.len()),
        misses.len()
    );
    println!(
        "{:<24} {:>6} {:>6} {:>6} {:>16} {:>10}  the fixture's text",
        "page", "w.pt", "h.pt", "w.px", "text layer", "nearest"
    );
    for miss in misses.iter().take(MISS_COUNT) {
        let nearest = match miss.nearest {
            Some((iou, class, confidence)) => format!(
                "{}{confidence:.2}@{iou:.2}",
                if class == "inline_formula" { "i" } else { "d" }
            ),
            None => "none".to_string(),
        };
        println!(
            "{:<24} {:>6.1} {:>6.1} {:>6.1} {:>16} {:>10}  {}",
            format!("{} {}", miss.page, miss.kind),
            miss.rect.x1 - miss.rect.x0,
            miss.rect.y1 - miss.rect.y0,
            miss.width_px,
            format!(
                "{}g/{}u/{}ink",
                miss.reading.glyphs, miss.reading.unmapped, miss.reading.ink
            ),
            nearest,
            miss.text,
        );
    }

    let Some(dir) = dir else { return Ok(()) };
    let mut opened: Option<(PathBuf, Document)> = None;
    for (rank, miss) in misses.iter().take(MISS_COUNT).enumerate() {
        if opened
            .as_ref()
            .map(|(path, _)| *path != miss.pdf)
            .unwrap_or(true)
        {
            opened = Some((miss.pdf.clone(), Document::open(miss.pdf.as_path())?));
        }
        let document = &opened.as_ref().expect("a document was just opened").1;
        let page = document.load_page(miss.page_index)?;
        let area = Rect {
            x0: miss.rect.x0 - MISS_CONTEXT_POINTS,
            y0: miss.rect.y0 - MISS_CONTEXT_POINTS,
            x1: miss.rect.x1 + MISS_CONTEXT_POINTS,
            y1: miss.rect.y1 + MISS_CONTEXT_POINTS,
        };
        let rect = IRect {
            x0: (area.x0 * MISS_SCALE).floor() as i32,
            y0: (area.y0 * MISS_SCALE).floor() as i32,
            x1: (area.x1 * MISS_SCALE).ceil() as i32,
            y1: (area.y1 * MISS_SCALE).ceil() as i32,
        };
        let mut pixmap = mupdf::Pixmap::new_with_rect(&Colorspace::device_rgb(), rect, false)?;
        pixmap.clear_with(0xff)?;
        let device = Device::from_pixmap_with_clip(&pixmap, rect)?;
        page.run(&device, &Matrix::new_scale(MISS_SCALE, MISS_SCALE))?;
        drop(device);
        let mut canvas = decode(
            pixmap.width(),
            pixmap.height(),
            pixmap.n() as usize,
            pixmap.stride() as usize,
            pixmap.samples(),
        )
        .map_err(|reason| anyhow::anyhow!("a miss crop did not decode: {reason}"))?
        .pixels;
        // The label, where it sits in the crop. Blue is what the whole-page
        // dump paints a miss, and the two are meant to be read together.
        let origin = (rect.x0 as f32 / MISS_SCALE, rect.y0 as f32 / MISS_SCALE);
        draw_rect(
            &mut canvas,
            &Rect {
                x0: miss.rect.x0 - origin.0,
                y0: miss.rect.y0 - origin.1,
                x1: miss.rect.x1 - origin.0,
                y1: miss.rect.y1 - origin.1,
            },
            MISS_SCALE,
            BLUE,
            1,
        );
        let safe: String = miss
            .page
            .chars()
            .map(|c| if c == '#' || c == '/' { '-' } else { c })
            .collect();
        let recipe_tag: String = run
            .geometry
            .tag()
            .chars()
            .map(|c| if c == '/' { '-' } else { c })
            .collect();
        let file = dir.join(format!(
            "{recipe_tag}-t{threshold:.2}-miss{rank:02}-{safe}-{:.0}x{:.0}.png",
            miss.rect.x0, miss.rect.y0
        ));
        canvas
            .save(&file)
            .with_context(|| format!("could not write {}", file.display()))?;
    }
    println!("miss crops written to {}", dir.display());
    Ok(())
}

/// Render and detect the whole fixture once under one geometry.
///
/// The passes are held rather than decoded, so every threshold in the sweep
/// scores the same detections. The clock covers the passes only: rendering is
/// the host's work and is the same for every recipe of the same render side.
fn detect_all(
    detector: &DocLayout,
    entries: &[Entry],
    names: &[String],
    geometry: Geometry,
) -> anyhow::Result<Detected> {
    let recipe = geometry.recipe(doclayout::FORMULA_THRESHOLD);
    let mut passes = Vec::with_capacity(entries.len());
    let mut wall = Vec::new();
    let mut opened: Option<(PathBuf, Document)> = None;
    for (entry, name) in entries.iter().zip(names) {
        // One handle per file, reopened only when the fixture moves on to the
        // next document: the fixture is grouped by PDF and these are large.
        if opened
            .as_ref()
            .map(|(path, _)| *path != entry.pdf)
            .unwrap_or(true)
        {
            let document = Document::open(entry.pdf.as_path())
                .with_context(|| format!("could not open {}", entry.pdf.display()))?;
            opened = Some((entry.pdf.clone(), document));
        }
        let document = &opened.as_ref().expect("a document was just opened").1;
        let page = document
            .load_page(entry.page)
            .with_context(|| format!("{name}: could not load the page"))?;
        let bounds = page.bounds()?;
        let (width, height) = (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
        anyhow::ensure!(
            (width - entry.page_width).abs() < 1.0 && (height - entry.page_height).abs() < 1.0,
            "{name}: the fixture records {}x{} points, the page is {width:.1}x{height:.1}",
            entry.page_width,
            entry.page_height
        );

        let square = typeset::render_page(&page, recipe.render_side)
            .with_context(|| format!("{name}: could not render for detection"))?;
        let started = Instant::now();
        let found = detector
            .passes(&square, &recipe)
            .with_context(|| format!("{name}: detection failed"))?;
        wall.push(started.elapsed());
        passes.push(found);
    }

    let total: Duration = wall.iter().sum();
    let ms_per_page = total.as_secs_f64() * 1000.0 / wall.len() as f64;
    println!(
        "{}: {} pages, {} passes each, detected in {:.2?} — {ms_per_page:.0} ms a page",
        geometry.tag(),
        wall.len(),
        recipe.windows().len(),
        total,
    );
    Ok(Detected {
        geometry,
        ms_per_page,
        passes,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_at(
    entries: &[Entry],
    names: &[String],
    readings: &[Vec<Reading>],
    survey: &[PageWords],
    run: &Detected,
    threshold: f32,
    iou_floor: f32,
    per_page: bool,
    dump_dir: Option<&Path>,
    miss_crops: Option<&Path>,
) -> anyhow::Result<Scored> {
    let recipe = run.geometry.recipe(threshold);
    println!(
        "\n══ {} · formula threshold {threshold:.2} · {:.0} ms a page ══",
        run.geometry.tag(),
        run.ms_per_page
    );
    if per_page {
        println!(
            "{:<26} {:>6} {:>6} {:>6} {:>6}   {:>6} {:>6} {:>6}   {:>6} {:>6} {:>6}",
            "page",
            "in.lbl",
            "in.det",
            "di.lbl",
            "di.det",
            "in.rec",
            "in.cov",
            "in.pre",
            "di.rec",
            "di.cov",
            "di.pre"
        );
    }

    let mut overall: BTreeMap<&'static str, Tally> = BTreeMap::new();
    // Recall by how big the expression is in the *graph's* square — the number
    // the hypothesis about small inline mathematics is about, and deliberately
    // not the render side, so a bucket means the same thing under every
    // recipe and the before and after can be read off one another.
    let mut sizes: Vec<Tally> = vec![Tally::default(); SIZE_EDGES.len() + 1];
    // The same table over the labels the text layer cannot read. The whole
    // table over every label answers "how small is what it misses"; this one
    // answers the question that decides anything, which is how small is what
    // it misses *and* nothing else can supply.
    let mut needs_sizes: Vec<Tally> = vec![Tally::default(); SIZE_EDGES.len() + 1];
    let mut by_class: BTreeMap<(Class, &'static str), Tally> = BTreeMap::new();
    let mut over = [0usize; 3];
    let mut misses: Vec<Miss> = Vec::new();
    let mut scored = Vec::new();
    // What survives the claiming stage, which is what the recognizer is
    // actually handed — see [`claiming`].
    let mut claimed = Claiming::default();
    for ((((entry, name), passes), page_readings), page_survey) in entries
        .iter()
        .zip(names)
        .zip(&run.passes)
        .zip(readings.iter())
        .zip(survey)
    {
        let regions = doclayout::decode(passes, &recipe);
        let found: Vec<(Rect, &'static str, f32)> = regions
            .iter()
            .filter(|region| FORMULA_CLASSES.contains(&region.label))
            .map(|region| {
                (
                    to_points(region, entry.page_width, entry.page_height),
                    region.label,
                    region.score,
                )
            })
            .collect();
        let page_score = score(&entry.boxes, page_readings, &found, iou_floor);
        claimed.add(&claiming(entry, page_survey, &regions, &page_score.labels));
        for (kind, tally) in &page_score.by_kind {
            overall.entry(kind).or_default().add(tally);
        }
        for (key, tally) in &page_score.by_class {
            by_class.entry(*key).or_default().add(tally);
        }
        for (index, count) in page_score.over.iter().enumerate() {
            over[index] += count;
        }
        let to_px = DocLayout::graph_side() as f32 / entry.page_width;
        for (index, row) in page_score.labels.iter().enumerate() {
            let width_px = (row.rect.x1 - row.rect.x0) * to_px;
            let tally = &mut sizes[bucket(width_px)];
            tally.labels += 1;
            tally.matched += usize::from(row.matched);
            tally.covered += usize::from(row.covered);
            if row.reading.class() == Class::Needs {
                let tally = &mut needs_sizes[bucket(width_px)];
                tally.labels += 1;
                tally.matched += usize::from(row.matched);
                tally.covered += usize::from(row.covered);
            }
            if row.reading.class() == Class::Needs && !row.matched && !row.covered {
                // How near the detector came: the best IoU any formula
                // detection managed against this label, and what it was.
                let nearest = found
                    .iter()
                    .map(|(rect, class, confidence)| (rect.iou(&row.rect), *class, *confidence))
                    .max_by(|a, b| a.0.partial_cmp(&b.0).expect("no NaN areas"));
                misses.push(Miss {
                    page: name.clone(),
                    pdf: entry.pdf.clone(),
                    page_index: entry.page,
                    rect: row.rect,
                    kind: row.kind,
                    text: entry.boxes[index].text.clone(),
                    reading: row.reading,
                    width_px,
                    nearest,
                });
            }
        }
        let inline = page_score
            .by_kind
            .get("inline")
            .copied()
            .unwrap_or_default();
        let display = page_score
            .by_kind
            .get("display")
            .copied()
            .unwrap_or_default();
        if per_page {
            println!(
                "{name:<26} {:>6} {:>6} {:>6} {:>6}   {:>6} {:>6} {:>6}   {:>6} {:>6} {:>6}",
                inline.labels,
                inline.detections,
                display.labels,
                display.detections,
                percent(inline.recall()),
                percent(inline.coverage()),
                percent(inline.precision()),
                percent(display.recall()),
                percent(display.coverage()),
                percent(display.precision()),
            );
        }
        scored.push((name, entry, page_score));
    }

    let mut whole = Tally::default();
    for tally in overall.values() {
        whole.add(tally);
    }
    println!(
        "{:<10} {:>7} {:>7} {:>7} {:>7} {:>8} {:>9} {:>10}",
        "kind", "labels", "dets", "matched", "covered", "recall", "coverage", "precision"
    );
    for kind in ["inline", "display"] {
        let tally = overall.get(kind).copied().unwrap_or_default();
        println!(
            "{kind:<10} {:>7} {:>7} {:>7} {:>7} {:>8} {:>9} {:>10}",
            tally.labels,
            tally.detections,
            tally.matched,
            tally.covered,
            percent(tally.recall()),
            percent(tally.coverage()),
            percent(tally.precision()),
        );
    }
    println!(
        "{:<10} {:>7} {:>7} {:>7} {:>7} {:>8} {:>9} {:>10}\n",
        "overall",
        whole.labels,
        whole.detections,
        whole.matched,
        whole.covered,
        percent(whole.recall()),
        percent(whole.coverage()),
        percent(whole.precision()),
    );

    // ── split by what the text layer already reads ──────────────────────
    println!(
        "{:<12} {:<9} {:>7} {:>7} {:>7} {:>8} {:>9}",
        "class", "kind", "labels", "matched", "covered", "recall", "coverage"
    );
    let mut class_total: BTreeMap<Class, Tally> = BTreeMap::new();
    for class in [Class::Needs, Class::Suffices, Class::Blank] {
        for kind in ["inline", "display"] {
            let tally = by_class.get(&(class, kind)).copied().unwrap_or_default();
            if tally.labels == 0 {
                continue;
            }
            class_total.entry(class).or_default().add(&tally);
            println!(
                "{:<12} {kind:<9} {:>7} {:>7} {:>7} {:>8} {:>9}",
                class.name(),
                tally.labels,
                tally.matched,
                tally.covered,
                percent(tally.recall()),
                percent(tally.coverage()),
            );
        }
    }
    for (class, tally) in &class_total {
        println!(
            "{:<12} {:<9} {:>7} {:>7} {:>7} {:>8} {:>9}",
            class.name(),
            "both",
            tally.labels,
            tally.matched,
            tally.covered,
            percent(tally.recall()),
            percent(tally.coverage()),
        );
    }

    // Precision, under the same lens. A detection over a label the text layer
    // already reads is not a useful hit and is not a false positive on prose
    // either, so it is its own number rather than being folded into one.
    let dets: usize = over.iter().sum();
    println!("\n{dets} formula detections:");
    for verdict in Over::ALL {
        let count = over[verdict.index()];
        println!(
            "  {:>5}  {:>5}  {}",
            count,
            percent(ratio(count, dets)),
            verdict.name()
        );
    }

    println!(
        "\n{:<10} {:>7} {:>7} {:>7} {:>8} {:>9}   {:>7} {:>7} {:>7} {:>8} {:>9}",
        "width",
        "labels",
        "matched",
        "covered",
        "recall",
        "coverage",
        "n.lbl",
        "n.mat",
        "n.cov",
        "n.rec",
        "n.cov%"
    );
    for (index, (tally, needs)) in sizes.iter().zip(&needs_sizes).enumerate() {
        println!(
            "{:<10} {:>7} {:>7} {:>7} {:>8} {:>9}   {:>7} {:>7} {:>7} {:>8} {:>9}",
            bucket_name(index),
            tally.labels,
            tally.matched,
            tally.covered,
            percent(tally.recall()),
            percent(tally.coverage()),
            needs.labels,
            needs.matched,
            needs.covered,
            percent(needs.recall()),
            percent(needs.coverage()),
        );
    }
    println!("(the `n.` columns are the needs_recognizer labels alone)");

    // ── and what survives the claiming stage ────────────────────────────
    println!(
        "\nwhat reaches Texify: {} Formula crops over {} pages — {:.1} a page, {:.1} s of \
         recognizer a page",
        claimed.crops,
        claimed.pages,
        claimed.crops_per_page(),
        claimed.crops_per_page() * TEXIFY_SECONDS,
    );
    println!(
        "a crop reaches a label when it covers >= {:.0}% of it; cleanly when it also covers no \
         second\nneeds_recognizer label and at most {:.0}% of the words in it lie outside every \
         fixture label",
        COVERAGE * 100.0,
        CLEAN_PROSE_SHARE * 100.0
    );
    for (what, count) in [
        ("needs_recognizer labels", claimed.labels),
        ("covered by a formula detection (decode)", claimed.decoded),
        ("reached by a crop", claimed.reached),
        ("reached by a crop, cleanly", claimed.clean),
        ("covered at decode and by no crop", claimed.lost),
    ] {
        println!(
            "  {what:<42} {count:>5}  {}",
            percent(ratio(count, claimed.labels))
        );
    }
    for (what, count) in [
        ("nothing typeset under the label", claimed.lost_wordless),
        (
            "a table or chart claimed the words",
            claimed.lost_to_other_kind,
        ),
        (
            "a crop over it holds under half of it",
            claimed.lost_crop_short,
        ),
        (
            "the detection owned no words of its own",
            claimed.lost_unclaimed,
        ),
    ] {
        println!(
            "    of those, {what:<38} {count:>5}  {}",
            percent(ratio(count, claimed.lost))
        );
    }

    report_misses(&mut misses, run, threshold, miss_crops)?;

    let result = Scored {
        tag: run.geometry.tag(),
        threshold,
        ms_per_page: run.ms_per_page,
        inline: overall.get("inline").copied().unwrap_or_default(),
        display: overall.get("display").copied().unwrap_or_default(),
        claimed,
    };

    let Some(dir) = dump_dir else {
        return Ok(result);
    };
    let mut opened: Option<(PathBuf, Document)> = None;
    for (name, entry, page_score) in scored {
        if opened
            .as_ref()
            .map(|(path, _)| *path != entry.pdf)
            .unwrap_or(true)
        {
            opened = Some((entry.pdf.clone(), Document::open(entry.pdf.as_path())?));
        }
        let document = &opened.as_ref().expect("a document was just opened").1;
        let page = document.load_page(entry.page)?;
        let (mut canvas, scale) = render_for_dump(&page, entry.page_width, entry.page_height)?;
        for row in &page_score.labels {
            // Green for a label the detector reached; blue for one it missed
            // outright, so a page's misses are findable without reading the
            // table beside it. Grey for a miss the page's own text layer
            // already reads: nothing was lost there, and painting it like a
            // loss is what made the old dumps read worse than the page is.
            let colour = match (row.matched || row.covered, row.reading.class()) {
                (true, _) => GREEN,
                (false, Class::Needs) => BLUE,
                (false, _) => GREY,
            };
            draw_rect(&mut canvas, &row.rect, scale, colour, 1);
            draw_text(
                &mut canvas,
                (row.rect.x0 * scale).round() as i32,
                (row.rect.y1 * scale).round() as i32 + 2,
                if row.kind == "inline" { "i" } else { "d" },
                colour,
            );
        }
        for (rect, class, confidence, matched) in &page_score.detections {
            draw_rect(&mut canvas, rect, scale, RED, 1);
            let mark = if *matched { "" } else { "x" };
            draw_text(
                &mut canvas,
                (rect.x0 * scale).round() as i32,
                (rect.y0 * scale).round() as i32 - 9,
                &format!(
                    "{}{:.2}{mark}",
                    if *class == "inline_formula" { "i" } else { "d" },
                    confidence
                ),
                RED,
            );
        }
        let safe: String = name
            .chars()
            .map(|c| if c == '#' || c == '/' { '-' } else { c })
            .collect();
        let recipe_tag: String = run
            .geometry
            .tag()
            .chars()
            .map(|c| if c == '/' { '-' } else { c })
            .collect();
        let file = dir.join(format!("{recipe_tag}-t{threshold:.2}-{safe}.png"));
        canvas
            .save(&file)
            .with_context(|| format!("could not write {}", file.display()))?;
    }
    println!("dumps written to {}", dir.display());
    Ok(result)
}
