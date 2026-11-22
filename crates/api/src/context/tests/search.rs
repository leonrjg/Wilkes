use super::*;

#[tokio::test]
async fn test_is_semantic_ready() {
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

    assert!(!ctx.is_semantic_ready());

    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
    *ctx.embedder.lock() = Some(embedder);
    let index = SemanticIndex::create(
        &ctx.data_dir,
        "semantic-ready",
        384,
        EmbeddingEngine::Candle,
        None,
    )
    .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));
    assert!(ctx.is_semantic_ready());
}

#[tokio::test]
async fn test_start_search_grep() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "body without the query terms").unwrap();
    let canonical_path = std::fs::canonicalize(&path).unwrap();
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
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": dir.path() }),
    )
    .await
    .unwrap();
    let identity = FileIdentity::for_path(&canonical_path).unwrap();
    ctx.metadata_cache()
        .unwrap()
        .lock()
        .unwrap()
        .upsert(
            &canonical_path,
            identity,
            &DocumentMetadata {
                title: Some("Composed Search Title".into()),
                ..DocumentMetadata::default()
            },
            MetadataSource::File,
        )
        .unwrap();

    let query = SearchQuery {
        pattern: "composed search title".to_string(),
        is_regex: false,
        case_sensitive: false,
        root: dir.path().to_path_buf(),
        max_results: 0,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 2,
        mode: SearchMode::Grep,
        scope: Default::default(),
        supported_extensions: vec![],
        collection_id: None,
        tag_ids: Vec::new(),
    };

    let mut handle = ctx.clone().start_search(query).await.unwrap();
    let result = handle.next().await.unwrap();
    assert_eq!(result.path, canonical_path);
    assert!(result.matches.is_empty());
    assert_eq!(result.field_matches.len(), 1);
    assert_eq!(
        result.field_matches[0].field,
        wilkes_core::types::SearchField::Title
    );
    assert!(handle.next().await.is_none());
    assert!(handle.finish().await.is_empty());
}

#[tokio::test]
async fn exact_search_setting_routes_the_text_index_and_reports_fallbacks() {
    let dir = tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let pdf = root.join("paper.pdf");
    // Intentionally not a valid PDF: successful matching proves the app
    // boundary supplied stored text instead of silently extracting live.
    std::fs::write(&pdf, "%PDF-1.7 opaque bytes").unwrap();

    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        root.clone(),
        root.join("settings.json"),
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
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({
            "last_directory": root,
            "grep_use_index": true
        }),
    )
    .await
    .unwrap();

    ctx.text_index()
        .unwrap()
        .write_text(
            &pdf,
            FileIdentity::for_path(&pdf).unwrap(),
            &wilkes_core::text_index::TextSource::current_native(),
            "alpha beta gamma",
            &wilkes_core::types::SourceMap {
                segments: vec![wilkes_core::types::SourceSegment {
                    text_range: ByteRange { start: 0, end: 16 },
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: None,
                    },
                    provenance: Default::default(),
                }],
            },
        )
        .unwrap();

    let query = SearchQuery {
        pattern: "beta".to_string(),
        is_regex: false,
        case_sensitive: true,
        root: root.clone(),
        max_results: 10,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 0,
        mode: SearchMode::Grep,
        scope: SearchScope::File { path: pdf.clone() },
        supported_extensions: Vec::new(),
        collection_id: None,
        tag_ids: Vec::new(),
    };

    let mut indexed_results = Vec::new();
    let indexed_stats = ctx
        .clone()
        .start_search(query.clone())
        .await
        .unwrap()
        .run(|result| {
            indexed_results.push(result);
            async { true }
        })
        .await;
    assert_eq!(indexed_results.len(), 1);
    assert_eq!(indexed_results[0].matches[0].matched_text, "beta");
    assert_eq!(indexed_stats.indexed_pdf_reads, 1);
    assert_eq!(indexed_stats.live_pdf_fallbacks, 0);
    assert_eq!(indexed_stats.index_unavailable_fallbacks, 0);
    assert!(indexed_stats.errors.is_empty());

    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "grep_use_index": false }),
    )
    .await
    .unwrap();
    let disabled_stats = ctx
        .clone()
        .start_search(query.clone())
        .await
        .unwrap()
        .run(|_| async { true })
        .await;
    assert_eq!(disabled_stats.indexed_pdf_reads, 0);
    assert_eq!(disabled_stats.live_pdf_fallbacks, 1);
    assert_eq!(disabled_stats.index_unavailable_fallbacks, 0);
    assert_eq!(disabled_stats.errors.len(), 1);

    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "grep_use_index": true }),
    )
    .await
    .unwrap();
    // A store that cannot be opened: every document is read live, and
    // the search says why.
    *ctx.text_index.lock() = None;
    for name in [
        "document_text.db",
        "document_text.db-wal",
        "document_text.db-shm",
    ] {
        let _ = std::fs::remove_file(ctx.data_dir.join(name));
    }
    std::fs::create_dir(ctx.data_dir.join("document_text.db")).unwrap();
    let unavailable_stats = ctx
        .clone()
        .start_search(query)
        .await
        .unwrap()
        .run(|_| async { true })
        .await;
    assert_eq!(unavailable_stats.indexed_pdf_reads, 0);
    assert_eq!(unavailable_stats.live_pdf_fallbacks, 1);
    assert_eq!(unavailable_stats.index_unavailable_fallbacks, 1);
    assert_eq!(unavailable_stats.errors.len(), 1);
}

