use serde::{de, Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// ── Byte range (replaces std::ops::Range<usize> for serde compat) ────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

// ── Indexing configuration ────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct IndexingConfig {
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub supported_extensions: Vec<String>,
}

// ── Search mode ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub enum SearchMode {
    #[default]
    Grep,
    Semantic,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchScope {
    #[default]
    Corpus,
    All,
    File {
        path: PathBuf,
    },
}

// ── Query ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchQuery {
    pub pattern: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    pub root: PathBuf,
    /// 0 = unlimited
    pub max_results: usize,
    /// Respect .gitignore / .ignore files during the walk.
    #[serde(default = "default_true")]
    pub respect_gitignore: bool,
    /// Skip files larger than this many bytes (0 = unlimited).
    #[serde(default)]
    pub max_file_size: u64,
    /// Lines of context to include around each match (text files only).
    #[serde(default = "default_context_lines")]
    pub context_lines: u32,
    /// Which search backend to use.
    #[serde(default)]
    pub mode: SearchMode,
    /// Optional result scope inside `root`.
    #[serde(default)]
    pub scope: SearchScope,
    /// The global list of supported extensions from settings.
    #[serde(default)]
    pub supported_extensions: Vec<String>,
    /// Optional saved smart collection intersected with `scope` by the backend.
    #[serde(default)]
    pub collection_id: Option<String>,
    /// Optional document tags. A document must contain every requested tag.
    #[serde(default)]
    pub tag_ids: Vec<String>,
}

fn default_true() -> bool {
    true
}
fn default_context_lines() -> u32 {
    2
}

// ── Results ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Match {
    /// Byte range into the extracted text.
    /// Some for plain-text files (used for highlight positioning).
    /// None for PDF chunks (highlight routes through origin.bbox instead).
    pub text_range: Option<ByteRange>,
    pub matched_text: String,
    pub context_before: String,
    pub context_after: String,
    pub origin: SourceOrigin,
    /// Cosine similarity score for semantic matches; None for grep matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileMatches {
    pub path: PathBuf,
    pub file_type: FileType,
    /// Composed cached document title when available from the search catalog.
    /// The application may refresh it again at the metadata boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Direct matches against document identity fields. Kept separate from
    /// content matches because they have no truthful line, page, or byte
    /// position inside the document.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_matches: Vec<SearchFieldMatch>,
    pub matches: Vec<Match>,
}

impl FileMatches {
    pub fn total_match_count(&self) -> usize {
        self.field_matches.len() + self.matches.len()
    }
}

