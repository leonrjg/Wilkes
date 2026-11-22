use crate::*;

pub(crate) async fn literature_provider_status_for_ctx(
    ctx: Arc<AppContext>,
    provider: String,
) -> Result<IntegrationStatus, String> {
    ctx.literature_provider_status(provider)
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn literature_provider_lookup_for_ctx(
    ctx: Arc<AppContext>,
    provider: String,
    doi: String,
) -> Result<wilkes_core::types::ProviderWork, String> {
    ctx.literature_provider_lookup(provider, doi)
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn custom_integration_summary_for_ctx(
    ctx: Arc<AppContext>,
    manifest: String,
) -> ManifestSummary {
    ctx.custom_integration_summary(manifest).await
}

pub(crate) async fn custom_integration_probe_for_ctx(
    ctx: Arc<AppContext>,
    manifest: String,
    secrets: HashMap<String, String>,
    query: String,
) -> Result<ProbeReport, String> {
    ctx.custom_integration_probe(manifest, secrets, query)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn custom_integration_summary(
    manifest: String,
    window: tauri::WebviewWindow,
) -> ManifestSummary {
    custom_integration_summary_for_ctx(window_context(&window), manifest).await
}

#[tauri::command]
pub(crate) fn custom_integration_authoring_prompt(window: tauri::WebviewWindow) -> String {
    window_context(&window).custom_integration_authoring_prompt()
}

#[tauri::command]
pub(crate) async fn custom_integration_probe(
    manifest: String,
    secrets: HashMap<String, String>,
    query: String,
    window: tauri::WebviewWindow,
) -> Result<ProbeReport, String> {
    custom_integration_probe_for_ctx(window_context(&window), manifest, secrets, query).await
}

pub(crate) fn catalogue_failed(error: wilkes_api::commands::catalogue::CatalogueError) -> String {
    format!("{error}")
}

/// Drops one catalogue's records from the mirror. Syncing it again is how it
/// comes back.
#[tauri::command]
pub(crate) async fn catalogue_remove(
    provider: String,
    window: tauri::WebviewWindow,
) -> Result<wilkes_api::commands::catalogue::CatalogueRemoveResponse, String> {
    wilkes_api::commands::catalogue::remove(&window_context(&window).catalogue_dir, provider)
        .map_err(catalogue_failed)
}

/// Every literature provider this installation knows, switched on or not.
#[tauri::command]
pub(crate) async fn literature_providers(
    window: tauri::WebviewWindow,
) -> Vec<wilkes_api::commands::integrations::literature::LiteratureProviderInfo> {
    window_context(&window).literature_providers().await
}

/// Every enabled literature provider asked at once — or only those named by
/// `providers` — each answering for itself: one that fails is reported beside
/// the ones that did not.
#[tauri::command]
pub(crate) async fn literature_search(
    query: String,
    limit: Option<usize>,
    providers: Option<Vec<String>>,
    page: Option<usize>,
    window: tauri::WebviewWindow,
) -> Result<wilkes_api::commands::integrations::literature::LiteratureSearchResponse, String> {
    window_context(&window)
        .literature_search(query, limit, providers, page)
        .await
        .map_err(|error| error.to_string())
}

/// Fetches one provider result into the workspace's uploads directory, by URL
/// or by the program its manifest names. Importing it into a library root is
/// a separate step, and the user's: this writes only into Wilkes's own staging
/// area. A download's bytes arrive on `catalogue-download-progress`.
#[tauri::command]
pub(crate) async fn literature_acquire(
    provider: String,
    result: wilkes_core::types::LiteratureSearchResult,
    window: tauri::WebviewWindow,
) -> Result<wilkes_core::acquire::AcquiredFiles, String> {
    window_context(&window)
        .literature_acquire(provider, result)
        .await
        .map_err(|error| error.to_string())
}

/// One provider's listing by filter and sort. A request its manifest does not
/// allow comes back inside the answer, beside the provider's name.
#[tauri::command]
pub(crate) async fn literature_browse(
    provider: String,
    request: wilkes_core::types::BrowseRequest,
    window: tauri::WebviewWindow,
) -> Result<wilkes_api::commands::integrations::literature::LiteratureProviderAnswer, String> {
    window_context(&window)
        .literature_browse(provider, request)
        .await
        .map_err(|error| error.to_string())
}

/// What a provider suggests for one of its filters as the user types.
#[tauri::command]
pub(crate) async fn literature_filter_suggest(
    provider: String,
    filter: String,
    text: String,
    window: tauri::WebviewWindow,
) -> Result<Vec<wilkes_core::types::FilterSuggestion>, String> {
    window_context(&window)
        .literature_filter_suggest(provider, filter, text)
        .await
        .map_err(|error| error.to_string())
}

/// Turns a request written in ordinary words into filters on each named
/// provider. `request_id` is the caller's, for `literature_compose_cancel`.
#[tauri::command]
pub(crate) async fn literature_compose(
    request_id: String,
    text: String,
    providers: Vec<String>,
    window: tauri::WebviewWindow,
) -> Result<wilkes_api::commands::integrations::literature::LiteratureComposition, String> {
    window_context(&window)
        .literature_compose(request_id, text, providers)
        .await
        .map_err(|error| error.to_string())
}

/// Stops a composition in flight; the request it belongs to answers that it
/// was cancelled.
#[tauri::command]
pub(crate) async fn literature_compose_cancel(
    request_id: String,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    window_context(&window).literature_compose_cancel(&request_id);
    Ok(())
}

/// Whether one literature provider — shipped or pasted in — is reachable now.
/// Answers whether or not it is enabled, so a form can check before enabling.
#[tauri::command]
pub(crate) async fn literature_provider_status(
    provider: String,
    window: tauri::WebviewWindow,
) -> Result<IntegrationStatus, String> {
    literature_provider_status_for_ctx(window_context(&window), provider).await
}

#[tauri::command]
pub(crate) async fn literature_provider_lookup(
    provider: String,
    doi: String,
    window: tauri::WebviewWindow,
) -> Result<wilkes_core::types::ProviderWork, String> {
    literature_provider_lookup_for_ctx(window_context(&window), provider, doi).await
}

/// Refreshes the named providers, or all of them when none is named. Minutes,
/// for all four — the settings panel names one at a time so it can show which,
/// and each page lands on `catalogue-sync-progress` as it arrives.
#[tauri::command]
pub(crate) async fn catalogue_sync(
    providers: Option<Vec<String>>,
    window: tauri::WebviewWindow,
) -> Result<wilkes_api::commands::catalogue::CatalogueSyncResponse, String> {
    window_context(&window)
        .catalogue_sync(providers)
        .await
        .map_err(catalogue_failed)
}

/// Fetches a whole MIT OCW course — its PDFs plus a generated
/// Markdown document carrying the syllabus, calendar and reading list that
/// OCW publishes only as web pages — into a directory of its own under
/// uploads. The walk and the per-document counter arrive on
/// `catalogue-course-progress`, each document's bytes on
/// `catalogue-download-progress`.
#[tauri::command]
pub(crate) async fn catalogue_acquire_course(
    course_url: String,
    window: tauri::WebviewWindow,
) -> Result<wilkes_api::commands::catalogue::CatalogueCourseResponse, String> {
    window_context(&window)
        .catalogue_acquire_course(course_url)
        .await
        .map_err(catalogue_failed)
}