/// The reason the document text index exists: a root nobody embedded is
/// read once, in the background, as soon as it joins the library, and
/// exact search over it then reads no PDF at all.
#[tokio::test]
async fn an_unembedded_root_is_read_once_and_exact_search_is_served_from_its_text() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let root = dir.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let pdf = root.join("book.pdf");
    write_text_pdf(&pdf, "A needle in the library");

    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        data_dir.clone(),
        data_dir.join("settings.json"),
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
    ctx.update_settings(serde_json::json!({ "last_directory": root }))
        .await
        .unwrap();
    assert!(!ctx.is_index_loaded(), "nothing here is embedded");

    let store = ctx.text_index().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while store.document_for_path(&pdf).unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the new root was never read into the text index"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let query = SearchQuery {
        pattern: "needle".to_string(),
        is_regex: false,
        case_sensitive: false,
        root: root.clone(),
        max_results: 10,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 0,
        mode: SearchMode::Grep,
        scope: SearchScope::All,
        supported_extensions: Vec::new(),
        collection_id: None,
        tag_ids: Vec::new(),
    };
    let mut results = Vec::new();
    let stats = ctx
        .clone()
        .start_search(query)
        .await
        .unwrap()
        .run(|result| {
            results.push(result);
            async { true }
        })
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matches[0].matched_text, "needle");
    assert!(matches!(
        results[0].matches[0].origin,
        SourceOrigin::PdfPage {
            page: 1,
            bbox: Some(_)
        }
    ));
    assert_eq!(stats.indexed_pdf_reads, 1);
    assert_eq!(stats.live_pdf_fallbacks, 0);
    assert!(stats.errors.is_empty(), "{:?}", stats.errors);

    // Turning the setting off stops the upkeep and the reads alike.
    ctx.update_settings(serde_json::json!({ "grep_use_index": false }))
        .await
        .unwrap();
    assert!(matches!(
        ctx.stored_text(&ctx.get_settings().await),
        StoredText::Disabled
    ));
}

#[test]
fn test_prepare_search_query_normalizes_file_scope() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("paper.txt"), "hello").unwrap();

    let query = SearchQuery {
        pattern: "hello".to_string(),
        is_regex: false,
        case_sensitive: false,
        root: dir.path().to_path_buf(),
        max_results: 0,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 2,
        mode: SearchMode::Grep,
        scope: SearchScope::File {
            path: PathBuf::from("paper.txt"),
        },
        supported_extensions: vec![],
        collection_id: None,
        tag_ids: Vec::new(),
    };

    let prepared = AppContext::prepare_search_query(
        query,
        vec!["txt".to_string()],
        &[dir.path().canonicalize().unwrap()],
    )
    .unwrap();
    assert_eq!(prepared.root, std::fs::canonicalize(dir.path()).unwrap());
    assert_eq!(
        prepared.scope,
        SearchScope::File {
            path: std::fs::canonicalize(dir.path().join("paper.txt")).unwrap()
        }
    );
}

