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
use wilkes_core::extract::{pdf::PdfExtractor, ExtractorRegistry};
use wilkes_core::integrations::{
    openalex::OpenAlexClient, semantic_scholar::SemanticScholarClient,
};
use wilkes_core::types::IntegrationsSettings;

use crate::{
    context::{ActiveDoc, ContextFile},
    reader,
    search::SearchService,
    session::ContextStateHandle,
};

const DEFAULT_TEXT_CHAR_LIMIT: usize = 24_000;
const MAX_TEXT_CHAR_LIMIT: usize = 120_000;
const DEFAULT_SEARCH_MAX_RESULTS: usize = 10;
const MAX_SEARCH_MAX_RESULTS: usize = 50;
const DEFAULT_SEARCH_CONTEXT_LINES: u32 = 2;
const MAX_SEARCH_CONTEXT_LINES: u32 = 5;
const DEFAULT_RELATED_DOCUMENTS_LIMIT: usize = 8;
const MAX_RELATED_DOCUMENTS_LIMIT: usize = 25;
const DEFAULT_LIST_DOCUMENTS_LIMIT: usize = 50;
const MAX_LIST_DOCUMENTS_LIMIT: usize = 500;
const MAX_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024;
const SEMANTIC_INDEX_GUIDANCE: &str = "The user can enable the semantic index in Wilkes Settings. Use exact search with mode='exact' instead in the meantime.";
const EXTERNAL_DOCUMENT_PATH_REQUIRED: &str =
    "External Wilkes MCP does not default document tools to the active document; pass path explicitly after reading list_context.";

