use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chat::ChatManager;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{error, info};
use wilkes_api::commands::integrations::custom::ManifestSummary;
use wilkes_api::context::{AppContext, EventAudience, EventEmitter, ViewEventSink};
use wilkes_api::startup::StartupStatus;
use wilkes_api::workspace::{WorkspaceManager, WorkspaceState, WorkspaceSummary};
use wilkes_core::embed::index::IndexActivity;
use wilkes_core::integrations::custom::ProbeReport;
use wilkes_core::types::{
    AddOutcome, Bookmark, BookmarkClustersQuery, BookmarkClustersResult, ChunkTopicsQuery,
    ChunkTopicsResult, CitationResult, CollectionValidation, DataPaths, DocumentMetadata,
    DocumentTagUpdate, EmbedderCapabilityManifest, EmbeddingEngine, ExternalMcpSettings,
    FileListResponse, HttpApiSettings, IndexStatus, IntegrationStatus, LanSharingSettings,
    LiteratureSearchResult, NewBookmark, NewSavedWork, NewSmartCollection, NewTag, RootCoverage,
    SavedWork, SearchLogEntry, SelectedEmbedder, Settings, SmartCollection, Tag,
    UpdateSmartCollection, UpdateTag,
};
use wilkes_core::worker::manager::WorkerStatus;

mod agent_browser;
mod browser_find;
mod browser_hover;
mod browser_pane;
mod browser_script;
#[cfg(not(target_os = "macos"))]
mod browser_status;
#[cfg(target_os = "macos")]
mod browser_webkit_download;
mod chat;
mod cookie_import;
mod lan_share;
mod platform;
#[cfg(target_os = "macos")]
mod shell_path;
mod window_controls;

use platform::{
    build_startup_plan, validate_open_target, validate_reveal_target, DesktopPlatform,
    DesktopStartupPlan, SystemDesktopPlatform, TauriPlatform,
};

mod commands;
mod native_open;

use commands::*;
use native_open::{NativeOpenRequest, NativeOpenState};

struct GlobalSettingsPath(PathBuf);

#[derive(Default)]
struct DesktopStartupState(std::sync::RwLock<StartupStatus>);

impl DesktopStartupState {
    fn status(&self) -> StartupStatus {
        self.0.read().unwrap().clone()
    }

    fn replace(&self, status: StartupStatus) {
        *self.0.write().unwrap() = status;
    }
}

#[tauri::command]
fn get_startup_status(app: AppHandle) -> StartupStatus {
    app.state::<Arc<DesktopStartupState>>().status()
}

/// The calling window says it is listening, and takes whatever arrived
/// before it was. Every window that shows external open requests calls this
/// -- the standalone reader and the main window alike -- so the readiness
/// handshake is one mechanism keyed by which window is asking.
#[tauri::command]
fn native_open_ready(window: tauri::WebviewWindow, app: AppHandle) -> Vec<NativeOpenRequest> {
    app.state::<NativeOpenState>()
        .mark_ready_and_drain(window.label())
}

/// Paint the main window in the saved appearance before the page has loaded.
/// The webview is white until its first paint, which flashes in dark mode; the
/// page's own `<style>` covers the span after that, and the settings store
/// remains the only thing that decides the theme once the UI is running -- it
/// repaints this background (which the title bar shows) on every change.
fn paint_window_background(app: &tauri::App, settings_path: &std::path::Path) {
    let Some(window) = app.get_webview_window("main") else {
        error!("window background: no main window to paint");
        return;
    };
    let settings = match tauri::async_runtime::block_on(
        wilkes_api::commands::settings::get_settings(settings_path),
    ) {
        Ok(settings) => settings,
        Err(error) => {
            error!("window background: could not read the saved theme: {error:#}");
            return;
        }
    };
    let dark = match settings.theme {
        wilkes_core::types::Theme::Dark => true,
        wilkes_core::types::Theme::Light => false,
        wilkes_core::types::Theme::System => {
            matches!(window.theme(), Ok(tauri::Theme::Dark))
        }
    };
    // `--bg-app` in ui/src/styles.css, for each palette.
    let color = if dark {
        tauri::window::Color(10, 10, 10, 255)
    } else {
        tauri::window::Color(255, 255, 255, 255)
    };
    if let Err(error) = window.set_background_color(Some(color)) {
        error!("window background: could not set it: {error:#}");
    }
}