#[test]
fn test_prepare_search_query_accepts_file_in_another_library_root() {
    let root = tempdir().unwrap();
    let other_root = tempdir().unwrap();
    let other_file = other_root.path().join("paper.txt");
    std::fs::write(&other_file, "hello").unwrap();

    let query = SearchQuery {
        pattern: "hello".to_string(),
        is_regex: false,
        case_sensitive: false,
        root: root.path().to_path_buf(),
        max_results: 0,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 2,
        mode: SearchMode::Grep,
        scope: SearchScope::File {
            path: other_file.clone(),
        },
        supported_extensions: vec![],
        collection_id: None,
        tag_ids: Vec::new(),
    };

    let prepared = AppContext::prepare_search_query(
        query,
        vec!["txt".to_string()],
        &[
            root.path().canonicalize().unwrap(),
            other_root.path().canonicalize().unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(
        prepared.scope,
        SearchScope::File {
            path: other_file.canonicalize().unwrap()
        }
    );
}

#[test]
fn test_prepare_search_query_rejects_file_outside_library() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_file = outside.path().join("paper.txt");
    std::fs::write(&outside_file, "hello").unwrap();

    let query = SearchQuery {
        pattern: "hello".to_string(),
        is_regex: false,
        case_sensitive: false,
        root: root.path().to_path_buf(),
        max_results: 0,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 2,
        mode: SearchMode::Grep,
        scope: SearchScope::File { path: outside_file },
        supported_extensions: vec![],
        collection_id: None,
        tag_ids: Vec::new(),
    };

    let err = AppContext::prepare_search_query(
        query,
        vec!["txt".to_string()],
        &[root.path().canonicalize().unwrap()],
    )
    .unwrap_err();
    assert!(err.contains("Search file is not in the library"));
}

#[test]
fn test_prepare_search_query_rejects_root_outside_library() {
    let library = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let query = SearchQuery {
        pattern: "secret".to_string(),
        is_regex: false,
        case_sensitive: false,
        root: outside.path().to_path_buf(),
        max_results: 0,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 2,
        mode: SearchMode::Grep,
        scope: SearchScope::Corpus,
        supported_extensions: vec![],
        collection_id: None,
        tag_ids: Vec::new(),
    };

    let err = AppContext::prepare_search_query(
        query,
        vec!["txt".to_string()],
        &[library.path().canonicalize().unwrap()],
    )
    .unwrap_err();
    assert!(err.contains("not in the library"));
}

#[test]
fn test_prepare_search_query_allows_nested_root_inside_library() {
    let library = tempdir().unwrap();
    let nested = library.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let query = SearchQuery {
        pattern: "allowed".to_string(),
        is_regex: false,
        case_sensitive: false,
        root: nested.clone(),
        max_results: 0,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 2,
        mode: SearchMode::Grep,
        scope: SearchScope::Corpus,
        supported_extensions: vec![],
        collection_id: None,
        tag_ids: Vec::new(),
    };

    let prepared = AppContext::prepare_search_query(
        query,
        vec!["txt".to_string()],
        &[library.path().canonicalize().unwrap()],
    )
    .unwrap();
    assert_eq!(prepared.root, nested.canonicalize().unwrap());
}

#[tokio::test]
async fn test_start_search_semantic_missing() {
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
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": dir.path() }),
    )
    .await
    .unwrap();

    let query = SearchQuery {
        pattern: "test".to_string(),
        is_regex: false,
        case_sensitive: false,
        root: dir.path().to_path_buf(),
        max_results: 0,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 2,
        mode: SearchMode::Semantic,
        scope: Default::default(),
        supported_extensions: vec![],
        collection_id: None,
        tag_ids: Vec::new(),
    };

    let res = ctx.clone().start_search(query).await;
    match res {
        Err(e) => assert!(e.contains("No semantic index found")),
        Ok(_) => panic!("Expected error but got Ok"),
    }
}

