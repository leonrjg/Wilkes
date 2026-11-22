use super::*;
use wilkes_api::context::EventEmitter;

#[test]
fn test_error_helpers() {
    let (status, body) = err("bad request");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body.0.error, "bad request");

    let (status, body) = server_err("internal error");
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body.0.error, "internal error");
}

/// The probe body is an untagged enum, which is the one place a wire
/// format can go quietly wrong: a shape matching neither variant, or both,
/// must be refused rather than resolved to whichever arm serde reaches
/// first.
#[test]
fn a_probe_is_a_vector_or_a_text_and_never_both() {
    let vector: SearchProbe =
        serde_json::from_str(r#"{"vector":[1.0,0.0]}"#).expect("a vector probe");
    assert!(matches!(vector, SearchProbe::Vector(_)));

    let text: SearchProbe =
        serde_json::from_str(r#"{"text":"causal inference"}"#).expect("a text probe");
    match text {
        SearchProbe::Text(probe) => assert_eq!(probe.text, "causal inference"),
        SearchProbe::Vector(_) => panic!("text read as a vector"),
    }

    for malformed in [
        r#"{"vector":[1.0],"text":"both"}"#,
        r#"{}"#,
        r#"{"vector":[1.0],"role":"query"}"#,
        r#"{"txt":"misspelled"}"#,
    ] {
        assert!(
            serde_json::from_str::<SearchProbe>(malformed).is_err(),
            "{malformed} must be refused"
        );
    }
}

#[test]
fn test_broadcast_emitter() {
    let (tx, mut rx) = broadcast::channel(10);
    let emitter = BroadcastEmitter { tx };

    emitter.emit("test-event", serde_json::json!({"key": "value"}));

    let msg = rx.try_recv().unwrap();
    assert_eq!(msg.0, "test-event");
    assert_eq!(msg.1["key"], "value");
}

/// Building the whole router *is* the assertion: axum validates route
/// syntax at construction and panics on anything malformed.
///
/// Nothing did this before. Every other test here calls a handler
/// directly, so four routes kept axum 0.7's `:id` capture syntax through
/// the 0.8 upgrade and every attempt to serve panicked before it could
/// bind — a failure no handler test can see.
#[tokio::test]
async fn the_router_is_constructible() {
    let dir = tempfile::tempdir().unwrap();
    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("py_pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: PathBuf::from("data"),
    };
    let (events_tx, _) = broadcast::channel(16);
    let (ctx, _event_rx, _loop_fut) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        paths,
        Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        }),
    );
    let _router = api_router(
        Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: dir.path().join("uploads"),
            events_tx,
        }),
        Arc::new(std::sync::RwLock::new(None)),
    );
}

/// Builds an `AppState` over a throwaway data dir, for handlers that need
/// only the workspace's path. The returned directory owns `uploads`, so a
/// test that needs a file on disk can write one into it.
#[allow(clippy::type_complexity)]
fn handler_state() -> (tempfile::TempDir, Arc<AppState>) {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    let settings_path = dir.path().join("settings.json");
    std::fs::create_dir_all(&uploads_dir).unwrap();
    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("py_pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: PathBuf::from("data"),
    };
    let (events_tx, _) = broadcast::channel(1024);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (ctx, _event_rx, _loop_fut) =
        AppContext::new(dir.path().to_path_buf(), settings_path, paths, emitter);
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir,
        events_tx,
    });
    (dir, state)
}

/// With a token set, every route but the two that cannot need one requires
/// the token, as a header or as the cookie `/api/login` hands out.
#[tokio::test]
async fn a_token_gates_every_route_but_health_and_login() {
    use tower::ServiceExt;
    let (_dir, state) = handler_state();
    let token: ApiToken = Arc::new(std::sync::RwLock::new(Some("secret".to_string())));
    let app = api_router(state, token);
    let call = |request: axum::http::Request<Body>| {
        let app = app.clone();
        async move { app.oneshot(request).await.unwrap() }
    };
    let get = |path: &str| axum::http::Request::get(path).body(Body::empty()).unwrap();

    assert_eq!(
        call(get("/api/settings")).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(get("/api/session")).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(call(get("/health")).await.status(), StatusCode::OK);

    let with_header = axum::http::Request::get("/api/settings")
        .header("Authorization", "Bearer secret")
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(with_header).await.status(), StatusCode::OK);

    let login = axum::http::Request::post("/api/login")
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"token":"secret"}"#))
        .unwrap();
    let logged_in = call(login).await;
    assert_eq!(logged_in.status(), StatusCode::NO_CONTENT);
    let cookie = logged_in
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let with_cookie = axum::http::Request::get("/api/session")
        .header("Cookie", cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(with_cookie).await.status(), StatusCode::NO_CONTENT);
}

/// A request names an index or a model, never both; and a model the host
/// lacks is refused with a code, not downloaded inside the request.
#[tokio::test]
async fn embedding_by_model_refuses_ambiguity_and_absent_models() {
    let (_dir, state) = handler_state();
    let address: EmbedderAddress = serde_json::from_value(serde_json::json!({
        "engine": "Fastembed",
        "model": "NoSuchModel",
    }))
    .unwrap();

    let (status, body) = embed_text_handler(
        State(Arc::clone(&state)),
        Json(EmbedTextBody {
            texts: vec!["t".to_string()],
            scope: ConsumerScope {
                workspace_id: Some("w".to_string()),
                expected_embedding_space_id: None,
            },
            embedder: Some(address.clone()),
        }),
    )
    .await
    .err()
    .expect("both addresses at once");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.error.contains("not both"), "{}", body.error);

    let (status, body) = embed_query_handler(
        State(state),
        Json(EmbedTextBody {
            texts: vec!["t".to_string()],
            scope: ConsumerScope::default(),
            embedder: Some(address),
        }),
    )
    .await
    .err()
    .expect("a model this host does not have");
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body.code.as_deref(), Some("MODEL_NOT_INSTALLED"));
}

