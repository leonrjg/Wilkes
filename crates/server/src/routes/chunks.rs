use crate::*;

/// A chunk request names an index with `scope` and its passages with refs, and
/// that is the whole of its addressing.
///
/// There were two of each of these routes — one taking a corpus id and a
/// pinned space, one taking a workspace id and SQLite rowids — and they
/// disagreed about what a caller could name. A rowid is reissued when a file
/// is re-indexed, so the second vocabulary could only ever be safe for a
/// caller that never stored anything, which is not the caller these exist for.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChunksGroupsBody {
    #[serde(default)]
    pub(crate) scope: ConsumerScope,
    pub(crate) groups: Vec<Vec<ChunkRef>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChunksResolveBody {
    #[serde(default)]
    pub(crate) scope: ConsumerScope,
    pub(crate) chunk_refs: Vec<ChunkRef>,
    /// Absent, the reply carries the named passages and nothing else.
    #[serde(default)]
    pub(crate) neighbors: Option<wilkes_api::context::NeighborWindow>,
}

pub(crate) async fn chunks_resolve_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChunksResolveBody>,
) -> Result<Json<wilkes_api::context::ChunkResolution>, (StatusCode, Json<ErrorBody>)> {
    addressable_context(&state, &body.scope)
        .await?
        .resolve_chunks(body.chunk_refs, body.neighbors)
        .await
        .map(Json)
        .map_err(consumer_err)
}

pub(crate) async fn chunks_accumulate_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChunksGroupsBody>,
) -> Result<Json<wilkes_api::context::ChunkAccumulations>, (StatusCode, Json<ErrorBody>)> {
    addressable_context(&state, &body.scope)
        .await?
        .accumulate_chunks(body.groups)
        .await
        .map(Json)
        .map_err(consumer_err)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChunksSimilarityBody {
    #[serde(default)]
    pub(crate) scope: ConsumerScope,
    pub(crate) probes: Vec<wilkes_api::context::ProbeRequest>,
    #[serde(default)]
    pub(crate) chunk_refs: Vec<ChunkRef>,
}

pub(crate) async fn chunks_similarity_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChunksSimilarityBody>,
) -> Result<Json<wilkes_api::context::ChunkSimilarities>, (StatusCode, Json<ErrorBody>)> {
    addressable_context(&state, &body.scope)
        .await?
        .chunk_similarity(body.probes, body.chunk_refs)
        .await
        .map(Json)
        .map_err(consumer_err)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VectorProbe {
    pub(crate) vector: Vec<f32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TextProbe {
    pub(crate) text: String,
}

/// A probe is a vector the caller already holds or the text it wants searched
/// for, and the two are not interchangeable in what they can express.
///
/// A caller sending text gets its query embedded in the **query** role, as
/// `embed/query` does for a caller ranking vectors it holds itself;
/// `embed/text` answers in the passage role and has to, because the vectors it
/// returns are stored. So
/// "search the corpus for this text" used to be two calls of which the first
/// could not know what the text was for. Sending text is also one round trip
/// instead of two, and it lets a later model with query prefixes work without
/// the consumer learning that it exists.
///
/// Vectors stay, and not for compatibility: a caller searching for something
/// it holds a vector *for* — a centroid, a stored embedding — has no text to
/// send.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum SearchProbe {
    Vector(VectorProbe),
    Text(TextProbe),
}

impl From<SearchProbe> for wilkes_api::context::SearchProbeInput {
    fn from(probe: SearchProbe) -> Self {
        match probe {
            SearchProbe::Vector(probe) => Self::Vector(probe.vector),
            SearchProbe::Text(probe) => Self::Text(probe.text),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChunksSearchBody {
    #[serde(default)]
    pub(crate) scope: ConsumerScope,
    pub(crate) probes: Vec<SearchProbe>,
    pub(crate) top_k: usize,
    pub(crate) min_similarity: f32,
}

pub(crate) async fn chunks_search_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChunksSearchBody>,
) -> Result<Json<wilkes_api::context::ChunkSearch>, (StatusCode, Json<ErrorBody>)> {
    addressable_context(&state, &body.scope)
        .await?
        .chunk_search(
            body.probes.into_iter().map(Into::into).collect(),
            body.top_k,
            body.min_similarity,
        )
        .await
        .map(Json)
        .map_err(consumer_err)
}

#[derive(Deserialize)]
pub(crate) struct ExportChunksBody {
    pub(crate) root: PathBuf,
    pub(crate) path: PathBuf,
    /// Which index holds this file's passages. Absent means the active
    /// workspace.
    ///
    /// Each workspace owns its index, so this is not a filter — it names the
    /// database the chunks are read from. Without it, exporting a document
    /// from another library meant switching the whole server to it first.
    #[serde(default)]
    pub(crate) scope: ConsumerScope,
}

/// The document scope shared by export routes that read one source file.
#[derive(Deserialize)]
pub(crate) struct ExportOutlineBody {
    pub(crate) root: PathBuf,
    pub(crate) path: PathBuf,
    /// Which workspace owns the configured library root. Absent means the
    /// active workspace, consistent with the rest of the export surface.
    #[serde(default)]
    pub(crate) scope: ConsumerScope,
}

/// Chunk + vector export for one indexed file: text, byte ranges, source
/// origins, and stored embeddings, in extraction order.
pub(crate) async fn export_chunks_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExportChunksBody>,
) -> Result<Json<wilkes_api::context::FileChunkExport>, (StatusCode, Json<ErrorBody>)> {
    addressable_context(&state, &body.scope)
        .await?
        .export_file_chunks(body.root, body.path)
        .await
        .map(Json)
        .map_err(server_err)
}

/// Declared PDF/document outline without opening the semantic index or
/// returning embeddings. Entries retain the source document's page and byte
/// locators; `/api/export/chunks` additionally resolves them to chunk ordinals.
pub(crate) async fn export_outline_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExportOutlineBody>,
) -> Result<Json<wilkes_api::context::FileOutlineExport>, (StatusCode, Json<ErrorBody>)> {
    state
        .consumer_index(&body.scope)
        .await?
        .into_context()
        .export_file_outline(body.root, body.path)
        .await
        .map(Json)
        .map_err(server_err)
}

#[derive(Deserialize)]
pub(crate) struct ExportFilesBody {
    pub(crate) root: PathBuf,
    /// Which workspace's library and index to read. Absent means the active
    /// one, as everywhere else on the export surface.
    #[serde(default)]
    pub(crate) scope: ConsumerScope,
}

/// The documents one library root holds, each with the passage count that says
/// whether `/api/export/chunks` can answer for it.
///
/// Deliberately not `/api/files`: that endpoint is confined to the uploads
/// directory, which is the right jail for a browser talking to a shared
/// server and the wrong one for a consumer asking about the library itself.
/// This one is confined to the workspace's own library roots, exactly as the
/// chunk export is.
pub(crate) async fn export_files_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExportFilesBody>,
) -> Result<Json<wilkes_api::context::LibraryFileExport>, (StatusCode, Json<ErrorBody>)> {
    state
        .consumer_index(&body.scope)
        .await?
        .into_context()
        .export_library_files(body.root)
        .await
        .map(Json)
        .map_err(server_err)
}

