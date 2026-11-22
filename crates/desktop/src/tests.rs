/// The two model downloads are gated on this crate declaring the feature
/// its `#[cfg]`s test. It did not, and both commands compiled to the stub
/// that tells the user the build has no detector — in a build that had
/// one, with the weights already downloadable and the panel already
/// offering them. A cfg on an undeclared feature is not a switch, it is
/// the constant `false`.
#[test]
fn the_default_build_offers_the_model_downloads() {
    if !cfg!(feature = "recognize-onnx") {
        panic!("the desktop build must declare the feature its install commands are gated on");
    }
}

/// The engine picker offers what this build contains. Vision is out of
/// `wilkes-core`'s own default so that a library consumer is not made to
/// build an Objective-C shim — but the application that shows the picker
/// has to contain what the picker shows, and for a while it did not: the
/// entry was there and greyed out on the only platform that has it.
#[test]
#[cfg(target_os = "macos")]
fn a_mac_build_can_actually_recognize_with_vision() {
    use wilkes_core::extract::image::dispatch::{RecognitionEngine, RecognizerCatalogue};
    assert!(
        RecognitionEngine::supported_engines().contains(&RecognitionEngine::Vision),
        "Vision is offered by the picker, so it must be compiled in"
    );
    let catalogue: RecognizerCatalogue =
        wilkes_core::extract::image::recognizer_catalogue(std::path::Path::new("/nonexistent"));
    assert!(
        catalogue
            .models
            .iter()
            .any(|model| model.engine == RecognitionEngine::Vision),
        "and it must appear in the catalogue the panel renders"
    );
}

use super::*;
use tempfile::tempdir;
use wilkes_api::context::EventEmitter;
use wilkes_core::types::SourceOrigin;
use wilkes_core::worker::manager::WorkerPaths;

static OPEN_PATH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct MockEmitter;

impl EventEmitter for MockEmitter {
    fn emit(&self, _name: &str, _payload: serde_json::Value) {}
}

impl ViewEventSink for MockEmitter {
    fn emit_to(&self, _audience: EventAudience<'_>, _name: &str, _payload: serde_json::Value) {}
}

#[test]
fn desktop_startup_state_can_hold_feature_blockers_without_a_runtime() {
    let state = DesktopStartupState::default();
    assert!(state.status().is_ready());
    state.replace(StartupStatus::unexpected("database upgrade required"));
    let status = state.status();
    assert!(!status.is_ready());
    assert_eq!(status.blockers[0].id, "application.startup-failed");
}

/// An alpha install reaches the application instead of the gate: its
/// library is adopted at startup, so there is nothing for the user to do.
#[test]
fn desktop_preflight_does_not_stop_a_pre_workspace_install() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    std::fs::write(
        &settings_path,
        serde_json::to_vec(&serde_json::json!({
            "last_directory": dir.path().join("legacy-library")
        }))
        .unwrap(),
    )
    .unwrap();

    let status = collect_startup_status(dir.path(), &settings_path).unwrap();

    assert!(status.is_ready());
}

