use std::io::BufRead;
use std::sync::Arc;

use wilkes_core::embed::dispatch;
use wilkes_core::embed::installer::EmbedProgress;
use wilkes_core::embed::worker_ipc::{WorkerEvent, WorkerRequest};
use wilkes_core::types::EmbedderModel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    wilkes_core::logging::init_logging_stderr();

    tracing::info!("[worker] starting up...");

    let stdin = std::io::stdin();
    let mut active_embedder: Option<Arc<dyn wilkes_core::embed::Embedder>> = None;

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            break;
        }

        let trimmed = line.trim();
        let log_line = if trimmed.len() > 300 {
            format!("{}...", &trimmed[..300])
        } else {
            trimmed.to_string()
        };
        tracing::info!("[worker] received request: {}", log_line);

        let req: WorkerRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                emit(WorkerEvent::Error(format!("Failed to parse worker config: {e}")));
                continue;
            }
        };

        match req.mode.as_str() {
            "build" => {
                let model = EmbedderModel(req.model.clone());
                let embedder = match dispatch::load_embedder_local(req.engine, &model, &req.data_dir, &req.device) {
                    Ok(e) => e,
                    Err(e) => {
                        emit(WorkerEvent::Error(format!("Failed to load embedder: {e}")));
                        continue;
                    }
                };
                active_embedder = Some(Arc::clone(&embedder));

                let (tx, mut rx) = tokio::sync::mpsc::channel::<EmbedProgress>(64);
                let forward = tokio::spawn(async move {
                    while let Some(progress) = rx.recv().await {
                        emit(WorkerEvent::Progress(progress));
                    }
                });

                let result = wilkes_api::commands::embed::build_index_with_embedder(
                    req.root,
                    req.engine,
                    embedder,
                    req.data_dir,
                    tx,
                    match req.chunk_size {
                        Some(v) => v,
                        None => {
                            emit(WorkerEvent::Error("build request missing chunk_size".into()));
                            continue;
                        }
                    },
                    match req.chunk_overlap {
                        Some(v) => v,
                        None => {
                            emit(WorkerEvent::Error("build request missing chunk_overlap".into()));
                            continue;
                        }
                    },
                    req.supported_extensions,
                )
                .await;

                forward.await?;

                match result {
                    Ok(_) => emit(WorkerEvent::Done),
                    Err(e) => emit(WorkerEvent::Error(e.to_string())),
                }
            }

            "embed" => {
                let embedder = match get_or_load_embedder(&mut active_embedder, &req) {
                    Ok(e) => e,
                    Err(e) => {
                        emit(WorkerEvent::Error(format!("Failed to load embedder: {e}")));
                        continue;
                    }
                };

                let texts = req.texts.unwrap_or_default();
                let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                match embedder.embed(&text_refs) {
                    Ok(embeddings) => {
                        emit(WorkerEvent::Embeddings(embeddings));
                        emit(WorkerEvent::Done);
                    }
                    Err(e) => emit(WorkerEvent::Error(format!("Embed error: {e}"))),
                }
            }

            "info" => {
                let embedder = match get_or_load_embedder(&mut active_embedder, &req) {
                    Ok(e) => e,
                    Err(e) => {
                        emit(WorkerEvent::Error(format!("Failed to load embedder for info: {e}")));
                        continue;
                    }
                };
                emit(WorkerEvent::Info {
                    dimension: embedder.dimension(),
                    max_seq_length: 512,
                });
                emit(WorkerEvent::Done);
            }

            other => emit(WorkerEvent::Error(format!("Unknown mode: {other}"))),
        }
    }

    Ok(())
}

fn get_or_load_embedder(
    active: &mut Option<Arc<dyn wilkes_core::embed::Embedder>>,
    req: &WorkerRequest,
) -> anyhow::Result<Arc<dyn wilkes_core::embed::Embedder>> {
    if let Some(ref e) = active {
        return Ok(Arc::clone(e));
    }
    let model = EmbedderModel(req.model.clone());
    let embedder = dispatch::load_embedder_local(req.engine, &model, &req.data_dir, &req.device)?;
    *active = Some(Arc::clone(&embedder));
    Ok(embedder)
}

fn emit(event: WorkerEvent) {
    let line = serde_json::to_string(&event).expect("WorkerEvent serialization failed");
    println!("{line}");
}
