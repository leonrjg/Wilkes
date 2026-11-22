use super::*;

#[test]
fn test_extract_chunks_fallback() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.txt");
    fs::write(&path, "hello world").unwrap();

    let registry = ExtractorRegistry::new(); // empty registry
    let (full_text, chunks, _) = SemanticIndex::extract_chunks(&path, &registry, 100, 10).unwrap();

    assert_eq!(full_text, "hello world");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].text, "hello world");
}

#[test]
fn test_build_full() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("test.txt"), "build test content").unwrap();

    let data_dir = dir.path().join("data");
    fs::create_dir(&data_dir).unwrap();

    let registry = ExtractorRegistry::new();
    struct MockEmbedder;
    impl Embedder for MockEmbedder {
        fn embedding_space_identity(&self) -> crate::embed::EmbeddingSpaceIdentity {
            crate::embed::EmbeddingSpaceIdentity::for_test(
                self.engine(),
                self.model_id(),
                self.dimension(),
            )
        }

        fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(vec![vec![1.0]])
        }
        fn model_id(&self) -> &str {
            "mock"
        }
        fn dimension(&self) -> usize {
            1
        }
        fn engine(&self) -> EmbeddingEngine {
            EmbeddingEngine::Candle
        }
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel(10);
    let indexing = IndexingConfig {
        chunk_size: 100,
        chunk_overlap: 0,
        supported_extensions: vec!["txt".to_string()],
    };

    let idx = SemanticIndex::build(
        &data_dir,
        &root,
        &[root.join("test.txt")],
        &registry,
        &MockEmbedder,
        BuildOptions::new(
            BuildReporter::without_journal(tx),
            Arc::new(AtomicBool::new(false)),
        ),
        &indexing,
    )
    .unwrap();

    assert_eq!(idx.status().total_chunks, 1);

    // Check progress messages
    let mut progress_count = 0;
    while let Ok(_p) = rx.try_recv() {
        progress_count += 1;
    }
    assert!(progress_count >= 2);
}

/// The invariant this build is arranged around: every document is read
/// for its figures before anything is embedded, and the recognizer is let
/// go at the boundary — so the two models are never both resident, and
/// neither sits idle through the other's work waiting to be reaped.
#[test]
fn recognition_is_a_whole_pass_that_finishes_before_the_first_embedding() {
    assert_eq!(
        build_recording("test-analyzer-v1"),
        vec![
            "read:a", "read:b",  // the recognition pass, whole
            "release", // and the recognizer let go at its end
            "read:a", "read:b", // the embedding pass re-reads from cache
            "embed",
        ]
    );
}

/// The pass is spent only where there is something to recognize. With no
/// analyzer configured there are no figures to read, so a document is
/// read once and nothing is released — the separation must not cost a
/// second read of every file in a library nobody enriched.
#[test]
fn a_build_with_no_analyzer_reads_each_document_once() {
    assert_eq!(build_recording(""), vec!["read:a", "read:b", "embed"]);
}

#[test]
fn test_build_reembeds_legacy_rows_without_exact_identity() {
    for missing_text in [None, Some("")] {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("legacy.txt");
        fs::write(&path, "legacy body").unwrap();

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
            path: path.clone(),
            full_text: "legacy body".to_string(),
            chunks: vec![(test_chunk(&path, "legacy body"), vec![1.0])],
        })
        .unwrap();
        idx.conn
            .execute("UPDATE files SET full_text = ?1", params![missing_text])
            .unwrap();
        drop(idx);

        let registry = ExtractorRegistry::new();
        let embedder = CountingEmbedder::new();
        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        let rebuilt = SemanticIndex::build(
            &data_dir,
            &root,
            std::slice::from_ref(&path),
            &registry,
            &embedder,
            BuildOptions::new(
                BuildReporter::without_journal(tx),
                Arc::new(AtomicBool::new(false)),
            ),
            &txt_indexing(),
        )
        .unwrap();

        // Matching model/dimension and unchanged path metadata are not a
        // compatibility proof. This row lacks source/recipe/rendition
        // identity, so a fresh embedding is the safe outcome.
        assert_eq!(embedder.calls(), 1);
        let (full_text, _) = rebuilt
            .indexed_document_for_path(&path)
            .unwrap()
            .expect("rebuilt document should have stored full text");
        assert_eq!(full_text, "legacy body");
    }
}

