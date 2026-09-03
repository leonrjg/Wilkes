//! Turning a page's layout into a document's reading.
//!
//! A PDF text page is a raster-ordered list of blocks, and transcribing it
//! verbatim produces a transcription of the *layout*: words broken by the
//! hyphen that ended a line, page numbers and running heads spliced between
//! paragraphs, and margin glossary boxes emitted in the middle of the sentence
//! they sit beside. None of that is in the document's text; all of it is in
//! its typesetting.
//!
//! This module is the pass that removes it, between word extraction and
//! `ExtractedContent` assembly. Every transform here is an edit on the item
//! list, and the text and the source map are both rendered from that list
//! afterwards — so a transform cannot move the text without moving the map
//! with it, which is the property the whole design rests on.
//!
//! What survives is deliberately narrow. Only three classes are removed, each
//! decided from the document's own geometry or vocabulary rather than from a
//! dictionary or a language assumption, and each failing towards today's
//! behaviour rather than towards a guess.

use tracing::debug;

use crate::types::{
    BoundingBox, ByteRange, ExtractedImage, ExtractionDiagnostics, SourceOrigin, SourceSegment,
    TextProvenance,
};

use crate::extract::image::serialize;
use crate::extract::pdf::typeset::Anchor;

/// The flow a line belongs to. Body text is one flow for the whole document —
/// it continues across block and page boundaries, which is what lets a word
/// wrapped at the foot of a page rejoin its other half at the head of the
/// next. Each relocated aside is its own flow, so nothing joins across the
/// seam a relocation creates.
type Flow = u64;

const BODY_FLOW: Flow = 0;

/// One word and the box it was drawn in.
#[derive(Clone, Debug)]
pub(super) struct Word {
    pub text: String,
    pub bbox: Option<BoundingBox>,
    /// The typeset region marked out over this word, as an index into the
    /// document's images.
    ///
    /// Marked at discovery and acted on in [`supersede_typeset_regions`],
    /// after the recognizer has answered — because until then it is not known
    /// whether these glyphs are leaving. A region whose reading is refused
    /// leaves its words exactly where the page put them, which is what makes a
    /// false positive cost time and no bytes.
    ///
    /// A word with no text and a mark is a *carrier*: it stands for a region
    /// and for no glyphs of the page. One is inserted for every admitted
    /// region that covers no word at all — an expression the page drew as
    /// paths — so that from there on the reading has exactly one way to say
    /// where a region goes, which is a marked word.
    pub typeset: Option<usize>,
}

/// One visual line: the whitespace the page put before its first word, then
/// each word with the whitespace that followed it. Whitespace is carried
/// verbatim rather than normalized — this pass reorders and removes text, and
/// respacing it as well would be a fourth transform nobody asked for.
#[derive(Clone, Debug, Default)]
pub(super) struct Line {
    pub leading: String,
    pub words: Vec<Word>,
    pub trailing: Vec<String>,
}

impl Line {
    pub(super) fn push_word(&mut self, word: Word) {
        self.words.push(word);
        self.trailing.push(String::new());
    }

    /// Whitespace seen after the last word, or before the first if there is
    /// none yet.
    pub(super) fn push_space(&mut self, c: char) {
        match self.trailing.last_mut() {
            Some(trailing) => trailing.push(c),
            None => self.leading.push(c),
        }
    }
}

/// One block, as the page grouped it — typically a paragraph, a heading, a
/// running head, or a margin box, or else a native image.
///
/// An image is a block here rather than a list beside the blocks so that it
/// keeps its place among them through every transform in this module. That
/// place *is* the reading anchor: no caption is matched, nothing is inferred
/// from the words nearby, and the enrichment is written where the page drew
/// the picture.
///
/// An image block has no lines, and every judgement in this module is made
/// from words — so it has no extent, is never a furniture candidate, and is
/// never relocated as marginalia. Those are consequences of it having no
/// text, not special cases written for it.
#[derive(Clone, Debug, Default)]
pub(super) struct Block {
    pub lines: Vec<Line>,
    /// Index into the document's images when this block is one.
    pub image: Option<usize>,
}

impl Block {
    fn word_count(&self) -> usize {
        self.lines.iter().map(|line| line.words.len()).sum()
    }

    fn extent(&self) -> Option<Extent> {
        self.lines
            .iter()
            .flat_map(|line| line.words.iter())
            .filter_map(|word| word.bbox.as_ref())
            .fold(None, |extent: Option<Extent>, bbox| {
                let next = Extent::of(bbox);
                Some(match extent {
                    Some(existing) => existing.merge(&next),
                    None => next,
                })
            })
    }
}

