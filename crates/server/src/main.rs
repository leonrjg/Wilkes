//! The headless Wilkes server: owns a workspace, serves the API over it, and
//! serves the web UI's static assets in front of it.
//!
//! Everything except the assets and the process lifecycle lives in the library
//! (`wilkes_server`), because the desktop app mounts the same routes over the
//! workspace it already owns.

use std::sync::Arc;

use tokio::sync::broadcast;
use tower_http::services::{ServeDir, ServeFile};
use tracing::info;
use wilkes_api::workspace::WorkspaceManager;
use wilkes_server::config::parse_config;
use wilkes_server::http::state::{AppState, BroadcastEmitter};

/// Stops on Ctrl-C or SIGTERM, shutting the workspace down before the process
/// goes away. The desktop app has its own exit path, so this stays here.
async fn shutdown_signal(workspaces: Arc<WorkspaceManager>) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};

        if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
            sigterm.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    workspaces.active().shutdown().await;
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    wilkes_core::logging::init_logging();
    let config = parse_config();

    let settings_path = config.data_dir.join("settings.json");

    let (events_tx, _) = broadcast::channel::<(String, serde_json::Value)>(1024);
    let emitter: Arc<dyn wilkes_api::context::EventEmitter> = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (workspaces, event_rx, loop_fut) =
        WorkspaceManager::new(config.data_dir.clone(), settings_path, emitter)?;
    let ctx = workspaces.active();
    let uploads_dir = ctx.data_dir.join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await?;

    ctx.clone().spawn_background_tasks(event_rx, loop_fut);

    let state = Arc::new(AppState {
        ctx: None,
        workspaces: Some(Arc::clone(&workspaces)),
        uploads_dir,
        events_tx,
    });
    let shutdown_workspaces = Arc::clone(&workspaces);
    let index_html = config.dist_dir.join("index.html");

    // Assets sit behind the API rather than inside it: this is the only shell
    // that has to hand out the web UI, and the desktop app must not.
    let app = wilkes_server::api_router(state).fallback_service(
        ServeDir::new(&config.dist_dir).not_found_service(ServeFile::new(index_html)),
    );

    let addr = format!("{}:{}", config.host, config.port);
    info!("wilkes-server listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_workspaces))
        .await?;

    Ok(())
}