/// One document admitted by the application's authoritative search catalog.
/// Providers search this list instead of independently walking the filesystem.
#[derive(Clone, Debug)]
pub struct SearchDocument {
    pub path: PathBuf,
    pub file_type: FileType,
    pub title: Option<String>,
    pub author: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchField {
    Filename,
    Title,
    Author,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchFieldMatch {
    pub field: SearchField,
    pub matched_text: String,
    pub context_before: String,
    pub context_after: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelatedDocumentsQuery {
    pub root: PathBuf,
    pub path: PathBuf,
    #[serde(default)]
    pub scope: SearchScope,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub collection_id: Option<String>,
}

// ── Research state ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tag {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewTag {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateTag {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentTagUpdate {
    pub paths: Vec<PathBuf>,
    #[serde(default)]
    pub add_tag_ids: Vec<String>,
    #[serde(default)]
    pub remove_tag_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmartCollection {
    pub id: String,
    pub name: String,
    pub expression: String,
    pub filter_schema_version: i64,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewSmartCollection {
    pub name: String,
    pub expression: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateSmartCollection {
    pub name: String,
    pub expression: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CollectionValidation {
    pub valid: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchLogStatus {
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchLogEntry {
    pub id: String,
    pub query: SearchQuery,
    pub collection_name: Option<String>,
    pub collection_revision: Option<i64>,
    pub initiated_by: String,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub result_count: usize,
    pub duration_ms: Option<u64>,
    pub status: SearchLogStatus,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelatedDocument {
    #[serde(flatten)]
    pub entry: FileEntry,
    pub score: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CitationLinksQuery {
    pub root: PathBuf,
    pub path: PathBuf,
}

/// Citation neighbours of a document that are present in the library, resolved
/// by DOI. `references` are documents the anchor cites; `cited_by` are
/// documents that cite the anchor. `all_references` contains every outgoing
/// DOI known to the citation provider, including works absent from the library.
/// Both library directions carry the same metadata-enriched [`FileEntry`] shape
/// as every other document list.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CitationLinks {
    pub references: Vec<FileEntry>,
    pub cited_by: Vec<FileEntry>,
    #[serde(default)]
    pub all_references: Vec<CitationReference>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CitationReference {
    pub doi: String,
    /// First document line that contains this exact normalized DOI.
    pub citation_line: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileType {
    PlainText,
    Pdf,
}

impl FileType {
    pub fn detect(path: &std::path::Path, supported_extensions: &[String]) -> Option<Self> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());

        if let Some(ext) = &ext {
            if supported_extensions
                .iter()
                .any(|s| s.to_ascii_lowercase() == *ext)
            {
                if ext == "pdf" {
                    return Some(FileType::Pdf);
                } else {
                    return Some(FileType::PlainText);
                }
            }
        }

        // Special case: check well-known filenames if no extension or unknown extension
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let name_lc = name.to_ascii_lowercase();
            if [
                "makefile",
                "dockerfile",
                "jenkinsfile",
                "procfile",
                "gemfile",
                "rakefile",
                "vagrantfile",
                "podfile",
                "brewfile",
            ]
            .contains(&name_lc.as_str())
            {
                return Some(FileType::PlainText);
            }
        }
        None
    }
}

// ── Source Mapping ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceMap {
    pub segments: Vec<SourceSegment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceSegment {
    pub text_range: ByteRange,
    pub origin: SourceOrigin,
    /// Where these bytes came from: the document's own glyphs, an image's
    /// transcription, or a description derived from one. `origin` says *where
    /// on the page* the bytes belong; this says *what they are*, and the two
    /// answer different questions — a transcribed label and the caption beside
    /// it can share a page region while differing in whether a reader may
    /// quote them as the author's words.
    ///
    /// Defaulted on read so a source map written before image enrichment
    /// existed loads as what it was: entirely native text.
    #[serde(default)]
    pub provenance: TextProvenance,
}

/// What produced a run of the canonical reading.
///
/// Every inserted byte carries one. `Native` is the document's own glyphs;
/// the other two are Wilkes' additions, and each names the image it came from
/// so a consumer can reach the region, the confidence, and the analyzer that
/// produced it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum TextProvenance {
    /// The document's own glyphs. Also what a source map written before image
    /// enrichment existed deserializes to, which is what those maps were.
    #[default]
    Native,
    /// A map rebuilt at a coarser granularity than extraction produced —
    /// notably the per-chunk map the index reconstructs for cached text,
    /// where one segment can span both native and inserted bytes. Saying so
    /// beats claiming either.
    Unrecorded,
    /// Text a recognizer read out of pixels.
    ///
    /// Those pixels are one of two things, and `image_id` resolves to an
    /// [`ExtractedImage`] whose [`RegionOrigin`] says which: a raster the PDF
    /// embeds, or an area the page typeset that Wilkes rendered so it could
    /// be read as the formula or table it was set as. The second replaces the
    /// glyph run the page drew there, so these bytes are the reading's only
    /// account of that area — which is exactly why the origin is recorded on
    /// the image rather than left to be inferred from the label.
    ImageOcr {
        image_id: String,
        /// The region's admission signal, carried through so a consumer can
        /// see how strong the evidence for these bytes was. Uncalibrated —
        /// see [`ImageOcrRegion::confidence`].
        ///
        /// `None` for the bytes Wilkes wrote to frame the transcription — the
        /// `Image embedded text:` label and the separators between regions.
        /// Those belong to the transcription block and to no single region,
        /// and inventing a confidence for them would be the one kind of lie
        /// this enum exists to prevent.
        confidence: Option<f32>,
        /// What was recognized here, and therefore how these bytes are to be
        /// read. A formula and a paragraph are both recognized content and
        /// are not the same claim about the document, so provenance names the
        /// kind rather than leaving a consumer to infer it from the label
        /// standing in front of it.
        ///
        /// Defaulted on read: a source map written before recognizers
        /// distinguished kinds transcribed prose and says so.
        #[serde(default)]
        kind: RegionKind,
    },
    ImageDescription {
        image_id: String,
        analyzer_id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SourceOrigin {
    TextFile {
        line: u32,
        col: u32,
    },
    PdfPage {
        page: u32,
        bbox: Option<BoundingBox>,
    },
}

/// One entry of a document's declared table of contents.
///
/// *Declared*, not inferred: a PDF bookmark tree or a Markdown heading, read
/// from the file as the author wrote it. Nothing here is derived from the
/// text's meaning, so an empty outline is a fact about the document rather
/// than a failure to compute one.
///
/// The locator is whichever kind the document expresses — a PDF bookmark
/// resolves to a page, a heading to a byte offset in the extracted text — and
/// callers needing a single unit resolve it against material they already hold
/// (the chunk export maps both onto chunk ordinals). Carrying both as options
/// beats inventing the missing one: a page number derived from a byte offset
/// would be a guess wearing a locator's clothes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutlineEntry {
    pub title: String,
    /// Depth in the outline tree, 0 for a top-level entry.
    pub level: u32,
    /// 1-based page, for documents paginated at extraction (PDFs).
    pub page: Option<u32>,
    /// Byte offset into `ExtractedContent.text`. Exact for documents whose
    /// outline lives in the text; for a PDF it is whatever [`OutlineAnchor`]
    /// says it is, and absent when nothing could establish it.
    pub byte_offset: Option<usize>,
    /// How `byte_offset` was established — see [`OutlineAnchor`].
    pub anchor: OutlineAnchor,
}

/// What established an [`OutlineEntry`]'s position in the extracted reading.
///
/// Reported per entry because the answer differs per entry, and a consumer
/// segmenting a document by its outline has to know which boundaries are exact.
/// A document whose entries are mostly `Page` is a document still segmented a
/// page at a time, and that is a fact worth surfacing here rather than leaving
/// a consumer to discover it as a section that starts in the wrong place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlineAnchor {
    /// The heading's own position in the text (Markdown). Exact by
    /// construction: the outline *is* text here.
    TextOffset,
    /// A PDF destination's vertical coordinate, resolved to the first word at
    /// or below it on the destination page.
    DestinationCoordinate,
    /// The bookmark title, found in the destination page's text. Used when the
    /// destination carries no coordinate, or carries one nothing sits below.
    TitleMatch,
    /// Neither was available. The entry carries its page and no offset, and a
    /// consumer resolving it lands on the first passage of that page.
    Page,
}

/// One document's declared outline, with what its extraction had to decide.
///
/// The two travel together because they are produced together: resolving a
/// bookmark to a byte offset means reading the document, and reading the
/// document is where the sanitation judgements are made. A caller that asks
/// for the outline is therefore holding the evidence for how good the offsets
/// it just received are, without a second call to go and find it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeclaredOutline {
    pub entries: Vec<OutlineEntry>,
    pub diagnostics: ExtractionDiagnostics,
}

/// What a document's extraction had to decide for itself, counted.
///
/// Sanitation is not a pure function of the page: whether a repeating band is
/// furniture, and whether a page's words cluster into one body column, are
/// judgements made from the document's own geometry. A judgement that is made
/// silently is one nobody can check, so each is counted here and reported with
/// the document rather than only logged.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionDiagnostics {
    pub pages: u32,
    /// Pages whose words clustered into one dominant body column.
    pub body_column_pages: u32,
    /// Pages left in raster order because the clustering was ambiguous — two
    /// columns, a table, a full-page figure. Marginalia on these pages stay
    /// where the page put them.
    pub ambiguous_column_pages: u32,
    /// Blocks moved out of the reading order to the end of their own page.
    pub relocated_marginalia_blocks: u32,
    /// Repeating head/foot runs removed from the reading.
    pub removed_furniture_runs: u32,
    /// Line-wrap hyphens the document's own vocabulary said to join.
    pub joined_wrap_hyphens: u32,
    /// Line-wrap hyphens kept, for want of that evidence.
    pub kept_wrap_hyphens: u32,
    /// Words still broken across a line, because the half that would complete
    /// them sits on the far side of a relocated margin box. The only way a
    /// reading still contains a hyphen-broken word.
    pub unjoinable_wrap_breaks: u32,

    // ── Native image analysis ────────────────────────────────────────────
    //
    // Defaulted on read, each of them: these counters joined a published wire
    // contract, and an export written before they existed reports no images
    // rather than failing to parse — which is what it found.
    //
    // Counted for the same reason as the rest: an image that produced no
    // enrichment and an image that was never looked at read identically in
    // the text, and only one of them is a fact about the document. "Twenty
    // found, nineteen read, one decoder failure, descriptions not
    // configured" is actionable; a silently missing figure is not.
    /// Native image blocks MuPDF exposed.
    #[serde(default)]
    pub native_images_found: u32,
    /// Images at least one analysis stage ran on.
    ///
    /// This and the two counters around it are about the *document's* images
    /// and count embedded rasters only. The recognition counters that follow
    /// — succeeded, failed, and everything by kind and admission — are about
    /// regions read and cover typeset regions as well, because a formula that
    /// came back as invalid LaTeX is the same fact whichever pixels it was
    /// read from.
    #[serde(default)]
    pub native_images_analyzed: u32,
    /// Images a fixed technical limit rejected before any stage.
    #[serde(default)]
    pub native_images_skipped_technical_limit: u32,
    #[serde(default)]
    pub images_ocr_succeeded: u32,
    #[serde(default)]
    pub images_ocr_failed: u32,
    #[serde(default)]
    pub ocr_regions_accepted: u32,
    #[serde(default)]
    pub ocr_regions_rejected_low_confidence: u32,
    #[serde(default)]
    pub ocr_regions_deduplicated_against_native_text: u32,

    // ── What was recognized, by kind ─────────────────────────────────────
    //
    // A recognizer that parses a document answers a second question beside
    // "how much text": *what kind* of content it found. Counted separately
    // from admission because they fail differently — a table Wilkes never
    // recognized and a table it recognized and rejected as ragged read
    // identically in a reading that contains neither.
    /// Regions by the kind they were read as. Sums to the regions the
    /// recognizer returned.
    #[serde(default)]
    pub regions_routed_text: u32,
    #[serde(default)]
    pub regions_routed_formula: u32,
    #[serde(default)]
    pub regions_routed_table: u32,
    #[serde(default)]
    pub regions_routed_chart: u32,
    #[serde(default)]
    pub regions_routed_code: u32,
    /// Content the recognizer marked out and Wilkes has no kind for, so it
    /// reached no admission rule and no label. Never silently dropped: a
    /// region nobody can name is a fact about the coverage of this build's
    /// kinds, and the count is where it is answerable.
    #[serde(default)]
    pub regions_unroutable: u32,

    // ── Per-kind admission ───────────────────────────────────────────────
    //
    // Each kind is admitted by what makes *that* kind wrong, so each is
    // counted by what rejected it. A low-confidence paragraph, an
    // unparseable formula and a ragged table are three different failures
    // and one number would hide all three.
    #[serde(default)]
    pub formulas_accepted: u32,
    #[serde(default)]
    pub formulas_rejected_invalid_latex: u32,
    #[serde(default)]
    pub tables_accepted: u32,
    #[serde(default)]
    pub tables_rejected_malformed: u32,
    #[serde(default)]
    pub charts_accepted: u32,
    #[serde(default)]
    pub charts_rejected_malformed: u32,

    // ── Typeset regions ──────────────────────────────────────────────────
    //
    // Areas the page draws with fonts and paths rather than embedding as a
    // raster, routed to the recognizer and rendered for it. Counted apart
    // from the embedded images they share a pipeline with, because they cost
    // differently and fail differently: an embedded figure that reads as
    // nothing leaves the reading as it was, and a typeset region that reads
    // as nothing leaves the page's own glyph run in place of it.
    /// Areas the typography marked out as a formula or a ruled table.
    #[serde(default)]
    pub typeset_regions_found: u32,
    /// Regions past the per-document budget, discovered and not rendered.
    /// Never silently dropped: a bounded run that reports nothing dropped
    /// reads identically to a document that had no more to find.
    #[serde(default)]
    pub typeset_regions_over_budget: u32,
    /// Regions whose reading was admitted, and which therefore stand in the
    /// canonical reading in place of the glyphs the page drew there. The
    /// difference from `typeset_regions_found` is the regions where the page's
    /// own glyphs were kept because the recognizer's answer was refused.
    #[serde(default)]
    pub typeset_regions_superseded_native_text: u32,

    #[serde(default)]
    pub images_description_succeeded: u32,
    #[serde(default)]
    pub images_description_failed: u32,
    /// Images that reached the description stage with no describer
    /// configured. Not a failure — a configuration this reading was produced
    /// under, and one that changes what the reading contains.
    #[serde(default)]
    pub images_description_not_configured: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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

impl SourceMap {
    /// Resolve a byte offset in extracted text to a SourceOrigin.
    pub fn resolve(&self, offset: usize) -> Option<SourceOrigin> {
        // Walk segments to find which one contains the offset.
        // Segments should be ordered by text_range.start.
        for seg in &self.segments {
            if offset >= seg.text_range.start && offset < seg.text_range.end {
                return Some(seg.origin.clone());
            }
        }
        // Fall back to last segment
        self.segments.last().map(|s| s.origin.clone())
    }

    /// Resolve a byte range in extracted text to a merged SourceOrigin.
    /// If the range spans multiple PDF segments on the same page, their bboxes are merged.
    pub fn resolve_range(&self, range: ByteRange) -> Option<SourceOrigin> {
        let mut merged_bbox: Option<BoundingBox> = None;
        let mut page_num: Option<u32> = None;
        let mut first_origin: Option<SourceOrigin> = None;

        for seg in &self.segments {
            // Check if segment overlaps with the range
            if seg.text_range.start < range.end && seg.text_range.end > range.start {
                if first_origin.is_none() {
                    first_origin = Some(seg.origin.clone());
                }

                if let SourceOrigin::PdfPage { page, bbox } = &seg.origin {
                    if let Some(p) = page_num {
                        if p != *page {
                            // If match spans multiple pages, we stick to segments on the first page
                            // that overlaps with the match start.
                            continue;
                        }
                    } else {
                        page_num = Some(*page);
                    }

                    if let Some(b) = bbox {
                        merged_bbox = match merged_bbox {
                            Some(existing) => Some(existing.merge(b)),
                            None => Some(b.clone()),
                        };
                    }
                }
            }
        }

        if let Some(p) = page_num {
            Some(SourceOrigin::PdfPage {
                page: p,
                bbox: merged_bbox,
            })
        } else {
            first_origin.or_else(|| self.resolve(range.start))
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
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RegionOrigin {
    /// A raster image the PDF embeds.
    #[default]
    Embedded,
    /// An area the page draws with fonts and paths, rasterized by Wilkes so a
    /// recognizer could read it.
    Typeset,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
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
    /// Where the enrichment block landed in the canonical reading. `None`
    /// when nothing was serialized for this image, which is the ordinary case
    /// for a logo the recognizer read no text in.
    pub reading_range: Option<ByteRange>,
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub doi: Option<String>,
    pub created_at: Option<String>,
    #[serde(default)]
    pub semantic_scholar: Option<SemanticScholarPaper>,
    #[serde(default)]
    pub openalex: Option<OpenAlexWork>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileMetadata {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub mime: Option<String>,
    pub title: Option<String>,
    pub page_count: Option<u32>,
}

// ── Preview ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchRef {
    pub path: PathBuf,
    pub origin: SourceOrigin,
    pub text_range: Option<ByteRange>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub path: PathBuf,
    pub origin: SourceOrigin,
    #[serde(default)]
    pub text_range: Option<ByteRange>,
    pub quote: String,
    pub created_at: String,
    #[serde(default)]
    pub note: Option<String>,
    /// Per-line rectangles (page coordinates) covering exactly the selected
    /// text. Empty for text bookmarks.
    #[serde(default)]
    pub rects: Vec<BoundingBox>,
    /// Content fingerprint of the bookmarked file, captured at creation. Lets a
    /// bookmark survive a rename: when `path` goes missing, the current path is
    /// re-resolved from this identity via the metadata cache. `None` only when
    /// the file could not be stat-ed at creation.
    #[serde(default)]
    pub identity: Option<crate::metadata::cache::FileIdentity>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookmarkClusterGranularity {
    MuchFewer,
    Fewer,
    #[default]
    Balanced,
    More,
    MuchMore,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BookmarkClustersQuery {
    pub bookmark_ids: Vec<String>,
    #[serde(default)]
    pub granularity: BookmarkClusterGranularity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BookmarkCluster {
    /// Content-derived identity: `sha256` over the sorted member
    /// `bookmark_id:input_hash` pairs. Clusters are recomputed on every call
    /// and have no persistent id, so a label cannot be keyed to the cluster
    /// itself — only to its membership. This is also the only stable handle the
    /// UI can patch a late-arriving label against; `representative_bookmark_id`
    /// moves when granularity changes.
    pub cluster_key: String,
    pub bookmark_ids: Vec<String>,
    pub representative_bookmark_id: String,
    pub cohesion: f32,
    /// `None` until the label has been generated, or forever if generation is
    /// disabled. A missing label is not an error.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BookmarkClustersResult {
    pub clusters: Vec<BookmarkCluster>,
    pub unclustered_bookmark_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkTopicsQuery {
    pub root: PathBuf,
    /// When present, discover topics only within this indexed document.
    /// Absence retains the active-root cloud semantics.
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default = "chunk_topic_default_granularity")]
    pub granularity: BookmarkClusterGranularity,
}

fn chunk_topic_default_granularity() -> BookmarkClusterGranularity {
    BookmarkClusterGranularity::MuchFewer
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkTopicMember {
    pub chunk_id: i64,
    pub file_path: PathBuf,
    pub chunk_text: String,
    pub extraction_byte_range: ByteRange,
    pub origin: SourceOrigin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkTopic {
    /// SHA-256 over the sorted member chunk ids. Reindexing naturally changes
    /// the key when membership drifts.
    pub cluster_key: String,
    pub chunks: Vec<ChunkTopicMember>,
    pub representative_chunk_id: i64,
    pub chunk_count: usize,
    pub distinct_document_count: usize,
    pub cohesion: f32,
    /// For a document-scoped topic, the number of other indexed documents
    /// containing at least one passage that meets the topic's own cohesion
    /// boundary. Root-scoped topics leave this absent because their membership
    /// already describes the selected root's distribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_coverage: Option<TopicLibraryCoverage>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TopicLibraryCoverage {
    pub related_document_count: usize,
    /// Other indexed documents across all configured library roots; the source
    /// document is deliberately excluded from both numerator and denominator.
    pub eligible_document_count: usize,
    /// The highest-similarity qualifying passages retained for each related
    /// document, ready to surface through the normal search-results pipeline.
    #[serde(default)]
    pub chunks: Vec<ChunkTopicMember>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChunkTopicsResult {
    pub topics: Vec<ChunkTopic>,
    pub total_chunk_count: usize,
    pub sampled_chunk_count: usize,
    pub total_document_count: usize,
    pub sampled_document_count: usize,
    pub input_cap: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewBookmark {
    pub path: PathBuf,
    pub origin: SourceOrigin,
    #[serde(default)]
    pub text_range: Option<ByteRange>,
    pub quote: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub rects: Vec<BoundingBox>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PreviewData {
    Text {
        content: String,
        language: Option<String>,
        highlight_line: u32,
        highlight_range: ByteRange,
    },
    Pdf {
        page: u32,
        highlight_bbox: Option<BoundingBox>,
    },
}

// ── Embedder model ────────────────────────────────────────────────────────────

/// Identifies an embedding model. For fastembed models this is the Debug representation
/// of the `EmbeddingModel` enum variant (e.g. "BGEBaseENV15"); for SBERT/Candle models
/// it is the HuggingFace model code. Serialises as a plain string.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct EmbedderModel(pub String);

impl EmbedderModel {
    pub fn model_id(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for EmbedderModel {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(EmbedderModel(s))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelectedEmbedder {
    pub engine: EmbeddingEngine,
    pub model: EmbedderModel,
    pub dimension: usize,
}

impl SelectedEmbedder {
    pub fn default_for(engine: EmbeddingEngine) -> Self {
        Self {
            engine,
            model: EmbedderModel(engine.default_model().to_string()),
            dimension: 384,
        }
    }
}

impl Default for SelectedEmbedder {
    fn default() -> Self {
        Self::default_for(EmbeddingEngine::default())
    }
}

// ── Model descriptor (returned by list_models) ────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub model_id: String,
    pub display_name: String,
    pub description: String,
    pub dimension: usize,
    pub is_cached: bool,
    pub is_default: bool,
    pub is_recommended: bool,
    /// Total bytes of all model files. Populated from disk for cached models;
    /// `None` for uncached models until explicitly fetched from HuggingFace.
    pub size_bytes: Option<u64>,
    /// How many texts to embed at once. `None` means process all texts as one batch
    /// (required for some quantized models to ensure consistent results).
    pub preferred_batch_size: Option<usize>,
}

/// Where a model's retrieval prefixes came from, and whether the answer is
/// known at all.
///
/// A prefix is not cosmetic — Underdog measured the same model putting the
/// right answer at rank 52 with its query prefix and rank 1792 without it — so
/// a consumer choosing a model needs to know not only what the prefixes are
/// but whether anything has actually established them.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrefixSource {
    /// Parsed from the model's own auxiliary config, which is authoritative.
    Discovered,
    /// Supplied by the curated table, for models that document their
    /// convention only in the model card.
    Curated,
    /// The artifacts are here, they were read, and they name no prefix: this
    /// model takes none.
    NotDocumented,
    /// The model is not local and nothing here has labelled it, so whether it
    /// needs prefixes cannot be known until it is downloaded.
    Undetermined,
}

/// Everything a consumer can learn about an embedding model *without* loading
/// it, so that the choice of model is made where the models are.
///
/// This is deliberately more than [`ModelDescriptor`]. A descriptor answers
/// "what may I show in a picker"; this answers "what would embedding under
/// this model actually mean" — the dimension, the input recipe, whether the
/// artifacts are here — which is what a caller migrating a corpus has to know
/// before it commits. Every field is either something Wilkes can establish or
/// an explicit absence; nothing here is inferred from a model's name.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmbedderCapability {
    pub engine: EmbeddingEngine,
    /// The id this engine takes back: a catalogue key for fastembed, a
    /// HuggingFace repository id for the engines that load one directly.
    pub model_id: String,
    pub display_name: String,
    pub description: String,
    /// The weights this model is, when the engine names a repository. Absent
    /// where the engine's catalogue does not expose one.
    pub repository_id: Option<String>,
    /// The width of the vectors this model produces.
    ///
    /// `None` is a real answer and not a failure: a model the user added by
    /// hand has no catalogue entry, and its dimension is a property of the
    /// weights that only the first load reveals. A caller must not fill that
    /// in from a picker — it is the one mistake a rebuilt corpus cannot
    /// recover from.
    pub dimension: Option<usize>,
    /// Widths this model may be truncated to. One entry — its own dimension —
    /// until an engine here implements a truncation contract; never a claim
    /// copied from a model card Wilkes cannot honour.
    pub supported_dimensions: Vec<usize>,
    pub query_prefix: Option<String>,
    pub passage_prefix: Option<String>,
    pub prefix_source: PrefixSource,
    /// Longest input the model accepts, when its own config says so.
    pub max_input_tokens: Option<usize>,
    /// Whether the artifacts are on this machine already.
    pub locally_available: bool,
    /// Bytes on disk once installed, where they are known — from disk for a
    /// cached model, and otherwise only after an explicit size fetch.
    pub size_bytes: Option<u64>,
    pub preferred_batch_size: Option<usize>,
    /// True for the entries the engine's own catalogue lists, false for a
    /// model the user added by hand.
    pub catalogued: bool,
}

/// What this Wilkes can embed with, as one answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmbedderCapabilityManifest {
    pub engines: Vec<EmbeddingEngine>,
    /// The input roles the managed surface can produce vectors for. A role is
    /// implied by the endpoint that embeds — `chunks/search` embeds a query,
    /// import and `embed/text` embed passages — so this names what a consumer
    /// can ask for, not a flag it may set.
    pub roles: Vec<String>,
    pub models: Vec<EmbedderCapability>,
}

// ── Generation ────────────────────────────────────────────────────────────────

/// The weights repo id of a generation model. The sibling of `EmbedderModel`.
pub const LEGACY_QUANTIZED_GEMMA_MODEL: &str = "unsloth/gemma-3-1b-it-GGUF";
pub const DENSE_GEMMA_MODEL: &str = "unsloth/gemma-3-1b-it";
pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize)]
pub struct GeneratorModel(pub String);

impl GeneratorModel {
    pub fn model_id(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for GeneratorModel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let model = String::deserialize(deserializer)?;
        Ok(Self(if model == LEGACY_QUANTIZED_GEMMA_MODEL {
            DENSE_GEMMA_MODEL.to_string()
        } else {
            model
        }))
    }
}

/// A backend-neutral catalog entry for a generation model.
///
/// Download artifact details belong to the Candle implementation. Ollama owns
/// its model store, so exposing Hugging Face filenames here would make every
/// non-Candle backend pretend to have artifacts it does not manage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeneratorDescriptor {
    pub engine: crate::generate::GenerationEngine,
    pub model_id: String,
    pub display_name: String,
    pub description: String,
    pub context_tokens: usize,
    pub is_cached: bool,
    pub is_default: bool,
    pub is_recommended: bool,
    pub size_bytes: Option<u64>,
}

/// Sibling of `SemanticSettings`. Deliberately not folded into it: the two
/// subsystems are independently toggleable, and coupling them would make
/// enabling semantic search silently enable a second resident model.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GenerationSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Missing in settings written before multiple generation backends existed.
    #[serde(default)]
    pub engine: crate::generate::GenerationEngine,
    #[serde(default)]
    pub model: Option<GeneratorModel>,
    /// "auto", "cpu", "metal". Absent falls back to the engine default.
    #[serde(default)]
    pub device: Option<String>,
    /// Base URL of the Ollama HTTP API. Kept even while Candle is selected so
    /// switching back restores the user's endpoint.
    #[serde(default = "default_ollama_url")]
    pub ollama_url: String,
    /// Explicit Ollama context window. `None` selects the model-reported
    /// maximum; a smaller value lets users trade prompt capacity for KV-cache
    /// memory without relying on Ollama's small implicit default.
    #[serde(default)]
    pub context_tokens: Option<usize>,
    /// Per-task sampling overrides. Absent entries use the task default; the
    /// settings exist for tuning, not as a step the user must take.
    #[serde(default)]
    pub sampling_overrides: HashMap<GenerationTask, crate::generate::Sampling>,
}

/// Enrichment of the pictures inside a document.
///
/// Off by default, and deliberately not a quiet default-on: turning it on
/// installs a recognizer, and it changes the extraction recipe, which re-reads
/// and re-embeds every document that has a picture in it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAnalysisSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Which recognizer reads the pictures.
    ///
    /// Defaulted, and the default is the ONNX engine — which means a settings
    /// file written before this field existed would silently change
    /// recognizer, change the extraction recipe, and re-read every document
    /// with a picture in it. `migrate_recognizer_choice` exists to stop that;
    /// see its note.
    #[serde(default)]
    pub engine: crate::extract::image::dispatch::RecognitionEngine,
    /// The recognizer's model id. Absent takes the engine's default.
    #[serde(default)]
    pub model: Option<String>,
    /// "auto", "cpu", "metal". Absent takes the recognizer's default.
    #[serde(default)]
    pub device: Option<String>,
    /// The Ollama tag figures are described with, or empty for transcription
    /// only. The server is [`GenerationSettings::ollama_url`]: there is one
    /// Ollama endpoint per app, and a second field for the same server would
    /// be a second answer to where it is.
    ///
    /// A description is a separate fact from a transcription, produced by a
    /// separate model, and it is optional in a way the transcription is not.
    #[serde(default)]
    pub describer_model: String,
}

/// Tasks whose sampling the user may override.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationTask {
    ClusterLabel,
    RelationExplanation,
    DocumentSummary,
    SearchResultsSummary,
    HypotheticalContinuation,
    GroundedCompletion,
}

/// One event protocol for every user-facing token stream. Task inputs and
/// validation stay task-specific; correlation and lifecycle do not.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum GenerationStreamEvent {
    Delta {
        request_id: String,
        task: GenerationTask,
        delta: String,
    },
    Completed {
        request_id: String,
        task: GenerationTask,
        text: String,
    },
    Failed {
        request_id: String,
        task: GenerationTask,
        error: String,
    },
}

impl Default for GenerationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: crate::generate::GenerationEngine::default(),
            model: None,
            device: None,
            ollama_url: default_ollama_url(),
            context_tokens: None,
            sampling_overrides: HashMap::new(),
        }
    }
}

fn default_ollama_url() -> String {
    DEFAULT_OLLAMA_URL.to_string()
}

// ── Embedding engine ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum EmbeddingEngine {
    #[serde(alias = "sbert")]
    SBERT,
    #[serde(alias = "candle")]
    Candle,
    #[default]
    #[serde(alias = "fastembed")]
    Fastembed,
}

impl EmbeddingEngine {
    pub fn as_str(&self) -> &'static str {
        match self {
            EmbeddingEngine::SBERT => "sbert",
            EmbeddingEngine::Candle => "candle",
            EmbeddingEngine::Fastembed => "fastembed",
        }
    }

    /// Default device string for this engine. Used when no explicit override is set.
    pub fn default_device(&self) -> &'static str {
        match self {
            EmbeddingEngine::SBERT => "auto",
            EmbeddingEngine::Candle => "auto",
            EmbeddingEngine::Fastembed => "cpu",
        }
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            EmbeddingEngine::SBERT => "intfloat/e5-small-v2",
            EmbeddingEngine::Candle => "sentence-transformers/all-MiniLM-L6-v2",
            EmbeddingEngine::Fastembed => "AllMiniLML6V2",
        }
    }

    pub fn supports_custom_models(&self) -> bool {
        match self {
            EmbeddingEngine::SBERT => true,
            EmbeddingEngine::Candle => true,
            EmbeddingEngine::Fastembed => false,
        }
    }

    pub fn supported_engines() -> Vec<Self> {
        let mut engines = vec![EmbeddingEngine::SBERT];
        #[cfg(feature = "candle")]
        engines.push(EmbeddingEngine::Candle);
        #[cfg(feature = "fastembed")]
        engines.push(EmbeddingEngine::Fastembed);
        engines
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CustomModel {
    pub engine: EmbeddingEngine,
    pub model_id: String,
}

// ── Recognizer inventory ──────────────────────────────────────────────────────

/// One file of a recognizer, as an inventory names it.
#[derive(Debug, Clone, Serialize)]
pub struct InventoriedArtifact {
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: String,
}

/// What a recognizer is, where it came from, and under what terms.
///
/// FIGURE.md requires this of a redistributed checkpoint before it is
/// packaged, and it is data rather than prose for the reason the pins
/// themselves are: an inventory kept in a comment is one nobody can check.
/// Every file the install writes appears here with the digest it is verified
/// against, so the inventory describes the bytes on disk and not a
/// recollection of them.
///
/// Wilkes fetches these artifacts at the user's request rather than shipping
/// them inside the application, which is why the inventory is shown where the
/// download is offered: the terms are disclosed before the bytes arrive, not
/// after.
///
/// Lives here rather than in a model module because the desktop, server and
/// MCP surfaces all name it. A type three API boundaries carry should not be
/// owned by whichever recognizer happened to need it first.
#[derive(Debug, Clone, Serialize)]
pub struct RecognizerInventory {
    pub name: String,
    pub repo: String,
    pub revision: String,
    pub license: String,
    pub license_url: String,
    pub derived_from: Vec<String>,
    pub artifacts: Vec<InventoriedArtifact>,
    pub footprint_bytes: u64,
}

// ── Semantic settings ─────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize)]
pub struct SemanticSettings {
    pub enabled: bool,
    #[serde(default = "SemanticSettings::default_selected")]
    pub selected: SelectedEmbedder,
    /// Per-engine device overrides ("auto", "cpu", "mps", "cuda").
    /// Missing entries fall back to each engine's own default_device().
    #[serde(default)]
    pub engine_devices: HashMap<EmbeddingEngine, String>,
    pub index_path: Option<PathBuf>,
    /// List of arbitrary HuggingFace IDs manually added by the user, scoped by engine.
    #[serde(default, deserialize_with = "deserialize_custom_models")]
    pub custom_models: Vec<CustomModel>,
    #[serde(default = "SemanticSettings::default_chunk_size")]
    pub chunk_size: usize,
    #[serde(default = "SemanticSettings::default_chunk_overlap")]
    pub chunk_overlap: usize,
    /// Maximum number of indexed chunks admitted to the flat Ward topic pass.
    /// This is the resource control for the O(n²) clustering work; topic
    /// granularity only changes the requested cut.
    #[serde(default = "SemanticSettings::default_topic_cloud_input_cap")]
    pub topic_cloud_input_cap: usize,
    /// Idle timeout for worker processes in seconds.
    #[serde(default = "SemanticSettings::default_worker_timeout")]
    pub worker_timeout_secs: u64,
}

#[derive(Deserialize)]
struct SemanticSettingsSerde {
    enabled: bool,
    #[serde(default = "SemanticSettings::default_selected")]
    selected: SelectedEmbedder,
    #[serde(default)]
    engine_devices: HashMap<EmbeddingEngine, String>,
    index_path: Option<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_custom_models")]
    custom_models: Vec<CustomModel>,
    #[serde(default = "SemanticSettings::default_chunk_size")]
    chunk_size: usize,
    #[serde(default = "SemanticSettings::default_chunk_overlap")]
    chunk_overlap: usize,
    #[serde(default = "SemanticSettings::default_topic_cloud_input_cap")]
    topic_cloud_input_cap: usize,
    #[serde(default = "SemanticSettings::default_worker_timeout")]
    worker_timeout_secs: u64,
}

#[derive(Deserialize)]
struct LegacySemanticSettingsSerde {
    enabled: bool,
    #[serde(default)]
    engine: EmbeddingEngine,
    #[serde(default = "SemanticSettings::default_model")]
    model: EmbedderModel,
    #[serde(default = "SemanticSettings::default_dimension")]
    dimension: usize,
    #[serde(default)]
    engine_devices: HashMap<EmbeddingEngine, String>,
    index_path: Option<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_custom_models")]
    custom_models: Vec<CustomModel>,
    #[serde(default = "SemanticSettings::default_chunk_size")]
    chunk_size: usize,
    #[serde(default = "SemanticSettings::default_chunk_overlap")]
    chunk_overlap: usize,
    #[serde(default = "SemanticSettings::default_topic_cloud_input_cap")]
    topic_cloud_input_cap: usize,
    #[serde(default = "SemanticSettings::default_worker_timeout")]
    worker_timeout_secs: u64,
}

fn deserialize_custom_models<'de, D>(deserializer: D) -> Result<Vec<CustomModel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;

    if let Some(arr) = value.as_array() {
        let mut result = Vec::new();
        for item in arr {
            if let Some(s) = item.as_str() {
                // Migration: old Vec<String> format. Default to SBERT.
                result.push(CustomModel {
                    engine: EmbeddingEngine::SBERT,
                    model_id: s.to_string(),
                });
            } else if let Ok(custom) = serde_json::from_value::<CustomModel>(item.clone()) {
                result.push(custom);
            } else {
                return Err(D::Error::custom("Invalid custom_model format"));
            }
        }
        Ok(result)
    } else {
        Ok(Vec::new())
    }
}

impl SemanticSettings {
    fn default_selected() -> SelectedEmbedder {
        SelectedEmbedder::default_for(EmbeddingEngine::default())
    }

    fn default_model() -> EmbedderModel {
        EmbedderModel(EmbeddingEngine::default().default_model().to_string())
    }

    fn default_chunk_size() -> usize {
        600
    }

    fn default_chunk_overlap() -> usize {
        128
    }

    pub const fn default_topic_cloud_input_cap() -> usize {
        1_500
    }

    fn default_worker_timeout() -> u64 {
        crate::worker::DEFAULT_IDLE_TIMEOUT_SECS
    }

    fn default_dimension() -> usize {
        384 // Default for AllMiniLML6V2
    }

    /// Returns the effective device string for the given engine,
    /// falling back to that engine's built-in default when no override is set.
    pub fn device_for(&self, engine: EmbeddingEngine) -> &str {
        self.engine_devices
            .get(&engine)
            .map(String::as_str)
            .unwrap_or_else(|| engine.default_device())
    }
}

impl<'de> Deserialize<'de> for SemanticSettings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("selected").is_some() {
            let parsed = serde_json::from_value::<SemanticSettingsSerde>(value)
                .map_err(serde::de::Error::custom)?;
            Ok(Self {
                enabled: parsed.enabled,
                selected: parsed.selected,
                engine_devices: parsed.engine_devices,
                index_path: parsed.index_path,
                custom_models: parsed.custom_models,
                chunk_size: parsed.chunk_size,
                chunk_overlap: parsed.chunk_overlap,
                topic_cloud_input_cap: parsed.topic_cloud_input_cap,
                worker_timeout_secs: parsed.worker_timeout_secs,
            })
        } else {
            let parsed = serde_json::from_value::<LegacySemanticSettingsSerde>(value)
                .map_err(serde::de::Error::custom)?;
            Ok(Self {
                enabled: parsed.enabled,
                selected: SelectedEmbedder {
                    engine: parsed.engine,
                    model: parsed.model,
                    dimension: parsed.dimension,
                },
                engine_devices: parsed.engine_devices,
                index_path: parsed.index_path,
                custom_models: parsed.custom_models,
                chunk_size: parsed.chunk_size,
                chunk_overlap: parsed.chunk_overlap,
                topic_cloud_input_cap: parsed.topic_cloud_input_cap,
                worker_timeout_secs: parsed.worker_timeout_secs,
            })
        }
    }
}

