//! Transcription of the text inside a native image.
//!
//! Wilkes owns the contract; the engine behind it is replaceable. What crosses
//! this boundary is Wilkes' own types — normalized text, an admission signal,
//! and quadrilaterals in Wilkes' coordinate spaces — so no engine's token
//! format or geometry convention leaks into the reading, the source map or
//! the cache key.
//!
//! There is exactly one production engine. A recognition failure is a partial
//! result, never a second engine's turn: two engines producing the reading
//! would mean two answers to "what does this document say", and the whole
//! design rests on there being one.

use crate::types::{
    BoundingBox, ImageOcrRegion, ImageTransform, OcrAdmission, Point, RegionOrigin,
};

use super::AnalysisContext;

// The kind of a region is a property of the region and crosses the API
// boundary on both [`ImageOcrRegion`] and [`crate::types::TextProvenance`], so
// it is declared beside them and re-exported here — this module is still where
// a recognizer reaches for it.
pub use crate::types::RegionKind;

/// The largest location token. The recognizer emits coordinates as
/// `<|LOC_0|>` .. `<|LOC_1000|>`, so the grid has 1001 stops and a coordinate
/// is `n / 1000` of the way across the image it was given.
pub const LOC_MAX: u16 = 1000;

/// One region as the model emitted it: text, an admission signal, and a
/// quadrilateral in fractions of the image, before any of Wilkes' geometry.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpottedRegion {
    /// What `text` is. Defaulted so a stored region written before recognizers
    /// distinguished kinds still reads as the prose it was.
    #[serde(default)]
    pub kind: RegionKind,
    pub text: String,
    /// Mean probability of the tokens that spell `text`, from the decode's own
    /// log-probabilities. Uncalibrated by construction — it says how sure the
    /// decoder was of its own next token, not how often such a decode is
    /// right.
    pub confidence: f32,
    /// Top-left, top-right, bottom-right, bottom-left, each in `0.0..=1.0` of
    /// the image's width and height.
    pub quad: [Point; 4],
    /// The decode that produced `text` reached its token cap without emitting
    /// an end-of-sequence token, so `text` is truncated by construction and
    /// not a reading.
    ///
    /// Defaulted so a region built before this field existed reads as
    /// complete, which is what every decode was until a recognizer's own cap
    /// was measured against this corpus. Set by the engine, not inferred
    /// downstream: only the decoder itself knows whether it stopped because
    /// it was finished or because it was cut off.
    #[serde(default)]
    pub truncated: bool,
    /// Set when this region is a table built from a structure model's grid and
    /// the page's own glyphs, and carries the facts [`admit`] judges such a
    /// table on.
    ///
    /// `None` for everything else, which is every region a recognizer
    /// transcribed: those have no grid behind them and no word of the page to
    /// have left unplaced, so the structural clauses do not apply and a
    /// defaulted summary would silently apply them anyway. Absent from the wire
    /// when it is `None`, so a worker that predates the field is unaffected.
    ///
    /// It travels *with the region* because the fill and the admission happen
    /// in two different places: the analyzer puts the page's words into the
    /// grid, and [`place_and_admit`] decides afterwards whether the result may
    /// stand in place of those words. A summary recomputed at the second place
    /// would mean holding the grid and the page's words until then, or asking
    /// the model again — and a rule that had to re-run a model to be applied is
    /// a rule nobody would move.
    ///
    /// It is not what a cached reading is re-decided from. The annotation cache
    /// stores [`crate::types::ImageOcrRegion`]s, whose verdict is already
    /// settled, and its key carries [`ADMISSION_RULES_VERSION`] — so moving a
    /// clause does not re-judge the stored entry, it stops the entry from being
    /// found and the crop is read again under the new rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structure: Option<super::table_structure::TableFillSummary>,
}

/// What a recognizer made of one image.
///
/// The regions are the answer; the two counts are the parts of the answer that
/// produced no region, kept apart because they are opposite facts. A recognizer
/// that marks out a region of a kind this build has no name for must say so
/// rather than drop it, because a reading missing that content and a picture
/// that never held any read identically otherwise — which is the same reason
/// every rejected region is kept.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImageRecognition {
    pub regions: Vec<SpottedRegion>,
    /// Regions the recognizer delimited and this build has no [`RegionKind`]
    /// for. Reported, counted, and not guessed at.
    ///
    /// A gap in this build, so any of these is worth a reader's attention.
    #[serde(default)]
    pub unroutable: u32,
    /// Regions the recognizer delimited that carry no text to read — a
    /// picture, most often the whole of a figure crop.
    ///
    /// The recognizer working correctly, not a gap: a document parser given a
    /// figure answers "this is a figure", and there is nothing to transcribe
    /// in that. Counted anyway, and counted *separately*, because one number
    /// covering both cannot distinguish a hundred figures correctly named
    /// from a hundred regions of content this build lost.
    #[serde(default)]
    pub not_text: u32,
}

impl ImageRecognition {
    pub fn from_regions(regions: Vec<SpottedRegion>) -> Self {
        Self {
            regions,
            unroutable: 0,
            not_text: 0,
        }
    }
}

/// One decoded token of a spotting response.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpottingToken {
    pub id: u32,
    /// Log-probability the decoder assigned the token it chose.
    pub logprob: f32,
    /// `Some(n)` when this token is `<|LOC_n|>`.
    pub loc: Option<u16>,
}

/// Turns token ids back into text. Supplied by the engine, which owns the
/// tokenizer; kept behind a trait so the parser below is testable without one.
pub trait SpottingDecoder {
    fn decode(&self, ids: &[u32]) -> anyhow::Result<String>;
}

/// The production recognizer.
pub trait OcrEngine: Send + Sync {
    /// Model, revision, task prompt, preprocessing and threshold, as one
    /// string. Enters the extraction recipe.
    fn identity(&self) -> String;

