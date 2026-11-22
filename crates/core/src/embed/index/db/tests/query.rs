use super::*;

#[test]
fn test_write_and_query() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let model = "test-model";
    let dim = 3;
    let engine = EmbeddingEngine::Candle;

    let mut idx = SemanticIndex::create(dir.path(), model, dim, engine, Some(root)).unwrap();

    let file_path = root.join("test.txt");
    fs::write(&file_path, "hello world\nfoo bar").unwrap();
    let prepared = PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: file_path.clone(),
        chunks: vec![
            (
                Chunk {
                    file_path: file_path.clone(),
                    text: "hello world".to_string(),
                    byte_range: ByteRange { start: 0, end: 11 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0, 0.0, 0.0],
            ),
            (
                Chunk {
                    file_path: file_path.clone(),
                    text: "foo bar".to_string(),
                    byte_range: ByteRange { start: 12, end: 19 },
                    origin: SourceOrigin::TextFile { line: 2, col: 1 },
                },
                vec![0.0, 1.0, 0.0],
            ),
        ],
    };

    idx.write_file(prepared).unwrap();

    // Query for "hello" (vec [1, 0, 0])
    let results = idx.query(&[1.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_text, "hello world");
    assert!(results[0].score > 0.99);

    // Query for "foo" (vec [0, 1, 0])
    let results2 = idx.query(&[0.0, 1.0, 0.0], 1).unwrap();
    assert_eq!(results2.len(), 1);
    assert_eq!(results2[0].chunk_text, "foo bar");

    // Remove file
    idx.remove_file(&file_path).unwrap();
    let results3 = idx.query(&[1.0, 0.0, 0.0], 10).unwrap();
    assert_eq!(results3.len(), 0);
}

#[test]
fn test_query_file_scope_selects_within_file_before_top_k() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut idx = SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
    let scoped_path = root.join("scoped.txt");
    let other_path = root.join("other.txt");
    fs::write(&scoped_path, "scoped chunk").unwrap();
    fs::write(&other_path, "other chunk").unwrap();

    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: scoped_path.clone(),
        chunks: vec![(
            Chunk {
                file_path: scoped_path.clone(),
                text: "scoped chunk".to_string(),
                byte_range: ByteRange { start: 0, end: 12 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![0.0, 1.0],
        )],
    })
    .unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: other_path.clone(),
        chunks: vec![(
            Chunk {
                file_path: other_path.clone(),
                text: "other chunk".to_string(),
                byte_range: ByteRange { start: 0, end: 11 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![1.0, 0.0],
        )],
    })
    .unwrap();

    let global = idx.query(&[1.0, 0.0], 1).unwrap();
    assert_eq!(global.len(), 1);
    assert_eq!(global[0].file_path, canon(&other_path));

    let eligible = std::collections::HashSet::from([canon(&scoped_path)]);
    let filtered = idx
        .query_scoped_filtered(
            &[1.0, 0.0],
            1,
            SemanticQueryScope::Corpus,
            Some(&eligible),
            None,
        )
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].file_path, canon(&scoped_path));

    let excluded = std::collections::HashSet::from([canon(&other_path)]);
    let without_other = idx
        .query_scoped_filtered(
            &[1.0, 0.0],
            1,
            SemanticQueryScope::Corpus,
            None,
            Some(&excluded),
        )
        .unwrap();
    assert_eq!(without_other.len(), 1);
    assert_eq!(without_other[0].file_path, canon(&scoped_path));

    let scoped = idx
        .query_scoped(&[1.0, 0.0], 1, SemanticQueryScope::File(&scoped_path))
        .unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].file_path, canon(&scoped_path));
    assert_eq!(scoped[0].chunk_text, "scoped chunk");
}

