use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use agent_client_protocol::schema::v1::{HttpHeader, McpServer, McpServerHttp};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
    Router,
};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, ServerHandler,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use wilkes_core::integrations::{
    openalex::OpenAlexClient, semantic_scholar::SemanticScholarClient,
};
use wilkes_core::types::IntegrationsSettings;

use crate::{
    context::{ActiveDoc, ContextFile},
    reader,
    search::SearchService,
    session::{read_access_error, ContextStateHandle},
};

const DEFAULT_TEXT_CHAR_LIMIT: usize = 24_000;
const MAX_TEXT_CHAR_LIMIT: usize = 120_000;
const DEFAULT_SEARCH_MAX_RESULTS: usize = 10;
const MAX_SEARCH_MAX_RESULTS: usize = 50;
const DEFAULT_SEARCH_CONTEXT_LINES: u32 = 2;
const MAX_SEARCH_CONTEXT_LINES: u32 = 5;
const DEFAULT_RELATED_DOCUMENTS_LIMIT: usize = 8;
const MAX_RELATED_DOCUMENTS_LIMIT: usize = 25;
const DEFAULT_SEARCH_MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024;
const SEMANTIC_INDEX_GUIDANCE: &str = "The user can enable the semantic index in Wilkes Settings. Use exact search with mode='exact' instead in the meantime.";

/// Names of the read-only tools this server exposes. Shared with the permission
/// boundary in `session.rs` so calls to Wilkes's *own* MCP server are
/// auto-allowed without ever prompting the user (they are the Q&A pane's own
/// internal plumbing). Mutating tools such as `download` must not be added here,
/// so the agent's normal permission flow remains in effect.
pub(crate) const WILKES_MCP_TOOL_NAMES: &[&str] = &[
    "list_context",
    "get_document_text",
    "get_related_documents",
    "search",
    "literature_search",
];

pub(crate) struct McpRuntime {
    url: String,
    token: Arc<RwLock<Option<String>>>,
    shutdown: CancellationToken,
    _server_task: tokio::task::JoinHandle<()>,
}

impl McpRuntime {
    pub(crate) fn server_config(&self) -> McpServer {
        let token = self
            .token
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let server = McpServerHttp::new("wilkes", self.url.clone());
        McpServer::Http(match token.as_ref() {
            Some(token) => server.headers(vec![HttpHeader::new(
                "Authorization",
                format!("Bearer {token}"),
            )]),
            None => server,
        })
    }

    async fn shutdown(mut self) {
        self.shutdown.cancel();
        let _ = (&mut self._server_task).await;
    }
}

impl Drop for McpRuntime {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

pub(crate) async fn start(
    context: ContextStateHandle,
    cwd: PathBuf,
    search: Option<Arc<dyn SearchService>>,
    integrations: IntegrationsSettings,
) -> anyhow::Result<McpRuntime> {
    let token = uuid::Uuid::new_v4().to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    start_server(
        listener,
        format!("/mcp/{token}"),
        Some(token),
        McpContext::Session(context),
        cwd,
        search,
        Some(integrations),
        HostValidation::LoopbackOnly,
        "chat",
    )
    .await
}

/// Application-scoped MCP server for regular Claude Code and Codex clients.
///
/// Unlike the private chat server, this endpoint has no active-document or
/// per-conversation context. Its readable scope is resolved dynamically from
/// [`SearchService::library_roots`], so settings changes take effect without
/// restarting the listener.
pub struct ExternalMcpRuntime {
    runtime: McpRuntime,
}

impl ExternalMcpRuntime {
    pub fn url(&self) -> &str {
        &self.runtime.url
    }

    pub fn set_token(&self, token: Option<String>) {
        *self
            .runtime
            .token
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = token;
    }

    pub fn token(&self) -> Option<String> {
        self.runtime
            .token
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub async fn shutdown(self) {
        self.runtime.shutdown().await;
    }
}

pub async fn start_external(
    bind_address: IpAddr,
    port: u16,
    token: Option<String>,
    search: Arc<dyn SearchService>,
) -> anyhow::Result<ExternalMcpRuntime> {
    if port == 0 {
        anyhow::bail!("External MCP port must be between 1 and 65535");
    }
    let listener = tokio::net::TcpListener::bind((bind_address, port)).await?;
    let runtime = start_server(
        listener,
        "/mcp".to_string(),
        token,
        McpContext::Library,
        PathBuf::new(),
        Some(search),
        None,
        HostValidation::Any,
        "external",
    )
    .await?;
    Ok(ExternalMcpRuntime { runtime })
}

#[derive(Clone, Copy, Debug)]
enum HostValidation {
    LoopbackOnly,
    Any,
}

#[derive(Clone, Debug)]
enum McpContext {
    Session(ContextStateHandle),
    Library,
}

async fn start_server(
    listener: tokio::net::TcpListener,
    route_path: String,
    token: Option<String>,
    context: McpContext,
    cwd: PathBuf,
    search: Option<Arc<dyn SearchService>>,
    integrations: Option<IntegrationsSettings>,
    host_validation: HostValidation,
    lifecycle: &'static str,
) -> anyhow::Result<McpRuntime> {
    let token = Arc::new(RwLock::new(token));
    let shutdown = CancellationToken::new();
    let config = rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_cancellation_token(shutdown.child_token());
    let config = match host_validation {
        HostValidation::LoopbackOnly => config,
        HostValidation::Any => config.disable_allowed_hosts(),
    };
    let service: rmcp::transport::streamable_http_server::StreamableHttpService<
        WilkesMcp,
        rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
    > = rmcp::transport::streamable_http_server::StreamableHttpService::new(
        move || {
            Ok(WilkesMcp::new(
                context.clone(),
                cwd.clone(),
                search.clone(),
                integrations.clone(),
            ))
        },
        Default::default(),
        config,
    );

    let router =
        Router::new()
            .nest_service(&route_path, service)
            .layer(middleware::from_fn_with_state(
                Arc::clone(&token),
                require_bearer_token,
            ));
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}{route_path}");
    let shutdown_for_task = shutdown.clone();
    let server_task = tokio::spawn(async move {
        let result = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown_for_task.cancelled_owned().await })
            .await;
        if let Err(err) = result {
            error!(%lifecycle, "Wilkes MCP server exited with error: {err:#}");
        }
    });

    info!(%url, %lifecycle, "started Wilkes MCP server");
    Ok(McpRuntime {
        url,
        token,
        shutdown,
        _server_task: server_task,
    })
}

async fn require_bearer_token(
    State(token): State<Arc<RwLock<Option<String>>>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected = {
        let token = token
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        token.as_ref().map(|token| format!("Bearer {token}"))
    };
    let Some(expected) = expected else {
        return Ok(next.run(request).await);
    };
    let authorized = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected);
    if authorized {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[derive(Clone)]
struct WilkesMcp {
    context: McpContext,
    cwd: PathBuf,
    search: Option<Arc<dyn SearchService>>,
    integrations: Option<IntegrationsSettings>,
    tool_router: ToolRouter<Self>,
}

impl WilkesMcp {
    fn new(
        context: McpContext,
        cwd: PathBuf,
        search: Option<Arc<dyn SearchService>>,
        integrations: Option<IntegrationsSettings>,
    ) -> Self {
        Self {
            context,
            cwd,
            search,
            integrations,
            tool_router: Self::tool_router(),
        }
    }
}

impl std::fmt::Debug for WilkesMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WilkesMcp")
            .field("context", &self.context)
            .field("cwd", &self.cwd)
            .field("search", &self.search.as_ref().map(|_| "SearchService"))
            .field("integrations", &self.integrations)
            .finish_non_exhaustive()
    }
}