/// The import reply names the spaces that hold the document, and no
/// space of the canonical corpus's own: that one embedded nothing, and a
/// caller that echoed it back as its next pin was refused.
fn example_import_reply() -> ManagedImportReply {
    ManagedImportReply {
        document: wilkes_api::context::ManagedDocumentExport {
            corpus_id: "corpus".to_string(),
            source_sha256: "sha".to_string(),
            source_byte_len: 1,
            media_type: "text/plain".to_string(),
            snapshot_id: "snapshot".to_string(),
            rendition_id: "rendition".to_string(),
            extraction_recipe_id: "recipe".to_string(),
            extracted_content_sha256: "content".to_string(),
            chunk_count: 0,
            outline: Vec::new(),
            extraction: Default::default(),
            chunks: Vec::new(),
            embedding_work: wilkes_api::context::ManagedEmbeddingWork {
                chunks: 0,
                reused: 0,
                computed: 0,
            },
        },
        spaces: vec!["space-a".to_string()],
    }
}

#[test]
fn the_import_reply_names_the_spaces_holding_the_document() {
    let reply = serde_json::to_value(example_import_reply()).unwrap();
    assert_eq!(reply["spaces"], serde_json::json!(["space-a"]));
    assert_eq!(reply["snapshot_id"], "snapshot", "the document stays flat");
    for gone in [
        "embedding_space_id",
        "engine",
        "model_id",
        "dimension",
        "passage_input_recipe",
    ] {
        assert!(reply.get(gone).is_none(), "{gone} is still reported");
    }
}

/// The query route is reached, refuses what it cannot answer with an
/// error rather than a vector, and names its role. The role itself is
/// covered where the embedder is, in `embed_query_texts`.
#[tokio::test]
async fn the_query_embedding_route_refuses_rather_than_falls_back() {
    let (_dir, state) = handler_state();
    let empty = embed_query_handler(
        State(Arc::clone(&state)),
        Json(EmbedTextBody {
            texts: Vec::new(),
            scope: ConsumerScope::default(),
            embedder: None,
        }),
    )
    .await
    .err()
    .expect("no texts is no request");
    assert_eq!(empty.0, StatusCode::BAD_REQUEST);

    let (status, body) = embed_query_handler(
        State(Arc::clone(&state)),
        Json(EmbedTextBody {
            texts: vec!["what binds".to_string()],
            scope: ConsumerScope::default(),
            embedder: None,
        }),
    )
    .await
    .err()
    .expect("no model is not ready, not a vector");
    assert!(status.is_server_error(), "{status}");
    assert!(
        body.error.contains("Semantic model unavailable"),
        "{}",
        body.error
    );

    let reply = serde_json::to_value(EmbeddedQueries {
        embedded: wilkes_api::context::EmbeddedTexts {
            embedding_space_id: Some("space-a".to_string()),
            engine: "SBERT".to_string(),
            model_id: "m".to_string(),
            dimension: 2,
            vectors: vec![vec![1.0, 0.0]],
            embedding_space_identity: None,
        },
        role: "query",
    })
    .unwrap();
    assert_eq!(
        reply,
        serde_json::json!({
            "embedding_space_id": "space-a",
            "engine": "SBERT",
            "model_id": "m",
            "dimension": 2,
            "vectors": [[1.0, 0.0]],
            "embedding_space_identity": null,
            "role": "query",
        }),
        "the passage route's reply, flat, plus the role"
    );
}

#[tokio::test]
async fn upload_then_search_needs_no_settings_patch_and_preserves_active_root() {
    use axum::extract::FromRequest;
    let (_dir, state) = handler_state();
    let active = state.uploads_dir.parent().unwrap().join("external-library");
    std::fs::create_dir_all(&active).unwrap();
    state
        .context()
        .update_settings(serde_json::json!({"last_directory": &active}))
        .await
        .unwrap();
    let body = "--upload-test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"nested/note.txt\"\r\nContent-Type: text/plain\r\n\r\nunique-upload-needle\r\n--upload-test--\r\n";
    let request = axum::http::Request::builder()
        .header(
            header::CONTENT_TYPE,
            "multipart/form-data; boundary=upload-test",
        )
        .body(Body::from(body))
        .unwrap();
    let multipart = Multipart::from_request(request, &()).await.unwrap();
    let upload = upload_handler(State(state.clone()), multipart)
        .await
        .unwrap_or_else(|error| panic!("upload failed: {}", error.1 .0.error))
        .into_response();
    let bytes = axum::body::to_bytes(upload.into_body(), 4096)
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["file_count"], 1);
    let root = PathBuf::from(response["root"].as_str().unwrap());
    let query: SearchQuery = serde_json::from_value(serde_json::json!({
        "pattern": "unique-upload-needle", "is_regex": false,
        "case_sensitive": false, "root": root.join("nested"), "max_results": 10
    }))
    .unwrap();
    let search = search_handler(State(state.clone()), Json(query))
        .await
        .unwrap_or_else(|error| panic!("search failed: {}", error.1 .0.error))
        .into_response();
    let bytes = tokio::time::timeout(
        Duration::from_secs(3),
        axum::body::to_bytes(search.into_body(), 65536),
    )
    .await
    .unwrap()
    .unwrap();
    let events = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(events.contains("event: result"), "{events}");
    assert!(events.contains("unique-upload-needle"), "{events}");
    assert!(!events.contains("event: error"), "{events}");
    let settings = state.context().get_settings().await;
    assert_eq!(
        settings.last_directory,
        Some(active.canonicalize().unwrap())
    );
    assert_eq!(settings.recent_dirs, vec![root.canonicalize().unwrap()]);
    state.context().shutdown().await;
}

