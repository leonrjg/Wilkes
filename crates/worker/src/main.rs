use std::io::BufRead;

use wilkes_core::embed::dispatch;
use wilkes_core::embed::installer::EmbedProgress;
use wilkes_core::embed::worker_ipc::{WorkerEvent, WorkerRequest};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    wilkes_core::logging::init_logging();

    // Read one JSON line from stdin — the desktop writes it before closing stdin.
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let req: WorkerRequest = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("Failed to parse worker config: {e}"))?;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<EmbedProgress>(64);

    // Forward progress events to stdout so the parent can emit Tauri events.
    let forward = tokio::spawn(async move {
        while let Some(progress) = rx.recv().await {
            emit(WorkerEvent::Progress(progress));
        }
    });

    let installer = dispatch::get_installer(req.engine, wilkes_core::types::EmbedderModel(req.model));
    let result = wilkes_api::commands::embed::build_index(
        req.root,
        installer.as_ref(),
        req.engine,
        req.data_dir,
        tx,
        req.chunk_size,
        req.chunk_overlap,
    )
    .await;

    // Wait for the forwarder so all progress lines are flushed before Done/Error.
    forward.await?;

    match result {
        Ok(_) => emit(WorkerEvent::Done),
        Err(e) => emit(WorkerEvent::Error(e.to_string())),
    }

    Ok(())
}

fn emit(event: WorkerEvent) {
    let line = serde_json::to_string(&event).expect("WorkerEvent serialization failed");
    println!("{line}");
}
