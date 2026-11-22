use super::*;

#[test]
fn test_reconcile_indexes_new_file_missed_while_offline() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut idx =
        SemanticIndex::create(root, "counting", 1, EmbeddingEngine::Candle, Some(root)).unwrap();
    let embedder = CountingEmbedder::new();
    let registry = ExtractorRegistry::new();
    let indexing = txt_indexing();

    let path = root.join("new.txt");
    fs::write(&path, "new content").unwrap();

    let errors = idx
        .reconcile_root(root, &registry, &embedder, &indexing)
        .unwrap();
    assert!(errors.is_empty());
    assert_eq!(embedder.calls(), 1);

    let results = idx.query(&[1.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].file_path, canon(&path));
    assert_eq!(results[0].chunk_text, "new content");
}

#[test]
fn test_reconcile_rekeys_offline_rename_without_reembedding() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut idx =
        SemanticIndex::create(root, "counting", 1, EmbeddingEngine::Candle, Some(root)).unwrap();
    let embedder = CountingEmbedder::new();
    let registry = ExtractorRegistry::new();
    let indexing = txt_indexing();

    let old = root.join("old.txt");
    let new = root.join("new.txt");
    fs::write(&old, "stable content").unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: old.clone(),
        chunks: vec![(
            Chunk {
                file_path: old.clone(),
                text: "stable content".to_string(),
                byte_range: ByteRange { start: 0, end: 14 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![1.0],
        )],
    })
    .unwrap();

    fs::rename(&old, &new).unwrap();

    let errors = idx
        .reconcile_root(root, &registry, &embedder, &indexing)
        .unwrap();
    assert!(errors.is_empty());
    assert_eq!(embedder.calls(), 0);

    let results = idx.query(&[1.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].file_path, canon(&new));
    assert_eq!(idx.status().indexed_files, 1);
}

#[test]
fn test_reconcile_deletes_file_missing_after_offline_delete() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut idx =
        SemanticIndex::create(root, "counting", 1, EmbeddingEngine::Candle, Some(root)).unwrap();
    let embedder = CountingEmbedder::new();
    let registry = ExtractorRegistry::new();
    let indexing = txt_indexing();

    let path = root.join("gone.txt");
    fs::write(&path, "delete me").unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: path.clone(),
        chunks: vec![(
            Chunk {
                file_path: path.clone(),
                text: "delete me".to_string(),
                byte_range: ByteRange { start: 0, end: 9 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![1.0],
        )],
    })
    .unwrap();

    fs::remove_file(&path).unwrap();

    let errors = idx
        .reconcile_root(root, &registry, &embedder, &indexing)
        .unwrap();
    assert!(errors.is_empty());
    assert_eq!(idx.query(&[1.0], 1).unwrap().len(), 0);
    assert_eq!(idx.status().indexed_files, 0);
}

#[test]
fn test_reconcile_reindexes_changed_file_at_same_path() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut idx =
        SemanticIndex::create(root, "counting", 1, EmbeddingEngine::Candle, Some(root)).unwrap();
    let embedder = CountingEmbedder::new();
    let registry = ExtractorRegistry::new();
    let indexing = txt_indexing();

    let path = root.join("changed.txt");
    fs::write(&path, "old").unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: path.clone(),
        chunks: vec![(
            Chunk {
                file_path: path.clone(),
                text: "old".to_string(),
                byte_range: ByteRange { start: 0, end: 3 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![0.5],
        )],
    })
    .unwrap();

    fs::write(&path, "changed content").unwrap();

    let errors = idx
        .reconcile_root(root, &registry, &embedder, &indexing)
        .unwrap();
    assert!(errors.is_empty());
    assert_eq!(embedder.calls(), 1);

    let results = idx.query(&[1.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].file_path, canon(&path));
    assert_eq!(results[0].chunk_text, "changed content");
}

#[test]
fn test_build_second_root_preserves_first_root() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let root_a = dir.path().join("a");
    let root_b = dir.path().join("b");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();
    fs::write(root_a.join("a.txt"), "alpha").unwrap();
    fs::write(root_b.join("b.txt"), "beta").unwrap();

    let registry = ExtractorRegistry::new();
    let embedder = CountingEmbedder::new();
    let indexing = txt_indexing();
    let (tx, _rx) = tokio::sync::mpsc::channel(10);

    SemanticIndex::build(
        &data_dir,
        &root_a,
        &[root_a.join("a.txt")],
        &registry,
        &embedder,
        BuildOptions::new(
            BuildReporter::without_journal(tx.clone()),
            Arc::new(AtomicBool::new(false)),
        ),
        &indexing,
    )
    .unwrap();
    let idx = SemanticIndex::build(
        &data_dir,
        &root_b,
        &[root_b.join("b.txt")],
        &registry,
        &embedder,
        BuildOptions::new(
            BuildReporter::without_journal(tx),
            Arc::new(AtomicBool::new(false)),
        ),
        &indexing,
    )
    .unwrap();

    assert_eq!(idx.status().indexed_files, 2);
    assert_eq!(idx.status_for_root(Some(&root_a)).indexed_files, 1);
    assert_eq!(idx.status_for_root(Some(&root_b)).indexed_files, 1);
}