#[test]
fn test_validate_open_target() {
    let dir = tempdir().unwrap();
    assert!(validate_open_target(&dir.path().display().to_string()).is_ok());
    assert_eq!(
        validate_open_target(&dir.path().join("missing").display().to_string()),
        Err("Path does not exist".into())
    );
    assert!(validate_open_target("https://doi.org/10.1000/xyz123").is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn test_open_path_uses_spawned_opener() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = OPEN_PATH_LOCK.lock().await;
    let dir = tempdir().unwrap();
    let opener_name = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let opener = dir.path().join(opener_name);
    std::fs::write(&opener, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(&opener).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&opener, perms).unwrap();

    let path = dir.path().join("folder");
    std::fs::create_dir(&path).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", dir.path().display(), original_path);
    std::env::set_var("PATH", &new_path);

    let res = open_path(path.display().to_string()).await;
    std::env::set_var("PATH", original_path);

    assert!(res.is_ok());
}

#[tokio::test]
async fn test_get_model_size_for_ctx_with_injected_fetcher() {
    let result = get_model_size_for_ctx_with(
        EmbeddingEngine::Candle,
        "model-x".to_string(),
        |_engine, model_id| async move {
            assert_eq!(model_id, "model-x");
            Ok(42)
        },
    )
    .await
    .unwrap();

    assert_eq!(result, 42);
}

#[tokio::test]
async fn test_get_model_size_for_ctx_with_error() {
    let err = get_model_size_for_ctx_with(
        EmbeddingEngine::Candle,
        "model-x".to_string(),
        |_engine, _model_id| async move { Err(anyhow::anyhow!("no size")) },
    )
    .await
    .unwrap_err();

    assert!(err.contains("no size"));
}

#[tokio::test]
async fn test_delete_index_for_ctx_removes_db() {
    let (_dir, ctx) = test_ctx();
    let db_path = ctx.data_dir.join("semantic_index.db");
    std::fs::write(&db_path, "fake db").unwrap();

    delete_index_for_ctx(Arc::clone(&ctx), None).await.unwrap();
    assert!(!db_path.exists());
}

#[tokio::test]
async fn test_get_index_status_for_ctx_missing() {
    let (_dir, ctx) = test_ctx();
    let result = get_index_status_for_ctx(ctx, None).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_open_file_for_ctx_allows_outside_data_dir() {
    let (_dir, ctx) = test_ctx();
    let outside_dir = tempdir().unwrap();
    let outside = outside_dir.path().join("outside.txt");
    std::fs::write(&outside, "hello").unwrap();

    let preview = open_file_for_ctx(ctx, outside.display().to_string())
        .await
        .unwrap();
    match preview {
        wilkes_core::types::PreviewData::Text { content, .. } => {
            assert_eq!(content, "hello")
        }
        _ => panic!("Expected text preview"),
    }
}

#[tokio::test]
async fn test_get_file_metadata_for_path_allows_outside_data_dir() {
    let outside_dir = tempdir().unwrap();
    let outside = outside_dir.path().join("outside.txt");
    std::fs::write(&outside, "hello").unwrap();

    let (_dir, ctx) = test_ctx();
    let metadata = get_file_metadata_for_path(ctx, outside.display().to_string())
        .await
        .unwrap();
    assert_eq!(metadata, DocumentMetadata::default());
}

#[tokio::test]
async fn test_refresh_file_metadata_for_ctx_allows_outside_data_dir() {
    let outside_dir = tempdir().unwrap();
    let outside = outside_dir.path().join("outside.txt");
    std::fs::write(&outside, "hello").unwrap();

    let (_data_dir, ctx) = test_ctx();
    refresh_file_metadata_for_ctx(ctx, Some(outside.display().to_string()))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_rename_file_for_path_allows_outside_data_dir() {
    let (_data_dir, ctx) = test_ctx();
    let outside_dir = tempdir().unwrap();
    let outside = outside_dir.path().join("outside.txt");
    std::fs::write(&outside, "hello").unwrap();

    let renamed = rename_file_for_path(ctx, outside.display().to_string(), "renamed.txt".into())
        .await
        .unwrap();

    let renamed_path = outside_dir.path().join("renamed.txt");
    assert_eq!(renamed, renamed_path.display().to_string());
    assert!(!outside.exists());
    assert_eq!(std::fs::read_to_string(renamed_path).unwrap(), "hello");
}

#[tokio::test]
async fn test_path_kinds_classifies_folders_and_files() {
    let dir = tempdir().unwrap();
    let folder = dir.path().join("library");
    std::fs::create_dir(&folder).unwrap();
    let file = dir.path().join("paper.pdf");
    std::fs::write(&file, "pdf").unwrap();

    let kinds = path_kinds(vec![
        file.display().to_string(),
        folder.display().to_string(),
    ])
    .await
    .unwrap();

    assert_eq!(kinds, vec![PathKind::File, PathKind::Directory]);
}

#[tokio::test]
async fn test_path_kinds_reports_a_missing_path() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("gone.pdf");

    let error = path_kinds(vec![missing.display().to_string()])
        .await
        .unwrap_err();

    assert!(error.contains("Could not inspect"), "{error}");
}

#[tokio::test]
async fn test_import_files_into_current_root_for_ctx_moves_external_file() {
    let (_dir, ctx) = test_ctx();
    let source_dir = tempdir().unwrap();
    let root_dir = tempdir().unwrap();
    let source = source_dir.path().join("paper.pdf");
    std::fs::write(&source, "pdf").unwrap();

    ctx.update_settings(serde_json::json!({
        "last_directory": root_dir.path().display().to_string(),
        "supported_extensions": ["pdf"]
    }))
    .await
    .unwrap();

    let imported = import_files_into_current_root_for_ctx(
        ctx,
        vec![source.display().to_string()],
        root_dir.path().display().to_string(),
        None,
        wilkes_api::commands::files::FileImportMode::Move,
    )
    .await
    .unwrap();

    let target = root_dir.path().join("paper.pdf");
    assert_eq!(
        imported,
        vec![target.canonicalize().unwrap().display().to_string()]
    );
    assert!(!source.exists());
    assert_eq!(std::fs::read_to_string(target).unwrap(), "pdf");
}

#[tokio::test]
async fn test_import_files_into_current_root_for_ctx_copies_external_file() {
    let (_dir, ctx) = test_ctx();
    let source_dir = tempdir().unwrap();
    let root_dir = tempdir().unwrap();
    let source = source_dir.path().join("paper.pdf");
    std::fs::write(&source, "pdf").unwrap();

    ctx.update_settings(serde_json::json!({
        "last_directory": root_dir.path().display().to_string(),
        "supported_extensions": ["pdf"]
    }))
    .await
    .unwrap();

    let imported = import_files_into_current_root_for_ctx(
        ctx,
        vec![source.display().to_string()],
        root_dir.path().display().to_string(),
        None,
        wilkes_api::commands::files::FileImportMode::Copy,
    )
    .await
    .unwrap();

    let target = root_dir.path().join("paper.pdf");
    assert_eq!(
        imported,
        vec![target.canonicalize().unwrap().display().to_string()]
    );
    assert_eq!(std::fs::read_to_string(source).unwrap(), "pdf");
    assert_eq!(std::fs::read_to_string(target).unwrap(), "pdf");
}

#[tokio::test]
/// The activity view answers for a directory nobody has indexed rather
/// than failing: a workspace's first look at it is the common case.
async fn test_index_activity_for_ctx_reports_an_unindexed_root() {
    let (dir, ctx) = test_ctx();
    let activity = index_activity_for_ctx(ctx, dir.path().to_string_lossy().into_owned())
        .await
        .unwrap();
    assert!(activity.job.is_none());
    assert!(activity.documents.is_empty());
}

#[tokio::test]
/// Neither action starts a build for a directory with no recorded job.
async fn test_continue_and_retry_for_ctx_refuse_without_a_job() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().to_string_lossy().into_owned();
    let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);

    let err = continue_index_job_for_ctx(Arc::clone(&ctx), root.clone(), selected.clone())
        .await
        .unwrap_err();
    assert!(err.contains("No indexing job has been recorded"), "{err}");

    let err = retry_failed_documents_for_ctx(ctx, root, selected)
        .await
        .unwrap_err();
    assert!(err.contains("No indexing job has been recorded"), "{err}");
}