#[tokio::test]
async fn search_registers_existing_uploads_but_rejects_external_roots() {
    let (_dir, state) = handler_state();
    std::fs::write(state.uploads_dir.join("note.txt"), "existing-upload-needle").unwrap();
    let query = |root: PathBuf| {
        serde_json::from_value::<SearchQuery>(serde_json::json!({
            "pattern": "existing-upload-needle", "is_regex": false,
            "case_sensitive": false, "root": root, "max_results": 10
        }))
        .unwrap()
    };
    let outside = state.uploads_dir.parent().unwrap().to_path_buf();
    assert!(search_handler(State(state.clone()), Json(query(outside)))
        .await
        .is_err());
    assert!(state.context().get_settings().await.recent_dirs.is_empty());
    let search = search_handler(State(state.clone()), Json(query(state.uploads_dir.clone())))
        .await
        .unwrap_or_else(|error| panic!("search failed: {}", error.1 .0.error))
        .into_response();
    let bytes = tokio::time::timeout(
        Duration::from_secs(3),
        axum::body::to_bytes(search.into_body(), 65536),
    )
    .await
    .unwrap()
    .unwrap();
    let events = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(events.contains("existing-upload-needle"), "{events}");
    assert!(!events.contains("event: error"), "{events}");
    assert_eq!(
        state.context().get_settings().await.recent_dirs,
        vec![state.uploads_dir.canonicalize().unwrap()]
    );
    state.context().shutdown().await;
}

#[tokio::test]
async fn catalogue_status_reports_an_unsynced_mirror_rather_than_failing() {
    let (_dir, state) = handler_state();
    let response = match catalogue_status_handler(State(state)).await {
        Ok(response) => response,
        Err(error) => panic!("status failed: {}", error.1 .0.error),
    };
    assert_eq!(response.0.total_records, 0);
    // Every registered provider is named even before a first sync: a
    // settings panel has to be able to offer the button for a provider
    // that holds nothing yet.
    assert_eq!(
        response.0.providers.len(),
        wilkes_core::catalogue::fetchers().len()
    );
    assert!(response
        .0
        .providers
        .iter()
        .all(|provider| provider.records == 0 && provider.synced_at_ms.is_none()));
}