#[derive(Clone, Debug)]
pub(super) struct Page {
    pub number: u32,
    /// Page height in the same space as the word boxes: origin top-left, y
    /// down. Zero when the page reports no bounds, which disables the
    /// margin-band half of furniture detection rather than inventing one.
    pub height: f32,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Extent {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Extent {
    fn of(bbox: &BoundingBox) -> Self {
        Self {
            x0: bbox.x,
            y0: bbox.y,
            x1: bbox.x + bbox.width,
            y1: bbox.y + bbox.height,
        }
    }

    fn merge(&self, other: &Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }

    /// How much of the narrower of the two horizontal spans the two share.
    /// Used to decide whether two blocks stand in the same column.
    fn horizontal_overlap_ratio(&self, other: &Self) -> f32 {
        let overlap = self.x1.min(other.x1) - self.x0.max(other.x0);
        let narrower = self.width().min(other.width());
        if narrower <= f32::EPSILON {
            return if overlap >= 0.0 { 1.0 } else { 0.0 };
        }
        (overlap / narrower).clamp(0.0, 1.0)
    }
}

/// A rendered document: the reading, and one segment per surviving word.
pub(super) struct Reading {
    pub text: String,
    pub segments: Vec<SourceSegment>,
}

/// Nothing this short is a paragraph, so a repeating run of at most this many
/// words in a margin band is furniture. A running head that is a full section
/// title stays under it; a paragraph never does. How many lines the page set
/// the run on is not evidence either way — a page number and its unit label
/// are routinely two lines of one block.
const FURNITURE_MAX_WORDS: usize = 12;

/// Fraction of the page height at the top and bottom within which a repeating
/// short run counts as a head or foot.
const FURNITURE_BAND: f32 = 0.15;

/// Vertical resolution, in points, at which two runs count as "the same band".
/// Coarse enough to absorb a baseline that shifts by a fraction of a point
/// between pages, fine enough that a head and the first body line never share
/// a bucket.
const FURNITURE_BAND_BUCKET: f32 = 3.0;

/// Below this many pages there is no such thing as a repeating band: a
/// majority of two pages is one page, which is a coincidence and not evidence.
const FURNITURE_MIN_PAGES: usize = 4;

/// Two blocks stand in the same column when they share at least this much of
/// the narrower one's width.
const COLUMN_OVERLAP: f32 = 0.5;

/// The dominant cluster must hold this share of a page's words to be called
/// the body column. A two-column page splits near half and is therefore
/// ambiguous, which is the intended answer: its "marginalia" would be its
/// second column.
const BODY_COLUMN_SHARE: f32 = 0.6;

/// Remove page furniture, move marginalia out of the reading order, join
/// line-wrapped words, and render the result to text plus source map.
/// Turn the pages into the document's one reading.
///
/// `images` arrives already analyzed, and leaves carrying the byte range each
/// image's enrichment occupies in the reading — filled here because here is
/// where the text is written, and a range computed anywhere else would be a
/// claim about bytes rather than the bytes themselves.
///
/// `anchored` names the typeset regions that cover no word of the page, with
/// the place the page's own geometry gave each of them. They have no glyphs to
/// be marked on, so the mark is put on a word made for them — see
/// [`supersede_typeset_regions`].
pub(super) fn sanitize(
    mut pages: Vec<Page>,
    images: &mut [ExtractedImage],
    anchored: &[(usize, Anchor)],
    diagnostics: &mut ExtractionDiagnostics,
) -> Reading {
    diagnostics.pages = pages.len() as u32;
    supersede_typeset_regions(&mut pages, images, anchored, diagnostics);
    remove_page_furniture(&mut pages, diagnostics);
    let flows = relocate_marginalia(&mut pages, diagnostics);
    let mut items = flatten(&pages, &flows);
    join_wrapped_words(&mut items, diagnostics);
    render(&items, images)
}

// ── Typeset regions ─────────────────────────────────────────────────────────

/// Hand the glyphs of an admitted typeset region over to that region.
///
/// This is the one place the ownership of those bytes is settled, and it
/// settles it by removing the competing claim rather than by reconciling two.
/// A display formula reaches this point twice over: as the glyph run MuPDF
/// read off the page — `ci = ai ⊕bi`, which is not mathematics — and as the
/// region a recognizer read as LaTeX. Exactly one of them is in the reading
/// afterwards.
///
/// A region owns *words* — and a region that covers none of the page's own is
/// given one here, empty of text and carrying its mark, at the place
/// `typeset::anchor_after` read off the page. That is the whole of the special
/// case: an expression the page drew as paths has no glyph run to replace, so
/// a word is put where the reading passes it, and every line below this one
/// treats it exactly as it treats a word the page drew. Inserting only for an
/// *admitted* region is not a special case either — it is the same promise a
/// refused region that owns words keeps, that the page is left as it was.
///
/// So a region can own a whole line, or four words in the middle of one, or
/// one word that was not there before, and the two ways they are written are:
///
/// - **A region that owns every word of the lines it touches is a block.** Its
///   lines go and an image block is written where the first of them was. That
///   is a display formula, a table, a chart: the page set it apart and the
///   reading sets it apart too.
/// - **A region that owns part of a line is spliced into it.** The words go
///   and the region stands between the words either side of it, in the
///   sentence, with the spacing the page had. That is an inline formula, and
///   writing it as a block would take a sentence apart to insert its own
///   middle.
///
/// The recognizer wins only where it actually said something admissible. A
/// region whose formula did not parse, whose table came back ragged, or whose
/// recognition failed outright leaves its words untouched, and the reading is
/// what it was before typeset routing existed. That is deliberate: the failure
/// mode of a wrongly marked-out region is a wasted recognizer call, never a
/// paragraph replaced by nothing.
///
/// Run before every other pass here so that what follows sees an image block
/// where a block region is — a formula is not a furniture candidate and not a
/// marginalia candidate, and it stops being either by the ordinary rule that
/// those are decided from words.
fn supersede_typeset_regions(
    pages: &mut [Page],
    images: &[ExtractedImage],
    anchored: &[(usize, Anchor)],
    diagnostics: &mut ExtractionDiagnostics,
) {
    let admitted = |index: usize| {
        images
            .get(index)
            .is_some_and(|image| image.accepted_ocr().next().is_some())
    };

    // A region whose reading was refused keeps nothing: the mark comes off
    // first so that every pass after this one, here and in `flatten`, can
    // trust a mark it finds.
    for page in pages.iter_mut() {
        for block in page.blocks.iter_mut() {
            for line in block.lines.iter_mut() {
                for word in line.words.iter_mut() {
                    if word.typeset.is_some_and(|index| !admitted(index)) {
                        word.typeset = None;
                    }
                }
            }
        }
    }

    // And a region that covers no word of the page gets one, for the same
    // reason and by the same test: everything below here reads marks off
    // words, so an expression the page drew as paths is given a word to be
    // marked on rather than a second way of saying where a region goes.
    //
    // Applied last position first, so an insertion never moves the position of
    // one not yet applied. Two regions anchored to the same word are ordered
    // by where the page draws them, so a reader meets them in that order: an
    // inline one stands on the anchor's own line and a block one below it, so
    // inline comes first; along a line the reading runs left to right, and
    // down a page it runs top to bottom. Ordering both by height would put two
    // expressions drawn side by side in the order of their *baselines*, which
    // for a tall one beside a short one is not the order they are read in.
    let mut carried: Vec<(usize, Anchor, &ExtractedImage)> = anchored
        .iter()
        .filter(|(index, _)| admitted(*index))
        .filter_map(|(index, anchor)| images.get(*index).map(|image| (*index, *anchor, image)))
        .collect();
    carried.sort_by(|(_, a, left), (_, b, right)| {
        (left.page, a.after, !a.inline)
            .cmp(&(right.page, b.after, !b.inline))
            .then_with(|| {
                // Equal this far means the same anchor and the same kind, so
                // one of the two orders below is the reading's own.
                if a.inline {
                    left.bbox
                        .x
                        .total_cmp(&right.bbox.x)
                        .then(left.bbox.y.total_cmp(&right.bbox.y))
                } else {
                    left.bbox
                        .y
                        .total_cmp(&right.bbox.y)
                        .then(left.bbox.x.total_cmp(&right.bbox.x))
                }
            })
    });
    for (index, anchor, image) in carried.into_iter().rev() {
        let carrier = Word {
            text: String::new(),
            bbox: Some(image.bbox.clone()),
            typeset: Some(index),
        };
        let Some(page) = pages.iter_mut().find(|page| page.number == image.page) else {
            debug!(
                "typeset region {} is anchored to page {}, which this reading does not hold",
                image.id, image.page
            );
            continue;
        };
        let Some((block, line, word)) = anchor.after else {
            // Nothing on the page precedes it.
            page.blocks.insert(
                0,
                Block {
                    lines: vec![line_of(carrier)],
                    image: None,
                },
            );
            continue;
        };
        let target = page
            .blocks
            .get_mut(block)
            .filter(|block| block.image.is_none())
            .and_then(|block| (line < block.lines.len()).then_some(block));
        let Some(target) = target else {
            debug!(
                "typeset region {} is anchored after block {block} line {line} of page {}, \
                 which holds no such line",
                image.id, image.page
            );
            continue;
        };
        if !anchor.inline {
            target.lines.insert(line + 1, line_of(carrier));
            continue;
        }
        let at = &mut target.lines[line];
        if word >= at.words.len() {
            debug!(
                "typeset region {} is anchored after word {word} of a line holding {}",
                image.id,
                at.words.len()
            );
            continue;
        }
        // The carrier takes the whitespace the page left after the word it
        // follows, and a single space is written between the two: the page
        // drew nothing there to copy, and a run of LaTeX abutting the word
        // before it is one token to every consumer of the reading.
        let trailing = std::mem::replace(&mut at.trailing[word], " ".to_string());
        at.words.insert(word + 1, carrier);
        at.trailing.insert(word + 1, trailing);
    }

    // Which regions own every word of every line they touch. Asked across the
    // whole document before anything is rewritten, because a region can span
    // two of a page's blocks and the answer for one of its lines is not the
    // answer for the region.
    let mut whole_line: std::collections::HashMap<usize, bool> = Default::default();
    for page in pages.iter() {
        for block in &page.blocks {
            for line in &block.lines {
                let marks: std::collections::HashSet<usize> =
                    line.words.iter().filter_map(|word| word.typeset).collect();
                for index in marks {
                    // A word with nothing in it is not evidence either way —
                    // unless it carries *another* region, in which case this
                    // line holds two things and taking it away whole would
                    // take the second with it.
                    let all = line.words.iter().all(|word| {
                        word.typeset == Some(index)
                            || (word.typeset.is_none() && word.text.trim().is_empty())
                    });
                    let entry = whole_line.entry(index).or_insert(true);
                    *entry = *entry && all;
                }
            }
        }
    }

    let mut placed: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for page in pages.iter_mut() {
        let mut rebuilt: Vec<Block> = Vec::new();
        for mut block in std::mem::take(&mut page.blocks) {
            if block.image.is_some() {
                rebuilt.push(block);
                continue;
            }

            // Inline first, in place: the line keeps its shape and its
            // neighbours, and only the run of words the region speaks for is
            // replaced by a carrier that `flatten` writes the region at.
            for line in block.lines.iter_mut() {
                splice_inline(line, &whole_line, &mut placed, diagnostics);
            }

            // Consecutive lines of one block region are one run. A region that
            // spans two of the page's blocks is still one image, so its block
            // is written where its first line was and the rest simply go.
            let mut pending: Vec<Line> = Vec::new();
            let flush = |pending: &mut Vec<Line>, rebuilt: &mut Vec<Block>| {
                if !pending.is_empty() {
                    rebuilt.push(Block {
                        lines: std::mem::take(pending),
                        image: None,
                    });
                }
            };
            for line in std::mem::take(&mut block.lines) {
                let owner = line
                    .words
                    .iter()
                    .find_map(|word| word.typeset)
                    .filter(|index| whole_line.get(index).copied().unwrap_or(false));
                match owner {
                    Some(index) => {
                        flush(&mut pending, &mut rebuilt);
                        if placed.insert(index) {
                            diagnostics.typeset_regions_superseded_native_text += 1;
                            rebuilt.push(Block {
                                lines: Vec::new(),
                                image: Some(index),
                            });
                        }
                    }
                    None => pending.push(line),
                }
            }
            flush(&mut pending, &mut rebuilt);
        }
        page.blocks = rebuilt;
    }
}

/// One word standing alone on a line of its own.
fn line_of(word: Word) -> Line {
    let mut line = Line::default();
    line.push_word(word);
    line
}

/// Replace each run of words an inline region owns with a single carrier.
///
/// The carrier is the run's first word, emptied of its text and keeping the
/// region's mark; `flatten` writes the region's reading at it. The whitespace
/// the page left after the *last* word of the run is kept, so the sentence
/// reads with the spacing it was set with rather than the spacing of whichever
/// word happened to come first.
fn splice_inline(
    line: &mut Line,
    whole_line: &std::collections::HashMap<usize, bool>,
    placed: &mut std::collections::HashSet<usize>,
    diagnostics: &mut ExtractionDiagnostics,
) {
    let inline = |word: &Word| {
        word.typeset
            .filter(|index| !whole_line.get(index).copied().unwrap_or(false))
    };
    if !line.words.iter().any(|word| inline(word).is_some()) {
        return;
    }

    let mut words: Vec<Word> = Vec::with_capacity(line.words.len());
    let mut trailing: Vec<String> = Vec::with_capacity(line.trailing.len());
    let mut at = 0usize;
    while at < line.words.len() {
        let Some(index) = inline(&line.words[at]) else {
            words.push(line.words[at].clone());
            trailing.push(line.trailing[at].clone());
            at += 1;
            continue;
        };
        let mut end = at;
        while end + 1 < line.words.len() && inline(&line.words[end + 1]) == Some(index) {
            end += 1;
        }
        let mut carrier = line.words[at].clone();
        // The hull of the run, so a reading surface resolving these bytes
        // reaches the whole expression and not its first token.
        for word in &line.words[at..=end] {
            carrier.bbox = match (carrier.bbox.take(), word.bbox.as_ref()) {
                (Some(hull), Some(next)) => Some(hull.merge(next)),
                (hull, next) => hull.or_else(|| next.cloned()),
            };
        }
        carrier.text = String::new();
        if placed.insert(index) {
            diagnostics.typeset_regions_superseded_native_text += 1;
        } else {
            // A region already written elsewhere leaves nothing here: two
            // carriers would put one expression into the reading twice.
            carrier.typeset = None;
        }
        words.push(carrier);
        trailing.push(line.trailing[end].clone());
        at = end + 1;
    }
    line.words = words;
    line.trailing = trailing;
}

// ── Class 2: page furniture ─────────────────────────────────────────────────

/// Drop the runs that exist because the page is a page: bare page numbers,
/// running heads, running feet.
///
/// Structural, not textual: a candidate is a short single-line run in a margin
/// band, and it is furniture only if that band carries a candidate on a
/// majority of the document's pages. One short line in the margin of one page
/// is left alone — it has no repetition to convict it. A running head equal to
/// a section title still goes: it is not prose, and the outline already
/// carries the title.
fn remove_page_furniture(pages: &mut [Page], diagnostics: &mut ExtractionDiagnostics) {
    if pages.len() < FURNITURE_MIN_PAGES {
        return;
    }

    let mut bands: std::collections::HashMap<i32, std::collections::HashSet<u32>> =
        std::collections::HashMap::new();
    for page in pages.iter() {
        for block in &page.blocks {
            if let Some(band) = furniture_band(page, block) {
                bands.entry(band).or_default().insert(page.number);
            }
        }
    }

    let majority = pages.len() / 2;
    let furniture: std::collections::HashSet<i32> = bands
        .into_iter()
        .filter(|(_, seen)| seen.len() > majority)
        .map(|(band, _)| band)
        .collect();
    if furniture.is_empty() {
        return;
    }

    for page in pages.iter_mut() {
        let keep: Vec<bool> = page
            .blocks
            .iter()
            .map(|block| !furniture_band(page, block).is_some_and(|band| furniture.contains(&band)))
            .collect();
        let mut index = 0;
        page.blocks.retain(|_| {
            let keep = keep[index];
            index += 1;
            keep
        });
        diagnostics.removed_furniture_runs += keep.iter().filter(|keep| !**keep).count() as u32;
    }
}

/// The band bucket a block would repeat in, or `None` if it is not a
/// furniture candidate at all. Top and bottom bands cannot collide: the bucket
/// is derived from the absolute vertical position, and a page is never
/// `FURNITURE_BAND` tall.
fn furniture_band(page: &Page, block: &Block) -> Option<i32> {
    if page.height <= 0.0 || block.word_count() > FURNITURE_MAX_WORDS {
        return None;
    }
    let extent = block.extent()?;
    let centre = (extent.y0 + extent.y1) / 2.0;
    let band = page.height * FURNITURE_BAND;
    if centre > band && centre < page.height - band {
        return None;
    }
    Some((centre / FURNITURE_BAND_BUCKET).round() as i32)
}

// ── Class 3: marginalia ─────────────────────────────────────────────────────

/// Move each page's out-of-column blocks after its last body block, and report
/// the flow each block ends up in.
///
/// Moved, never deleted: a margin glossary box is authored content — these
/// documents define half their key terms there — and its segments survive the
/// move intact, so its bytes still point at the box and a preview still
/// highlights it. What changes is only where it sits in the reading, which is
/// after the page it annotates rather than inside the sentence it interrupts.
///
/// A page whose words do not cluster into one dominant column is left exactly
/// as the page ordered it. That is the honest answer for a table, a two-column
/// spread or a full-page figure, and it is counted so that a document full of
/// them is visible as one.
fn relocate_marginalia(
    pages: &mut [Page],
    diagnostics: &mut ExtractionDiagnostics,
) -> Vec<Vec<Flow>> {
    let mut next_flow: Flow = BODY_FLOW + 1;
    let mut flows = Vec::with_capacity(pages.len());

    for page in pages.iter_mut() {
        let extents: Vec<Option<Extent>> = page.blocks.iter().map(Block::extent).collect();
        let words: Vec<usize> = page.blocks.iter().map(Block::word_count).collect();
        let total_words: usize = words.iter().sum();

        let Some(body) = body_column(&extents, &words, total_words) else {
            if total_words > 0 {
                diagnostics.ambiguous_column_pages += 1;
            }
            flows.push(vec![BODY_FLOW; page.blocks.len()]);
            continue;
        };
        diagnostics.body_column_pages += 1;

        let in_body: Vec<bool> = extents
            .iter()
            .map(|extent| match extent {
                // A block with no boxes at all cannot be placed, so it stays
                // where the page put it rather than being relocated on no
                // evidence.
                None => true,
                Some(extent) => extent.horizontal_overlap_ratio(&body) >= COLUMN_OVERLAP,
            })
            .collect();

        if in_body.iter().all(|body| *body) {
            flows.push(vec![BODY_FLOW; page.blocks.len()]);
            continue;
        }

        // The last block of *text* in the body column. Read from the extents
        // rather than from `in_body`, because a block with no extent is in
        // the body only in the sense that it cannot be placed — a native
        // image at the foot of a page is one, and letting it stand as the
        // last body block would relocate an aside that sits above it and was
        // previously left alone.
        let last_body = in_body
            .iter()
            .zip(&extents)
            .rposition(|(body, extent)| *body && extent.is_some());
        let mut reordered = Vec::with_capacity(page.blocks.len());
        let mut page_flows = Vec::with_capacity(page.blocks.len());
        let mut asides = Vec::new();
        for (index, block) in std::mem::take(&mut page.blocks).into_iter().enumerate() {
            // An aside after the last body block is already out of the way;
            // moving it would be a no-op with a diagnostic attached.
            if in_body[index] || last_body.is_some_and(|last| index > last) {
                reordered.push(block);
                page_flows.push(BODY_FLOW);
            } else {
                diagnostics.relocated_marginalia_blocks += 1;
                asides.push(block);
            }
        }
        for aside in asides {
            reordered.push(aside);
            page_flows.push(next_flow);
            next_flow += 1;
        }
        page.blocks = reordered;
        flows.push(page_flows);
    }

    flows
}

/// The horizontal span of the page's body column, or `None` when the page's
/// words do not concentrate in one.
fn body_column(extents: &[Option<Extent>], words: &[usize], total_words: usize) -> Option<Extent> {
    if total_words == 0 {
        return None;
    }

    // Clusters of blocks that share a column, merged transitively: a block
    // overlapping two clusters joins them, because a heading spanning the body
    // and the margin belongs to neither on its own.
    let mut clusters: Vec<(Extent, usize)> = Vec::new();
    for (index, extent) in extents.iter().enumerate() {
        let Some(extent) = extent else { continue };
        let mut merged = *extent;
        let mut count = words[index];
        let mut remaining = Vec::with_capacity(clusters.len());
        for (cluster, cluster_words) in clusters.drain(..) {
            if cluster.horizontal_overlap_ratio(&merged) >= COLUMN_OVERLAP {
                merged = merged.merge(&cluster);
                count += cluster_words;
            } else {
                remaining.push((cluster, cluster_words));
            }
        }
        remaining.push((merged, count));
        clusters = remaining;
    }

    let (dominant, dominant_words) = clusters
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .filter(|(_, count)| *count > 0)?;
    ((dominant_words as f32) >= (total_words as f32) * BODY_COLUMN_SHARE).then_some(dominant)
}

// ── Class 1: line-wrap hyphenation ──────────────────────────────────────────

#[derive(Clone, Debug)]
enum Item {
    Word {
        text: String,
        bbox: Option<BoundingBox>,
        page: u32,
        flow: Flow,
    },
    Space(char),
    /// End of a visual line: renders as a newline.
    LineEnd,
    /// End of a page: renders as a newline unless one was just written.
    PageEnd,
    /// A native image, at the position the page drew it. Renders as its
    /// enrichment block, or as nothing when analysis established nothing.
    Image(usize),
    /// An area the page typeset *inside* a line — an expression in the middle
    /// of a sentence. Renders as the region's own reading and nothing else:
    /// no label, no line of its own, no terminator, because it is not a block
    /// standing beside the prose but a run of words within it.
    InlineImage(usize),
}

fn flatten(pages: &[Page], flows: &[Vec<Flow>]) -> Vec<Item> {
    let mut items = Vec::new();
    for (page, page_flows) in pages.iter().zip(flows) {
        for (block, flow) in page.blocks.iter().zip(page_flows) {
            if let Some(image) = block.image {
                items.push(Item::Image(image));
                continue;
            }
            for line in &block.lines {
                for c in line.leading.chars() {
                    items.push(Item::Space(c));
                }
                for (word, trailing) in line.words.iter().zip(&line.trailing) {
                    match word.typeset {
                        // A mark that survived `supersede_typeset_regions` is
                        // an admitted region on a carrier word, and the
                        // carrier's own text was emptied when it was made.
                        Some(index) => items.push(Item::InlineImage(index)),
                        None => items.push(Item::Word {
                            text: word.text.clone(),
                            bbox: word.bbox.clone(),
                            page: page.number,
                            flow: *flow,
                        }),
                    }
                    for c in trailing.chars() {
                        items.push(Item::Space(c));
                    }
                }
                items.push(Item::LineEnd);
            }
        }
        items.push(Item::PageEnd);
    }
    items
}

/// Join words the typesetter broke across a line.
///
/// The hyphen goes only on the document's own evidence: if the joined form
/// occurs elsewhere in this document as an unhyphenated word, the break was
/// hyphenation and the hyphen is dropped; otherwise it was a real compound and
/// the hyphen stays. No dictionary, no language assumption, self-calibrating
/// on the document's vocabulary, and deterministic — the rendition hash stays
/// a function of the document.
///
/// The *line break* goes either way. It is layout in both cases: a compound
/// broken at its own hyphen reads `pre-shared`, never `pre-\nshared`. So the
/// failure mode of a missed join is a hyphen left in the middle of a word, not
/// a word left split across two lines — which is what makes the "no
/// hyphen-broken words" property checkable at all.
fn join_wrapped_words(items: &mut Vec<Item>, diagnostics: &mut ExtractionDiagnostics) {
    let vocabulary = unhyphenated_vocabulary(items);
    let mut removals: Vec<usize> = Vec::new();
    let mut joins: Vec<usize> = Vec::new();

    let mut index = 0;
    while index < items.len() {
        let Item::Word { text, flow, .. } = &items[index] else {
            index += 1;
            continue;
        };
        let Some(stem) = wrap_hyphen_stem(text) else {
            index += 1;
            continue;
        };
        let flow = *flow;

        // The continuation is the next word, provided nothing but the line
        // break and its whitespace separates them and it belongs to the same
        // flow.
        let mut lookahead = index + 1;
        let mut crossed_line_end = false;
        let continuation = loop {
            match items.get(lookahead) {
                Some(Item::Space(c)) if c.is_whitespace() => lookahead += 1,
                Some(Item::LineEnd) | Some(Item::PageEnd) => {
                    crossed_line_end = true;
                    lookahead += 1;
                }
                Some(Item::Word {
                    text,
                    flow: next_flow,
                    ..
                }) if crossed_line_end && text.chars().next().is_some_and(char::is_alphabetic) => {
                    break (*next_flow == flow).then(|| (lookahead, text.clone()))
                }
                _ => break None,
            }
        };
        let Some((continuation_index, continuation_text)) = continuation else {
            // A page that ends mid-word and also carries marginalia puts the
            // relocated box between the two halves. The word stays broken:
            // joining across that seam would have to delete the box, and the
            // box is authored content. Rare, and counted rather than hidden —
            // this is the one thing that can leave a hyphen at a line end.
            if crossed_line_end {
                diagnostics.unjoinable_wrap_breaks += 1;
            }
            index += 1;
            continue;
        };

        if vocabulary.contains(&normalize_word(&format!("{stem}{continuation_text}"))) {
            diagnostics.joined_wrap_hyphens += 1;
            joins.push(index);
        } else {
            diagnostics.kept_wrap_hyphens += 1;
        }
        removals.extend((index + 1)..continuation_index);
        index = continuation_index;
    }

    for index in joins {
        if let Item::Word { text, .. } = &mut items[index] {
            if let Some(stem) = wrap_hyphen_stem(text) {
                *text = stem;
            }
        }
    }

    let removed: std::collections::HashSet<usize> = removals.into_iter().collect();
    if removed.is_empty() {
        return;
    }
    let mut index = 0;
    items.retain(|_| {
        let keep = !removed.contains(&index);
        index += 1;
        keep
    });
}

/// The word without its trailing wrap hyphen, when it ends in one.
fn wrap_hyphen_stem(text: &str) -> Option<String> {
    let mut chars = text.chars().rev();
    let last = chars.next()?;
    if !is_discretionary_hyphen(last) {
        return None;
    }
    // `3-` or `--` is not a word broken across a line.
    if !chars.next()?.is_alphabetic() {
        return None;
    }
    Some(text[..text.len() - last.len_utf8()].to_string())
}

fn is_discretionary_hyphen(c: char) -> bool {
    matches!(c, '-' | '\u{00ad}' | '\u{2010}' | '\u{2011}')
}

/// Every word the document writes without a hyphen in it, folded for
/// comparison. Words that only ever appear hyphenated are absent on purpose:
/// they are the evidence *for* keeping a hyphen, not against it.
fn unhyphenated_vocabulary(items: &[Item]) -> std::collections::HashSet<String> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Word { text, .. } => Some(normalize_word(text)),
            _ => None,
        })
        .filter(|word| !word.is_empty() && !word.contains('-'))
        .collect()
}

