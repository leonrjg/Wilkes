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
