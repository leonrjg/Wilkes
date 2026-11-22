//! Backing up the managed corpus, and its snapshots, covers and exports.

use crate::context::*;

pub(crate) fn backup_managed_directory(
    data_dir: &Path,
    corpus_id: &str,
    expected_embedding_space_id: &str,
    destination: &Path,
) -> anyhow::Result<ManagedCorpusBackup> {
    backup_managed_directory_parts(
        data_dir,
        data_dir,
        corpus_id,
        expected_embedding_space_id,
        destination,
    )
}

pub(in crate::context) fn backup_managed_directory_parts(
    canonical_data_dir: &Path,
    projection_data_dir: &Path,
    corpus_id: &str,
    expected_embedding_space_id: &str,
    destination: &Path,
) -> anyhow::Result<ManagedCorpusBackup> {
    anyhow::ensure!(!destination.exists(), "backup destination already exists");
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("backup destination has no parent"))?;
    std::fs::create_dir_all(parent)?;

    let index = SemanticIndex::open_for_maintenance(projection_data_dir)?;
    let actual_space = index.embedding_space_identity()?.id().0;
    consumer_ensure!(
        actual_space == expected_embedding_space_id,
        ConsumerErrorCode::EmbeddingSpaceMismatch,
        "index={actual_space}, request={expected_embedding_space_id}",
    );
    drop(index);

    let leaf = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("managed-corpus");
    let staging = parent.join(format!(".{leaf}.partial-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&staging)?;
    let result = (|| -> anyhow::Result<ManagedCorpusBackup> {
        copy_managed_tree(
            &canonical_data_dir.join("managed_sources"),
            &staging.join("managed_sources"),
        )?;
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(canonical_data_dir.join("workspace.json"))?)?;
        if projection_data_dir != canonical_data_dir {
            let projection: serde_json::Value = serde_json::from_slice(&std::fs::read(
                projection_data_dir.join("workspace.json"),
            )?)?;
            manifest["semantic"] = projection["semantic"].clone();
        }
        std::fs::write(
            staging.join("workspace.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;

        let live_db = projection_data_dir.join("semantic_index.db");
        anyhow::ensure!(live_db.is_file(), "managed semantic index is absent");
        let snapshot_db = staging.join("semantic_index.db");
        let connection = rusqlite::Connection::open(&live_db)?;
        connection.execute(
            "VACUUM INTO ?1",
            [snapshot_db.to_string_lossy().to_string()],
        )?;

        let mut files = managed_backup_files(&staging)?;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let byte_len = files.iter().map(|file| file.byte_len).sum();
        let backup = ManagedCorpusBackup {
            format: "wilkes-managed-corpus-backup/v1".to_string(),
            corpus_id: corpus_id.to_string(),
            embedding_space_id: actual_space,
            path: destination.display().to_string(),
            file_count: files.len(),
            byte_len,
            files,
        };
        std::fs::write(
            staging.join("backup-manifest.json"),
            serde_json::to_vec_pretty(&backup)?,
        )?;
        std::fs::rename(&staging, destination)?;
        Ok(backup)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

pub(in crate::context) fn copy_managed_tree(
    source: &Path,
    destination: &Path,
) -> anyhow::Result<()> {
    anyhow::ensure!(source.is_dir(), "managed source directory is absent");
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_managed_tree(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), target)?;
        } else {
            anyhow::bail!(
                "managed backup refuses non-file entry {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

pub(crate) fn managed_backup_files(root: &Path) -> anyhow::Result<Vec<ManagedBackupFile>> {
    fn walk(root: &Path, directory: &Path, out: &mut Vec<ManagedBackupFile>) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                walk(root, &entry.path(), out)?;
            } else if file_type.is_file() {
                let relative = entry.path().strip_prefix(root)?.to_path_buf();
                let metadata = entry.metadata()?;
                out.push(ManagedBackupFile {
                    path: relative.to_string_lossy().replace('\\', "/"),
                    byte_len: metadata.len(),
                    sha256: wilkes_core::embed::identity::sha256_file(&entry.path())?,
                });
            } else {
                anyhow::bail!(
                    "managed backup refuses non-file entry {}",
                    entry.path().display()
                );
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

impl AppContext {
    /// Writes one point-in-time, self-verifying managed-corpus directory.
    /// Imports and runtime replacement are excluded for the whole snapshot;
    /// immutable sources are copied under that lock and SQLite supplies the
    /// index snapshot through `VACUUM INTO`, so WAL/checkpoint timing cannot
    /// produce a database assembled from different instants.
    pub async fn backup_managed_corpus(
        &self,
        corpus_id: String,
        expected_embedding_space_id: String,
    ) -> Result<ManagedCorpusBackup, ConsumerError> {
        let _import_guard = self.managed_import_lock.lock().await;
        let _runtime_guard = self.managed_runtime_lock.lock().await;
        let data_dir = self.data_dir.clone();
        let destination = self.shared_data_dir.join("managed_backups").join(format!(
            "{}-{}",
            corpus_id,
            uuid::Uuid::new_v4()
        ));
        tokio::task::spawn_blocking(move || {
            backup_managed_directory(
                &data_dir,
                &corpus_id,
                &expected_embedding_space_id,
                &destination,
            )
        })
        .await
        .map_err(|error| ConsumerError::untyped(format!("Managed backup task panicked: {error}")))?
        .map_err(|error| {
            ConsumerError::from_anyhow(&error.context("Could not back up managed corpus"))
        })
    }
    /// Back up canonical membership with whichever embedding projection is
    /// currently selected by Underdog. Secondary projection workspaces do not
    /// own sources, so snapshotting one alone would create an unrestorable
    /// half-corpus.
    pub async fn backup_managed_corpus_projection(
        &self,
        projection: &Arc<Self>,
        corpus_id: String,
        expected_embedding_space_id: String,
    ) -> Result<ManagedCorpusBackup, ConsumerError> {
        let _canonical_import_guard = self.managed_import_lock.lock().await;
        let _canonical_runtime_guard = self.managed_runtime_lock.lock().await;
        let _projection_import_guard = projection.managed_import_lock.lock().await;
        let _projection_runtime_guard = projection.managed_runtime_lock.lock().await;
        let canonical_data_dir = self.data_dir.clone();
        let projection_data_dir = projection.data_dir.clone();
        let destination = self.shared_data_dir.join("managed_backups").join(format!(
            "{}-{}",
            corpus_id,
            uuid::Uuid::new_v4()
        ));
        tokio::task::spawn_blocking(move || {
            backup_managed_directory_parts(
                &canonical_data_dir,
                &projection_data_dir,
                &corpus_id,
                &expected_embedding_space_id,
                &destination,
            )
        })
        .await
        .map_err(|error| ConsumerError::untyped(format!("Managed backup task panicked: {error}")))?
        .map_err(|error| {
            ConsumerError::from_anyhow(&error.context("Could not back up managed corpus"))
        })
    }
    /// What a retained snapshot is, told from its name.
    ///
    /// The extension rather than the bytes: the snapshot's name is the one the
    /// original carried, the extraction registry already keyed off it to read
    /// the document at all, and sniffing here could only disagree with the
    /// decision that produced the passages. The fallback is the honest one —
    /// `application/octet-stream` says "bytes, and I am not telling you what
    /// kind", which is exactly what is known.
    pub(in crate::context) fn snapshot_media_type(path: &Path) -> &'static str {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "pdf" => "application/pdf",
            "md" | "markdown" => "text/markdown",
            "txt" => "text/plain",
            _ => "application/octet-stream",
        }
    }
    /// The retained bytes of one imported document.
    ///
    /// The counterpart to `import_managed_document`: import hands back a
    /// snapshot id and the passages carved out of it, and this hands back what
    /// those passages were carved *from*. A consumer that stored the id can
    /// therefore read its own documents without holding a path — which matters
    /// because the path a registration recorded is a path on this machine, and
    /// a consumer running somewhere else has no filesystem to resolve it
    /// against and no business trying.
    ///
    /// Only the retained copy is served. Falling back to the original when the
    /// snapshot is gone would answer a question nobody asked — the original
    /// may have been edited since, and bytes that no longer match the
    /// rendition are worse than an error, because they look like an answer.
    ///
    /// Call this on the **canonical corpus** context, never on one reached
    /// through an embedding pin. A pin routes to the space's workspace, and a
    /// secondary space is a projection whose file rows carry no retained path
    /// at all — it holds the canonical rendition re-embedded, not a second
    /// import. Asking it for a document's bytes is asking a coordinate system
    /// what a book says.
    pub async fn managed_document_snapshot(
        &self,
        snapshot_id: String,
    ) -> Result<DocumentSnapshotExport, ConsumerError> {
        let index_arc = self.index.lock().clone();
        let data_dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            let (path, source_sha256) =
                Self::retained_snapshot(&index_arc, &data_dir, &snapshot_id)?;
            let bytes = std::fs::read(&path).map_err(|error| {
                ConsumerError::new(
                    ConsumerErrorCode::DocumentIndexIncomplete,
                    format!("Retained snapshot {snapshot_id} could not be read: {error}"),
                )
            })?;
            Ok(DocumentSnapshotExport {
                media_type: Self::snapshot_media_type(&path),
                source_byte_len: bytes.len() as u64,
                source_sha256,
                bytes,
            })
        })
        .await
        .map_err(|error| {
            ConsumerError::untyped(format!("Managed snapshot task panicked: {error}"))
        })?
    }
    /// The first page of one imported document, small.
    ///
    /// What a list of documents needs, and the reason not to send it
    /// [`Self::managed_document_snapshot`]: a whole book to draw one page the
    /// size of a thumbnail. Addressed the same way and subject to the same
    /// rule, so call it on the canonical corpus context.
    ///
    /// Drawn by [`Self::file_cover`], the one cover path: a retained snapshot
    /// is a file on disk like any other, and an immutable one, so the cover
    /// cache keyed on its identity never goes stale. The index lock is held
    /// only to find the file — it serves every search in this corpus, and a
    /// list of documents asks for a dozen of these at once.
    pub async fn managed_document_cover(
        &self,
        snapshot_id: String,
        max_edge: u32,
    ) -> Result<DocumentCoverExport, ConsumerError> {
        let index_arc = self.index.lock().clone();
        let data_dir = self.data_dir.clone();
        let (path, source_sha256) = tokio::task::spawn_blocking(move || {
            Self::retained_snapshot(&index_arc, &data_dir, &snapshot_id)
        })
        .await
        .map_err(|error| ConsumerError::untyped(format!("Snapshot lookup panicked: {error}")))??;
        let png = self
            .file_cover(path, max_edge)
            .await
            .map_err(|error| ConsumerError::from_anyhow(&error))?;
        let (width, height) = wilkes_core::figure::png_dimensions(&png)
            .map_err(|error| ConsumerError::from_anyhow(&error))?;
        Ok(DocumentCoverExport {
            bytes: png,
            media_type: "image/png",
            width,
            height,
            source_sha256,
        })
    }
    /// Where a retained snapshot is on disk, and what it hashed to.
    ///
    /// The index guard does not outlive this call, so a caller can read or
    /// render the file without holding up the index.
    pub(in crate::context) fn retained_snapshot(
        index: &Mutex<Option<SemanticIndex>>,
        data_dir: &Path,
        snapshot_id: &str,
    ) -> Result<(PathBuf, String), ConsumerError> {
        let guard = index
            .lock()
            .map_err(|_| ConsumerError::untyped("Semantic index lock was poisoned"))?;
        let index = guard
            .as_ref()
            .ok_or_else(|| ConsumerError::untyped("Semantic index unavailable"))?;
        let source = index
            .managed_snapshot_source(snapshot_id)
            .map_err(|error| ConsumerError::from_anyhow(&error))?
            .ok_or_else(|| {
                ConsumerError::new(
                    ConsumerErrorCode::DocumentIndexIncomplete,
                    format!("This corpus retains no snapshot {snapshot_id}."),
                )
            })?;
        Ok((
            data_dir.join(&source.managed_snapshot_relative_path),
            source.source_sha256,
        ))
    }
    /// Async because of the outline, which is a full read of the snapshot.
    ///
    /// Reading a document reaches the layout detector and the recognizers
    /// through their worker proxies, and a proxy waits for the subprocess'
    /// reply by blocking on the runtime handle. `Handle::block_on` panics on
    /// one of the runtime's own threads, so extraction has a requirement no
    /// signature states: it does not run inside an async task. Every other
    /// extraction in this file satisfies it with `spawn_blocking`; this one
    /// did not, because the reading was hidden behind a plain call to
    /// `document_outline` in a synchronous helper, and every managed import
    /// that reached it panicked the moment the document had a page worth
    /// detecting on.
    pub(in crate::context) async fn managed_export(
        corpus_id: String,
        document: ManagedDocumentData,
        snapshot_path: PathBuf,
        reused: bool,
    ) -> Result<ManagedDocumentExport, ConsumerError> {
        let source_byte_len = std::fs::metadata(&snapshot_path)
            .map_err(|error| format!("Could not read retained snapshot metadata: {error}"))?
            .len();
        let media_type = Self::snapshot_media_type(&snapshot_path).to_string();
        let outline_path = snapshot_path.clone();
        let declared_outline = tokio::task::spawn_blocking(move || {
            let registry = wilkes_core::extract::production_registry();
            wilkes_core::extract::document_outline(&outline_path, &registry)
        })
        .await
        .map_err(|error| format!("Managed outline task panicked: {error}"))?
        .map_err(|error| format!("Could not read retained document outline: {error:#}"))?;
        let outline = resolve_outline(&declared_outline.entries, &document.chunks);
        let chunk_count = document.chunks.len();
        Ok(ManagedDocumentExport {
            corpus_id,
            source_sha256: document.source_sha256,
            source_byte_len,
            media_type,
            snapshot_id: document.snapshot_id.0,
            rendition_id: document.rendition_id.0,
            extraction_recipe_id: document.extraction_recipe_id,
            extracted_content_sha256: document.extracted_content_sha256,
            chunk_count,
            outline,
            extraction: declared_outline.diagnostics,
            chunks: document.chunks.into_iter().map(ChunkExport::from).collect(),
            embedding_work: ManagedEmbeddingWork {
                chunks: chunk_count,
                reused: if reused { chunk_count } else { 0 },
                computed: if reused { 0 } else { chunk_count },
            },
        })
    }
}