#[tokio::test]
async fn test_build_index_for_ctx_missing_root() {
    let (_dir, ctx) = test_ctx();
    let err = build_index_for_ctx(
        ctx,
        "/definitely/missing/root".to_string(),
        SelectedEmbedder::default_for(EmbeddingEngine::Candle),
    )
    .await
    .unwrap_err();
    assert!(err.contains("Index root not found"));
}

#[tokio::test]
async fn test_get_worker_status_for_ctx_and_timeout_update() {
    let dir = tempdir().unwrap();
    use std::path::PathBuf;

    let emitter = Arc::new(MockEmitter);
    let (ctx, _rx, loop_fut) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        },
        emitter,
    );
    let _loop_handle = tokio::spawn(loop_fut);

    let status = get_worker_status_for_ctx(Arc::clone(&ctx));
    assert!(!status.active);
    assert_eq!(status.timeout_secs, 300);

    set_worker_timeout_for_ctx(Arc::clone(&ctx), 123)
        .await
        .unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let status = get_worker_status_for_ctx(ctx);
    assert_eq!(status.timeout_secs, 123);
}

#[tokio::test]
async fn capabilities_cover_every_supported_engine() {
    let (_dir, ctx) = test_ctx();
    let manifest = embedder_capabilities_for_ctx(ctx).await.unwrap();
    assert_eq!(manifest.engines, EmbeddingEngine::supported_engines());
    assert!(!manifest.models.is_empty());
    // The picker reads its default from here now, so the manifest has to
    // carry one rather than leaving the UI to guess at a name.
    assert!(manifest.models.iter().any(|model| model.is_default));
}

