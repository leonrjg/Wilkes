use crate::*;

#[tauri::command]
pub(crate) async fn get_global_settings(app: AppHandle) -> Result<Settings, String> {
    wilkes_api::commands::settings::get_settings(&app.state::<GlobalSettingsPath>().0)
        .await
        .map_err(|error| error.to_string())
}

pub(crate) async fn get_settings_for_ctx(ctx: Arc<AppContext>) -> Result<Settings, String> {
    Ok(ctx.get_settings().await)
}

pub(crate) async fn update_settings_for_ctx(
    ctx: Arc<AppContext>,
    patch: serde_json::Value,
) -> Result<Settings, String> {
    ctx.update_settings(patch).await.map_err(|e| e.to_string())
}

/// Applies a settings patch, keeping the two network listeners in step with it.
///
/// Both are settings whose effect is a bound port rather than a stored value,
/// so the listener is moved *before* the write and moved back if the write
/// fails — a saved setting that describes a listener which never started is
/// the state this ordering exists to prevent.
pub(crate) async fn update_settings_with_listeners(
    ctx: Arc<AppContext>,
    workspaces: Arc<WorkspaceManager>,
    external_mcp: Arc<ExternalMcpManager>,
    http_api: Arc<HttpApiManager>,
    lan_share: Arc<lan_share::LanShareManager>,
    patch: serde_json::Value,
) -> Result<Settings, String> {
    let before = ctx.get_settings().await;
    let requested_external = patch
        .get("external_mcp")
        .cloned()
        .map(serde_json::from_value::<ExternalMcpSettings>)
        .transpose()
        .map_err(|error| format!("Invalid external MCP settings: {error}"))?;
    let requested_http = patch
        .get("http_api")
        .cloned()
        .map(serde_json::from_value::<HttpApiSettings>)
        .transpose()
        .map_err(|error| format!("Invalid HTTP API settings: {error}"))?;
    let requested_lan = patch
        .get("lan_sharing")
        .cloned()
        .map(serde_json::from_value::<LanSharingSettings>)
        .transpose()
        .map_err(|error| format!("Invalid local network sharing settings: {error}"))?;

    if requested_external.is_none() && requested_http.is_none() && requested_lan.is_none() {
        return update_settings_for_ctx(ctx, patch).await;
    }

    if let Some(requested) = &requested_external {
        external_mcp
            .apply(requested, Arc::clone(&workspaces))
            .await?;
    }
    if let Some(requested) = &requested_http {
        http_api.apply(requested).await?;
    }
    if let Some(requested) = &requested_lan {
        lan_share.apply(requested).await?;
    }

    match ctx.update_settings(patch).await {
        Ok(updated) => Ok(updated),
        Err(error) => {
            if requested_external.is_some() {
                let _ = external_mcp
                    .apply(&before.external_mcp, Arc::clone(&workspaces))
                    .await;
            }
            if requested_http.is_some() {
                let _ = http_api.apply(&before.http_api).await;
            }
            if requested_lan.is_some() {
                let _ = lan_share.apply(&before.lan_sharing).await;
            }
            Err(error.to_string())
        }
    }
}

#[tauri::command]
pub(crate) async fn get_settings(window: tauri::Window) -> Result<Settings, String> {
    get_settings_for_ctx(window_context(&window)).await
}

#[tauri::command]
pub(crate) async fn update_settings(
    patch: serde_json::Value,
    app: AppHandle,
    window: tauri::Window,
) -> Result<Settings, String> {
    update_settings_with_listeners(
        window_context(&window),
        workspace_manager(&app),
        external_mcp_manager_state(&app),
        http_api_manager_state(&app),
        lan_share_manager_state(&app),
        patch,
    )
    .await
}