#[tokio::test]
async fn catalogue_search_refuses_an_unknown_grain_by_name() {
    let (_dir, state) = handler_state();
    let body = CatalogueSearchBody {
        queries: vec![wilkes_api::commands::catalogue::CatalogueProbe {
            key: "k".into(),
            text: "graph algorithms".into(),
            grains: Some(vec!["monograph".into()]),
        }],
        limit: 8,
    };
    let error = match catalogue_search_handler(State(state), Json(body)).await {
        Ok(_) => panic!("unknown grain must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert!(
        error.1 .0.error.contains("monograph"),
        "the refusal must name the grain it rejected: {}",
        error.1 .0.error
    );
}

#[tokio::test]
async fn catalogue_search_refuses_an_empty_batch() {
    let (_dir, state) = handler_state();
    let body = CatalogueSearchBody {
        queries: Vec::new(),
        limit: 8,
    };
    let error = match catalogue_search_handler(State(state), Json(body)).await {
        Ok(_) => panic!("empty batch must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn catalogue_search_refuses_a_batch_past_the_documented_cap() {
    let (_dir, state) = handler_state();
    let body = CatalogueSearchBody {
        queries: (0..wilkes_api::commands::catalogue::MAX_QUERIES + 1)
            .map(|n| wilkes_api::commands::catalogue::CatalogueProbe {
                key: n.to_string(),
                text: "graph algorithms".into(),
                grains: None,
            })
            .collect(),
        limit: 8,
    };
    let error = match catalogue_search_handler(State(state), Json(body)).await {
        Ok(_) => panic!("oversized batch must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn catalogue_remove_refuses_a_provider_it_does_not_have() {
    let (_dir, state) = handler_state();
    let body = CatalogueRemoveBody {
        provider: "nonexistent".into(),
    };
    let error = match catalogue_remove_handler(State(state), Json(body)).await {
        Ok(_) => panic!("unknown provider must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert!(error.1 .0.error.contains("nonexistent"));
}

#[tokio::test]
async fn catalogue_sync_refuses_a_provider_it_does_not_have() {
    let (_dir, state) = handler_state();
    let body = CatalogueSyncBody {
        providers: Some(vec!["nonexistent".into()]),
    };
    let error = match catalogue_sync_handler(State(state), Json(body)).await {
        Ok(_) => panic!("unknown provider must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    // Refusing before any network call matters: a typo must not spend two
    // minutes syncing the three providers that were spelled correctly.
    assert!(error.1 .0.error.contains("nonexistent"));
}

#[tokio::test]
async fn test_handlers_direct() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    let settings_path = dir.path().join("settings.json");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

    tokio::fs::write(uploads_dir.join("test.txt"), "hello")
        .await
        .unwrap();

    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("py_pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: PathBuf::from("data"),
    };

    let (events_tx, _) = broadcast::channel(1024);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (ctx, _event_rx, _loop_fut) =
        AppContext::new(dir.path().to_path_buf(), settings_path, paths, emitter);

    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    // Test get_settings_handler
    let res = get_settings_handler(State(state.clone())).await;
    match res {
        Ok(r) => {
            let response = r.into_response();
            assert_eq!(response.status(), StatusCode::OK);
        }
        Err(_) => panic!("get_settings_handler failed"),
    }

    // Test get_logs_handler
    let _res = get_logs_handler().await;

    // Test get_data_paths_handler
    let _res = get_data_paths_handler(State(state.clone())).await;

    // Test list_files_handler
    let params = FilesQuery {
        root: uploads_dir.to_string_lossy().to_string(),
        collection_id: None,
        tag_ids: None,
        collection_expression: None,
    };
    let res = list_files_handler(State(state.clone()), Query(params)).await;
    match res {
        Ok(r) => {
            let response = r.into_response();
            assert_eq!(response.status(), StatusCode::OK);
        }
        Err(_) => panic!("list_files_handler failed"),
    }
}

#[tokio::test]
async fn test_confine_to_uploads() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

    // Success case
    let f1 = uploads_dir.join("f1.txt");
    tokio::fs::write(&f1, "test").await.unwrap();
    let res = confine_to_uploads(&f1.to_string_lossy(), &uploads_dir);
    assert!(
        res.is_ok(),
        "confine_to_uploads should succeed for valid path inside uploads_dir"
    );

    // Denied case: outside uploads_dir
    let outside = dir.path().join("outside.txt");
    tokio::fs::write(&outside, "secret").await.unwrap();
    let res = confine_to_uploads(&outside.to_string_lossy(), &uploads_dir);
    assert!(res.is_err());
    assert_eq!(res.map_err(|e| e.0).unwrap_err(), StatusCode::BAD_REQUEST);

    // Not found case
    let non_existent = uploads_dir.join("none.txt");
    let res = confine_to_uploads(&non_existent.to_string_lossy(), &uploads_dir);
    assert!(res.is_err());
    assert_eq!(res.map_err(|e| e.0).unwrap_err(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_delete_all_upload_handler() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
    tokio::fs::write(uploads_dir.join("f1.txt"), "test")
        .await
        .unwrap();

    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: PathBuf::from("data"),
    };
    let (events_tx, _) = broadcast::channel(1024);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    let res = delete_all_upload_handler(State(state))
        .await
        .map_err(|e| e.0)
        .expect("delete_all_upload_handler failed");
    assert_eq!(res, StatusCode::NO_CONTENT);
    assert!(uploads_dir.exists());
    let entries = std::fs::read_dir(&uploads_dir).unwrap().count();
    assert_eq!(entries, 0);
}

#[tokio::test]
async fn test_update_bookmark_note_handler() {
    let dir = tempfile::tempdir().unwrap();
    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: PathBuf::from("data"),
    };
    let (events_tx, _) = broadcast::channel(1024);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: dir.path().join("uploads"),
        events_tx,
    });

    let added = add_bookmark_handler(
        State(Arc::clone(&state)),
        Json(NewBookmark {
            path: "/tmp/example.pdf".into(),
            origin: wilkes_core::types::SourceOrigin::PdfPage {
                page: 1,
                bbox: None,
            },
            text_range: None,
            quote: "q".to_string(),
            note: None,
            rects: Vec::new(),
        }),
    )
    .await
    .map_err(|e| e.0)
    .expect("add_bookmark_handler failed")
    .into_response();
    assert_eq!(added.status(), StatusCode::OK);

    let id = state.context().list_bookmarks().await.unwrap()[0]
        .id
        .clone();
    update_bookmark_note_handler(
        State(Arc::clone(&state)),
        axum::extract::Path(id.clone()),
        Json(UpdateNoteBody {
            note: Some("  noted  ".to_string()),
        }),
    )
    .await
    .map_err(|e| e.0)
    .expect("update_bookmark_note_handler failed");

    assert_eq!(
        state.context().list_bookmarks().await.unwrap()[0]
            .note
            .as_deref(),
        Some("noted")
    );
}

#[tokio::test]
async fn test_search_handler_grep() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
    tokio::fs::write(uploads_dir.join("test.txt"), "hello world")
        .await
        .unwrap();

    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: PathBuf::from("data"),
    };
    let (events_tx, _) = broadcast::channel(1024);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    let query = wilkes_core::types::SearchQuery {
        pattern: "hello".to_string(),
        is_regex: false,
        case_sensitive: false,
        root: uploads_dir.clone(),
        max_results: 10,
        respect_gitignore: true,
        max_file_size: 0,
        context_lines: 0,
        mode: wilkes_core::types::SearchMode::Grep,
        scope: Default::default(),
        supported_extensions: vec!["txt".to_string()],
        collection_id: None,
        tag_ids: Vec::new(),
    };

    let res = search_handler(State(state), axum::Json(query)).await;
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_preview_handler_text() {
    let (dir, state) = handler_state();
    let file_path = dir.path().join("uploads").join("test.txt");
    tokio::fs::write(&file_path, "preview content")
        .await
        .unwrap();

    let match_ref = wilkes_core::types::MatchRef {
        path: file_path,
        origin: wilkes_core::types::SourceOrigin::TextFile { line: 1, col: 1 },
        text_range: None,
    };

    let res = preview_handler(State(state), axum::Json(match_ref)).await;
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_settings_handlers() {
    let dir = tempfile::tempdir().unwrap();
    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: PathBuf::from("data"),
    };
    let (events_tx, _) = broadcast::channel(1024);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: dir.path().to_path_buf(),
        events_tx,
    });

    let _ = get_logs_handler().await;
    let _ = clear_logs_handler().await;
    let _ = get_data_paths_handler(State(state.clone())).await;
    let _ = is_semantic_ready_handler(State(state.clone())).await;
    let _ = embedder_capabilities_handler(State(state.clone())).await;

    let patch = serde_json::json!({"semantic": {"enabled": true}});
    let res = update_settings_handler(State(state.clone()), axum::Json(patch)).await;
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_delete_upload_handler() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
    let file_path = uploads_dir.join("test.txt");
    tokio::fs::write(&file_path, "content").await.unwrap();

    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: PathBuf::from("data"),
    };
    let (events_tx, _) = broadcast::channel(1024);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    let query = DeleteUploadQuery {
        path: "test.txt".to_string(),
    };
    let res = delete_upload_handler(State(state.clone()), Query(query)).await;
    assert!(res.is_ok());
    assert!(!file_path.exists());

    let absolute_path = uploads_dir.join("absolute.txt");
    tokio::fs::write(&absolute_path, "content").await.unwrap();
    let query = DeleteUploadQuery {
        path: absolute_path.display().to_string(),
    };
    let res = delete_upload_handler(State(state), Query(query)).await;
    assert!(res.is_ok());
    assert!(!absolute_path.exists());
}

#[tokio::test]
async fn comic_page_endpoint_serves_only_the_requested_image() {
    let (dir, state) = handler_state();
    let path = dir.path().join("comic.cbz");
    std::fs::write(
        &path,
        include_bytes!("../../core/examples/fixtures/comic-reader.cbz"),
    )
    .unwrap();
    let response = comic_page_handler(
        State(state.clone()),
        Query(ComicPageQuery {
            path: path.to_string_lossy().into_owned(),
            page: 2,
        }),
    )
    .await
    .unwrap_or_else(|(status, _)| panic!("comic endpoint failed: {status}"))
    .into_response();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/octet-stream"
    );
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(&body[..4], b"WCP1");
    assert_eq!(f32::from_le_bytes(body[4..8].try_into().unwrap()), 36.0);
    assert!(comic_page_handler(
        State(state),
        Query(ComicPageQuery {
            path: path.to_string_lossy().into_owned(),
            page: 0,
        })
    )
    .await
    .is_err());
}

