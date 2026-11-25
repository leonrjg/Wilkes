use crate::*;

pub(crate) fn data_paths_from(app_data: String, workspace: String) -> DataPaths {
    DataPaths {
        app_data,
        workspace,
    }
}

#[tauri::command]
pub(crate) async fn get_data_paths(window: tauri::Window) -> Result<DataPaths, String> {
    let ctx = window_context(&window);
    Ok(data_paths_from(
        ctx.shared_data_dir.display().to_string(),
        ctx.data_dir.display().to_string(),
    ))
}

/// The listing as the calling window sees it: its own workspace as the
/// active one, and the workspaces other windows show.
#[tauri::command]
pub(crate) async fn list_workspaces(
    window: tauri::Window,
) -> Result<WorkspaceState, String> {
    workspace_manager(window.app_handle())
        .state_for_view(window.label())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn create_workspace(
    app: AppHandle,
    name: String,
) -> Result<WorkspaceSummary, String> {
    workspace_manager(&app)
        .create(name)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn rename_workspace(
    app: AppHandle,
    workspace_id: String,
    name: String,
) -> Result<WorkspaceSummary, String> {
    let manager = workspace_manager(&app);
    let renamed = manager
        .rename(&workspace_id, name)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(view) = manager.view_showing(&workspace_id) {
        title_window(&app, &view, &renamed.name);
    }
    Ok(renamed)
}

#[tauri::command]
pub(crate) async fn delete_workspace(
    window: tauri::Window,
    workspace_id: String,
) -> Result<WorkspaceState, String> {
    // No search cancellation and no chat teardown, unlike `switch_workspace`:
    // the manager refuses to delete a workspace any window shows, so nothing
    // on screen belongs to the one going away.
    let manager = workspace_manager(window.app_handle());
    manager
        .delete(&workspace_id)
        .await
        .map_err(|error| error.to_string())?;
    manager
        .state_for_view(window.label())
        .await
        .map_err(|error| error.to_string())
}

/// Titles a workspace window with the workspace it shows, so the Window menu
/// and the window switcher tell two windows apart.
pub(crate) fn title_window(app: &AppHandle, view: &str, workspace_name: &str) {
    let Some(window) = app.get_webview_window(view) else {
        return;
    };
    if let Err(error) = window.set_title(&format!("Wilkes — {workspace_name}")) {
        error!("could not title window {view}: {error}");
    }
}

/// Titles `view` with the name of the workspace it shows.
pub(crate) async fn title_window_for_view(app: &AppHandle, view: &str) -> Result<(), String> {
    let manager = workspace_manager(app);
    let Some(id) = manager.view_workspace_id(view) else {
        return Ok(());
    };
    let state = manager.state().await.map_err(|error| error.to_string())?;
    let workspace = state
        .workspaces
        .iter()
        .find(|workspace| workspace.id == id)
        .ok_or_else(|| format!("The workspace window {view} shows is not listed"))?;
    title_window(app, view, &workspace.name);
    Ok(())
}

/// Brings forward the window showing `workspace_id`, or opens a new one on
/// it. One command for both because to the user they are one act: "show me
/// that workspace in a window of its own".
#[tauri::command]
pub(crate) async fn open_workspace_window(
    app: AppHandle,
    workspace_id: String,
) -> Result<(), String> {
    show_workspace_in_window(&app, &workspace_id)
        .await
        .map(|_| ())
}

/// The label of the window showing `workspace_id`, brought forward — or of a
/// new window opened on it.
pub(crate) async fn show_workspace_in_window(
    app: &AppHandle,
    workspace_id: &str,
) -> Result<String, String> {
    let manager = workspace_manager(app);
    if let Some(view) = manager.view_showing(workspace_id) {
        native_open::focus_window(app, &view)?;
        return Ok(view);
    }
    let label = format!(
        "{WORKSPACE_WINDOW_LABEL_PREFIX}{}",
        uuid::Uuid::new_v4().simple()
    );
    // Bound before the window exists, so the first command its interface
    // sends already resolves to this workspace.
    manager
        .open_view(&label, workspace_id)
        .await
        .map_err(|error| error.to_string())?;
    let built =
        tauri::WebviewWindowBuilder::new(app, &label, tauri::WebviewUrl::App("index.html".into()))
            .title("Wilkes")
            .inner_size(1200.0, 800.0)
            .min_inner_size(800.0, 200.0)
            .title_bar_style(tauri::TitleBarStyle::Transparent)
            .build();
    if let Err(error) = built {
        manager
            .close_view(&label)
            .await
            .map_err(|close| format!("{close:#}"))?;
        return Err(format!("Could not open a window: {error}"));
    }
    title_window_for_view(app, &label).await?;
    Ok(label)
}

#[tauri::command]
pub(crate) async fn switch_workspace(
    app: AppHandle,
    window: tauri::Window,
    workspace_id: String,
) -> Result<WorkspaceState, String> {
    let manager = workspace_manager(&app);
    let view = window.label();
    let current = manager
        .state_for_view(view)
        .await
        .map_err(|error| error.to_string())?;
    if current.active_workspace_id == workspace_id {
        return Ok(current);
    }
    if !current
        .workspaces
        .iter()
        .any(|workspace| workspace.id == workspace_id)
    {
        return Err("Unknown workspace".to_string());
    }
    // Shown in another window: that window is brought forward and this one
    // stays where it is. The interface checks first; this is the answer to
    // a listing that went stale between its check and this call.
    if let Some(other) = manager.view_showing(&workspace_id) {
        native_open::focus_window(&app, &other)?;
        return Err("This workspace is already open in another window".to_string());
    }

    // Open chats are left alone: a session is the window's, and its MCP server
    // resolves the workspace on every call rather than holding the one it
    // started in.
    active_searches_state(&app).cancel_view(view);

    // The external MCP listener is not stopped and restarted around the
    // switch: it resolves its workspace through the manager on every call, so
    // it follows the default workspace on its own. Restarting it here would
    // only drop connected clients.
    let state = manager
        .switch_view(view, &workspace_id)
        .await
        .map_err(|error| error.to_string())?;
    title_window_for_view(&app, view).await?;
    Ok(state)
}
