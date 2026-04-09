use std::convert::Infallible;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::sse::Event;
use axum::response::Sse;
use axum::Json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::errors::ErrorBody;
use super::state::{confined_root_for_search, AppState, TokioServerFs};
use wilkes_core::types::SearchQuery;

pub async fn search_handler(
    state: Arc<AppState>,
    mut query: SearchQuery,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, (StatusCode, Json<ErrorBody>)> {
    query.root = confined_root_for_search(
        &query.root.to_string_lossy(),
        &state.uploads_dir,
        &TokioServerFs,
    )
    .await?;

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);
    let ctx = Arc::clone(&state.ctx);

    tokio::spawn(async move {
        forward_search_results(ctx, query, tx).await;
    });

    Ok(Sse::new(ReceiverStream::new(rx)))
}

pub async fn forward_search_results(
    ctx: Arc<wilkes_api::context::AppContext>,
    query: SearchQuery,
    tx: mpsc::Sender<Result<Event, Infallible>>,
) {
    let handle = match Arc::clone(&ctx).start_search(query).await {
        Ok(h) => h,
        Err(e) => {
            let event = Event::default().event("error").data(e);
            let _ = tx.send(Ok(event)).await;
            return;
        }
    };

    let stats = handle
        .run(|fm| {
            let tx = tx.clone();
            async move {
                let data = match serde_json::to_string(&fm) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!("search serialize: {e}");
                        return true;
                    }
                };
                tx.send(Ok(Event::default().event("result").data(data)))
                    .await
                    .is_ok()
            }
        })
        .await;

    let data = serde_json::to_string(&stats).unwrap_or_default();
    let _ = tx
        .send(Ok(Event::default().event("complete").data(data)))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::state::{AppState, BroadcastEmitter};
    use tempfile::tempdir;
    use tokio::sync::broadcast;
    use wilkes_api::context::AppContext;
    use wilkes_core::embed::worker::manager::WorkerPaths;
    use wilkes_core::types::SearchMode;

    fn test_state() -> (tempfile::TempDir, Arc<AppState>) {
        let dir = tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        std::fs::create_dir_all(&uploads_dir).unwrap();
        std::fs::write(uploads_dir.join("settings.json"), "{}").unwrap();
        let settings_path = dir.path().join("settings.json");
        let paths = WorkerPaths {
            python_path: dir.path().join("python"),
            python_package_dir: dir.path().join("pkg"),
            requirements_path: dir.path().join("reqs.txt"),
            venv_dir: dir.path().join("venv"),
            worker_bin: dir.path().join("worker"),
            data_dir: dir.path().to_path_buf(),
        };
        let (events_tx, _) = broadcast::channel(16);
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _rx, _loop) =
            AppContext::new(dir.path().to_path_buf(), settings_path, paths, emitter);
        (
            dir,
            Arc::new(AppState {
                ctx,
                uploads_dir,
                events_tx,
            }),
        )
    }

    fn grep_query(root: std::path::PathBuf) -> SearchQuery {
        SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root,
            file_type_filters: vec![],
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 1024 * 1024,
            context_lines: 0,
            mode: SearchMode::Grep,
            supported_extensions: vec![],
        }
    }

    #[tokio::test]
    async fn forward_search_results_emits_result_and_complete() {
        let (dir, state) = test_state();
        let root = state.uploads_dir.join("docs");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.txt"), "hello world").unwrap();

        let (tx, mut rx) = mpsc::channel(16);
        forward_search_results(Arc::clone(&state.ctx), grep_query(root.clone()), tx).await;

        let mut received = 0usize;
        while let Ok(Some(_event)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
        {
            received += 1;
            if received >= 2 {
                break;
            }
        }

        assert!(received >= 2);
        drop(dir);
    }

    #[tokio::test]
    async fn forward_search_results_emits_error_for_semantic_without_index() {
        let (_dir, state) = test_state();
        let mut query = grep_query(state.uploads_dir.clone());
        query.mode = SearchMode::Semantic;

        let (tx, mut rx) = mpsc::channel(16);
        forward_search_results(Arc::clone(&state.ctx), query, tx).await;

        let _first = rx.recv().await.unwrap().unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn search_handler_confines_roots_to_uploads() {
        let (_dir, state) = test_state();
        let query = grep_query(state.uploads_dir.clone());
        let response = search_handler(Arc::clone(&state), query).await;
        assert!(response.is_ok());
    }
}
