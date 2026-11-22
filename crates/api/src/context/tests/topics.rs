use super::*;

#[test]
fn topic_cap_is_hard_and_distributed_per_document() {
    let chunks = vec![
        topic_chunk(1, 1),
        topic_chunk(2, 2),
        topic_chunk(2, 3),
        topic_chunk(2, 4),
        topic_chunk(3, 5),
        topic_chunk(3, 6),
        topic_chunk(3, 7),
        topic_chunk(3, 8),
        topic_chunk(3, 9),
        topic_chunk(3, 10),
    ];
    let sampled = cap_topic_chunks(chunks, 6);
    assert_eq!(sampled.len(), 6);
    let counts = sampled.iter().fold(
        std::collections::BTreeMap::<i64, usize>::new(),
        |mut counts, chunk| {
            *counts.entry(chunk.file_id).or_default() += 1;
            counts
        },
    );
    assert_eq!(counts, [(1, 1), (2, 3), (3, 2)].into_iter().collect());
    assert_eq!(
        sampled
            .iter()
            .filter(|chunk| chunk.file_id == 3)
            .map(|chunk| chunk.chunk_id)
            .collect::<Vec<_>>(),
        vec![6, 9],
        "within-document sampling should cover the passage range"
    );
}

#[test]
fn topic_cap_samples_documents_evenly_when_documents_exceed_budget() {
    let chunks = (0..10)
        .map(|index| topic_chunk(index, index))
        .collect::<Vec<_>>();
    let sampled = cap_topic_chunks(chunks, 3);
    assert_eq!(sampled.len(), 3);
    assert_eq!(
        sampled
            .iter()
            .map(|chunk| chunk.file_id)
            .collect::<Vec<_>>(),
        vec![0, 3, 6]
    );
}

#[test]
fn topic_cap_allows_the_complete_data_set() {
    let chunks = (0..10)
        .map(|index| topic_chunk(index, index))
        .collect::<Vec<_>>();
    let sampled = cap_topic_chunks(chunks, usize::MAX);
    assert_eq!(sampled.len(), 10);
}

#[tokio::test]
async fn topic_granularity_recuts_one_cached_tree_and_cap_rebuilds_it() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("topic-root");
    std::fs::create_dir_all(&root).unwrap();
    let settings = Settings {
        last_directory: Some(root.clone()),
        ..Default::default()
    };
    std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();
    let mut index = SemanticIndex::create(
        dir.path(),
        "topic-cache-model",
        2,
        EmbeddingEngine::Candle,
        Some(&root),
    )
    .unwrap();
    let vectors = [
        [1.0, 0.0],
        [0.99, 0.01],
        [0.0, 1.0],
        [0.01, 0.99],
        [-1.0, 0.0],
        [-0.99, 0.01],
    ];
    for (index_number, vector) in vectors.into_iter().enumerate() {
        let path = root.join(format!("{index_number}.txt"));
        let text = format!("topic passage {index_number}");
        std::fs::write(&path, &text).unwrap();
        index
            .write_file(wilkes_core::embed::index::db::PreparedFile {
                retained: Default::default(),
                full_text: String::new(),
                path: path.clone(),
                chunks: vec![(
                    wilkes_core::embed::index::chunk::Chunk {
                        file_path: path,
                        text: text.clone(),
                        byte_range: ByteRange {
                            start: 0,
                            end: text.len(),
                        },
                        origin: SourceOrigin::TextFile { line: 1, col: 1 },
                    },
                    vector.to_vec(),
                )],
            })
            .unwrap();
    }
    *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

    let fewer = Arc::clone(&ctx)
        .chunk_topics(
            "root-fewer".to_string(),
            ChunkTopicsQuery {
                root: root.clone(),
                path: None,
                granularity: wilkes_core::types::BookmarkClusterGranularity::MuchFewer,
            },
        )
        .await
        .unwrap();
    let first_tree = ctx.topic_tree_caches.lock().root.as_ref().unwrap().clone();
    assert!(first_tree
        .sampled
        .iter()
        .all(|chunk| chunk.embedding.is_empty()));

    let more = Arc::clone(&ctx)
        .chunk_topics(
            "root-more".to_string(),
            ChunkTopicsQuery {
                root: root.clone(),
                path: None,
                granularity: wilkes_core::types::BookmarkClusterGranularity::MuchMore,
            },
        )
        .await
        .unwrap();
    let recut_tree = ctx.topic_tree_caches.lock().root.as_ref().unwrap().clone();
    assert!(Arc::ptr_eq(&first_tree, &recut_tree));
    assert_ne!(fewer.topics.len(), more.topics.len());

    ctx.update_semantic_settings(|settings| SemanticSettings {
        topic_cloud_input_cap: 3,
        ..settings
    })
    .await;
    let capped = Arc::clone(&ctx)
        .chunk_topics(
            "root-capped".to_string(),
            ChunkTopicsQuery {
                root,
                path: None,
                granularity: wilkes_core::types::BookmarkClusterGranularity::Balanced,
            },
        )
        .await
        .unwrap();
    let rebuilt_tree = ctx.topic_tree_caches.lock().root.as_ref().unwrap().clone();
    assert!(!Arc::ptr_eq(&first_tree, &rebuilt_tree));
    assert_eq!(capped.sampled_chunk_count, 3);

    let revision = ctx.semantic_index_revision.load(Ordering::Acquire);
    ctx.detach_semantic_embedder();
    ctx.detach_search_index();
    assert!(ctx.topic_tree_caches.lock().root.is_none());
    assert!(ctx.topic_tree_caches.lock().document.is_none());
    assert!(ctx.semantic_index_revision.load(Ordering::Acquire) > revision);
}