#[derive(Deserialize)]
pub(crate) struct ExportFiguresBody {
    pub(crate) root: PathBuf,
    pub(crate) path: PathBuf,
    #[serde(default)]
    pub(crate) scope: ConsumerScope,
}

/// One document's pictures: where each sits and what its pixels must hash to.
///
/// The browse half of the figure surface, addressed exactly as
/// `/api/export/chunks` is. It says nothing about passages — `/api/chunks/figures`
/// is the half that links a picture to the text it was drawn into.
pub(crate) async fn export_figures_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExportFiguresBody>,
) -> Result<Json<wilkes_api::context::FileFigureExport>, (StatusCode, Json<ErrorBody>)> {
    addressable_context(&state, &body.scope)
        .await?
        .export_file_figures(body.root, body.path)
        .await
        .map(Json)
        .map_err(server_err)
}

#[derive(Deserialize)]
pub(crate) struct ChunkFiguresBody {
    #[serde(default)]
    pub(crate) scope: ConsumerScope,
    pub(crate) chunk_refs: Vec<ChunkRef>,
    /// Bytes of slack outside a passage within which a figure still counts.
    ///
    /// Absent means none, which admits only the figures a passage contains.
    /// A window is a widening the caller asks for: it reaches the figure
    /// printed a page over from the prose discussing it, at the price of
    /// sometimes reaching one the prose never mentions. Hits carry their
    /// distance so the caller can spend a budget nearest-first.
    #[serde(default)]
    pub(crate) window_bytes: usize,
}

/// The figures these passages bear on, strongest relation first.
pub(crate) async fn chunk_figures_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChunkFiguresBody>,
) -> Result<Json<wilkes_api::context::ChunkFigureResolution>, (StatusCode, Json<ErrorBody>)> {
    let index = state.consumer_index(&body.scope).await?;
    index
        .into_context()
        .chunk_figures(body.chunk_refs, body.window_bytes)
        .await
        .map(Json)
        .map_err(consumer_err)
}

#[derive(Deserialize)]
pub(crate) struct FigureImageBody {
    #[serde(default)]
    pub(crate) scope: ConsumerScope,
    /// The document, named either way a consumer holds it: a library root and
    /// path, or any passage of it.
    #[serde(default)]
    pub(crate) root: Option<PathBuf>,
    #[serde(default)]
    pub(crate) path: Option<PathBuf>,
    #[serde(default)]
    pub(crate) chunk_ref: Option<ChunkRef>,
    /// `p{page}-i{ordinal}`, from the inventory.
    pub(crate) area_id: String,
    /// Longest edge to downscale to. Absent serves the picture at the size the
    /// page draws it — which is the size its digest covers.
    #[serde(default)]
    pub(crate) max_edge: Option<u32>,
}

/// One figure's pixels, re-derived from the document and checked against the
/// digest the rendition recorded.
///
/// PNG bytes rather than a JSON envelope with base64 in it: this is an image,
/// every client that wants one can read one, and a base64 field would make
/// every consumer decode a third more bytes than it needed to. The digest and
/// the dimensions ride on headers so a caller can check what it got without
/// parsing the picture.
pub(crate) async fn figure_image_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<FigureImageBody>,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    let address = match (body.root, body.path, body.chunk_ref) {
        (Some(root), Some(path), None) => wilkes_api::context::FigureAddress::File { root, path },
        (None, None, Some(chunk_ref)) => wilkes_api::context::FigureAddress::Chunk(chunk_ref),
        _ => {
            return Err(err(
                "name the document either by root and path or by chunk_ref, and not by both",
            ))
        }
    };
    let index = state.consumer_index(&body.scope).await?;
    let rendered = index
        .into_context()
        .figure_pixels(address, body.area_id, body.max_edge)
        .await
        .map_err(consumer_err)?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, rendered.media_type)
        .header("x-wilkes-image-sha256", rendered.image_sha256)
        .header("x-wilkes-image-width", rendered.width)
        .header("x-wilkes-image-height", rendered.height)
        .header("x-wilkes-image-source-width", rendered.source_width)
        .header("x-wilkes-image-source-height", rendered.source_height)
        .body(Body::from(rendered.bytes))
        .unwrap())
}
