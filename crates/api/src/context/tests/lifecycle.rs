use super::*;

#[test]
fn test_emit_embed_error_logs_and_emits() {
    wilkes_core::logging::clear_logs();

    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: events.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths::resolve(dir.path()),
        emitter,
    );

    let subscriber = tracing_subscriber::registry().with(wilkes_core::logging::BufferLayer);
    subscriber::with_default(subscriber, || {
        ctx.emit_embed_error("Build", "Worker error");
    });

    let logs = wilkes_core::logging::get_logs();
    assert!(logs
        .iter()
        .any(|line| line.contains("Build failed: Worker error")));

    let events_guard = events.lock().unwrap();
    assert!(events_guard.iter().any(|(name, payload)| {
        name == "embed-error"
            && payload["operation"] == "Build"
            && payload["message"] == "Worker error"
    }));
}

#[test]
fn test_emit_embed_error_preserves_error_chain() {
    wilkes_core::logging::clear_logs();

    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: events.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths::resolve(dir.path()),
        emitter,
    );

    let err = anyhow::anyhow!("inner worker failure")
        .context("Fatal embedder error while indexing /tmp/example.md");

    let subscriber = tracing_subscriber::registry().with(wilkes_core::logging::BufferLayer);
    subscriber::with_default(subscriber, || {
        ctx.emit_embed_error("Build", format!("{err:#}"));
    });

    let logs = wilkes_core::logging::get_logs();
    assert!(logs.iter().any(|line| {
        line.contains("Build failed: Fatal embedder error while indexing /tmp/example.md")
    }));
    assert!(logs
        .iter()
        .any(|line| line.contains("inner worker failure")));

    let events_guard = events.lock().unwrap();
    assert!(events_guard.iter().any(|(name, payload)| {
        name == "embed-error"
            && payload["operation"] == "Build"
            && payload["message"].as_str().is_some_and(|msg| {
                msg.contains("Fatal embedder error while indexing /tmp/example.md")
                    && msg.contains("inner worker failure")
            })
    }));
}

#[tokio::test]
async fn test_app_context_new() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let settings_path = dir.path().join("settings.json");
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("py_pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: PathBuf::from("data"),
    };

    let (ctx, _event_rx, _loop_fut) =
        AppContext::new(data_dir, settings_path.clone(), paths, emitter);

    ctx.update_semantic_settings(|s| SemanticSettings {
        enabled: true,
        chunk_size: 1234,
        ..s
    })
    .await;

    let updated = get_settings(&settings_path).await.unwrap();
    assert!(updated.semantic.enabled);
    assert_eq!(updated.semantic.chunk_size, 1234);
}

#[tokio::test]
async fn test_event_forwarder() {
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: events.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
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
        emitter.clone(),
    );

    let (tx, rx) = mpsc::channel(1);
    let forwarder = tokio::spawn(AppContext::run_event_forwarder(Arc::clone(&ctx.events), rx));

    tx.send(ManagerEvent::WorkerStarting).await.unwrap();
    tx.send(ManagerEvent::ReindexingDone).await.unwrap();
    drop(tx);
    forwarder.await.unwrap();

    let events_guard = events.lock().unwrap();
    assert_eq!(events_guard.len(), 2);
    assert_eq!(events_guard[0].0, "manager-event");
    assert_eq!(events_guard[0].1, serde_json::json!("WorkerStarting"));
    assert_eq!(events_guard[1].1, serde_json::json!("ReindexingDone"));
}

#[tokio::test]
async fn test_spawn_background_tasks() {
    let dir = tempdir().unwrap();
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, rx, loop_fut) = AppContext::new(
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

    ctx.spawn_background_tasks(rx, loop_fut);
    // Just verify it doesn't panic and tasks are spawned
}
