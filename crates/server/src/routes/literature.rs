use crate::*;

pub(crate) fn catalogue_err(
    error: wilkes_api::commands::catalogue::CatalogueError,
) -> (StatusCode, Json<ErrorBody>) {
    match error {
        wilkes_api::commands::catalogue::CatalogueError::Request(message) => err(message),
        wilkes_api::commands::catalogue::CatalogueError::Failed(message) => server_err(message),
    }
}

/// The installation's catalogue mirror lives in `catalogue_dir`, not the
/// workspace's `data_dir`: it describes the public world rather than any one
/// library, which is also why these routes take no corpus scope.
pub(crate) fn catalogue_dir(state: &AppState) -> std::path::PathBuf {
    state.context().catalogue_dir.clone()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogueSearchBody {
    pub(crate) queries: Vec<wilkes_api::commands::catalogue::CatalogueProbe>,
    #[serde(default = "default_catalogue_limit")]
    pub(crate) limit: usize,
}

pub(crate) fn default_catalogue_limit() -> usize {
    wilkes_api::commands::catalogue::DEFAULT_LIMIT
}

pub(crate) async fn catalogue_search_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CatalogueSearchBody>,
) -> Result<
    Json<wilkes_api::commands::catalogue::CatalogueSearchResponse>,
    (StatusCode, Json<ErrorBody>),