impl Default for SemanticSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            selected: Self::default_selected(),
            engine_devices: HashMap::new(),
            index_path: None,
            custom_models: Vec::new(),
            chunk_size: Self::default_chunk_size(),
            chunk_overlap: Self::default_chunk_overlap(),
            topic_cloud_input_cap: Self::default_topic_cloud_input_cap(),
            worker_timeout_secs: Self::default_worker_timeout(),
        }
    }
}

// ── Retrieval query enhancement ───────────────────────────────────────────────

/// Techniques that reshape the *query vector* before the nearest-neighbour
/// lookup. Both are optional and off by default. Neither adds a ranking stage
/// after retrieval: search relevance stays owned by the vector index. They only
/// change where in the latent space the query lands before that lookup happens.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct RetrievalSettings {
    /// HyDE: search with the embedding of an LLM-generated hypothetical answer,
    /// which sits in document space rather than terse-question space.
    #[serde(default)]
    pub hyde: HydeSettings,
    /// Pseudo-relevance feedback (Rocchio): fold the centroid of the top initial
    /// hits back into the query vector and retrieve a second time.
    #[serde(default)]
    pub pseudo_relevance_feedback: PrfSettings,
}

/// Hypothetical Document Embeddings (Gao et al., 2022).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HydeSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Number of hypothetical documents generated and averaged together. More
    /// broadens topical coverage at a linear generation-latency cost.
    #[serde(default = "HydeSettings::default_hypotheticals")]
    pub hypotheticals: usize,
    /// Keep the original query vector in the average. When false, retrieval
    /// relies solely on the generated hypotheticals.
    #[serde(default = "default_true")]
    pub include_query: bool,
}

