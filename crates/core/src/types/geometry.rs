//! Positions on a page: bounding boxes, transforms, and the regions a page is
//! read in.

use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl BoundingBox {
    pub fn merge(&self, other: &Self) -> Self {
        let x1 = self.x.min(other.x);
        let y1 = self.y.min(other.y);
        let x2 = (self.x + self.width).max(other.x + other.width);
        let y2 = (self.y + self.height).max(other.y + other.height);
        BoundingBox {
            x: x1,
            y: y1,
            width: (x2 - x1).max(0.0),
            height: (y2 - y1).max(0.0),
        }
    }
}

// ── Native image enrichment ──────────────────────────────────────────────────

/// One point in MuPDF page space, or in an image's own pixel space —
/// whichever the field holding it names. Origin top-left, y increasing
/// downward in both.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// The affine placement of a native image on its page, as MuPDF reports it.
///
/// Maps the unit square onto the page region the image occupies. This is the
/// only thing that turns a coordinate inside the image into a coordinate on
/// the page, so it is retained rather than reduced to the bounding box it
/// implies: a rotated or mirrored placement has a bounding box that is not
/// its outline.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImageTransform {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl ImageTransform {
    /// Map a point of the unit square through the placement.
    pub fn apply(&self, x: f32, y: f32) -> Point {
        Point {
            x: self.a * x + self.c * y + self.e,
            y: self.b * x + self.d * y + self.f,
        }
    }

    /// Map a pixel position inside the decoded image onto the page.
    ///
    /// MuPDF places an image by mapping the unit square onto the page, with
    /// the image's *first* row of pixels along the square's top edge — which
    /// is the edge at `y = 0` before the transform, because the transform
    /// carries whatever flip the page applied. So a pixel's normalized
    /// position is used as-is, without a vertical inversion of our own: the
    /// page already said which way up the image goes.
    pub fn pixel_to_page(&self, x: f32, y: f32, pixel_width: u32, pixel_height: u32) -> Point {
        let width = pixel_width.max(1) as f32;
        let height = pixel_height.max(1) as f32;
        self.apply(x / width, y / height)
    }
}

/// What a recognized region *is*, and therefore how its text is to be read.
///
/// A recognizer that only transcribes prose has no use for this and says
/// `Text` for everything. One that parses a document does: a formula's text is
/// LaTeX and a table's is a Markdown table, and a consumer that cannot tell
/// them apart from prose will quote a table as if the document had written it
/// in pipes.
///
/// The kind belongs to the region and not to the engine, because it is a
/// property of what was read. Which kinds an engine can produce at all is a
/// property of the engine *and its task configuration*, and is answered by the
/// recognizer catalogue rather than inferred from here.
///
/// It lives here, beside [`ImageOcrRegion`] and [`TextProvenance`], because
/// both carry it across the API boundary — the same reason
/// [`RecognizerInventory`] is here rather than inside the model module that
/// first needed it.
/// Where the pixels a region was read from came from.
///
/// Two answers, and the reading says which. An `Embedded` region was read out
/// of a raster the PDF carries: the bytes are a transcription of a picture,
/// and the document's own glyphs are elsewhere. A `Typeset` region was read
/// out of a rasterization *Wilkes* made of an area the page draws with fonts
/// and paths — a display formula, a ruled table — and the bytes stand in place
/// of the glyph run that area drew.
///
/// The distinction is load-bearing twice over. It chooses the label, because
/// "embedded" is a false claim about content the page typeset. And it chooses
/// whether the native-glyph check applies: a transcription that repeats
/// glyphs the page already draws is redundant for an embedded picture and is
/// the entire *point* for a typeset region, where the recognizer is the
/// designated owner of those bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionOrigin {
    /// A raster image the PDF embeds.
    #[default]
    Embedded,
    /// An area the page draws with fonts and paths, rasterized by Wilkes so a
    /// recognizer could read it.
    Typeset,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionKind {
    /// Prose, a heading, a caption — the reading, verbatim.
    #[default]
    Text,
    /// LaTeX.
    Formula,
    /// A Markdown table.
    Table,
    /// A Markdown table reconstructed from a chart. Distinct from `Table`
    /// because it is Wilkes' reading of a picture rather than a transcription
    /// of ruled cells, and a consumer is entitled to know which it has.
    Chart,
    /// A source-code listing, verbatim.
    Code,
}

impl RegionKind {
    /// Every kind, so a consumer that must handle all of them can be checked
    /// against the list rather than against its own memory of it.
    pub const ALL: [RegionKind; 5] = [
        RegionKind::Text,
        RegionKind::Formula,
        RegionKind::Table,
        RegionKind::Chart,
        RegionKind::Code,
    ];