/// The single registration point for feature preflights. A future breaking
/// feature adds its provider here and the shell/UI need no corresponding
/// special case.
///
/// No feature contributes a blocker today: the pre-workspace library migration
/// that used to stop startup is performed by the application itself. The gate
/// stays because the shape of the question — is there anything only the user
/// can resolve before this build may open their data — outlives the one
/// feature that first had to ask it, and an unexpected startup failure is
/// still reported through it.
fn collect_startup_status(
    _data_dir: &std::path::Path,
    _settings_path: &std::path::Path,
) -> anyhow::Result<StartupStatus> {
    Ok(StartupStatus::ready())
}

/// The context the view labelled `view` works in: the workspace that window
/// shows, or the default workspace for a window that shows none (the
/// standalone reader).
pub(crate) fn view_context(app: &AppHandle, view: &str) -> Arc<AppContext> {
    workspace_manager(app).view_context(view)
}

/// The context a command works in: the workspace of the window it came from.
pub(crate) fn window_context(window: &tauri::WebviewWindow) -> Arc<AppContext> {
    view_context(window.app_handle(), window.label())
}

pub(crate) fn workspace_manager(app: &AppHandle) -> Arc<WorkspaceManager> {
    app.state::<Arc<WorkspaceManager>>().inner().clone()
}

/// The browser MCP tools fetch and read pages through.
pub(crate) fn agent_browser(app: &AppHandle) -> Arc<dyn wilkes_agent::browser::AgentBrowser> {
    app.state::<Arc<dyn wilkes_agent::browser::AgentBrowser>>()
        .inner()
        .clone()
}

// ── External MCP lifecycle ────────────────────────────────────────────────────

// ── HTTP API lifecycle ────────────────────────────────────────────────────────

/// What every window opened on a workspace after the main one is labelled
/// with, followed by an id. `capabilities/default.json` grants its
/// permissions to `workspace-*`: a window whose label it does not match gets
/// none, and its interface cannot even listen for events.
const WORKSPACE_WINDOW_LABEL_PREFIX: &str = "workspace-";

/// The native menu's File → New Window.
const NEW_WINDOW_MENU_ID: &str = "new-window";
/// What File → New Window asks the window in front to do: let the user pick
/// the workspace the new window shows.
const NEW_WINDOW_EVENT: &str = "menu-new-window";

/// The platform's default menu, with File → New Window added.
///
/// macOS and Windows only: Tauri's default menu has no File menu elsewhere,
/// and there the workspace picker's own button is the way to a new window.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn install_menu(app: &AppHandle) -> anyhow::Result<()> {
    use tauri::menu::{Menu, MenuItem, MenuItemKind};
    let menu = Menu::default(app)?;
    let file = menu
        .items()?
        .into_iter()
        .find_map(|item| match item {
            MenuItemKind::Submenu(submenu) if submenu.text().is_ok_and(|text| text == "File") => {
                Some(submenu)
            }
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("the default menu has no File menu"))?;
    let new_window = MenuItem::with_id(
        app,
        NEW_WINDOW_MENU_ID,
        "New Window…",
        true,
        Some("CmdOrCtrl+Shift+N"),
    )?;
    file.prepend(&new_window)?;
    app.set_menu(menu)?;
    Ok(())
}

/// Sends File → New Window to the workspace window in front, or — when the
/// standalone reader is in front — to the window showing the default
/// workspace, since the reader has no workspace to choose from.
fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    if event.id() != NEW_WINDOW_MENU_ID {
        return;
    }
    let Some(manager) = app.try_state::<Arc<WorkspaceManager>>() else {
        return;
    };
    let focused = app
        .webview_windows()
        .into_values()
        .find(|window| matches!(window.is_focused(), Ok(true)))
        .map(|window| window.label().to_string())
        .filter(|label| manager.view_workspace_id(label).is_some());
    let target = match focused {
        Some(label) => Some(label),
        None => match manager.active_workspace_id() {
            Ok(id) => manager.view_showing(&id),
            Err(error) => {
                error!("New Window: could not read the default workspace: {error:#}");
                None
            }
        },
    };
    let Some(target) = target else {
        error!("New Window was chosen with no workspace window to ask");
        return;
    };
    if let Err(error) = Emitter::emit_to(app, target.as_str(), NEW_WINDOW_EVENT, ()) {
        error!("could not send New Window to window {target}: {error}");
    }
}