impl HydeSettings {
    pub const fn default_hypotheticals() -> usize {
        1
    }
}

impl Default for HydeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            hypotheticals: Self::default_hypotheticals(),
            include_query: true,
        }
    }
}

/// Pseudo-relevance feedback via the Rocchio update `q' = α·q + β·centroid`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PrfSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Number of top initial hits treated as pseudo-relevant feedback.
    #[serde(default = "PrfSettings::default_feedback_docs")]
    pub feedback_docs: usize,
    /// Weight on the original query vector.
    #[serde(default = "PrfSettings::default_alpha")]
    pub alpha: f32,
    /// Weight on the feedback centroid.
    #[serde(default = "PrfSettings::default_beta")]
    pub beta: f32,
}

impl PrfSettings {
    pub const fn default_feedback_docs() -> usize {
        5
    }
    pub fn default_alpha() -> f32 {
        1.0
    }
    pub fn default_beta() -> f32 {
        0.5
    }
}

impl Default for PrfSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            feedback_docs: Self::default_feedback_docs(),
            alpha: Self::default_alpha(),
            beta: Self::default_beta(),
        }
    }
}

// ── Index status ──────────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexStatus {
    pub indexed_files: usize,
    pub total_chunks: usize,
    pub built_at: Option<u64>,
    pub build_duration_ms: Option<u64>,
    pub engine: EmbeddingEngine,
    pub model_id: String,
    pub dimension: usize,
    pub root_path: Option<std::path::PathBuf>,
    pub db_size_bytes: Option<u64>,
}

