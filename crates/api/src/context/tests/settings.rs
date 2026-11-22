use super::*;

#[tokio::test]
async fn test_update_settings_last_directory_restarts_directory_watcher() {
    let (dir, ctx) = test_ctx();
    let root1 = dir.path().join("root1");
    let root2 = dir.path().join("root2");
    std::fs::create_dir_all(&root1).unwrap();
    std::fs::create_dir_all(&root2).unwrap();

    ctx.update_settings(serde_json::json!({ "last_directory": root1 }))
        .await
        .unwrap();
    assert!(ctx.directory_watcher.lock().is_some());

    ctx.update_settings(serde_json::json!({ "last_directory": root2 }))
        .await
        .unwrap();
    assert!(ctx.directory_watcher.lock().is_some());

    ctx.update_settings(serde_json::json!({ "last_directory": null }))
        .await
        .unwrap();
    assert!(ctx.directory_watcher.lock().is_none());
}

/// The feature's reachability, end to end at this layer: a settings edit
/// is what turns image enrichment on and off, and what extraction reads is
/// the process-wide analyzer the edit moves.
///
/// Enabling it here fails, because the test context has no recognizer
/// installed — and that failure is the point twice over: it proves the
/// edit is acted on rather than ignored, and it proves an enabled feature
/// that cannot run says so instead of quietly extracting without it.
#[tokio::test]
async fn an_image_analysis_settings_edit_attaches_and_detaches_the_analyzer() {
    let (_dir, ctx) = test_ctx();
    assert!(
        wilkes_core::extract::image::configured().is_none(),
        "nothing is configured before anything asks for it"
    );

    ctx.update_settings(serde_json::json!({
        "image_analysis": { "enabled": true }
    }))
    .await
    .unwrap();
    assert!(
        ctx.load_image_analyzer().await.is_err(),
        "an enabled analyzer with no recognizer installed is a failure"
    );
    assert!(
        wilkes_core::extract::image::configured().is_none(),
        "a failed load detaches rather than leaving a stale analyzer"
    );

    ctx.update_settings(serde_json::json!({
        "image_analysis": { "enabled": false }
    }))
    .await
    .unwrap();
    assert!(wilkes_core::extract::image::configured().is_none());
    // Of the models that have something to download. A recognizer that is
    // part of the operating system — Apple Vision — has no artifacts and
    // no footprint, and reports itself installed because it is; asking a
    // temporary model directory about it would be asking the wrong thing.
    assert!(
        ctx.image_recognizer_catalogue()
            .models
            .iter()
            .filter(|model| model.footprint_bytes > 0)
            .all(|model| !model.is_cached),
        "the catalogue is the one answer to what is downloaded, and nothing is"
    );
}

/// Turning semantic preference off releases the embedder — the resource
/// that toggle owns — and nothing else. The index stays open with no
/// search consumer at all, because the reading surfaces read it.
#[tokio::test]
async fn semantic_pref_off_with_no_exact_consumer_releases_only_the_embedder() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();

    // Seed persisted settings with the toggle already ON so the update below
    // is a pure ON->OFF transition (no stray activate spawn to race with).
    std::fs::write(
        dir.path().join("settings.json"),
        serde_json::to_string(&Settings {
            search_prefer_semantic: true,
            last_directory: Some(root.clone()),
            ..Settings::default()
        })
        .unwrap(),
    )
    .unwrap();

    // Stand up a resident index + watcher as if semantic were active.
    let index = SemanticIndex::create(
        &ctx.data_dir,
        "teardown-model",
        384,
        EmbeddingEngine::Candle,
        None,
    )
    .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));
    *ctx.embedder.lock() = Some(Arc::new(MockEmbedder::default()) as Arc<dyn Embedder>);
    ctx.start_directory_watcher(root);
    assert!(ctx.directory_watcher.lock().is_some());

    ctx.update_settings(serde_json::json!({ "search_prefer_semantic": false }))
        .await
        .unwrap();

    // Turning the toggle off must leave file-list watching active while
    // releasing the resident model so file changes no longer reindex.
    assert!(ctx.directory_watcher.lock().is_some());
    assert!(ctx.embedder.lock().is_none());
    // The reading surfaces keep their store.
    assert!(ctx.is_index_loaded());
}

#[tokio::test]
async fn semantic_pref_off_retains_index_for_exact_search() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        dir.path().join("settings.json"),
        serde_json::to_string(&Settings {
            search_prefer_semantic: true,
            grep_use_index: true,
            last_directory: Some(root.clone()),
            ..Settings::default()
        })
        .unwrap(),
    )
    .unwrap();
    let index = SemanticIndex::create(
        &ctx.data_dir,
        "retained-model",
        384,
        EmbeddingEngine::Candle,
        None,
    )
    .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));
    *ctx.embedder.lock() = Some(Arc::new(MockEmbedder::default()) as Arc<dyn Embedder>);
    ctx.start_directory_watcher(root);

    ctx.update_settings(serde_json::json!({ "search_prefer_semantic": false }))
        .await
        .unwrap();

    assert!(ctx.embedder.lock().is_none());
    assert!(ctx.is_index_loaded());
    assert!(ctx.directory_watcher.lock().is_some());
}