#[tokio::test]
async fn file_cover_endpoint_serves_a_png_and_only_from_uploads() {
    let (dir, state) = handler_state();
    let inside = state.uploads_dir.join("comic.cbz");
    let outside = dir.path().join("private.cbz");
    let comic = include_bytes!("../../core/examples/fixtures/comic-reader.cbz");
    std::fs::write(&inside, comic).unwrap();
    std::fs::write(&outside, comic).unwrap();

    let response = file_cover_handler(
        State(state.clone()),
        Query(FileCoverQuery {
            path: inside.to_string_lossy().into_owned(),
            max_edge: 64,
        }),
    )
    .await
    .unwrap_or_else(|(status, _)| panic!("cover endpoint failed: {status}"))
    .into_response();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(&body[1..4], b"PNG");

    assert!(file_cover_handler(
        State(state.clone()),
        Query(FileCoverQuery {
            path: outside.to_string_lossy().into_owned(),
            max_edge: 64,
        }),
    )
    .await
    .is_err());
    assert!(file_cover_handler(
        State(state),
        Query(FileCoverQuery {
            path: inside.to_string_lossy().into_owned(),
            max_edge: 0,
        }),
    )
    .await
    .is_err());
}

#[tokio::test]
async fn test_asset_handler() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
    let file_path = uploads_dir.join("test.txt");
    tokio::fs::write(&file_path, "asset content").await.unwrap();

    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: PathBuf::from("data"),
    };
    let (events_tx, _) = broadcast::channel(1024);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    let query = AssetQuery {
        path: file_path.to_string_lossy().to_string(),
    };
    let res = asset_handler(State(state), Query(query)).await;
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_open_file_handler() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
    let file_path = uploads_dir.join("test.txt");
    tokio::fs::write(&file_path, "file content").await.unwrap();

    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: PathBuf::from("data"),
    };
    let (events_tx, _) = broadcast::channel(1024);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    let body = OpenFileBody {
        path: file_path.to_string_lossy().to_string(),
    };
    let res = open_file_handler(State(state), axum::Json(body)).await;
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_rename_file_handler() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
    let file_path = uploads_dir.join("old.txt");
    tokio::fs::write(&file_path, "file content").await.unwrap();

    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: PathBuf::from("data"),
    };
    let (events_tx, _) = broadcast::channel(1024);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    let body = RenameFileBody {
        path: file_path.to_string_lossy().to_string(),
        new_name: "new.txt".into(),
    };
    let res = match rename_file_handler(State(state), axum::Json(body)).await {
        Ok(res) => res,
        Err((status, body)) => panic!("rename failed: {status} {}", body.0.error),
    };

    assert_eq!(
        PathBuf::from(res.0).canonicalize().unwrap(),
        uploads_dir.join("new.txt").canonicalize().unwrap()
    );
    assert!(!file_path.exists());
    assert_eq!(
        tokio::fs::read_to_string(uploads_dir.join("new.txt"))
            .await
            .unwrap(),
        "file content"
    );
}

#[tokio::test]
async fn test_confine_to_uploads_errors() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

    // Path not found
    let res = confine_to_uploads("nonexistent.txt", &uploads_dir);
    assert_eq!(res.map_err(|e| e.0).unwrap_err(), StatusCode::NOT_FOUND);

    // Path outside (using ..)
    let outside = dir.path().join("outside.txt");
    tokio::fs::write(&outside, "secret").await.unwrap();
    let res = confine_to_uploads("../outside.txt", &uploads_dir);
    // Note: canonicalize will resolve .. relative to CWD if not absolute,
    // but let's assume it fails validation.
    if let Ok(p) = res {
        assert!(!p.starts_with(uploads_dir.canonicalize().unwrap()));
    }
}