// ── Settings ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalMcpSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub require_token: bool,
    #[serde(default = "default_external_mcp_bind_address")]
    pub bind_address: std::net::IpAddr,
    #[serde(default = "default_external_mcp_port")]
    pub port: u16,
}

fn default_external_mcp_bind_address() -> std::net::IpAddr {
    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

fn default_external_mcp_port() -> u16 {
    39_217
}

impl Default for ExternalMcpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            require_token: false,
            bind_address: default_external_mcp_bind_address(),
            port: default_external_mcp_port(),
        }
    }
}

/// The Wilkes HTTP API, served by the desktop app over the workspace it
/// already has open.
///
/// It exists so that another program on this machine does not have to open the
/// workspace itself to read from it. A Wilkes workspace has one owner — the
/// process holding its databases — and a second opener races it for
/// `settings.json` and the semantic index. With this on, the app *is* the
/// owner and everything else asks it over HTTP.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpApiSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_http_api_bind_address")]
    pub bind_address: std::net::IpAddr,
    #[serde(default = "default_http_api_port")]
    pub port: u16,
}

fn default_http_api_bind_address() -> std::net::IpAddr {
    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

/// Not `wilkes-server`'s 2000: the two are different owners of the same
/// workspace, and a shared default would have them fight for the port on the
/// one machine where both might be started.
fn default_http_api_port() -> u16 {
    2020
}

impl Default for HttpApiSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: default_http_api_bind_address(),
            port: default_http_api_port(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, alias = "bookmarked_dirs")]
    pub favorites: Vec<PathBuf>,
    #[serde(default)]
    pub recent_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub last_directory: Option<PathBuf>,
    pub respect_gitignore: bool,
    pub max_file_size: u64,
    pub context_lines: usize,
    pub theme: Theme,
    #[serde(default)]
    pub search_prefer_semantic: bool,
    /// When enabled, exact (grep) search reads a PDF's text from the semantic
    /// index instead of re-extracting it, falling back to live extraction only
    /// for files the index does not yet hold. Off by default.
    #[serde(default)]
    pub grep_use_index: bool,
    #[serde(default)]
    pub semantic: SemanticSettings,
    #[serde(default)]
    pub integrations: IntegrationsSettings,
    #[serde(default)]
    pub primary_metadata_source: MetadataSourcePreference,
    #[serde(default = "default_supported_extensions")]
    pub supported_extensions: Vec<String>,
    #[serde(default)]
    pub max_results: usize,
    #[serde(default)]
    pub bookmarks_dock: BookmarkDock,
    #[serde(default)]
    pub file_sort_key: FileSortKey,
    #[serde(default)]
    pub file_sort_direction: FileSortDirection,
    #[serde(default = "default_file_display_fields")]
    pub file_display_fields: Vec<FileDisplayField>,
    /// Desired CSS-pixel height for body text when a PDF is auto-zoomed.
    #[serde(default = "default_pdf_auto_zoom_target_px")]
    pub pdf_auto_zoom_target_px: f64,
    /// Preferred agent backend for the "Ask the documents" chat pane. The
    /// in-pane selector and header dropdown may switch a session to a
    /// different backend transiently, but this Settings field is the single
    /// persisted default (see docs/chat-agent-integration-spec.md §7.10).
    #[serde(default, deserialize_with = "deserialize_chat_backend_setting")]
    pub chat_backend: AgentBackend,
    /// Per-backend chat config defaults applied to newly started sessions.
    /// Written whenever the user changes a config option in the chat pane, so
    /// each backend restores its last model/thought level/mode on a new chat
    /// (see docs/chat-agent-integration-spec.md §7.10). Distinct from a
    /// conversation's own `config_values` snapshot, which restores *that*
    /// conversation on reopen.
    #[serde(default)]
    pub chat_config: Vec<ChatBackendConfig>,
    /// User-authored instructions prepended to every chat turn. Kept in the
    /// global settings (rather than a conversation record) so an edit applies
    /// consistently to new and existing conversations.
    #[serde(default)]
    pub chat_custom_instructions: String,
    /// Optional MCP endpoint for regular Claude Code and Codex clients.
    /// Authentication material, when enabled, is stored separately in app data
    /// and is intentionally never serialized as part of Settings.
    #[serde(default)]
    pub external_mcp: ExternalMcpSettings,
    /// The read/write HTTP API, off by default. Unauthenticated like
    /// `wilkes-server` itself, which is why it binds loopback by default.
    #[serde(default)]
    pub http_api: HttpApiSettings,
    /// Local text generation. Off by default; every affordance that depends on
    /// it is invisible until it is both enabled and ready.
    #[serde(default)]
    pub generation: GenerationSettings,
    /// Query-vector enhancement for semantic search (HyDE, pseudo-relevance
    /// feedback). Off by default.
    #[serde(default)]
    pub retrieval: RetrievalSettings,
    /// Transcription and description of the pictures inside documents. Off by
    /// default.
    #[serde(default)]
    pub image_analysis: ImageAnalysisSettings,
}

