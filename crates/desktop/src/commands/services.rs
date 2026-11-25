use crate::*;

pub(crate) const EXTERNAL_MCP_TOKEN_FILENAME: &str = "external-mcp-token";

pub(crate) struct ManagedExternalMcp {
    pub(crate) bind_address: std::net::IpAddr,
    pub(crate) port: u16,
    pub(crate) runtime: wilkes_agent::mcp::ExternalMcpRuntime,
}

pub(crate) struct ExternalMcpManager {
    pub(crate) token_path: PathBuf,
    pub(crate) context: wilkes_agent::mcp::ExternalMcpContext,
    pub(crate) browser: Option<Arc<dyn wilkes_agent::browser::AgentBrowser>>,
    pub(crate) runtime: tokio::sync::Mutex<Option<ManagedExternalMcp>>,
    pub(crate) last_error: Mutex<Option<String>>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ExternalMcpStatus {
    pub(crate) enabled: bool,
    pub(crate) running: bool,
    pub(crate) require_token: bool,
    pub(crate) bind_address: std::net::IpAddr,
    pub(crate) port: u16,
    pub(crate) url: Option<String>,
    pub(crate) token: Option<String>,
    pub(crate) error: Option<String>,
}

impl ExternalMcpManager {
    pub(crate) fn new(
        data_dir: PathBuf,
        browser: Option<Arc<dyn wilkes_agent::browser::AgentBrowser>>,
    ) -> Self {
        Self {
            token_path: data_dir.join(EXTERNAL_MCP_TOKEN_FILENAME),
            context: wilkes_agent::mcp::ExternalMcpContext::default(),
            browser,
            runtime: tokio::sync::Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }

    pub(crate) fn set_active_document(&self, path: Option<String>, page: Option<u32>) {
        self.context.set_active_document(path, page);
    }

    /// The listener is given the workspace manager rather than one workspace's
    /// context, so it answers for whichever workspace is active — and for any
    /// workspace a call names — without being restarted.
    pub(crate) async fn apply(
        &self,
        settings: &ExternalMcpSettings,
        workspaces: Arc<WorkspaceManager>,
    ) -> Result<(), String> {
        if !settings.enabled {
            let managed = self.runtime.lock().await.take();
            if let Some(managed) = managed {
                managed.runtime.shutdown().await;
            }
            *self.last_error.lock().unwrap() = None;
            return Ok(());
        }
        if settings.port == 0 {
            let message = "External MCP port must be between 1 and 65535".to_string();
            *self.last_error.lock().unwrap() = Some(message.clone());
            return Err(message);
        }

        let token = if settings.require_token {
            match wilkes_core::token_file::load_or_create(&self.token_path) {
                Ok(token) => Some(token),
                Err(error) => {
                    let message = format!("Could not load the external MCP credential: {error:#}");
                    *self.last_error.lock().unwrap() = Some(message.clone());
                    return Err(message);
                }
            }
        } else {
            None
        };

        let mut runtime = self.runtime.lock().await;
        if let Some(managed) = runtime.as_ref().filter(|managed| {
            managed.bind_address == settings.bind_address && managed.port == settings.port
        }) {
            managed.runtime.set_token(token);
            *self.last_error.lock().unwrap() = None;
            return Ok(());
        }

        let workspaces: Arc<dyn wilkes_agent::search::WorkspaceCatalog> = workspaces;
        let start = || {
            wilkes_agent::mcp::start_external(
                settings.bind_address,
                settings.port,
                token.clone(),
                Arc::clone(&workspaces),
                self.context.clone(),
                self.browser.clone(),
            )
        };
        let mut next = start().await;
        let previous = if next.is_err()
            && runtime
                .as_ref()
                .is_some_and(|managed| managed.port == settings.port)
        {
            let previous = runtime.take().expect("checked above");
            let previous_address = previous.bind_address;
            let previous_port = previous.port;
            let previous_token = previous.runtime.token();
            previous.runtime.shutdown().await;
            next = start().await;
            Some((previous_address, previous_port, previous_token))
        } else {
            None
        };

        match next {
            Ok(next) => {
                *runtime = Some(ManagedExternalMcp {
                    bind_address: settings.bind_address,
                    port: settings.port,
                    runtime: next,
                });
                *self.last_error.lock().unwrap() = None;
                Ok(())
            }
            Err(error) => {
                if let Some((previous_address, previous_port, previous_token)) = previous {
                    match wilkes_agent::mcp::start_external(
                        previous_address,
                        previous_port,
                        previous_token,
                        workspaces,
                        self.context.clone(),
                        self.browser.clone(),
                    )
                    .await
                    {
                        Ok(previous_runtime) => {
                            *runtime = Some(ManagedExternalMcp {
                                bind_address: previous_address,
                                port: previous_port,
                                runtime: previous_runtime,
                            });
                        }
                        Err(restore_error) => {
                            let message = format!(
                                "Could not start Wilkes MCP on {}:{}: {error:#}; the previous listener could not be restored: {restore_error:#}",
                                settings.bind_address, settings.port
                            );
                            *self.last_error.lock().unwrap() = Some(message.clone());
                            return Err(message);
                        }
                    }
                }
                let message = format!(
                    "Could not start Wilkes MCP on {}:{}: {error:#}",
                    settings.bind_address, settings.port
                );
                *self.last_error.lock().unwrap() = Some(message.clone());
                Err(message)
            }
        }
    }

    pub(crate) async fn status(&self, settings: &ExternalMcpSettings) -> ExternalMcpStatus {
        let runtime = self.runtime.lock().await;
        let token = runtime
            .as_ref()
            .filter(|_| settings.require_token)
            .and_then(|managed| managed.runtime.token());
        ExternalMcpStatus {
            enabled: settings.enabled,
            running: runtime.is_some(),
            require_token: settings.require_token,
            bind_address: settings.bind_address,
            port: settings.port,
            url: runtime
                .as_ref()
                .map(|managed| managed.runtime.url().to_string()),
            token,
            error: self.last_error.lock().unwrap().clone(),
        }
    }

    pub(crate) async fn rotate_token(&self, settings: &ExternalMcpSettings) -> Result<(), String> {
        if !settings.require_token {
            return Err("External MCP bearer authentication is not enabled".to_string());
        }
        let token = wilkes_core::token_file::generate();
        wilkes_core::token_file::write(&self.token_path, &token)
            .map_err(|error| error.to_string())?;
        if let Some(runtime) = self.runtime.lock().await.as_ref() {
            runtime.runtime.set_token(Some(token));
        }
        Ok(())
    }

    pub(crate) async fn stop(&self) {
        let managed = self.runtime.lock().await.take();
        if let Some(managed) = managed {
            managed.runtime.shutdown().await;
        }
    }
}

pub(crate) fn external_mcp_manager_state(app: &AppHandle) -> Arc<ExternalMcpManager> {
    app.state::<Arc<ExternalMcpManager>>().inner().clone()
}

pub(crate) struct ManagedHttpApi {
    pub(crate) bind_address: std::net::IpAddr,
    pub(crate) port: u16,
    pub(crate) runtime: wilkes_server::ApiRuntime,
}

/// Starts and stops the Wilkes HTTP API over the workspace this app already
/// owns.
///
/// The point is not convenience. A workspace has exactly one owner, and a
/// second process that opens the same directory races this one for
/// `settings.json` and the semantic index. Serving the API from inside the
/// owner is what lets another program read the library without becoming that
/// second owner.
pub(crate) struct HttpApiManager {
    /// Built once and reused across restarts: it resolves the active workspace
    /// per request, so toggling the listener must not re-derive which
    /// workspace is current.
    pub(crate) state: Arc<wilkes_server::http::state::AppState>,
    /// Where the token the API requires, when `require_token` is on, is kept.
    /// Created the first time it is turned on, and kept across restarts so the
    /// clients holding it keep working.
    pub(crate) token_path: PathBuf,
    pub(crate) runtime: tokio::sync::Mutex<Option<ManagedHttpApi>>,
    pub(crate) last_error: Mutex<Option<String>>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct HttpApiStatus {
    pub(crate) enabled: bool,
    pub(crate) running: bool,
    pub(crate) bind_address: std::net::IpAddr,
    pub(crate) port: u16,
    pub(crate) url: Option<String>,
    /// Whether every request must carry `token`.
    pub(crate) require_token: bool,
    /// The token the running listener requires, shown so it can be copied to
    /// the machines that call it.
    pub(crate) token: Option<String>,
    pub(crate) error: Option<String>,
}

pub(crate) const HTTP_API_TOKEN_FILENAME: &str = "http-api-token";

impl HttpApiManager {
    pub(crate) fn new(
        state: Arc<wilkes_server::http::state::AppState>,
        token_path: PathBuf,
    ) -> Self {
        Self {
            state,
            token_path,
            runtime: tokio::sync::Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }

    /// Brings the listener into the state `settings` describes.
    ///
    /// A failed start leaves nothing listening and says why. It deliberately
    /// does not resurrect the previous listener: the settings would then
    /// describe a port that is not the one in use, and a silently different
    /// answer to "where is the API" is worse than none.
    pub(crate) async fn apply(&self, settings: &HttpApiSettings) -> Result<(), String> {
        let mut runtime = self.runtime.lock().await;

        if !settings.enabled {
            if let Some(managed) = runtime.take() {
                managed.runtime.shutdown().await;
            }
            *self.last_error.lock().unwrap() = None;
            return Ok(());
        }

        let token = if settings.require_token {
            match wilkes_core::token_file::load_or_create(&self.token_path) {
                Ok(token) => Some(token),
                Err(error) => {
                    let message = format!("Could not load the HTTP API's token: {error:#}");
                    error!("{message}");
                    *self.last_error.lock().unwrap() = Some(message.clone());
                    return Err(message);
                }
            }
        } else {
            None
        };

        if let Some(managed) = runtime.as_ref().filter(|managed| {
            managed.bind_address == settings.bind_address && managed.port == settings.port
        }) {
            managed.runtime.set_token(token);
            *self.last_error.lock().unwrap() = None;
            return Ok(());
        }

        // The old listener goes down first and is awaited, so the new one
        // cannot lose a race with it for the same port.
        if let Some(managed) = runtime.take() {
            managed.runtime.shutdown().await;
        }

        // The standalone server creates this before it serves, and the upload
        // routes assume it. Without it the same request would succeed against
        // one shell and fail against the other.
        let uploads_dir = self.state.context().data_dir.join("uploads");
        if let Err(error) = tokio::fs::create_dir_all(&uploads_dir).await {
            // Not fatal: it fails only the upload routes, which the desktop
            // does not use itself, and refusing to serve the rest over it
            // would be the larger breakage.
            error!(
                "could not create {} for the HTTP API: {error:#}",
                uploads_dir.display()
            );
        }

        match wilkes_server::start_api(
            settings.bind_address,
            settings.port,
            Arc::clone(&self.state),
            token,
        )
        .await
        {
            Ok(started) => {
                info!("Wilkes HTTP API listening on {}", started.url());
                *runtime = Some(ManagedHttpApi {
                    bind_address: settings.bind_address,
                    port: settings.port,
                    runtime: started,
                });
                *self.last_error.lock().unwrap() = None;
                Ok(())
            }
            Err(error) => {
                let message = format!(
                    "Could not start the Wilkes HTTP API on {}:{}: {error:#}",
                    settings.bind_address, settings.port
                );
                error!("{message}");
                *self.last_error.lock().unwrap() = Some(message.clone());
                Err(message)
            }
        }
    }

    pub(crate) async fn status(&self, settings: &HttpApiSettings) -> HttpApiStatus {
        let runtime = self.runtime.lock().await;
        HttpApiStatus {
            enabled: settings.enabled,
            running: runtime.is_some(),
            bind_address: settings.bind_address,
            port: settings.port,
            url: runtime.as_ref().map(|managed| managed.runtime.url()),
            require_token: settings.require_token,
            token: runtime.as_ref().and_then(|managed| managed.runtime.token()),
            error: self.last_error.lock().unwrap().clone(),
        }
    }

    /// Replace the token, revoking the old one at once. Only meaningful with
    /// `require_token` on, when a token is required at all.
    pub(crate) async fn rotate_token(&self, settings: &HttpApiSettings) -> Result<(), String> {
        if !settings.require_token {
            return Err("The HTTP API does not require a token".to_string());
        }
        let token = wilkes_core::token_file::generate();
        wilkes_core::token_file::write(&self.token_path, &token)
            .map_err(|error| error.to_string())?;
        if let Some(managed) = self.runtime.lock().await.as_ref() {
            managed.runtime.set_token(Some(token));
        }
        Ok(())
    }

    pub(crate) async fn stop(&self) {
        let managed = self.runtime.lock().await.take();
        if let Some(managed) = managed {
            managed.runtime.shutdown().await;
        }
    }
}

pub(crate) fn http_api_manager_state(app: &AppHandle) -> Arc<HttpApiManager> {
    app.state::<Arc<HttpApiManager>>().inner().clone()
}

pub(crate) fn lan_share_manager_state(app: &AppHandle) -> Arc<lan_share::LanShareManager> {
    app.state::<Arc<lan_share::LanShareManager>>()
        .inner()
        .clone()
}

#[tauri::command]
pub(crate) async fn configure_external_mcp(
    enabled: bool,
    require_token: bool,
    bind_address: String,
    port: u16,
    app: AppHandle,
    window: tauri::Window,
) -> Result<ExternalMcpStatus, String> {
    let bind_address = bind_address.parse::<std::net::IpAddr>().map_err(|_| {
        "External MCP bind address must be a valid IPv4 or IPv6 address".to_string()
    })?;
    let ctx = window_context(&window);
    let manager = external_mcp_manager_state(&app);
    let settings = update_settings_with_listeners(
        Arc::clone(&ctx),
        workspace_manager(&app),
        Arc::clone(&manager),
        http_api_manager_state(&app),
        lan_share_manager_state(&app),
        serde_json::json!({
            "external_mcp": {
                "enabled": enabled,
                "require_token": require_token,
                "bind_address": bind_address,
                "port": port,
            }
        }),
    )
    .await?;
    Ok(manager.status(&settings.external_mcp).await)
}

/// Turns the HTTP API on or off and reports where it ended up listening.
#[tauri::command]
pub(crate) async fn configure_http_api(
    enabled: bool,
    require_token: bool,
    bind_address: String,
    port: u16,
    app: AppHandle,
    window: tauri::Window,
) -> Result<HttpApiStatus, String> {
    let bind_address = bind_address
        .parse::<std::net::IpAddr>()
        .map_err(|_| "HTTP API bind address must be a valid IPv4 or IPv6 address".to_string())?;
    let manager = http_api_manager_state(&app);
    let settings = update_settings_with_listeners(
        window_context(&window),
        workspace_manager(&app),
        external_mcp_manager_state(&app),
        Arc::clone(&manager),
        lan_share_manager_state(&app),
        serde_json::json!({
            "http_api": {
                "enabled": enabled,
                "require_token": require_token,
                "bind_address": bind_address,
                "port": port,
            }
        }),
    )
    .await?;
    Ok(manager.status(&settings.http_api).await)
}

#[tauri::command]
pub(crate) async fn get_http_api_status(
    app: AppHandle,
    window: tauri::Window,
) -> Result<HttpApiStatus, String> {
    let settings = window_context(&window).get_settings().await;
    Ok(http_api_manager_state(&app)
        .status(&settings.http_api)
        .await)
}

#[tauri::command]
pub(crate) async fn rotate_http_api_token(
    app: AppHandle,
    window: tauri::Window,
) -> Result<HttpApiStatus, String> {
    let settings = window_context(&window).get_settings().await;
    let manager = http_api_manager_state(&app);
    manager.rotate_token(&settings.http_api).await?;
    Ok(manager.status(&settings.http_api).await)
}

/// Where embeddings run, and whether the host there answers.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct RemoteEmbeddingStatus {
    pub(crate) enabled: bool,
    pub(crate) address: String,
    /// Whether a token for the host is stored. The token itself is not sent
    /// back: it was typed here, and is the host's to show.
    pub(crate) has_token: bool,
    /// What the host answered when asked for its models, when it was asked.
    pub(crate) error: Option<String>,
}

pub(crate) async fn remote_embedding_status(ctx: &Arc<AppContext>) -> RemoteEmbeddingStatus {
    let settings = ctx.get_settings().await;
    let has_token = matches!(
        wilkes_core::token_file::read_if_present(&ctx.remote_embedding_token_path()),
        Ok(Some(_))
    );
    let error = if settings.remote_embedding.enabled {
        ctx.embedder_capabilities().await.err()
    } else {
        None
    };
    RemoteEmbeddingStatus {
        enabled: settings.remote_embedding.enabled,
        address: settings.remote_embedding.address,
        has_token,
        error,
    }
}

/// Run embeddings on another Wilkes instance, or on this machine again.
///
/// `token` replaces the stored one when given, and an empty string removes
/// it; `None` keeps what is stored, so the address can change without
/// retyping it.
#[tauri::command]
pub(crate) async fn configure_remote_embedding(
    enabled: bool,
    address: String,
    token: Option<String>,
    app: AppHandle,
) -> Result<RemoteEmbeddingStatus, String> {
    if enabled {
        wilkes_core::embed::engines::remote::normalize_address(&address)
            .map_err(|error| format!("{error:#}"))?;
    }
    let workspaces = workspace_manager(&app);
    workspaces
        .configure_remote_embedding(
            wilkes_core::types::RemoteEmbeddingSettings {
                enabled,
                address: address.trim().to_string(),
            },
            token,
        )
        .await?;
    Ok(remote_embedding_status(&workspaces.active()).await)
}

#[tauri::command]
pub(crate) async fn get_remote_embedding_status(
    window: tauri::Window,
) -> Result<RemoteEmbeddingStatus, String> {
    Ok(remote_embedding_status(&window_context(&window)).await)
}

/// Turns sharing with other Wilkes instances on the local network on or off.
#[tauri::command]
pub(crate) async fn configure_lan_sharing(
    enabled: bool,
    app: AppHandle,
    window: tauri::Window,
) -> Result<lan_share::LanSharingStatus, String> {
    let manager = lan_share_manager_state(&app);
    let settings = update_settings_with_listeners(
        window_context(&window),
        workspace_manager(&app),
        external_mcp_manager_state(&app),
        http_api_manager_state(&app),
        Arc::clone(&manager),
        serde_json::json!({ "lan_sharing": { "enabled": enabled } }),
    )
    .await?;
    Ok(manager.status(&settings.lan_sharing).await)
}

#[tauri::command]
pub(crate) async fn get_lan_sharing_status(
    app: AppHandle,
    window: tauri::Window,
) -> Result<lan_share::LanSharingStatus, String> {
    let settings = window_context(&window).get_settings().await;
    Ok(lan_share_manager_state(&app)
        .status(&settings.lan_sharing)
        .await)
}

#[tauri::command]
pub(crate) fn list_lan_peers(app: AppHandle) -> Vec<lan_share::LanPeer> {
    lan_share_manager_state(&app).peers()
}

#[tauri::command]
pub(crate) async fn send_files_to_lan_peer(
    peer_id: String,
    paths: Vec<String>,
    app: AppHandle,
) -> Result<(), String> {
    lan_share_manager_state(&app)
        .send(&peer_id, paths.into_iter().map(PathBuf::from).collect())
        .await
}

#[tauri::command]
pub(crate) async fn get_external_mcp_status(
    app: AppHandle,
    window: tauri::Window,
) -> Result<ExternalMcpStatus, String> {
    let settings = window_context(&window).get_settings().await;
    Ok(external_mcp_manager_state(&app)
        .status(&settings.external_mcp)
        .await)
}

#[tauri::command]
pub(crate) fn set_active_document(path: Option<String>, page: Option<u32>, app: AppHandle) {
    external_mcp_manager_state(&app).set_active_document(path, page);
}

#[tauri::command]
pub(crate) async fn rotate_external_mcp_token(
    app: AppHandle,
    window: tauri::Window,
) -> Result<ExternalMcpStatus, String> {
    let ctx = window_context(&window);
    let settings = ctx.get_settings().await;
    let manager = external_mcp_manager_state(&app);
    manager.rotate_token(&settings.external_mcp).await?;
    Ok(manager.status(&settings.external_mcp).await)
}