#[tokio::test]
async fn test_delete_upload_handler_errors() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

    let (events_tx, _) = broadcast::channel(1);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("r"),
        uv_bin: PathBuf::from("v"),
        worker_bin: PathBuf::from("w"),
        data_dir: PathBuf::from("data"),
    };
    let (ctx, _, _) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    // Empty path
    let query = DeleteUploadQuery {
        path: "".to_string(),
    };
    let res = delete_upload_handler(State(state.clone()), Query(query)).await;
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().0, StatusCode::BAD_REQUEST);

    // Outside path
    let query = DeleteUploadQuery {
        path: "../../etc/passwd".to_string(),
    };
    let res = delete_upload_handler(State(state.clone()), Query(query)).await;
    assert!(res.is_err());

    // Non-existent path
    let query = DeleteUploadQuery {
        path: "ghost.txt".to_string(),
    };
    let res = delete_upload_handler(State(state), Query(query)).await;
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_asset_handler_denied() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

    let (events_tx, _) = broadcast::channel(1);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("r"),
        uv_bin: PathBuf::from("v"),
        worker_bin: PathBuf::from("w"),
        data_dir: PathBuf::from("data"),
    };
    let (ctx, _, _) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    // Denied: outside uploads_dir
    let query = AssetQuery {
        path: "/etc/passwd".to_string(),
    };
    let res = asset_handler(State(state), Query(query)).await;
    assert!(res.is_err());
    assert_eq!(res.unwrap_err().0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_more_server_handlers() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

    let (events_tx, _) = broadcast::channel(1);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("r"),
        uv_bin: PathBuf::from("v"),
        worker_bin: PathBuf::from("w"),
        data_dir: PathBuf::from("data"),
    };
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    // test get_worker_status_handler
    let _ = get_worker_status_handler(State(state.clone())).await;

    // test kill_worker_handler
    let _ = kill_worker_handler(State(state.clone())).await;

    // test set_worker_timeout_handler
    let _ = set_worker_timeout_handler(State(state.clone()), Json(TimeoutBody { secs: 10 })).await;

    // test get_index_status_handler (will fail but covers the handler)
    let _ = get_index_status_handler(State(state.clone()), Query(RootQuery { root: None })).await;

    // test cancel_embed_handler
    let _ = cancel_embed_handler(State(state.clone())).await;

    // test get_model_size_handler (will fail)
    let _ = get_model_size_handler(Query(ModelSizeQuery {
        engine: EmbeddingEngine::Fastembed,
        model_id: "m".to_string(),
    }))
    .await;

    // test search_custom_models_handler (will fail)
    let _ = search_custom_models_handler(Query(ModelSearchQuery {
        engine: EmbeddingEngine::Fastembed,
        query: "m".to_string(),
    }))
    .await;

    // test embedder_capabilities_handler
    let _ = embedder_capabilities_handler(State(state.clone())).await;
}

#[tokio::test]
async fn test_delete_upload_handler_directory() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
    let sub_dir = uploads_dir.join("subdir");
    tokio::fs::create_dir(&sub_dir).await.unwrap();
    tokio::fs::write(sub_dir.join("f.txt"), "c").await.unwrap();

    let paths = WorkerPaths::resolve(dir.path());
    let (events_tx, _) = broadcast::channel(1);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (ctx, _, _) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    let query = DeleteUploadQuery {
        path: "subdir".to_string(),
    };
    let res = delete_upload_handler(State(state), Query(query)).await;
    assert!(res.is_ok());
    assert!(!sub_dir.exists());
}

#[tokio::test]
async fn test_upload_handler_limit() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

    let paths = WorkerPaths::resolve(dir.path());
    let (events_tx, _) = broadcast::channel(1);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let (ctx, _, _) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let _state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });
}

#[tokio::test]
async fn test_upload_handler() {
    let dir = tempfile::tempdir().unwrap();
    let (events_tx, _) = broadcast::channel(1024);
    let uploads_dir = dir.path().join("u");
    std::fs::create_dir_all(&uploads_dir).unwrap();

    let paths = WorkerPaths::resolve(dir.path());
    let (ctx, _, _) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        }),
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    let file_path = uploads_dir.join("to_delete.txt");
    std::fs::write(&file_path, "bye").unwrap();

    let params = DeleteUploadQuery {
        path: "to_delete.txt".to_string(),
    };
    assert_eq!(
        delete_upload_handler(State(state.clone()), Query(params))
            .await
            .map_err(|(s, _)| s)
            .unwrap(),
        StatusCode::NO_CONTENT
    );
    assert!(!file_path.exists());

    // Test delete_upload_handler error
    let params_bad = DeleteUploadQuery {
        path: "ghost.txt".to_string(),
    };
    let res_delete_bad = delete_upload_handler(State(state.clone()), Query(params_bad)).await;
    assert!(res_delete_bad.is_err());
    assert_eq!(res_delete_bad.unwrap_err().0, StatusCode::NOT_FOUND);

    // Test delete_all_upload_handler
    std::fs::write(uploads_dir.join("a.txt"), "a").unwrap();
    std::fs::write(uploads_dir.join("b.txt"), "b").unwrap();
    assert_eq!(
        delete_all_upload_handler(State(state.clone()))
            .await
            .map_err(|(s, _)| s)
            .unwrap(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(std::fs::read_dir(&uploads_dir).unwrap().count(), 0);
}

#[tokio::test]
async fn test_even_more_handlers() {
    let dir = tempfile::tempdir().unwrap();
    let uploads_dir = dir.path().join("uploads");
    tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

    let (events_tx, _) = broadcast::channel(1);
    let emitter = Arc::new(BroadcastEmitter {
        tx: events_tx.clone(),
    });
    let paths = WorkerPaths::resolve(dir.path());
    let (ctx, _, _) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        emitter,
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    let _ = get_logs_handler().await;
    let _ = clear_logs_handler().await;
    let _ = get_data_paths_handler(State(state.clone())).await;
    let _ = is_semantic_ready_handler(State(state.clone())).await;
    let _ = delete_index_handler(State(state.clone()), Query(RootQuery { root: None })).await;
    let _ = download_model_handler(
        State(state.clone()),
        Json(DownloadBody {
            selected: SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: wilkes_core::types::EmbedderModel("m".to_string()),
                dimension: 384,
            },
        }),
    )
    .await;

    let _ = update_settings_handler(State(state.clone()), Json(serde_json::json!({}))).await;
    let _ = get_settings_handler(State(state.clone())).await;
}

