use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo},
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
    session::ContextStateHandle,
};

const DEFAULT_TEXT_CHAR_LIMIT: usize = 24_000;
const MAX_TEXT_CHAR_LIMIT: usize = 120_000;
const DEFAULT_SEARCH_MAX_RESULTS: usize = 10;
const MAX_SEARCH_MAX_RESULTS: usize = 50;
const DEFAULT_SEARCH_CONTEXT_LINES: u32 = 2;
const MAX_SEARCH_CONTEXT_LINES: u32 = 5;
const DEFAULT_SEARCH_MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024;

/// Names of the read-only tools this server exposes. Shared with the permission
/// boundary in `session.rs` so calls to Wilkes's *own* MCP server are
/// auto-allowed without ever prompting the user (they are the Q&A pane's own
/// internal plumbing). Mutating tools such as `download` must not be added here,
/// so the agent's normal permission flow remains in effect.
pub(crate) const WILKES_MCP_TOOL_NAMES: &[&str] = &[
    "list_context",
    "get_document_text",
    "search",
    "literature_search",
];

pub(crate) struct McpRuntime {
    url: String,
    token: String,
    shutdown: CancellationToken,
    _server_task: tokio::task::JoinHandle<()>,
}

impl McpRuntime {
    pub(crate) fn server_config(&self) -> McpServer {
        McpServer::Http(McpServerHttp::new("wilkes", self.url.clone()).headers(vec![
            HttpHeader::new("Authorization", format!("Bearer {}", self.token)),
        ]))
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
    let shutdown = CancellationToken::new();
    let config = rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_cancellation_token(shutdown.child_token());
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

    let route_path = format!("/mcp/{token}");
    let router =
        Router::new()
            .nest_service(&route_path, service)
            .layer(middleware::from_fn_with_state(
                token.clone(),
                require_bearer_token,
            ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}{route_path}");
    let shutdown_for_task = shutdown.clone();
    let server_task = tokio::spawn(async move {
        let result = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown_for_task.cancelled_owned().await })
            .await;
        if let Err(err) = result {
            error!("chat: Wilkes MCP server exited with error: {err:#}");
        }
    });

    info!(%url, "chat: started Wilkes MCP server");
    Ok(McpRuntime {
        url,
        token,
        shutdown,
        _server_task: server_task,
    })
}

async fn require_bearer_token(
    State(token): State<String>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected = format!("Bearer {token}");
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
    context: ContextStateHandle,
    cwd: PathBuf,
    search: Option<Arc<dyn SearchService>>,
    integrations: IntegrationsSettings,
    tool_router: ToolRouter<Self>,
}

impl WilkesMcp {
    fn new(
        context: ContextStateHandle,
        cwd: PathBuf,
        search: Option<Arc<dyn SearchService>>,
        integrations: IntegrationsSettings,
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
    /// Use exact for literal/regex matching, semantic for meaning-based search.
    mode: Option<SearchModeParam>,
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
struct SearchFileResponse {
    path: String,
    file_type: wilkes_core::types::FileType,
    matches: Vec<SearchMatchResponse>,
}

#[derive(Debug, Serialize)]
struct SearchMatchResponse {
    text: String,
    context_before: String,
    context_after: String,
    line: Option<u32>,
    page: Option<u32>,
    score: Option<f32>,
}

#[tool_router]
impl WilkesMcp {
    #[tool(
        description = "List the current Wilkes chat context: active document and files explicitly added to context."
    )]
    fn list_context(&self) -> CallToolResult {
        let snapshot = self.context.snapshot();
        structured(ListContextResponse::from_snapshot(
            snapshot.active_doc,
            snapshot.context_files,
        ))
    }