> {
    let dir = catalogue_dir(&state);
    // Off the async runtime: a batch of 64 queries is 64 FTS scans, and an
    // executor thread blocked on them is one not serving anything else.
    tokio::task::spawn_blocking(move || {
        wilkes_api::commands::catalogue::search(&dir, body.queries, body.limit)
    })
    .await
    .map_err(|error| server_err(format!("Catalogue search task panicked: {error}")))?
    .map(Json)
    .map_err(catalogue_err)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiteratureSearchBody {
    pub(crate) query: String,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    /// The providers to ask, or every enabled one when absent. Naming an
    /// unknown provider, or none at all, is a 400.
    #[serde(default)]
    pub(crate) providers: Option<Vec<String>>,
    /// Which page of each provider's results, 1-based. Absent is the first.
    #[serde(default)]
    pub(crate) page: Option<usize>,
}

/// Every literature provider this installation knows, switched on or not.
/// What a client offers as a provider filter before it has asked anything.
pub(crate) async fn literature_providers_handler(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<wilkes_api::commands::integrations::literature::LiteratureProviderInfo>> {
    Json(state.context().literature_providers().await)
}

/// Every enabled literature provider asked at once, or only those the body
/// names. A provider that fails is reported inside the 200 beside the ones
/// that answered; only a request that could not be run at all — an empty
/// query, a limit out of range, an unknown provider — is a 400.
pub(crate) async fn literature_search_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LiteratureSearchBody>,
) -> Result<
    Json<wilkes_api::commands::integrations::literature::LiteratureSearchResponse>,
    (StatusCode, Json<ErrorBody>),
> {
    state
        .context()
        .literature_search(body.query, body.limit, body.providers, body.page)
        .await
        .map(Json)
        .map_err(|error| err(error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiteratureAcquireBody {
    pub(crate) provider: String,
    pub(crate) result: wilkes_core::types::LiteratureSearchResult,
}

/// Fetches one provider result into uploads, however its provider says it is
/// fetched.
///
/// Not gated by `ensure_writable`, unlike `/api/upload` into the same
/// directory. The gate turns away the *user* adding documents to a library
/// another application owns; this is that application fetching into Wilkes's
/// own staging area on its way to the managed import API, which is the
/// corpus's writer by contract.
pub(crate) async fn literature_acquire_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LiteratureAcquireBody>,
) -> Result<Json<wilkes_core::acquire::AcquiredFiles>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .literature_acquire(body.provider, body.result)
        .await
        .map(Json)
        .map_err(|error| err(error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiteratureBrowseBody {
    pub(crate) provider: String,
    #[serde(default)]
    pub(crate) request: wilkes_core::types::BrowseRequest,
}

/// One provider's listing by filter and sort. A request the provider's
/// manifest does not allow is reported inside the 200, beside the provider's
/// name, like a provider that is down; only a provider that is off or does not
/// exist is a 400.
pub(crate) async fn literature_browse_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LiteratureBrowseBody>,
) -> Result<
    Json<wilkes_api::commands::integrations::literature::LiteratureProviderAnswer>,
    (StatusCode, Json<ErrorBody>),
> {
    state
        .context()
        .literature_browse(body.provider, body.request)
        .await
        .map(Json)
        .map_err(|error| err(error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiteratureSuggestBody {
    pub(crate) provider: String,
    pub(crate) filter: String,
    pub(crate) text: String,
}

/// What a provider suggests for one of its filters as the user types. A
/// filter with no suggestions, text too short to ask about, or a provider that
/// could not answer is a 400 with the reason.
pub(crate) async fn literature_suggest_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LiteratureSuggestBody>,
) -> Result<Json<Vec<wilkes_core::types::FilterSuggestion>>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .literature_filter_suggest(body.provider, body.filter, body.text)
        .await
        .map(Json)
        .map_err(|error| err(error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiteratureComposeBody {
    /// The caller's, so it can stop the composition with the cancel route.
    pub(crate) request_id: String,
    pub(crate) text: String,
    pub(crate) providers: Vec<String>,
}

/// Turns a request written in ordinary words into filters on each named
/// provider. A request that could not be composed at all — no model, an
/// unknown provider, a failed decode, a cancellation — is a 400 with the reason;
/// a provider unable to apply the composition is reported in its own entry.
pub(crate) async fn literature_compose_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LiteratureComposeBody>,
) -> Result<
    Json<wilkes_api::commands::integrations::literature::LiteratureComposition>,
    (StatusCode, Json<ErrorBody>),
> {
    state
        .context()
        .literature_compose(body.request_id, body.text, body.providers)
        .await
        .map(Json)
        .map_err(|error| err(error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiteratureComposeCancelBody {
    pub(crate) request_id: String,
}

pub(crate) async fn literature_compose_cancel_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LiteratureComposeCancelBody>,
) -> StatusCode {
    state.context().literature_compose_cancel(&body.request_id);
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiteratureProviderBody {
    pub(crate) provider: String,
}

/// Whether one literature provider — shipped or pasted in — is reachable now.
/// Answers whether or not it is enabled, so a form can check before enabling.
pub(crate) async fn literature_status_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LiteratureProviderBody>,
) -> Result<Json<IntegrationStatus>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .literature_provider_status(body.provider)
        .await
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiteratureLookupBody {
    pub(crate) provider: String,
    pub(crate) doi: String,
}

pub(crate) async fn literature_lookup_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LiteratureLookupBody>,
) -> Result<Json<wilkes_core::types::ProviderWork>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .literature_provider_lookup(body.provider, body.doi)
        .await
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogueSyncBody {
    /// Which catalogues to refresh. Absent means all of them, which is a
    /// minutes-long request; a caller wanting progress should name one at a
    /// time and drive the loop itself.
    #[serde(default)]
    pub(crate) providers: Option<Vec<String>>,
}

pub(crate) async fn catalogue_sync_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CatalogueSyncBody>,
) -> Result<
    Json<wilkes_api::commands::catalogue::CatalogueSyncResponse>,
    (StatusCode, Json<ErrorBody>),
> {
    state
        .context()
        .catalogue_sync(body.providers)
        .await
        .map(Json)
        .map_err(catalogue_err)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogueRemoveBody {
    pub(crate) provider: String,
}

pub(crate) async fn catalogue_remove_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CatalogueRemoveBody>,
) -> Result<
    Json<wilkes_api::commands::catalogue::CatalogueRemoveResponse>,
    (StatusCode, Json<ErrorBody>),
> {
    wilkes_api::commands::catalogue::remove(&catalogue_dir(&state), body.provider)
        .map(Json)
        .map_err(catalogue_err)
}

pub(crate) async fn catalogue_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<
    Json<wilkes_api::commands::catalogue::CatalogueStatusResponse>,
    (StatusCode, Json<ErrorBody>),
> {
    wilkes_api::commands::catalogue::status(&catalogue_dir(&state))
        .map(Json)
        .map_err(catalogue_err)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogueCourseBody {
    pub(crate) course_url: String,
}

/// Fetches a whole course into its own directory under uploads. Ungated for
/// the same reason `catalogue_acquire_handler` is: this is Wilkes writing into
/// its own staging area, not the user writing into a library root.
pub(crate) async fn catalogue_acquire_course_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CatalogueCourseBody>,
) -> Result<
    Json<wilkes_api::commands::catalogue::CatalogueCourseResponse>,
    (StatusCode, Json<ErrorBody>),
> {
    state
        .context()
        .catalogue_acquire_course(body.course_url)
        .await
        .map(Json)
        .map_err(catalogue_err)
}

#[derive(serde::Deserialize)]
pub(crate) struct ManifestBody {
    pub(crate) manifest: String,
}

#[derive(serde::Deserialize)]
pub(crate) struct ProbeBody {
    pub(crate) manifest: String,
    #[serde(default)]
    pub(crate) secrets: std::collections::HashMap<String, String>,
    pub(crate) query: String,
}

pub(crate) async fn custom_integration_summary_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ManifestBody>,
) -> Json<wilkes_api::commands::integrations::custom::ManifestSummary> {
    Json(
        state
            .context()
            .custom_integration_summary(body.manifest)
            .await,
    )
}

pub(crate) async fn custom_integration_authoring_prompt_handler(
    State(state): State<Arc<AppState>>,
) -> String {
    state.context().custom_integration_authoring_prompt()
}

pub(crate) async fn custom_integration_probe_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ProbeBody>,
) -> Result<Json<wilkes_core::integrations::custom::ProbeReport>, (StatusCode, Json<ErrorBody>)> {
    let report = state
        .context()
        .custom_integration_probe(body.manifest, body.secrets, body.query)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(report))
}