#[tokio::test]
async fn test_cancel_and_kill_helpers() {
    let (_dir, ctx) = test_ctx();
    cancel_embed_for_ctx(Arc::clone(&ctx)).await.unwrap();
    kill_worker_for_ctx(ctx).await.unwrap();
}

#[tokio::test]
async fn test_set_worker_timeout_for_ctx_errors_without_loop() {
    let (_dir, ctx) = test_ctx();
    let err = set_worker_timeout_for_ctx(ctx, 12).await.unwrap_err();
    assert!(!err.is_empty());
}

#[tokio::test]
async fn test_cancel_search_for_ctx_removes_handle() {
    let active = Arc::new(ActiveSearches(Mutex::new(HashMap::new())));
    active.0.lock().unwrap().insert(
        "search-1".to_string(),
        (
            "main".to_string(),
            tokio::spawn(async {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            }),
        ),
    );

    cancel_search_for_ctx(Arc::clone(&active), "search-1");

    assert!(active.0.lock().unwrap().is_empty());
}

/// Every window the app opens is one the capability grants its
/// permissions to. A workspace window it missed came up unable to listen
/// for any event or set its own title.
#[test]
fn the_default_capability_covers_every_window_label() {
    let capability: serde_json::Value =
        serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
    let windows: Vec<&str> = capability["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|label| label.as_str().unwrap())
        .collect();
    assert!(windows.contains(&native_open::MAIN_WINDOW_LABEL));
    assert!(windows.contains(&native_open::DOCUMENT_WINDOW_LABEL));
    assert!(windows.contains(&format!("{WORKSPACE_WINDOW_LABEL_PREFIX}*").as_str()));
}

/// A window leaving its workspace stops its own searches, not another
/// window's.
#[tokio::test]
async fn cancelling_a_view_leaves_other_windows_searches_running() {
    let active = ActiveSearches(Mutex::new(HashMap::new()));
    let sleeper = || {
        tokio::spawn(async {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        })
    };
    active
        .0
        .lock()
        .unwrap()
        .insert("a".to_string(), ("main".to_string(), sleeper()));
    active
        .0
        .lock()
        .unwrap()
        .insert("b".to_string(), ("workspace-2".to_string(), sleeper()));

    active.cancel_view("main");

    let remaining = active.0.lock().unwrap();
    assert_eq!(remaining.keys().collect::<Vec<_>>(), vec!["b"]);
    assert!(!remaining["b"].1.is_finished());
}

/// A workspace manager over a temporary data directory, for the listener
/// tests: the external MCP resolves its library through one of these
/// rather than through a single context.
fn test_workspaces() -> (tempfile::TempDir, Arc<WorkspaceManager>) {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let emitter: Arc<dyn ViewEventSink> = Arc::new(MockEmitter);
    let (workspaces, _rx, _loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings_path, emitter).unwrap();
    (dir, workspaces)
}

fn test_ctx() -> (tempfile::TempDir, Arc<AppContext>) {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let emitter = Arc::new(MockEmitter);
    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: std::path::PathBuf::from("pkg"),
        requirements_path: std::path::PathBuf::from("reqs.txt"),
        uv_bin: std::path::PathBuf::from("venv"),
        worker_bin: std::path::PathBuf::from("worker"),
        data_dir: dir.path().to_path_buf(),
    };
    let (ctx, _rx, _loop) =
        AppContext::new(dir.path().to_path_buf(), settings_path, paths, emitter);
    (dir, ctx)
}

