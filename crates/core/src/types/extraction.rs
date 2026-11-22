//! File types and what extraction produces: source maps, text provenance,
//! outlines and diagnostics.

use super::*;

/// How Wilkes reads a file — not what format it is. `extension` and
/// [`crate::extract::document::format::PagedFormat`] answer that.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileType {
    /// The file's bytes are its text, addressed by line and column.
    PlainText,
    /// A document read as laid-out pages, addressed by page and box. PDFs,
    /// and every format the MuPDF backend paginates: EPUB, MOBI, FB2.
    ///
    /// Serialized as `"Pdf"` because that is what it has always been on the
    /// wire, and it is a value users have written saved filters against. The
    /// name was accurate when PDFs were the only paginated format; the
    /// meaning has widened and the name had not.
    #[serde(rename = "Pdf")]
    Paged,
}

impl FileType {
    /// Which reading a file gets, or `None` for one Wilkes does not read.
    ///
    /// **A document is not a preference.** Every format the MuPDF backend
    /// reads — PDF, EPUB, MOBI, KF8, FB2, comic archives — is admitted here
    /// whatever `supported_extensions` says, because Wilkes is a reader of
    /// documents and a library with its books switched off is not a smaller
    /// library, it is a broken one. The extension list decides which *text*
    /// files a corpus contains, which is a real choice: a source tree's `.rs`
    /// files are a corpus to one user and noise to another. There is no
    /// corresponding sense in which a PDF sitting in a library is noise, and
    /// letting one drop out of it produced the same silence either way —
    /// a shelf of EPUBs that indexed as nothing, with no error anywhere,
    /// because a settings file written before EPUB support existed did not
    /// name them.
    ///
    /// This is the one gate: every admission in the application reaches it,
    /// directly or through `directory_watcher::path_has_supported_extension`.
    pub fn detect(path: &std::path::Path, supported_extensions: &[String]) -> Option<Self> {
        // `Pdf` on the wire has meant "a document read as laid-out pages"
        // since the MuPDF backend started reading EPUB, MOBI and FB2 as well;
        // the name is kept because it is a queryable field users have written
        // saved filters against.
        if crate::extract::document::format::PagedFormat::is_document(path) {
            return Some(FileType::Paged);
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());

        if let Some(ext) = &ext {
            if supported_extensions
                .iter()
                .any(|s| s.to_ascii_lowercase() == *ext)
            {
                return Some(FileType::PlainText);
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
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
    ///
    /// *Into which reading.* A PDF's outline is anchored in the page's own
    /// glyphs, because reading one must not run inference — so this offset
    /// indexes the unenriched reading, not one in which recognized formulas
    /// and tables have superseded the words they cover. The two agree on
    /// every other field of this entry, and on nothing about this one past
    /// the document's first recognized region. A consumer holding an
    /// enriched rendition resolves by `page` and `title` instead; the offset
    /// is still exported, because for an unenriched consumer it is exact and
    /// for everyone it says how the entry was found.
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

/// One document's declared outline, with what the read that anchored it had
/// to decide.
///
/// The two travel together because they are produced together: resolving a
/// bookmark to a byte offset means reading the document, and reading the
/// document is where the sanitation judgements are made. A caller that asks
/// for the outline is therefore holding the evidence for how good the offsets
/// it just received are, without a second call to go and find it.
///
/// *Of which read.* The outline's, and no other — the page's own glyphs, no
/// analyzer, because an outline request must not run inference. So the
/// sanitation counters here are exactly the evidence they were always meant to
/// be: column clustering, relocated marginalia and joined hyphens are what
/// decide whether an offset is where the heading is. The recognition counters
/// are zero, and mean "this read recognized nothing", not "this document holds
/// nothing to recognize". What an *enriched* read of the document found is a
/// fact about an indexing job, and is reported by the job that ran it.
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
    /// Images the configured [`ImageScope`] withheld from the recognizer.
    /// Their pixels were never decoded, so nothing recorded about them is a
    /// claim about what they contain.
    #[serde(default)]
    pub native_images_not_read: u32,
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
    /// Regions refused because the decode that produced them reached its
    /// token cap without emitting an end-of-sequence token — a decode that
    /// did not finish, counted apart from a decode that finished and was
    /// simply wrong.
    #[serde(default)]
    pub ocr_regions_rejected_truncated: u32,

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
    ///
    /// Read it against `regions_marked_not_text` below, which is the count
    /// this one used to be mixed with. A non-zero number here is a gap worth
    /// looking at; a non-zero number there is the recognizer doing its job.
    #[serde(default)]
    pub regions_unroutable: u32,
    /// Regions the recognizer marked out that carry no text to read — a
    /// picture, most often the whole of a figure crop handed to a document
    /// parser.
    ///
    /// Expected, and roughly one per textless figure. It is counted because a
    /// figure the recognizer named and one it never saw read identically in a
    /// reading that contains neither, and it is counted *here* rather than
    /// with the unroutable regions because those two say opposite things
    /// about the build.
    #[serde(default)]
    pub regions_marked_not_text: u32,

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
    /// Tables read from the structure model and refused because their first
    /// row held no glyph, because a word inside their rectangle landed in no
    /// cell, or because too much of the grid was blank.
    ///
    /// Three counters rather than one, for the same reason the kinds above are
    /// counted apart: a grid shifted off the ink, a grid missing a column and
    /// a grid that is mostly air are three different failures of the same
    /// model, and one number covering them would answer none of them.
    #[serde(default)]
    pub tables_rejected_empty_header_row: u32,
    #[serde(default)]
    pub tables_rejected_unassigned_words: u32,
    #[serde(default)]
    pub tables_rejected_sparse: u32,
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
    /// Areas the layout detector marked out on any page, of any class —
    /// including the classes nothing reads. The denominator for the two
    /// counters below.
    #[serde(default)]
    pub layout_regions_detected: u32,
    /// Detected areas this build reads nothing for. Not a failure: a heading
    /// is better read from the page's own glyphs than from a picture of them.
    /// Counted because one class in it is a real gap — an inline formula is
    /// found and not routed, because supersession replaces whole lines and an
    /// inline formula shares its line with prose.
    #[serde(default)]
    pub layout_regions_not_routed: u32,
    /// Pages the detector could not be run on. Their glyph runs stand, which
    /// is what they would have had anyway; the log says which page and why.
    #[serde(default)]
    pub layout_pages_failed: u32,
    /// Areas the detector marked out that Wilkes routes to a recognizer, and
    /// that the page's reading has a place for. Every one of them is rendered
    /// and read — there is no per-document cap, so this is also the number of
    /// regions the recognizer was asked about.
    #[serde(default)]
    pub typeset_regions_found: u32,
    /// Of those, the ones the page *drew* rather than set: no word of the text
    /// layer lies under them, so they replace nothing and their reading is
    /// inserted at the position the page's geometry gives them. A document
    /// with many of these is one whose mathematics would otherwise be absent
    /// from the reading altogether rather than merely flattened.
    #[serde(default)]
    pub typeset_regions_anchored: u32,
    /// Regions whose reading was admitted, and which therefore stand in the
    /// canonical reading in place of the glyphs the page drew there — or, for
    /// an anchored region, at the place the page put it. The difference from
    /// `typeset_regions_found` is the regions whose recognizer answer was
    /// refused, where the page's own glyphs were kept and nothing was
    /// inserted.
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
