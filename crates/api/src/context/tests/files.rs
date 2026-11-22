use super::*;

#[test]
fn library_roots_are_canonical_deduplicated_and_collapse_nested_paths() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    let nested = root.join("nested");
    let sibling = dir.path().join("sibling");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    let missing = dir.path().join("missing");
    let settings = Settings {
        last_directory: Some(root.clone()),
        favorites: vec![nested, sibling.clone()],
        recent_dirs: vec![root.clone(), missing.clone()],
        ..Default::default()
    };

    let (roots, errors) = library_roots(&settings);

    assert_eq!(
        roots,
        vec![
            root.canonicalize().unwrap(),
            sibling.canonicalize().unwrap()
        ]
    );
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains(&missing.display().to_string()));
}

#[tokio::test]
async fn test_start_directory_watcher_invalid_root_leaves_no_watcher() {
    let dir = tempdir().unwrap();
    let (_tmp, ctx) = test_ctx();
    ctx.start_directory_watcher(dir.path().join("missing"));
    assert!(ctx.directory_watcher.lock().is_none());
}

/// The managed import API is the only writer of an application-managed
/// index. A watcher on `managed_sources` would re-index every imported
/// document through the identity-less path, dropping the stable chunk refs
/// the import had already handed to its consumer.
#[tokio::test]
async fn test_application_managed_root_is_never_watched() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("managed_sources");
    std::fs::create_dir_all(&root).unwrap();

    let managed = scoped_ctx(
        dir.path(),
        serde_json::json!({
            "kind": "application_managed",
            "owner": "underdog",
            "purpose": "semantic-corpus",
            "corpus_key": "store-id",
        }),
    );
    managed.start_directory_watcher(root.clone());
    assert!(
        managed.directory_watcher.lock().is_none(),
        "an application-managed corpus must not be watched"
    );

    let personal = tempdir().unwrap();
    let personal_root = personal.path().join("library");
    std::fs::create_dir_all(&personal_root).unwrap();
    let user = scoped_ctx(personal.path(), serde_json::json!({ "kind": "user" }));
    user.start_directory_watcher(personal_root);
    assert!(
        user.directory_watcher.lock().is_some(),
        "an ordinary workspace is still watched"
    );
}

/// The corpus is reachable now — listed, activatable, searchable — so the
/// protection that used to come from being unreachable has to be stated on
/// the write itself.
#[tokio::test]
async fn test_application_managed_workspace_refuses_writes() {
    let dir = tempdir().unwrap();
    let managed = scoped_ctx(
        dir.path(),
        serde_json::json!({
            "kind": "application_managed",
            "owner": "underdog",
            "purpose": "semantic-corpus",
            "corpus_key": "store-id",
        }),
    );
    assert!(managed.is_read_only());
    let error = managed.ensure_writable().unwrap_err();
    assert_eq!(
        error.code(),
        Some(wilkes_core::consumer::ConsumerErrorCode::ManagedWorkspaceProtected),
        "{error}"
    );
    assert!(managed
        .save_document(dir.path().join("note.md"), "text".to_string())
        .await
        .unwrap_err()
        .contains("MANAGED_WORKSPACE_PROTECTED"));
    assert!(managed
        .delete_index(None)
        .await
        .unwrap_err()
        .to_string()
        .contains("MANAGED_WORKSPACE_PROTECTED"));

    let personal = tempdir().unwrap();
    let user = scoped_ctx(personal.path(), serde_json::json!({ "kind": "user" }));
    assert!(!user.is_read_only());
    assert!(user.ensure_writable().is_ok());
}

#[tokio::test]
async fn test_stop_directory_watcher() {
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
    assert!(ctx.directory_watcher.lock().is_some());
    ctx.stop_directory_watcher();
    assert!(ctx.directory_watcher.lock().is_none());
}

#[tokio::test]
async fn test_file_operations() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    tokio::fs::write(root.join("test.txt"), "hello")
        .await
        .unwrap();
    tokio::fs::write(root.join("test.pdf"), "fake pdf")
        .await
        .unwrap();
    tokio::fs::create_dir(root.join("subdir")).await.unwrap();

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

    let files = ctx.list_files(root.to_path_buf()).await.unwrap();
    assert!(files.files.len() >= 2);

    let preview = ctx.open_file(root.join("test.txt")).await.unwrap();
    match preview {
        PreviewData::Text { content, .. } => assert!(content.contains("hello")),
        _ => panic!("Expected Text preview"),
    }
}

#[tokio::test]
async fn test_document_operations_allow_files_outside_data_dir() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir(&data_dir).unwrap();

    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        data_dir.clone(),
        dir.path().join("settings.json"),
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

    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, "secret").unwrap();

    let preview = ctx.open_file(outside.clone()).await.unwrap();
    match preview {
        PreviewData::Text { content, .. } => assert_eq!(content, "secret"),
        _ => panic!("Expected text preview"),
    }

    assert_eq!(
        ctx.get_file_metadata(outside.clone()).await.unwrap(),
        DocumentMetadata::default()
    );
    assert_eq!(
        wilkes_agent::search::SearchService::document_metadata(Arc::clone(&ctx), outside.clone(),)
            .await
            .unwrap(),
        DocumentMetadata::default()
    );
    assert_eq!(
        ctx.resolve_file_metadata(outside).await.unwrap(),
        DocumentMetadata::default()
    );
}
