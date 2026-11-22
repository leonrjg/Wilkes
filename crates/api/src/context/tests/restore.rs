use super::*;

#[test]
fn test_restore_state_needs_reset_on_missing_db() {
    let settings = Settings {
        favorites: vec![],
        recent_dirs: vec![],
        last_directory: None,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 2,
        theme: Theme::default(),
        search_prefer_semantic: false,
        semantic: SemanticSettings {
            enabled: true,
            selected: SelectedEmbedder::default_for(EmbeddingEngine::Candle),
            index_path: Some(PathBuf::from("semantic_index.db")),
            ..SemanticSettings::default()
        },
        integrations: Default::default(),
        supported_extensions: vec![],
        max_results: 0,
        bookmarks_dock: BookmarkDock::default(),
        ..Settings::default()
    };

    assert!(AppContext::restore_state_needs_reset(&settings, None));
}

#[test]
fn test_restore_state_needs_reset_on_mismatch() {
    let settings = Settings {
        favorites: vec![],
        recent_dirs: vec![],
        last_directory: None,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 2,
        theme: Theme::default(),
        search_prefer_semantic: false,
        semantic: SemanticSettings {
            enabled: true,
            selected: SelectedEmbedder {
                engine: EmbeddingEngine::Fastembed,
                model: EmbedderModel("model-a".to_string()),
                dimension: 384,
            },
            index_path: Some(PathBuf::from("semantic_index.db")),
            ..SemanticSettings::default()
        },
        integrations: Default::default(),
        supported_extensions: vec![],
        max_results: 0,
        bookmarks_dock: BookmarkDock::default(),
        ..Settings::default()
    };
    let db_status = IndexStatus {
        indexed_files: 1,
        total_chunks: 1,
        built_at: None,
        build_duration_ms: None,
        engine: EmbeddingEngine::Candle,
        model_id: "model-b".to_string(),
        dimension: 384,
        root_path: None,
        db_size_bytes: None,
    };

    assert!(AppContext::restore_state_needs_reset(
        &settings,
        Some(&db_status)
    ));
}

#[test]
fn test_restore_state_needs_reset_false_when_matching() {
    let settings = Settings {
        favorites: vec![],
        recent_dirs: vec![],
        last_directory: None,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 2,
        theme: Theme::default(),
        search_prefer_semantic: false,
        semantic: SemanticSettings {
            enabled: true,
            selected: SelectedEmbedder {
                engine: EmbeddingEngine::Fastembed,
                model: EmbedderModel("model-a".to_string()),
                dimension: 384,
            },
            index_path: Some(PathBuf::from("semantic_index.db")),
            ..SemanticSettings::default()
        },
        integrations: Default::default(),
        supported_extensions: vec![],
        max_results: 0,
        bookmarks_dock: BookmarkDock::default(),
        ..Settings::default()
    };
    let db_status = IndexStatus {
        indexed_files: 1,
        total_chunks: 1,
        built_at: None,
        build_duration_ms: None,
        engine: EmbeddingEngine::Fastembed,
        model_id: "model-a".to_string(),
        dimension: 384,
        root_path: None,
        db_size_bytes: None,
    };

    assert!(!AppContext::restore_state_needs_reset(
        &settings,
        Some(&db_status)
    ));
}

#[test]
fn test_restore_state_indexing_config() {
    let settings = Settings {
        supported_extensions: vec!["txt".to_string(), "md".to_string()],
        semantic: SemanticSettings {
            chunk_size: 128,
            chunk_overlap: 32,
            ..SemanticSettings::default()
        },
        ..Settings::default()
    };

    let indexing = AppContext::restore_state_indexing_config(&settings);
    assert_eq!(indexing.chunk_size, 128);
    assert_eq!(indexing.chunk_overlap, 32);
    assert_eq!(indexing.supported_extensions, vec!["txt", "md"]);
}

#[test]
fn test_restore_state_enabled_settings() {
    let current = SemanticSettings {
        enabled: false,
        index_path: None,
        ..SemanticSettings::default()
    };
    let selected = SelectedEmbedder {
        engine: EmbeddingEngine::Fastembed,
        model: EmbedderModel("model-a".to_string()),
        dimension: 384,
    };
    let updated = AppContext::restore_state_enabled_settings(
        current,
        PathBuf::from("semantic_index.db"),
        selected,
        768,
    );

    assert!(updated.enabled);
    assert_eq!(updated.index_path, Some(PathBuf::from("semantic_index.db")));
    assert_eq!(updated.selected.dimension, 768);
}

