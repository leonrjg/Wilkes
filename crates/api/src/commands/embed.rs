use std::path::{Path, PathBuf};
use std::sync::Arc;

use ignore::WalkBuilder;
use wilkes_core::embed::Embedder;
use wilkes_core::embed::index::{PreparedFile, SemanticIndex};
use wilkes_core::embed::installer::{EmbedProgress, EmbedderInstaller, IndexBuildProgress, ProgressTx};
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::types::IndexStatus;

/// Download and install the model for `installer`. Reports progress via `tx`.
pub async fn download_model(
    installer: &dyn EmbedderInstaller,
    data_dir: PathBuf,
    tx: ProgressTx,
) -> anyhow::Result<()> {
    installer.install(&data_dir, tx).await
}

/// Walk `root`, embed every file, and write a new `SemanticIndex` at `data_dir`.
/// Returns the `Arc<dyn Embedder>` used during the build so the caller can store
/// it in state without loading the model a second time.
///
/// Cancellation is handled by the caller via `tokio::select!` on the returned
/// future; this function runs to completion once started.
pub async fn build_index(
    root: PathBuf,
    installer: &dyn EmbedderInstaller,
    data_dir: PathBuf,
    tx: ProgressTx,
) -> anyhow::Result<Arc<dyn Embedder>> {
    let embedder: Arc<dyn Embedder> = installer.build(&data_dir)?;
    let embedder_clone = Arc::clone(&embedder);

    let paths: Vec<PathBuf> = WalkBuilder::new(&root)
        .hidden(false)
        .git_ignore(true)
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();

    let data_dir_clone = data_dir.clone();

    tokio::task::spawn_blocking(move || {
        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(PdfExtractor::new()));

        let mut idx = SemanticIndex::create(&data_dir_clone, embedder_clone.as_ref())?;
        let total = paths.len();

        let _ = tx.blocking_send(EmbedProgress::Build(IndexBuildProgress {
            files_processed: 0,
            total_files: total,
            done: false,
        }));

        // Phase 1: extract and chunk all files (no embedding yet).
        let mut file_chunks: Vec<(PathBuf, Vec<_>)> = Vec::new();
        for path in &paths {
            match SemanticIndex::extract_chunks(path, &registry) {
                Ok(chunks) if !chunks.is_empty() => file_chunks.push((path.clone(), chunks)),
                Ok(_) => {}
                Err(e) => eprintln!("Skipping {}: {e:#}", path.display()),
            }
        }

        // Phase 2: embed all chunks in one batch.
        let all_embeddings = {
            let all_texts: Vec<&str> = file_chunks
                .iter()
                .flat_map(|(_, chunks)| chunks.iter().map(|c| c.text.as_str()))
                .collect();
            if all_texts.is_empty() {
                vec![]
            } else {
                embedder_clone.embed_passages(&all_texts)?
            }
        };

        // Phase 3: pair embeddings back with their chunks and write.
        let mut emb_iter = all_embeddings.into_iter();
        for (i, (path, chunks)) in file_chunks.into_iter().enumerate() {
            let n = chunks.len();
            let prepared = PreparedFile {
                path: path.clone(),
                chunks: chunks.into_iter().zip(emb_iter.by_ref().take(n)).collect(),
            };
            if let Err(e) = idx.write_file(prepared) {
                eprintln!("Failed to write {}: {e:#}", path.display());
            }
            let _ = tx.blocking_send(EmbedProgress::Build(IndexBuildProgress {
                files_processed: i + 1,
                total_files: total,
                done: false,
            }));
        }

        let _ = tx.blocking_send(EmbedProgress::Build(IndexBuildProgress {
            files_processed: total,
            total_files: total,
            done: true,
        }));

        anyhow::Ok(())
    })
    .await??;

    Ok(embedder)
}

/// Fetch the total download size for `model_id` from the HuggingFace API.
pub async fn get_model_size(model_id: String) -> anyhow::Result<u64> {
    tokio::task::spawn_blocking(move || {
        wilkes_core::embed::candle::fetch_model_size(&model_id)
    })
    .await?
}

/// Return all candle-supported models, annotated with local cache availability.
pub async fn list_models(data_dir: &Path) -> Vec<wilkes_core::types::ModelDescriptor> {
    let data_dir = data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        wilkes_core::embed::candle::list_supported_models(&data_dir)
    })
    .await
    .unwrap_or_default()
}

/// Read index status from disk without opening the full index.
pub async fn get_index_status(data_dir: &Path) -> anyhow::Result<IndexStatus> {
    let data_dir = data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || SemanticIndex::read_status_from_path(&data_dir)).await?
}

/// Delete the index database from disk.
pub async fn delete_index(data_dir: &Path) -> anyhow::Result<()> {
    let path = data_dir.join("semantic_index.db");
    tokio::fs::remove_file(&path).await.map_err(Into::into)
}
