//! Serving the MCP endpoint: the in-process and external runtimes, the HTTP
//! router, host validation and the bearer token.

use super::*;

pub(crate) struct McpRuntime {
    pub(super) url: String,
    pub(super) token: Arc<RwLock<Option<String>>>,
    pub(super) shutdown: CancellationToken,
    pub(super) _server_task: tokio::task::JoinHandle<()>,
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

    pub(super) async fn shutdown(mut self) {
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
    workspaces: Option<Arc<dyn WorkspaceCatalog>>,
    browser: Option<Arc<dyn AgentBrowser>>,
) -> anyhow::Result<McpRuntime> {
    let token = uuid::Uuid::new_v4().to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    start_server(
        listener,
        format!("/mcp/{token}"),
        Some(token),
        McpContext::Session(context),
        cwd,
        workspaces,
        browser,
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
    pub(super) active_doc: Arc<RwLock<Option<ActiveDoc>>>,
}

impl ExternalMcpContext {
    pub fn set_active_document(&self, path: Option<String>, page: Option<u32>) {
        *self
            .active_doc
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            path.map(|path| ActiveDoc { path, page });
    }

    pub(super) fn active_document(&self) -> Option<ActiveDoc> {
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
    pub(super) runtime: McpRuntime,
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
    workspaces: Arc<dyn WorkspaceCatalog>,
    context: ExternalMcpContext,
    browser: Option<Arc<dyn AgentBrowser>>,
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
        Some(workspaces),
        browser,
        HostValidation::Any,
        "external",
    )
    .await?;
    Ok(ExternalMcpRuntime { runtime })
}

/// Mount the same MCP implementation on the shared Wilkes HTTP API. This
/// endpoint is loopback-only at the protocol boundary (Host and Origin),
/// even when the outer API has a more permissive CORS policy.
pub fn api_router(workspaces: Arc<dyn WorkspaceCatalog>) -> Router {
    let service = mcp_service(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(workspaces),
        // The shared HTTP API answers with or without the desktop app, so it
        // has no browser to drive.
        None,
        rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default()
            .with_stateful_mode(false)
            .with_json_response(true)
            .with_sse_keep_alive(None),
    );
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(
            |request: Request, next: Next| async move {
                if request.headers().contains_key("origin") {
                    return Err(StatusCode::FORBIDDEN);
                }
                Ok(next.run(request).await)
            },
        ))
}

pub(super) type McpService = rmcp::transport::streamable_http_server::StreamableHttpService<
    WilkesMcp,
    rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
>;

pub(super) fn mcp_service(
    context: McpContext,
    cwd: PathBuf,
    workspaces: Option<Arc<dyn WorkspaceCatalog>>,
    browser: Option<Arc<dyn AgentBrowser>>,
    config: rmcp::transport::streamable_http_server::StreamableHttpServerConfig,
) -> McpService {
    let literature_results = LiteratureResultCache::default();
    McpService::new(
        move || {
            Ok(
                WilkesMcp::new(context.clone(), cwd.clone(), workspaces.clone())
                    .with_literature_results(literature_results.clone())
                    .with_browser(browser.clone()),
            )
        },
        Default::default(),
        config,
    )
}

#[derive(Clone, Copy, Debug)]
pub(super) enum HostValidation {
    LoopbackOnly,
    Any,
}

#[derive(Clone, Debug)]
pub(super) enum McpContext {
    Session(ContextStateHandle),
    Library(ExternalMcpContext),
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn start_server(
    listener: tokio::net::TcpListener,
    route_path: String,
    token: Option<String>,
    context: McpContext,
    cwd: PathBuf,
    workspaces: Option<Arc<dyn WorkspaceCatalog>>,
    browser: Option<Arc<dyn AgentBrowser>>,
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
    let service = mcp_service(context, cwd, workspaces, browser, config);

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

/// The cookie a browser carries the HTTP API's token in. A page cannot set an
/// `Authorization` header on an `EventSource` or an `<img src>`, so the web
/// interface exchanges the token for this cookie once and the browser sends it
/// with everything after.
pub const TOKEN_COOKIE: &str = "wilkes_api_token";

/// Admit a request that carries the current token, as a bearer header or as
/// [`TOKEN_COOKIE`]; admit every request while no token is set.
///
/// Shared by the external MCP listener and the HTTP API, so the two cannot
/// disagree about what a credential is.
pub async fn require_bearer_token(
    State(token): State<Arc<RwLock<Option<String>>>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected = {
        let token = token
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        token.clone()
    };
    let Some(expected) = expected else {
        return Ok(next.run(request).await);
    };
    let headers = request.headers();
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| value == expected);
    let cookie = headers
        .get_all("cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .any(|(name, value)| name == TOKEN_COOKIE && value == expected);
    if bearer || cookie {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}