/// Keeps the manager's views in step with the windows: the focused window's
/// workspace becomes the default, and a closed window's is let go.
fn track_workspace_window(window: &tauri::Window, event: &tauri::WindowEvent) {
    let app = window.app_handle();
    // Before startup finished there is no manager, and no view to track.
    let Some(manager) = app.try_state::<Arc<WorkspaceManager>>() else {
        return;
    };
    let manager = manager.inner().clone();
    let view = window.label().to_string();
    match event {
        tauri::WindowEvent::Focused(true) => {
            tauri::async_runtime::spawn(async move {
                if let Err(error) = manager.focus_view(&view).await {
                    error!("could not make window {view}'s workspace the default: {error:#}");
                }
            });
        }
        tauri::WindowEvent::Destroyed => {
            active_searches_state(app).cancel_view(&view);
            tauri::async_runtime::spawn(async move {
                if let Err(error) = manager.close_view(&view).await {
                    error!("could not let go of window {view}'s workspace: {error:#}");
                }
            });
        }
        _ => {}
    }
}

fn handle_exit_event(app_handle: &AppHandle, event: tauri::RunEvent) {
    // macOS delivers both kinds through `application:openURLs:` -- files the
    // user opened with Wilkes, and `wilkes://` links clicked elsewhere -- so
    // this is the one runtime entry point for either on this platform.
    #[cfg(target_os = "macos")]
    if let tauri::RunEvent::Opened { urls } = &event {
        native_open::deliver_all(app_handle, native_open::requests_from_urls(urls.clone()));
    }

    if matches!(
        event,
        tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
    ) {
        let Some(workspaces) = app_handle.try_state::<Arc<WorkspaceManager>>() else {
            return;
        };
        let workspaces = workspaces.inner().clone();
        // Kill any in-flight turn and the chat subprocesses themselves before
        // the process tree goes away, rather than leaving orphaned CLIs behind.
        if let Some(chat_manager) = app_handle.try_state::<Arc<ChatManager>>() {
            chat_manager.close_all();
        }
        let external_mcp = app_handle
            .try_state::<Arc<ExternalMcpManager>>()
            .map(|manager| manager.inner().clone());
        // Stopped before the workspace shuts down: the API answers *from* this
        // workspace, so a listener outliving it would serve requests against a
        // context that is already going away.
        let http_api = app_handle
            .try_state::<Arc<HttpApiManager>>()
            .map(|manager| manager.inner().clone());
        // Stopped before the workspace for the same reason: it writes what it
        // receives into the active workspace's root.
        let lan_share = app_handle
            .try_state::<Arc<lan_share::LanShareManager>>()
            .map(|manager| manager.inner().clone());
        tauri::async_runtime::spawn(async move {
            if let Some(external_mcp) = external_mcp {
                external_mcp.stop().await;
            }
            if let Some(http_api) = http_api {
                http_api.stop().await;
            }
            if let Some(lan_share) = lan_share {
                lan_share.stop().await;
            }
            // Every workspace this manager opened, not only the active one:
            // the HTTP API can have opened contexts for others.
            workspaces.shutdown_all().await;
        });
    }
}

// ── Tauri EventEmitter impl ───────────────────────────────────────────────────

pub(crate) struct TauriEmitter(pub(crate) AppHandle);

impl EventEmitter for TauriEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        let platform = TauriPlatform(self.0.clone());
        platform.emit(name, payload);
    }
}

/// Workspace events, to the webview and to any HTTP subscriber.
///
/// `/api/events` streams from a broadcast channel, so serving the API without
/// this fan-out would give HTTP consumers a route that connects and then
/// reports nothing forever — an endpoint that exists on the standalone server
/// and is silently dead here. Chat keeps using [`TauriEmitter`] directly: its
/// events belong to one webview turn, not to the workspace.
struct WorkspaceEmitter {
    webview: TauriEmitter,
    http: broadcast::Sender<(String, serde_json::Value)>,
}

impl EventEmitter for WorkspaceEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        // No subscribers is the normal case (the API is off), and `send`
        // reporting that is not a failure worth logging.
        let _ = self.http.send((name.to_string(), payload.clone()));
        self.webview.emit(name, payload);
    }
}