impl WilkesMcp {
    async fn context_snapshot(&self) -> crate::session::ContextSnapshot {
        match &self.context {
            McpContext::Session(context) => context.snapshot(),
            McpContext::Library => {
                let root = match &self.search {
                    Some(search) => search.clone().default_root().await,
                    None => None,
                };
                crate::session::ContextSnapshot {
                    active_doc: None,
                    context_files: Vec::new(),
                    root: crate::context::root_context(root.as_deref()),
                    branch_history: None,
                }
            }
        }
    }

    async fn library_roots(&self) -> Vec<PathBuf> {
        match &self.search {
            Some(search) => search.clone().library_roots().await,
            None => Vec::new(),
        }
    }

    async fn is_path_allowed(&self, path: &Path) -> bool {
        match &self.context {
            McpContext::Session(context) => context.is_allowed(path),
            McpContext::Library => is_within_roots(path, &self.library_roots().await),
        }
    }

    async fn current_root(&self) -> Result<PathBuf, String> {
        match &self.context {
            McpContext::Session(context) => {
                if let Some(root) = context.search_root() {
                    return Ok(root);
                }
                if let Some(search) = &self.search {
                    if let Some(root) = search.clone().default_root().await {
                        return Ok(root);
                    }
                }
                Ok(self.cwd.clone())
            }
            McpContext::Library => {
                let search = self
                    .search
                    .clone()
                    .ok_or_else(|| "Wilkes search is not available.".to_string())?;
                search.default_root().await.ok_or_else(|| {
                    "No current Wilkes library root is configured. Open a directory in Wilkes first."
                        .to_string()
                })
            }
        }
    }

    async fn integrations(&self) -> IntegrationsSettings {
        if let Some(integrations) = &self.integrations {
            return integrations.clone();
        }
        match &self.search {
            Some(search) => search.clone().integrations().await,
            None => IntegrationsSettings::default(),
        }
    }
}

fn is_within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    let Ok(canonical) = path.canonicalize() else {
        return false;
    };
    roots.iter().any(|root| {
        root.canonicalize()
            .map(|root| canonical.starts_with(root))
            .unwrap_or(false)
    })
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetDocumentTextParams {
    path: Option<String>,
    page: Option<u32>,
    page_range: Option<PageRange>,
    max_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchParams {
    /// Text to search for.
    query: String,
    /// Required. Use exact for literal/regex matching, semantic for meaning-based search.
    mode: SearchModeParam,
    /// Search location. Use all for a library-wide search; omit for the current root.
    scope: Option<SearchScopeParam>,
    /// Corpus/index root. Omit unless searching a different root is intentional.
    root: Option<String>,
    /// Restrict search to this single file inside root. Use this for questions
    /// about the open/current document or a concrete context document.
    file: Option<String>,
    /// Maximum matches to return.
    max_results: Option<usize>,
    /// Exact search only.
    case_sensitive: Option<bool>,
    /// Exact search only.
    is_regex: Option<bool>,
    /// Exact search context lines.
    context_lines: Option<u32>,
    /// Optional saved smart collection ID to intersect with the chosen scope.
    collection_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetRelatedDocumentsParams {
    /// Document to find related documents for. Omit to use the active document.
    path: Option<String>,
    /// Search location. Use all for the whole library; omit for the current root.
    scope: Option<SearchScopeParam>,
    /// Corpus/index root. Omit unless using a different root is intentional.
    root: Option<String>,
    /// Maximum related documents to return (1-25, default 8).
    limit: Option<usize>,
    /// Optional saved smart collection ID to constrain returned documents.
    collection_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LiteratureSearchParams {
    /// Scholarly works search query.
    query: String,
    /// Enabled literature provider to use.
    provider: LiteratureProviderParam,
    /// Maximum works to return (1-100, default 10).
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DownloadParams {
    /// Direct HTTP(S) URL of the file to download.
    url: String,
    /// File name inside the current Wilkes root. Must not contain directories.
    filename: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LiteratureProviderParam {
    SemanticScholar,
    Openalex,
}

#[derive(Debug, Serialize)]
struct LiteratureSearchResponse<T> {
    query: String,
    provider: LiteratureProviderParam,
    results: Vec<T>,
}

#[derive(Debug, Serialize)]
struct DownloadResponse {
    path: String,
    bytes: usize,
    already_present: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SearchModeParam {
    Exact,
    Semantic,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SearchScopeParam {
    All,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, schemars::JsonSchema, PartialEq, Eq)]
struct PageRange {
    #[serde(alias = "start_page")]
    start: u32,
    #[serde(alias = "end_page")]
    end: u32,
}

#[derive(Debug, Serialize)]
struct ListContextResponse {
    current_root: Option<String>,
    roots: Vec<String>,
    first_files: Vec<String>,
    active_doc: Option<ActiveDocInfo>,
    context_files: Vec<ContextFileInfo>,
}

#[derive(Debug, Serialize)]
struct ActiveDocInfo {
    path: String,
    page: Option<u32>,
}

#[derive(Debug, Serialize)]
struct ContextFileInfo {
    path: String,
    pages: Option<u32>,
}

#[derive(Debug, Serialize)]
struct GetDocumentTextResponse {
    path: String,
    page: Option<u32>,
    page_range: Option<PageRange>,
    text: String,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    query: String,
    mode: SearchModeParam,
    root: String,
    file: Option<String>,
    matches: Vec<SearchFileResponse>,
    stats: wilkes_core::types::SearchStats,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct GetRelatedDocumentsResponse {
    path: String,
    root: String,
    scope: SearchScopeParamResponse,
    documents: Vec<wilkes_core::types::RelatedDocument>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SearchScopeParamResponse {
    CurrentRoot,
    All,
}

#[derive(Debug, Serialize)]
struct SearchFileResponse {
    path: String,
    file_type: wilkes_core::types::FileType,
    matches: Vec<SearchMatchResponse>,
}

#[derive(Debug, Serialize)]
struct SearchMatchResponse {
    text: String,
    line: Option<u32>,
    page: Option<u32>,
    score: Option<f32>,
}

#[tool_router]
impl WilkesMcp {
    #[tool(
        description = "List the current Wilkes context: configured library roots, active root, and, for an in-app chat, its active document and explicitly added files."
    )]
    async fn list_context(&self) -> CallToolResult {
        let snapshot = self.context_snapshot().await;
        let roots = self.library_roots().await;
        structured(ListContextResponse::from_snapshot(
            snapshot.root,
            snapshot.active_doc,
            snapshot.context_files,
            roots,
        ))
    }

    #[tool(
        description = "List saved Wilkes smart collections and their IDs for collection-scoped searches."
    )]
    async fn list_smart_collections(&self) -> CallToolResult {
        match &self.search {
            Some(search) => match search.clone().list_smart_collections().await {
                Ok(collections) => structured(collections),
                Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
            },
            None => structured(Vec::<wilkes_core::types::SmartCollection>::new()),
        }
    }

    #[tool(
        description = "Read Wilkes-extracted document text from the active document, a context file, or any document in the current root. Use page for one PDF page or page_range for an inclusive PDF page range."
    )]
    async fn get_document_text(
        &self,
        Parameters(params): Parameters<GetDocumentTextParams>,
    ) -> CallToolResult {
        match get_document_text_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Find documents semantically related to a document in Wilkes. Omit path to use the active document. Set scope='all' to search the whole library; otherwise results are limited to the current root."
    )]
    async fn get_related_documents(
        &self,
        Parameters(params): Parameters<GetRelatedDocumentsParams>,
    ) -> CallToolResult {
        match get_related_documents_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Search Wilkes-readable documents. You must explicitly set mode='exact' for literal/regex text search or mode='semantic' for meaning-based search; mode has no default. If the user asks about the open/current document or a specific context file, set file to that document path; omit file only for corpus-wide searches."
    )]
    async fn search(&self, Parameters(params): Parameters<SearchParams>) -> CallToolResult {
        match search_documents_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Search scholarly literature using an enabled external integration. Set provider='semantic_scholar' or provider='openalex'."
    )]
    async fn literature_search(
        &self,
        Parameters(params): Parameters<LiteratureSearchParams>,
    ) -> CallToolResult {
        let query = params.query.trim().to_string();
        if query.is_empty() {
            return CallToolResult::error(vec![ContentBlock::text(
                "Literature search query cannot be empty.",
            )]);
        }
        let limit = params
            .limit
            .unwrap_or(DEFAULT_SEARCH_MAX_RESULTS)
            .clamp(1, 100);
        let integrations = self.integrations().await;
        match params.provider {
            LiteratureProviderParam::SemanticScholar => {
                let settings = &integrations.semantic_scholar;
                if !settings.enabled {
                    return CallToolResult::error(vec![ContentBlock::text(
                        "Semantic Scholar integration is disabled.",
                    )]);
                }
                match SemanticScholarClient::from_settings(settings)
                    .search(&query, limit)
                    .await
                {
                    Ok(results) => structured(LiteratureSearchResponse {
                        query,
                        provider: params.provider,
                        results,
                    }),
                    Err(error) => {
                        CallToolResult::error(vec![ContentBlock::text(error.to_string())])
                    }
                }
            }
            LiteratureProviderParam::Openalex => {
                let settings = &integrations.openalex;
                if !settings.enabled {
                    return CallToolResult::error(vec![ContentBlock::text(
                        "OpenAlex integration is disabled.",
                    )]);
                }
                match OpenAlexClient::from_settings(settings)
                    .search(&query, limit)
                    .await
                {
                    Ok(results) => structured(LiteratureSearchResponse {
                        query,
                        provider: params.provider,
                        results,
                    }),
                    Err(error) => {
                        CallToolResult::error(vec![ContentBlock::text(error.to_string())])
                    }
                }
            }
        }
    }

    #[tool(
        description = "Download a direct HTTP(S) file URL into the current Wilkes root. Use only when the user asks to import or download a file. Pass filename to choose the saved name; existing files are never overwritten. Literature search results may provide pdf_url values suitable for this tool."
    )]
    async fn download(&self, Parameters(params): Parameters<DownloadParams>) -> CallToolResult {
        let root = match self.current_root().await {
            Ok(root) => root,
            Err(message) => {
                return CallToolResult::error(vec![ContentBlock::text(message)]);
            }
        };
        match download_to_root(&root, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WilkesMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("wilkes", env!("CARGO_PKG_VERSION"))
                    .with_title("Wilkes")
                    .with_description("Local document search and literature tools"),
            )
            .with_instructions("Wilkes document context and literature tools. Context and search tools are read-only. The download tool writes a file into the current root and must only be used when the user asks to import or download it.")
    }
}