fn default_file_display_fields() -> Vec<FileDisplayField> {
    vec![FileDisplayField::Size]
}

fn default_pdf_auto_zoom_target_px() -> f64 {
    15.5
}

fn default_supported_extensions() -> Vec<String> {
    vec![
        "txt", "md", "json", "xml", "html", "htm", "log", "csv", "jsonl", "pdf",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            favorites: Vec::new(),
            recent_dirs: Vec::new(),
            last_directory: None,
            respect_gitignore: true,
            max_file_size: 10 * 1024 * 1024,
            context_lines: 2,
            theme: Theme::default(),
            search_prefer_semantic: false,
            grep_use_index: false,
            semantic: SemanticSettings::default(),
            integrations: IntegrationsSettings::default(),
            primary_metadata_source: MetadataSourcePreference::default(),
            supported_extensions: default_supported_extensions(),
            max_results: 50,
            bookmarks_dock: BookmarkDock::default(),
            file_sort_key: FileSortKey::default(),
            file_sort_direction: FileSortDirection::default(),
            file_display_fields: default_file_display_fields(),
            pdf_auto_zoom_target_px: default_pdf_auto_zoom_target_px(),
            chat_backend: AgentBackend::default(),
            chat_config: Vec::new(),
            chat_custom_instructions: String::new(),
            external_mcp: ExternalMcpSettings::default(),
            http_api: HttpApiSettings::default(),
            generation: GenerationSettings::default(),
            retrieval: RetrievalSettings::default(),
            image_analysis: ImageAnalysisSettings::default(),
        }
    }
}

fn deserialize_chat_backend_setting<'de, D>(deserializer: D) -> Result<AgentBackend, D::Error>
where
    D: Deserializer<'de>,
{
    match String::deserialize(deserializer)?.as_str() {
        "ClaudeCode" => Ok(AgentBackend::ClaudeCode),
        "Codex" => Ok(AgentBackend::Codex),
        "Nanocoder" => Ok(AgentBackend::Nanocoder),
        // Migration-only: Gemini support was removed, but older settings files
        // may still contain this persisted preference.
        "Gemini" => Ok(AgentBackend::default()),
        value => Err(de::Error::unknown_variant(
            value,
            &["ClaudeCode", "Codex", "Nanocoder"],
        )),
    }
}

/// Which CLI the "Ask the documents" chat pane drives over ACP. The launch
/// command is the only per-backend difference (see `wilkes_agent::launch_spec`);
/// everything else -- context injection, permission boundary, transport -- is
/// shared (docs/chat-agent-integration-spec.md §5).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq, Hash)]
pub enum AgentBackend {
    #[default]
    ClaudeCode,
    Codex,
    Nanocoder,
}

/// One resolved ACP session config selection (e.g. `model` = `sonnet`). The
/// canonical shape for persisting config, shared by the per-conversation
/// snapshot and the per-backend default below.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatConfigValue {
    pub id: String,
    pub value: String,
}

/// The chat config (model, thought level, mode, ...) last chosen for a given
/// backend, persisted so a *new* chat with that backend restores it instead of
/// falling back to the agent's own defaults.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatBackendConfig {
    pub backend: AgentBackend,
    pub values: Vec<ChatConfigValue>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileSortKey {
    #[default]
    Filename,
    Title,
    Author,
    Created,
    Modified,
    Size,
    Publication,
    Citations,
}

/// Optional document-metadata field that can be shown as a column in the file
/// list. `Settings::file_display_fields` holds the set currently visible.
/// Extend with new variants as more metadata is projected onto `FileEntry`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileDisplayField {
    Title,
    Author,
    Created,
    Modified,
    Publication,
    Citations,
    Size,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetadataSourcePreference {
    File,
    #[default]
    Zotero,
    SemanticScholar,
    OpenAlex,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileSortDirection {
    #[default]
    Asc,
    Desc,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum BookmarkDock {
    Left,
    #[default]
    Right,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct IntegrationsSettings {
    #[serde(default)]
    pub zotero: ZoteroSettings,
    #[serde(default)]
    pub semantic_scholar: SemanticScholarSettings,
    #[serde(default)]
    pub openalex: OpenAlexSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZoteroSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_zotero_base_url")]
    pub base_url: String,
    #[serde(default = "default_zotero_citation_style")]
    pub citation_style: String,
}

impl Default for ZoteroSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_zotero_base_url(),
            citation_style: default_zotero_citation_style(),
        }
    }
}

fn default_zotero_base_url() -> String {
    "http://127.0.0.1:23119".to_string()
}

fn default_zotero_citation_style() -> String {
    "chicago-note-bibliography".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticScholarSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_semantic_scholar_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

impl Default for SemanticScholarSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_semantic_scholar_base_url(),
            api_key: None,
        }
    }
}

fn default_semantic_scholar_base_url() -> String {
    "https://api.semanticscholar.org".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAlexSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_openalex_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub email: Option<String>,
}

impl Default for OpenAlexSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_openalex_base_url(),
            email: None,
        }
    }
}

fn default_openalex_base_url() -> String {
    "https://api.openalex.org".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationState {
    Disabled,
    ZoteroDown,
    LocalApiDisabled,
    RemoteApiDown,
    RateLimited,
    Ready,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntegrationStatus {
    pub id: String,
    pub enabled: bool,
    pub state: IntegrationState,
    pub message: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum AddOutcome {
    Added { item_key: Option<String> },
    AlreadyPresent { item_key: String },
    PossibleDuplicate { item_key: String, message: String },
}

/// CSL citation strings for a resolved Zotero item. `citation` is the in-text
/// form and `bibliography` the full reference; both are HTML produced by Zotero.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CitationResult {
    pub citation: Option<String>,
    pub bibliography: Option<String>,
    /// True when the item was resolved by a weak signal (filename or title),
    /// so the citation may belong to the wrong work.
    pub low_confidence: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticScholarPaper {
    pub doi: String,
    pub paper_id: String,
    pub title: Option<String>,
    pub year: Option<i64>,
    pub publication_date: Option<String>,
    pub venue: Option<String>,
    pub citation_count: i64,
    pub external_ids: HashMap<String, serde_json::Value>,
    pub cached_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAlexWork {
    pub doi: String,
    pub work_id: String,
    pub title: Option<String>,
    pub year: Option<i64>,
    pub publication_date: Option<String>,
    pub venue: Option<String>,
    pub citation_count: i64,
    pub external_ids: HashMap<String, serde_json::Value>,
    pub cached_at_ms: i64,
}

/// Provider-neutral result returned by external literature searches.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiteratureSearchResult {
    pub id: String,
    pub doi: Option<String>,
    pub title: Option<String>,
    pub year: Option<i64>,
    pub publication_date: Option<String>,
    pub venue: Option<String>,
    pub citation_count: i64,
    pub is_open_access: bool,
    pub pdf_url: Option<String>,
    pub landing_page_url: Option<String>,
    pub open_access_status: Option<String>,
    pub license: Option<String>,
}

/// What kind of source a catalogue record is, which decides what it can
/// answer. A gap in an API's behaviour is answered by `Reference` and never
/// well by `Textbook`; a subject with no ordering yet is answered by `Course`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CatalogueGrain {
    /// A concept built up from its prerequisites: an open textbook.
    Textbook,
    /// A subject with a sequence — a syllabus, lecture notes, a course.
    Course,
    /// An API, a language construct, a standard. Authoritative documentation,
    /// where no textbook chapter is the right answer.
    Reference,
}

impl CatalogueGrain {
    pub fn as_str(self) -> &'static str {
        match self {
            CatalogueGrain::Textbook => "textbook",
            CatalogueGrain::Course => "course",
            CatalogueGrain::Reference => "reference",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "textbook" => Some(CatalogueGrain::Textbook),
            "course" => Some(CatalogueGrain::Course),
            "reference" => Some(CatalogueGrain::Reference),
            _ => None,
        }
    }
}

/// Provider-neutral record for one acquirable teaching resource.
///
/// Deliberately not [`LiteratureSearchResult`]: that type is shaped by what a
/// bibliographic index knows about a *paper* — DOI, venue, citation count —
/// and a textbook has none of those while having a description, a subject and
/// a licence that decide whether it may be kept. Neither substitutes for the
/// other, so they do not share a struct.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CatalogueRecord {
    pub provider: String,
    pub external_id: String,
    pub title: String,
    /// The blurb. This is what a caller's probe is matched against, so a
    /// record without one is close to unfindable.
    pub summary: String,
    pub subject: String,
    pub authors: String,
    pub license: String,
    pub landing_url: Option<String>,
    /// Present only where the provider serves the whole work at a stable URL.
    /// Its absence is why admission stays a separate decision from discovery.
    pub pdf_url: Option<String>,
    pub outline_url: Option<String>,
    pub grain: CatalogueGrain,
    pub pages: Option<i64>,
}