    /// The threshold this engine's admission signal is compared against.
    fn admission_threshold(&self) -> f32;

    /// Transcribe every text region of each image, one result per input, in
    /// the order they were given.
    ///
    /// A batch and not a single image, because this is the unit that crosses a
    /// process boundary and the boundary is what makes it a batch. Recognizing
    /// one image per request left the host looping — issuing a request,
    /// waiting minutes, issuing another — and a loop that issues requests is
    /// work that outlives any attempt to kill the process serving it. A
    /// document's images go in one call, so the caller waits in exactly one
    /// place and killing the recognizer ends the wait.
    ///
    /// An engine that recognizes one image at a time satisfies this by
    /// looping; the point is where the loop lives, not that one exists.
    fn spot_batch(&self, images: &[image::RgbImage]) -> anyhow::Result<Vec<ImageRecognition>>;

    /// Let go of whatever this engine keeps resident, without ending it.
    ///
    /// Called when a caller knows it has no more images for a while — the
    /// index build, which reads every figure in one pass and then embeds for
    /// as long again. A recognizer that stays loaded through the embedding
    /// pass holds a second model's worth of memory to answer no questions,
    /// and an idle timeout reaps it anyway at a moment nobody chose.
    ///
    /// Reversible by construction: the next `spot_batch` loads again. An
    /// engine that runs in this process holds nothing the host can hand back,
    /// so doing nothing is the honest answer and the default.
    fn release(&self) {}
}

/// Parse a spotting response into regions.
///
/// The response is a flat token stream in which each text instance is followed
/// by eight location tokens — `x`,`y` for the top-left, top-right,
/// bottom-right and bottom-left corners in that order. Emission order is the
/// model's reading order and is preserved: reordering the regions here would
/// be a layout decision, and this phase does not make those.
///
/// A trailing run of text with no coordinates, or a run of location tokens
/// that is not eight long, is dropped rather than guessed at. Autoregressive
/// transcription can truncate, and half a quadrilateral is not a location.
pub fn parse_spotting(
    tokens: &[SpottingToken],
    decoder: &dyn SpottingDecoder,
) -> anyhow::Result<Vec<SpottedRegion>> {
    let mut regions = Vec::new();
    let mut text_ids: Vec<u32> = Vec::new();
    let mut text_logprobs: Vec<f32> = Vec::new();
    let mut coordinates: Vec<u16> = Vec::new();

    for token in tokens {
        match token.loc {
            Some(value) => coordinates.push(value),
            None => {
                // A text token after a complete quadrilateral starts the next
                // region; one after a partial quadrilateral means the model
                // emitted something this parser cannot place, and the whole
                // pending instance goes.
                if !coordinates.is_empty() {
                    if coordinates.len() == 8 {
                        if let Some(region) =
                            finish(&text_ids, &text_logprobs, &coordinates, decoder)?
                        {
                            regions.push(region);
                        }
                    }
                    text_ids.clear();
                    text_logprobs.clear();
                    coordinates.clear();
                }
                text_ids.push(token.id);
                text_logprobs.push(token.logprob);
            }
        }
    }
    if coordinates.len() == 8 {
        if let Some(region) = finish(&text_ids, &text_logprobs, &coordinates, decoder)? {
            regions.push(region);
        }
    }
    Ok(regions)
}

fn finish(
    text_ids: &[u32],
    logprobs: &[f32],
    coordinates: &[u16],
    decoder: &dyn SpottingDecoder,
) -> anyhow::Result<Option<SpottedRegion>> {
    let text = normalize_recognized_text(&decoder.decode(text_ids)?);
    if text.is_empty() {
        return Ok(None);
    }
    let scale = f32::from(LOC_MAX);
    let point = |index: usize| Point {
        x: f32::from(coordinates[index * 2]).min(scale) / scale,
        y: f32::from(coordinates[index * 2 + 1]).min(scale) / scale,
    };
    let confidence = if logprobs.is_empty() {
        0.0
    } else {
        (logprobs.iter().sum::<f32>() / logprobs.len() as f32).exp()
    };
    Ok(Some(SpottedRegion {
        // Spotting transcribes; it does not classify. Every region it
        // produces is the reading, and saying so here is what keeps the kind
        // a fact about the region rather than a guess made downstream.
        kind: RegionKind::Text,
        text,
        confidence: confidence.clamp(0.0, 1.0),
        quad: [point(0), point(1), point(2), point(3)],
        // A region only reaches here with a complete quadrilateral — the
        // caller above drops any run of coordinates shorter than eight,
        // which is exactly what a decode cut off mid-region leaves behind.
        // The text preceding a complete quad was fully decoded before the
        // cap could have ended the stream, so nothing this parser hands back
        // is truncated by construction.
        truncated: false,
        // Spotting transcribes; it fills no grid from the page.
        structure: None,
    }))
}

/// Collapse the whitespace a recognizer emits and nothing else.
///
/// Punctuation, percent signs, units, diacritics and scripts all survive
/// verbatim: they are the content. Character-aware throughout — a transcription
/// is arbitrary Unicode, and there is no byte offset here that is safe to
/// assume lands on a character boundary.
pub fn normalize_recognized_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out
}

/// Bumped when the geometry below changes: how a normalized quad becomes
/// pixels, how pixels become page coordinates, or what the deduplication
/// against native glyphs counts as the same claim.
///
/// This is Wilkes' own mapping, not the engine's, which is why it is versioned
/// here and not inside the engine's settings. It is in the analyzer identity
/// because a region that moves is a different reading: the bytes may be
/// identical and still resolve to a different part of the page, and a search
/// hit that lands somewhere else is exactly the failure this feature exists to
/// avoid.
pub const MAPPING_VERSION: &str = "image-mapping-v1";