async fn download_to_root(root: &Path, params: DownloadParams) -> Result<DownloadResponse, String> {
    if !root.is_dir() {
        return Err(format!(
            "Current Wilkes root does not exist: {}",
            root.display()
        ));
    }
    let url = reqwest::Url::parse(params.url.trim())
        .map_err(|error| format!("Invalid download URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Download URL must use HTTP or HTTPS.".to_string());
    }
    let filename = params
        .filename
        .or_else(|| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back())
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "download.pdf".to_string());
    let filename_path = Path::new(&filename);
    if filename_path.components().count() != 1
        || !matches!(
            filename_path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err("filename must be a single file name without directories.".to_string());
    }
    let target = root.join(filename_path);
    if target.exists() {
        return Err(format!(
            "Refusing to overwrite existing file: {}",
            target.display()
        ));
    }

    let response = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|error| format!("Download failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Download failed: {error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOWNLOAD_BYTES as u64)
    {
        return Err(format!(
            "Download exceeds the {} MiB limit.",
            MAX_DOWNLOAD_BYTES / 1024 / 1024
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("Failed to read download: {error}"))?;
    if bytes.len() > MAX_DOWNLOAD_BYTES {
        return Err(format!(
            "Download exceeds the {} MiB limit.",
            MAX_DOWNLOAD_BYTES / 1024 / 1024
        ));
    }
    if let Some(existing) = find_file_with_content(root, &target, &bytes)? {
        return Ok(DownloadResponse {
            path: display_path(&existing),
            bytes: bytes.len(),
            already_present: true,
        });
    }
    std::fs::write(&target, &bytes)
        .map_err(|error| format!("Failed to save {}: {error}", target.display()))?;
    Ok(DownloadResponse {
        path: display_path(&target),
        bytes: bytes.len(),
        already_present: false,
    })
}

/// Find an existing regular file with exactly the downloaded content. Size is
/// the cheap prefilter; SHA-256 is only computed for equal-size candidates.
/// Symlinked directories are not followed, keeping the search inside `root`.
fn find_file_with_content(
    root: &Path,
    target: &Path,
    downloaded: &[u8],
) -> Result<Option<PathBuf>, String> {
    let expected_len = u64::try_from(downloaded.len()).unwrap_or(u64::MAX);
    let expected_digest = Sha256::digest(downloaded);
    let mut directories = vec![root.to_path_buf()];

    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| format!("Failed to inspect {}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "Failed to inspect an entry in {}: {error}",
                    directory.display()
                )
            })?;
            let file_type = entry.file_type().map_err(|error| {
                format!("Failed to inspect {}: {error}", entry.path().display())
            })?;
            if file_type.is_dir() {
                directories.push(entry.path());
                continue;
            }
            if !file_type.is_file() || entry.path() == target {
                continue;
            }
            let path = entry.path();
            let metadata = entry
                .metadata()
                .map_err(|error| format!("Failed to inspect {}: {error}", path.display()))?;
            if metadata.len() != expected_len {
                continue;
            }
            let candidate = std::fs::read(&path)
                .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
            if Sha256::digest(&candidate) == expected_digest {
                return Ok(Some(path));
            }
        }
    }

    Ok(None)
}

impl ListContextResponse {
    fn from_snapshot(
        root: crate::context::RootContext,
        active_doc: Option<ActiveDoc>,
        context_files: Vec<ContextFile>,
        roots: Vec<PathBuf>,
    ) -> Self {
        Self {
            current_root: root.path.map(|path| path.to_string_lossy().into_owned()),
            roots: roots.into_iter().map(|path| display_path(&path)).collect(),
            first_files: root
                .first_files
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            active_doc: active_doc.map(|doc| ActiveDocInfo {
                path: doc.path,
                page: doc.page,
            }),
            context_files: context_files
                .into_iter()
                .map(|file| ContextFileInfo {
                    path: file.path,
                    pages: file.pages,
                })
                .collect(),
        }
    }
}