/// A rebuild that merges into the existing database must still report
/// itself as the last build. The merge path returns the live database and
/// drops the temporary one it filled, so a stamp written at creation — or
/// into the temporary — left the panel showing the age of the file while
/// its contents were rebuilt underneath it.
#[test]
fn a_rebuild_that_merges_still_moves_the_build_stamp_forward() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let root = dir.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("doc.txt");
    fs::write(&path, "body").unwrap();

    let registry = ExtractorRegistry::new();
    let build = || {
        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        SemanticIndex::build(
            &data_dir,
            &root,
            std::slice::from_ref(&path),
            &registry,
            &CountingEmbedder::new(),
            BuildOptions::new(
                BuildReporter::without_journal(tx),
                Arc::new(AtomicBool::new(false)),
            ),
            &txt_indexing(),
        )
        .unwrap()
    };

    let first = build().status();
    let first_built_at = first.built_at.expect("a build records when it finished");
    assert!(
        first.build_duration_ms.is_some(),
        "a build records how long it took"
    );

    // Far enough back that the second build's stamp cannot tie with the
    // first at one-second resolution.
    SemanticIndex::open_exact(
        &data_dir,
        &CountingEmbedder::new().embedding_space_identity(),
    )
    .unwrap()
    .conn
    .execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('built_at', ?1)",
        params![(first_built_at - 3600).to_string()],
    )
    .unwrap();

    let second = build().status();
    assert!(
        second.built_at.unwrap() >= first_built_at,
        "the rebuild left a stale stamp: {:?} < {first_built_at}",
        second.built_at
    );
}

#[test]
fn test_backfill_missing_full_text_fills_only_unchanged_stale_rows() {
    use std::sync::Mutex;

    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let root = dir.path().join("root");
    fs::create_dir_all(&root).unwrap();

    // A legacy row: chunks present, full_text NULL, on-disk identity intact.
    let stale_path = root.join("stale.txt");
    fs::write(&stale_path, "stale body").unwrap();
    // A row whose file is edited after indexing: its stored text is stale, so
    // backfill must leave it for the watcher rather than persist old content.
    let changed_path = root.join("changed.txt");
    fs::write(&changed_path, "original").unwrap();

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
        path: stale_path.clone(),
        full_text: "stale body".to_string(),
        chunks: vec![(test_chunk(&stale_path, "stale body"), vec![1.0])],
    })
    .unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        path: changed_path.clone(),
        full_text: "original".to_string(),
        chunks: vec![(test_chunk(&changed_path, "original"), vec![1.0])],
    })
    .unwrap();
    // Strip stored text from both rows to reproduce a pre-v4 index.
    idx.conn
        .execute("UPDATE files SET full_text = NULL", [])
        .unwrap();
    // Change one file on disk so its identity no longer matches the index.
    fs::write(&changed_path, "original body is now longer").unwrap();

    let index = Arc::new(Mutex::new(Some(idx)));

    let filled = SemanticIndex::backfill_missing_full_text(&index);
    assert_eq!(filled, 1, "only the unchanged legacy row is backfilled");

    let guard = index.lock().unwrap();
    let idx = guard.as_ref().unwrap();
    // The unchanged file's text is restored and now served from the index.
    let (text, _) = idx
        .indexed_document_for_path(&stale_path)
        .unwrap()
        .expect("stale row should now have stored full text");
    assert_eq!(text, "stale body");
    // The edited file was skipped: still no stored text, so grep stays live.
    assert!(idx
        .indexed_document_for_path(&changed_path)
        .unwrap()
        .is_none());
    drop(guard);

    // Idempotent: the filled row is no longer stale and the edited row is
    // still correctly skipped, so a second pass writes nothing.
    assert_eq!(SemanticIndex::backfill_missing_full_text(&index), 0);
}

/// Embedding is a unit of its own: a bar that moved only when text was
/// extracted reached its end while the model was still working.
#[test]
fn build_progress_counts_embedding_as_its_own_unit() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let root = dir.path().join("root");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&root).unwrap();
    let paths: Vec<PathBuf> = ["a.txt", "b.txt"]
        .iter()
        .map(|name| {
            let path = root.join(name);
            fs::write(&path, format!("contents of {name}")).unwrap();
            path
        })
        .collect();

    let registry = ExtractorRegistry::new();
    let embedder = CountingEmbedder::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    SemanticIndex::build(
        &data_dir,
        &root,
        &paths,
        &registry,
        &embedder,
        BuildOptions::new(
            BuildReporter::without_journal(tx),
            Arc::new(AtomicBool::new(false)),
        ),
        &txt_indexing(),
    )
    .unwrap();

    let mut events = Vec::new();
    while let Ok(crate::models::progress::EmbedProgress::Build(event)) = rx.try_recv() {
        events.push(event);
    }
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event.total_files == 4));
    assert!(events
        .windows(2)
        .all(|pair| pair[0].files_processed <= pair[1].files_processed));
    // A document waiting on the model is not counted as done with.
    assert!(events
        .iter()
        .filter(|event| event.stage == Some(DocumentStage::Embedding))
        .all(|event| event.files_processed < 4));
    let settled: Vec<usize> = events
        .iter()
        .filter(|event| event.outcome.is_some())
        .map(|event| event.files_processed)
        .collect();
    assert_eq!(settled.last(), Some(&4));
    let last = events.last().unwrap();
    assert!(last.done);
    assert_eq!(last.files_processed, 4);
}