/// Case-folded, with the punctuation a word carries at either end removed —
/// the comparison form for "does this document write this word".
fn normalize_word(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

// ── Rendering ───────────────────────────────────────────────────────────────

/// Write the item list out as text plus one segment per word.
///
/// This is the only place the text is built, which is what keeps the map
/// exact: a segment's range is the range the word was actually written at, not
/// a range computed alongside it.
fn render(items: &[Item], images: &mut [ExtractedImage]) -> Reading {
    let mut text = String::new();
    let mut segments = Vec::new();
    for item in items {
        match item {
            Item::Word {
                text: word,
                bbox,
                page,
                ..
            } => {
                if word.is_empty() {
                    continue;
                }
                let start = text.len();
                text.push_str(word);
                segments.push(SourceSegment {
                    text_range: ByteRange {
                        start,
                        end: text.len(),
                    },
                    origin: SourceOrigin::PdfPage {
                        page: *page,
                        bbox: bbox.clone(),
                    },
                    provenance: TextProvenance::Native,
                });
            }
            Item::Space(c) => text.push(*c),
            Item::LineEnd => text.push('\n'),
            Item::PageEnd => {
                if !text.ends_with('\n') {
                    text.push('\n');
                }
            }
            Item::InlineImage(index) => {
                let Some(image) = images.get(*index) else {
                    continue;
                };
                let pieces = serialize::inline_pieces(image);
                if pieces.is_empty() {
                    continue;
                }
                let start = text.len();
                for piece in pieces {
                    let piece_start = text.len();
                    text.push_str(&piece.text);
                    segments.push(SourceSegment {
                        text_range: ByteRange {
                            start: piece_start,
                            end: text.len(),
                        },
                        origin: SourceOrigin::PdfPage {
                            page: image.page,
                            bbox: Some(piece.bbox),
                        },
                        provenance: piece.provenance,
                    });
                }
                images[*index].reading_range = Some(ByteRange {
                    start,
                    end: text.len(),
                });
            }
            Item::Image(index) => {
                let Some(image) = images.get(*index) else {
                    continue;
                };
                // Where the page drew this picture, in the reading being
                // built. Recorded for every image before anything decides
                // whether there are bytes to write for it: an image nothing
                // was established about is still *somewhere*, and this loop is
                // the only place that knows where. Taken before the newline
                // below, so an analyzed image anchors at the end of the prose
                // it interrupts and its block occupies the bytes after.
                let anchor = text.len();
                let pieces = serialize::enrichment_pieces(image);
                let page = image.page;
                images[*index].reading_anchor = Some(anchor);
                if pieces.is_empty() {
                    continue;
                }
                // The block starts its own line. Whitespace only, so it falls
                // in the gap between segments exactly as the space between
                // two words does.
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                let start = text.len();
                for piece in pieces {
                    let piece_start = text.len();
                    text.push_str(&piece.text);
                    segments.push(SourceSegment {
                        text_range: ByteRange {
                            start: piece_start,
                            end: text.len(),
                        },
                        origin: SourceOrigin::PdfPage {
                            page,
                            bbox: Some(piece.bbox),
                        },
                        provenance: piece.provenance,
                    });
                }
                images[*index].reading_range = Some(ByteRange {
                    start,
                    end: text.len(),
                });
            }
        }
    }
    Reading { text, segments }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, x: f32, y: f32) -> Word {
        Word {
            text: text.to_string(),
            bbox: Some(BoundingBox {
                x,
                y,
                width: 6.0 * text.chars().count() as f32,
                height: 10.0,
            }),
            typeset: None,
        }
    }

    fn line(words: Vec<Word>) -> Line {
        let mut line = Line::default();
        let last = words.len().saturating_sub(1);
        for (index, w) in words.into_iter().enumerate() {
            line.push_word(w);
            if index < last {
                line.push_space(' ');
            }
        }
        line
    }

    /// One page, body words at x=100, at the given vertical positions.
    fn body_page(number: u32, lines: Vec<Line>) -> Page {
        Page {
            number,
            height: 800.0,
            blocks: vec![Block { lines, image: None }],
        }
    }

    fn read(pages: Vec<Page>) -> (String, ExtractionDiagnostics) {
        let mut diagnostics = ExtractionDiagnostics::default();
        let reading = sanitize(pages, &mut [], &[], &mut diagnostics);
        (reading.text, diagnostics)
    }

    fn bbox_at(reading: &Reading, needle: &str) -> BoundingBox {
        let start = reading.text.find(needle).expect("word is in the reading");
        let segment = reading
            .segments
            .iter()
            .find(|segment| segment.text_range.start == start)
            .expect("word has a segment");
        match &segment.origin {
            SourceOrigin::PdfPage { bbox, .. } => bbox.clone().expect("word has a box"),
            other => panic!("expected a page origin, got {other:?}"),
        }
    }

    // ── Typeset regions ──────────────────────────────────────────────────

    /// One recognized typeset region, admitted or refused.
    fn recognized(admitted: bool) -> ExtractedImage {
        use crate::types::{
            ImageAnalysisStatus, ImageOcrRegion, ImageTransform, OcrAdmission, Point, RegionKind,
            RegionOrigin,
        };
        ExtractedImage {
            id: "p1-v0".into(),
            page: 1,
            origin: RegionOrigin::Typeset,
            bbox: BoundingBox {
                x: 90.0,
                y: 90.0,
                width: 200.0,
                height: 30.0,
            },
            transform: ImageTransform {
                a: 200.0,
                b: 0.0,
                c: 0.0,
                d: 30.0,
                e: 90.0,
                f: 90.0,
            },
            pixel_width: 800,
            pixel_height: 120,
            image_sha256: "digest".into(),
            reading_range: None,
            reading_anchor: None,
            ocr_regions: vec![ImageOcrRegion {
                kind: RegionKind::Formula,
                text: "c_i = a_i \\oplus b_i".into(),
                confidence: 0.95,
                polygon_within_image: vec![Point { x: 0.0, y: 0.0 }],
                page_polygon: vec![Point { x: 90.0, y: 90.0 }],
                admission: if admitted {
                    OcrAdmission::Accepted
                } else {
                    OcrAdmission::RejectedInvalidLatex
                },
            }],
            description: None,
            analyzer_identity: "test".into(),
            status: ImageAnalysisStatus::Complete,
        }
    }

    /// A paragraph with a display formula in the middle of it, marked as one
    /// region.
    fn page_with_a_formula_in_the_middle() -> Page {
        let mut lines = vec![
            line(vec![word("prose", 100.0, 80.0), word("above", 140.0, 80.0)]),
            line(vec![
                word("ci", 100.0, 100.0),
                word("=", 120.0, 100.0),
                word("ai", 140.0, 100.0),
            ]),
            line(vec![
                word("prose", 100.0, 120.0),
                word("below", 140.0, 120.0),
            ]),
        ];
        for word in lines[1].words.iter_mut() {
            word.typeset = Some(0);
        }
        Page {
            number: 1,
            height: 800.0,
            blocks: vec![Block { lines, image: None }],
        }
    }

    /// The recognizer's reading takes the glyph run's place, and the prose
    /// around it stays in its own blocks on either side. One owner for those
    /// bytes: the flattened run is gone, not sitting beside its transcription.
    #[test]
    fn an_admitted_region_replaces_the_lines_it_speaks_for() {
        let mut images = [recognized(true)];
        let mut diagnostics = ExtractionDiagnostics::default();
        let reading = sanitize(
            vec![page_with_a_formula_in_the_middle()],
            &mut images,
            &[],
            &mut diagnostics,
        );

        assert!(reading.text.contains("prose above"), "{:?}", reading.text);
        assert!(reading.text.contains("prose below"), "{:?}", reading.text);
        assert!(
            reading
                .text
                .contains("Page formula: c_i = a_i \\oplus b_i."),
            "{:?}",
            reading.text
        );
        assert!(!reading.text.contains("ci = ai"), "{:?}", reading.text);
        assert_eq!(diagnostics.typeset_regions_superseded_native_text, 1);
        assert!(images[0].reading_range.is_some(), "the block has a range");

        // The order is the page's: the formula is between the two paragraphs
        // it was drawn between, not appended after them.
        let above = reading.text.find("prose above").expect("above");
        let formula = reading.text.find("Page formula:").expect("formula");
        let below = reading.text.find("prose below").expect("below");
        assert!(above < formula && formula < below, "{:?}", reading.text);
    }

    /// An expression inside a sentence is spliced into it, not lifted out of
    /// it. The sentence keeps its words and its spacing and gains the
    /// recognizer's LaTeX where the page drew the expression — writing it as a
    /// block would take the sentence apart to insert its own middle.
    #[test]
    fn an_inline_region_is_spliced_into_the_sentence() {
        let mut line = line(vec![
            word("with", 100.0, 200.0),
            word("√n", 130.0, 200.0),
            word("∈", 150.0, 200.0),
            word("ℕ", 165.0, 200.0),
            word("Thus", 185.0, 200.0),
        ]);
        for index in 1..=3 {
            line.words[index].typeset = Some(0);
        }

        let mut images = [recognized(true)];
        let mut diagnostics = ExtractionDiagnostics::default();
        let reading = sanitize(
            vec![body_page(1, vec![line])],
            &mut images,
            &[],
            &mut diagnostics,
        );

        assert_eq!(
            reading.text.trim(),
            "with c_i = a_i \\oplus b_i Thus",
            "{:?}",
            reading.text
        );
        assert!(
            !reading.text.contains("Page formula:"),
            "no label inside a sentence: {:?}",
            reading.text
        );
        assert_eq!(diagnostics.typeset_regions_superseded_native_text, 1);
        assert!(images[0].reading_range.is_some());
    }

    // ── Regions that replace nothing ─────────────────────────────────────
    //
    // The page drew the expression as paths, so the text layer holds no word
    // for the region to be marked on. One is made for it, at the place
    // `typeset::anchor_after` read off the page, and from there the reading
    // treats it as it treats any other marked word.

    fn after(block: usize, line: usize, word: usize, inline: bool) -> Anchor {
        Anchor {
            after: Some((block, line, word)),
            inline,
        }
    }

    /// An expression drawn between two words of a sentence is read between
    /// them, with a space either side — not appended to the page, and not
    /// given a line of its own inside the sentence.
    #[test]
    fn a_region_over_no_glyphs_lands_between_the_words_it_was_drawn_between() {
        let mut images = [recognized(true)];
        let mut diagnostics = ExtractionDiagnostics::default();
        let sentence = line(vec![word("with", 100.0, 200.0), word("Thus", 185.0, 200.0)]);
        let reading = sanitize(
            vec![body_page(1, vec![sentence])],
            &mut images,
            &[(0, after(0, 0, 0, true))],
            &mut diagnostics,
        );

        assert_eq!(
            reading.text.trim(),
            "with c_i = a_i \\oplus b_i Thus",
            "{:?}",
            reading.text
        );
        assert!(
            !reading.text.contains("Page formula:"),
            "no label inside a sentence: {:?}",
            reading.text
        );
        assert_eq!(diagnostics.typeset_regions_superseded_native_text, 1);
        assert!(images[0].reading_range.is_some());
    }

    /// Two drawn side by side after one word are read left to right, whatever
    /// height the page set them at. A tall expression beside a short one sits
    /// higher on the page and is still the second thing read.
    #[test]
    fn two_regions_over_no_glyphs_after_one_word_are_read_left_to_right() {
        let at = |latex: &str, x: f32, y: f32| {
            let mut image = recognized(true);
            image.ocr_regions[0].text = latex.to_string();
            image.bbox = BoundingBox {
                x,
                y,
                width: 20.0,
                height: 10.0,
            };
            image
        };
        // The left one drawn lower, so ordering by height alone reverses them.
        let mut images = [at("x_{L}", 120.0, 205.0), at("x_{R}", 200.0, 190.0)];
        let mut diagnostics = ExtractionDiagnostics::default();
        let sentence = line(vec![word("with", 100.0, 200.0), word("Thus", 185.0, 200.0)]);
        let reading = sanitize(
            vec![body_page(1, vec![sentence])],
            &mut images,
            &[(0, after(0, 0, 0, true)), (1, after(0, 0, 0, true))],
            &mut diagnostics,
        );

        let left = reading.text.find("x_{L}").expect("the left expression");
        let right = reading.text.find("x_{R}").expect("the right expression");
        assert!(left < right, "{:?}", reading.text);
        assert_eq!(diagnostics.typeset_regions_superseded_native_text, 2);
    }

    /// One drawn below a paragraph takes a line of its own after that
    /// paragraph's last word, which is what a display expression is.
    #[test]
    fn a_region_over_no_glyphs_below_a_paragraph_lands_after_its_last_word() {
        let mut images = [recognized(true)];
        let mut diagnostics = ExtractionDiagnostics::default();
        let paragraph = line(vec![word("prose", 100.0, 80.0), word("above", 140.0, 80.0)]);
        let reading = sanitize(
            vec![body_page(1, vec![paragraph])],
            &mut images,
            &[(0, after(0, 0, 1, false))],
            &mut diagnostics,
        );

        let prose = reading.text.find("prose above").expect("the paragraph");
        let formula = reading
            .text
            .find("Page formula: c_i = a_i \\oplus b_i.")
            .unwrap_or_else(|| panic!("the formula: {:?}", reading.text));
        assert!(prose < formula, "{:?}", reading.text);
        assert_eq!(diagnostics.typeset_regions_superseded_native_text, 1);
    }

    /// A page whose text layer holds nothing at all — every word of it drawn
    /// rather than set — reads its regions from the head of the page.
    #[test]
    fn a_region_on_a_page_with_no_words_lands_at_its_start() {
        let mut images = [recognized(true)];
        let mut diagnostics = ExtractionDiagnostics::default();
        let page = Page {
            number: 1,
            height: 800.0,
            blocks: Vec::new(),
        };
        let reading = sanitize(
            vec![page],
            &mut images,
            &[(
                0,
                Anchor {
                    after: None,
                    inline: false,
                },
            )],
            &mut diagnostics,
        );

        assert_eq!(
            reading.text.trim(),
            "Page formula: c_i = a_i \\oplus b_i.",
            "{:?}",
            reading.text
        );
        assert!(images[0].reading_range.is_some());
    }

    /// A refused one inserts nothing at all. The page had no glyphs there to
    /// keep, so "the reading is what it was" means byte for byte what it was.
    #[test]
    fn a_refused_region_over_no_glyphs_inserts_nothing() {
        let mut images = [recognized(false)];
        let mut diagnostics = ExtractionDiagnostics::default();
        let sentence = line(vec![word("with", 100.0, 200.0), word("Thus", 185.0, 200.0)]);
        let reading = sanitize(
            vec![body_page(1, vec![sentence.clone()])],
            &mut images,
            &[(0, after(0, 0, 0, true))],
            &mut diagnostics,
        );

        let untouched = sanitize(
            vec![body_page(1, vec![sentence])],
            &mut [],
            &[],
            &mut ExtractionDiagnostics::default(),
        );
        assert_eq!(reading.text, untouched.text, "{:?}", reading.text);
        assert_eq!(diagnostics.typeset_regions_superseded_native_text, 0);
        assert!(images[0].reading_range.is_none());
    }

    /// The bytes an inline region wrote resolve to the whole expression, not
    /// to its first token — a reading surface substitutes what it is given.
    #[test]
    fn an_inline_region_resolves_to_the_glyphs_it_replaced() {
        use crate::extract::image::serialize::{reading_regions, superseded_areas};

        let mut line = line(vec![
            word("with", 100.0, 200.0),
            word("√n", 130.0, 200.0),
            word("∈", 150.0, 200.0),
            word("Thus", 185.0, 200.0),
        ]);
        line.words[1].typeset = Some(0);
        line.words[2].typeset = Some(0);

        let mut images = [recognized(true)];
        let mut diagnostics = ExtractionDiagnostics::default();
        let reading = sanitize(
            vec![body_page(1, vec![line])],
            &mut images,
            &[],
            &mut diagnostics,
        );
        let content = crate::types::ExtractedContent {
            text: reading.text.clone(),
            source_map: crate::types::SourceMap {
                segments: reading.segments.clone(),
            },
            metadata: crate::types::FileMetadata {
                path: "doc.pdf".into(),
                size_bytes: 0,
                mime: None,
                title: None,
                page_count: None,
            },
            images: images.to_vec(),
        };

        let areas = superseded_areas(&content.text, &reading_regions(&content));
        assert_eq!(areas.len(), 1, "{areas:?}");
        assert_eq!(areas[0].text, "c_i = a_i \\oplus b_i");
    }

    /// A region the recognizer had no admissible answer for changes nothing.
    /// That is what makes a wrongly marked-out region cost time and no bytes.
    #[test]
    fn a_refused_region_leaves_its_lines_exactly_where_they_were() {
        let mut images = [recognized(false)];
        let mut diagnostics = ExtractionDiagnostics::default();
        let reading = sanitize(
            vec![page_with_a_formula_in_the_middle()],
            &mut images,
            &[],
            &mut diagnostics,
        );

        assert!(reading.text.contains("ci = ai"), "{:?}", reading.text);
        assert!(!reading.text.contains("Page formula:"));
        assert_eq!(diagnostics.typeset_regions_superseded_native_text, 0);
        assert!(images[0].reading_range.is_none());
    }

    /// A region whose lines fall in two of the page's blocks is still one
    /// image: its block is written where its first line was, and the rest of
    /// its lines simply go.
    #[test]
    fn a_region_spanning_two_blocks_is_written_once() {
        let mut first = line(vec![word("ci", 100.0, 100.0), word("=", 120.0, 100.0)]);
        for word in first.words.iter_mut() {
            word.typeset = Some(0);
        }
        let mut second = line(vec![word("ai", 100.0, 116.0), word("+", 120.0, 116.0)]);
        for word in second.words.iter_mut() {
            word.typeset = Some(0);
        }
        let page = Page {
            number: 1,
            height: 800.0,
            blocks: vec![
                Block {
                    lines: vec![first],
                    image: None,
                },
                Block {
                    lines: vec![second],
                    image: None,
                },
            ],
        };

        let mut images = [recognized(true)];
        let mut diagnostics = ExtractionDiagnostics::default();
        let reading = sanitize(vec![page], &mut images, &[], &mut diagnostics);

        assert_eq!(
            reading.text.matches("Page formula:").count(),
            1,
            "{:?}",
            reading.text
        );
        assert_eq!(diagnostics.typeset_regions_superseded_native_text, 1);
    }

    #[test]
    fn a_word_broken_across_a_line_is_joined_when_the_document_writes_it_whole() {
        let pages = vec![body_page(
            1,
            vec![
                line(vec![word("protect", 100.0, 100.0)]),
                line(vec![word("do", 100.0, 120.0), word("not", 130.0, 120.0)]),
                line(vec![word("pro-", 100.0, 140.0)]),
                line(vec![word("tect", 100.0, 160.0), word("data", 130.0, 160.0)]),
            ],
        )];
        let (text, diagnostics) = read(pages);

        assert!(text.contains("protect data"), "{text:?}");
        assert!(!text.contains("pro-"), "{text:?}");
        assert_eq!(diagnostics.joined_wrap_hyphens, 1);
        assert_eq!(diagnostics.kept_wrap_hyphens, 0);
    }

    #[test]
    fn a_compound_the_document_never_writes_whole_keeps_its_hyphen() {
        let pages = vec![body_page(
            1,
            vec![
                line(vec![word("a", 100.0, 100.0), word("pre-", 110.0, 100.0)]),
                line(vec![
                    word("shared", 100.0, 120.0),
                    word("key", 140.0, 120.0),
                ]),
            ],
        )];
        let (text, diagnostics) = read(pages);

        assert!(text.contains("pre-shared key"), "{text:?}");
        assert_eq!(diagnostics.kept_wrap_hyphens, 1);
        assert_eq!(diagnostics.joined_wrap_hyphens, 0);
    }

    /// Whether the break is joined or not, no reading keeps a word split
    /// across two lines by a hyphen — that is the property §9 measures.
    #[test]
    fn no_reading_keeps_a_hyphen_at_the_end_of_a_line() {
        let pages = vec![body_page(
            1,
            vec![
                line(vec![word("multi-", 100.0, 100.0)]),
                line(vec![word("factor", 100.0, 120.0)]),
                line(vec![word("exam-", 100.0, 140.0)]),
                line(vec![
                    word("ple", 100.0, 160.0),
                    word("example", 130.0, 160.0),
                ]),
            ],
        )];
        let (text, _) = read(pages);

        assert!(!text.contains("-\n"), "{text:?}");
        assert!(text.contains("multi-factor"), "{text:?}");
        assert!(text.contains("example example"), "{text:?}");
    }

    /// A trailing hyphen that is not a broken word — a number, a dash — is
    /// left exactly where it is, line break included.
    #[test]
    fn a_trailing_hyphen_that_is_not_a_broken_word_is_untouched() {
        let pages = vec![body_page(
            1,
            vec![
                line(vec![word("3-", 100.0, 100.0)]),
                line(vec![word("fold", 100.0, 120.0)]),
            ],
        )];
        let (text, diagnostics) = read(pages);

        assert!(text.contains("3-\nfold"), "{text:?}");
        assert_eq!(diagnostics.joined_wrap_hyphens, 0);
        assert_eq!(diagnostics.kept_wrap_hyphens, 0);
    }

    #[test]
    fn a_page_number_repeating_in_the_same_band_is_removed() {
        let pages = (1..=6)
            .map(|number| Page {
                number,
                height: 800.0,
                blocks: vec![
                    Block {
                        lines: vec![line(vec![word("body", 100.0, 300.0)])],
                        image: None,
                    },
                    Block {
                        lines: vec![line(vec![word(&format!("{}", 127 + number), 100.0, 760.0)])],
                        image: None,
                    },
                ],
            })
            .collect();
        let (text, diagnostics) = read(pages);

        assert!(!text.contains("128"), "{text:?}");
        assert!(text.contains("body"), "{text:?}");
        assert_eq!(diagnostics.removed_furniture_runs, 6);
    }

    #[test]
    fn a_short_line_in_one_pages_margin_is_left_alone() {
        let mut pages: Vec<Page> = (1..=6)
            .map(|number| Page {
                number,
                height: 800.0,
                blocks: vec![Block {
                    lines: vec![line(vec![word("body", 100.0, 300.0)])],
                    image: None,
                }],
            })
            .collect();
        pages[2].blocks.push(Block {
            lines: vec![line(vec![word("once", 100.0, 760.0)])],
            image: None,
        });
        let (text, diagnostics) = read(pages);

        assert!(text.contains("once"), "{text:?}");
        assert_eq!(diagnostics.removed_furniture_runs, 0);
    }

    #[test]
    fn a_short_document_has_no_repeating_bands_to_convict() {
        let pages = (1..=3)
            .map(|number| Page {
                number,
                height: 800.0,
                blocks: vec![Block {
                    lines: vec![line(vec![word("7", 100.0, 760.0)])],
                    image: None,
                }],
            })
            .collect();
        let (text, diagnostics) = read(pages);

        assert!(text.contains('7'), "{text:?}");
        assert_eq!(diagnostics.removed_furniture_runs, 0);
    }

    #[test]
    fn a_margin_box_is_moved_after_the_page_and_not_deleted() {
        let page = Page {
            number: 1,
            height: 800.0,
            blocks: vec![
                Block {
                    lines: vec![line(vec![
                        word("Serialization", 20.0, 100.0),
                        word("is", 20.0, 120.0),
                    ])],
                    image: None,
                },
                Block {
                    lines: vec![
                        line(vec![
                            word("the", 200.0, 100.0),
                            word("sentence", 230.0, 100.0),
                        ]),
                        line(vec![
                            word("it", 200.0, 120.0),
                            word("interrupted", 230.0, 120.0),
                        ]),
                        line(vec![word("continues", 200.0, 140.0)]),
                    ],
                    image: None,
                },
            ],
        };
        let (text, diagnostics) = read(vec![page]);

        let body = text.find("the sentence").expect("body text");
        let aside = text.find("Serialization").expect("margin box kept");
        assert!(
            body < aside,
            "the margin box moved after the body: {text:?}"
        );
        assert_eq!(diagnostics.relocated_marginalia_blocks, 1);
        assert_eq!(diagnostics.body_column_pages, 1);
    }

    /// Relocation moves where a box is read, never where it is drawn: a
    /// preview highlight still lands on the margin, not on the body text the
    /// box now follows.
    #[test]
    fn a_relocated_margin_box_keeps_the_position_it_was_drawn_at() {
        let page = Page {
            number: 1,
            height: 800.0,
            blocks: vec![
                Block {
                    lines: vec![line(vec![word("Serialization", 20.0, 100.0)])],
                    image: None,
                },
                Block {
                    lines: vec![
                        line(vec![
                            word("the", 200.0, 100.0),
                            word("sentence", 230.0, 100.0),
                        ]),
                        line(vec![word("continues", 200.0, 120.0)]),
                    ],
                    image: None,
                },
            ],
        };
        let mut diagnostics = ExtractionDiagnostics::default();
        let reading = sanitize(vec![page], &mut [], &[], &mut diagnostics);

        let aside = bbox_at(&reading, "Serialization");
        assert_eq!((aside.x, aside.y), (20.0, 100.0));
        let body = bbox_at(&reading, "the sentence");
        assert_eq!((body.x, body.y), (200.0, 100.0));
    }

    #[test]
    fn a_two_column_page_is_ambiguous_and_left_in_raster_order() {
        let page = Page {
            number: 1,
            height: 800.0,
            blocks: vec![
                Block {
                    lines: vec![line(vec![
                        word("left", 50.0, 100.0),
                        word("column", 90.0, 100.0),
                    ])],
                    image: None,
                },
                Block {
                    lines: vec![line(vec![
                        word("right", 400.0, 100.0),
                        word("column", 450.0, 100.0),
                    ])],
                    image: None,
                },
            ],
        };
        let (text, diagnostics) = read(vec![page]);

        assert!(text.starts_with("left column"), "{text:?}");
        assert_eq!(diagnostics.ambiguous_column_pages, 1);
        assert_eq!(diagnostics.relocated_marginalia_blocks, 0);
    }

    /// The seam a relocation creates is not a line wrap: the last body word of
    /// the page must not marry the first word of the box moved behind it.
    #[test]
    fn a_relocated_aside_does_not_continue_the_body_it_follows() {
        let page = Page {
            number: 1,
            height: 800.0,
            blocks: vec![
                Block {
                    lines: vec![line(vec![word("aside", 20.0, 100.0)])],
                    image: None,
                },
                Block {
                    lines: vec![
                        line(vec![
                            word("the", 200.0, 100.0),
                            word("body", 230.0, 100.0),
                            word("ends", 260.0, 100.0),
                        ]),
                        line(vec![word("bro-", 200.0, 120.0)]),
                    ],
                    image: None,
                },
            ],
        };
        let (text, diagnostics) = read(vec![page]);

        assert!(text.contains("bro-\naside"), "{text:?}");
        assert_eq!(diagnostics.joined_wrap_hyphens, 0);
        assert_eq!(diagnostics.kept_wrap_hyphens, 0);
        // The one case in which a reading still holds a hyphen-broken word,
        // counted rather than passed off as a decision.
        assert_eq!(diagnostics.unjoinable_wrap_breaks, 1);
    }

    /// Every retained word is in the map, at the range it was written to, in
    /// order and without overlap.
    #[test]
    fn the_map_covers_every_word_of_the_reading_exactly_once() {
        let pages = vec![
            body_page(
                1,
                vec![
                    line(vec![
                        word("alpha", 100.0, 100.0),
                        word("beta-", 150.0, 100.0),
                    ]),
                    line(vec![word("gamma", 100.0, 120.0)]),
                ],
            ),
            body_page(2, vec![line(vec![word("delta", 100.0, 100.0)])]),
        ];
        let mut diagnostics = ExtractionDiagnostics::default();
        let reading = sanitize(pages, &mut [], &[], &mut diagnostics);

        let mut covered = vec![false; reading.text.len()];
        let mut previous_end = 0;
        for segment in &reading.segments {
            assert!(segment.text_range.start >= previous_end, "segments overlap");
            assert!(segment.text_range.end <= reading.text.len());
            previous_end = segment.text_range.end;
            covered[segment.text_range.start..segment.text_range.end].fill(true);
        }
        for (offset, byte) in reading.text.bytes().enumerate() {
            assert_eq!(
                covered[offset],
                !byte.is_ascii_whitespace(),
                "byte {offset} of {:?}",
                reading.text
            );
        }
    }
}