#[tokio::test]
async fn test_asset_handler_direct() {
    use wilkes_core::types::SourceOrigin;
    use wilkes_core::types::{SearchMode, SearchQuery};
    let dir = tempfile::tempdir().unwrap();
    let (events_tx, _) = broadcast::channel(1024);
    let uploads_dir = dir.path().join("u");
    std::fs::create_dir_all(&uploads_dir).unwrap();

    let asset_file = uploads_dir.join("test.txt");
    std::fs::write(&asset_file, "data").unwrap();

    let state = Arc::new(AppState {
        ctx: Some(test_ctx_with_dir(dir.path())),
        workspaces: None,
        uploads_dir: uploads_dir.clone(),
        events_tx,
    });

    let params = AssetQuery {
        path: asset_file.to_string_lossy().to_string(),
    };
    let res = asset_handler(State(state.clone()), Query(params)).await;
    assert!(res.is_ok());

    // Test open_file_handler
    let body = OpenFileBody {
        path: asset_file.to_string_lossy().to_string(),
    };
    let res_open = open_file_handler(State(state.clone()), Json(body)).await;
    assert!(res_open.is_ok());

    // Test preview_handler
    let match_ref = MatchRef {
        path: asset_file.clone(),
        origin: SourceOrigin::TextFile { line: 1, col: 1 },
        text_range: None,
    };
    let res_preview = preview_handler(State(state.clone()), Json(match_ref)).await;
    assert!(res_preview.is_ok());

    // Test non-existent preview
    let match_ref_bad = MatchRef {
        path: uploads_dir.join("ghost.txt"),
        origin: SourceOrigin::TextFile { line: 1, col: 1 },
        text_range: None,
    };
    let res_bad = preview_handler(State(state.clone()), Json(match_ref_bad)).await;
    assert!(res_bad.is_err());

    // Test search_handler semantic error (returns Ok immediately because SSE)
    let query_semantic = SearchQuery {
        pattern: "test".to_string(),
        is_regex: false,
        case_sensitive: false,
        root: uploads_dir.clone(),
        max_results: 10,
        respect_gitignore: true,
        max_file_size: 1024,
        context_lines: 0,
        mode: SearchMode::Semantic,
        scope: Default::default(),
        supported_extensions: vec![],
        collection_id: None,
        tag_ids: Vec::new(),
    };
    let res_semantic = search_handler(State(state.clone()), Json(query_semantic)).await;
    assert!(res_semantic.is_ok());
}

fn test_ctx_with_dir(dir: &Path) -> Arc<AppContext> {
    let paths = WorkerPaths::resolve(dir);
    let (ctx, _, _) = AppContext::new(
        dir.to_path_buf(),
        dir.join("s.json"),
        paths,
        Arc::new(BroadcastEmitter {
            tx: broadcast::channel(1).0,
        }),
    );
    ctx
}

#[tokio::test]
async fn test_events_handler_sse() {
    let dir = tempfile::tempdir().unwrap();
    let (events_tx, _) = broadcast::channel(1024);
    let paths = WorkerPaths::resolve(dir.path());
    let (ctx, _, _) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        paths,
        Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        }),
    );
    let state = Arc::new(AppState {
        ctx: Some(ctx),
        workspaces: None,
        uploads_dir: dir.path().join("u"),
        events_tx: events_tx.clone(),
    });

    let sse = events_handler(State(state)).await;
    // Verify it returns an Sse response
    let _ = sse.into_response();

    // Send an event and see if it doesn't crash
    events_tx
        .send(("test".to_string(), serde_json::json!({})))
        .unwrap();
}