    /// The label this kind is serialized under in the canonical reading.
    ///
    /// Every kind has one, and the exhaustive match is the mechanism: a kind
    /// added without a label does not compile, so a reader cannot meet
    /// recognized content that does not say what it is.
    ///
    /// `Image transcribed chart:` deliberately does not say *embedded*. The
    /// other labels name content that is present in the image and was read; a
    /// chart rendered as rows is Wilkes' reconstruction of what the picture
    /// depicts, and the label is the only place a consumer learns that.
    /// `Typeset` labels say *page*, and never *embedded*: the content is the
    /// document's own typesetting, re-read from a rasterization of it, and the
    /// glyph run it stands in place of is no longer in the reading beside it.
    /// Calling that "embedded" would claim the page carries a picture of a
    /// formula, which is exactly what it does not do.
    pub const fn label(&self, origin: RegionOrigin) -> &'static str {
        match (origin, self) {
            (RegionOrigin::Embedded, RegionKind::Text) => "Image embedded text:",
            (RegionOrigin::Embedded, RegionKind::Formula) => "Image embedded formula:",
            (RegionOrigin::Embedded, RegionKind::Table) => "Image embedded table:",
            (RegionOrigin::Embedded, RegionKind::Chart) => "Image transcribed chart:",
            (RegionOrigin::Embedded, RegionKind::Code) => "Image embedded code:",
            (RegionOrigin::Typeset, RegionKind::Text) => "Page text:",
            (RegionOrigin::Typeset, RegionKind::Formula) => "Page formula:",
            (RegionOrigin::Typeset, RegionKind::Table) => "Page table:",
            (RegionOrigin::Typeset, RegionKind::Chart) => "Page transcribed chart:",
            (RegionOrigin::Typeset, RegionKind::Code) => "Page code:",
        }
    }

    /// Whether reading this kind is worth putting in place of the glyphs a
    /// page drew.
    ///
    /// Only asked of a typeset region, where the two accounts of an area are
    /// the page's own glyph run and the recognizer's reading of a rendering
    /// of it. A formula and a table are why such a region was marked out: the
    /// glyph run for those is flattened past use — `ci = ai ⊕bi` is not
    /// mathematics — and the reading is the only usable account of them.
    ///
    /// Prose and code are not. If a recognizer reads a region the page's own
    /// typography called a formula and answers "this is a sentence", the two
    /// disagree, and the document's glyphs are the better evidence for a
    /// sentence — they are what the author wrote, at no risk of transcription
    /// error. Replacing them on the strength of that disagreement would trade
    /// certainty for a model's opinion.
    pub const fn supersedes_native_glyphs(&self) -> bool {
        match self {
            RegionKind::Formula | RegionKind::Table | RegionKind::Chart => true,
            RegionKind::Text | RegionKind::Code => false,
        }
    }

    /// Whether these bytes are indivisible in the reading.
    ///
    /// Half a LaTeX expression is not a shorter expression, it is an invalid
    /// one, and half a Markdown table is not a smaller table. Prose survives
    /// being cut; these do not, so a chunk boundary may not fall inside them.
    pub const fn is_indivisible(&self) -> bool {
        match self {
            RegionKind::Formula | RegionKind::Table | RegionKind::Chart => true,
            RegionKind::Text | RegionKind::Code => false,
        }
    }
}

/// One stretch of the reading that owns a page area outright, and where on
/// the page it belongs.
///
/// The reading records what each of its bytes *is* through
/// [`TextProvenance`], and where each of them came from through
/// [`SourceOrigin`], but only for as long as the extraction is in hand. This
/// is the part of that worth keeping: the areas where a recognizer's reading
/// of a region the page typeset *replaced* the glyph run drawn there, which
/// are the only areas the document's own glyphs no longer account for. Every
/// other byte of the reading has the page's text behind it and needs no
/// record.
///
/// No text is carried. `full_text` already holds these bytes at
/// `text_range`, and a second copy of them would be a second answer to the
/// same question. What is missing from the index — and what this supplies —
/// is *where on the page* a stretch of the reading lives.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReadingRegion {
    /// The [`ExtractedImage`] this was read from. Pieces sharing one are one
    /// area of the page, and are read back as one.
    pub area_id: String,
    /// 1-based, numbered as extraction numbers pages.
    pub page: u32,
    /// The area whose glyph run the reading replaced — the region as it was
    /// marked out and rendered, not the recognizer's polygon around its ink.
    /// The former is exactly what stopped being in the reading; the latter can
    /// be tighter, and would leave the flattened glyphs it missed beside the
    /// transcription that superseded them.
    pub bbox: BoundingBox,
    pub text_range: ByteRange,
}

/// A page area whose text the reading owns, as a reading surface needs it:
/// resolved, joined, and ready to stand in place of what the page draws there.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SupersededArea {
    pub page: u32,
    pub bbox: BoundingBox,
    /// The reading's account of the area. The pieces of one area joined by
    /// newlines, as the reading separates them — and never the `Page formula:`
    /// label standing in front of them, which belongs to the transcription
    /// block and to no region of the page.
    pub text: String,
}