#[tokio::test]
async fn test_start_search_semantic_build_in_progress() {
    let dir = tempdir().unwrap();
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        WorkerPaths::resolve(dir.path()),
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": dir.path() }),
    )
    .await
    .unwrap();

    // Mock a task in progress
    let cancel = CancellationToken::new();
    let join = tokio::spawn(async {
        std::future::pending::<()>().await;
        Ok(())
    });
    *ctx.embed_task.lock() = Some(EmbedTaskHandle {
        operation: EmbedOperation::Build,
        cancel,
        cancel_flag: Arc::new(AtomicBool::new(false)),
        join: BuildHandle::new(join),
        settlement: Arc::new(BuildSettlement::new()),
    });

    let query = SearchQuery {
        mode: SearchMode::Semantic,
        root: dir.path().to_path_buf(),
        pattern: "test".to_string(),
        is_regex: false,
        case_sensitive: false,
        max_results: 10,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 0,
        scope: Default::default(),
        supported_extensions: vec![],
        collection_id: None,
        tag_ids: Vec::new(),
    };

    let res = ctx.start_search(query).await;
    match res {
        Err(e) => assert!(e.contains("Semantic index is currently being built")),
        Ok(_) => panic!("Expected error but got Ok"),
    }
}

#[tokio::test]
async fn test_related_documents_requires_current_semantic_root() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let (ctx, _rx, _loop) = AppContext::new(
        data_dir.clone(),
        dir.path().join("settings.json"),
        WorkerPaths::resolve(&data_dir),
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );

    let embedder = Arc::new(MockEmbedder {
        dimension: 2,
        ..MockEmbedder::default()
    });
    let model_id = embedder.model_id().to_string();
    *ctx.embedder.lock() = Some(embedder);
    let indexed_root = dir.path().join("indexed");
    let requested_root = dir.path().join("requested");
    std::fs::create_dir_all(&indexed_root).unwrap();
    std::fs::create_dir_all(&requested_root).unwrap();
    let requested_file = requested_root.join("source.txt");
    std::fs::write(&requested_file, "source").unwrap();
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": requested_root.clone() }),
    )
    .await
    .unwrap();
    let idx = SemanticIndex::create(
        &data_dir,
        &model_id,
        2,
        EmbeddingEngine::Candle,
        Some(&indexed_root),
    )
    .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(idx)));

    let err = ctx
        .clone()
        .related_documents(RelatedDocumentsQuery {
            root: requested_root,
            path: requested_file,
            scope: SearchScope::Corpus,
            limit: Some(8),
            collection_id: None,
        })
        .await
        .unwrap_err();

    assert!(err.contains("Semantic index is not ready"));
}

#[tokio::test]
async fn test_related_documents_rejects_root_outside_library() {
    let (dir, ctx) = test_ctx();
    let library = dir.path().join("library");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let source = outside.join("source.txt");
    std::fs::write(&source, "source").unwrap();
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": library }),
    )
    .await
    .unwrap();

    let err = ctx
        .related_documents(RelatedDocumentsQuery {
            root: outside,
            path: source,
            scope: SearchScope::Corpus,
            limit: None,
            collection_id: None,
        })
        .await
        .unwrap_err();

    assert!(err.contains("not in the library"));
}

#[tokio::test]
async fn test_related_documents_returns_index_results() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let root = dir.path().join("root");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("source.txt");
    let related = root.join("related.txt");
    std::fs::write(&source, "source").unwrap();
    std::fs::write(&related, "related").unwrap();
    let (ctx, _rx, _loop) = AppContext::new(
        data_dir.clone(),
        dir.path().join("settings.json"),
        WorkerPaths::resolve(&data_dir),
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );

    let embedder = Arc::new(MockEmbedder {
        dimension: 2,
        ..MockEmbedder::default()
    });
    let model_id = embedder.model_id().to_string();
    *ctx.embedder.lock() = Some(embedder);
    let mut idx = SemanticIndex::create(
        &data_dir,
        &model_id,
        2,
        EmbeddingEngine::Candle,
        Some(&root),
    )
    .unwrap();
    let chunk = |path: &Path, text: &str| wilkes_core::embed::index::chunk::Chunk {
        file_path: path.to_path_buf(),
        text: text.to_string(),
        byte_range: ByteRange {
            start: 0,
            end: text.len(),
        },
        origin: SourceOrigin::TextFile { line: 1, col: 1 },
    };
    idx.write_file(wilkes_core::embed::index::db::PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: source.clone(),
        chunks: vec![(chunk(&source, "source"), vec![1.0, 0.0])],
    })
    .unwrap();
    idx.write_file(wilkes_core::embed::index::db::PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: related.clone(),
        chunks: vec![(chunk(&related, "related"), vec![0.9, 0.1])],
    })
    .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(idx)));
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": root.clone() }),
    )
    .await
    .unwrap();

    let docs = ctx
        .related_documents(RelatedDocumentsQuery {
            root,
            path: source,
            scope: SearchScope::Corpus,
            limit: Some(8),
            collection_id: None,
        })
        .await
        .unwrap();

    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].entry.path, std::fs::canonicalize(related).unwrap());
}

