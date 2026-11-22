use super::*;

#[tokio::test]
async fn test_shutdown() {
    let dir = tempdir().unwrap();
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
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
        emitter,
    );

    ctx.start_directory_watcher(dir.path().to_path_buf());

    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();
    let join = tokio::spawn(async move {
        cancel_for_task.cancelled().await;
        Ok(())
    });
    *ctx.embed_task.lock() = Some(EmbedTaskHandle {
        operation: EmbedOperation::Build,
        cancel,
        cancel_flag: Arc::new(AtomicBool::new(false)),
        join: BuildHandle::new(join),
        settlement: Arc::new(BuildSettlement::new()),
    });

    ctx.shutdown().await;

    assert!(ctx.directory_watcher.lock().is_none());
    assert!(ctx.embed_task.lock().is_none());
}

#[tokio::test]
async fn nothing_is_removed_from_under_a_running_build() {
    let (_dir, ctx) = test_ctx();
    pretend_a_build_is_running(&ctx);

    let env = ctx.remove_python_environments(vec!["0".repeat(16)]).await;
    assert!(env.unwrap_err().contains("being built"));

    let model = ctx
        .remove_custom_model(EmbeddingEngine::SBERT, "org/model".to_string())
        .await;
    assert!(model.unwrap_err().contains("being built"));
}

#[tokio::test]
async fn the_selected_model_cannot_be_removed() {
    let (_dir, ctx) = test_ctx();
    let selected = ctx.settings().await.semantic.selected;

    let err = ctx
        .remove_custom_model(selected.engine, selected.model.model_id().to_string())
        .await
        .unwrap_err();

    assert!(err.contains("Choose another model"), "{err}");
}

#[tokio::test]
async fn a_removed_custom_model_leaves_the_catalog_and_the_disk() {
    let (_dir, ctx) = test_ctx();
    ctx.update_semantic_settings(|mut semantic| {
        semantic
            .custom_models
            .push(wilkes_core::types::CustomModel {
                engine: EmbeddingEngine::Candle,
                model_id: "org/model".to_string(),
            });
        semantic
    })
    .await;

    ctx.remove_custom_model(EmbeddingEngine::Candle, "org/model".to_string())
        .await
        .unwrap();

    assert!(ctx.settings().await.semantic.custom_models.is_empty());
}

#[tokio::test]
async fn test_kill_worker() {
    let dir = tempdir().unwrap();
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
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
        emitter,
    );

    ctx.kill_worker(); // Should not panic
}

#[tokio::test]
async fn test_set_worker_timeout() {
    let dir = tempdir().unwrap();
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
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
        emitter,
    );

    let res = ctx.set_worker_timeout(100).await;
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_get_worker_status() {
    let dir = tempdir().unwrap();
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
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
        emitter,
    );

    let status = ctx.get_worker_status();
    assert!(!status.active);
}

#[tokio::test]
async fn test_worker_operations() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let settings_path = dir.path().join("settings.json");
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        data_dir.clone(),
        settings_path,
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: data_dir.clone(),
        },
        emitter,
    );

    let status = ctx.get_worker_status();
    assert!(!status.active);

    ctx.kill_worker();

    // set_worker_timeout sends to the manager loop, which is running, so it should succeed.
    let res = ctx.set_worker_timeout(10).await;
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_worker_status_timeout_and_delete_index_wrapper() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let settings_path = dir.path().join("settings.json");
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });

    let (ctx, _event_rx, loop_fut) = AppContext::new(
        data_dir.clone(),
        settings_path,
        WorkerPaths::resolve(dir.path()),
        emitter,
    );
    let _loop_handle = tokio::spawn(loop_fut);

    let status = ctx.get_worker_status();
    assert!(!status.active);
    assert_eq!(status.timeout_secs, 300);

    ctx.set_worker_timeout(123).await.unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    let status = ctx.get_worker_status();
    assert_eq!(status.timeout_secs, 123);

    let index = SemanticIndex::create(
        &data_dir,
        "test-model",
        3,
        EmbeddingEngine::Candle,
        Some(dir.path()),
    )
    .unwrap();
    drop(index);

    let index_path = data_dir.join("semantic_index.db");
    assert!(index_path.exists());

    let status = ctx.get_index_status(None).await.unwrap();
    assert_eq!(status.model_id, "test-model");

    ctx.delete_index(None).await.unwrap();
    assert!(!index_path.exists());
}

#[tokio::test]
async fn test_shutdown_twice() {
    let (_dir, ctx) = test_ctx();
    ctx.shutdown().await;
    ctx.shutdown().await; // Should return early
}