async fn get_document_text_for_mcp(
    mcp: &WilkesMcp,
    params: GetDocumentTextParams,
) -> Result<GetDocumentTextResponse, String> {
    match &mcp.context {
        McpContext::Session(context) => get_document_text(context, params),
        McpContext::Library => {
            let path = params
                .path
                .as_ref()
                .filter(|path| !path.trim().is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| {
                    "External Wilkes MCP has no active document; pass path explicitly.".to_string()
                })?;
            if !mcp.is_path_allowed(&path).await {
                return Err(read_access_error(&path));
            }
            get_document_text_at_path(path, None, params)
        }
    }
}

fn get_document_text(
    context: &ContextStateHandle,
    params: GetDocumentTextParams,
) -> Result<GetDocumentTextResponse, String> {
    let snapshot = context.snapshot();
    let (path, default_page) = match params.path.as_ref() {
        Some(path) => (PathBuf::from(path), None),
        None => {
            let active_doc = snapshot
                .active_doc
                .ok_or_else(|| "No active Wilkes document is available.".to_string())?;
            (PathBuf::from(active_doc.path), active_doc.page)
        }
    };

    if params.page.is_some() && params.page_range.is_some() {
        return Err("Pass either page or page_range, not both.".to_string());
    }
    if !context.is_allowed(&path) {
        return Err(read_access_error(&path));
    }
    get_document_text_at_path(path, default_page, params)
}

fn get_document_text_at_path(
    path: PathBuf,
    default_page: Option<u32>,
    params: GetDocumentTextParams,
) -> Result<GetDocumentTextResponse, String> {
    let started_at = Instant::now();
    let page_range = match (params.page, params.page_range) {
        (Some(page), None) => Some((page, page)),
        (None, Some(range)) => Some((range.start, range.end)),
        (None, None) => default_page.map(|page| (page, page)),
        (Some(_), Some(_)) => return Err("Pass either page or page_range, not both.".to_string()),
    };
    let read_started_at = Instant::now();
    let text = reader::read_text_range(&path, page_range, None, None).map_err(|err| {
        info!(
            path = %path.display(),
            page_range = ?page_range,
            error = %err,
            elapsed_ms = started_at.elapsed().as_millis(),
            "chat: get_document_text failed"
        );
        format!("Failed to extract text from {}: {err:#}", path.display())
    })?;
    let read_elapsed_ms = read_started_at.elapsed().as_millis();
    let max_chars = params
        .max_chars
        .unwrap_or(DEFAULT_TEXT_CHAR_LIMIT)
        .min(MAX_TEXT_CHAR_LIMIT);
    let excerpt = reader::limit_excerpt(&text, max_chars);
    let response = GetDocumentTextResponse {
        path: display_path(&path),
        page: page_range.and_then(|(start, end)| (start == end).then_some(start)),
        page_range: page_range.and_then(|(start, end)| {
            (start != end).then_some(PageRange {
                start: start.min(end),
                end: start.max(end),
            })
        }),
        text: excerpt.text,
        truncated: excerpt.truncated,
    };
    let serialized_bytes = serde_json::to_vec(&response).map(|bytes| bytes.len()).ok();
    info!(
        path = %path.display(),
        page_range = ?page_range,
        read_elapsed_ms,
        elapsed_ms = started_at.elapsed().as_millis(),
        extracted_bytes = text.len(),
        response_text_bytes = response.text.len(),
        response_serialized_bytes = ?serialized_bytes,
        truncated = response.truncated,
        "chat: get_document_text completed"
    );
    Ok(response)
}

async fn search_documents_for_mcp(
    mcp: &WilkesMcp,
    mut params: SearchParams,
) -> Result<SearchResponse, String> {
    match &mcp.context {
        McpContext::Session(context) => {
            search_documents(context, mcp.search.clone(), &mcp.cwd, params).await
        }
        McpContext::Library => {
            let root = match params.root.as_ref() {
                Some(root) if !root.trim().is_empty() => PathBuf::from(root),
                Some(_) => return Err("Search root cannot be empty.".to_string()),
                None => mcp.current_root().await?,
            };
            if !is_within_roots(&root, &mcp.library_roots().await) {
                return Err(read_access_error(&root));
            }
            if let Some(file) = params.file.as_ref() {
                let file = PathBuf::from(file);
                if !mcp.is_path_allowed(&file).await {
                    return Err(read_access_error(&file));
                }
            }
            params.root = Some(root.to_string_lossy().into_owned());
            search_documents(
                &ContextStateHandle::default(),
                mcp.search.clone(),
                Path::new(""),
                params,
            )
            .await
        }
    }
}

async fn search_documents(
    context: &ContextStateHandle,
    search: Option<Arc<dyn SearchService>>,
    cwd: &Path,
    mut params: SearchParams,
) -> Result<SearchResponse, String> {
    let search =
        search.ok_or_else(|| "Wilkes search is not available in this session.".to_string())?;
    let mode = params.mode;
    let root = match params.root.take() {
        Some(root) => PathBuf::from(root),
        None => match context.search_root() {
            Some(root) => root,
            None => search
                .clone()
                .default_root()
                .await
                .unwrap_or_else(|| cwd.to_path_buf()),
        },
    };
    let (query, max_files) = build_search_query(root, params)?;
    let root = display_path(&query.root);
    let file = match &query.scope {
        wilkes_core::types::SearchScope::Corpus | wilkes_core::types::SearchScope::All => None,
        wilkes_core::types::SearchScope::File { path } => Some(display_path(path)),
    };
    let query_text = query.pattern.clone();
    let collected = search
        .search(query, max_files)
        .await
        .map_err(|message| match mode {
            SearchModeParam::Semantic => with_semantic_index_guidance(message),
            SearchModeParam::Exact => message,
        })?;

    Ok(SearchResponse {
        query: query_text,
        mode,
        root,
        file,
        matches: collected
            .files
            .into_iter()
            .map(SearchFileResponse::from)
            .collect(),
        stats: collected.stats,
        truncated: collected.truncated,
    })
}

async fn get_related_documents_for_mcp(
    mcp: &WilkesMcp,
    mut params: GetRelatedDocumentsParams,
) -> Result<GetRelatedDocumentsResponse, String> {
    match &mcp.context {
        McpContext::Session(context) => {
            get_related_documents(context, mcp.search.clone(), &mcp.cwd, params).await
        }
        McpContext::Library => {
            let path = params
                .path
                .as_ref()
                .filter(|path| !path.trim().is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| {
                    "External Wilkes MCP has no active document; pass path explicitly.".to_string()
                })?;
            if !mcp.is_path_allowed(&path).await {
                return Err(read_access_error(&path));
            }
            let root = match params.root.as_ref() {
                Some(root) if !root.trim().is_empty() => PathBuf::from(root),
                Some(_) => return Err("Related-documents root cannot be empty.".to_string()),
                None => mcp.current_root().await?,
            };
            if !is_within_roots(&root, &mcp.library_roots().await) {
                return Err(read_access_error(&root));
            }
            params.root = Some(root.to_string_lossy().into_owned());
            get_related_documents(
                &ContextStateHandle::default(),
                mcp.search.clone(),
                Path::new(""),
                params,
            )
            .await
        }
    }
}