#[test]
fn test_related_documents_ranks_by_centroid_similarity_and_filters() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut idx = SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
    let source = root.join("source.txt");
    let close = root.join("close.txt");
    let far = root.join("far.txt");
    let unsupported = root.join("image.bin");
    for path in [&source, &close, &far, &unsupported] {
        fs::write(path, "content").unwrap();
    }

    let chunk = |path: &Path, text: &str| Chunk {
        file_path: path.to_path_buf(),
        text: text.to_string(),
        byte_range: ByteRange {
            start: 0,
            end: text.len(),
        },
        origin: SourceOrigin::TextFile { line: 1, col: 1 },
    };

    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: source.clone(),
        chunks: vec![
            (chunk(&source, "source one"), vec![1.0, 0.0]),
            (chunk(&source, "source two"), vec![1.0, 0.0]),
        ],
    })
    .unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: close.clone(),
        chunks: vec![(chunk(&close, "close"), vec![0.9, 0.1])],
    })
    .unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: far.clone(),
        chunks: vec![(chunk(&far, "far"), vec![0.0, 1.0])],
    })
    .unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: unsupported.clone(),
        chunks: vec![(chunk(&unsupported, "unsupported"), vec![1.0, 0.0])],
    })
    .unwrap();

    let related = idx
        .related_documents(root, &source, 10, &["txt".to_string()], false)
        .unwrap();

    assert_eq!(
        related
            .iter()
            .map(|doc| doc.entry.path.clone())
            .collect::<Vec<_>>(),
        vec![canon(&close), canon(&far)]
    );
    assert!(related[0].score > related[1].score);
    assert!(related[0].entry.size_bytes > 0);

    let eligible = std::collections::HashSet::from([canon(&far)]);
    let filtered = idx
        .related_documents_filtered(
            root,
            &source,
            1,
            &["txt".to_string()],
            false,
            Some(&eligible),
        )
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].entry.path, canon(&far));
}

#[test]
fn test_related_documents_missing_source_errors() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let idx = SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
    let source = root.join("source.txt");
    fs::write(&source, "content").unwrap();

    let err = idx
        .related_documents(root, &source, 10, &["txt".to_string()], false)
        .unwrap_err();

    assert!(err.to_string().contains("not present"));
}

#[test]
fn test_related_documents_can_search_the_whole_index() {
    let dir = tempdir().unwrap();
    let first_root = dir.path().join("first");
    let second_root = dir.path().join("second");
    fs::create_dir_all(&first_root).unwrap();
    fs::create_dir_all(&second_root).unwrap();
    let source = first_root.join("source.txt");
    let other = second_root.join("other.txt");
    let stale = second_root.join("stale.txt");
    fs::write(&source, "source").unwrap();
    fs::write(&other, "other").unwrap();
    fs::write(&stale, "stale").unwrap();

    let mut idx = SemanticIndex::create(
        dir.path(),
        "m",
        2,
        EmbeddingEngine::Candle,
        Some(&first_root),
    )
    .unwrap();
    let prepared = |path: &Path, text: &str| PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: path.to_path_buf(),
        chunks: vec![(
            Chunk {
                file_path: path.to_path_buf(),
                text: text.to_string(),
                byte_range: ByteRange {
                    start: 0,
                    end: text.len(),
                },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![1.0, 0.0],
        )],
    };
    idx.write_file(prepared(&source, "source")).unwrap();
    idx.activate_root(&second_root).unwrap();
    idx.write_file(prepared(&other, "other")).unwrap();
    idx.write_file(prepared(&stale, "stale")).unwrap();
    fs::remove_file(&stale).unwrap();

    let current = idx
        .related_documents(&first_root, &source, 10, &["txt".to_string()], false)
        .unwrap();
    let whole_library = idx
        .related_documents(&first_root, &source, 10, &["txt".to_string()], true)
        .unwrap();

    assert!(current.is_empty());
    assert_eq!(whole_library.len(), 1);
    assert_eq!(whole_library[0].entry.path, canon(&other));
}