#[test]
fn test_overlapping_roots_share_file_rows_but_keep_root_coverage() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    let nested = root.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let shared = nested.join("shared.txt");
    fs::write(&shared, "shared").unwrap();

    let registry = ExtractorRegistry::new();
    let embedder = CountingEmbedder::new();
    let indexing = txt_indexing();
    let (tx, _rx) = tokio::sync::mpsc::channel(10);

    let idx = SemanticIndex::build(
        dir.path(),
        &root,
        std::slice::from_ref(&shared),
        &registry,
        &embedder,
        BuildOptions::new(
            BuildReporter::without_journal(tx.clone()),
            Arc::new(AtomicBool::new(false)),
        ),
        &indexing,
    )
    .unwrap();
    assert_eq!(idx.status().indexed_files, 1);
    assert_eq!(embedder.calls(), 1);

    let mut idx = SemanticIndex::build(
        dir.path(),
        &nested,
        std::slice::from_ref(&shared),
        &registry,
        &embedder,
        BuildOptions::new(
            BuildReporter::without_journal(tx),
            Arc::new(AtomicBool::new(false)),
        ),
        &indexing,
    )
    .unwrap();

    assert_eq!(idx.status().indexed_files, 1);
    assert_eq!(idx.status_for_root(Some(&root)).indexed_files, 1);
    assert_eq!(idx.status_for_root(Some(&nested)).indexed_files, 1);
    assert_eq!(embedder.calls(), 1);

    idx.delete_root(&nested).unwrap();
    assert_eq!(idx.status().indexed_files, 1);
    assert_eq!(idx.status_for_root(Some(&root)).indexed_files, 1);
    assert_eq!(idx.status_for_root(Some(&nested)).indexed_files, 0);
}

#[test]
fn test_cancelled_build_preserves_existing_root_coverage() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let root = dir.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("doc.txt");
    fs::write(&path, "old content").unwrap();

    let registry = ExtractorRegistry::new();
    let embedder = CountingEmbedder::new();
    let indexing = txt_indexing();
    let (tx, _rx) = tokio::sync::mpsc::channel(10);

    let mut idx = SemanticIndex::create(
        &data_dir,
        "counting",
        1,
        EmbeddingEngine::Candle,
        Some(&root),
    )
    .unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: path.clone(),
        chunks: vec![(test_chunk(&path, "old content"), vec![1.0])],
    })
    .unwrap();
    drop(idx);

    fs::write(&path, "new content").unwrap();
    let cancelled = Arc::new(AtomicBool::new(true));
    let result = SemanticIndex::build(
        &data_dir,
        &root,
        std::slice::from_ref(&path),
        &registry,
        &embedder,
        BuildOptions::new(BuildReporter::without_journal(tx), cancelled),
        &indexing,
    );
    let err = match result {
        Ok(_) => panic!("expected cancelled build to fail"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("cancelled"));

    let idx = SemanticIndex::open(&data_dir, "counting", 1).unwrap();
    assert_eq!(idx.status_for_root(Some(&root)).indexed_files, 1);
    let results = idx
        .query_scoped(&[1.0], 1, SemanticQueryScope::Root(&root))
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_text, "old content");
}

#[test]
fn test_cancelled_overlapping_root_retries_without_reembedding() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    let nested = root.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let shared = nested.join("shared.txt");
    fs::write(&shared, "shared").unwrap();

    let registry = ExtractorRegistry::new();
    let embedder = CountingEmbedder::new();
    let indexing = txt_indexing();
    let (tx, _rx) = tokio::sync::mpsc::channel(10);

    SemanticIndex::build(
        dir.path(),
        &root,
        std::slice::from_ref(&shared),
        &registry,
        &embedder,
        BuildOptions::new(
            BuildReporter::without_journal(tx.clone()),
            Arc::new(AtomicBool::new(false)),
        ),
        &indexing,
    )
    .unwrap();
    assert_eq!(embedder.calls(), 1);

    let cancelled = SemanticIndex::build(
        dir.path(),
        &nested,
        std::slice::from_ref(&shared),
        &registry,
        &embedder,
        BuildOptions::new(
            BuildReporter::without_journal(tx.clone()),
            Arc::new(AtomicBool::new(true)),
        ),
        &indexing,
    );
    assert!(cancelled.is_err());

    let idx = SemanticIndex::open(dir.path(), "counting", 1).unwrap();
    assert_eq!(idx.status_for_root(Some(&root)).indexed_files, 1);
    assert_eq!(idx.status_for_root(Some(&nested)).indexed_files, 0);
    assert_eq!(embedder.calls(), 1);
    drop(idx);

    let idx = SemanticIndex::build(
        dir.path(),
        &nested,
        std::slice::from_ref(&shared),
        &registry,
        &embedder,
        BuildOptions::new(
            BuildReporter::without_journal(tx),
            Arc::new(AtomicBool::new(false)),
        ),
        &indexing,
    )
    .unwrap();
    assert_eq!(idx.status_for_root(Some(&root)).indexed_files, 1);
    assert_eq!(idx.status_for_root(Some(&nested)).indexed_files, 1);
    assert_eq!(embedder.calls(), 1);
}