/// Bumped when a kind's admission rule changes: the threshold comparison, what
/// counts as parseable LaTeX, what shape a table has to have, or which kinds
/// may displace the glyphs a page drew.
///
/// In the analyzer identity for the same reason the threshold is in the
/// engine's: the rules decide which recognized bytes reach the reading, so two
/// readings produced under different rules are different readings even when
/// the same model read the same picture.
///
/// v2: admission gained the region's origin. A typeset region is refused when
/// what came back is prose or code, because those bytes would go into the
/// reading in place of the author's own glyphs.
///
/// v3: admission gained the truncated-decode refusal. A reading admitted
/// under v2 could be a decode that never reached EOS — Texify's `$$…$$`
/// unwrapping left an unterminated fence behind, and the LaTeX inside still
/// closed by chance often enough to pass `latex_parses`. Bumped so a stored
/// annotation from before this rule existed is never re-served as-is: the
/// cache key carries this constant, so the old entry simply stops being
/// found and the image is re-recognized under the rule that would have
/// refused it.
///
/// v4: admission gained three clauses for a table built from a structure
/// model's grid — an empty first row, a word of the page left in no cell, and
/// a grid too much of which is blank. They do not apply to a table a page
/// reader transcribed, which carries no grid, and they exist because
/// [`markdown_table_is_rectangular`] cannot see any of them: a grid shifted one
/// row off the ink is perfectly rectangular.
pub const ADMISSION_RULES_VERSION: &str = "image-admission-v4";

/// A table built from a structure model's grid is refused when strictly more
/// than one in `TABLE_MAX_EMPTY_CELL_DENOMINATOR` of its grid positions holds
/// no glyph — a third, written as integers so the comparison is exact and a
/// table sitting on the boundary does not depend on a float.
///
/// A third from the 56 table crops of the corpus this was measured on. Of the
/// 51 grids that came back rectangular, the ones that read correctly reach
/// 7 empty positions of 32 — 0.219 — at their sparsest, and the one that did
/// not is 2 of 4, at 0.500. A third sits in that gap, nearer the failure than
/// the successes so that a genuinely sparse table is not thrown away for being
/// sparse. What it catches is a grid that is mostly air, which is what a model
/// proposing rows over blank paper produces.
///
/// The other two clauses are not thresholds and have no constant: a first row
/// with no glyph in it, and a word inside the table's own rectangle that landed
/// in no cell, are each wrong at one.
pub const TABLE_MAX_EMPTY_CELL_NUMERATOR: u32 = 1;
pub const TABLE_MAX_EMPTY_CELL_DENOMINATOR: u32 = 3;

/// Whether a formula's LaTeX parses.
///
/// Structural, not semantic: this says the expression is *closed* — every
/// group, environment and `\left` has its partner, and it does not stop in the
/// middle of a command. That is the failure a truncating decoder produces, and
/// it is the one confidence cannot see. Judging whether the mathematics is the
/// mathematics the figure draws is not something a parser can do, and nothing
/// here pretends otherwise.
///
/// Character-aware throughout: LaTeX carries arbitrary Unicode in its text
/// arguments and there is no byte offset here safe to assume lands on a
/// character boundary.
pub fn latex_parses(latex: &str) -> bool {
    let text = latex.trim();
    if text.is_empty() {
        return false;
    }

    let mut braces = 0i32;
    let mut brackets = 0i32;
    let mut left_right = 0i32;
    let mut environments: Vec<String> = Vec::new();
    let mut dollars = 0u32;

    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            // An escape covers exactly the next character, so `\{` and `\$`
            // are literals and never delimiters.
            '\\' => {
                let Some(command) = read_command(&chars, i) else {
                    // A trailing backslash with nothing after it: the decode
                    // stopped inside a command.
                    return false;
                };
                match command.name.as_str() {
                    "left" => left_right += 1,
                    "right" => left_right -= 1,
                    "begin" | "end" => {
                        let Some(name) = read_group(&chars, command.end) else {
                            return false;
                        };
                        if command.name == "begin" {
                            environments.push(name.text);
                        } else if environments.pop().as_deref() != Some(name.text.as_str()) {
                            return false;
                        }
                        i = name.end;
                        continue;
                    }
                    _ => {}
                }
                if left_right < 0 {
                    return false;
                }
                i = command.end;
                continue;
            }
            '{' => braces += 1,
            '}' => {
                braces -= 1;
                if braces < 0 {
                    return false;
                }
            }
            '[' => brackets += 1,
            ']' => {
                brackets -= 1;
                if brackets < 0 {
                    return false;
                }
            }
            '$' => dollars += 1,
            _ => {}
        }
        i += 1;
    }

    braces == 0
        && brackets == 0
        && left_right == 0
        && environments.is_empty()
        && dollars.is_multiple_of(2)
}

struct Command {
    name: String,
    /// Index of the first character after the command.
    end: usize,
}

/// Read `\name` — or `\<single character>` for the escapes that are not
/// alphabetic — starting at the backslash.
fn read_command(chars: &[char], at: usize) -> Option<Command> {
    let mut end = at + 1;
    if end >= chars.len() {
        return None;
    }
    if !chars[end].is_alphabetic() {
        return Some(Command {
            name: chars[end].to_string(),
            end: end + 1,
        });
    }
    let start = end;
    while end < chars.len() && chars[end].is_alphabetic() {
        end += 1;
    }
    Some(Command {
        name: chars[start..end].iter().collect(),
        end,
    })
}

struct Group {
    text: String,
    /// Index of the first character after the closing brace.
    end: usize,
}