#[tokio::test]
async fn test_get_settings_for_ctx() {
    let (_dir, ctx) = test_ctx();
    let settings = get_settings_for_ctx(ctx).await.unwrap();
    assert!(settings.favorites.is_empty());
    assert!(settings.last_directory.is_none());
    assert!(!settings.semantic.enabled);
}

#[tokio::test]
async fn test_bookmark_helpers_for_ctx() {
    let (_dir, ctx) = test_ctx();
    let bookmark = add_bookmark_for_ctx(
        Arc::clone(&ctx),
        NewBookmark {
            path: "/tmp/example.pdf".into(),
            origin: SourceOrigin::PdfPage {
                page: 4,
                bbox: None,
            },
            text_range: None,
            quote: "quote".to_string(),
            note: None,
            rects: Vec::new(),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        list_bookmarks_for_ctx(Arc::clone(&ctx))
            .await
            .unwrap()
            .len(),
        1
    );

    let noted = update_bookmark_note_for_ctx(
        Arc::clone(&ctx),
        bookmark.id.clone(),
        Some("a note".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(noted.note.as_deref(), Some("a note"));

    remove_bookmark_for_ctx(ctx.clone(), bookmark.id)
        .await
        .unwrap();
    assert!(list_bookmarks_for_ctx(ctx).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_update_settings_for_ctx() {
    let (_dir, ctx) = test_ctx();
    let updated = update_settings_for_ctx(
        Arc::clone(&ctx),
        serde_json::json!({
            "semantic": {
                "enabled": true
            }
        }),
    )
    .await
    .unwrap();
    assert!(updated.semantic.enabled);
}

/// A manager over a context that owns a workspace, as the app has at
/// startup.
fn http_api_manager(ctx: Arc<AppContext>) -> HttpApiManager {
    let (events_tx, _) = broadcast::channel(16);
    HttpApiManager::new(
        Arc::new(wilkes_server::http::state::AppState {
            ctx: Some(Arc::clone(&ctx)),
            workspaces: None,
            uploads_dir: ctx.data_dir.join("uploads"),
            events_tx,
        }),
        ctx.shared_data_dir.join(HTTP_API_TOKEN_FILENAME),
    )
}

#[tokio::test]
async fn http_api_serves_the_workspace_this_process_already_owns() {
    let (_dir, ctx) = test_ctx();
    let manager = http_api_manager(ctx);

    // Off is the default, and off means nothing is listening.
    let settings = HttpApiSettings::default();
    assert!(!settings.enabled);
    manager.apply(&settings).await.unwrap();
    assert!(!manager.status(&settings).await.running);

    // A real port, the way the other listener's tests pick one: port 0 is
    // rejected on purpose, since a setting the user cannot find the app on
    // is not a valid setting.
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let enabled = HttpApiSettings {
        enabled: true,
        port,
        ..HttpApiSettings::default()
    };
    manager.apply(&enabled).await.unwrap();
    let status = manager.status(&enabled).await;
    assert!(status.running);
    assert!(status.error.is_none());
    let url = status.url.expect("a running listener reports its url");

    // The point of the whole exercise: a consumer reaches this workspace
    // over HTTP instead of opening its databases as a second owner.
    let health = reqwest_get(&format!("{url}/health")).await;
    assert_eq!(health, "200");

    manager.stop().await;
    assert!(!manager.status(&enabled).await.running);
}

/// The API is open until a token is asked for. Asked for, it makes one,
/// keeps it, refuses requests without it and can replace it — all on the
/// listener already running — and turned off again it is open again.
#[tokio::test]
async fn http_api_requires_a_token_only_when_asked_and_can_rotate_it() {
    let (_dir, ctx) = test_ctx();
    let token_path = ctx.shared_data_dir.join(HTTP_API_TOKEN_FILENAME);
    let manager = http_api_manager(ctx);
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let url = format!("http://127.0.0.1:{port}");

    let open = HttpApiSettings {
        enabled: true,
        port,
        ..HttpApiSettings::default()
    };
    manager.apply(&open).await.unwrap();
    let status = manager.status(&open).await;
    assert!(status.running && !status.require_token);
    assert_eq!(status.token, None);
    assert_eq!(reqwest_get(&format!("{url}/api/settings")).await, "200");
    assert!(manager.rotate_token(&open).await.is_err());
    assert!(
        !token_path.exists(),
        "no token is made until one is asked for"
    );

    let guarded = HttpApiSettings {
        require_token: true,
        ..open.clone()
    };
    manager.apply(&guarded).await.unwrap();
    let status = manager.status(&guarded).await;
    assert!(status.running && status.require_token);
    let token = status.token.expect("a token once required");
    assert_eq!(wilkes_core::token_file::read(&token_path).unwrap(), token);
    assert_eq!(reqwest_get(&format!("{url}/api/settings")).await, "401");
    assert_eq!(reqwest_get(&format!("{url}/health")).await, "200");

    manager.rotate_token(&guarded).await.unwrap();
    let rotated = manager.status(&guarded).await.token.unwrap();
    assert_ne!(rotated, token);
    assert_eq!(wilkes_core::token_file::read(&token_path).unwrap(), rotated);

    manager.apply(&open).await.unwrap();
    assert_eq!(manager.status(&open).await.token, None);
    assert_eq!(reqwest_get(&format!("{url}/api/settings")).await, "200");
    manager.stop().await;
}

#[tokio::test]
async fn a_port_already_taken_is_reported_and_leaves_nothing_listening() {
    let (_dir, ctx) = test_ctx();
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = occupied.local_addr().unwrap().port();

    let manager = http_api_manager(ctx);
    let settings = HttpApiSettings {
        enabled: true,
        port,
        ..HttpApiSettings::default()
    };
    let error = manager.apply(&settings).await.unwrap_err();
    assert!(
        error.contains("Could not start the Wilkes HTTP API"),
        "{error}"
    );

    // Reported, and reported where the settings screen reads it — not
    // logged and forgotten.
    let status = manager.status(&settings).await;
    assert!(!status.running);
    assert_eq!(status.error.as_deref(), Some(error.as_str()));
}

/// Minimal HTTP/1.1 GET returning the status code, so the desktop crate
/// does not gain an HTTP client dependency for one assertion.
async fn reqwest_get(url: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.strip_prefix("http://").expect("http url");
    let (authority, path) = rest.split_once('/').expect("url has a path");
    let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();
    stream
        .write_all(
            format!("GET /{path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
        .split_whitespace()
        .nth(1)
        .expect("status line")
        .to_string()
}

#[tokio::test]
async fn external_mcp_manager_starts_and_stops_with_setting() {
    let (_dir, workspaces) = test_workspaces();
    let library = tempdir().unwrap();
    workspaces
        .active()
        .update_settings(serde_json::json!({
            "last_directory": library.path()
        }))
        .await
        .unwrap();
    let token_dir = tempdir().unwrap();
    let manager = ExternalMcpManager::new(token_dir.path().to_path_buf(), None);
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let enabled = ExternalMcpSettings {
        enabled: true,
        require_token: false,
        bind_address: "127.0.0.1".parse().unwrap(),
        port,
    };

    manager
        .apply(&enabled, Arc::clone(&workspaces))
        .await
        .unwrap();
    let status = manager.status(&enabled).await;
    assert!(status.enabled);
    assert!(status.running);
    let expected_url = format!("http://127.0.0.1:{port}/mcp");
    assert_eq!(status.url.as_deref(), Some(expected_url.as_str()));
    assert!(!status.require_token);
    assert!(status.token.is_none());

    let disabled = ExternalMcpSettings {
        enabled: false,
        require_token: false,
        bind_address: "127.0.0.1".parse().unwrap(),
        port,
    };
    manager.apply(&disabled, workspaces).await.unwrap();
    let status = manager.status(&disabled).await;
    assert!(!status.running);
    assert!(status.token.is_none());
}

#[tokio::test]
async fn external_mcp_manager_toggles_token_authentication_live() {
    let (_dir, workspaces) = test_workspaces();
    let token_dir = tempdir().unwrap();
    let manager = ExternalMcpManager::new(token_dir.path().to_path_buf(), None);
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let open = ExternalMcpSettings {
        enabled: true,
        require_token: false,
        bind_address: "127.0.0.1".parse().unwrap(),
        port,
    };
    manager.apply(&open, Arc::clone(&workspaces)).await.unwrap();
    let url = manager.status(&open).await.url.unwrap();
    assert!(manager.status(&open).await.token.is_none());
    assert!(manager.rotate_token(&open).await.is_err());

    let authenticated = ExternalMcpSettings {
        require_token: true,
        ..open.clone()
    };
    manager
        .apply(&authenticated, Arc::clone(&workspaces))
        .await
        .unwrap();
    let status = manager.status(&authenticated).await;
    assert!(status.token.is_some());
    assert_eq!(status.url.as_deref(), Some(url.as_str()));

    manager.apply(&open, Arc::clone(&workspaces)).await.unwrap();
    let status = manager.status(&open).await;
    assert_eq!(status.url.as_deref(), Some(url.as_str()));
    assert!(status.token.is_none());
    assert!(manager.token_path.exists());

    manager
        .apply(
            &ExternalMcpSettings {
                enabled: false,
                ..open
            },
            workspaces,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn external_mcp_manager_reports_port_collision_without_fallback() {
    let (_dir, workspaces) = test_workspaces();
    let token_dir = tempdir().unwrap();
    let manager = ExternalMcpManager::new(token_dir.path().to_path_buf(), None);
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = occupied.local_addr().unwrap().port();
    let enabled = ExternalMcpSettings {
        enabled: true,
        require_token: false,
        bind_address: "127.0.0.1".parse().unwrap(),
        port,
    };

    let error = manager.apply(&enabled, workspaces).await.unwrap_err();
    assert!(error.contains(&port.to_string()));
    let status = manager.status(&enabled).await;
    assert!(!status.running);
    assert!(status.error.is_some());
}

#[tokio::test]
async fn external_mcp_manager_switches_bind_address_on_the_same_port() {
    let (_dir, workspaces) = test_workspaces();
    let token_dir = tempdir().unwrap();
    let manager = ExternalMcpManager::new(token_dir.path().to_path_buf(), None);
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let loopback = ExternalMcpSettings {
        enabled: true,
        require_token: false,
        bind_address: "127.0.0.1".parse().unwrap(),
        port,
    };
    manager
        .apply(&loopback, Arc::clone(&workspaces))
        .await
        .unwrap();

    let all_interfaces = ExternalMcpSettings {
        enabled: true,
        require_token: false,
        bind_address: "0.0.0.0".parse().unwrap(),
        port,
    };
    manager
        .apply(&all_interfaces, Arc::clone(&workspaces))
        .await
        .unwrap();
    let status = manager.status(&all_interfaces).await;
    let expected_url = format!("http://0.0.0.0:{port}/mcp");
    assert!(status.running);
    assert_eq!(status.bind_address, all_interfaces.bind_address);
    assert_eq!(status.url.as_deref(), Some(expected_url.as_str()));

    manager
        .apply(
            &ExternalMcpSettings {
                enabled: false,
                ..all_interfaces
            },
            workspaces,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn test_list_files_for_ctx() {
    let (_dir, ctx) = test_ctx();
    let dir = tempdir().unwrap();
    let path = dir.path().join("example.txt");
    std::fs::write(&path, "hello").unwrap();
    let files = list_files_for_ctx(
        Arc::clone(&ctx),
        dir.path().display().to_string(),
        None,
        vec![],
        None,
    )
    .await
    .unwrap();
    assert!(!files.files.is_empty());

    let tag = ctx
        .create_tag(NewTag {
            name: "Reviewed".into(),
            color: None,
        })
        .unwrap();
    ctx.update_document_tags(DocumentTagUpdate {
        paths: vec![path],
        add_tag_ids: vec![tag.id.clone()],
        remove_tag_ids: vec![],
    })
    .unwrap();
    let tagged = list_files_for_ctx(
        Arc::clone(&ctx),
        dir.path().display().to_string(),
        None,
        vec![tag.id],
        None,
    )
    .await
    .unwrap();
    assert_eq!(tagged.files.len(), 1);
    let unmatched = list_files_for_ctx(
        Arc::clone(&ctx),
        dir.path().display().to_string(),
        None,
        vec!["missing-tag".into()],
        None,
    )
    .await
    .unwrap();
    assert!(unmatched.files.is_empty());

    let preview = list_files_for_ctx(
        ctx,
        dir.path().display().to_string(),
        None,
        vec![],
        Some("extension == 'txt'".into()),
    )
    .await
    .unwrap();
    assert_eq!(preview.files.len(), 1);
}

#[tokio::test]
async fn test_trash_file_rejects_missing_and_non_file_paths() {
    let dir = tempdir().unwrap();
    let directory_error = trash_file_for_path(dir.path().display().to_string())
        .await
        .unwrap_err();
    assert!(directory_error.contains("non-file path"));

    let missing_error = trash_file_for_path(dir.path().join("missing.pdf").display().to_string())
        .await
        .unwrap_err();
    assert!(missing_error.contains("Cannot trash"));
}

#[tokio::test]
async fn test_is_semantic_ready_for_ctx() {
    let (_dir, ctx) = test_ctx();
    assert!(!is_semantic_ready_for_ctx(ctx));
}

#[tokio::test]
async fn test_cancel_embed_for_ctx() {
    let (_dir, ctx) = test_ctx();
    assert!(super::cancel_embed_for_ctx(ctx).await.is_ok());
}

#[tokio::test]
async fn test_data_paths_and_logs_for_ctx() {
    let (_dir, _ctx) = test_ctx();

    let paths = data_paths_from(
        "test-data".to_string(),
        "test-data/workspaces/w1".to_string(),
    );
    assert_eq!(paths.app_data, "test-data");
    // The workspace path is its own answer, not the installation's: the
    // Data page names both, and naming one twice is what it used to do.
    assert_eq!(paths.workspace, "test-data/workspaces/w1");

    super::clear_logs().await.unwrap();
    let logs = super::get_logs().await.unwrap();
    assert!(logs.is_empty());
}