async fn get_related_documents(
    context: &ContextStateHandle,
    search: Option<Arc<dyn SearchService>>,
    cwd: &Path,
    mut params: GetRelatedDocumentsParams,
) -> Result<GetRelatedDocumentsResponse, String> {
    let search = search.ok_or_else(|| {
        "Wilkes related-document search is not available in this session.".to_string()
    })?;
    let path = match params.path.take() {
        Some(path) if !path.trim().is_empty() => PathBuf::from(path),
        Some(_) => return Err("Document path cannot be empty.".to_string()),
        None => context
            .snapshot()
            .active_doc
            .map(|document| PathBuf::from(document.path))
            .ok_or_else(|| "No active document is available; pass path explicitly.".to_string())?,
    };
    let root = match params.root.take() {
        Some(root) if !root.trim().is_empty() => PathBuf::from(root),
        Some(_) => return Err("Related-documents root cannot be empty.".to_string()),
        None => match context.search_root() {
            Some(root) => root,
            None => search
                .clone()
                .default_root()
                .await
                .unwrap_or_else(|| cwd.to_path_buf()),
        },
    };
    let scope = if params.scope == Some(SearchScopeParam::All) {
        wilkes_core::types::SearchScope::All
    } else {
        wilkes_core::types::SearchScope::Corpus
    };
    let query = wilkes_core::types::RelatedDocumentsQuery {
        root: root.clone(),
        path: path.clone(),
        scope: scope.clone(),
        limit: Some(
            params
                .limit
                .unwrap_or(DEFAULT_RELATED_DOCUMENTS_LIMIT)
                .clamp(1, MAX_RELATED_DOCUMENTS_LIMIT),
        ),
        collection_id: params.collection_id,
    };
    let documents = search
        .related_documents(query)
        .await
        .map_err(with_semantic_index_guidance)?;

    Ok(GetRelatedDocumentsResponse {
        path: display_path(&path),
        root: display_path(&root),
        scope: if scope == wilkes_core::types::SearchScope::All {
            SearchScopeParamResponse::All
        } else {
            SearchScopeParamResponse::CurrentRoot
        },
        documents,
    })
}

fn with_semantic_index_guidance(message: String) -> String {
    let lower = message.to_ascii_lowercase();
    let index_unavailable = [
        "no semantic index",
        "semantic index is not ready",
        "semantic index is currently being built",
        "semantic index has no searchable documents",
        "semantic index is not built",
        "semantic search requires a loaded embedder and built index",
    ]
    .iter()
    .any(|needle| lower.contains(needle));

    if index_unavailable {
        format!("{message} {SEMANTIC_INDEX_GUIDANCE}")
    } else {
        message
    }
}

fn build_search_query(
    root: PathBuf,
    params: SearchParams,
) -> Result<(wilkes_core::types::SearchQuery, usize), String> {
    let mode = params.mode;
    let pattern = params.query.trim().to_string();
    if pattern.is_empty() {
        return Err("Search query cannot be empty.".to_string());
    }

    let max_results = params
        .max_results
        .unwrap_or(DEFAULT_SEARCH_MAX_RESULTS)
        .clamp(1, MAX_SEARCH_MAX_RESULTS);
    let context_lines = params
        .context_lines
        .unwrap_or(DEFAULT_SEARCH_CONTEXT_LINES)
        .min(MAX_SEARCH_CONTEXT_LINES);
    Ok((
        wilkes_core::types::SearchQuery {
            pattern,
            is_regex: mode == SearchModeParam::Exact && params.is_regex.unwrap_or(false),
            case_sensitive: params.case_sensitive.unwrap_or(false),
            root,
            max_results,
            respect_gitignore: true,
            max_file_size: DEFAULT_SEARCH_MAX_FILE_SIZE,
            context_lines,
            mode: match mode {
                SearchModeParam::Exact => wilkes_core::types::SearchMode::Grep,
                SearchModeParam::Semantic => wilkes_core::types::SearchMode::Semantic,
            },
            scope: if let Some(path) = params.file {
                wilkes_core::types::SearchScope::File {
                    path: PathBuf::from(path),
                }
            } else if params.scope == Some(SearchScopeParam::All) {
                wilkes_core::types::SearchScope::All
            } else {
                wilkes_core::types::SearchScope::Corpus
            },
            supported_extensions: Vec::new(),
            collection_id: params.collection_id,
            tag_ids: Vec::new(),
        },
        max_results,
    ))
}

impl From<wilkes_core::types::FileMatches> for SearchFileResponse {
    fn from(file: wilkes_core::types::FileMatches) -> Self {
        Self {
            path: display_path(&file.path),
            file_type: file.file_type,
            matches: file
                .matches
                .into_iter()
                .map(SearchMatchResponse::from)
                .collect(),
        }
    }
}

impl From<wilkes_core::types::Match> for SearchMatchResponse {
    fn from(matched: wilkes_core::types::Match) -> Self {
        let (line, page) = match matched.origin {
            wilkes_core::types::SourceOrigin::TextFile { line, .. } => (Some(line), None),
            wilkes_core::types::SourceOrigin::PdfPage { page, .. } => (None, Some(page)),
        };
        let mut text = String::with_capacity(
            matched.context_before.len() + matched.matched_text.len() + matched.context_after.len(),
        );
        text.push_str(&matched.context_before);
        text.push_str(&matched.matched_text);
        text.push_str(&matched.context_after);
        Self {
            text,
            line,
            page,
            score: matched.score,
        }
    }
}