#[test]
fn test_open_semantic_index_with_error() {
    let dir = tempdir().unwrap();
    let result = AppContext::open_semantic_index_with(
        &dir.path().to_path_buf(),
        "model-a",
        384,
        |_dir, _model_id, _dim| Err(anyhow::anyhow!("open failed")),
    );

    match result {
        Ok(_) => panic!("expected open error"),
        Err(err) => assert!(err.to_string().contains("open failed")),
    }
}

#[test]
fn test_prepare_restore_state_plan_variants() {
    let matching = Settings {
        semantic: SemanticSettings {
            selected: SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("model-a".to_string()),
                dimension: 384,
            },
            ..SemanticSettings::default()
        },
        ..Settings::default()
    };
    let db_status = IndexStatus {
        indexed_files: 1,
        total_chunks: 1,
        built_at: None,
        build_duration_ms: None,
        engine: EmbeddingEngine::Candle,
        model_id: "model-a".to_string(),
        dimension: 384,
        root_path: None,
        db_size_bytes: None,
    };

    match AppContext::prepare_restore_state_plan(matching.clone(), db_status.clone()) {
        RestoreStatePreparation::Ready(plan) => {
            assert_eq!(plan.selected.model.model_id(), "model-a");
            assert_eq!(plan.db_status.model_id, "model-a");
        }
        RestoreStatePreparation::ResetStaleSelection { .. } => panic!("expected ready plan"),
    }

    let mismatched = Settings {
        semantic: SemanticSettings {
            selected: SelectedEmbedder {
                engine: EmbeddingEngine::Fastembed,
                model: EmbedderModel("model-b".to_string()),
                dimension: 384,
            },
            ..SemanticSettings::default()
        },
        ..Settings::default()
    };
    match AppContext::prepare_restore_state_plan(mismatched, db_status) {
        RestoreStatePreparation::ResetStaleSelection {
            db_status,
            selected,
        } => {
            assert_eq!(db_status.model_id, "model-a");
            assert_eq!(selected.model.model_id(), "model-b");
        }
        RestoreStatePreparation::Ready(_) => panic!("expected stale-selection reset"),
    }
}

#[tokio::test]
async fn test_restore_embedder_with_installer_branches() {
    let (_dir, ctx) = test_ctx();

    let install_fail: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
        install_calls: Arc::new(AtomicUsize::new(0)),
        build_calls: Arc::new(AtomicUsize::new(0)),
        available: true,
        install_should_fail: true,
        build_should_fail: false,
    });
    assert!(ctx.restore_embedder_with(install_fail).await.is_none());

    let unavailable: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
        install_calls: Arc::new(AtomicUsize::new(0)),
        build_calls: Arc::new(AtomicUsize::new(0)),
        available: false,
        install_should_fail: false,
        build_should_fail: false,
    });
    assert!(ctx.restore_embedder_with(unavailable).await.is_none());

    let build_fail: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
        install_calls: Arc::new(AtomicUsize::new(0)),
        build_calls: Arc::new(AtomicUsize::new(0)),
        available: true,
        install_should_fail: false,
        build_should_fail: true,
    });
    assert!(ctx.restore_embedder_with(build_fail).await.is_none());

    let ok: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
        install_calls: Arc::new(AtomicUsize::new(0)),
        build_calls: Arc::new(AtomicUsize::new(0)),
        available: true,
        install_should_fail: false,
        build_should_fail: false,
    });
    assert!(ctx.restore_embedder_with(ok).await.is_some());
}

#[tokio::test]
async fn test_finish_restore_state_persists_semantic_state() {
    let (_dir2, ctx2) = test_ctx();
    let data_dir2 = ctx2.data_dir.clone();
    let index = SemanticIndex::create(
        &data_dir2,
        "restore-model",
        384,
        EmbeddingEngine::Candle,
        None,
    )
    .unwrap();
    let plan = RestoreStatePlan {
        location: EmbeddingLocation::Local {
            device: "cpu".to_string(),
            batch_size: 16,
        },
        db_status: IndexStatus {
            indexed_files: 1,
            total_chunks: 1,
            built_at: None,
            build_duration_ms: None,
            engine: EmbeddingEngine::Candle,
            model_id: "restore-model".to_string(),
            dimension: 384,
            root_path: None,
            db_size_bytes: None,
        },
        selected: SelectedEmbedder {
            engine: EmbeddingEngine::Candle,
            model: EmbedderModel("restore-model".to_string()),
            dimension: 1,
        },
    };
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
    ctx2.finish_restore_state(&plan, embedder, index).await;

    let settings = ctx2.get_settings().await;
    assert!(settings.semantic.enabled);
    assert_eq!(settings.semantic.selected.dimension, 384);
}

