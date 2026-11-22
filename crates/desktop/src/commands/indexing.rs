use crate::*;

pub(crate) fn is_semantic_ready_for_ctx(ctx: Arc<AppContext>) -> bool {
    ctx.is_semantic_ready()
}

pub(crate) async fn download_model_for_ctx(
    ctx: Arc<AppContext>,
    selected: SelectedEmbedder,
) -> Result<(), String> {
    info!(
        "desktop::download_model_for_ctx: engine={}, model={}",
        selected.engine.as_str(),
        selected.model.model_id()
    );
    ctx.start_download_model(selected).await
}

pub(crate) async fn build_index_for_ctx(
    ctx: Arc<AppContext>,
    root: String,
    selected: SelectedEmbedder,
) -> Result<(), String> {
    info!(
        "desktop::build_index_for_ctx: root={}, engine={}, model={}",
        root,
        selected.engine.as_str(),
        selected.model.model_id()
    );
    Arc::clone(&ctx).start_build_index(root, selected).await
}

/// What the most recent indexing job for `root` did, document by document.
pub(crate) async fn index_activity_for_ctx(
    ctx: Arc<AppContext>,
    root: String,
) -> Result<IndexActivity, String> {
    ctx.index_activity(PathBuf::from(root)).await
}

/// How much of each root the semantic index covers.
pub(crate) async fn index_coverage_for_ctx(
    ctx: Arc<AppContext>,
    roots: Vec<String>,
) -> Result<Vec<RootCoverage>, String> {
    ctx.index_coverage(roots.into_iter().map(PathBuf::from).collect())
        .await
}

/// Index the documents the last job for `root` never reached.
pub(crate) async fn continue_index_job_for_ctx(
    ctx: Arc<AppContext>,
    root: String,
    selected: SelectedEmbedder,
) -> Result<(), String> {
    info!("desktop::continue_index_job_for_ctx: root={root}");
    Arc::clone(&ctx).continue_index_job(root, selected).await
}

/// Index the documents under `root` the semantic index does not hold.
pub(crate) async fn index_missing_documents_for_ctx(
    ctx: Arc<AppContext>,
    root: String,
    selected: SelectedEmbedder,
) -> Result<(), String> {
    info!("desktop::index_missing_documents_for_ctx: root={root}");
    Arc::clone(&ctx)
        .index_missing_documents(root, selected)
        .await
}

/// Re-attempt the documents the last job for `root` failed on. Never automatic:
/// a document that broke the reader breaks it again, and a continuation that
/// swept failures up with it would re-attempt the same one forever.
pub(crate) async fn retry_failed_documents_for_ctx(
    ctx: Arc<AppContext>,
    root: String,
    selected: SelectedEmbedder,
) -> Result<(), String> {
    info!("desktop::retry_failed_documents_for_ctx: root={root}");
    Arc::clone(&ctx)
        .retry_failed_documents(root, selected)
        .await
}

/// What this build can embed with, as one answer.
///
/// The picker used to assemble this from two calls — the engine list, then a
/// model list per engine — which meant the UI decided what a model *was* by
/// joining two replies. The manifest is that join, made once and on the side
/// that knows.
pub(crate) async fn embedder_capabilities_for_ctx(
    ctx: Arc<AppContext>,
) -> Result<EmbedderCapabilityManifest, String> {
    ctx.embedder_capabilities().await
}

pub(crate) async fn cancel_embed_for_ctx(ctx: Arc<AppContext>) -> Result<(), String> {
    info!("desktop::cancel_embed_for_ctx");
    ctx.cancel_embed().await;
    Ok(())
}

pub(crate) async fn get_index_status_for_ctx(
    ctx: Arc<AppContext>,
    root: Option<String>,
) -> Result<IndexStatus, String> {
    ctx.get_index_status(root.map(Into::into))
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn delete_index_for_ctx(
    ctx: Arc<AppContext>,
    root: Option<String>,
) -> Result<(), String> {
    ctx.delete_index(root.map(Into::into))
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn get_model_size_for_ctx_with<F, Fut>(
    engine: EmbeddingEngine,
    model_id: String,
    fetcher: F,
) -> Result<u64, String>
where
    F: FnOnce(EmbeddingEngine, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<u64>>,
{
    fetcher(engine, model_id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn is_semantic_ready(window: tauri::WebviewWindow) -> bool {
    is_semantic_ready_for_ctx(window_context(&window))
}

#[tauri::command]
pub(crate) async fn download_model(
    selected: SelectedEmbedder,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    download_model_for_ctx(window_context(&window), selected).await
}

#[tauri::command]
pub(crate) async fn build_index(
    root: String,
    selected: SelectedEmbedder,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    build_index_for_ctx(window_context(&window), root, selected).await
}

#[tauri::command]
pub(crate) async fn index_activity(
    root: String,
    window: tauri::WebviewWindow,
) -> Result<IndexActivity, String> {
    index_activity_for_ctx(window_context(&window), root).await
}

#[tauri::command]
pub(crate) async fn index_coverage(
    roots: Vec<String>,
    window: tauri::WebviewWindow,
) -> Result<Vec<RootCoverage>, String> {
    index_coverage_for_ctx(window_context(&window), roots).await
}

#[tauri::command]
pub(crate) async fn continue_index_job(
    root: String,
    selected: SelectedEmbedder,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    continue_index_job_for_ctx(window_context(&window), root, selected).await
}

#[tauri::command]
pub(crate) async fn index_missing_documents(
    root: String,
    selected: SelectedEmbedder,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    index_missing_documents_for_ctx(window_context(&window), root, selected).await
}

#[tauri::command]
pub(crate) async fn retry_failed_documents(
    root: String,
    selected: SelectedEmbedder,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    retry_failed_documents_for_ctx(window_context(&window), root, selected).await
}

#[tauri::command]
pub(crate) async fn embedder_capabilities(
    window: tauri::WebviewWindow,
) -> Result<EmbedderCapabilityManifest, String> {
    embedder_capabilities_for_ctx(window_context(&window)).await
}

#[tauri::command]
pub(crate) async fn get_model_size(
    engine: EmbeddingEngine,
    model_id: String,
) -> Result<u64, String> {
    get_model_size_for_ctx_with(engine, model_id, |engine, model_id| async move {
        wilkes_api::commands::embed::get_model_size(engine, model_id).await
    })
    .await
}

#[tauri::command]
pub(crate) async fn search_custom_models(
    engine: EmbeddingEngine,
    query: String,
) -> Result<Vec<wilkes_core::types::ModelSearchHit>, String> {
    wilkes_api::commands::embed::search_custom_models(engine, query)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn cancel_embed(window: tauri::WebviewWindow) -> Result<(), String> {
    cancel_embed_for_ctx(window_context(&window)).await
}

#[tauri::command]
pub(crate) async fn get_index_status(
    root: Option<String>,
    window: tauri::WebviewWindow,
) -> Result<IndexStatus, String> {
    get_index_status_for_ctx(window_context(&window), root).await
}

#[tauri::command]
pub(crate) async fn delete_index(
    root: Option<String>,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    delete_index_for_ctx(window_context(&window), root).await
}