/// One workspace's event goes to the windows showing it, and to the HTTP
/// stream when it is the default workspace — the one the API answers from.
impl ViewEventSink for WorkspaceEmitter {
    fn emit_to(&self, audience: EventAudience<'_>, name: &str, payload: serde_json::Value) {
        if audience.default {
            let _ = self.http.send((name.to_string(), payload.clone()));
        }
        for view in audience.views {
            if let Err(error) = Emitter::emit_to(&self.webview.0, view.as_str(), name, &payload) {
                error!("could not send {name} to window {view}: {error}");
            }
        }
    }
}

// ── Chat (ACP) state ─────────────────────────────────────────────────────────

// ── Desktop-only commands ─────────────────────────────────────────────────────

// ── Search ────────────────────────────────────────────────────────────────────

// ── Delegating commands ───────────────────────────────────────────────────────

// ── Embed commands (delegating to AppContext) ─────────────────────────────────

// ── Catalogue ────────────────────────────────────────────────────────────────
//
// The mirror of the open learning catalogues. Installation-wide, not
// workspace-owned: what it holds is what four public catalogues publish, which
// is the same answer whichever workspace is open.

// ── Feeds ────────────────────────────────────────────────────────────────────

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run() {
    // Restore the GUI launch environment before logging or Tauri starts threads.
    #[cfg(target_os = "macos")]
    let shell_path_result = shell_path::initialize();

    wilkes_core::logging::init_logging();

    #[cfg(target_os = "macos")]
    if let Err(error) = shell_path_result {
        tracing::warn!(
            "Could not initialize PATH from the login shell; keeping inherited PATH: {error:#}"
        );
    }

    let app = tauri::Builder::default()
        // This must remain the first plugin: on Windows and Linux it owns the
        // handoff from a second OS "Open With" launch to this process.
        .plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
            native_open::deliver_all(
                app,
                native_open::requests_from_args(
                    args.into_iter().map(OsString::from).collect(),
                    std::path::Path::new(&cwd),
                ),
            );
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .on_window_event(track_workspace_window)
        .on_menu_event(handle_menu_event)
        .manage(NativeOpenState::default())
        .manage(browser_pane::DarkFilters::default())
        .manage(browser_pane::Downloads::default())
        .manage(browser_pane::PageSaves::default())
        .manage(agent_browser::AgentState::default())
        .setup(|app| {
            // Before anything titles the main window: a new title is one of
            // the things AppKit lays its traffic lights out again for.
            match app.get_webview_window(native_open::MAIN_WINDOW_LABEL) {
                Some(window) => window_controls::pin(&window),
                None => error!("window controls: no main window to place them in"),
            }
            let startup = Arc::new(DesktopStartupState::default());
            app.manage(Arc::clone(&startup));
            let handle = app.handle().clone();
            let platform = TauriPlatform(handle.clone());
            let DesktopStartupPlan {
                data_dir,
                settings_path,
            } = match build_startup_plan(&platform) {
                Ok(plan) => plan,
                Err(error) => {
                    error!("desktop startup preflight failed: {error:#}");
                    startup.replace(StartupStatus::unexpected(error));
                    return Ok(());
                }
            };
            app.manage(GlobalSettingsPath(settings_path.clone()));
            paint_window_background(app, &settings_path);

            let status = match collect_startup_status(&data_dir, &settings_path) {
                Ok(status) => status,
                Err(error) => {
                    error!("workspace startup preflight failed: {error:#}");
                    startup.replace(StartupStatus::unexpected(error));
                    return Ok(());
                }
            };
            if !status.is_ready() {
                startup.replace(status);
                return Ok(());
            }

            let (events_tx, _) = broadcast::channel::<(String, serde_json::Value)>(1024);
            let emitter: Arc<dyn ViewEventSink> = Arc::new(WorkspaceEmitter {
                webview: TauriEmitter(handle.clone()),
                http: events_tx.clone(),
            });
            let agent_browser: Arc<dyn wilkes_agent::browser::AgentBrowser> = Arc::new(
                agent_browser::DesktopAgentBrowser::new(app.handle().clone()),
            );
            app.manage(Arc::clone(&agent_browser));
            let external_mcp = Arc::new(ExternalMcpManager::new(
                data_dir.clone(),
                Some(agent_browser),
            ));
            let lan_share_data_dir = data_dir.clone();
            let (workspaces, event_rx, loop_fut) =
                match WorkspaceManager::new(data_dir, settings_path, Arc::clone(&emitter)) {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        error!("desktop startup initialization failed: {error:#}");
                        startup.replace(
                            match error
                                .downcast_ref::<wilkes_api::installation::InstallationInUse>()
                            {
                                Some(in_use) => StartupStatus::installation_in_use(in_use),
                                None => StartupStatus::unexpected(error),
                            },
                        );
                        return Ok(());
                    }
                };
            let ctx = workspaces.active();

            // Workspace-scoped, not context-scoped: the state resolves the
            // active workspace per request, so a workspace switch is followed
            // by the API without restarting the listener.
            let http_api = Arc::new(HttpApiManager::new(
                Arc::new(wilkes_server::http::state::AppState {
                    ctx: None,
                    workspaces: Some(Arc::clone(&workspaces)),
                    uploads_dir: ctx.data_dir.join("uploads"),
                    events_tx,
                }),
                ctx.shared_data_dir.join(HTTP_API_TOKEN_FILENAME),
            ));

            app.manage(Arc::clone(&workspaces));
            app.manage(Arc::new(ActiveSearches(Mutex::new(HashMap::new()))));
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            if let Err(error) = install_menu(&handle) {
                error!("could not add New Window to the menu: {error:#}");
            }
            let title_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) =
                    title_window_for_view(&title_handle, wilkes_api::workspace::PRIMARY_VIEW).await
                {
                    error!("could not title the main window: {error}");
                }
            });
            // Saved chats are the installation's, not a workspace's: one
            // store beside the workspaces, gathered here from the per-workspace
            // stores earlier builds kept. Then conversations written before
            // the chat moved into `wilkes-chat`, which are in a shape that
            // crate does not read, are brought into it. Both before anything
            // opens the store -- and a failure is logged rather than fatal,
            // because a chat history that cannot be migrated is not a reason
            // to refuse to start.
            let chat_store_dir = ctx.shared_data_dir.clone();
            if let Err(e) =
                wilkes_api::commands::chat::gather_workspace_conversations(&chat_store_dir)
            {
                error!("chat: could not gather the workspaces' saved conversations: {e:#}");
            }
            if let Err(e) =
                wilkes_api::commands::chat::migrate_legacy_conversations(&chat_store_dir)
            {
                error!("chat: could not migrate saved conversations: {e:#}");
            }
            app.manage(Arc::new(ChatManager::new(chat_store_dir)));
            let lan_share = Arc::new(lan_share::LanShareManager::new(
                &lan_share_data_dir,
                Arc::clone(&workspaces),
                emitter,
            ));
            app.manage(Arc::clone(&external_mcp));
            app.manage(Arc::clone(&http_api));
            app.manage(Arc::clone(&lan_share));

            let ctx_c = Arc::clone(&ctx);
            tauri::async_runtime::spawn(async move {
                ctx_c.spawn_background_tasks(event_rx, loop_fut);
            });
            let ctx_c = Arc::clone(&ctx);
            let workspaces_for_listeners = Arc::clone(&workspaces);
            tauri::async_runtime::spawn(async move {
                let settings = ctx_c.get_settings().await;
                if let Err(error) = external_mcp
                    .apply(&settings.external_mcp, workspaces_for_listeners)
                    .await
                {
                    error!("external MCP startup failed: {error}");
                }
                if let Err(error) = http_api.apply(&settings.http_api).await {
                    error!("HTTP API startup failed: {error}");
                }
                if let Err(error) = lan_share.apply(&settings.lan_sharing).await {
                    error!("local network sharing startup failed: {error}");
                }
            });

            native_open::deliver_all(
                &handle,
                native_open::requests_from_args(
                    std::env::args_os().collect(),
                    &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                ),
            );

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_startup_status,
            native_open_ready,
            open_workspace_window,
            get_global_settings,
            preview_standalone,
            get_standalone_file_metadata,
            search,
            cancel_search,
            related_documents,
            tidy_folder,
            reading_order,
            citation_links,
            citation_graph,
            fetch_missing_citations,
            preview,
            surrogate_bytes,
            resolve_pdf_font,
            pdf_font_bytes,
            save_pdf_edits,
            comic_page,
            file_cover,
            list_files,
            open_file,
            rename_file,
            import_files,
            read_clipboard_files,
            move_file,
            create_directory,
            list_directories,
            path_kinds,
            trash_file,
            get_file_metadata,
            get_python_envs,
            remove_python_envs,
            remove_custom_model,
            get_settings,
            update_settings,
            configure_external_mcp,
            get_external_mcp_status,
            set_active_document,
            rotate_external_mcp_token,
            rotate_http_api_token,
            configure_remote_embedding,
            get_remote_embedding_status,
            configure_http_api,
            get_http_api_status,
            configure_lan_sharing,
            get_lan_sharing_status,
            list_lan_peers,
            send_files_to_lan_peer,
            list_bookmarks,
            add_bookmark,
            remove_bookmark,
            update_bookmark_note,
            list_saved_works,
            save_work,
            remove_saved_work,
            update_saved_work_note,
            mark_saved_work_acquired,
            cluster_bookmarks,
            chunk_topics,
            cancel_chunk_topics,
            list_tags,
            create_tag,
            update_tag,
            delete_tag,
            update_document_tags,
            list_smart_collections,
            create_smart_collection,
            update_smart_collection,
            delete_smart_collection,
            validate_smart_collection,
            list_search_log,
            delete_search_log,
            clear_search_log,
            zotero_status,
            resolve_file_metadata,
            refresh_file_metadata,
            zotero_add_item,
            zotero_generate_citation,
            custom_integration_summary,
            custom_integration_authoring_prompt,
            custom_integration_probe,
            pick_directory,
            download_model,
            build_index,
            index_activity,
            index_coverage,
            continue_index_job,
            retry_failed_documents,
            index_missing_documents,
            embedder_capabilities,
            get_model_size,
            search_custom_models,
            cancel_embed,
            get_index_status,
            delete_index,
            get_http_calls,
            clear_http_calls,
            get_logs,
            clear_logs,
            get_data_paths,
            list_workspaces,
            create_workspace,
            rename_workspace,
            switch_workspace,
            delete_workspace,
            open_path,
            reveal_path,
            window_controls::window_controls,
            browser_pane::browser_pane_show,
            browser_pane::browser_pane_set_bounds,
            browser_pane::browser_pane_set_visible,
            browser_pane::browser_pane_set_dark_filter,
            browser_pane::browser_pane_step,
            browser_pane::browser_pane_clear_data,
            browser_pane::browser_pane_save_page,
            browser_pane::browser_pane_close,
            browser_find::browser_pane_find,
            browser_find::browser_pane_end_find,
            cookie_import::browser_cookie_sources,
            cookie_import::browser_pane_import_cookies,
            is_semantic_ready,
            is_generation_ready,
            list_generation_models,
            get_generation_model_size,
            load_generation_model,
            literature_providers,
            literature_search,
            literature_acquire,
            literature_browse,
            feeds_list,
            feed_discover,
            feed_add,
            feed_update,
            feed_remove,
            feeds_refresh,
            feeds_refresh_cancel,
            feed_items,
            feed_item_set_status,
            literature_filter_suggest,
            literature_compose,
            literature_compose_cancel,
            literature_provider_status,
            literature_provider_lookup,
            catalogue_sync,
            catalogue_remove,
            catalogue_acquire_course,
            image_recognizer_catalogue,
            image_recognizer_inventory,
            install_image_recognizer,
            install_layout_detector,
            explain_related_document,
            summarize_document,
            summarize_search_results,
            request_completion,
            cancel_completion,
            completion_feedback,
            get_session_steering,
            reset_session_steering,
            save_document,
            get_worker_status,
            get_worker_statuses,
            kill_worker,
            set_worker_timeout,
            chat::chat_list_backends,
            chat::chat_install_backend,
            chat::chat_list_conversations,
            chat::chat_forget_conversation,
            chat::chat_start,
            chat::chat_open_conversation,
            chat::chat_fork_conversation,
            chat::chat_set_config_option,
            chat::chat_send,
            chat::chat_cancel,
            chat::chat_answer_permission,
            chat::chat_log_in,
            chat::chat_cancel_log_in,
            chat::chat_log_out,
            chat::chat_close,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(handle_exit_event);
}

#[cfg(test)]
mod tests;