/// With both search consumers off, an index on disk is still opened —
/// without an embedder — because the reading surfaces read it. The proof
/// is the surface itself: a PDF preview that carries the areas this
/// document's reading owns rather than the page's raw glyphs.
#[tokio::test]
async fn restore_state_opens_the_index_for_the_reading_surfaces_with_search_off() {
    use wilkes_core::embed::index::chunk::Chunk;
    use wilkes_core::embed::index::db::PreparedFile;
    use wilkes_core::types::{
        BoundingBox, ByteRange, MatchRef, PreviewData, ReadingRegion, RetainedExtraction,
        SourceOrigin,
    };

    let (dir, ctx) = test_ctx();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();

    // A fully restorable index DB sits on disk with the default selection,
    // holding one document whose reading owns a page area.
    let selected = SelectedEmbedder::default();
    let mut index = SemanticIndex::create(
        &ctx.data_dir,
        selected.model.model_id(),
        selected.dimension,
        selected.engine,
        None,
    )
    .unwrap();
    let pdf = root.join("paper.pdf");
    std::fs::write(&pdf, "pdf bytes").unwrap();
    let full_text = "1 A\\to A S\\colon A,B,Z_{A}\n".to_string();
    let end = full_text.trim_end().len();
    index
        .write_file(PreparedFile {
            path: pdf.clone(),
            full_text: full_text.clone(),
            retained: RetainedExtraction {
                reading_regions: vec![ReadingRegion {
                    area_id: "p1-i0".to_string(),
                    page: 1,
                    bbox: BoundingBox {
                        x: 10.0,
                        y: 20.0,
                        width: 300.0,
                        height: 24.0,
                    },
                    text_range: ByteRange { start: 0, end },
                }],
                ..Default::default()
            },
            chunks: vec![(
                Chunk {
                    file_path: pdf.clone(),
                    text: full_text.clone(),
                    byte_range: ByteRange {
                        start: 0,
                        end: full_text.len(),
                    },
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: None,
                    },
                },
                vec![0.0; selected.dimension],
            )],
        })
        .unwrap();
    drop(index);

    // Persist settings with both semantic preference and index-backed exact
    // search off while the built index remains recorded on disk.
    let disabled = Settings {
        search_prefer_semantic: false,
        grep_use_index: false,
        last_directory: Some(root),
        semantic: SemanticSettings {
            enabled: true,
            index_path: Some(ctx.data_dir.join("semantic_index.db")),
            selected: selected.clone(),
            ..SemanticSettings::default()
        },
        ..Settings::default()
    };
    std::fs::write(
        dir.path().join("settings.json"),
        serde_json::to_string(&disabled).unwrap(),
    )
    .unwrap();

    // Control: the index is structurally restorable, so nothing but the
    // consumer question is in play here.
    let db_status = ctx
        .load_restore_db_status(&disabled)
        .await
        .expect("db status present");
    assert!(!AppContext::restore_state_needs_reset(
        &disabled,
        Some(&db_status)
    ));

    Arc::clone(&ctx).restore_state().await;

    // Directory watching is independent of semantic restore, so it starts
    // for file-list invalidation even when semantic search is disabled.
    assert!(ctx.directory_watcher.lock().is_some());
    // The embedder is what the search settings decide; it stays absent.
    assert!(ctx.embedder.lock().is_none());
    assert!(!ctx.is_semantic_ready());
    assert!(ctx.is_index_loaded());
    assert!(!ctx.get_settings().await.search_prefer_semantic);

    let data = ctx
        .preview(MatchRef {
            path: pdf,
            origin: SourceOrigin::PdfPage {
                page: 1,
                bbox: None,
            },
            text_range: None,
        })
        .await
        .expect("a PDF match previews");
    let PreviewData::Pdf { superseded, .. } = data else {
        panic!("a PDF match previews as a PDF");
    };
    assert_eq!(superseded.len(), 1);
    assert_eq!(superseded[0].text, full_text.trim_end());
}