/// Read `{...}` at `at`, skipping the spaces LaTeX allows before it.
fn read_group(chars: &[char], at: usize) -> Option<Group> {
    let mut start = at;
    while start < chars.len() && chars[start] == ' ' {
        start += 1;
    }
    if start >= chars.len() || chars[start] != '{' {
        return None;
    }
    let mut end = start + 1;
    while end < chars.len() && chars[end] != '}' {
        end += 1;
    }
    if end >= chars.len() {
        return None;
    }
    Some(Group {
        text: chars[start + 1..end].iter().collect(),
        end: end + 1,
    })
}

/// Whether a transcription is a well-formed Markdown table: a header row, its
/// delimiter, at least one body row, and every row the same width of at least
/// two columns.
///
/// This is the admission rule for both `Table` and `Chart`, and it lives here
/// rather than inside a recognizer's parser so that one rule decides it for
/// every engine. A parser that quietly returned nothing for a ragged table
/// would be a second admission mechanism, and a rejection nobody counted.
pub fn markdown_table_is_rectangular(table: &str) -> bool {
    let rows: Vec<Vec<&str>> = table
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.trim_start_matches('|')
                .trim_end_matches('|')
                .split('|')
                .map(str::trim)
                .collect()
        })
        .collect();
    // Header, delimiter, and a body row.
    if rows.len() < 3 {
        return false;
    }
    let width = rows[0].len();
    if width < 2 || rows.iter().any(|row| row.len() != width) {
        return false;
    }
    rows[1]
        .iter()
        .all(|cell| !cell.is_empty() && cell.chars().all(|c| c == '-' || c == ':'))
}

/// Whether one region enters the reading, and why not when it does not.
///
/// Each kind is admitted by what makes *that* kind wrong. Confidence-
/// thresholding is a text rule and does not transfer: a formula is admitted on
/// validity and a table on structure, because a decoder that truncates them
/// scores its own truncation highly. The native-glyph checks come first for
/// every kind — the document's own glyphs are the better evidence whatever the
/// recognizer called this region — and there are two of them, one for each
/// origin: an embedded picture is refused when the page already draws the
/// words over it, and a typeset region is refused when what came back is not a
/// kind worth displacing a glyph run for. A truncated decode is refused next,
/// ahead of every kind's own rule: whether a formula's LaTeX closes or a
/// table's rows line up is a question about *content* the decoder finished
/// producing, and a decode that hit its cap never finished.
#[allow(clippy::too_many_arguments)]
fn admit(
    origin: RegionOrigin,
    kind: RegionKind,
    text: &str,
    confidence: f32,
    threshold: f32,
    drawn_natively: bool,
    truncated: bool,
    structure: Option<&super::table_structure::TableFillSummary>,
) -> OcrAdmission {
    if drawn_natively {
        return OcrAdmission::DeduplicatedAgainstNativeText;
    }
    // The same verdict for the same reason, reached by kind instead of by
    // text: these bytes would go into the reading *in place of* glyphs the
    // page drew, and for prose those glyphs are the better evidence. See
    // [`RegionKind::supersedes_native_glyphs`].
    if origin == RegionOrigin::Typeset && !kind.supersedes_native_glyphs() {
        return OcrAdmission::DeduplicatedAgainstNativeText;
    }
    if truncated {
        return OcrAdmission::RejectedTruncated;
    }
    match kind {
        RegionKind::Text | RegionKind::Code => {
            if confidence < threshold {
                OcrAdmission::RejectedLowConfidence
            } else {
                OcrAdmission::Accepted
            }
        }
        RegionKind::Formula => {
            if latex_parses(text) {
                OcrAdmission::Accepted
            } else {
                OcrAdmission::RejectedInvalidLatex
            }
        }
        RegionKind::Table | RegionKind::Chart => {
            // Kept, and asked first, for a table from a structure model too.
            // It is the rule about the *shape of the bytes that enter the
            // reading* — a header row, its delimiter, and every row the same
            // width of at least two columns — and nothing about a grid
            // guarantees that: a one-column grid expands to a one-column
            // Markdown table, which is a list and not a table, and five of the
            // 56 measured crops are exactly that. It is also what keeps one
            // rule deciding this for every engine, which is why it is here and
            // not inside a parser.
            if !markdown_table_is_rectangular(text) {
                return OcrAdmission::RejectedMalformedTable;
            }
            // And then the three a rectangle cannot show. Only for a table
            // whose grid the host filled: a transcription has no grid, no word
            // it could have failed to place, and nothing here to judge it on.
            match structure {
                None => OcrAdmission::Accepted,
                Some(structure) => admit_filled_table(structure),
            }
        }
    }
}

/// The three clauses a table built from a structure model's grid answers to,
/// in the order a reader would want the reason reported.
///
/// Each is a different failure of the same model and each is counted apart —
/// see [`crate::types::ExtractionDiagnostics`]. None of them is a score: what
/// makes a filled grid wrong is that it does not hold the page's glyphs, and
/// that is a fact rather than a confidence.
fn admit_filled_table(structure: &super::table_structure::TableFillSummary) -> OcrAdmission {
    // A grid whose first row holds nothing is one shifted off the ink: the
    // model proposed a band of cells above the table. The body below it may
    // look perfectly plausible, which is exactly why this is refused rather
    // than admitted with a blank header.
    if structure.first_row_empty {
        return OcrAdmission::RejectedEmptyHeaderRow;
    }
    // The glyphs are certainly there — the page drew them and the crop was cut
    // around them — so a word with nowhere to go is a cell the grid does not
    // have. Admitting it would put this table into the reading *in place of*
    // the page's own run while losing part of that run.
    if structure.unassigned_words > 0 {
        return OcrAdmission::RejectedUnassignedWords;
    }
    // Integer arithmetic, so a table exactly on the boundary is decided by the
    // rule rather than by a float: refused when `empty / cells` is strictly
    // greater than the fraction the two constants name.
    if structure.empty_cells * TABLE_MAX_EMPTY_CELL_DENOMINATOR
        > structure.cells * TABLE_MAX_EMPTY_CELL_NUMERATOR
    {
        return OcrAdmission::RejectedSparseTable;
    }
    OcrAdmission::Accepted
}

