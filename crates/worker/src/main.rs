use std::io::BufRead;

use wilkes_core::embed::dispatch;
use wilkes_core::embed::installer::EmbedProgress;
use wilkes_core::embed::worker_ipc::{WorkerEvent, WorkerRequest};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    wilkes_core::logging::init_logging();

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            break;
        }

        let req: WorkerRequest = match serde_json::from_str(line.trim()) {
            Ok(r) => r,
            Err(e) => {
                emit(WorkerEvent::Error(format!("Failed to parse worker config: {e}")));
                continue;
            }
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel::<EmbedProgress>(64);

        // Forward progress events to stdout so the parent can emit Tauri events.
        let forward = tokio::spawn(async move {
            while let Some(progress) = rx.recv().await {
                emit(WorkerEvent::Progress(progress));
            }
        });

        // The worker binary only handles Candle/Fastembed, never SBERT.
        // We create a dummy manager to satisfy the API.
        let (dummy_manager, _, _) = wilkes_core::embed::worker_manager::WorkerManager::new(
            wilkes_core::embed::worker_manager::WorkerPaths {
                python_path: "".into(),
                script_path: "".into(),
                worker_bin: "".into(),
            }
        );

        let result = if req.mode == "build" {
            wilkes_api::commands::embed::build_index(
                req.root,
                req.engine,
                wilkes_core::types::EmbedderModel(req.model.clone()),
                dummy_manager,
                req.device.clone(),
                req.data_dir,
                tx,
                req.chunk_size,
                req.chunk_overlap,
            )
            .await
        } else {
            // Note: Rust worker embed texts mode isn't implemented in the binary yet,
            // but we can just emit an error or skip if it's called.
            Err(anyhow::anyhow!("Rust worker embed mode not implemented via binary"))
        };

        // Wait for the forwarder so all progress lines are flushed before Done/Error.
        forward.await?;

        match result {
            Ok(_) => emit(WorkerEvent::Done),
            Err(e) => emit(WorkerEvent::Error(e.to_string())),
        }
    }

    Ok(())
}

fn emit(event: WorkerEvent) {
    let line = serde_json::to_string(&event).expect("WorkerEvent serialization failed");
    println!("{line}");
}