    #[tool(
        description = "Read Wilkes-extracted document text from the active document or a context file. Use page for one PDF page or page_range for an inclusive PDF page range."
    )]
    fn get_document_text(
        &self,
        Parameters(params): Parameters<GetDocumentTextParams>,
    ) -> CallToolResult {
        match get_document_text(&self.context, params) {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Search Wilkes-readable documents. Use mode='exact' for literal/regex text search, or mode='semantic' to search the semantic index when it is available. If the user asks about the open/current document or a specific context file, set file to that document path; omit file only for corpus-wide searches."
    )]
    async fn search(&self, Parameters(params): Parameters<SearchParams>) -> CallToolResult {
        match search_documents(&self.context, self.search.clone(), &self.cwd, params).await {
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
        match params.provider {
            LiteratureProviderParam::SemanticScholar => {
                let settings = &self.integrations.semantic_scholar;
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
                let settings = &self.integrations.openalex;
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
        description = "Download a direct HTTP(S) file URL into the current Wilkes root. Use only when the user explicitly asks to import or download a file. Pass filename to choose the saved name; existing files are never overwritten. Literature search results may provide pdf_url values suitable for this tool."
    )]
    async fn download(&self, Parameters(params): Parameters<DownloadParams>) -> CallToolResult {
        let root = self
            .context
            .search_root()
            .unwrap_or_else(|| self.cwd.clone());
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
            .with_instructions("Wilkes document context and literature tools. Context and search tools are read-only. The download tool writes a file into the current root and must only be used when the user explicitly asks to import or download it.")
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
    fn from_snapshot(active_doc: Option<ActiveDoc>, context_files: Vec<ContextFile>) -> Self {
        Self {
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

fn get_document_text(
    context: &ContextStateHandle,
    params: GetDocumentTextParams,
) -> Result<GetDocumentTextResponse, String> {
    let started_at = Instant::now();
    let snapshot = context.snapshot();
    let (path, default_page) = match params.path {
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
        return Err(format!("{} is not in this chat's context.", path.display()));
    }

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

async fn search_documents(
    context: &ContextStateHandle,
    search: Option<Arc<dyn SearchService>>,
    cwd: &Path,
    mut params: SearchParams,
) -> Result<SearchResponse, String> {
    let search =
        search.ok_or_else(|| "Wilkes search is not available in this session.".to_string())?;
    let mode = params.mode.unwrap_or(SearchModeParam::Exact);
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
    let (query, max_files) = build_search_query(root, params, mode)?;
    let root = display_path(&query.root);
    let file = match &query.scope {
        wilkes_core::types::SearchScope::Corpus | wilkes_core::types::SearchScope::All => None,
        wilkes_core::types::SearchScope::File { path } => Some(display_path(path)),
    };
    let query_text = query.pattern.clone();
    let collected = search.search(query, max_files).await?;

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

fn build_search_query(
    root: PathBuf,
    params: SearchParams,
    mode: SearchModeParam,
) -> Result<(wilkes_core::types::SearchQuery, usize), String> {
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
                if params.scope == Some(SearchScopeParam::All) {
                    return Err("file cannot be combined with scope='all'.".to_string());
                }
                wilkes_core::types::SearchScope::File {
                    path: PathBuf::from(path),
                }
            } else if params.scope == Some(SearchScopeParam::All) {
                wilkes_core::types::SearchScope::All
            } else {
                wilkes_core::types::SearchScope::Corpus
            },
            supported_extensions: Vec::new(),
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
        Self {
            text: matched.matched_text,
            context_before: matched.context_before,
            context_after: matched.context_after,
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
        FileMatches, FileType, Match, SearchMode, SearchQuery, SearchScope, SearchStats,
        SourceOrigin,
    };

    struct FakeSearch {
        last_query: Mutex<Option<SearchQuery>>,
        default_root: Option<PathBuf>,
        response: Mutex<Option<CollectedSearch>>,
    }

    #[async_trait]
    impl SearchService for FakeSearch {
        async fn default_root(self: Arc<Self>) -> Option<PathBuf> {
            self.default_root.clone()
        }

        async fn search(
            self: Arc<Self>,
            query: SearchQuery,
            _max_files: usize,
        ) -> Result<CollectedSearch, String> {
            *self.last_query.lock().unwrap() = Some(query);
            Ok(self.response.lock().unwrap().take().unwrap())
        }
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
    fn denies_file_outside_context() {
        let dir = tempdir().unwrap();
        let allowed = dir.path().join("allowed.txt");
        let denied = dir.path().join("denied.txt");
        std::fs::write(&allowed, "allowed").unwrap();
        std::fs::write(&denied, "denied").unwrap();
        let context = ContextStateHandle::default();
        context.add_context(allowed.to_string_lossy().into_owned(), None);

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

        assert!(err.contains("is not in this chat's context"));
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
                mode: Some(SearchModeParam::Exact),
                scope: None,
                root: None,
                file: None,
                max_results: Some(500),
                case_sensitive: Some(true),
                is_regex: Some(true),
                context_lines: Some(100),
            },
            SearchModeParam::Exact,
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
                mode: Some(SearchModeParam::Semantic),
                scope: None,
                root: None,
                file: None,
                max_results: None,
                case_sensitive: None,
                is_regex: Some(true),
                context_lines: None,
            },
            SearchModeParam::Semantic,
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
                mode: None,
                scope: None,
                root: None,
                file: Some("paper.pdf".to_string()),
                max_results: None,
                case_sensitive: None,
                is_regex: None,
                context_lines: None,
            },
            SearchModeParam::Exact,
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
                mode: None,
                scope: Some(SearchScopeParam::All),
                root: None,
                file: None,
                max_results: None,
                case_sensitive: None,
                is_regex: None,
                context_lines: None,
            },
            SearchModeParam::Exact,
        )
        .unwrap();

        assert_eq!(query.scope, SearchScope::All);
    }

    #[tokio::test]
    async fn search_documents_maps_service_results() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("paper.pdf");
        let default_root = dir.path().join("active-root");
        let live_root = dir.path().join("live-ui-root");
        let service = Arc::new(FakeSearch {
            last_query: Mutex::new(None),
            default_root: Some(default_root.clone()),
            response: Mutex::new(Some(CollectedSearch {
                files: vec![FileMatches {
                    path: path.clone(),
                    file_type: FileType::Pdf,
                    matches: vec![Match {
                        text_range: None,
                        matched_text: "IO programming".to_string(),
                        context_before: "before".to_string(),
                        context_after: "after".to_string(),
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
        });
        let context = ContextStateHandle::default();
        context.set_search_root(Some(live_root.to_string_lossy().into_owned()));

        let response = search_documents(
            &context,
            Some(service.clone()),
            dir.path(),
            SearchParams {
                query: "IO".to_string(),
                mode: Some(SearchModeParam::Semantic),
                scope: None,
                root: None,
                file: None,
                max_results: Some(3),
                case_sensitive: None,
                is_regex: None,
                context_lines: None,
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
        assert_eq!(response.matches[0].matches[0].text, "IO programming");
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
            default_root: Some(dir.path().join("active-root")),
            response: Mutex::new(Some(CollectedSearch {
                files: Vec::new(),
                stats: SearchStats::default(),
                truncated: false,
            })),
        });

        let response = search_documents(
            &context,
            Some(service.clone()),
            Path::new("/fallback"),
            SearchParams {
                query: "multi-turn".to_string(),
                mode: None,
                scope: None,
                root: Some(explicit_root.to_string_lossy().into_owned()),
                file: None,
                max_results: None,
                case_sensitive: None,
                is_regex: None,
                context_lines: None,
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
                mode: None,
                scope: None,
                root: None,
                file: None,
                max_results: None,
                case_sensitive: None,
                is_regex: None,
                context_lines: None,
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