/// Place each spotted region on the page, and decide whether it enters the
/// reading.
///
/// Three outcomes, all recorded: admitted, below the threshold, or already
/// present as native glyphs. Nothing is discarded silently — a label missing
/// from the reading is answerable from the image's own regions.
#[allow(clippy::too_many_arguments)]
pub fn place_and_admit(
    spotted: Vec<SpottedRegion>,
    transform: &ImageTransform,
    image_bbox: &BoundingBox,
    pixel_width: u32,
    pixel_height: u32,
    page: u32,
    origin: RegionOrigin,
    context: &AnalysisContext,
    threshold: f32,
) -> Vec<ImageOcrRegion> {
    // Only an embedded picture can duplicate the page's glyphs. A typeset
    // region *is* those glyphs, rendered so they could be read as the
    // mathematics or the table they were set as, and the recognizer is the
    // designated owner of the bytes there — the page's own run leaves the
    // reading when this answer is admitted. Asking the duplicate question of
    // it would refuse every region for being what it was marked out for.
    let native = match origin {
        RegionOrigin::Embedded => context.native_text_within(page, image_bbox),
        RegionOrigin::Typeset => String::new(),
    };
    spotted
        .into_iter()
        .map(|region| {
            let polygon_within_image: Vec<Point> = region
                .quad
                .iter()
                .map(|point| Point {
                    x: point.x * pixel_width as f32,
                    y: point.y * pixel_height as f32,
                })
                .collect();
            let page_polygon: Vec<Point> = polygon_within_image
                .iter()
                .map(|point| transform.pixel_to_page(point.x, point.y, pixel_width, pixel_height))
                .collect();
            let comparable = normalize_for_comparison(&region.text);
            let drawn_natively = !comparable.is_empty() && native.contains(comparable.as_str());
            let admission = admit(
                origin,
                region.kind,
                &region.text,
                region.confidence,
                threshold,
                drawn_natively,
                region.truncated,
                region.structure.as_ref(),
            );
            ImageOcrRegion {
                // Carried, never decided here. What a region contains is the
                // recognizer's answer; this function places it on the page and
                // says whether it may enter the reading, and a kind assigned
                // downstream would be a second router.
                kind: region.kind,
                text: region.text,
                confidence: region.confidence,
                polygon_within_image,
                page_polygon,
                admission,
            }
        })
        .collect()
}