#[tokio::test]
async fn document_topics_are_file_scoped_and_keep_the_root_tree_cached() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("document-topic-root");
    std::fs::create_dir_all(&root).unwrap();
    let document = root.join("paper.txt");
    let other = root.join("other.txt");
    std::fs::write(&document, "document passages").unwrap();
    std::fs::write(&other, "other passage").unwrap();
    let settings = Settings {
        last_directory: Some(root.clone()),
        ..Default::default()
    };
    std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

    let mut index = SemanticIndex::create(
        dir.path(),
        "document-topic-model",
        2,
        EmbeddingEngine::Candle,
        Some(&root),
    )
    .unwrap();
    let vectors = [
        [1.0, 0.0],
        [0.99, 0.01],
        [0.0, 1.0],
        [0.01, 0.99],
        [-1.0, 0.0],
        [-0.99, 0.01],
    ];
    let chunks = vectors
        .into_iter()
        .enumerate()
        .map(|(number, vector)| {
            let text = format!("document topic passage {number}");
            (
                wilkes_core::embed::index::chunk::Chunk {
                    file_path: document.clone(),
                    text: text.clone(),
                    byte_range: ByteRange {
                        start: 0,
                        end: text.len(),
                    },
                    origin: SourceOrigin::TextFile {
                        line: (number + 1) as u32,
                        col: 1,
                    },
                },
                vector.to_vec(),
            )
        })
        .collect();
    index
        .write_file(wilkes_core::embed::index::db::PreparedFile {
            retained: Default::default(),
            full_text: String::new(),
            path: document.clone(),
            chunks,
        })
        .unwrap();
    index
        .write_file(wilkes_core::embed::index::db::PreparedFile {
            retained: Default::default(),
            full_text: String::new(),
            path: other.clone(),
            chunks: vec![(
                wilkes_core::embed::index::chunk::Chunk {
                    file_path: other,
                    text: "other passage".to_string(),
                    byte_range: ByteRange { start: 0, end: 13 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![0.5, 0.5],
            )],
        })
        .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

    let root_result = Arc::clone(&ctx)
        .chunk_topics(
            "root-cloud".to_string(),
            ChunkTopicsQuery {
                root: root.clone(),
                path: None,
                granularity: wilkes_core::types::BookmarkClusterGranularity::MuchFewer,
            },
        )
        .await
        .unwrap();
    assert!(root_result
        .topics
        .iter()
        .all(|topic| topic.library_coverage.is_none()));
    let document_result = Arc::clone(&ctx)
        .chunk_topics(
            "document-cloud".to_string(),
            ChunkTopicsQuery {
                root,
                path: Some(document.clone()),
                granularity: wilkes_core::types::BookmarkClusterGranularity::MuchFewer,
            },
        )
        .await
        .unwrap();

    assert_eq!(document_result.total_document_count, 1);
    assert_eq!(document_result.total_chunk_count, 6);
    let canonical_document = document.canonicalize().unwrap();
    assert!(document_result
        .topics
        .iter()
        .flat_map(|topic| &topic.chunks)
        .all(|chunk| chunk.file_path == canonical_document));
    let caches = ctx.topic_tree_caches.lock();
    assert!(caches.root.is_some());
    assert!(caches.document.is_some());
}

/// A name already in the store was paid for by an earlier run. Switching
/// generation off stops new names being made; it does not make the old ones
/// unreadable, and a cloud of permanently nameless tags is what treating it
/// that way produced.
#[tokio::test]
async fn cached_topic_names_survive_generation_being_switched_off() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("cached-label-root");
    std::fs::create_dir_all(&root).unwrap();
    let document = root.join("passages.txt");
    std::fs::write(&document, "cached label passages").unwrap();

    let mut settings = Settings {
        last_directory: Some(root.clone()),
        ..Default::default()
    };
    // The state under test: a model is chosen, generation is off, and no
    // generator is loaded — so nothing below can run one.
    settings.generation.enabled = false;
    settings.generation.model = Some(wilkes_core::types::GeneratorModel(
        "cached-label-model".to_string(),
    ));
    std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();
    assert!(ctx.generator.lock().is_none());

    let mut index = SemanticIndex::create(
        dir.path(),
        "cached-label-embedder",
        2,
        EmbeddingEngine::Candle,
        Some(&root),
    )
    .unwrap();
    let chunks = [[1.0, 0.0], [0.99, 0.01], [0.0, 1.0], [0.01, 0.99]]
        .into_iter()
        .enumerate()
        .map(|(number, vector)| {
            let text = format!("cached label passage {number}");
            (
                wilkes_core::embed::index::chunk::Chunk {
                    file_path: document.clone(),
                    text: text.clone(),
                    byte_range: ByteRange {
                        start: 0,
                        end: text.len(),
                    },
                    origin: SourceOrigin::TextFile {
                        line: (number + 1) as u32,
                        col: 1,
                    },
                },
                vector.to_vec(),
            )
        })
        .collect();
    index
        .write_file(wilkes_core::embed::index::db::PreparedFile {
            retained: Default::default(),
            full_text: String::new(),
            path: document.clone(),
            chunks,
        })
        .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

    let query = || ChunkTopicsQuery {
        root: root.clone(),
        path: None,
        granularity: wilkes_core::types::BookmarkClusterGranularity::MuchFewer,
    };

    // Nothing is cached yet, and nothing can be generated, so the cloud
    // comes back unnamed. This is the state the user sees today.
    let first = Arc::clone(&ctx)
        .chunk_topics("uncached".to_string(), query())
        .await
        .unwrap();
    assert!(!first.topics.is_empty());
    assert!(first.topics.iter().all(|topic| topic.label.is_none()));

    // Stand in for an earlier run that had generation on.
    {
        let store = ctx.research_store().unwrap();
        let mut store = store.lock().unwrap();
        for topic in &first.topics {
            store
                .upsert_chunk_cluster_label(
                    &topic.cluster_key,
                    "Cached Label Passages",
                    "cached-label-model",
                    CHUNK_CLUSTER_LABEL_RECIPE_VERSION,
                )
                .unwrap();
        }
    }

    let second = Arc::clone(&ctx)
        .chunk_topics("cached".to_string(), query())
        .await
        .unwrap();
    assert_eq!(second.topics.len(), first.topics.len());
    assert!(
        second
            .topics
            .iter()
            .all(|topic| topic.label.as_deref() == Some("Cached Label Passages")),
        "generation is off, but the names it produced earlier are still readable: {:?}",
        second.topics.iter().map(|t| &t.label).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn document_topics_report_full_library_coverage_without_changing_membership() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("document-coverage-root");
    let other_root = dir.path().join("document-coverage-other-root");
    let stale_root = dir.path().join("document-coverage-stale-root");
    let unindexed_root = dir.path().join("document-coverage-unindexed-root");
    for path in [&root, &other_root, &stale_root, &unindexed_root] {
        std::fs::create_dir_all(path).unwrap();
    }
    let source = root.join("source.txt");
    let matching = other_root.join("matching.txt");
    let unrelated = other_root.join("unrelated.txt");
    let stale_match = stale_root.join("stale-match.txt");
    for path in [&source, &matching, &unrelated, &stale_match] {
        std::fs::write(path, "indexed passage").unwrap();
    }
    let settings = Settings {
        last_directory: Some(root.clone()),
        favorites: vec![other_root.clone(), unindexed_root],
        ..Default::default()
    };
    std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

    let mut index = SemanticIndex::create(
        dir.path(),
        "document-coverage-model",
        2,
        EmbeddingEngine::Candle,
        Some(&root),
    )
    .unwrap();
    let source_chunks = [
        [1.0, 0.0],
        [1.0, 0.0],
        [1.0, 0.0],
        [0.0, 1.0],
        [0.0, 1.0],
        [0.0, 1.0],
    ]
    .into_iter()
    .enumerate()
    .map(|(number, embedding)| {
        let text = format!("source passage {number}");
        (
            wilkes_core::embed::index::chunk::Chunk {
                file_path: source.clone(),
                text: text.clone(),
                byte_range: ByteRange {
                    start: 0,
                    end: text.len(),
                },
                origin: SourceOrigin::TextFile {
                    line: (number + 1) as u32,
                    col: 1,
                },
            },
            embedding.to_vec(),
        )
    })
    .collect();
    index
        .write_file(wilkes_core::embed::index::db::PreparedFile {
            retained: Default::default(),
            full_text: String::new(),
            path: source.clone(),
            chunks: source_chunks,
        })
        .unwrap();
    index.activate_root(&other_root).unwrap();
    for (path, embedding) in [
        (matching.clone(), vec![1.0, 0.0]),
        (unrelated.clone(), vec![-1.0, 0.0]),
    ] {
        index
            .write_file(wilkes_core::embed::index::db::PreparedFile {
                retained: Default::default(),
                full_text: String::new(),
                path: path.clone(),
                chunks: vec![(
                    wilkes_core::embed::index::chunk::Chunk {
                        file_path: path,
                        text: "other passage".to_string(),
                        byte_range: ByteRange { start: 0, end: 13 },
                        origin: SourceOrigin::TextFile { line: 1, col: 1 },
                    },
                    embedding,
                )],
            })
            .unwrap();
    }
    index.activate_root(&stale_root).unwrap();
    index
        .write_file(wilkes_core::embed::index::db::PreparedFile {
            retained: Default::default(),
            full_text: String::new(),
            path: stale_match.clone(),
            chunks: vec![(
                wilkes_core::embed::index::chunk::Chunk {
                    file_path: stale_match,
                    text: "stale matching passage".to_string(),
                    byte_range: ByteRange { start: 0, end: 22 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0, 0.0],
            )],
        })
        .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

    let result = Arc::clone(&ctx)
        .chunk_topics(
            "document-coverage".to_string(),
            ChunkTopicsQuery {
                root,
                path: Some(source.clone()),
                granularity: wilkes_core::types::BookmarkClusterGranularity::MuchFewer,
            },
        )
        .await
        .unwrap();

    assert_eq!(result.topics.len(), 2);
    let canonical_source = source.canonicalize().unwrap();
    assert!(result
        .topics
        .iter()
        .flat_map(|topic| &topic.chunks)
        .all(|chunk| chunk.file_path == canonical_source));
    let mut related_counts = result
        .topics
        .iter()
        .map(|topic| {
            let coverage = topic.library_coverage.as_ref().unwrap();
            assert_eq!(coverage.eligible_document_count, 2);
            (
                coverage.related_document_count,
                coverage
                    .chunks
                    .iter()
                    .map(|chunk| chunk.file_path.clone())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    related_counts.sort_by_key(|(count, _)| *count);
    assert_eq!(related_counts[0], (0, Vec::new()));
    assert_eq!(related_counts[1].0, 1);
    assert_eq!(related_counts[1].1, vec![matching.canonicalize().unwrap()]);
}

#[test]
fn cancelling_one_topic_request_does_not_cancel_another() {
    let (_dir, ctx) = test_ctx();
    let (_, first_flag) = ctx.start_topic_operation("first").unwrap();
    let (_, second_flag) = ctx.start_topic_operation("second").unwrap();

    ctx.cancel_chunk_topics("first");

    assert!(first_flag.load(Ordering::Relaxed));
    assert!(!second_flag.load(Ordering::Relaxed));
    assert!(ctx.topic_operations.lock().contains_key("second"));
}

#[test]
fn chunk_cluster_identity_depends_only_on_the_member_set() {
    assert_eq!(chunk_cluster_key([3, 1, 2]), chunk_cluster_key([2, 3, 1]));
    assert_ne!(chunk_cluster_key([1, 2]), chunk_cluster_key([1, 3]));
}