#[test]
fn document_vectors_average_normalised_chunks_across_roots_and_omit_unindexed_files() {
    let dir = tempdir().unwrap();
    let first_root = dir.path().join("first");
    let second_root = dir.path().join("second");
    fs::create_dir_all(&first_root).unwrap();
    fs::create_dir_all(&second_root).unwrap();
    let two_chunks = first_root.join("two.txt");
    let elsewhere = second_root.join("elsewhere.txt");
    let unindexed = second_root.join("unindexed.txt");
    for path in [&two_chunks, &elsewhere, &unindexed] {
        fs::write(path, "text").unwrap();
    }
    let chunk = |path: &Path, vector: Vec<f32>| {
        (
            Chunk {
                file_path: path.to_path_buf(),
                text: "text".to_string(),
                byte_range: ByteRange { start: 0, end: 4 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vector,
        )
    };
    let mut idx = SemanticIndex::create(
        dir.path(),
        "m",
        2,
        EmbeddingEngine::Candle,
        Some(&first_root),
    )
    .unwrap();
    // A long chunk and a short one count the same once normalised.
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: two_chunks.clone(),
        chunks: vec![
            chunk(&two_chunks, vec![4.0, 0.0]),
            chunk(&two_chunks, vec![0.0, 0.5]),
        ],
    })
    .unwrap();
    idx.activate_root(&second_root).unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: elsewhere.clone(),
        chunks: vec![chunk(&elsewhere, vec![0.0, 2.0])],
    })
    .unwrap();

    let vectors = idx
        .document_vectors(&[two_chunks.clone(), elsewhere.clone(), unindexed.clone()])
        .unwrap();

    assert_eq!(vectors.len(), 2);
    assert_eq!(vectors[&two_chunks], vec![0.5, 0.5]);
    assert_eq!(vectors[&elsewhere], vec![0.0, 1.0]);
    assert!(!vectors.contains_key(&unindexed));
}

#[test]
fn test_query_dimension_mismatch() {
    let dir = tempdir().unwrap();
    let idx = SemanticIndex::create(dir.path(), "m", 1, EmbeddingEngine::Candle, None).unwrap();
    let res = idx.query(&[1.0, 2.0], 1);
    match res {
        Err(e) => assert!(e
            .to_string()
            .contains("Expected 1 dimensions but received 2")),
        Ok(_) => panic!("Expected query dimension mismatch"),
    }
}

#[test]
fn test_query_skips_unknown_origin_types() {
    let dir = tempdir().unwrap();
    let mut idx = SemanticIndex::create(dir.path(), "m", 1, EmbeddingEngine::Candle, None).unwrap();

    let path = dir.path().join("mystery.txt");
    fs::write(&path, "mystery").unwrap();
    let prepared = PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: path.clone(),
        chunks: vec![(
            Chunk {
                file_path: path.clone(),
                text: "mystery".to_string(),
                byte_range: ByteRange { start: 0, end: 7 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![1.0],
        )],
    };
    idx.write_file(prepared).unwrap();

    idx.conn
        .execute("UPDATE chunks SET origin_type = 'mystery'", [])
        .unwrap();

    let results = idx.query(&[1.0], 1).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_root_query_is_scoped_by_root_files() {
    let dir = tempdir().unwrap();
    let root_a = dir.path().join("a");
    let root_b = dir.path().join("b");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();
    let a = root_a.join("a.txt");
    let b = root_b.join("b.txt");
    fs::write(&a, "alpha").unwrap();
    fs::write(&b, "beta").unwrap();

    let mut idx =
        SemanticIndex::create(dir.path(), "m", 2, EmbeddingEngine::Candle, Some(&root_a)).unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: a.clone(),
        chunks: vec![(
            test_chunk(&a, "close outside requested root"),
            vec![1.0, 0.0],
        )],
    })
    .unwrap();
    idx.activate_root(&root_b).unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: b.clone(),
        chunks: vec![(test_chunk(&b, "inside requested root"), vec![0.5, 0.5])],
    })
    .unwrap();

    let root_results = idx
        .query_scoped(&[1.0, 0.0], 10, SemanticQueryScope::Root(&root_b))
        .unwrap();
    assert_eq!(root_results.len(), 1);
    assert_eq!(root_results[0].file_path, canon(&b));

    let corpus_results = idx.query(&[1.0, 0.0], 1).unwrap();
    assert_eq!(corpus_results[0].file_path, canon(&a));
}