/// The comparison form for "does the page already draw this". Case-folded,
/// whitespace-collapsed, and stripped of the punctuation a label carries at
/// either end — so `Knowledge base` and `knowledge base.` are the same claim.
pub fn normalize_for_comparison(text: &str) -> String {
    normalize_recognized_text(text)
        .to_lowercase()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

/// Whether a point lies inside a rectangle.
pub(super) fn contains(bbox: &BoundingBox, point: &Point) -> bool {
    point.x >= bbox.x
        && point.x <= bbox.x + bbox.width
        && point.y >= bbox.y
        && point.y <= bbox.y + bbox.height
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table built from a structure model's grid, as the analyzer builds it:
    /// the Markdown that would enter the reading, and the facts about the fill
    /// that produced it.
    fn filled_table(
        markdown: &str,
        cells: u32,
        empty_cells: u32,
        unassigned_words: u32,
        first_row_empty: bool,
    ) -> OcrAdmission {
        admit(
            RegionOrigin::Typeset,
            RegionKind::Table,
            markdown,
            // Deliberately below any threshold: a filled table is never
            // admitted or refused on a score, and a test that passed a
            // comfortable confidence would not show that.
            0.0,
            0.9,
            false,
            false,
            Some(&super::super::table_structure::TableFillSummary {
                cells,
                empty_cells,
                unassigned_words,
                words_in_box: 0,
                first_row_empty,
            }),
        )
    }

    /// A clean 2x3 table, filled: the shape every one of the clauses below is
    /// a departure from.
    const CLEAN: &str = "| a | b | c |\n| --- | --- | --- |\n| d | e | f |\n";

    /// The ordinary case. Nothing about a table from a grid is admitted on a
    /// score, and this one carries none.
    #[test]
    fn a_filled_table_that_holds_the_pages_glyphs_is_admitted() {
        assert_eq!(filled_table(CLEAN, 6, 0, 0, false), OcrAdmission::Accepted);
        // A few blank positions are ordinary — a truth table's leading column
        // is set once and left empty under it — and stay admitted.
        assert_eq!(filled_table(CLEAN, 6, 2, 0, false), OcrAdmission::Accepted);
    }

    /// A first row with no glyph in it is a grid shifted off the ink: the
    /// model proposed a band of cells above the table. Refused even though the
    /// Markdown is a perfect rectangle, which is exactly the failure
    /// `markdown_table_is_rectangular` cannot see.
    #[test]
    fn a_filled_table_with_an_empty_first_row_is_refused() {
        let shifted = "|  |  |  |\n| --- | --- | --- |\n| d | e | f |\n";
        assert!(markdown_table_is_rectangular(shifted));
        assert_eq!(
            filled_table(shifted, 6, 3, 0, true),
            OcrAdmission::RejectedEmptyHeaderRow
        );
    }

    /// A word the page draws inside the table's own rectangle that landed in no
    /// cell is a column the grid does not have. Admitting the table would put
    /// it into the reading in place of the page's run while losing that word.
    #[test]
    fn a_filled_table_that_left_a_word_unplaced_is_refused() {
        assert_eq!(
            filled_table(CLEAN, 6, 0, 1, false),
            OcrAdmission::RejectedUnassignedWords
        );
    }

    /// A grid that is mostly air. The boundary is the rule's own fraction, and
    /// it is checked on both sides: a table exactly at it is admitted, one past
    /// it is not.
    #[test]
    fn a_filled_table_that_is_mostly_empty_is_refused() {
        // Exactly a third: admitted, because the rule refuses *more* than that.
        assert_eq!(filled_table(CLEAN, 6, 2, 0, false), OcrAdmission::Accepted);
        // One position more.
        assert_eq!(
            filled_table(CLEAN, 6, 3, 0, false),
            OcrAdmission::RejectedSparseTable
        );
        // And the shape the corpus actually produced: a 2x2 grid over a caption,
        // two of four positions blank.
        assert_eq!(
            filled_table("| a | b |\n| --- | --- |\n|  |  |\n", 4, 2, 0, false),
            OcrAdmission::RejectedSparseTable
        );
    }

    /// The clauses are ordered so the reason names the worst thing about the
    /// table, and raggedness — which is a fact about the bytes rather than
    /// about the fill — is still asked first.
    #[test]
    fn a_ragged_filled_table_is_refused_as_malformed_before_anything_else() {
        // One column: a list, not a table. Five of the 56 measured crops are
        // exactly this, and this is the clause that catches them.
        let one_column = "| a |\n| --- |\n| b |\n";
        assert!(!markdown_table_is_rectangular(one_column));
        assert_eq!(
            filled_table(one_column, 2, 1, 1, true),
            OcrAdmission::RejectedMalformedTable,
        );
    }

    /// A structure summary is not consulted for anything but a table. A
    /// formula carrying one — which nothing produces — must still be admitted
    /// on whether its LaTeX closes.
    #[test]
    fn the_structural_clauses_apply_to_tables_only() {
        assert_eq!(
            admit(
                RegionOrigin::Typeset,
                RegionKind::Formula,
                "x^{2}",
                0.0,
                0.9,
                false,
                false,
                Some(&super::super::table_structure::TableFillSummary {
                    cells: 4,
                    empty_cells: 4,
                    unassigned_words: 9,
                    words_in_box: 9,
                    first_row_empty: true,
                }),
            ),
            OcrAdmission::Accepted
        );
    }

    /// A table a page reader transcribed carries no summary, so the structural
    /// clauses do not apply to it: nothing filled a grid, so nothing could have
    /// left a word out of one. It is admitted on its shape exactly as before.
    #[test]
    fn a_transcribed_table_is_admitted_on_its_shape_alone() {
        assert_eq!(
            admit_transcribed(
                RegionOrigin::Typeset,
                RegionKind::Table,
                CLEAN,
                0.0,
                0.9,
                false,
                false
            ),
            OcrAdmission::Accepted
        );
    }

    /// [`admit`] for a region a recognizer transcribed — which is every region
    /// but a table built from a structure model's grid. Those carry a
    /// [`super::super::table_structure::TableFillSummary`] and are covered by
    /// their own tests below.
    #[allow(clippy::too_many_arguments)]
    fn admit_transcribed(
        origin: RegionOrigin,
        kind: RegionKind,
        text: &str,
        confidence: f32,
        threshold: f32,
        drawn_natively: bool,
        truncated: bool,
    ) -> OcrAdmission {
        admit(
            origin,
            kind,
            text,
            confidence,
            threshold,
            drawn_natively,
            truncated,
            None,
        )
    }

    /// Ids below 1000 stand for the character at that code point offset from
    /// `a`, which is enough to spell words in a test without a tokenizer.
    struct Letters;
    impl SpottingDecoder for Letters {
        fn decode(&self, ids: &[u32]) -> anyhow::Result<String> {
            Ok(ids
                .iter()
                .map(|id| char::from_u32(*id).unwrap_or('?'))
                .collect())
        }
    }

    fn text_token(c: char) -> SpottingToken {
        SpottingToken {
            id: c as u32,
            logprob: 0.0,
            loc: None,
        }
    }

    fn loc(value: u16) -> SpottingToken {
        SpottingToken {
            id: 0,
            logprob: 0.0,
            loc: Some(value),
        }
    }

    fn spell(word: &str) -> Vec<SpottingToken> {
        word.chars().map(text_token).collect()
    }

    fn quad(values: [u16; 8]) -> Vec<SpottingToken> {
        values.iter().map(|value| loc(*value)).collect()
    }

    #[test]
    fn a_region_is_its_text_followed_by_four_corners() {
        let mut tokens = spell("DREAM");
        tokens.extend(quad([253, 286, 346, 298, 345, 339, 252, 330]));

        let regions = parse_spotting(&tokens, &Letters).expect("parses");
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].text, "DREAM");
        assert_eq!(regions[0].quad[0], Point { x: 0.253, y: 0.286 });
        assert_eq!(regions[0].quad[2], Point { x: 0.345, y: 0.339 });
    }

    #[test]
    fn regions_keep_the_order_the_model_emitted_them_in() {
        let mut tokens = spell("first");
        tokens.extend(quad([0, 0, 10, 0, 10, 10, 0, 10]));
        tokens.extend(spell("second"));
        tokens.extend(quad([0, 20, 10, 20, 10, 30, 0, 30]));

        let regions = parse_spotting(&tokens, &Letters).expect("parses");
        let texts: Vec<&str> = regions.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, vec!["first", "second"]);
    }

    /// Autoregressive transcription can stop mid-instance. Half a
    /// quadrilateral is not a location, so the instance goes rather than
    /// being placed at a guessed corner.
    #[test]
    fn a_truncated_quadrilateral_drops_its_region() {
        let mut tokens = spell("cut");
        tokens.extend(quad([1, 2, 3, 4, 5, 6, 7, 8]));
        tokens.extend(spell("short"));
        tokens.extend(vec![loc(1), loc(2), loc(3)]);

        let regions = parse_spotting(&tokens, &Letters).expect("parses");
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].text, "cut");
    }

    #[test]
    fn text_with_no_coordinates_at_all_yields_nothing() {
        let regions = parse_spotting(&spell("unplaced"), &Letters).expect("parses");
        assert!(regions.is_empty());
    }

    /// The signal is the mean probability of the tokens that spell the text,
    /// so a decode that hesitated over its characters scores below one that
    /// did not.
    #[test]
    fn confidence_comes_from_the_decodes_own_log_probabilities() {
        let mut confident = spell("sure");
        for token in &mut confident {
            token.logprob = -0.01;
        }
        confident.extend(quad([0, 0, 1, 0, 1, 1, 0, 1]));

        let mut hesitant = spell("maybe");
        for token in &mut hesitant {
            token.logprob = -2.0;
        }
        hesitant.extend(quad([0, 0, 1, 0, 1, 1, 0, 1]));

        let confident = parse_spotting(&confident, &Letters).expect("parses");
        let hesitant = parse_spotting(&hesitant, &Letters).expect("parses");
        assert!(confident[0].confidence > 0.98);
        assert!(hesitant[0].confidence < 0.2);
    }

    #[test]
    fn normalization_collapses_whitespace_and_keeps_everything_else() {
        assert_eq!(
            normalize_recognized_text("  50 %\tof\n Größe — Ω  "),
            "50 % of Größe — Ω"
        );
    }

    /// A truncating decoder is perfectly confident about every token it did
    /// emit, so confidence cannot see the failure that matters for a formula.
    /// Validity can.
    #[test]
    fn a_formula_is_admitted_on_whether_its_latex_closes() {
        assert!(latex_parses("S(q,d) = \\Sigma_{i} w_{i}"));
        assert!(latex_parses("\\left( \\frac{a}{b} \\right)^{2}"));
        assert!(latex_parses(
            "\\begin{matrix} a & b \\\\ c & d \\end{matrix}"
        ));
        assert!(latex_parses("x \\{ y \\}"), "an escaped brace is a literal");

        assert!(!latex_parses(""));
        assert!(!latex_parses("   "));
        assert!(!latex_parses("\\frac{a}{b"), "a group left open");
        assert!(!latex_parses("a}"), "a group closed that never opened");
        assert!(!latex_parses("\\left( a"), "a \\left with no \\right");
        assert!(!latex_parses("a \\right)"), "a \\right with no \\left");
        assert!(
            !latex_parses("\\begin{matrix} a"),
            "an environment left open"
        );
        assert!(
            !latex_parses("\\begin{matrix} a \\end{pmatrix}"),
            "an environment closed as another"
        );
        assert!(
            !latex_parses("a + b \\"),
            "a decode that stopped in a command"
        );
        assert!(!latex_parses("$x = 1"), "an unclosed inline segment");
    }

    /// LaTeX carries arbitrary Unicode in its text arguments, and nothing here
    /// may assume a byte offset lands on a character boundary.
    #[test]
    fn latex_validity_is_character_safe() {
        assert!(latex_parses("\\text{Größe} = 日本語^{2}"));
        assert!(!latex_parses("\\text{Größe"));
    }

    /// A ragged table is a failed recognition wearing the shape of a result,
    /// so structure — not confidence — is what admits one.
    #[test]
    fn a_table_is_admitted_on_being_rectangular() {
        assert!(markdown_table_is_rectangular(
            "| Corpus | Recall |\n| --- | --- |\n| Reports | 0.91 |"
        ));
        assert!(markdown_table_is_rectangular(
            "| a | b |\n| :-- | --: |\n| 1 | 2 |\n| 3 | 4 |"
        ));

        assert!(
            !markdown_table_is_rectangular("| a | b |\n| --- | --- |"),
            "a header and a rule are not yet a table"
        );
        assert!(
            !markdown_table_is_rectangular("| a |\n| --- |\n| 1 |"),
            "one column is not a table"
        );
        assert!(
            !markdown_table_is_rectangular("| a | b |\n| --- | --- |\n| 1 |"),
            "a short row makes it ragged"
        );
        assert!(
            !markdown_table_is_rectangular("| a | b |\n| x | y |\n| 1 | 2 |"),
            "the delimiter row is part of being a Markdown table"
        );
        assert!(!markdown_table_is_rectangular(""));
    }

    /// Each kind is admitted by what makes *that* kind wrong. A formula and a
    /// table both score highly and are both refused when they are broken; a
    /// paragraph is refused only by the threshold.
    #[test]
    fn each_kind_is_admitted_by_its_own_rule() {
        let table = "| a | b |\n| --- | --- |\n| 1 | 2 |";
        assert_eq!(
            admit_transcribed(
                RegionOrigin::Embedded,
                RegionKind::Text,
                "Knowledge base",
                0.9,
                0.7,
                false,
                false
            ),
            OcrAdmission::Accepted
        );
        assert_eq!(
            admit_transcribed(
                RegionOrigin::Embedded,
                RegionKind::Text,
                "blurred",
                0.4,
                0.7,
                false,
                false
            ),
            OcrAdmission::RejectedLowConfidence
        );
        assert_eq!(
            admit_transcribed(
                RegionOrigin::Embedded,
                RegionKind::Formula,
                "E = mc^{2}",
                0.1,
                0.7,
                false,
                false
            ),
            OcrAdmission::Accepted,
            "a valid formula is not thresholded on confidence"
        );
        assert_eq!(
            admit_transcribed(
                RegionOrigin::Embedded,
                RegionKind::Formula,
                "\\frac{a}{b",
                0.99,
                0.7,
                false,
                false
            ),
            OcrAdmission::RejectedInvalidLatex,
            "a confident truncation is still a truncation"
        );
        assert_eq!(
            admit_transcribed(
                RegionOrigin::Embedded,
                RegionKind::Table,
                table,
                0.1,
                0.7,
                false,
                false
            ),
            OcrAdmission::Accepted
        );
        assert_eq!(
            admit_transcribed(
                RegionOrigin::Embedded,
                RegionKind::Table,
                "| a |\n| --- |\n| 1 |",
                0.99,
                0.7,
                false,
                false
            ),
            OcrAdmission::RejectedMalformedTable
        );
        assert_eq!(
            admit_transcribed(
                RegionOrigin::Embedded,
                RegionKind::Chart,
                table,
                0.99,
                0.7,
                false,
                false
            ),
            OcrAdmission::Accepted,
            "a chart is admitted as a table, by the same rule"
        );
        assert_eq!(
            admit_transcribed(
                RegionOrigin::Embedded,
                RegionKind::Chart,
                "roughly rising",
                0.99,
                0.7,
                false,
                false
            ),
            OcrAdmission::RejectedMalformedTable
        );
    }

    /// A decode that hit its cap is refused whatever the bytes it emitted
    /// look like — LaTeX that would otherwise parse included. Confidence and
    /// structural validity both stay silent about this failure; only the
    /// decoder itself knows it never reached EOS.
    #[test]
    fn a_truncated_decode_is_refused_even_when_it_would_otherwise_parse() {
        assert_eq!(
            admit_transcribed(
                RegionOrigin::Embedded,
                RegionKind::Formula,
                "E = mc^{2}",
                0.99,
                0.7,
                false,
                true,
            ),
            OcrAdmission::RejectedTruncated,
            "closed LaTeX from a decode that hit its cap is still not a reading"
        );
    }

    /// The flag changes nothing about a decode that actually finished: the
    /// existing per-kind rules still decide it.
    #[test]
    fn a_completed_decode_is_admitted_as_before() {
        assert_eq!(
            admit_transcribed(
                RegionOrigin::Embedded,
                RegionKind::Formula,
                "E = mc^{2}",
                0.99,
                0.7,
                false,
                false,
            ),
            OcrAdmission::Accepted
        );
    }

    /// The document's own glyphs are the better evidence whatever the
    /// recognizer called the region, so the native check comes first for every
    /// kind.
    #[test]
    fn native_glyphs_outrank_every_kinds_rule() {
        for kind in RegionKind::ALL {
            assert_eq!(
                admit_transcribed(
                    RegionOrigin::Embedded,
                    kind,
                    "E = mc^{2}",
                    0.99,
                    0.7,
                    true,
                    false
                ),
                OcrAdmission::DeduplicatedAgainstNativeText,
                "{kind:?}"
            );
        }
    }

    /// A typeset region's bytes go into the reading *in place of* the glyphs
    /// the page drew. A formula or a table is why the region was marked out
    /// and is worth that; prose is not — the author's own glyphs are better
    /// evidence for a sentence than a model's reading of a picture of one.
    #[test]
    fn only_a_kind_worth_displacing_glyphs_is_admitted_from_a_typeset_region() {
        let table = "| a | b |\n| --- | --- |\n| 1 | 2 |";
        let of = |kind, text| {
            admit_transcribed(RegionOrigin::Typeset, kind, text, 0.99, 0.7, false, false)
        };

        assert_eq!(
            of(RegionKind::Formula, "E = mc^{2}"),
            OcrAdmission::Accepted
        );
        assert_eq!(of(RegionKind::Table, table), OcrAdmission::Accepted);
        assert_eq!(of(RegionKind::Chart, table), OcrAdmission::Accepted);
        for kind in [RegionKind::Text, RegionKind::Code] {
            assert_eq!(
                of(kind, "for all i we have"),
                OcrAdmission::DeduplicatedAgainstNativeText,
                "{kind:?}"
            );
        }
    }

    /// The same kinds read out of an embedded picture are admitted as they
    /// always were: there are no glyphs of the document's under a figure, so
    /// nothing is being displaced.
    #[test]
    fn the_typeset_rule_does_not_reach_an_embedded_picture() {
        assert_eq!(
            admit_transcribed(
                RegionOrigin::Embedded,
                RegionKind::Text,
                "Knowledge base",
                0.99,
                0.7,
                false,
                false
            ),
            OcrAdmission::Accepted
        );
    }

    /// Placement carries the recognizer's answer about what a region is; it
    /// never forms one of its own. A kind decided here would be a second
    /// router, and the design has exactly one.
    #[test]
    fn placement_carries_the_kind_and_does_not_assign_one() {
        let spotted = vec![SpottedRegion {
            kind: RegionKind::Formula,
            text: "E = mc^{2}".to_string(),
            confidence: 0.2,
            quad: [Point { x: 0.0, y: 0.0 }; 4],
            truncated: false,
            structure: None,
        }];
        let placed = place_and_admit(
            spotted,
            &ImageTransform {
                a: 10.0,
                b: 0.0,
                c: 0.0,
                d: 10.0,
                e: 0.0,
                f: 0.0,
            },
            &BoundingBox {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            100,
            100,
            1,
            RegionOrigin::Embedded,
            &AnalysisContext::default(),
            0.7,
        );
        assert_eq!(placed[0].kind, RegionKind::Formula);
        assert_eq!(
            placed[0].admission,
            OcrAdmission::Accepted,
            "the formula rule ran, not the threshold"
        );
    }

    /// The one thing byte indexing would break. A transcription is arbitrary
    /// Unicode and is never sliced by byte offset.
    #[test]
    fn normalization_is_character_safe() {
        assert_eq!(
            normalize_recognized_text("日本語  テキスト"),
            "日本語 テキスト"
        );
        assert_eq!(normalize_for_comparison("«Größe»"), "größe");
    }
}