/// The same startup path with nothing on disk opens nothing and reports no
/// error: the reading surfaces get the empty answer, not a failure.
#[tokio::test]
async fn restore_state_opens_nothing_when_no_index_exists_and_search_is_off() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let settings = Settings {
        search_prefer_semantic: false,
        grep_use_index: false,
        last_directory: Some(root),
        ..Settings::default()
    };
    std::fs::write(
        dir.path().join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();

    Arc::clone(&ctx).restore_state().await;

    assert!(!ctx.is_index_loaded());
    assert!(ctx.embedder.lock().is_none());
    assert!(!ctx.is_semantic_ready());
    assert!(ctx.directory_watcher.lock().is_some());
    // Nothing was built, so nothing stale was recorded to clear either.
    let after = ctx.get_settings().await;
    assert!(!after.semantic.enabled);
    assert!(after.semantic.index_path.is_none());
}

#[tokio::test]
async fn restore_state_loads_index_without_embedder_for_exact_search() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let selected = SelectedEmbedder::default();
    SemanticIndex::create(
        &ctx.data_dir,
        selected.model.model_id(),
        selected.dimension,
        selected.engine,
        None,
    )
    .unwrap();
    let settings = Settings {
        search_prefer_semantic: false,
        grep_use_index: true,
        last_directory: Some(root),
        semantic: SemanticSettings {
            enabled: true,
            index_path: Some(ctx.data_dir.join("semantic_index.db")),
            selected,
            ..SemanticSettings::default()
        },
        ..Settings::default()
    };
    std::fs::write(
        dir.path().join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();

    Arc::clone(&ctx).restore_state().await;

    assert!(ctx.is_index_loaded());
    assert!(ctx.embedder.lock().is_none());
    assert!(!ctx.is_semantic_ready());
    assert!(ctx.directory_watcher.lock().is_some());
}

#[tokio::test]
async fn test_load_restore_db_status_clears_stale_settings_when_missing_db() {
    let (_dir, ctx) = test_ctx();
    let settings = Settings {
        semantic: SemanticSettings {
            enabled: true,
            index_path: Some(PathBuf::from("semantic_index.db")),
            ..SemanticSettings::default()
        },
        ..Settings::default()
    };
    ctx.update_settings(serde_json::json!({
        "semantic": {
            "enabled": true,
            "index_path": "semantic_index.db"
        }
    }))
    .await
    .unwrap();

    let db_status = ctx.load_restore_db_status(&settings).await;
    assert!(db_status.is_none());

    let updated = ctx.get_settings().await;
    assert!(!updated.semantic.enabled);
    assert!(updated.semantic.index_path.is_none());
}

#[tokio::test]
async fn test_load_restore_db_status_reads_existing_db() {
    let (dir, ctx) = test_ctx();
    let index = SemanticIndex::create(
        &ctx.data_dir,
        "restore-model",
        384,
        EmbeddingEngine::Candle,
        Some(dir.path()),
    )
    .unwrap();
    let settings = Settings {
        semantic: SemanticSettings {
            enabled: true,
            index_path: Some(PathBuf::from("semantic_index.db")),
            ..SemanticSettings::default()
        },
        ..Settings::default()
    };

    let db_status = ctx.load_restore_db_status(&settings).await;
    let db_status = db_status.expect("expected restore db status");
    assert_eq!(db_status.model_id, "restore-model");
    assert_eq!(db_status.dimension, 384);

    drop(index);
}

#[tokio::test]
async fn test_restore_state_no_index() {
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

    // No index on disk, no settings
    ctx.clone().restore_state().await;
    assert!(!ctx.is_semantic_ready());
}

#[tokio::test]
async fn test_restore_state_invalid_settings_json_returns_early() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let settings_path = dir.path().join("settings.json");
    std::fs::write(&settings_path, "{not valid json").unwrap();

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
            data_dir,
        },
        emitter,
    );

    ctx.clone().restore_state().await;

    assert!(ctx.embedder.lock().is_none());
    assert!(ctx.index.lock().lock().unwrap().is_none());
    assert!(ctx.directory_watcher.lock().is_none());
}