#[tokio::test]
async fn tidy_folder_suggests_a_home_for_each_indexed_document() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let inbox = dir.path().join("inbox");
    let library = dir.path().join("library");
    let recommendation = library.join("Recommendation");
    let law = library.join("Law");
    let shelf = library.join("Shelf");
    let inner = shelf.join("Inner");
    for path in [&data_dir, &inbox, &recommendation, &law, &inner] {
        std::fs::create_dir_all(path).unwrap();
    }
    let write = |path: PathBuf| {
        std::fs::write(&path, "text").unwrap();
        std::fs::canonicalize(path).unwrap()
    };
    let recsys = write(inbox.join("recsys.txt"));
    let legal = write(inbox.join("legal.txt"));
    let unindexed = write(inbox.join("unindexed.txt"));
    let precedent = write(law.join("precedent.txt"));
    let nested = write(inner.join("nested.txt"));
    let (ctx, _rx, _loop) = AppContext::new(
        data_dir.clone(),
        dir.path().join("settings.json"),
        WorkerPaths::resolve(&data_dir),
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );
    *ctx.embedder.lock() = Some(Arc::new(FolderNameEmbedder));
    let mut idx = SemanticIndex::create(
        &data_dir,
        "folder-name-test",
        3,
        EmbeddingEngine::Candle,
        Some(&inbox),
    )
    .unwrap();
    let prepared = |path: &Path, vector: Vec<f32>| wilkes_core::embed::index::db::PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: path.to_path_buf(),
        chunks: vec![(
            wilkes_core::embed::index::chunk::Chunk {
                file_path: path.to_path_buf(),
                text: "text".to_string(),
                byte_range: ByteRange { start: 0, end: 4 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vector,
        )],
    };
    idx.write_file(prepared(&recsys, vec![1.0, 0.1, 0.0]))
        .unwrap();
    idx.write_file(prepared(&legal, vec![0.1, 1.0, 0.0]))
        .unwrap();
    idx.activate_root(&library).unwrap();
    idx.write_file(prepared(&precedent, vec![0.0, 1.0, 0.1]))
        .unwrap();
    idx.write_file(prepared(&nested, vec![0.0, 0.1, 1.0]))
        .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(idx)));
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": inbox.clone(), "favorites": [library.clone()] }),
    )
    .await
    .unwrap();

    let plan = ctx.clone().tidy_folder(inbox.clone()).await.unwrap();

    // The root holding only folders, and the folder holding only "Inner",
    // are shelves; the folder being tidied is not its own destination.
    let destinations: Vec<(&Path, usize, usize)> = plan
        .destinations
        .iter()
        .map(|d| (d.path.as_path(), d.documents, d.indexed_documents))
        .collect();
    let law = std::fs::canonicalize(&law).unwrap();
    let recommendation = std::fs::canonicalize(&recommendation).unwrap();
    let inner = std::fs::canonicalize(&inner).unwrap();
    assert_eq!(
        destinations,
        vec![
            (law.as_path(), 1, 1),
            (recommendation.as_path(), 0, 0),
            (inner.as_path(), 1, 1),
        ]
    );
    let suggested: Vec<(&Path, &Path)> = plan
        .suggestions
        .iter()
        .map(|s| (s.entry.path.as_path(), s.ranking[0].destination.as_path()))
        .collect();
    assert_eq!(
        suggested,
        vec![
            (legal.as_path(), law.as_path()),
            (recsys.as_path(), recommendation.as_path()),
        ]
    );
    assert!(plan.suggestions.iter().all(|s| s.ranking.len() == 3));
    assert_eq!(
        plan.unindexed
            .iter()
            .map(|e| e.path.clone())
            .collect::<Vec<_>>(),
        vec![unindexed]
    );

    let err = ctx.clone().tidy_folder(recommendation).await.unwrap_err();
    assert!(err.contains("no documents directly in"), "{err}");
    let err = ctx
        .clone()
        .tidy_folder(dir.path().to_path_buf())
        .await
        .unwrap_err();
    assert!(err.contains("not in the library"), "{err}");
}