/// The whole point of publishing an interrupted build.
///
/// A build stopped part-way used to lose everything it had read, because
/// the temporary database it fills was deleted rather than merged. Now it
/// is merged additively: what finished is searchable, and the documents the
/// build never reached are neither indexed nor destroyed.
#[test]
fn a_stopped_build_saves_the_documents_it_finished() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let root = dir.path().join("root");
    fs::create_dir_all(&root).unwrap();
    // Named so the walk order below is the order they are written.
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(root.join(name), format!("content of {name}")).unwrap();
    }
    let paths: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt"]
        .iter()
        .map(|n| root.join(n))
        .collect();

    let cancel = Arc::new(AtomicBool::new(false));
    let embedder = CancelAfter {
        cancel: Arc::clone(&cancel),
        after: 1,
        calls: AtomicUsize::new(0),
    };
    let (tx, _rx) = tokio::sync::mpsc::channel(64);

    let result = SemanticIndex::build(
        &data_dir,
        &root,
        &paths,
        &ExtractorRegistry::new(),
        &embedder,
        BuildOptions::new(BuildReporter::without_journal(tx), Arc::clone(&cancel)),
        &txt_indexing(),
    );
    let err = match result {
        Ok(_) => panic!("a stopped build reports that it was stopped"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("cancelled"), "{err:#}");

    // The one document it got through is in the index, and searchable.
    let idx = SemanticIndex::open(&data_dir, "counting", 1).unwrap();
    assert_eq!(
        idx.status_for_root(Some(&root)).indexed_files,
        1,
        "the document the build finished before it was stopped is saved"
    );
    let hits = idx
        .query_scoped(&[1.0], 10, SemanticQueryScope::Root(&root))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].file_path, canon(&paths[0]));

    // And the build is not claimed to have happened.
    assert!(
        idx.status_for_root(Some(&root)).built_at.is_none(),
        "a stopped build must not stamp the index as built"
    );
}

/// The other half of the same guarantee: continuing over what is left adds
/// to the root rather than replacing it, so the first run's work survives
/// the second run that never looked at it.
#[test]
fn continuing_over_the_remaining_documents_keeps_what_was_already_saved() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let root = dir.path().join("root");
    fs::create_dir_all(&root).unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(root.join(name), format!("content of {name}")).unwrap();
    }
    let paths: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt"]
        .iter()
        .map(|n| root.join(n))
        .collect();
    let (tx, _rx) = tokio::sync::mpsc::channel(64);

    let cancel = Arc::new(AtomicBool::new(false));
    let stopped = SemanticIndex::build(
        &data_dir,
        &root,
        &paths,
        &ExtractorRegistry::new(),
        &CancelAfter {
            cancel: Arc::clone(&cancel),
            after: 1,
            calls: AtomicUsize::new(0),
        },
        BuildOptions::new(
            BuildReporter::without_journal(tx.clone()),
            Arc::clone(&cancel),
        ),
        &txt_indexing(),
    );
    assert!(stopped.is_err());

    // Continue over only what was never reached. A whole-root merge here
    // would drop `a.txt` from the root, because this build never saw it.
    let embedder = CountingEmbedder::new();
    let resumed = SemanticIndex::build(
        &data_dir,
        &root,
        &paths[1..],
        &ExtractorRegistry::new(),
        &embedder,
        BuildOptions::over_subset(
            BuildReporter::without_journal(tx),
            Arc::new(AtomicBool::new(false)),
        ),
        &txt_indexing(),
    )
    .unwrap();

    assert_eq!(
        resumed.status_for_root(Some(&root)).indexed_files,
        3,
        "the first run's document is still in the root after the second"
    );
}

