//! Retained extractions and the images, OCR and descriptions they keep.

use super::*;

/// What the index keeps of an extraction beyond its text and its chunks.
///
/// One retained object rather than one table per kind of thing a page holds.
/// `reading_regions` was the first of those tables and is folded in here; the
/// image inventory would have been the second and formula anchors the third,
/// each costing a migration, a write path, a delete and a copy in the adoption
/// loop — four places apiece, for facts that arrive together from one
/// extraction and are read together whole-file.
///
/// A new retained item is a `#[serde(default)]` field here, which is the same
/// evolution mechanism [`ExtractedImage::origin`] and [`ImageOcrRegion::kind`]
/// already use.
///
/// The rule that keeps this from becoming a junk drawer: it holds only what
/// the extraction recipe computed, never anything derived at query time, and a
/// stored shape under another [`RETAINED_EXTRACTION_VERSION`] is refused
/// rather than guessed at.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RetainedExtraction {
    pub version: String,
    /// Areas whose glyph run the reading replaced.
    #[serde(default)]
    pub reading_regions: Vec<ReadingRegion>,
    /// Every image the pages draw, as placement records.
    ///
    /// `None` is *unknown* — a rendition retained before the inventory
    /// existed — and is emphatically not `Some(vec![])`, which is a document
    /// whose pages draw nothing. A consumer that cannot tell those apart
    /// reports "no figures" for every document indexed last month.
    #[serde(default)]
    pub images: Option<Vec<RetainedImage>>,
}

/// Bumped when the retained shape changes. An entry written under another
/// version is not read, for the reason `CACHE_FORMAT_VERSION` gives: a field
/// that moved would be read as a field that means something else.
pub const RETAINED_EXTRACTION_VERSION: &str = "retained-extraction-v1";

/// One image of one rendition, as the index keeps it: where it is and what it
/// should hash to.
///
/// Placement, not annotation. The transcription and the description are
/// already in the reading, and their structured form is already in the
/// annotation cache under a key that is exactly these fields — so a copy here
/// would be a third store of one fact. What this adds is the part nothing else
/// holds: that the picture exists, where it sits, and the digest a re-derived
/// copy of it must match.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RetainedImage {
    /// `p{page}-i{ordinal}`, as extraction assigned it.
    pub id: String,
    pub page: u32,
    pub origin: RegionOrigin,
    pub bbox: BoundingBox,
    pub transform: ImageTransform,
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Digest of the decoded RGB pixels. Empty for an image that was never
    /// decoded, which is what a technical limit leaves behind.
    pub image_sha256: String,
    /// Where this image's enrichment block landed, when one was written.
    pub reading_range: Option<ByteRange>,
    /// Where the picture sits in the reading. See
    /// [`ExtractedImage::reading_anchor`] — `None` means the rendition
    /// predates anchors, not that the image is unplaced.
    pub reading_anchor: Option<usize>,
    pub status: ImageAnalysisStatus,
    /// Whether analysis produced prose about this picture. The prose itself is
    /// in the reading; this says whether asking for it is worth the trip.
    pub has_description: bool,
}

impl RetainedImage {
    /// The placement record of one extracted image.
    pub fn of(image: &ExtractedImage) -> Self {
        Self {
            id: image.id.clone(),
            page: image.page,
            origin: image.origin,
            bbox: image.bbox.clone(),
            transform: image.transform,
            pixel_width: image.pixel_width,
            pixel_height: image.pixel_height,
            image_sha256: image.image_sha256.clone(),
            reading_range: image.reading_range.clone(),
            reading_anchor: image.reading_anchor,
            status: image.status.clone(),
            has_description: image.description.is_some(),
        }
    }
}

/// Nothing retained, under the current version — which is what a rendition
/// that kept nothing looks like, and what an unreadable stored shape falls
/// back to. `images: None` throughout: "nobody wrote an inventory" is the
/// honest reading of both, and it is not "this document draws nothing".
impl Default for RetainedExtraction {
    fn default() -> Self {
        Self {
            version: RETAINED_EXTRACTION_VERSION.to_string(),
            reading_regions: Vec::new(),
            images: None,
        }
    }
}

impl RetainedExtraction {
    /// What this extraction leaves behind for the index to keep.
    pub fn of(content: &ExtractedContent) -> Self {
        Self {
            version: RETAINED_EXTRACTION_VERSION.to_string(),
            reading_regions: crate::extract::image::serialize::reading_regions(content),
            images: Some(content.images.iter().map(RetainedImage::of).collect()),
        }
    }

    /// The stored form, or `None` for a shape written under another version.
    ///
    /// A refusal rather than a partial read: the whole point of the version is
    /// that a field which moved would otherwise be read as a different field.
    pub fn parse(json: &str) -> Option<Self> {
        let parsed: Self = serde_json::from_str(json).ok()?;
        (parsed.version == RETAINED_EXTRACTION_VERSION).then_some(parsed)
    }
}