/// One catalogue record matched against a text query, with the *recall* score
/// that surfaced it.
///
/// The score is BM25 over title, subject and summary, and is explicitly not a
/// ranking a caller should consume. It exists to cut thousands of records down
/// to a few dozen that the caller can then rank properly against something
/// Wilkes does not know about — which learner is asking, and what they already
/// know. Wilkes cannot answer that and does not pretend to.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueHit {
    #[serde(flatten)]
    pub record: CatalogueRecord,
    pub recall_score: f64,
}

/// Per-provider state of the local catalogue mirror.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueProviderStatus {
    pub provider: String,
    pub grain: CatalogueGrain,
    pub records: i64,
    pub synced_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

// ── File listing ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub file_type: FileType,
    pub extension: String,
    pub created_at_ms: Option<i64>,
    pub modified_at_ms: Option<i64>,
    /// Document title from cached extracted metadata. `None` until the metadata
    /// cache has processed this file.
    #[serde(default)]
    pub title: Option<String>,
    /// Document author from cached extracted metadata. `None` until the metadata
    /// cache has processed this file.
    #[serde(default)]
    pub author: Option<String>,
    /// Document DOI from cached extracted metadata (normalized, no URL prefix).
    /// `None` until the metadata cache has processed this file or when the
    /// document carries no DOI.
    #[serde(default)]
    pub doi: Option<String>,
    /// Document publication date ("YYYY-MM") from cached extracted metadata.
    /// `None` until the metadata cache has processed this file.
    #[serde(default)]
    pub publication_date: Option<String>,
    /// Semantic Scholar citation count from cached document metadata. `None`
    /// until metadata extraction has found a DOI and the integration has
    /// enriched it.
    #[serde(default)]
    pub citation_count: Option<i64>,
    #[serde(default)]
    pub metadata_conflicts: HashMap<String, Vec<MetadataConflictValue>>,
    #[serde(default)]
    pub tags: Vec<Tag>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MetadataConflictValue {
    pub source: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileListResponse {
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub omitted: Vec<OmittedFileEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OmittedFileEntry {
    #[serde(flatten)]
    pub file: FileEntry,
    pub reason: OmittedFileReason,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum OmittedFileReason {
    TooLarge,
    UnsupportedExtension,
}

// ── Paths ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DataPaths {
    pub app_data: String,
}

// ── Capabilities ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchCapabilities {
    pub supports_regex: bool,
    pub supports_case_sensitivity: bool,
    pub is_indexed: bool,
    pub supported_file_types: Vec<String>,
    /// True if this provider requires a pre-built index.
    #[serde(default)]
    pub requires_index: bool,
    /// True if the semantic index has been built and is ready.
    #[serde(default)]
    pub semantic_index_built: bool,
    /// List of embedding engines compiled into the app.
    #[serde(default)]
    pub supported_engines: Vec<EmbeddingEngine>,
}

// ── Search completion stats ───────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SearchStats {
    /// Number of supported files whose contents were actually searched. This
    /// includes files that produced no matches and excludes policy-filtered
    /// files that were never opened.
    pub files_scanned: usize,
    pub total_matches: usize,
    /// Time spent enumerating and filtering documents before the search worker
    /// starts. Kept separate from `elapsed_ms`, which remains worker time.
    #[serde(default)]
    pub catalog_elapsed_ms: u64,
    pub elapsed_ms: u64,
    /// PDFs served from stored semantic-index text during an exact search.
    #[serde(default)]
    pub indexed_pdf_reads: usize,
    /// PDFs extracted live because indexed text was disabled or unavailable.
    #[serde(default)]
    pub live_pdf_fallbacks: usize,
    /// Live PDF fallbacks caused by an enabled but non-resident index.
    #[serde(default)]
    pub index_unavailable_fallbacks: usize,
    #[serde(default)]
    pub errors: Vec<String>,
    /// Exact generated passages whose embeddings contributed to the final
    /// semantic query vector. Empty for grep, disabled HyDE, or degradation to
    /// the raw query.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hyde_documents: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn bookmark_cluster_query_defaults_to_balanced_granularity() {
        let query: BookmarkClustersQuery =
            serde_json::from_str(r#"{"bookmark_ids":["bookmark-1"]}"#).unwrap();
        assert_eq!(query.granularity, BookmarkClusterGranularity::Balanced);
        assert_eq!(
            serde_json::to_value(BookmarkClusterGranularity::MuchMore).unwrap(),
            serde_json::json!("much_more")
        );
    }

    #[test]
    fn chunk_topic_query_defaults_to_minimal_granularity() {
        let query: ChunkTopicsQuery = serde_json::from_str(r#"{"root":"/library"}"#).unwrap();
        assert_eq!(query.granularity, BookmarkClusterGranularity::MuchFewer);
        assert!(query.path.is_none());
        assert_eq!(
            SemanticSettings::default().topic_cloud_input_cap,
            SemanticSettings::default_topic_cloud_input_cap()
        );
    }

    #[test]
    fn test_file_type_detect() {
        let extensions = vec!["txt".to_string(), "pdf".to_string()];

        assert_eq!(
            FileType::detect(Path::new("test.txt"), &extensions),
            Some(FileType::PlainText)
        );
        assert_eq!(
            FileType::detect(Path::new("test.pdf"), &extensions),
            Some(FileType::Pdf)
        );
        assert_eq!(
            FileType::detect(Path::new("Makefile"), &extensions),
            Some(FileType::PlainText)
        );
        assert_eq!(FileType::detect(Path::new("test.exe"), &extensions), None);
    }

    #[test]
    fn test_bounding_box_merge() {
        let b1 = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let b2 = BoundingBox {
            x: 5.0,
            y: 5.0,
            width: 10.0,
            height: 10.0,
        };
        let merged = b1.merge(&b2);

        assert_eq!(merged.x, 0.0);
        assert_eq!(merged.y, 0.0);
        assert_eq!(merged.width, 15.0);
        assert_eq!(merged.height, 15.0);
    }

    #[test]
    fn test_source_map_resolve() {
        let map = SourceMap {
            segments: vec![
                SourceSegment {
                    text_range: ByteRange { start: 0, end: 10 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                    provenance: Default::default(),
                },
                SourceSegment {
                    text_range: ByteRange { start: 10, end: 20 },
                    origin: SourceOrigin::TextFile { line: 2, col: 1 },
                    provenance: Default::default(),
                },
            ],
        };

        match map.resolve(5).unwrap() {
            SourceOrigin::TextFile { line, .. } => assert_eq!(line, 1),
            _ => panic!("Expected TextFile origin"),
        }

        match map.resolve(15).unwrap() {
            SourceOrigin::TextFile { line, .. } => assert_eq!(line, 2),
            _ => panic!("Expected TextFile origin"),
        }
    }

    #[test]
    fn test_source_map_resolve_range_pdf() {
        let map = SourceMap {
            segments: vec![
                SourceSegment {
                    text_range: ByteRange { start: 0, end: 10 },
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: Some(BoundingBox {
                            x: 0.0,
                            y: 0.0,
                            width: 10.0,
                            height: 10.0,
                        }),
                    },
                    provenance: Default::default(),
                },
                SourceSegment {
                    text_range: ByteRange { start: 10, end: 20 },
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: Some(BoundingBox {
                            x: 5.0,
                            y: 5.0,
                            width: 10.0,
                            height: 10.0,
                        }),
                    },
                    provenance: Default::default(),
                },
            ],
        };

        let origin = map.resolve_range(ByteRange { start: 5, end: 15 }).unwrap();
        match origin {
            SourceOrigin::PdfPage { page, bbox } => {
                assert_eq!(page, 1);
                let b = bbox.unwrap();
                assert_eq!(b.x, 0.0);
                assert_eq!(b.y, 0.0);
                assert_eq!(b.width, 15.0);
                assert_eq!(b.height, 15.0);
            }
            _ => panic!("Expected PdfPage origin"),
        }
    }

    #[test]
    fn test_embedding_engine_methods() {
        assert_eq!(EmbeddingEngine::SBERT.as_str(), "sbert");
        assert_eq!(EmbeddingEngine::Candle.as_str(), "candle");
        assert_eq!(EmbeddingEngine::Fastembed.as_str(), "fastembed");

        assert_eq!(EmbeddingEngine::SBERT.default_device(), "auto");
        assert_eq!(EmbeddingEngine::Candle.default_device(), "auto");
        assert_eq!(EmbeddingEngine::Fastembed.default_device(), "cpu");

        assert!(EmbeddingEngine::SBERT.supports_custom_models());
        assert!(EmbeddingEngine::Candle.supports_custom_models());
        assert!(!EmbeddingEngine::Fastembed.supports_custom_models());
    }

    #[test]
    fn test_semantic_settings_defaults() {
        let settings = SemanticSettings::default();
        assert_eq!(settings.enabled, false);
        assert_eq!(settings.selected.engine, EmbeddingEngine::default());
        assert_eq!(settings.selected.model.model_id(), "AllMiniLML6V2");
        assert_eq!(settings.selected.dimension, 384);
        assert_eq!(settings.chunk_size, 600);
        assert_eq!(settings.chunk_overlap, 128);
        assert_eq!(settings.worker_timeout_secs, 300);

        assert_eq!(settings.device_for(EmbeddingEngine::SBERT), "auto");

        let mut settings = SemanticSettings::default();
        settings
            .engine_devices
            .insert(EmbeddingEngine::SBERT, "cuda".to_string());
        assert_eq!(settings.device_for(EmbeddingEngine::SBERT), "cuda");
    }

    #[test]
    fn test_generation_settings_defaults() {
        let settings = GenerationSettings::default();
        assert!(!settings.enabled);
        assert_eq!(settings.engine, crate::generate::GenerationEngine::Candle);
        assert_eq!(settings.model, None);
        assert_eq!(settings.device, None);
        assert_eq!(settings.ollama_url, DEFAULT_OLLAMA_URL);
        assert_eq!(settings.context_tokens, None);
        assert!(settings.sampling_overrides.is_empty());
    }

    #[test]
    fn legacy_generation_settings_default_to_candle_and_the_local_ollama_url() {
        let settings: GenerationSettings = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "model": "org/model"
        }))
        .unwrap();
        assert_eq!(settings.engine, crate::generate::GenerationEngine::Candle);
        assert_eq!(settings.ollama_url, DEFAULT_OLLAMA_URL);
    }

    #[test]
    fn legacy_quantized_gemma_selection_migrates_to_dense_model() {
        let settings: GenerationSettings = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "model": LEGACY_QUANTIZED_GEMMA_MODEL
        }))
        .unwrap();
        assert_eq!(
            settings.model,
            Some(GeneratorModel(DENSE_GEMMA_MODEL.to_string()))
        );
        assert_eq!(
            serde_json::to_value(settings.model.unwrap()).unwrap(),
            serde_json::json!(DENSE_GEMMA_MODEL)
        );
    }

    #[test]
    fn legacy_generation_timeout_is_not_persisted_as_a_second_worker_policy() {
        let settings: GenerationSettings = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "model": "org/model",
            "device": null,
            "worker_timeout_secs": 60,
            "sampling_overrides": {}
        }))
        .unwrap();

        let serialized = serde_json::to_value(settings).unwrap();
        assert_eq!(
            serialized.get("worker_timeout_secs"),
            None,
            "generation must use the shared five-minute worker residency"
        );
        assert_eq!(crate::worker::DEFAULT_IDLE_TIMEOUT_SECS, 300);
    }

    #[test]
    fn test_source_map_resolve_fallback() {
        let map = SourceMap {
            segments: vec![SourceSegment {
                text_range: ByteRange { start: 0, end: 10 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
                provenance: Default::default(),
            }],
        };

        // Offset beyond all segments should fall back to last segment
        match map.resolve(100).unwrap() {
            SourceOrigin::TextFile { line, .. } => assert_eq!(line, 1),
            _ => panic!("Expected TextFile origin"),
        }
    }

    #[test]
    fn test_source_map_resolve_range_multi_page() {
        let map = SourceMap {
            segments: vec![
                SourceSegment {
                    text_range: ByteRange { start: 0, end: 10 },
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: None,
                    },
                    provenance: Default::default(),
                },
                SourceSegment {
                    text_range: ByteRange { start: 10, end: 20 },
                    origin: SourceOrigin::PdfPage {
                        page: 2,
                        bbox: None,
                    },
                    provenance: Default::default(),
                },
            ],
        };

        // Range spanning page 1 and 2
        let origin = map.resolve_range(ByteRange { start: 5, end: 15 }).unwrap();
        match origin {
            SourceOrigin::PdfPage { page, .. } => assert_eq!(page, 1),
            _ => panic!("Expected PdfPage origin on page 1"),
        }
    }

    #[test]
    fn test_source_map_resolve_range_no_overlap() {
        let map = SourceMap {
            segments: vec![SourceSegment {
                text_range: ByteRange { start: 10, end: 20 },
                origin: SourceOrigin::TextFile { line: 2, col: 1 },
                provenance: Default::default(),
            }],
        };

        // Range before any segment
        let origin = map.resolve_range(ByteRange { start: 0, end: 5 }).unwrap();
        match origin {
            SourceOrigin::TextFile { line, .. } => assert_eq!(line, 2),
            _ => panic!("Expected fallback to last segment"),
        }
    }

    #[test]
    fn test_embedding_engine_supported() {
        let engines = EmbeddingEngine::supported_engines();
        assert!(!engines.is_empty());
        assert!(engines.contains(&EmbeddingEngine::SBERT));
    }

    #[test]
    fn test_deserialize_custom_models() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[allow(dead_code)]
            #[serde(deserialize_with = "deserialize_custom_models")]
            models: Vec<CustomModel>,
        }

        // Test old format (Vec<String>)
        let json = r#"{"models": ["model1", "model2"]}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(w.models.len(), 2);
        assert_eq!(w.models[0].model_id, "model1");
        assert_eq!(w.models[0].engine, EmbeddingEngine::SBERT);

        // Test new format (Vec<CustomModel>)
        let json = r#"{"models": [{"engine": "Candle", "model_id": "model3"}]}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(w.models.len(), 1);
        assert_eq!(w.models[0].model_id, "model3");
        assert_eq!(w.models[0].engine, EmbeddingEngine::Candle);
    }

    #[test]
    fn test_deserialize_custom_models_invalid() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[allow(dead_code)]
            #[serde(deserialize_with = "deserialize_custom_models")]
            models: Vec<CustomModel>,
        }

        let json = r#"{"models": [123]}"#;
        let res: Result<Wrapper, _> = serde_json::from_str(json);
        assert!(res.is_err());
    }

    #[test]
    fn test_semantic_settings_deserialize_legacy_fields() {
        let json = r#"{
            "enabled": true,
            "engine": "Candle",
            "model": "sentence-transformers/all-MiniLM-L12-v2",
            "dimension": 384,
            "engine_devices": {},
            "index_path": null,
            "custom_models": [],
            "chunk_size": 600,
            "chunk_overlap": 128,
            "worker_timeout_secs": 300
        }"#;
        let settings: SemanticSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.selected.engine, EmbeddingEngine::Candle);
        assert_eq!(
            settings.selected.model.model_id(),
            "sentence-transformers/all-MiniLM-L12-v2"
        );
        assert_eq!(settings.selected.dimension, 384);
    }

    #[test]
    fn test_file_type_detect_none() {
        assert_eq!(FileType::detect(Path::new("unknown"), &[]), None);
        assert_eq!(FileType::detect(Path::new("test.unknown"), &[]), None);
    }

    #[test]
    fn test_file_type_detect_known_names() {
        let extensions = vec![];
        assert_eq!(
            FileType::detect(Path::new("Dockerfile"), &extensions),
            Some(FileType::PlainText)
        );
        assert_eq!(
            FileType::detect(Path::new("Makefile"), &extensions),
            Some(FileType::PlainText)
        );
        assert_eq!(
            FileType::detect(Path::new("dockerfile"), &extensions),
            Some(FileType::PlainText)
        );
    }

    #[test]
    fn test_deserialize_custom_models_non_array() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[allow(dead_code)]
            #[serde(deserialize_with = "deserialize_custom_models")]
            models: Vec<CustomModel>,
        }

        let json = r#"{"models": "not an array"}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert!(w.models.is_empty());
    }

    #[test]
    fn test_search_query_defaults() {
        let json = r#"{"pattern": "p", "is_regex": false, "case_sensitive": false, "root": ".", "max_results": 10}"#;
        let q: SearchQuery = serde_json::from_str(json).unwrap();
        assert_eq!(q.respect_gitignore, true);
        assert_eq!(q.max_file_size, 0);
        assert_eq!(q.context_lines, 2);
        assert_eq!(q.mode, SearchMode::Grep);
    }

    #[test]
    fn test_embedder_model_serde() {
        let m = EmbedderModel("model-1".to_string());
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, "\"model-1\"");

        let m2: EmbedderModel = serde_json::from_str(&json).unwrap();
        assert_eq!(m2, m);
        assert_eq!(m2.model_id(), "model-1");
    }

    #[test]
    fn test_settings_default() {
        let s = Settings::default();
        assert!(s.supported_extensions.contains(&"pdf".to_string()));
        assert_eq!(s.context_lines, 2);
        assert_eq!(s.chat_backend, AgentBackend::ClaudeCode);
        assert_eq!(s.pdf_auto_zoom_target_px, 15.5);
    }

    #[test]
    fn external_mcp_settings_default_old_configs_to_loopback() {
        let settings: ExternalMcpSettings =
            serde_json::from_str(r#"{"enabled":true,"port":39217}"#).unwrap();
        assert_eq!(
            settings.bind_address,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        );
        assert!(!settings.require_token);
        assert!(serde_json::from_str::<ExternalMcpSettings>(
            r#"{"enabled":true,"bind_address":"not-an-address","port":39217}"#
        )
        .is_err());
    }

    #[test]
    fn test_removed_gemini_chat_backend_setting_migrates_to_default() {
        let json = r#"{
            "favorites": [],
            "recent_dirs": [],
            "respect_gitignore": true,
            "max_file_size": 10485760,
            "context_lines": 2,
            "theme": "System",
            "semantic": {
                "enabled": false,
                "index_path": null
            },
            "chat_backend": "Gemini"
        }"#;

        let settings: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.chat_backend, AgentBackend::ClaudeCode);
        assert_eq!(settings.pdf_auto_zoom_target_px, 15.5);
    }

    #[test]
    fn test_nanocoder_chat_backend_setting_deserializes() {
        let json = r#"{
            "favorites": [],
            "recent_dirs": [],
            "respect_gitignore": true,
            "max_file_size": 10485760,
            "context_lines": 2,
            "theme": "System",
            "semantic": {
                "enabled": false,
                "index_path": null
            },
            "chat_backend": "Nanocoder"
        }"#;

        let settings: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.chat_backend, AgentBackend::Nanocoder);
    }

    #[test]
    fn generation_stream_event_uses_one_tagged_transport_contract() {
        let event = GenerationStreamEvent::Completed {
            request_id: "summary-42".to_string(),
            task: GenerationTask::DocumentSummary,
            text: "Final summary.".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "phase": "completed",
                "request_id": "summary-42",
                "task": "document_summary",
                "text": "Final summary."
            })
        );
        assert_eq!(
            serde_json::from_value::<GenerationStreamEvent>(json).unwrap(),
            event
        );
    }
}