fn structured(value: impl Serialize) -> CallToolResult {
    match serde_json::to_value(value) {
        Ok(value) => CallToolResult::structured(value),
        Err(err) => CallToolResult::error(vec![ContentBlock::text(format!(
            "Failed to serialize Wilkes MCP response: {err}"
        ))]),
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{CollectedSearch, SearchService};
    use async_trait::async_trait;
    use std::sync::Mutex;
    use tempfile::tempdir;
    use wilkes_core::types::{
        FileMatches, FileType, Match, RelatedDocument, RelatedDocumentsQuery, SearchMode,
        SearchQuery, SearchScope, SearchStats, SourceOrigin,
    };

    struct FakeSearch {
        last_query: Mutex<Option<SearchQuery>>,
        last_related_query: Mutex<Option<RelatedDocumentsQuery>>,
        default_root: Option<PathBuf>,
        response: Mutex<Option<CollectedSearch>>,
        related_response: Mutex<Option<Vec<RelatedDocument>>>,
    }

    #[test]
    fn list_context_includes_root_and_first_three_files() {
        let dir = tempdir().unwrap();
        for name in ["04.txt", "02.txt", "01.txt", "03.txt"] {
            std::fs::write(dir.path().join(name), name).unwrap();
        }
        let context = ContextStateHandle::default();
        context.set_search_root(Some(dir.path().to_string_lossy().into_owned()));
        let snapshot = context.snapshot();
        let response = ListContextResponse::from_snapshot(
            snapshot.root,
            snapshot.active_doc,
            snapshot.context_files,
            vec![dir.path().to_path_buf()],
        );

        assert_eq!(response.current_root.as_deref(), dir.path().to_str());
        assert_eq!(
            response.roots,
            vec![dir.path().to_string_lossy().into_owned()]
        );
        assert_eq!(response.first_files.len(), 3);
        assert!(response.first_files[0].ends_with("01.txt"));
        assert!(response.first_files[1].ends_with("02.txt"));
        assert!(response.first_files[2].ends_with("03.txt"));
    }

    #[test]
    fn semantic_index_errors_include_enablement_and_exact_search_guidance() {
        for message in [
            "No semantic index found. Build the index first.",
            "Semantic index is currently being built. Please wait.",
            "The global semantic index has no searchable documents.",
        ] {
            let enriched = with_semantic_index_guidance(message.to_string());
            assert!(enriched.starts_with(message));
            assert!(enriched.contains("The user can enable the semantic index"));
            assert!(enriched.contains("mode='exact'"));
            assert!(enriched.contains("in the meantime"));
        }

        let unrelated = "Search root does not exist.".to_string();
        assert_eq!(with_semantic_index_guidance(unrelated.clone()), unrelated);
    }

    #[async_trait]
    impl SearchService for FakeSearch {
        async fn default_root(self: Arc<Self>) -> Option<PathBuf> {
            self.default_root.clone()
        }

        async fn library_roots(self: Arc<Self>) -> Vec<PathBuf> {
            self.default_root.clone().into_iter().collect()
        }

        async fn search(
            self: Arc<Self>,
            query: SearchQuery,
            _max_files: usize,
        ) -> Result<CollectedSearch, String> {
            *self.last_query.lock().unwrap() = Some(query);
            Ok(self.response.lock().unwrap().take().unwrap())
        }

        async fn related_documents(
            self: Arc<Self>,
            query: RelatedDocumentsQuery,
        ) -> Result<Vec<RelatedDocument>, String> {
            *self.last_related_query.lock().unwrap() = Some(query);
            Ok(self.related_response.lock().unwrap().take().unwrap())
        }
    }

    fn fake_search_with_root(root: PathBuf) -> Arc<FakeSearch> {
        Arc::new(FakeSearch {
            last_query: Mutex::new(None),
            last_related_query: Mutex::new(None),
            default_root: Some(root),
            response: Mutex::new(None),
            related_response: Mutex::new(None),
        })
    }

    #[tokio::test]
    async fn external_context_reads_only_configured_library_roots() {
        let library = tempdir().unwrap();
        let inside = library.path().join("inside.txt");
        std::fs::write(&inside, "inside library").unwrap();
        let outside_dir = tempdir().unwrap();
        let outside = outside_dir.path().join("outside.txt");
        std::fs::write(&outside, "outside library").unwrap();
        let mcp = WilkesMcp::new(
            McpContext::Library,
            PathBuf::new(),
            Some(fake_search_with_root(library.path().to_path_buf())),
            None,
        );

        let response = get_document_text_for_mcp(
            &mcp,
            GetDocumentTextParams {
                path: Some(inside.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(response.text, "inside library");

        let error = get_document_text_for_mcp(
            &mcp,
            GetDocumentTextParams {
                path: Some(outside.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("not in the current root"));
    }

    #[tokio::test]
    async fn external_context_requires_explicit_document_path() {
        let library = tempdir().unwrap();
        let mcp = WilkesMcp::new(
            McpContext::Library,
            PathBuf::new(),
            Some(fake_search_with_root(library.path().to_path_buf())),
            None,
        );

        let error = get_document_text_for_mcp(
            &mcp,
            GetDocumentTextParams {
                path: None,
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("pass path explicitly"));
    }

    #[tokio::test]
    async fn external_search_rejects_root_outside_library() {
        let library = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let mcp = WilkesMcp::new(
            McpContext::Library,
            PathBuf::new(),
            Some(fake_search_with_root(library.path().to_path_buf())),
            None,
        );

        let error = search_documents_for_mcp(
            &mcp,
            SearchParams {
                query: "needle".to_string(),
                mode: SearchModeParam::Exact,
                scope: None,
                root: Some(outside.path().to_string_lossy().into_owned()),
                file: None,
                max_results: None,
                case_sensitive: None,
                is_regex: None,
                context_lines: None,
                collection_id: None,
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("not in the current root"));
    }

    #[tokio::test]
    async fn bearer_middleware_requires_current_token() {
        use axum::{body::Body, http::Request, routing::get};
        use tower::ServiceExt;

        let token = Arc::new(RwLock::new(Some("first-token".to_string())));
        let app = Router::new()
            .route("/", get(|| async { StatusCode::OK }))
            .layer(middleware::from_fn_with_state(
                Arc::clone(&token),
                require_bearer_token,
            ));

        let unauthorized = app
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = app
            .clone()
            .oneshot(
                Request::get("/")
                    .header("Authorization", "Bearer first-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::OK);

        *token.write().unwrap() = Some("second-token".to_string());
        let old = app
            .clone()
            .oneshot(
                Request::get("/")
                    .header("Authorization", "Bearer first-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(old.status(), StatusCode::UNAUTHORIZED);
        let current = app
            .oneshot(
                Request::get("/")
                    .header("Authorization", "Bearer second-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(current.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bearer_middleware_allows_requests_without_configured_token() {
        use axum::{body::Body, http::Request, routing::get};
        use tower::ServiceExt;

        let app = Router::new()
            .route("/", get(|| async { StatusCode::OK }))
            .layer(middleware::from_fn_with_state(
                Arc::new(RwLock::new(None)),
                require_bearer_token,
            ));
        let response = app
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn tokenless_external_server_initializes_and_lists_tools() {
        let library = tempdir().unwrap();
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let external_host = format!("192.168.1.6:{port}");
        let runtime = start_external(
            "127.0.0.1".parse().unwrap(),
            port,
            None,
            fake_search_with_root(library.path().to_path_buf()),
        )
        .await
        .unwrap();
        let client = reqwest::Client::new();

        // A raw GET is not a valid request for this stateless MCP transport,
        // but an external Host must reach the transport instead of being
        // rejected by rmcp's loopback-only DNS-rebinding default.
        let get = client
            .get(runtime.url())
            .header("Host", &external_host)
            .send()
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);

        let send = |body: serde_json::Value| {
            client
                .post(runtime.url())
                .header("Host", &external_host)
                .header("Accept", "application/json, text/event-stream")
                .header("MCP-Protocol-Version", "2025-11-25")
                .json(&body)
                .send()
        };

        let initialize = send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "wilkes-test", "version": "1.0" }
            }
        }))
        .await
        .unwrap();
        assert!(initialize.status().is_success());
        let initialize: serde_json::Value = initialize.json().await.unwrap();
        assert_eq!(initialize["result"]["serverInfo"]["name"], "wilkes");

        let tools = send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .await
        .unwrap();
        assert!(tools.status().is_success());
        let tools: serde_json::Value = tools.json().await.unwrap();
        let names: Vec<_> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(names.contains(&"search"));
        assert!(names.contains(&"get_document_text"));
        assert!(names.contains(&"download"));
    }

    #[tokio::test]
    async fn authenticated_external_server_accepts_external_host_only_with_bearer_token() {
        let library = tempdir().unwrap();
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let external_host = format!("192.168.1.6:{port}");
        let runtime = start_external(
            "127.0.0.1".parse().unwrap(),
            port,
            Some("test-token".to_string()),
            fake_search_with_root(library.path().to_path_buf()),
        )
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "wilkes-test", "version": "1.0" }
            }
        });
        let request = || {
            client
                .post(runtime.url())
                .header("Host", &external_host)
                .header("Accept", "application/json, text/event-stream")
                .header("MCP-Protocol-Version", "2025-11-25")
                .json(&initialize)
        };

        let unauthorized = request().send().await.unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = request()
            .header("Authorization", "Bearer test-token")
            .send()
            .await
            .unwrap();
        assert!(authorized.status().is_success());
    }

    #[test]
    fn reads_active_document_when_path_is_omitted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("active.txt");
        std::fs::write(&path, "active document text").unwrap();
        let context = ContextStateHandle::default();
        context.set_active_doc(Some(path.to_string_lossy().into_owned()), None);

        let response = get_document_text(
            &context,
            GetDocumentTextParams {
                path: None,
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .unwrap();

        assert_eq!(response.text, "active document text");
        assert!(!response.truncated);
    }

    #[test]
    fn reads_explicit_context_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("context.txt");
        std::fs::write(&path, "context document text").unwrap();
        let context = ContextStateHandle::default();
        context.add_context(path.to_string_lossy().into_owned(), None);

        let response = get_document_text(
            &context,
            GetDocumentTextParams {
                path: Some(path.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .unwrap();

        assert_eq!(response.text, "context document text");
    }

    #[test]
    fn reads_document_nested_in_current_root() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let path = nested.join("document.txt");
        std::fs::write(&path, "root document text").unwrap();
        let context = ContextStateHandle::default();
        context.set_search_root(Some(dir.path().to_string_lossy().into_owned()));

        let response = get_document_text(
            &context,
            GetDocumentTextParams {
                path: Some(path.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .unwrap();

        assert_eq!(response.text, "root document text");
    }

    #[test]
    fn denies_file_outside_current_root_and_context_with_guidance() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("library");
        let sibling = dir.path().join("library-other");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        let denied = sibling.join("denied.txt");
        std::fs::write(&denied, "denied").unwrap();
        let context = ContextStateHandle::default();
        context.set_search_root(Some(root.to_string_lossy().into_owned()));

        let err = get_document_text(
            &context,
            GetDocumentTextParams {
                path: Some(denied.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .unwrap_err();

        assert_eq!(
            err,
            format!(
                "{} is not in the current root or this chat's context. The user can either move the file to this root, switch to that root, or add it to the context using the right-click menu on the file list",
                denied.display()
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn denies_symlink_in_current_root_that_resolves_outside_it() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir(&root).unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "outside").unwrap();
        let link = root.join("linked.txt");
        symlink(&outside, &link).unwrap();
        let context = ContextStateHandle::default();
        context.set_search_root(Some(root.to_string_lossy().into_owned()));

        let err = get_document_text(
            &context,
            GetDocumentTextParams {
                path: Some(link.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .unwrap_err();

        assert!(err.contains("The user can either move the file to this root"));
    }

    #[test]
    fn limits_text_on_character_boundary() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("unicode.txt");
        std::fs::write(&path, "aé日b").unwrap();
        let context = ContextStateHandle::default();
        context.add_context(path.to_string_lossy().into_owned(), None);

        let response = get_document_text(
            &context,
            GetDocumentTextParams {
                path: Some(path.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: Some(3),
            },
        )
        .unwrap();

        assert_eq!(response.text, "aé日");
        assert!(response.truncated);
    }

    #[test]
    fn explicit_page_range_overrides_active_document_page() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("active.txt");
        std::fs::write(&path, "active document text").unwrap();
        let context = ContextStateHandle::default();
        context.set_active_doc(Some(path.to_string_lossy().into_owned()), Some(3));

        let response = get_document_text(
            &context,
            GetDocumentTextParams {
                path: None,
                page: None,
                page_range: Some(PageRange { start: 1, end: 5 }),
                max_chars: None,
            },
        )
        .unwrap();

        assert_eq!(response.page, None);
        assert_eq!(response.page_range, Some(PageRange { start: 1, end: 5 }));
        assert_eq!(response.text, "active document text");
    }

    #[test]
    fn builds_bounded_exact_search_query() {
        let dir = tempdir().unwrap();
        let explicit_root = dir.path().join("root");
        let (query, max_files) = build_search_query(
            explicit_root.clone(),
            SearchParams {
                query: "  IO programming  ".to_string(),
                mode: SearchModeParam::Exact,
                scope: None,
                root: None,
                file: None,
                max_results: Some(500),
                case_sensitive: Some(true),
                is_regex: Some(true),
                context_lines: Some(100),
                collection_id: None,
            },
        )
        .unwrap();

        assert_eq!(query.pattern, "IO programming");
        assert_eq!(query.mode, SearchMode::Grep);
        assert_eq!(query.root, explicit_root);
        assert_eq!(query.max_results, MAX_SEARCH_MAX_RESULTS);
        assert_eq!(max_files, MAX_SEARCH_MAX_RESULTS);
        assert!(query.case_sensitive);
        assert!(query.is_regex);
        assert_eq!(query.context_lines, MAX_SEARCH_CONTEXT_LINES);
        assert!(query.respect_gitignore);
        assert_eq!(query.max_file_size, DEFAULT_SEARCH_MAX_FILE_SIZE);
    }

    #[test]
    fn semantic_search_ignores_regex_flag() {
        let dir = tempdir().unwrap();
        let (query, _) = build_search_query(
            dir.path().to_path_buf(),
            SearchParams {
                query: "definitions".to_string(),
                mode: SearchModeParam::Semantic,
                scope: None,
                root: None,
                file: None,
                max_results: None,
                case_sensitive: None,
                is_regex: Some(true),
                context_lines: None,
                collection_id: None,
            },
        )
        .unwrap();

        assert_eq!(query.mode, SearchMode::Semantic);
        assert_eq!(query.root, dir.path());
        assert!(!query.is_regex);
        assert_eq!(query.max_results, DEFAULT_SEARCH_MAX_RESULTS);
    }

    #[test]
    fn builds_file_scoped_search_query() {
        let dir = tempdir().unwrap();
        let (query, _) = build_search_query(
            dir.path().to_path_buf(),
            SearchParams {
                query: "definitions".to_string(),
                mode: SearchModeParam::Exact,
                scope: None,
                root: None,
                file: Some("paper.pdf".to_string()),
                max_results: None,
                case_sensitive: None,
                is_regex: None,
                context_lines: None,
                collection_id: None,
            },
        )
        .unwrap();

        assert_eq!(
            query.scope,
            SearchScope::File {
                path: PathBuf::from("paper.pdf")
            }
        );
    }

    #[test]
    fn file_takes_precedence_over_all_scope() {
        let dir = tempdir().unwrap();
        let (query, _) = build_search_query(
            dir.path().to_path_buf(),
            SearchParams {
                query: "definitions".to_string(),
                mode: SearchModeParam::Exact,
                scope: Some(SearchScopeParam::All),
                root: None,
                file: Some("paper.pdf".to_string()),
                max_results: None,
                case_sensitive: None,
                is_regex: None,
                context_lines: None,
                collection_id: None,
            },
        )
        .unwrap();

        assert_eq!(
            query.scope,
            SearchScope::File {
                path: PathBuf::from("paper.pdf")
            }
        );
    }

    #[test]
    fn builds_all_scoped_search_query() {
        let dir = tempfile::tempdir().unwrap();
        let (query, _) = build_search_query(
            dir.path().to_path_buf(),
            SearchParams {
                query: "across the library".into(),
                mode: SearchModeParam::Exact,
                scope: Some(SearchScopeParam::All),
                root: None,
                file: None,
                max_results: None,
                case_sensitive: None,
                is_regex: None,
                context_lines: None,
                collection_id: None,
            },
        )
        .unwrap();

        assert_eq!(query.scope, SearchScope::All);
    }

    #[tokio::test]
    async fn related_documents_uses_active_document_and_all_scope() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("library");
        let active = root.join("paper.md");
        let service = Arc::new(FakeSearch {
            last_query: Mutex::new(None),
            last_related_query: Mutex::new(None),
            default_root: None,
            response: Mutex::new(None),
            related_response: Mutex::new(Some(Vec::new())),
        });
        let context = ContextStateHandle::default();
        context.set_search_root(Some(root.to_string_lossy().into_owned()));
        context.set_active_doc(Some(active.to_string_lossy().into_owned()), None);

        let response = get_related_documents(
            &context,
            Some(service.clone()),
            dir.path(),
            GetRelatedDocumentsParams {
                path: None,
                scope: Some(SearchScopeParam::All),
                root: None,
                limit: Some(100),
                collection_id: None,
            },
        )
        .await
        .unwrap();

        let query = service.last_related_query.lock().unwrap().clone().unwrap();
        assert_eq!(query.path, active);
        assert_eq!(query.root, root);
        assert_eq!(query.scope, SearchScope::All);
        assert_eq!(query.limit, Some(MAX_RELATED_DOCUMENTS_LIMIT));
        assert_eq!(response.scope, SearchScopeParamResponse::All);
        assert!(response.documents.is_empty());
    }

    #[tokio::test]
    async fn search_documents_maps_service_results() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("paper.pdf");
        let default_root = dir.path().join("active-root");
        let live_root = dir.path().join("live-ui-root");
        let service = Arc::new(FakeSearch {
            last_query: Mutex::new(None),
            last_related_query: Mutex::new(None),
            default_root: Some(default_root.clone()),
            response: Mutex::new(Some(CollectedSearch {
                files: vec![FileMatches {
                    path: path.clone(),
                    file_type: FileType::Pdf,
                    matches: vec![Match {
                        text_range: None,
                        matched_text: "IO programming".to_string(),
                        context_before: "before ".to_string(),
                        context_after: " after".to_string(),
                        origin: SourceOrigin::PdfPage {
                            page: 3,
                            bbox: None,
                        },
                        score: Some(0.91),
                    }],
                }],
                stats: SearchStats {
                    files_scanned: 1,
                    total_matches: 1,
                    elapsed_ms: 4,
                    errors: Vec::new(),
                },
                truncated: false,
            })),
            related_response: Mutex::new(None),
        });
        let context = ContextStateHandle::default();
        context.set_search_root(Some(live_root.to_string_lossy().into_owned()));

        let response = search_documents(
            &context,
            Some(service.clone()),
            dir.path(),
            SearchParams {
                query: "IO".to_string(),
                mode: SearchModeParam::Semantic,
                scope: None,
                root: None,
                file: None,
                max_results: Some(3),
                case_sensitive: None,
                is_regex: None,
                context_lines: None,
                collection_id: None,
            },
        )
        .await
        .unwrap();

        let captured = service.last_query.lock().unwrap().clone().unwrap();
        assert_eq!(captured.mode, SearchMode::Semantic);
        assert_eq!(captured.max_results, 3);
        assert_eq!(captured.root, live_root);
        assert_eq!(response.mode, SearchModeParam::Semantic);
        assert_eq!(response.root, display_path(&live_root));
        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.matches[0].path, display_path(&path));
        assert_eq!(response.matches[0].matches[0].page, Some(3));
        assert_eq!(
            response.matches[0].matches[0].text,
            "before IO programming after"
        );
        let serialized = serde_json::to_value(&response.matches[0].matches[0]).unwrap();
        assert!(serialized.get("context_before").is_none());
        assert!(serialized.get("context_after").is_none());
    }

    #[tokio::test]
    async fn search_documents_prefers_explicit_root_over_default_root() {
        let dir = tempdir().unwrap();
        let explicit_root = dir.path().join("explicit");
        let context = ContextStateHandle::default();
        context.set_search_root(Some(
            dir.path()
                .join("context-root")
                .to_string_lossy()
                .into_owned(),
        ));
        let service = Arc::new(FakeSearch {
            last_query: Mutex::new(None),
            last_related_query: Mutex::new(None),
            default_root: Some(dir.path().join("active-root")),
            response: Mutex::new(Some(CollectedSearch {
                files: Vec::new(),
                stats: SearchStats::default(),
                truncated: false,
            })),
            related_response: Mutex::new(None),
        });

        let response = search_documents(
            &context,
            Some(service.clone()),
            Path::new("/fallback"),
            SearchParams {
                query: "multi-turn".to_string(),
                mode: SearchModeParam::Exact,
                scope: None,
                root: Some(explicit_root.to_string_lossy().into_owned()),
                file: None,
                max_results: None,
                case_sensitive: None,
                is_regex: None,
                context_lines: None,
                collection_id: None,
            },
        )
        .await
        .unwrap();

        let captured = service.last_query.lock().unwrap().clone().unwrap();
        assert_eq!(captured.root, explicit_root);
        assert_eq!(response.root, display_path(&captured.root));
    }

    #[tokio::test]
    async fn search_documents_reports_missing_service() {
        let context = ContextStateHandle::default();
        let err = search_documents(
            &context,
            None,
            Path::new("/tmp"),
            SearchParams {
                query: "anything".to_string(),
                mode: SearchModeParam::Exact,
                scope: None,
                root: None,
                file: None,
                max_results: None,
                case_sensitive: None,
                is_regex: None,
                context_lines: None,
                collection_id: None,
            },
        )
        .await
        .unwrap_err();

        assert!(err.contains("not available"));
    }

    #[tokio::test]
    async fn download_rejects_path_traversal_and_existing_files() {
        let dir = tempdir().unwrap();
        let traversal = download_to_root(
            dir.path(),
            DownloadParams {
                url: "https://example.test/paper.pdf".to_string(),
                filename: Some("../paper.pdf".to_string()),
            },
        )
        .await
        .unwrap_err();
        assert!(traversal.contains("single file name"));

        let existing = dir.path().join("paper.pdf");
        std::fs::write(&existing, b"existing").unwrap();
        let overwrite = download_to_root(
            dir.path(),
            DownloadParams {
                url: "https://example.test/paper.pdf".to_string(),
                filename: Some("paper.pdf".to_string()),
            },
        )
        .await
        .unwrap_err();
        assert!(overwrite.contains("Refusing to overwrite"));
        assert_eq!(std::fs::read(existing).unwrap(), b"existing");
    }

    #[test]
    fn download_content_check_finds_equal_file_under_a_different_name() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("papers");
        std::fs::create_dir(&nested).unwrap();
        let existing = nested.join("original.pdf");
        std::fs::write(&existing, b"same paper").unwrap();
        std::fs::write(dir.path().join("same-size.pdf"), b"other text").unwrap();

        let found =
            find_file_with_content(dir.path(), &dir.path().join("new-name.pdf"), b"same paper")
                .unwrap();

        assert_eq!(found, Some(existing));
    }

    #[test]
    fn download_content_check_ignores_target_and_different_content() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("new-name.pdf");
        std::fs::write(&target, b"same paper").unwrap();
        std::fs::write(dir.path().join("other.pdf"), b"other text").unwrap();

        let found = find_file_with_content(dir.path(), &target, b"same paper").unwrap();

        assert_eq!(found, None);
    }
}