/// Names of the read-only tools this server exposes. Shared with the permission
/// boundary in `session.rs` so calls to Wilkes's *own* MCP server are
/// auto-allowed without ever prompting the user (they are the Q&A pane's own
/// internal plumbing). Mutating tools such as `download` must not be added here,
/// so the agent's normal permission flow remains in effect.
pub(crate) const WILKES_MCP_TOOL_NAMES: &[&str] = &[
    "list_context",
    "get_document_text",
    "get_document_outline",
    "get_related_documents",
    "get_file_metadata",
    "list_documents",
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

/// Live application context exposed by the external MCP server.
///
/// This is intentionally narrower than a chat session context: it reports the
/// document visible in Wilkes, but does not make that document an implicit
/// argument to document-reading tools.
#[derive(Clone, Debug, Default)]
pub struct ExternalMcpContext {
    active_doc: Arc<RwLock<Option<ActiveDoc>>>,
}

impl ExternalMcpContext {
    pub fn set_active_document(&self, path: Option<String>, page: Option<u32>) {
        *self
            .active_doc
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            path.map(|path| ActiveDoc { path, page });
    }

    fn active_document(&self) -> Option<ActiveDoc> {
        self.active_doc
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// Application-scoped MCP server for regular Claude Code and Codex clients.
///
/// Unlike the private chat server, this endpoint has no per-conversation
/// context. Its active document and readable library scope are resolved
/// dynamically, so viewer and settings changes take effect without restarting
/// the listener.
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
    context: ExternalMcpContext,
) -> anyhow::Result<ExternalMcpRuntime> {
    if port == 0 {
        anyhow::bail!("External MCP port must be between 1 and 65535");
    }
    let listener = tokio::net::TcpListener::bind((bind_address, port)).await?;
    let runtime = start_server(
        listener,
        "/mcp".to_string(),
        token,
        McpContext::Library(context),
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
    Library(ExternalMcpContext),
}

#[allow(clippy::too_many_arguments)]
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
        let mut tool_router = Self::tool_router();
        normalize_tool_input_schemas(&mut tool_router);
        Self {
            context,
            cwd,
            search,
            integrations,
            tool_router,
        }
    }
}

/// Keep Wilkes tool schemas within the broadly supported JSON Schema subset.
///
/// Schemars represents `Option<T>` as a nullable union such as
/// `"type": ["integer", "null"]`. That is valid JSON Schema 2020-12, but some
/// MCP clients discard array-valued `type` declarations before presenting a
/// tool to the model. Tool parameters are already optional through the
/// top-level `required` list, so advertising only their non-null value type is
/// both accurate for normal calls and more interoperable.
fn normalize_tool_input_schemas<S>(tool_router: &mut ToolRouter<S>) {
    for route in tool_router.map.values_mut() {
        let schema = Arc::make_mut(&mut route.attr.input_schema);
        for value in schema.values_mut() {
            normalize_optional_schema(value);
        }
    }
}

fn normalize_optional_schema(value: &mut serde_json::Value) {
    let serde_json::Value::Object(schema) = value else {
        if let serde_json::Value::Array(values) = value {
            for value in values {
                normalize_optional_schema(value);
            }
        }
        return;
    };

    let collapsed_type = match schema.get_mut("type") {
        Some(serde_json::Value::Array(types)) => {
            let non_null_count = types.iter().filter(|value| *value != "null").count();
            if non_null_count > 0 && non_null_count < types.len() {
                types.retain(|value| value != "null");
            }
            (types.len() == 1).then(|| types[0].clone())
        }
        _ => None,
    };
    if let Some(value_type) = collapsed_type {
        schema.insert("type".to_string(), value_type);
    }

    if let Some(serde_json::Value::Array(values)) = schema.get_mut("enum") {
        let has_non_null = values.iter().any(|value| !value.is_null());
        if has_non_null {
            values.retain(|value| !value.is_null());
        }
    }

    for keyword in ["anyOf", "oneOf"] {
        let single_branch = match schema.get_mut(keyword) {
            Some(serde_json::Value::Array(branches)) => {
                branches.retain(|branch| !is_null_schema(branch));
                for branch in branches.iter_mut() {
                    normalize_optional_schema(branch);
                }
                (branches.len() == 1).then(|| branches[0].clone())
            }
            _ => None,
        };

        if let Some(serde_json::Value::Object(branch)) = single_branch {
            if branch.keys().all(|key| !schema.contains_key(key)) {
                schema.remove(keyword);
                schema.extend(branch);
            }
        }
    }

    for nested in schema.values_mut() {
        normalize_optional_schema(nested);
    }
}

fn is_null_schema(value: &serde_json::Value) -> bool {
    let Some(schema) = value.as_object() else {
        return false;
    };
    if schema.get("const").is_some_and(serde_json::Value::is_null) {
        return true;
    }
    match schema.get("type") {
        Some(serde_json::Value::String(value)) => value == "null",
        Some(serde_json::Value::Array(values)) => {
            !values.is_empty() && values.iter().all(|value| value == "null")
        }
        _ => false,
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
            McpContext::Library(context) => {
                let root = match &self.search {
                    Some(search) => search.clone().default_root().await,
                    None => None,
                };
                crate::session::ContextSnapshot {
                    active_doc: context.active_document(),
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
            McpContext::Session(context) => {
                context.is_allowed(path) || is_within_roots(path, &self.library_roots().await)
            }
            McpContext::Library(_) => is_within_roots(path, &self.library_roots().await),
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
            McpContext::Library(_) => {
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

fn mcp_access_error(path: &Path) -> String {
    format!(
        "{} is not in a configured Wilkes library root or this chat's context. Open its containing directory as a Wilkes root or add the file to the chat context.",
        path.display()
    )
}

#[derive(Deserialize)]
#[serde(untagged)]
enum IntegerOrString<T> {
    Integer(T),
    String(String),
}

fn deserialize_optional_integer<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + std::str::FromStr,
{
    match Option::<IntegerOrString<T>>::deserialize(deserializer)? {
        None => Ok(None),
        Some(IntegerOrString::Integer(value)) => Ok(Some(value)),
        Some(IntegerOrString::String(value)) => value
            .parse()
            .map(Some)
            .map_err(|_| serde::de::Error::custom(format!("invalid integer string {value:?}"))),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BoolOrString {
    Bool(bool),
    String(String),
}

fn deserialize_optional_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<BoolOrString>::deserialize(deserializer)? {
        None => Ok(None),
        Some(BoolOrString::Bool(value)) => Ok(Some(value)),
        Some(BoolOrString::String(value)) => match value.as_str() {
            "true" => Ok(Some(true)),
            "false" => Ok(Some(false)),
            _ => Err(serde::de::Error::custom(format!(
                "invalid boolean string {value:?}"
            ))),
        },
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetDocumentTextParams {
    /// Document to read. Required for external MCP clients; an in-app chat may
    /// omit it to use that chat session's active document.
    path: Option<String>,
    /// One 1-based PDF page to read. Pass either page or page_range, not both.
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<u32>")]
    page: Option<u32>,
    /// Inclusive 1-based PDF page range in "N-M" format, for example "7-9".
    page_range: Option<String>,
    /// Maximum characters to return (default 24000, capped at 120000).
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
    max_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetDocumentOutlineParams {
    /// Document whose declared outline to read. Required for external MCP
    /// clients; an in-app chat may omit it to use that chat's active document.
    path: Option<String>,
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
    /// Restrict search to one file in any configured library root or chat
    /// context. Use this for questions about a concrete document.
    file: Option<String>,
    /// Maximum matches to return.
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
    max_results: Option<usize>,
    /// Exact search only.
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    #[schemars(with = "Option<bool>")]
    case_sensitive: Option<bool>,
    /// Exact search only.
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    #[schemars(with = "Option<bool>")]
    is_regex: Option<bool>,
    /// Exact search context lines.
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<u32>")]
    context_lines: Option<u32>,
    /// Optional saved smart collection ID to intersect with the chosen scope.
    collection_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetRelatedDocumentsParams {
    /// Document to find related documents for. Required for external MCP
    /// clients; an in-app chat may omit it to use its active document.
    path: Option<String>,
    /// Search location. Use all for the whole library; omit for the current root.
    scope: Option<SearchScopeParam>,
    /// Corpus/index root. Omit unless using a different root is intentional.
    root: Option<String>,
    /// Maximum related documents to return (1-25, default 8).
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
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
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetFileMetadataParams {
    /// Document to read metadata for. Required for external MCP clients; an
    /// in-app chat may omit it to use its active document.
    path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListDocumentsParams {
    /// Corpus/library root to list. Omit for the current root.
    root: Option<String>,
    /// List location. Use all for the whole library; omit for the current root only.
    scope: Option<SearchScopeParam>,
    /// Maximum documents to return (1-500, default 50).
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
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

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
struct PageRange {
    start: u32,
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
struct GetDocumentOutlineResponse {
    path: String,
    /// The document's declared table of contents. Empty means the document
    /// declares no outline; it is not an extraction failure.
    outline: Vec<wilkes_core::types::OutlineEntry>,
    /// Per-document extraction decisions made while resolving the outline.
    extraction: wilkes_core::types::ExtractionDiagnostics,
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
struct GetFileMetadataResponse {
    path: String,
    #[serde(flatten)]
    metadata: wilkes_core::types::DocumentMetadata,
}

#[derive(Debug, Serialize)]
struct ListDocumentsResponse {
    root: String,
    scope: SearchScopeParamResponse,
    documents: Vec<DocumentSummaryResponse>,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct DocumentSummaryResponse {
    path: String,
    title: Option<String>,
    author: Option<String>,
    doi: Option<String>,
    publication_date: Option<String>,
    citation_count: Option<i64>,
}

impl From<wilkes_core::types::FileEntry> for DocumentSummaryResponse {
    fn from(entry: wilkes_core::types::FileEntry) -> Self {
        Self {
            path: display_path(&entry.path),
            title: entry.title,
            author: entry.author,
            doi: entry.doi,
            publication_date: entry.publication_date,
            citation_count: entry.citation_count,
        }
    }
}

#[derive(Debug, Serialize)]
struct SearchFileResponse {
    path: String,
    file_type: wilkes_core::types::FileType,
    /// Document title from cached metadata; null until the file is processed.
    title: Option<String>,
    /// Document author from cached metadata; null until the file is processed.
    author: Option<String>,
    /// Document DOI from cached metadata; null when absent or not yet processed.
    doi: Option<String>,
    matches: Vec<SearchMatchResponse>,
}

#[derive(Debug, Serialize)]
struct SearchMatchResponse {
    kind: SearchMatchKindResponse,
    text: String,
    line: Option<u32>,
    page: Option<u32>,
    score: Option<f32>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SearchMatchKindResponse {
    Content,
    Filename,
    Title,
}

#[tool_router]
impl WilkesMcp {
    #[tool(
        description = "List the current Wilkes context: configured library roots, active root, the document currently visible in Wilkes, and any files explicitly added to an in-app chat."
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
        description = "Read Wilkes-extracted document text from any configured Wilkes library root. External MCP clients must pass path; an in-app chat may omit it to use that chat's active document. Use page for one PDF page or page_range in \"N-M\" format (for example, \"7-9\") for an inclusive PDF page range."
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
        description = "Read a document's declared outline (PDF bookmarks or Markdown headings) without requiring the semantic index. External MCP clients must pass path; an in-app chat may omit it to use that chat's active document. An empty outline means the document declares no table of contents."
    )]
    async fn get_document_outline(
        &self,
        Parameters(params): Parameters<GetDocumentOutlineParams>,
    ) -> CallToolResult {
        match get_document_outline_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Find documents semantically related to a document in Wilkes. External MCP clients must pass path; an in-app chat may omit it to use that chat's active document. Set scope='all' to search the whole library; otherwise results are limited to the current root."
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
        description = "Read full bibliographic metadata for a document in any configured Wilkes library root: title, author, DOI, publication date, and any Semantic Scholar / OpenAlex enrichment. External MCP clients must pass path; an in-app chat may omit it to use that chat's active document."
    )]
    async fn get_file_metadata(
        &self,
        Parameters(params): Parameters<GetFileMetadataParams>,
    ) -> CallToolResult {
        match get_file_metadata_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "List documents in the Wilkes library with their title, author, and DOI. Set scope='all' to list the whole library; otherwise lists the current root. Use this to browse what documents exist rather than searching their contents."
    )]
    async fn list_documents(
        &self,
        Parameters(params): Parameters<ListDocumentsParams>,
    ) -> CallToolResult {
        match list_documents_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Search Wilkes-readable documents, including direct filename and cached-title matches. You must explicitly set mode='exact' for literal/regex matching or mode='semantic' for meaning-based content search plus case-insensitive literal filename/title matching; mode has no default. Each returned match has kind='content', 'filename', or 'title'. Set scope='all' to search every configured library root; omit scope to search the current root. If the user asks about a specific document, set file to that document path; omit file only for corpus-wide searches."
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
    let (path, default_page) = match (&mcp.context, params.path.as_ref()) {
        (_, Some(path)) if !path.trim().is_empty() => (PathBuf::from(path), None),
        (_, Some(_)) => return Err("Document path cannot be empty.".to_string()),
        (McpContext::Session(context), None) => {
            let active_doc = context
                .snapshot()
                .active_doc
                .ok_or_else(|| "No active Wilkes document is available.".to_string())?;
            (PathBuf::from(active_doc.path), active_doc.page)
        }
        (McpContext::Library(_), None) => {
            return Err(EXTERNAL_DOCUMENT_PATH_REQUIRED.to_string());
        }
    };

    if params.page.is_some() && params.page_range.is_some() {
        return Err("Pass either page or page_range, not both.".to_string());
    }
    if !mcp.is_path_allowed(&path).await {
        return Err(mcp_access_error(&path));
    }
    get_document_text_at_path(path, default_page, params)
}

async fn get_document_outline_for_mcp(
    mcp: &WilkesMcp,
    params: GetDocumentOutlineParams,
) -> Result<GetDocumentOutlineResponse, String> {
    let path = resolve_document_path(mcp, params.path).await?;
    let outline_path = path.clone();
    let declared_outline = tokio::task::spawn_blocking(move || {
        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(PdfExtractor::new()));
        wilkes_core::extract::document_outline(&outline_path, &registry)
    })
    .await
    .map_err(|error| format!("Document outline task panicked: {error}"))?
    .map_err(|error| format!("Failed to read outline from {}: {error:#}", path.display()))?;

    Ok(GetDocumentOutlineResponse {
        path: display_path(&path),
        outline: declared_outline.entries,
        extraction: declared_outline.diagnostics,
    })
}

fn get_document_text_at_path(
    path: PathBuf,
    default_page: Option<u32>,
    params: GetDocumentTextParams,
) -> Result<GetDocumentTextResponse, String> {
    let started_at = Instant::now();
    let page_range = match (params.page, params.page_range.as_deref()) {
        (Some(page), None) => Some((page, page)),
        (None, Some(range)) => Some(parse_page_range(range)?),
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

fn parse_page_range(value: &str) -> Result<(u32, u32), String> {
    let invalid = || {
        format!(
            "Invalid page_range {value:?}. Use \"N-M\" with positive 1-based page numbers, for example \"1-2\"."
        )
    };
    let (start, end) = value.trim().split_once('-').ok_or_else(&invalid)?;
    if end.contains('-') {
        return Err(invalid());
    }
    let start = start.trim().parse::<u32>().map_err(|_| invalid())?;
    let end = end.trim().parse::<u32>().map_err(|_| invalid())?;
    if start == 0 || end == 0 {
        return Err(invalid());
    }
    Ok((start.min(end), start.max(end)))
}

async fn search_documents_for_mcp(
    mcp: &WilkesMcp,
    mut params: SearchParams,
) -> Result<SearchResponse, String> {
    match &mcp.context {
        McpContext::Session(context) => {
            search_documents(context, mcp.search.clone(), &mcp.cwd, params).await
        }
        McpContext::Library(_) => {
            let root = match params.root.as_ref() {
                Some(root) if !root.trim().is_empty() => PathBuf::from(root),
                Some(_) => return Err("Search root cannot be empty.".to_string()),
                None => mcp.current_root().await?,
            };
            if !is_within_roots(&root, &mcp.library_roots().await) {
                return Err(mcp_access_error(&root));
            }
            if let Some(file) = params.file.as_ref() {
                let file = PathBuf::from(file);
                if !mcp.is_path_allowed(&file).await {
                    return Err(mcp_access_error(&file));
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
    let max_file_size = search.clone().max_search_file_size().await;
    let (query, max_files) = build_search_query(root, params, max_file_size)?;
    let root = display_path(&query.root);
    let file = match &query.scope {
        wilkes_core::types::SearchScope::Corpus | wilkes_core::types::SearchScope::All => None,
        wilkes_core::types::SearchScope::File { path } => Some(display_path(path)),
    };
    let query_text = query.pattern.clone();
    let collected =
        search
            .clone()
            .search(query, max_files)
            .await
            .map_err(|message| match mode {
                SearchModeParam::Semantic => with_semantic_index_guidance(message),
                SearchModeParam::Exact => message,
            })?;

    let mut matches = Vec::with_capacity(collected.files.len());
    for file in collected.files {
        let path = file.path.clone();
        let mut response = SearchFileResponse::from(file);
        // Best-effort bibliographic enrichment from the same cache the file list
        // uses. Enrichment must never fail the search itself, but a real error
        // (cache lock, access boundary) is an anomaly worth surfacing rather
        // than silently dropping — so leave the fields null and log it.
        match search.clone().document_metadata(path.clone()).await {
            Ok(metadata) => {
                response.title = metadata.title.or(response.title);
                response.author = metadata.author;
                response.doi = metadata.doi;
            }
            Err(error) => {
                info!(path = %path.display(), %error, "search: metadata enrichment skipped");
            }
        }
        matches.push(response);
    }

    Ok(SearchResponse {
        query: query_text,
        mode,
        root,
        file,
        matches,
        stats: collected.stats,
        truncated: collected.truncated,
    })
}

/// Resolve the document path for a single-document tool: an explicit `path`
/// when given, otherwise the session's active document. Enforces the same
/// read-access boundary every other document tool uses.
async fn resolve_document_path(mcp: &WilkesMcp, path: Option<String>) -> Result<PathBuf, String> {
    let explicit = path
        .as_ref()
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from);
    let path = match (&mcp.context, explicit) {
        (_, Some(path)) => path,
        (McpContext::Session(context), None) => context
            .snapshot()
            .active_doc
            .map(|document| PathBuf::from(document.path))
            .ok_or_else(|| "No active document is available; pass path explicitly.".to_string())?,
        (McpContext::Library(_), None) => {
            return Err(EXTERNAL_DOCUMENT_PATH_REQUIRED.to_string());
        }
    };
    if !mcp.is_path_allowed(&path).await {
        return Err(mcp_access_error(&path));
    }
    Ok(path)
}

async fn get_file_metadata_for_mcp(
    mcp: &WilkesMcp,
    params: GetFileMetadataParams,
) -> Result<GetFileMetadataResponse, String> {
    let path = resolve_document_path(mcp, params.path).await?;
    let search = mcp
        .search
        .clone()
        .ok_or_else(|| "Wilkes document metadata is not available in this session.".to_string())?;
    let metadata = search.document_metadata(path.clone()).await?;
    Ok(GetFileMetadataResponse {
        path: display_path(&path),
        metadata,
    })
}

async fn list_documents_for_mcp(
    mcp: &WilkesMcp,
    params: ListDocumentsParams,
) -> Result<ListDocumentsResponse, String> {
    let search = mcp
        .search
        .clone()
        .ok_or_else(|| "Wilkes document listing is not available in this session.".to_string())?;
    let all = params.scope == Some(SearchScopeParam::All);
    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_DOCUMENTS_LIMIT)
        .clamp(1, MAX_LIST_DOCUMENTS_LIMIT);

    let roots: Vec<PathBuf> = if all {
        let roots = mcp.library_roots().await;
        if roots.is_empty() {
            return Err("No Wilkes library roots are configured.".to_string());
        }
        roots
    } else {
        let root = match params.root.as_ref() {
            Some(root) if !root.trim().is_empty() => PathBuf::from(root),
            Some(_) => return Err("List-documents root cannot be empty.".to_string()),
            None => mcp.current_root().await?,
        };
        // The external, library-scoped server must not list outside configured
        // roots; a session server already trusts its own root.
        if matches!(mcp.context, McpContext::Library(_))
            && !is_within_roots(&root, &mcp.library_roots().await)
        {
            return Err(mcp_access_error(&root));
        }
        vec![root]
    };

    let display_root = if all {
        "all".to_string()
    } else {
        display_path(&roots[0])
    };

    let mut documents = Vec::new();
    let mut truncated = false;
    'outer: for root in roots {
        let listed = search.clone().list_documents(root).await?;
        for entry in listed.files {
            if documents.len() >= limit {
                truncated = true;
                break 'outer;
            }
            documents.push(DocumentSummaryResponse::from(entry));
        }
    }

    Ok(ListDocumentsResponse {
        root: display_root,
        scope: if all {
            SearchScopeParamResponse::All
        } else {
            SearchScopeParamResponse::CurrentRoot
        },
        documents,
        truncated,
    })
}

async fn get_related_documents_for_mcp(
    mcp: &WilkesMcp,
    mut params: GetRelatedDocumentsParams,
) -> Result<GetRelatedDocumentsResponse, String> {
    let path = resolve_document_path(mcp, params.path.clone()).await?;
    params.path = Some(path.to_string_lossy().into_owned());

    match &mcp.context {
        McpContext::Session(context) => {
            get_related_documents(context, mcp.search.clone(), &mcp.cwd, params).await
        }
        McpContext::Library(_) => {
            let root = match params.root.as_ref() {
                Some(root) if !root.trim().is_empty() => PathBuf::from(root),
                Some(_) => return Err("Related-documents root cannot be empty.".to_string()),
                None => mcp.current_root().await?,
            };
            if !is_within_roots(&root, &mcp.library_roots().await) {
                return Err(mcp_access_error(&root));
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
    max_file_size: u64,
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
            max_file_size,
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
        let mut matches = file
            .field_matches
            .into_iter()
            .map(SearchMatchResponse::from)
            .collect::<Vec<_>>();
        matches.extend(file.matches.into_iter().map(SearchMatchResponse::from));
        Self {
            path: display_path(&file.path),
            file_type: file.file_type,
            title: file.title,
            author: None,
            doi: None,
            matches,
        }
    }
}

impl From<wilkes_core::types::SearchFieldMatch> for SearchMatchResponse {
    fn from(matched: wilkes_core::types::SearchFieldMatch) -> Self {
        let kind = match matched.field {
            wilkes_core::types::SearchField::Filename => SearchMatchKindResponse::Filename,
            wilkes_core::types::SearchField::Title => SearchMatchKindResponse::Title,
        };
        Self {
            kind,
            text: format!(
                "{}{}{}",
                matched.context_before, matched.matched_text, matched.context_after
            ),
            line: None,
            page: None,
            score: None,
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
            kind: SearchMatchKindResponse::Content,
            text,
            line,
            page,
            score: matched.score,
        }
    }
}

fn structured(value: impl Serialize) -> CallToolResult {
    match serde_json::to_value(value) {
        Ok(value) => match serde_yaml_ng::to_string(&value) {
            Ok(yaml) => {
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(yaml)];
                result
            }
            Err(err) => CallToolResult::error(vec![ContentBlock::text(format!(
                "Failed to format Wilkes MCP response: {err}"
            ))]),
        },
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
        DocumentMetadata, FileEntry, FileListResponse, FileMatches, FileType, Match,
        RelatedDocument, RelatedDocumentsQuery, SearchField, SearchFieldMatch, SearchMode,
        SearchQuery, SearchScope, SearchStats, SourceOrigin,
    };

    struct FakeSearch {
        last_query: Mutex<Option<SearchQuery>>,
        last_related_query: Mutex<Option<RelatedDocumentsQuery>>,
        default_root: Option<PathBuf>,
        library_roots: Vec<PathBuf>,
        response: Mutex<Option<CollectedSearch>>,
        related_response: Mutex<Option<Vec<RelatedDocument>>>,
        documents: Mutex<Option<FileListResponse>>,
        metadata: Mutex<Option<DocumentMetadata>>,
    }

    #[test]
    fn structured_response_uses_yaml_text_and_preserves_structured_content() {
        let response = structured(serde_json::json!({
            "document": {
                "title": "Readable MCP responses",
                "pages": 12
            },
            "tags": ["mcp", "yaml"]
        }));
        let serialized = serde_json::to_value(response).unwrap();

        assert_eq!(
            serialized["structuredContent"]["document"]["title"],
            "Readable MCP responses"
        );
        assert_eq!(serialized["structuredContent"]["tags"][1], "yaml");

        let text = serialized["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("document:\n"), "unexpected YAML: {text}");
        assert!(text.contains("  title: Readable MCP responses\n"));
        assert!(text.contains("tags:\n- mcp\n- yaml\n"));
        assert!(!text.trim_start().starts_with('{'));
    }

    #[test]
    fn search_tool_schema_keeps_optional_parameters_typed() {
        let mcp = WilkesMcp::new(
            McpContext::Library(ExternalMcpContext::default()),
            PathBuf::new(),
            None,
            None,
        );
        let search = &mcp.tool_router.map.get("search").unwrap().attr.input_schema;
        let properties = search
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .unwrap();

        assert_eq!(properties["max_results"]["type"], "integer");
        assert_eq!(properties["context_lines"]["type"], "integer");
        assert_eq!(properties["case_sensitive"]["type"], "boolean");
        assert_eq!(properties["is_regex"]["type"], "boolean");
        assert_eq!(properties["root"]["type"], "string");
        let scope_ref = properties["scope"]["$ref"].as_str().unwrap();
        let search_value = serde_json::Value::Object(search.as_ref().clone());
        let scope_schema = search_value
            .pointer(scope_ref.strip_prefix('#').unwrap())
            .unwrap();
        assert_eq!(scope_schema["type"], "string");
        assert_eq!(scope_schema["enum"], serde_json::json!(["all"]));

        let required = search
            .get("required")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert_eq!(
            required
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>(),
            vec!["query", "mode"]
        );
    }

    #[test]
    fn document_text_schema_describes_page_range_format() {
        let mcp = WilkesMcp::new(
            McpContext::Library(ExternalMcpContext::default()),
            PathBuf::new(),
            None,
            None,
        );
        let schema = &mcp
            .tool_router
            .map
            .get("get_document_text")
            .unwrap()
            .attr
            .input_schema;
        let properties = schema["properties"].as_object().unwrap();

        assert!(properties["page"]["description"]
            .as_str()
            .unwrap()
            .contains("1-based PDF page"));
        assert!(properties["page_range"]["description"]
            .as_str()
            .unwrap()
            .contains("\"N-M\""));
        assert_eq!(properties["page_range"]["type"], "string");
        assert!(properties["max_chars"]["description"]
            .as_str()
            .unwrap()
            .contains("default 24000"));
    }

    #[test]
    fn all_tool_schemas_omit_nullable_unions() {
        let mcp = WilkesMcp::new(
            McpContext::Library(ExternalMcpContext::default()),
            PathBuf::new(),
            None,
            None,
        );
        for route in mcp.tool_router.map.values() {
            assert_schema_has_no_null_union(
                &serde_json::Value::Object(route.attr.input_schema.as_ref().clone()),
                route.name(),
            );
        }
    }

    #[test]
    fn search_params_coerce_stringified_scalars() {
        let params: SearchParams = serde_json::from_value(serde_json::json!({
            "query": "test",
            "mode": "semantic",
            "scope": "all",
            "max_results": "5",
            "case_sensitive": "false",
            "is_regex": "true",
            "context_lines": "2"
        }))
        .unwrap();
        assert_eq!(params.max_results, Some(5));
        assert_eq!(params.case_sensitive, Some(false));
        assert_eq!(params.is_regex, Some(true));
        assert_eq!(params.context_lines, Some(2));
    }

    #[test]
    fn document_text_params_accept_page_range_string() {
        let params: GetDocumentTextParams = serde_json::from_value(serde_json::json!({
            "path": "paper.pdf",
            "page_range": "7-10"
        }))
        .unwrap();
        assert_eq!(params.page_range.as_deref(), Some("7-10"));

        assert!(
            serde_json::from_value::<GetDocumentTextParams>(serde_json::json!({
                "path": "paper.pdf",
                "page_range": { "start": 7, "end": 10 }
            }))
            .is_err()
        );
    }

    #[test]
    fn parses_page_range_string() {
        assert_eq!(parse_page_range("7-10").unwrap(), (7, 10));
        assert_eq!(parse_page_range(" 10 - 7 ").unwrap(), (7, 10));
    }

    #[test]
    fn rejects_invalid_page_range_strings() {
        for value in ["", "7", "0-2", "1-0", "1-2-3", "one-two"] {
            let error = parse_page_range(value).unwrap_err();
            assert!(error.contains("Use \"N-M\""), "unexpected error: {error}");
        }
    }

    #[test]
    fn mcp_scalar_coercion_rejects_invalid_strings() {
        assert!(serde_json::from_value::<SearchParams>(serde_json::json!({
            "query": "test",
            "mode": "exact",
            "scope": "all",
            "max_results": "5.0"
        }))
        .is_err());
        assert!(serde_json::from_value::<SearchParams>(serde_json::json!({
            "query": "test",
            "mode": "exact",
            "scope": "all",
            "case_sensitive": "yes"
        }))
        .is_err());
    }

    fn assert_schema_has_no_null_union(value: &serde_json::Value, path: &str) {
        match value {
            serde_json::Value::Object(schema) => {
                if let Some(serde_json::Value::Array(types)) = schema.get("type") {
                    assert!(
                        !types.iter().any(|value| value == "null"),
                        "nullable type remained in {path}: {value}"
                    );
                }
                if let Some(serde_json::Value::Array(values)) = schema.get("enum") {
                    assert!(
                        !values.iter().any(serde_json::Value::is_null),
                        "nullable enum remained in {path}: {value}"
                    );
                }
                for keyword in ["anyOf", "oneOf"] {
                    if let Some(serde_json::Value::Array(branches)) = schema.get(keyword) {
                        assert!(
                            !branches.iter().any(is_null_schema),
                            "nullable {keyword} branch remained in {path}: {value}"
                        );
                    }
                }
                for (key, nested) in schema {
                    assert_schema_has_no_null_union(nested, &format!("{path}.{key}"));
                }
            }
            serde_json::Value::Array(values) => {
                for (index, nested) in values.iter().enumerate() {
                    assert_schema_has_no_null_union(nested, &format!("{path}[{index}]"));
                }
            }
            _ => {}
        }
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
            if self.library_roots.is_empty() {
                self.default_root.clone().into_iter().collect()
            } else {
                self.library_roots.clone()
            }
        }

        async fn max_search_file_size(self: Arc<Self>) -> u64 {
            23 * 1024 * 1024
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

        async fn list_documents(
            self: Arc<Self>,
            _root: PathBuf,
        ) -> Result<FileListResponse, String> {
            Ok(self
                .documents
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(FileListResponse {
                    files: Vec::new(),
                    omitted: Vec::new(),
                }))
        }

        async fn document_metadata(
            self: Arc<Self>,
            _path: PathBuf,
        ) -> Result<DocumentMetadata, String> {
            self.metadata
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| "no metadata".to_string())
        }
    }

    fn fake_search_with_root(root: PathBuf) -> Arc<FakeSearch> {
        Arc::new(FakeSearch {
            last_query: Mutex::new(None),
            last_related_query: Mutex::new(None),
            default_root: Some(root.clone()),
            library_roots: vec![root],
            response: Mutex::new(None),
            related_response: Mutex::new(None),
            documents: Mutex::new(None),
            metadata: Mutex::new(None),
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
            McpContext::Library(ExternalMcpContext::default()),
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
        assert!(error.contains("not in a configured Wilkes library root"));
    }

    #[tokio::test]
    async fn external_context_requires_explicit_document_path() {
        let library = tempdir().unwrap();
        let mcp = WilkesMcp::new(
            McpContext::Library(ExternalMcpContext::default()),
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
            McpContext::Library(ExternalMcpContext::default()),
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
        assert!(error.contains("not in a configured Wilkes library root"));
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
            ExternalMcpContext::default(),
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
        assert!(names.contains(&"get_document_outline"));
        assert!(names.contains(&"download"));
        let search = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "search")
            .unwrap();
        assert_eq!(
            search["inputSchema"]["properties"]["max_results"]["type"],
            "integer"
        );
        assert_eq!(
            search["inputSchema"]["properties"]["case_sensitive"]["type"],
            "boolean"
        );
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
            ExternalMcpContext::default(),
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

    #[tokio::test]
    async fn reads_active_document_when_path_is_omitted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("active.txt");
        std::fs::write(&path, "active document text").unwrap();
        let context = ContextStateHandle::default();
        context.set_active_doc(Some(path.to_string_lossy().into_owned()), None);
        let mcp = WilkesMcp::new(
            McpContext::Session(context),
            dir.path().to_path_buf(),
            None,
            None,
        );

        let response = get_document_text_for_mcp(
            &mcp,
            GetDocumentTextParams {
                path: None,
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(response.text, "active document text");
        assert!(!response.truncated);
    }

    #[tokio::test]
    async fn document_outline_reads_active_document_without_a_semantic_index() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("active.md");
        std::fs::write(&path, "# Overview\ntext\n## Details\nmore text\n").unwrap();
        let context = ContextStateHandle::default();
        context.set_active_doc(Some(path.to_string_lossy().into_owned()), None);
        let mcp = WilkesMcp::new(
            McpContext::Session(context),
            dir.path().to_path_buf(),
            None,
            None,
        );

        let response = get_document_outline_for_mcp(&mcp, GetDocumentOutlineParams { path: None })
            .await
            .unwrap();

        assert_eq!(response.path, display_path(&path));
        assert_eq!(response.outline.len(), 2);
        assert_eq!(response.outline[0].title, "Overview");
        assert_eq!(response.outline[0].level, 0);
        assert_eq!(response.outline[1].title, "Details");
        assert_eq!(response.outline[1].level, 1);
        assert_eq!(response.extraction.pages, 0);
    }

    #[tokio::test]
    async fn external_document_outline_requires_an_explicit_path() {
        let library = tempdir().unwrap();
        let mcp = WilkesMcp::new(
            McpContext::Library(ExternalMcpContext::default()),
            PathBuf::new(),
            Some(fake_search_with_root(library.path().to_path_buf())),
            None,
        );

        let error = get_document_outline_for_mcp(&mcp, GetDocumentOutlineParams { path: None })
            .await
            .unwrap_err();

        assert!(error.contains("pass path explicitly"));
    }

    #[tokio::test]
    async fn reads_explicit_context_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("context.txt");
        std::fs::write(&path, "context document text").unwrap();
        let context = ContextStateHandle::default();
        context.add_context(path.to_string_lossy().into_owned(), None);
        let mcp = WilkesMcp::new(
            McpContext::Session(context),
            dir.path().to_path_buf(),
            None,
            None,
        );

        let response = get_document_text_for_mcp(
            &mcp,
            GetDocumentTextParams {
                path: Some(path.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(response.text, "context document text");
    }

    #[tokio::test]
    async fn reads_document_nested_in_current_root() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let path = nested.join("document.txt");
        std::fs::write(&path, "root document text").unwrap();
        let context = ContextStateHandle::default();
        context.set_search_root(Some(dir.path().to_string_lossy().into_owned()));
        let mcp = WilkesMcp::new(
            McpContext::Session(context),
            dir.path().to_path_buf(),
            None,
            None,
        );

        let response = get_document_text_for_mcp(
            &mcp,
            GetDocumentTextParams {
                path: Some(path.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(response.text, "root document text");
    }

    #[tokio::test]
    async fn in_app_mcp_reads_document_from_another_library_root() {
        let dir = tempdir().unwrap();
        let current_root = dir.path().join("current");
        let other_root = dir.path().join("other");
        std::fs::create_dir(&current_root).unwrap();
        std::fs::create_dir(&other_root).unwrap();
        let path = other_root.join("document.txt");
        std::fs::write(&path, "other library root text").unwrap();

        let context = ContextStateHandle::default();
        context.set_search_root(Some(current_root.to_string_lossy().into_owned()));
        let search = Arc::new(FakeSearch {
            last_query: Mutex::new(None),
            last_related_query: Mutex::new(None),
            default_root: Some(current_root.clone()),
            library_roots: vec![current_root.clone(), other_root],
            response: Mutex::new(None),
            related_response: Mutex::new(None),
            documents: Mutex::new(None),
            metadata: Mutex::new(None),
        });
        let mcp = WilkesMcp::new(
            McpContext::Session(context),
            current_root,
            Some(search),
            None,
        );

        let resolved = resolve_document_path(&mcp, Some(path.to_string_lossy().into_owned()))
            .await
            .unwrap();
        assert_eq!(resolved, path);

        let response = get_document_text_for_mcp(
            &mcp,
            GetDocumentTextParams {
                path: Some(path.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(response.text, "other library root text");
    }

    #[tokio::test]
    async fn denies_file_outside_library_roots_and_context_with_guidance() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("library");
        let sibling = dir.path().join("library-other");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        let denied = sibling.join("denied.txt");
        std::fs::write(&denied, "denied").unwrap();
        let context = ContextStateHandle::default();
        context.set_search_root(Some(root.to_string_lossy().into_owned()));
        let mcp = WilkesMcp::new(
            McpContext::Session(context),
            root.clone(),
            Some(fake_search_with_root(root)),
            None,
        );

        let err = get_document_text_for_mcp(
            &mcp,
            GetDocumentTextParams {
                path: Some(denied.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .await
        .unwrap_err();

        assert_eq!(
            err,
            format!(
                "{} is not in a configured Wilkes library root or this chat's context. Open its containing directory as a Wilkes root or add the file to the chat context.",
                denied.display()
            )
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn denies_symlink_in_current_root_that_resolves_outside_it() {
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
        let mcp = WilkesMcp::new(
            McpContext::Session(context),
            root.clone(),
            Some(fake_search_with_root(root)),
            None,
        );

        let err = get_document_text_for_mcp(
            &mcp,
            GetDocumentTextParams {
                path: Some(link.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: None,
            },
        )
        .await
        .unwrap_err();

        assert!(err.contains("not in a configured Wilkes library root"));
    }

    #[tokio::test]
    async fn limits_text_on_character_boundary() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("unicode.txt");
        std::fs::write(&path, "aé日b").unwrap();
        let context = ContextStateHandle::default();
        context.add_context(path.to_string_lossy().into_owned(), None);
        let mcp = WilkesMcp::new(
            McpContext::Session(context),
            dir.path().to_path_buf(),
            None,
            None,
        );

        let response = get_document_text_for_mcp(
            &mcp,
            GetDocumentTextParams {
                path: Some(path.to_string_lossy().into_owned()),
                page: None,
                page_range: None,
                max_chars: Some(3),
            },
        )
        .await
        .unwrap();

        assert_eq!(response.text, "aé日");
        assert!(response.truncated);
    }

    #[tokio::test]
    async fn explicit_page_range_overrides_active_document_page() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("active.txt");
        std::fs::write(&path, "active document text").unwrap();
        let context = ContextStateHandle::default();
        context.set_active_doc(Some(path.to_string_lossy().into_owned()), Some(3));
        let mcp = WilkesMcp::new(
            McpContext::Session(context),
            dir.path().to_path_buf(),
            None,
            None,
        );

        let response = get_document_text_for_mcp(
            &mcp,
            GetDocumentTextParams {
                path: None,
                page: None,
                page_range: Some("1-5".to_string()),
                max_chars: None,
            },
        )
        .await
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
            23 * 1024 * 1024,
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
        assert_eq!(query.max_file_size, 23 * 1024 * 1024);
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
            0,
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
            0,
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
            0,
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
            0,
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
            library_roots: Vec::new(),
            response: Mutex::new(None),
            related_response: Mutex::new(Some(Vec::new())),
            documents: Mutex::new(None),
            metadata: Mutex::new(None),
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
            library_roots: Vec::new(),
            response: Mutex::new(Some(CollectedSearch {
                files: vec![FileMatches {
                    path: path.clone(),
                    file_type: FileType::Pdf,
                    title: None,
                    field_matches: vec![SearchFieldMatch {
                        field: SearchField::Filename,
                        matched_text: "paper".into(),
                        context_before: String::new(),
                        context_after: ".pdf".into(),
                    }],
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
                    total_matches: 2,
                    catalog_elapsed_ms: 0,
                    elapsed_ms: 4,
                    indexed_pdf_reads: 0,
                    live_pdf_fallbacks: 0,
                    index_unavailable_fallbacks: 0,
                    errors: Vec::new(),
                    hyde_documents: Vec::new(),
                },
                truncated: false,
            })),
            related_response: Mutex::new(None),
            documents: Mutex::new(None),
            metadata: Mutex::new(None),
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
        assert_eq!(captured.max_file_size, 23 * 1024 * 1024);
        assert_eq!(captured.root, live_root);
        assert_eq!(response.mode, SearchModeParam::Semantic);
        assert_eq!(response.root, display_path(&live_root));
        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.matches[0].path, display_path(&path));
        assert_eq!(
            response.matches[0].matches[0].kind,
            SearchMatchKindResponse::Filename
        );
        assert_eq!(response.matches[0].matches[0].text, "paper.pdf");
        assert_eq!(response.matches[0].matches[0].page, None);
        assert_eq!(
            response.matches[0].matches[1].kind,
            SearchMatchKindResponse::Content
        );
        assert_eq!(response.matches[0].matches[1].page, Some(3));
        assert_eq!(
            response.matches[0].matches[1].text,
            "before IO programming after"
        );
        let serialized = serde_json::to_value(&response.matches[0].matches[1]).unwrap();
        assert_eq!(serialized["kind"], "content");
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
            library_roots: Vec::new(),
            response: Mutex::new(Some(CollectedSearch {
                files: Vec::new(),
                stats: SearchStats::default(),
                truncated: false,
            })),
            related_response: Mutex::new(None),
            documents: Mutex::new(None),
            metadata: Mutex::new(None),
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

    fn doc_entry(path: PathBuf, title: &str, doi: Option<&str>) -> FileEntry {
        FileEntry {
            path,
            size_bytes: 1,
            file_type: FileType::Pdf,
            extension: "pdf".into(),
            created_at_ms: None,
            modified_at_ms: None,
            title: Some(title.into()),
            author: Some("Author".into()),
            doi: doi.map(Into::into),
            publication_date: None,
            citation_count: None,
            metadata_conflicts: Default::default(),
            tags: Vec::new(),
        }
    }

    #[tokio::test]
    async fn list_documents_reports_titles_dois_and_truncation() {
        let library = tempdir().unwrap();
        let service = fake_search_with_root(library.path().to_path_buf());
        *service.documents.lock().unwrap() = Some(FileListResponse {
            files: vec![
                doc_entry(library.path().join("a.pdf"), "Alpha", Some("10.1/a")),
                doc_entry(library.path().join("b.pdf"), "Beta", None),
                doc_entry(library.path().join("c.pdf"), "Gamma", Some("10.1/c")),
            ],
            omitted: Vec::new(),
        });
        let mcp = WilkesMcp::new(
            McpContext::Library(ExternalMcpContext::default()),
            PathBuf::new(),
            Some(service),
            None,
        );

        let response = list_documents_for_mcp(
            &mcp,
            ListDocumentsParams {
                root: None,
                scope: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();

        assert_eq!(response.documents.len(), 2);
        assert!(response.truncated);
        assert_eq!(response.documents[0].title.as_deref(), Some("Alpha"));
        assert_eq!(response.documents[0].doi.as_deref(), Some("10.1/a"));
        assert_eq!(response.documents[1].title.as_deref(), Some("Beta"));
        assert_eq!(response.documents[1].doi, None);
    }

    #[tokio::test]
    async fn get_file_metadata_returns_full_record() {
        let library = tempdir().unwrap();
        let doc = library.path().join("paper.pdf");
        std::fs::write(&doc, b"paper").unwrap();
        let service = fake_search_with_root(library.path().to_path_buf());
        *service.metadata.lock().unwrap() = Some(DocumentMetadata {
            title: Some("Deep Nets".into()),
            author: Some("Ada".into()),
            doi: Some("10.1/deep".into()),
            created_at: Some("2024-05".into()),
            semantic_scholar: None,
            openalex: None,
        });
        let mcp = WilkesMcp::new(
            McpContext::Library(ExternalMcpContext::default()),
            PathBuf::new(),
            Some(service),
            None,
        );

        let response = get_file_metadata_for_mcp(
            &mcp,
            GetFileMetadataParams {
                path: Some(doc.to_string_lossy().into_owned()),
            },
        )
        .await
        .unwrap();

        assert_eq!(response.path, display_path(&doc));
        assert_eq!(response.metadata.title.as_deref(), Some("Deep Nets"));
        assert_eq!(response.metadata.author.as_deref(), Some("Ada"));
        assert_eq!(response.metadata.doi.as_deref(), Some("10.1/deep"));
    }

    #[tokio::test]
    async fn get_file_metadata_requires_explicit_path_without_active_document() {
        let library = tempdir().unwrap();
        let mcp = WilkesMcp::new(
            McpContext::Library(ExternalMcpContext::default()),
            PathBuf::new(),
            Some(fake_search_with_root(library.path().to_path_buf())),
            None,
        );

        let error = get_file_metadata_for_mcp(&mcp, GetFileMetadataParams { path: None })
            .await
            .unwrap_err();
        assert!(error.contains("pass path explicitly"));
    }

    #[tokio::test]
    async fn external_context_reports_active_document_without_defaulting_get_tools() {
        let library = tempdir().unwrap();
        let active = library.path().join("active.pdf");
        std::fs::write(&active, b"active document").unwrap();
        let context = ExternalMcpContext::default();
        context.set_active_document(Some(active.to_string_lossy().into_owned()), Some(4));
        let mcp = WilkesMcp::new(
            McpContext::Library(context),
            PathBuf::new(),
            Some(fake_search_with_root(library.path().to_path_buf())),
            None,
        );

        let snapshot = mcp.context_snapshot().await;
        let active_doc = snapshot.active_doc.expect("active document");
        assert_eq!(active_doc.path, active.to_string_lossy());
        assert_eq!(active_doc.page, Some(4));

        let text_error = get_document_text_for_mcp(
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
        assert!(text_error.contains("pass path explicitly"));

        let related_error = get_related_documents_for_mcp(
            &mcp,
            GetRelatedDocumentsParams {
                path: None,
                scope: None,
                root: None,
                limit: None,
                collection_id: None,
            },
        )
        .await
        .unwrap_err();
        assert!(related_error.contains("pass path explicitly"));

        let metadata_error = get_file_metadata_for_mcp(&mcp, GetFileMetadataParams { path: None })
            .await
            .unwrap_err();
        assert!(metadata_error.contains("pass path explicitly"));
    }

    #[tokio::test]
    async fn search_results_are_enriched_with_metadata() {
        let live_root = tempdir().unwrap();
        let path = live_root.path().join("hit.pdf");
        std::fs::write(&path, b"hit").unwrap();
        let service = fake_search_with_root(live_root.path().to_path_buf());
        *service.response.lock().unwrap() = Some(CollectedSearch {
            files: vec![FileMatches {
                path: path.clone(),
                file_type: FileType::Pdf,
                title: None,
                field_matches: Vec::new(),
                matches: vec![Match {
                    text_range: None,
                    matched_text: "hit".into(),
                    context_before: String::new(),
                    context_after: String::new(),
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: None,
                    },
                    score: Some(0.5),
                }],
            }],
            stats: SearchStats::default(),
            truncated: false,
        });
        *service.metadata.lock().unwrap() = Some(DocumentMetadata {
            title: Some("Hit Paper".into()),
            author: Some("Bo".into()),
            doi: Some("10.1/hit".into()),
            created_at: None,
            semantic_scholar: None,
            openalex: None,
        });
        let context = ContextStateHandle::default();

        let response = search_documents(
            &context,
            Some(service),
            Path::new("/fallback"),
            SearchParams {
                query: "hit".into(),
                mode: SearchModeParam::Exact,
                scope: None,
                root: Some(live_root.path().to_string_lossy().into_owned()),
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

        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.matches[0].title.as_deref(), Some("Hit Paper"));
        assert_eq!(response.matches[0].author.as_deref(), Some("Bo"));
        assert_eq!(response.matches[0].doi.as_deref(), Some("10.1/hit"));
    }
}