#[tokio::test]
async fn test_start_search_semantic_root_mismatch_does_not_build_an_index() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let settings_path = dir.path().join("settings.json");
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: events.clone(),
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

    let embedder = Arc::new(MockEmbedder::default());
    let model_id = embedder.model_id().to_string();
    let dimension = embedder.dimension();

    // Mock an embedder and index
    *ctx.embedder.lock() = Some(embedder);

    // Create an index on disk so we can open it
    let root1 = dir.path().join("root1");
    std::fs::create_dir_all(&root1).unwrap();
    let idx = SemanticIndex::create(
        &data_dir,
        &model_id,
        dimension,
        EmbeddingEngine::Candle,
        Some(&root1),
    )
    .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(idx)));

    // Search in a different root
    let root2 = dir.path().join("root2");
    std::fs::create_dir_all(&root2).unwrap();
    std::fs::write(root2.join("file.txt"), "hello").unwrap();
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": root2.clone() }),
    )
    .await
    .unwrap();
    let query = SearchQuery {
        pattern: "test".to_string(),
        is_regex: false,
        case_sensitive: false,
        root: root2.clone(),
        max_results: 0,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 2,
        mode: SearchMode::Semantic,
        scope: Default::default(),
        supported_extensions: vec![],
        collection_id: None,
        tag_ids: Vec::new(),
    };

    // The search says the root is not indexed and stops there. It does not
    // start indexing it: that is hours of inference, and a search is not
    // where the user asked for it.
    let err = match ctx.clone().start_search(query).await {
        Err(err) => err,
        Ok(_) => panic!("expected semantic root mismatch"),
    };
    assert!(err.contains("Semantic index is not ready"));
    assert!(
        !err.contains("try again"),
        "a search must not promise an index it is not building: {err}"
    );

    assert_no_reindex(&events).await;
}

#[tokio::test]
async fn test_start_search_semantic_without_indexed_root_does_not_build_one() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let settings_path = dir.path().join("settings.json");
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: events.clone(),
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

    let embedder = Arc::new(MockEmbedder::default());
    let model_id = embedder.model_id().to_string();
    let dimension = embedder.dimension();
    *ctx.embedder.lock() = Some(embedder);

    let idx = SemanticIndex::create(
        &data_dir,
        &model_id,
        dimension,
        EmbeddingEngine::Candle,
        None,
    )
    .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(idx)));

    let root2 = dir.path().join("root2");
    std::fs::create_dir_all(&root2).unwrap();
    std::fs::write(root2.join("file.txt"), "hello").unwrap();
    crate::commands::settings::update_settings(
        &ctx.settings_path,
        serde_json::json!({ "last_directory": root2.clone() }),
    )
    .await
    .unwrap();
    let query = SearchQuery {
        pattern: "test".to_string(),
        is_regex: false,
        case_sensitive: false,
        root: root2.clone(),
        max_results: 0,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 2,
        mode: SearchMode::Semantic,
        scope: Default::default(),
        supported_extensions: vec![],
        collection_id: None,
        tag_ids: Vec::new(),
    };

    let err = match ctx.clone().start_search(query).await {
        Err(err) => err,
        Ok(_) => panic!("expected semantic root mismatch"),
    };
    assert!(err.contains("Semantic index is not ready"));

    assert_no_reindex(&events).await;
}