/// Turning index-backed exact search on opens the semantic index with no
/// embedder — the document text index's upkeep copies its renditions;
/// turning it back off releases nothing, because the toggle owns no
/// embedder and the reading surfaces still read the index.
#[tokio::test]
async fn exact_index_toggle_loads_index_without_embedder_and_keeps_it() {
    let (dir, ctx) = test_ctx();
    let selected = SelectedEmbedder::default();
    SemanticIndex::create(
        &ctx.data_dir,
        selected.model.model_id(),
        selected.dimension,
        selected.engine,
        None,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("settings.json"),
        serde_json::to_string(&Settings {
            search_prefer_semantic: false,
            grep_use_index: false,
            semantic: SemanticSettings {
                enabled: true,
                index_path: Some(ctx.data_dir.join("semantic_index.db")),
                selected,
                ..SemanticSettings::default()
            },
            ..Settings::default()
        })
        .unwrap(),
    )
    .unwrap();

    ctx.update_settings(serde_json::json!({ "grep_use_index": true }))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !ctx.is_index_loaded() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("index-only activation should finish");
    assert!(ctx.embedder.lock().is_none());

    ctx.update_settings(serde_json::json!({ "grep_use_index": false }))
        .await
        .unwrap();
    assert!(ctx.is_index_loaded());
    assert!(ctx.embedder.lock().is_none());
}

#[tokio::test]
async fn disabling_exact_index_keeps_semantic_runtime_resident() {
    let (dir, ctx) = test_ctx();
    std::fs::write(
        dir.path().join("settings.json"),
        serde_json::to_string(&Settings {
            search_prefer_semantic: true,
            grep_use_index: true,
            ..Settings::default()
        })
        .unwrap(),
    )
    .unwrap();
    let index = SemanticIndex::create(
        &ctx.data_dir,
        "semantic-model",
        384,
        EmbeddingEngine::Candle,
        None,
    )
    .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));
    *ctx.embedder.lock() = Some(Arc::new(MockEmbedder::default()) as Arc<dyn Embedder>);

    ctx.update_settings(serde_json::json!({ "grep_use_index": false }))
        .await
        .unwrap();

    assert!(ctx.is_index_loaded());
    assert!(ctx.embedder.lock().is_some());
    assert!(ctx.is_semantic_ready());
}

#[tokio::test]
async fn test_settings_operations() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        settings_path.clone(),
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

    let initial = ctx.get_settings().await;
    assert_eq!(initial.context_lines, 2);

    let patch = serde_json::json!({
        "context_lines": 5,
        "max_file_size": 23 * 1024 * 1024,
        "max_download_bytes": 2_000_000_000u64
    });
    let updated = ctx.update_settings(patch).await.unwrap();
    assert_eq!(updated.context_lines, 5);
    assert_eq!(
        wilkes_agent::search::SearchService::max_search_file_size(ctx.clone()).await,
        23 * 1024 * 1024
    );
    assert_eq!(
        wilkes_agent::search::SearchService::max_download_bytes(ctx.clone()).await,
        2_000_000_000
    );

    let _updated_semantic = ctx
        .update_semantic_settings(|s| SemanticSettings { enabled: true, ..s })
        .await;

    // Settings should have been saved to disk
    let disk_content = tokio::fs::read_to_string(&settings_path).await.unwrap();
    assert!(disk_content.contains("\"context_lines\": 5"));
    assert!(disk_content.contains("\"enabled\": true"));
}

#[tokio::test]
async fn test_update_semantic_settings_error() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("invalid.json");
    std::fs::write(&settings_path, "{ broken }").unwrap();

    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        settings_path,
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

    ctx.update_semantic_settings(|s| s).await;
}

#[tokio::test]
async fn test_update_settings_patch() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        settings_path.clone(),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: dir.path().to_path_buf(),
        },
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );

    let patch = serde_json::json!({
        "supported_extensions": ["rs", "txt"]
    });
    ctx.update_settings(patch).await.unwrap();

    let settings = ctx.get_settings().await;
    assert_eq!(settings.supported_extensions, vec!["rs", "txt"]);
}

#[tokio::test]
async fn test_update_semantic_settings_patch() {
    let dir = tempdir().unwrap();
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths::resolve(dir.path()),
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );

    ctx.update_semantic_settings(|mut s| {
        s.chunk_size = 1234;
        s
    })
    .await;

    let settings = ctx.get_settings().await;
    assert_eq!(settings.semantic.chunk_size, 1234);
}

#[tokio::test]
async fn test_update_semantic_settings_success() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("s.json");
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        settings_path.clone(),
        WorkerPaths::resolve(dir.path()),
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );

    ctx.update_semantic_settings(|s| SemanticSettings { enabled: true, ..s })
        .await;

    let s = ctx.get_settings().await;
    assert!(s.semantic.enabled);
    assert!(settings_path.exists());
}