#[tokio::test]
async fn test_restore_state_model_mismatch() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    // Write settings with model A
    let settings = Settings {
        semantic: SemanticSettings {
            selected: SelectedEmbedder {
                model: EmbedderModel("model-A".to_string()),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    tokio::fs::write(&settings_path, serde_json::to_string(&settings).unwrap())
        .await
        .unwrap();

    // Write index status with model B
    SemanticIndex::create(&data_dir, "model-B", 1, EmbeddingEngine::Candle, None).unwrap();

    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        data_dir.clone(),
        settings_path.clone(),
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

    ctx.clone().restore_state().await;

    // Should have cleared the stale index reference in settings
    let updated_settings = ctx.get_settings().await;
    assert!(!updated_settings.semantic.enabled);
    assert!(updated_settings.semantic.index_path.is_none());
}

#[tokio::test]
async fn test_restore_state_model_mismatch_clears_settings() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let settings_path = dir.path().join("settings.json");

    let settings = Settings {
        search_prefer_semantic: true,
        semantic: SemanticSettings {
            selected: SelectedEmbedder {
                model: EmbedderModel("model-A".to_string()),
                ..Default::default()
            },
            enabled: true,
            index_path: Some(data_dir.join("semantic_index.db")),
            ..Default::default()
        },
        ..Default::default()
    };
    std::fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

    wilkes_core::embed::index::SemanticIndex::create(
        &data_dir,
        "model-B",
        1,
        EmbeddingEngine::Candle,
        None,
    )
    .unwrap();

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

    ctx.clone().restore_state().await;
    let updated = ctx.get_settings().await;
    assert!(!updated.semantic.enabled);
    assert!(updated.semantic.index_path.is_none());
}

#[tokio::test]
async fn test_restore_state_complex() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");

    // Seed settings.json with a different model
    let mut initial_settings = wilkes_core::types::Settings::default();
    initial_settings.semantic.selected.model = EmbedderModel("m1".to_string());
    std::fs::write(
        &settings_path,
        serde_json::to_string(&initial_settings).unwrap(),
    )
    .unwrap();

    // Create an index status file matching that model
    let index_dir = dir.path().join("index");
    std::fs::create_dir_all(&index_dir).unwrap();
    let status = wilkes_core::types::IndexStatus {
        model_id: "m1".to_string(),
        engine: EmbeddingEngine::Candle,
        dimension: 128,
        indexed_files: 1,
        total_chunks: 10,
        built_at: Some(12345678),
        build_duration_ms: Some(1000),
        root_path: Some(dir.path().to_path_buf()),
        db_size_bytes: Some(1024),
    };
    std::fs::write(
        index_dir.join("status.json"),
        serde_json::to_string(&status).unwrap(),
    )
    .unwrap();

    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        settings_path,
        WorkerPaths::resolve(dir.path()),
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );

    // AppContext::new might have already called restore_state internally,
    // but let's be explicit and check if it sticks.
    ctx.clone().restore_state().await;

    let s = ctx.get_settings().await;
    assert_eq!(s.semantic.selected.model.0, "m1");
}

#[tokio::test]
async fn test_restore_state_open_index_fail() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    // Create a corrupted index path (a directory where a file should be)
    let index_path = data_dir.join("semantic_index.db");
    std::fs::create_dir(&index_path).unwrap();

    let settings = Settings {
        search_prefer_semantic: true,
        semantic: SemanticSettings {
            enabled: true,
            index_path: Some(index_path),
            selected: SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("m".to_string()),
                dimension: 384,
            },
            ..Default::default()
        },
        ..Default::default()
    };
    std::fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

    let (ctx, _rx, _loop) = AppContext::new(
        data_dir.clone(),
        settings_path,
        WorkerPaths::resolve(dir.path()),
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );

    ctx.clone().restore_state().await;

    let updated = ctx.get_settings().await;
    assert!(!updated.semantic.enabled);
}

#[test]
fn test_restore_state_needs_reset_on_none_db() {
    let mut settings = Settings::default();
    settings.semantic.enabled = true;
    assert!(AppContext::restore_state_needs_reset(&settings, None));

    settings.semantic.enabled = false;
    settings.semantic.index_path = Some(PathBuf::from("any"));
    assert!(AppContext::restore_state_needs_reset(&settings, None));

    settings.semantic.index_path = None;
    assert!(!AppContext::restore_state_needs_reset(&settings, None));
}

#[tokio::test]
async fn test_clear_restore_state_settings_direct() {
    let (_dir, ctx) = test_ctx();
    ctx.update_settings(serde_json::json!({ "semantic": { "enabled": true } }))
        .await
        .unwrap();
    assert!(ctx.settings().await.semantic.enabled);

    ctx.clear_restore_state_settings().await;
    assert!(!ctx.settings().await.semantic.enabled);
}
