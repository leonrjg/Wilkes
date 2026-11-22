use super::*;

#[test]
fn topic_chunks_bulk_read_is_root_scoped_and_includes_passage_metadata() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    let path = root.join("notes.txt");
    fs::write(&path, "first second").unwrap();
    let mut idx =
        SemanticIndex::create(dir.path(), "m", 3, EmbeddingEngine::Candle, Some(&root)).unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: path.clone(),
        chunks: vec![
            (
                Chunk {
                    file_path: path.clone(),
                    text: "first".into(),
                    byte_range: ByteRange { start: 0, end: 5 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0, 0.0, 0.0],
            ),
            (
                Chunk {
                    file_path: path.clone(),
                    text: "second".into(),
                    byte_range: ByteRange { start: 6, end: 12 },
                    origin: SourceOrigin::TextFile { line: 1, col: 7 },
                },
                vec![0.0, 1.0, 0.0],
            ),
        ],
    })
    .unwrap();

    let chunks = idx.topic_chunks_for_root(&root).unwrap();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].file_path, canon(&path));
    assert_eq!(chunks[0].chunk_text, "first");
    assert_eq!(chunks[0].embedding, vec![1.0, 0.0, 0.0]);
    assert!(chunks[0].chunk_id > 0);
    let file_chunks = idx.topic_chunks_for_file(&root, &path).unwrap();
    assert_eq!(file_chunks.len(), 2);
    assert!(file_chunks
        .iter()
        .all(|chunk| chunk.file_path == canon(&path)));
    assert!(idx
        .topic_chunks_for_file(&root, &root.join("missing.txt"))
        .unwrap()
        .is_empty());
    assert!(idx
        .topic_chunks_for_root(&root.join("missing"))
        .unwrap()
        .is_empty());
}

#[test]
fn topic_library_coverage_spans_configured_roots_and_excludes_stale_roots() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let other_root = dir.path().join("other-root");
    let stale_root = dir.path().join("stale-root");
    let unindexed_root = dir.path().join("unindexed-root");
    for path in [&root, &other_root, &stale_root, &unindexed_root] {
        fs::create_dir(path).unwrap();
    }
    let source = root.join("source.txt");
    let matching = root.join("matching.txt");
    let boundary = root.join("boundary.txt");
    let unrelated = root.join("unrelated.txt");
    let cross_root_match = other_root.join("cross-root-match.txt");
    let stale_match = stale_root.join("stale-match.txt");
    for path in [
        &source,
        &matching,
        &boundary,
        &unrelated,
        &cross_root_match,
        &stale_match,
    ] {
        fs::write(path, "indexed text").unwrap();
    }
    let mut index =
        SemanticIndex::create(dir.path(), "m", 2, EmbeddingEngine::Candle, Some(&root)).unwrap();
    index
        .write_file(PreparedFile {
            retained: Default::default(),
            full_text: "source".into(),
            path: source.clone(),
            chunks: vec![(test_chunk(&source, "source"), vec![1.0, 0.0])],
        })
        .unwrap();
    index
        .write_file(PreparedFile {
            retained: Default::default(),
            full_text: "matching".into(),
            path: matching.clone(),
            // More than the per-document cap still counts as one document,
            // and only the fifteen strongest passages are retained.
            chunks: (0..17)
                .map(|number| {
                    (
                        test_chunk(&matching, &format!("matching {number:02}")),
                        vec![1.0, number as f32 * 0.01],
                    )
                })
                .collect(),
        })
        .unwrap();
    index
        .write_file(PreparedFile {
            retained: Default::default(),
            full_text: "boundary".into(),
            path: boundary.clone(),
            chunks: vec![(test_chunk(&boundary, "boundary"), vec![0.8, 0.6])],
        })
        .unwrap();
    index
        .write_file(PreparedFile {
            retained: Default::default(),
            full_text: "unrelated".into(),
            path: unrelated.clone(),
            chunks: vec![(test_chunk(&unrelated, "unrelated"), vec![0.0, 1.0])],
        })
        .unwrap();
    index.activate_root(&other_root).unwrap();
    index
        .write_file(PreparedFile {
            retained: Default::default(),
            full_text: "cross root".into(),
            path: cross_root_match.clone(),
            chunks: vec![(test_chunk(&cross_root_match, "cross root"), vec![1.0, 0.0])],
        })
        .unwrap();
    index.activate_root(&stale_root).unwrap();
    index
        .write_file(PreparedFile {
            retained: Default::default(),
            full_text: "stale".into(),
            path: stale_match.clone(),
            chunks: vec![(test_chunk(&stale_match, "stale"), vec![1.0, 0.0])],
        })
        .unwrap();

    let coverage = index
        .topic_library_coverage(
            &[root.clone(), root.clone(), other_root, unindexed_root],
            &source,
            &[
                TopicCoveragePrototype {
                    mean_member_embedding: vec![1.0, 0.0],
                    cohesion: 0.8,
                },
                TopicCoveragePrototype {
                    mean_member_embedding: vec![0.0, 1.0],
                    cohesion: 0.9,
                },
            ],
            &AtomicBool::new(false),
        )
        .unwrap();

    assert_eq!(coverage.eligible_document_count, 4);
    assert_eq!(coverage.related_document_counts, vec![3, 1]);
    let matching_chunks = coverage.related_chunks[0]
        .iter()
        .filter(|chunk| chunk.file_path == canon(&matching))
        .map(|chunk| chunk.chunk_text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(matching_chunks.len(), 15);
    assert_eq!(matching_chunks.first(), Some(&"matching 00"));
    assert_eq!(matching_chunks.last(), Some(&"matching 14"));
    assert!(!coverage.related_chunks[0]
        .iter()
        .any(|chunk| chunk.chunk_text == "matching 15" || chunk.chunk_text == "matching 16"));
    assert_eq!(coverage.related_chunks[1].len(), 1);
    assert_eq!(coverage.related_chunks[1][0].chunk_text, "unrelated");

    let cancelled = AtomicBool::new(true);
    let error = index
        .topic_library_coverage(
            std::slice::from_ref(&root),
            &source,
            &[TopicCoveragePrototype {
                mean_member_embedding: vec![1.0, 0.0],
                cohesion: 0.8,
            }],
            &cancelled,
        )
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
}
