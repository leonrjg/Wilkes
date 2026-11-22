//! Data the context hands out: chunk, figure, outline and library exports,
//! and the request limits that bound them.

use super::*;

#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkExport {
    pub chunk_ref: ChunkRef,
    pub ordinal: usize,
    pub text: String,
    pub text_sha256: String,
    pub byte_range: wilkes_core::types::ByteRange,
    pub origin: wilkes_core::types::SourceOrigin,
}

impl From<ManagedChunkData> for ChunkExport {
    fn from(chunk: ManagedChunkData) -> Self {
        ChunkExport {
            chunk_ref: chunk.chunk_ref,
            ordinal: chunk.ordinal,
            text: chunk.text,
            text_sha256: chunk.text_sha256,
            byte_range: chunk.extraction_byte_range,
            origin: chunk.origin,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedEmbeddingWork {
    pub chunks: usize,
    pub reused: usize,
    pub computed: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedDocumentExport {
    pub corpus_id: String,
    pub source_sha256: String,
    pub source_byte_len: u64,
    pub media_type: String,
    pub snapshot_id: String,
    pub rendition_id: String,
    pub extraction_recipe_id: String,
    pub extracted_content_sha256: String,
    pub chunk_count: usize,
    // No embedding space, engine, model or dimension. These used to be
    // reported from the canonical corpus's index, whose identity is the model
    // the corpus was *created* with: it embeds nothing, so its id is no space
    // the corpus holds, and a caller that echoed it back as the next import's
    // `expected_embedding_space_id` was refused. The spaces that hold the
    // document are the import route's to report, after it has brought them
    // level.
    pub outline: Vec<ExportedOutlineEntry>,
    /// What the read that anchored `outline` had to decide for itself — which
    /// pages clustered into a body column and which were too ambiguous to
    /// reorder, what was removed as furniture, how the wrap hyphens resolved.
    ///
    /// **The outline read's, not the imported rendition's.** They are the same
    /// document read twice, and the same on every counter above; they differ
    /// on the recognition counters, which are zero here because an outline
    /// read runs no recognizer. Reporting the rendition's would mean either
    /// storing them when it was written or extracting the document a second
    /// time with the model — and the second is what this field used to do,
    /// silently, on every managed import.
    pub extraction: wilkes_core::types::ExtractionDiagnostics,
    pub chunks: Vec<ChunkExport>,
    pub embedding_work: ManagedEmbeddingWork,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ManagedCorpusBackup {
    pub format: String,
    pub corpus_id: String,
    pub embedding_space_id: String,
    pub path: String,
    pub file_count: usize,
    pub byte_len: u64,
    pub files: Vec<ManagedBackupFile>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ManagedBackupFile {
    pub path: String,
    pub byte_len: u64,
    pub sha256: String,
}

/// Result of `accumulate_chunks`: one accumulation per requested group, in
/// the order the groups arrived.
///
/// The group shape is the index's own [`ChunkAccumulation`] rather than a
/// copy of it here: it is the same two numbers, and a second definition would
/// be a second place for them to drift.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkAccumulations {
    pub embedding_space_id: String,
    pub dimension: usize,
    pub groups: Vec<ChunkAccumulation>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkResolution {
    pub embedding_space_id: String,
    pub chunks: Vec<ChunkExport>,
    /// Present exactly when the request asked for a [`NeighborWindow`]: one
    /// entry per requested ref, in request order, as `chunks` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub neighbors: Option<Vec<ChunkNeighbors>>,
}

/// How many passages either side of each named one a `resolve` reply also
/// carries, in the same rendition.
///
/// A passage is cut to a size, not to a sentence, so it routinely starts
/// mid-clause; the passage before it is what says what the clause belongs
/// to. Only Wilkes knows which passages are adjacent — a caller holds the
/// refs it was handed and nothing that would let it name the ones between.
#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeighborWindow {
    pub before: usize,
    pub after: usize,
}

/// The passages either side of one requested passage, each side in reading
/// order: the last of `before` immediately precedes `of`, the first of
/// `after` immediately follows it. A side is shorter than asked only where
/// the document starts or ends.
///
/// Full passages rather than refs into `chunks`: a neighbour is usually not
/// one of the passages asked for, and a caller reading context should not
/// have to look it up a second time.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkNeighbors {
    pub of: ChunkRef,
    pub before: Vec<ChunkExport>,
    pub after: Vec<ChunkExport>,
}

impl From<ManagedChunkNeighbors> for ChunkNeighbors {
    fn from(neighbors: ManagedChunkNeighbors) -> Self {
        ChunkNeighbors {
            of: neighbors.of,
            before: neighbors
                .before
                .into_iter()
                .map(ChunkExport::from)
                .collect(),
            after: neighbors.after.into_iter().map(ChunkExport::from).collect(),
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct ProbeRequest {
    pub vector: Vec<f32>,
    #[serde(default)]
    pub scope: Vec<ChunkRef>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ProbeSimilarity {
    pub nearest_chunk_ref: Option<ChunkRef>,
    pub similarity: Option<f32>,
    pub scope_mean: Option<f32>,
    pub scope_size: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkNearest {
    pub chunk_ref: ChunkRef,
    pub probe: usize,
    pub similarity: f32,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkSimilarities {
    pub embedding_space_id: String,
    pub dimension: usize,
    pub probes: Vec<ProbeSimilarity>,
    pub chunks: Vec<ChunkNearest>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkSearchHit {
    pub chunk_ref: ChunkRef,
    pub snapshot_id: String,
    pub rendition_id: String,
    pub ordinal: usize,
    pub similarity: f32,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ProbeSearch {
    pub hits: Vec<ChunkSearchHit>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkSearch {
    pub embedding_space_id: String,
    pub dimension: usize,
    pub probes: Vec<ProbeSearch>,
}

pub const MAX_SEARCH_PROBES: usize = 512;

pub const MAX_SEARCH_TOP_K: usize = 100;

/// Most groups one `chunk_centroids` request may name, and most chunk ids it
/// may name across all of them.
///
/// The reply is one vector per group, so the group cap bounds what comes back
/// (256 groups at 384 dimensions is under half a megabyte) and the id cap
/// bounds the index scan that produces it. Both are generous by the standard
/// of the question: a caller asking for more than a few hundred regions in one
/// request is building a projection of the index, which is what
/// `export_file_chunks` is for.
pub const MAX_ACCUMULATE_GROUPS: usize = 256;

pub const MAX_ACCUMULATE_REFS: usize = 4_096;

/// Most probes one `chunk_similarity` request may carry, and most chunk ids it
/// may name across the searched set and every scope together.
///
/// The reply is two scalars per probe and one per chunk, so neither cap is
/// about the size of what comes back — they bound the dot products, which is
/// `probes × chunks`. 512 × 8,192 is a few million multiply-adds, well under a
/// second, and a caller wanting more is measuring a library rather than a
/// document and should say so one document at a time.
pub const MAX_SIMILARITY_PROBES: usize = 512;

pub const MAX_SIMILARITY_REFS: usize = 8_192;

/// One exported chunk: the same passage `resolve` returns, plus the stored
/// vector.
///
/// Flattened rather than restated, so there is one definition of what a chunk
/// looks like on the wire. A consumer that stored a passage from an export and
/// later resolved it by ref must be able to match the two, and two structs
/// listing the same fields is how they come to differ by one.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ExportedChunk {
    #[serde(flatten)]
    pub chunk: ChunkExport,
    pub embedding: Vec<f32>,
}

/// One entry of the document's declared table of contents, resolved to the
/// chunk the section starts at.
///
/// The resolution happens here because this is the only place that holds both
/// halves: the outline says "page 41" or "byte 90210", the export says which
/// chunk that is. A consumer given the raw locator would have to re-derive the
/// mapping from the chunk list it was just handed — the same answer, computed
/// twice, with two chances to disagree.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ExportedOutlineEntry {
    pub title: String,
    pub level: u32,
    /// Ordinal of the first exported chunk at or after the entry's position.
    pub chunk_ordinal: usize,
    /// The locator as the document expressed it, kept for display.
    pub page: Option<u32>,
    /// The offset in the reading the *outline* was read from, which for a PDF
    /// is the document's own glyphs and not the enriched text these chunks
    /// carry. Display and provenance only: `chunk_ordinal` is the position in
    /// this export, and is what a consumer segments by. See
    /// [`wilkes_core::types::OutlineEntry::byte_offset`].
    pub byte_offset: Option<usize>,
    /// What established `byte_offset` — the rung of the resolution ladder that
    /// answered. A consumer segmenting by this outline needs it to know which
    /// boundaries are exact and which are snapped to a page.
    pub anchor: wilkes_core::types::OutlineAnchor,
}

/// Result of `export_file_chunks`, in extraction order. `model_id` is absent
/// when no embedder is currently loaded (the stored vectors remain valid for
/// the model that built the index).
///
/// `outline` is empty when the document declares no table of contents — a fact
/// about the document, not a failure: a plain `.txt` without headings has no
/// sections to report and says so.
#[derive(Clone, Debug, serde::Serialize)]
pub struct FileChunkExport {
    pub file_path: PathBuf,
    pub model_id: Option<String>,
    pub dimension: Option<usize>,
    pub outline: Vec<ExportedOutlineEntry>,
    pub chunks: Vec<ExportedChunk>,
}

/// One picture a document draws, as a consumer that may want to see it needs
/// it described: where it is, how big it is, and what it must hash to.
///
/// Placement, never the annotation. The transcription and the description are
/// already in the exported text at the position the page drew the image, and
/// repeating them here would be a second copy of bytes the consumer already
/// holds. `has_description` says whether that prose exists without restating
/// it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ExportedFigure {
    /// `p{page}-i{ordinal}`, the id extraction assigned. What the pixel
    /// endpoint is addressed by.
    pub area_id: String,
    pub page: u32,
    pub origin: wilkes_core::types::RegionOrigin,
    pub bbox: wilkes_core::types::BoundingBox,
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Digest of the decoded pixels. Empty for an image a technical limit
    /// stopped before it was decoded — which is also an image whose pixels
    /// cannot be served.
    pub image_sha256: String,
    /// Where this figure's enrichment block landed in the reading, when one
    /// was written.
    pub reading_range: Option<wilkes_core::types::ByteRange>,
    /// Where the picture itself sits in the reading. Absent only for a
    /// rendition extracted before anchors existed.
    pub reading_anchor: Option<usize>,
    pub has_description: bool,
    pub status: wilkes_core::types::ImageAnalysisStatus,
}

impl From<wilkes_core::types::RetainedImage> for ExportedFigure {
    fn from(image: wilkes_core::types::RetainedImage) -> Self {
        Self {
            area_id: image.id,
            page: image.page,
            origin: image.origin,
            bbox: image.bbox,
            pixel_width: image.pixel_width,
            pixel_height: image.pixel_height,
            image_sha256: image.image_sha256,
            reading_range: image.reading_range,
            reading_anchor: image.reading_anchor,
            has_description: image.has_description,
            status: image.status,
        }
    }
}

/// One document's pictures, in reading order.
#[derive(Clone, Debug, serde::Serialize)]
pub struct FileFigureExport {
    pub file_path: PathBuf,
    /// Whether this rendition has an image inventory at all. `false` means
    /// *unknown* — it was retained before inventories existed — and an empty
    /// `figures` beside it must not be read as "this document draws nothing".
    /// Re-indexing the document settles it.
    pub inventory_known: bool,
    pub figures: Vec<ExportedFigure>,
}

/// One figure, and what it has to do with the passage that asked.
#[derive(Clone, Debug, serde::Serialize)]
pub struct LinkedFigureExport {
    #[serde(flatten)]
    pub figure: ExportedFigure,
    /// `block` — the passage is this figure's enrichment text; `drawn_in` —
    /// the picture sits inside the passage; `near` — it is outside but within
    /// the window the request asked for.
    pub relation: wilkes_core::figure::FigureRelation,
    /// Bytes between the picture and the passage, zero when it is inside.
    pub byte_distance: usize,
}

/// The figures of one passage, strongest relation first.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkFigureExport {
    pub chunk_ref: String,
    pub inventory_known: bool,
    pub figures: Vec<LinkedFigureExport>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkFigureResolution {
    pub embedding_space_id: Option<String>,
    pub chunks: Vec<ChunkFigureExport>,
}

/// How a pixel request names its document.
#[derive(Clone, Debug)]
pub enum FigureAddress {
    /// A library root and a path under it, as the export surface is addressed.
    File { root: PathBuf, path: PathBuf },
    /// Any passage of the document, as a managed consumer holds it.
    Chunk(ChunkRef),
}

/// One figure's pixels, ready to be served.
pub struct FigurePixelsExport {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
    pub width: u32,
    pub height: u32,
    /// What the source decoded to before any downscale — the dimensions the
    /// digest covers.
    pub source_width: u32,
    pub source_height: u32,
    pub image_sha256: String,
}

/// The largest edge a figure may be asked for, in pixels.
///
/// Not a quality judgement: it caps the work one request can ask of a resize
/// and the bytes one reply can weigh. Every model this feeds reads a picture
/// far smaller than this.
pub const MAX_FIGURE_EDGE: u32 = 4096;

/// A document's declared structure, independent of its semantic-index state.
///
/// Unlike [`FileChunkExport`], this deliberately carries the locators the
/// document expressed rather than resolving them to chunk ordinals. Reading a
/// PDF's bookmarks must not require an index or return embedding vectors.
#[derive(Clone, Debug, serde::Serialize)]
pub struct FileOutlineExport {
    pub file_path: PathBuf,
    pub outline: Vec<wilkes_core::types::OutlineEntry>,
    /// The outline read's own decisions. It runs no recognizer, so its
    /// recognition counters are zero for every document; see
    /// [`wilkes_core::types::DeclaredOutline`].
    pub extraction: wilkes_core::types::ExtractionDiagnostics,
}

/// One document Wilkes serves under a library root, as a consumer that wants to
/// ingest it needs to see it.
///
/// `chunk_count` is the fact that decides whether the document can be exported
/// at all: it counts the passages the semantic index holds for this file under
/// this root, so zero means an export would come back empty. It is a count
/// rather than a flag because a consumer showing the file to a person has a
/// use for the size of what it is about to read, and a flag would have thrown
/// that away to say less.
#[derive(Clone, Debug, serde::Serialize)]
pub struct LibraryFile {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub extension: String,
    pub modified_at_ms: Option<i64>,
    /// Title from Wilkes's metadata cache, absent until it has read the file.
    pub title: Option<String>,
    pub chunk_count: usize,
}

/// Result of `export_library_files`: one root's documents, ascending by path.
///
/// The root comes back canonicalised because that is the root the counts were
/// read against — a consumer that asked with a symlinked or relative path can
/// see which directory answered.
#[derive(Clone, Debug, serde::Serialize)]
pub struct LibraryFileExport {
    pub root: PathBuf,
    pub files: Vec<LibraryFile>,
}

/// One retained snapshot, as served.
///
/// The bytes travel raw rather than in a JSON envelope, for the reason
/// `/api/figure` does: a course book is megabytes, every consumer that wants
/// one wants the file, and base64 would make each of them carry a third more
/// bytes than it asked for. The digest and the length ride on headers so a
/// caller can check what it got without opening it.
#[derive(Clone, Debug)]
pub struct DocumentSnapshotExport {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
    /// What the retained bytes hashed to when the corpus retained them — the
    /// same `source_sha256` the import reply carried.
    pub source_sha256: String,
    pub source_byte_len: u64,
}

/// One imported document's cover, ready to be served.
#[derive(Clone, Debug)]
pub struct DocumentCoverExport {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
    pub width: u32,
    pub height: u32,
    /// The retained document it was drawn from, as
    /// [`DocumentSnapshotExport::source_sha256`] names it.
    pub source_sha256: String,
}

/// Most chunk refs one `resolve` request may name — and, with a
/// [`NeighborWindow`], most passages its reply may carry, counting each ref
/// once for itself and once for every neighbour asked of it.
///
/// The two surfaces this replaces disagreed: the export route capped at 64 on
/// the reasoning that displaying a passage should cost a passage, and the
/// managed route borrowed the similarity cap of 8,192, which is sized for an
/// operation returning two scalars per probe rather than one returning full
/// text. This is chosen against what the reply actually weighs — roughly half
/// a megabyte at typical chunk sizes, comparable to the other consumer
/// responses — and callers wanting more page. Neighbours weigh what the
/// passages asked for weigh, so they count against the same bound: a window
/// of one either side leaves room for 170 refs.
pub const MAX_RESOLVE_REFS: usize = 512;

/// Maps each outline entry onto the first exported chunk at or after its
/// position, dropping entries that resolve past the end of the document.
///
/// Ties are kept: a chapter and its first section legitimately begin at the
/// same chunk, and collapsing them would lose the chapter's title. Entries
/// that resolve nowhere — a bookmark pointing at a page whose text was not
/// extracted, which is what a scanned appendix looks like — are dropped, since
/// a section that starts nowhere is not a section.
pub(super) trait OutlineChunk {
    fn outline_ordinal(&self) -> usize;
    fn outline_byte_range(&self) -> &wilkes_core::types::ByteRange;
    fn outline_origin(&self) -> &wilkes_core::types::SourceOrigin;
    /// The chunk's text as the rendition holds it. What a bookmark title is
    /// looked for in, because it is the only material here that belongs to
    /// the same reading as the chunks being resolved against.
    fn outline_text(&self) -> &str;
}

impl OutlineChunk for ExportedChunk {
    fn outline_ordinal(&self) -> usize {
        self.chunk.ordinal
    }

    fn outline_byte_range(&self) -> &wilkes_core::types::ByteRange {
        &self.chunk.byte_range
    }

    fn outline_origin(&self) -> &wilkes_core::types::SourceOrigin {
        &self.chunk.origin
    }

    fn outline_text(&self) -> &str {
        &self.chunk.text
    }
}

impl OutlineChunk for ManagedChunkData {
    fn outline_ordinal(&self) -> usize {
        self.ordinal
    }

    fn outline_byte_range(&self) -> &wilkes_core::types::ByteRange {
        &self.extraction_byte_range
    }

    fn outline_origin(&self) -> &wilkes_core::types::SourceOrigin {
        &self.origin
    }

    fn outline_text(&self) -> &str {
        &self.text
    }
}

/// The page a chunk was extracted from, or `None` for an unpaginated format.
pub(super) fn chunk_page<T: OutlineChunk>(chunk: &T) -> Option<u32> {
    match chunk.outline_origin() {
        wilkes_core::types::SourceOrigin::PdfPage { page, .. } => Some(*page),
        _ => None,
    }
}

/// Paginated documents by page and title; unpaginated ones by offset.
///
/// **Why not by offset for both.** An outline's `byte_offset` is a position in
/// the reading that produced it, and for a PDF that reading is the page's own
/// glyphs — the outline is read without an analyzer, because an outline
/// request must not run inference (see `MuPdfBackend::outline`). The chunks
/// here come from an index built *with* one, where every recognized formula
/// and table has superseded the words it covers. The two are the same document
/// and not the same string: every offset past the first superseded region is
/// displaced by however much text recognition added, which for a textbook is
/// most of the book. Comparing them silently puts section boundaries on the
/// wrong chunk, and does it worst on exactly the documents worth reading.
///
/// So for a paginated entry the page is taken as the bound — it is a fact
/// about the file, and enrichment does not move a heading between pages — and
/// the title locates the entry inside it, matched against the chunks' own text
/// with the same normalization the PDF backend anchors by
/// ([`wilkes_core::extract::outline::title_offset`]). That is the backend's
/// second rung, asked of the reading the caller actually holds.
///
/// An unpaginated document has no enrichment to displace it — there is no
/// analyzer for a Markdown file and no picture to read — so `TextOffset`
/// entries keep the exact offset resolution they always had.
///
/// A title nothing on the page matches — renumbered, restyled, or set as an
/// image — falls back to the first chunk at or after its page, which is the
/// page rung and is where such an entry landed before. Entries that resolve
/// past the end are dropped: a section that starts nowhere is not a section.
pub(super) fn resolve_outline<T: OutlineChunk>(
    outline: &[wilkes_core::types::OutlineEntry],
    chunks: &[T],
) -> Vec<ExportedOutlineEntry> {
    outline
        .iter()
        .filter_map(|entry| {
            let ordinal = match (entry.page, entry.byte_offset) {
                (Some(page), _) => {
                    // The first chunk at or after the entry's page. A bookmark
                    // pointing at a page whose text was not extracted — a
                    // scanned appendix — resolves forward to the next page
                    // that has some, which is where its section's text begins.
                    let from = chunks
                        .iter()
                        .position(|chunk| chunk_page(chunk).is_some_and(|at| at >= page))?;
                    // Then the title, within that page's own chunks only. A
                    // heading whose words recur later in the document must not
                    // pull its section to the recurrence.
                    let landed = chunk_page(&chunks[from]);
                    chunks[from..]
                        .iter()
                        .take_while(|chunk| chunk_page(*chunk) == landed)
                        .find(|chunk| {
                            wilkes_core::extract::outline::title_offset(
                                chunk.outline_text(),
                                &entry.title,
                            )
                            .is_some()
                        })
                        .map_or(
                            chunks[from].outline_ordinal(),
                            OutlineChunk::outline_ordinal,
                        )
                }
                (None, Some(offset)) => chunks
                    .iter()
                    .find(|chunk| chunk.outline_byte_range().end > offset)
                    .map(OutlineChunk::outline_ordinal)?,
                (None, None) => return None,
            };
            Some(ExportedOutlineEntry {
                title: entry.title.clone(),
                level: entry.level,
                chunk_ordinal: ordinal,
                page: entry.page,
                byte_offset: entry.byte_offset,
                anchor: entry.anchor,
            })
        })
        .collect()
}