/// A retry in which every document fails again adds nothing to the index —
/// which looks, from the temporary database, exactly like a build that was
/// cancelled before its first document. It must not be reported as one.
#[test]
fn a_subset_build_that_adds_nothing_is_not_reported_as_cancelled() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let root = dir.path().join("root");
    fs::create_dir_all(&root).unwrap();
    let good = root.join("good.txt");
    fs::write(&good, "readable").unwrap();

    // An index to add to, holding the document that did work.
    SemanticIndex::build(
        &data_dir,
        &root,
        std::slice::from_ref(&good),
        &ExtractorRegistry::new(),
        &CountingEmbedder::new(),
        BuildOptions::new(
            BuildReporter::without_journal(tokio::sync::mpsc::channel(16).0),
            Arc::new(AtomicBool::new(false)),
        ),
        &txt_indexing(),
    )
    .unwrap();

    // Retry a document that cannot be read: nothing is added.
    let unreadable = root.join("unreadable.txt");
    fs::write(&unreadable, [0xff, 0xfe, 0xfd]).unwrap();
    let retried = SemanticIndex::build(
        &data_dir,
        &root,
        std::slice::from_ref(&unreadable),
        &ExtractorRegistry::new(),
        &CountingEmbedder::new(),
        BuildOptions::over_subset(
            BuildReporter::without_journal(tokio::sync::mpsc::channel(16).0),
            Arc::new(AtomicBool::new(false)),
        ),
        &txt_indexing(),
    );

    let idx = match retried {
        Ok(idx) => idx,
        Err(e) => panic!("a retry that adds nothing is not a cancellation: {e:#}"),
    };
    assert_eq!(
        idx.status_for_root(Some(&root)).indexed_files,
        1,
        "the document indexed earlier is untouched"
    );
}

#[test]
fn test_build_skips_file_on_embedding_dimension_mismatch() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("test.txt"), "build test content").unwrap();

    let data_dir = dir.path().join("data");
    fs::create_dir(&data_dir).unwrap();

    let registry = ExtractorRegistry::new();
    struct WrongDimEmbedder;
    impl Embedder for WrongDimEmbedder {
        fn embedding_space_identity(&self) -> crate::embed::EmbeddingSpaceIdentity {
            crate::embed::EmbeddingSpaceIdentity::for_test(
                self.engine(),
                self.model_id(),
                self.dimension(),
            )
        }

        fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(vec![vec![1.0, 2.0]])
        }
        fn model_id(&self) -> &str {
            "mock"
        }
        fn dimension(&self) -> usize {
            1
        }
        fn engine(&self) -> EmbeddingEngine {
            EmbeddingEngine::Candle
        }
    }

    let (tx, _rx) = tokio::sync::mpsc::channel(10);
    let indexing = IndexingConfig {
        chunk_size: 100,
        chunk_overlap: 0,
        supported_extensions: vec!["txt".to_string()],
    };

    let idx = SemanticIndex::build(
        &data_dir,
        &root,
        &[root.join("test.txt")],
        &registry,
        &WrongDimEmbedder,
        BuildOptions::new(
            BuildReporter::without_journal(tx),
            Arc::new(AtomicBool::new(false)),
        ),
        &indexing,
    )
    .unwrap();

    assert_eq!(idx.status().total_chunks, 0);
}

/// Fatality is a property of the error's type, not of how its message
/// happens to be worded. The predicate used to match message prefixes, so
/// a reworded log line could quietly demote a dead worker to a skipped
/// batch — and a skipped batch is followed by another submit, which with
/// no process left starts one.
#[test]
fn test_is_fatal_embedder_error_detects_worker_failures() {
    use crate::worker::fault::WorkerFault;
    use anyhow::Context;

    assert!(SemanticIndex::is_fatal_embedder_error(&WorkerFault::gone(
        "worker process closed stdout unexpectedly"
    )));
    assert!(SemanticIndex::is_fatal_embedder_error(
        &WorkerFault::reported("could not load the model")
    ));
    // Recognised through whatever context the layers in between stacked
    // on it, because by the time the build sees it there is always some.
    assert!(SemanticIndex::is_fatal_embedder_error(
        &Err::<(), _>(WorkerFault::gone("closed stdout"))
            .context("embedding a batch of 64 chunks")
            .unwrap_err()
    ));
    // A failure of this batch, not of the embedder: the next batch is
    // worth attempting and no process has to be started to attempt it.
    assert!(!SemanticIndex::is_fatal_embedder_error(&anyhow::anyhow!(
        "dimension mismatch"
    )));
    // A message that reads like the old prefixes is not, by itself, fatal.
    assert!(!SemanticIndex::is_fatal_embedder_error(&anyhow::anyhow!(
        "Worker error: something a local embedder said"
    )));
}
