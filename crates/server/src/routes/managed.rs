use crate::*;

pub(crate) async fn ensure_managed_workspace_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<EnsureManagedWorkspace>,
) -> Result<Json<ManagedWorkspaceStatus>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(workspace_manager_unavailable)?;
    let initial = manager
        .ensure_managed_workspace(request)
        .await
        .map_err(consumer_anyhow_err)?;
    let context = manager
        .context_for(&initial.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    context.ensure_managed_index().await.map_err(consumer_err)?;
    let status = manager
        .managed_workspace_status(&initial.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    Ok(Json(managed_status_with_pending(status, &context)))
}

pub(crate) fn managed_status_with_pending(
    mut status: ManagedWorkspaceStatus,
    context: &AppContext,
) -> ManagedWorkspaceStatus {
    // Builds are already in `status`: the manager counts the projection
    // builds it owns.
    status.pending_imports = context.managed_pending_imports();
    status
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManagedStatusQuery {
    pub(crate) corpus_id: String,
}

pub(crate) async fn managed_workspace_status_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ManagedStatusQuery>,
) -> Result<Json<ManagedWorkspaceStatus>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(workspace_manager_unavailable)?;
    let status = manager
        .managed_workspace_status(&query.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    let context = manager
        .context_for(&query.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    Ok(Json(managed_status_with_pending(status, &context)))
}

pub(crate) async fn ensure_managed_space_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<EnsureManagedEmbeddingSpace>,
) -> Result<Json<ManagedEmbeddingSpaceStatus>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(workspace_manager_unavailable)?;
    manager
        .ensure_managed_space(request)
        .await
        .map(Json)
        .map_err(consumer_anyhow_err)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ManagedImportSource {
    Path {
        path: PathBuf,
    },
    WilkesFile {
        workspace_id: String,
        root: PathBuf,
        path: PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManagedImportBody {
    pub(crate) corpus_id: String,
    /// Absent when the caller expects an empty corpus. Importing is how a
    /// corpus gets its first vectors, so this is the one managed endpoint that
    /// runs before an embedding space exists.
    #[serde(default)]
    pub(crate) expected_embedding_space_id: Option<String>,
    pub(crate) idempotency_key: String,
    pub(crate) source: ManagedImportSource,
}

/// What `documents/import` answers: the admitted document, and the embedding
/// spaces that hold it.
#[derive(Serialize)]
pub(crate) struct ManagedImportReply {
    #[serde(flatten)]
    pub(crate) document: wilkes_api::context::ManagedDocumentExport,
    /// Every space of the corpus that is level with it once this import's
    /// catch-up has run, so holds this document. Any of them is a valid
    /// `expected_embedding_space_id` for the next import. A space that could
    /// not follow is absent here, and logged; empty when the corpus has no
    /// projection yet, which is also when the next import takes no pin.
    pub(crate) spaces: Vec<String>,
}

pub(crate) async fn import_managed_document_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ManagedImportBody>,
) -> Result<Json<ManagedImportReply>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(workspace_manager_unavailable)?;
    let status = manager
        .managed_spaces(&body.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    let expected_exists = match body.expected_embedding_space_id.as_deref() {
        Some(expected) => status
            .spaces
            .iter()
            .any(|space| space.embedding_space_id == expected),
        None => status.spaces.is_empty(),
    };
    if !expected_exists {
        return Err(consumer_err(ConsumerError::new(
            ConsumerErrorCode::EmbeddingSpaceMismatch,
            match body.expected_embedding_space_id.as_deref() {
                Some(pin) => format!(
                    "corpus={}, request={pin}; {}",
                    describe_corpus_spaces(&status.spaces),
                    corpus_spaces_remedy(&status.spaces),
                ),
                // Unpinned, which only a corpus with no spaces may be.
                None => format!(
                    "corpus={}, request=none; a corpus with spaces is imported into with one \
                     of them as expected_embedding_space_id",
                    describe_corpus_spaces(&status.spaces),
                ),
            },
        )));
    }
    let managed = manager
        .context_for(&body.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    managed.ensure_managed_index().await.map_err(consumer_err)?;
    let (path, source_workspace) = match &body.source {
        ManagedImportSource::Path { path } => (path.clone(), None),
        ManagedImportSource::WilkesFile {
            workspace_id,
            root,
            path,
        } => {
            let source = manager
                .context_for(workspace_id)
                .await
                .map_err(consumer_anyhow_err)?;
            let (_, path) = source
                .export_file_path(root.clone(), path.clone(), "Managed import")
                .await
                .map_err(|error| consumer_err(ConsumerError::untyped(error)))?;
            (path, Some(source))
        }
    };
    let provenance = serde_json::to_value(&body.source).map_err(|error| {
        consumer_err(ConsumerError::untyped(format!(
            "Could not encode import provenance: {error}"
        )))
    })?;
    let exported = managed
        .import_managed_document(
            body.corpus_id.clone(),
            body.idempotency_key.clone(),
            path.clone(),
            source_workspace.clone(),
            provenance.clone(),
        )
        .await
        .map_err(consumer_err)?;
    // The document is admitted to the canonical corpus above; the spaces are
    // brought to it here. A secondary model that is slow, unavailable, or
    // broken must not decide whether a document is in the corpus — the corpus
    // is the membership authority, and a projection that cannot follow it just
    // goes on failing closed until it can. So the fan-out reports rather than
    // refuses, and whatever it could not do is left owed and retried.
    let failures = manager
        .catch_up_corpus(&body.corpus_id, source_workspace)
        .await
        .map_err(consumer_anyhow_err)?;
    for (space, error) in &failures {
        // Left owed rather than raised: the space is already unservable while
        // it is behind, and the next catch-up — the next import, or the
        // caller's own maintenance pass — is what clears it.
        tracing::warn!("embedding projection {space} did not follow the import: {error}");
    }
    let spaces = manager
        .managed_spaces(&body.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?
        .level_space_ids();
    Ok(Json(ManagedImportReply {
        document: exported,
        spaces,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocumentSnapshotBody {
    /// The corpus, and no embedding space beside it.
    ///
    /// Deliberately not a `scope`. A `scope` routes by the embedding pin, and
    /// under a corpus with a secondary space that lands on a projection
    /// workspace — which retains no sources, because a projection is the
    /// canonical rendition re-embedded rather than re-imported
    /// (`import_managed_document_into_projection` writes its file rows with no
    /// `managed_snapshot_relative_path`). The retained copy is the corpus's,
    /// and which model embedded it has nothing to do with what its bytes are.
    pub(crate) corpus_id: String,
    /// The id the import reply carried. A content hash, not an index-local
    /// handle: a re-index reissues rowids, and reissues nothing here.
    pub(crate) snapshot_id: String,
}

/// The retained bytes of one imported document, by snapshot id.
///
/// The read half of `documents/import`, addressed the way import is — by
/// corpus. A consumer that stored the snapshot id can open its own documents
/// from here rather than from a path, which is the point: the path an import
/// recorded is a path on *this* machine, and a consumer running on another one
/// was, until this route, resolving it against its own filesystem and finding
/// nothing.
///
/// Raw bytes for the same reason `/api/figure` serves them: this is a file,
/// and a base64 envelope would cost every caller a third more bytes than the
/// document weighs. The digest and the length ride on headers.
pub(crate) async fn document_snapshot_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DocumentSnapshotBody>,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(workspace_manager_unavailable)?;
    let corpus = manager
        .context_for(&body.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    let snapshot = corpus
        .managed_document_snapshot(body.snapshot_id)
        .await
        .map_err(consumer_err)?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, snapshot.media_type)
        .header("x-wilkes-source-sha256", snapshot.source_sha256)
        .header("x-wilkes-source-byte-len", snapshot.source_byte_len)
        .body(Body::from(snapshot.bytes))
        .unwrap())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocumentCoverBody {
    /// The corpus and no embedding space, as [`DocumentSnapshotBody`] says why.
    pub(crate) corpus_id: String,
    pub(crate) snapshot_id: String,
    /// The longest side of the picture, in pixels.
    pub(crate) max_edge: u32,
}

/// The first page of one imported document, small: what a list of documents
/// draws, for a caller that would otherwise be sent the whole book to draw it.
///
/// PNG bytes for the reason `/api/figure` gives. A snapshot id is a content
/// hash, so what this returns for one id never changes and a caller can keep it.
pub(crate) async fn document_cover_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DocumentCoverBody>,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(workspace_manager_unavailable)?;
    let corpus = manager
        .context_for(&body.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    let cover = corpus
        .managed_document_cover(body.snapshot_id, body.max_edge)
        .await
        .map_err(consumer_err)?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, cover.media_type)
        .header("x-wilkes-source-sha256", cover.source_sha256)
        .header("x-wilkes-image-width", cover.width)
        .header("x-wilkes-image-height", cover.height)
        .body(Body::from(cover.bytes))
        .unwrap())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManagedBackupBody {
    pub(crate) corpus_id: String,
    pub(crate) expected_embedding_space_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManagedRestoreBody {
    pub(crate) backup_name: String,
    pub(crate) expected_corpus_id: String,
    pub(crate) expected_embedding_space_id: String,
    pub(crate) expected_corpus_key: String,
}

pub(crate) async fn managed_backup_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ManagedBackupBody>,
) -> Result<Json<wilkes_api::context::ManagedCorpusBackup>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(workspace_manager_unavailable)?;
    manager
        .backup_managed_corpus(&body.corpus_id, &body.expected_embedding_space_id)
        .await
        .map(Json)
        .map_err(consumer_anyhow_err)
}

pub(crate) async fn managed_restore_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ManagedRestoreBody>,
) -> Result<Json<ManagedWorkspaceStatus>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(workspace_manager_unavailable)?;
    manager
        .restore_managed_workspace(
            &body.backup_name,
            &body.expected_corpus_id,
            &body.expected_embedding_space_id,
            &body.expected_corpus_key,
        )
        .await
        .map(Json)
        .map_err(consumer_anyhow_err)
}