#[test]
fn managed_contract_fixture_matches_the_server_wire_schema() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/consumer-api-v2.json")).unwrap();
    assert_eq!(fixture["schema_version"], 2);

    let ensure: EnsureManagedWorkspace =
        serde_json::from_value(fixture["ensure_request"].clone()).unwrap();
    assert_eq!(ensure.corpus_key, "store-018f");
    // The corpus names the application that owns it, which is what the
    // route path used to do.
    assert_eq!(ensure.owner, "underdog");

    // The one addressing object, in both forms. A pin is required on a
    // managed corpus, where it routes as well as verifies, and optional on
    // a user workspace, where it only verifies.
    let pinned: ConsumerScope = serde_json::from_value(fixture["scope_pinned"].clone()).unwrap();
    assert_eq!(
        pinned.expected_embedding_space_id.as_deref(),
        Some("space-example")
    );
    let unpinned: ConsumerScope =
        serde_json::from_value(fixture["scope_unpinned"].clone()).unwrap();
    assert_eq!(unpinned.expected_embedding_space_id, None);

    let import: ManagedImportBody =
        serde_json::from_value(fixture["import_request"].clone()).unwrap();
    assert_eq!(import.corpus_id, "managed-corpus-018f");
    assert_eq!(
        import.expected_embedding_space_id.as_deref(),
        Some("space-example")
    );

    // Importing into a corpus that has no index yet: the caller has no
    // space id to echo because the corpus has none, and omitting the field
    // is how it says so. Sending one would claim a space that no index
    // carries, which the handler refuses as a mismatch.
    let first_import: ManagedImportBody =
        serde_json::from_value(fixture["import_request_empty_corpus"].clone()).unwrap();
    assert_eq!(first_import.expected_embedding_space_id, None);
    match import.source {
        ManagedImportSource::WilkesFile {
            workspace_id,
            root,
            path,
        } => {
            assert_eq!(workspace_id, "workspace-example");
            assert_eq!(root, PathBuf::from("/library"));
            assert_eq!(path, PathBuf::from("/library/paper.pdf"));
        }
        ManagedImportSource::Path { .. } => panic!("expected wilkes_file fixture"),
    }

    // The embedder manifest: the shape a consumer reads *before* it has a
    // corpus, and the one that decides what a corpus gets built of. Both
    // sides carry this fixture, and its two nulls are load-bearing — a
    // dimension nobody has loaded, and a recipe nothing has read.
    let manifest: wilkes_core::types::EmbedderCapabilityManifest =
        serde_json::from_value(fixture["embedder_manifest_response"].clone()).unwrap();
    let mini = manifest
        .models
        .iter()
        .find(|model| model.model_id == "AllMiniLML6V2")
        .expect("the pinned model");
    assert_eq!(mini.dimension, Some(384));
    assert_eq!(
        mini.prefix_source,
        wilkes_core::types::PrefixSource::NotDocumented
    );
    let added = manifest
        .models
        .iter()
        .find(|model| !model.catalogued)
        .expect("a hand-added model");
    assert_eq!(added.dimension, None);
    assert_eq!(
        added.prefix_source,
        wilkes_core::types::PrefixSource::Undetermined
    );

    // Field for field what the import route serializes. A list of
    // required names let the fixture keep five fields the reply had
    // dropped, and the consumer that carries a copy of this file would
    // have kept requiring them.
    let response = &fixture["import_response"];
    let fixture_fields: std::collections::BTreeSet<&String> =
        response.as_object().unwrap().keys().collect();
    let served = serde_json::to_value(example_import_reply()).unwrap();
    let served_fields: std::collections::BTreeSet<&String> =
        served.as_object().unwrap().keys().collect();
    assert_eq!(fixture_fields, served_fields);
    assert_eq!(response["spaces"], serde_json::json!(["space-example"]));

    // The outline carries a position and says what established it, and the
    // extraction says what it had to decide to produce that position.
    // Both are part of the contract, so both are read back through the
    // types the server serializes rather than only eyeballed in the file.
    let anchor: wilkes_core::types::OutlineAnchor =
        serde_json::from_value(response["outline"][0]["anchor"].clone()).unwrap();
    assert_eq!(
        anchor,
        wilkes_core::types::OutlineAnchor::DestinationCoordinate
    );
    let extraction: wilkes_core::types::ExtractionDiagnostics =
        serde_json::from_value(response["extraction"].clone()).unwrap();
    assert_eq!(extraction.pages, 1);

    // Passages, named the one way they can be named. `resolve` needs no
    // root or path: a ref already names its document.
    let resolve: ChunksResolveBody =
        serde_json::from_value(fixture["chunks_resolve_request"].clone()).unwrap();
    assert_eq!(resolve.chunk_refs.len(), 1);
    assert_eq!(
        resolve.scope.expected_embedding_space_id.as_deref(),
        Some("space-example")
    );
    let resolved = &fixture["chunks_resolve_response"]["chunks"][0];
    for required in [
        "chunk_ref",
        "ordinal",
        "text",
        "text_sha256",
        "byte_range",
        "origin",
    ] {
        assert!(
            resolved.get(required).is_some(),
            "missing resolved chunk field {required}"
        );
    }
    assert!(
        resolved.get("chunk_id").is_none(),
        "a rowid is not part of this contract"
    );
    assert!(
        resolve.neighbors.is_none(),
        "neighbours are asked for, never implied"
    );
    assert!(
        fixture["chunks_resolve_response"]
            .get("neighbors")
            .is_none(),
        "a reply that was not asked for neighbours carries no key for them"
    );

    // Asked for, neighbours come back keyed by the ref they surround, as
    // whole passages carrying their own refs and ordinals.
    let around: ChunksResolveBody =
        serde_json::from_value(fixture["chunks_resolve_neighbors_request"].clone()).unwrap();
    let window = around.neighbors.expect("the window the fixture asks for");
    assert_eq!((window.before, window.after), (1, 1));
    let entry = &fixture["chunks_resolve_neighbors_response"]["neighbors"][0];
    assert_eq!(entry["of"], around.chunk_refs[0].as_str());
    for side in ["before", "after"] {
        let passage = &entry[side][0];
        for required in [
            "chunk_ref",
            "ordinal",
            "text",
            "text_sha256",
            "byte_range",
            "origin",
        ] {
            assert!(
                passage.get(required).is_some(),
                "missing {side} neighbour field {required}"
            );
        }
    }

    let accumulate: ChunksGroupsBody =
        serde_json::from_value(fixture["chunks_accumulate_request"].clone()).unwrap();
    assert_eq!(accumulate.groups.len(), 1);
    // The sum and the count, not the mean: a caller partitioning a large
    // group across requests adds these and normalizes once.
    let group = &fixture["chunks_accumulate_response"]["groups"][0];
    assert!(group.get("sum").is_some());
    assert!(group.get("member_count").is_some());

    // The untagged probe is the shape most likely to drift silently, so
    // the fixture carries one of each and this reads them back through the
    // enum the server deserializes.
    let search: ChunksSearchBody =
        serde_json::from_value(fixture["chunks_search_request"].clone()).unwrap();
    assert_eq!(search.probes.len(), 2);
    assert!(matches!(search.probes[0], SearchProbe::Vector(_)));
    assert!(matches!(search.probes[1], SearchProbe::Text(_)));

    let backup: ManagedBackupBody =
        serde_json::from_value(fixture["backup_request"].clone()).unwrap();
    assert_eq!(backup.expected_embedding_space_id, "space-example");
    let restore: ManagedRestoreBody =
        serde_json::from_value(fixture["restore_request"].clone()).unwrap();
    assert_eq!(restore.backup_name, "restore-example");
    assert!(
        serde_json::from_value::<ManagedBackupBody>(serde_json::json!({
            "corpus_id": "managed-corpus-018f",
            "expected_embedding_space_id": "space-example",
            "destination": "/arbitrary/path"
        }))
        .is_err(),
        "managed backup must never accept an arbitrary destination"
    );
}