/// One region of text the recognizer spotted inside a native image.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageOcrRegion {
    /// What `text` is. Defaulted so an annotation cached before recognizers
    /// distinguished kinds still reads as the prose it was.
    #[serde(default)]
    pub kind: RegionKind,
    pub text: String,
    /// Admission signal derived from token log-probabilities. Explicitly
    /// uncalibrated: it orders regions of one image by how confidently the
    /// model emitted them, and nothing more. One tested threshold decides
    /// admission; no consumer should read it as a probability.
    pub confidence: f32,
    /// The spotted quadrilateral in the decoded image's own pixel space,
    /// top-left origin.
    pub polygon_within_image: Vec<Point>,
    /// The same quadrilateral mapped onto the page through the image
    /// transform. What exact search highlights.
    pub page_polygon: Vec<Point>,
    /// Why this region is or is not in the canonical reading.
    pub admission: OcrAdmission,
}

/// What became of one spotted region.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum OcrAdmission {
    /// In the canonical reading.
    Accepted,
    /// Below the admission threshold. Kept here, not in the reading, so a
    /// missing label is visible rather than silently absent.
    RejectedLowConfidence,
    /// The page draws this text natively over the image; the document's own
    /// glyphs already carry it and are the better evidence.
    DeduplicatedAgainstNativeText,
    /// A formula whose LaTeX does not parse.
    ///
    /// Confidence is the wrong question for a formula: an autoregressive
    /// decoder that truncates mid-expression is perfectly confident about
    /// every token it did emit, and the result is invalid LaTeX at a high
    /// score. A formula that does not parse is a rejected region with a
    /// recorded reason, never a string inserted and hoped over.
    RejectedInvalidLatex,
    /// A table or chart whose transcription is not a rectangular table of at
    /// least two rows and two columns. A ragged table is a failed recognition
    /// wearing the shape of a result.
    RejectedMalformedTable,
    /// The decode that produced this region reached its token cap without
    /// emitting an end-of-sequence token.
    ///
    /// Not a heuristic and not a per-kind rule: a decoder stopped by a cap
    /// rather than by finishing is not a reading, whatever the bytes it did
    /// emit happen to look like. This is why it is checked before
    /// [`Self::RejectedInvalidLatex`]'s own structural check — a truncated
    /// formula that happens to close is still a decode that did not finish.
    RejectedTruncated,
    /// A table read from a structure model whose first row holds no glyph at
    /// all.
    ///
    /// The header is the row every other row is read against, and a grid whose
    /// first row is empty is one whose rows have been shifted off the ink —
    /// the model proposed a band of cells above the table. The body may still
    /// look plausible, which is exactly why this is refused rather than
    /// admitted with a blank header row.
    RejectedEmptyHeaderRow,
    /// A table read from a structure model that left a word of the page
    /// standing inside its own rectangle in no cell at all.
    ///
    /// The glyphs are certainly there — the page drew them, and the crop was
    /// cut around them — so a word with nowhere to go is a cell the grid does
    /// not have. Admitting it would put a table into the reading *in place of*
    /// the page's own run while silently losing part of that run.
    RejectedUnassignedWords,
    /// A table read from a structure model where too much of the grid holds no
    /// glyph. See `ocr::TABLE_MAX_EMPTY_CELL_NUMERATOR`.
    RejectedSparseTable,
}

/// A semantic description of what an image shows, generated rather than read.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageDescription {
    /// Prose, and only prose. This is what reaches the canonical reading.
    ///
    /// A structured relationship list lived here until 2026-08-27 and was
    /// withdrawn: nothing consumed it, it validated nothing a hallucinated
    /// sentence would not also pass, and requiring conformant JSON narrowed
    /// the models a local-first describer could run on. Structure returns as
    /// its own measured feature when something reads it.
    pub description: String,
}

/// How far analysis of one image got.
///
/// A failure leaves the native text and the stages that did succeed in place,
/// and says so here. It is never recorded as a successful analysis that found
/// nothing — those are different facts, and only one of them means the image
/// has no text.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ImageAnalysisStatus {
    /// Every configured stage ran.
    Complete,
    /// Not analyzed at all: a technical limit rejected it before any stage.
    SkippedTechnicalLimit { reason: String },
    /// Not analyzed at all: the configured [`ImageScope`] does not read
    /// pictures of this origin, so the pixels were never decoded.
    ///
    /// Its own variant rather than a technical limit's reason string: nothing
    /// is wrong with the image, and filing a setting's decision under the
    /// decoder's casualties would send a reader looking for a fault that does
    /// not exist.
    NotRead { reason: String },
    /// At least one stage failed. The message is the failure, not a summary.
    Partial { failures: Vec<String> },
}

/// One native image block of a PDF, and whatever analysis established about it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExtractedImage {
    /// Stable within one extraction: `p{page}-i{ordinal}`, where the ordinal
    /// counts images across the document in the order the pages draw them.
    /// Referenced by [`TextProvenance`], so it is fixed at discovery and
    /// never depends on what analysis found.
    pub id: String,
    /// 1-based, numbered as extraction numbers pages.
    pub page: u32,
    /// Whether the PDF embedded these pixels or Wilkes rendered them out of
    /// the page's own drawing. Defaulted on read: every rendition written
    /// before typeset routing existed contains embedded images only, which is
    /// what the default says.
    #[serde(default)]
    pub origin: RegionOrigin,
    /// The page region the image occupies, axis-aligned.
    pub bbox: BoundingBox,
    pub transform: ImageTransform,
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Digest of the decoded RGB pixels — not of the PDF's compressed stream,
    /// which two renditions of the same picture need not share. This is what
    /// makes the annotation cache addressable by the thing that was analyzed.
    pub image_sha256: String,
    /// Where this image's reading landed in the canonical reading. `None`
    /// when nothing was serialized for this image, which is the ordinary case
    /// for a logo the recognizer read no text in.
    ///
    /// Where the bytes *are*, and nothing about what surrounds them. An
    /// inline expression sets this exactly as a block does — it wrote bytes —
    /// so a consumer asking "is there a structural boundary here" must ask
    /// [`reading_block`](Self::reading_block) instead.
    pub reading_range: Option<ByteRange>,
    /// The same range, set only for a region that stands apart from the prose
    /// around it.
    ///
    /// Not every region that writes bytes is a structural unit. A picture the
    /// document embeds is one: its label, transcription and description are
    /// Wilkes' account of something drawn beside the text, and the prose on
    /// either side is not the sentence it interrupts. An expression the page
    /// typeset is not — its reading stands in place of the document's own
    /// glyphs, which makes it a constituent of the sentence that introduces
    /// it. `serialize::is_structural_block` owns that distinction and this
    /// field is where its answer is recorded; a table, a chart and a code
    /// listing are line-structured whatever set them, and keep the boundary.
    ///
    /// This exists because [`reading_range`](Self::reading_range) cannot tell
    /// those apart, and chunking asked it to: every region with bytes became a
    /// structural run, so one sentence carrying three inline expressions was
    /// cut into seven passages and its prose stranded in fragments as short as
    /// `"of "`. The reading said inline, the chunker heard block.
    ///
    /// The field alone was not the whole fix. A *displayed* formula kept its
    /// seam for another round, on the reasoning that a line of its own made it
    /// a block — and cut "…as the bit sequence", "expressed as a formula:" and
    /// "while Bob calculates" into passages of their own: the same failure one
    /// typographic step further out. Where the page put a region is not the
    /// test. What its bytes stand in place of is.
    #[serde(default)]
    pub reading_block: Option<ByteRange>,
    /// Where the picture *is* in the reading: the zero-width offset between
    /// the text above it and the text below it, at the position the page drew
    /// it.
    ///
    /// Distinct from [`reading_range`](Self::reading_range), which is where
    /// this image's enrichment *block* was written and is `None` whenever
    /// nothing was written. An image nothing was established about still has a
    /// place in the reading — `flatten` emits every image block in reading
    /// order, filtered on nothing — and this is that place. It is what links a
    /// figure to the passage it was drawn into, so a diagram no recognizer
    /// read is locatable on exactly the same terms as one that was.
    ///
    /// Recorded before the newline that opens an enrichment block, so an
    /// analyzed image anchors at the end of the prose it interrupts while its
    /// block occupies the bytes after. A consumer testing containment against
    /// a chunk therefore includes both endpoints: a zero-width point on a
    /// boundary belongs to the passages on either side of it.
    ///
    /// Defaulted on read: a rendition extracted before anchors existed has
    /// none, which is *unknown*, not "drawn nowhere".
    #[serde(default)]
    pub reading_anchor: Option<usize>,
    pub ocr_regions: Vec<ImageOcrRegion>,
    pub description: Option<ImageDescription>,
    /// The analyzer recipe that produced the above — the same string that
    /// enters the extraction recipe. Empty when no analyzer ran.
    pub analyzer_identity: String,
    pub status: ImageAnalysisStatus,
}

impl ExtractedImage {
    pub fn accepted_ocr(&self) -> impl Iterator<Item = &ImageOcrRegion> {
        self.ocr_regions
            .iter()
            .filter(|region| region.admission == OcrAdmission::Accepted)
    }
}

// ── Extraction ───────────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExtractedContent {
    pub text: String,
    pub source_map: SourceMap,
    pub metadata: FileMetadata,
    /// The native images this rendition found, in reading order. Empty for
    /// formats that have none and for a PDF whose pages draw no images.
    #[serde(default)]
    pub images: Vec<ExtractedImage>,
}
