use super::*;

/// Two indexes in one space adopt nothing when they chunk differently,
/// and the index can say that is what happened.
///
/// This is the case behind "0 of 605 chunks adopted" on a document another
/// workspace had already embedded under the very same model. Nothing is
/// refusing: the boundaries moved, so the passages are different passages,
/// and their vectors are for spans this corpus does not have. The number
/// alone cannot distinguish that from a space mismatch or from a document
/// the source has never seen, so `renditions_for_source` supplies what
/// does.
#[test]
fn a_source_that_chunked_differently_is_reported_as_such() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let file = root.join("book.txt");
    fs::write(&file, "one passage").unwrap();

    let identity = EmbeddingSpaceIdentity::with_artifact_revision(
        EmbeddingEngine::Candle,
        "model",
        1,
        "artifact-sha256:one".to_string(),
    );
    let mut source = SemanticIndex::create_exact(root, &identity, Some(root)).unwrap();
    let their_recipe = ExtractionRecipe::new(400, 0);
    source
        .write_file_with_recipe(
            PreparedFile {
                retained: Default::default(),
                path: file.clone(),
                full_text: "one passage".to_string(),
                chunks: vec![(test_chunk(&file, "one passage"), vec![0.5])],
            },
            &their_recipe,
            None,
            None,
            true,
            false,
            None,
        )
        .unwrap();
    let source_sha256 = sha256_file(&file).unwrap();

    // The corpus asks for text it chunked its own way, and gets nothing —
    // same space, same document, different spans.
    let wanted = vec![sha256_bytes(b"one pass"), sha256_bytes(b"age")];
    assert!(source.vectors_for_text_hashes(&wanted).unwrap().is_empty());

    // And the index can say why: it holds this document, under a recipe
    // that is not the asking corpus's.
    let renditions = source.renditions_for_source(&source_sha256).unwrap();
    assert_eq!(renditions.len(), 1);
    assert_eq!(renditions[0].0, their_recipe.id());
    assert_ne!(renditions[0].0, ExtractionRecipe::new(600, 128).id());
    assert_eq!(renditions[0].1, 1);

    // A document it has never seen reads differently, which is the other
    // thing the bare count could have meant.
    assert!(source
        .renditions_for_source(&sha256_bytes(b"never indexed"))
        .unwrap()
        .is_empty());
}

/// A chunk written with no vector gets no `vec_chunks` row, and the
/// passage is indexed all the same.
///
/// This is what "the canonical corpus holds no vectors" is in the schema.
/// The corpus owns retained sources, extraction, chunking and stable
/// passage identity; its projections own coordinates, one model each. It
/// used to embed every chunk on every import under the model it was
/// *created* with — frozen at creation, reachable from no setting — into
/// an index that nothing read once a store served a projection.
#[test]
fn a_chunk_written_without_a_vector_is_indexed_and_stores_no_coordinates() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let file = root.join("passages.txt");
    fs::write(&file, "a passage").unwrap();

    let identity = EmbeddingSpaceIdentity::with_artifact_revision(
        EmbeddingEngine::Candle,
        "model",
        1,
        "artifact-sha256:one".to_string(),
    );
    let mut index = SemanticIndex::create_exact(root, &identity, Some(root)).unwrap();
    index
        .write_file_with_recipe(
            PreparedFile {
                retained: Default::default(),
                path: file.clone(),
                full_text: "a passage".to_string(),
                chunks: vec![(test_chunk(&file, "a passage"), Vec::new())],
            },
            &ExtractionRecipe::new(100, 0),
            None,
            None,
            true,
            false,
            None,
        )
        .unwrap();

    let chunks: i64 = index
        .conn
        .query_row("SELECT count(*) FROM chunks", [], |row| row.get(0))
        .unwrap();
    let vectors: i64 = index
        .conn
        .query_row("SELECT count(*) FROM vec_chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(chunks, 1, "the passage is indexed");
    assert_eq!(vectors, 0, "and carries no coordinates");

    // Which is exactly what completeness reports: one admitted document,
    // one required chunk, nothing embedded. A canonical corpus reads as
    // ready on membership, and this is the number that says why it must
    // not be read as coordinates.
    let (documents, required, embedded) = index.managed_completeness().unwrap();
    assert_eq!((documents, required, embedded), (1, 1, 0));

    // The chunk still carries its text hash, so a projection can find the
    // vector for it by content without this index holding one.
    let hash: Option<String> = index
        .conn
        .query_row("SELECT text_sha256 FROM chunks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(hash.as_deref(), Some(sha256_bytes(b"a passage").as_str()));
}

#[test]
fn test_rename_file_rekeys_chunks_and_keeps_embeddings() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut idx = SemanticIndex::create(root, "m", 3, EmbeddingEngine::Candle, Some(root)).unwrap();

    let old = root.join("old.txt");
    let new = root.join("new.txt");
    fs::write(&old, "hello world").unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: old.clone(),
        chunks: vec![(
            Chunk {
                file_path: old.clone(),
                text: "hello world".to_string(),
                byte_range: ByteRange { start: 0, end: 11 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![1.0, 0.0, 0.0],
        )],
    })
    .unwrap();

    fs::rename(&old, &new).unwrap();
    idx.rename_file(&old, &new).unwrap();

    // The chunk (and its embedding) survives the rename and is queryable.
    let results = idx.query(&[1.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].file_path, canon(&new));

    // It is keyed under the new path only: removing the old path is a no-op,
    // removing the new path clears it.
    idx.remove_file(&old).unwrap();
    assert_eq!(idx.query(&[1.0, 0.0, 0.0], 1).unwrap().len(), 1);
    idx.remove_file(&new).unwrap();
    assert_eq!(idx.query(&[1.0, 0.0, 0.0], 1).unwrap().len(), 0);
}

/// The text and the positions are written together and read back joined:
/// what the reader is told about an area is the reading's own bytes at the
/// rectangle they were read from.
#[test]
fn superseded_areas_survive_a_round_trip_and_a_reindex() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut idx = SemanticIndex::create(root, "m", 3, EmbeddingEngine::Candle, Some(root)).unwrap();

    let path = root.join("paper.pdf");
    fs::write(&path, "pdf bytes").unwrap();
    let full_text = "before\nPage formula: y_{B} = w mod q.\nafter".to_string();
    let start = full_text.find("y_{B}").unwrap();
    let end = start + "y_{B} = w mod q".len();
    let prepared = |reading_regions: Vec<ReadingRegion>| PreparedFile {
        path: path.clone(),
        full_text: full_text.clone(),
        retained: RetainedExtraction {
            reading_regions,
            ..Default::default()
        },
        chunks: vec![(
            Chunk {
                file_path: path.clone(),
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
            vec![1.0, 0.0, 0.0],
        )],
    };

    idx.write_file(prepared(vec![formula_region(start, end)]))
        .unwrap();
    let areas = idx.superseded_areas_for_path(&path).unwrap();
    assert_eq!(areas.len(), 1);
    assert_eq!(areas[0].page, 1);
    assert_eq!(areas[0].text, "y_{B} = w mod q");
    assert!(!areas[0].text.contains("Page formula:"));

    // A second rendition replaces them. Left behind, the old rectangles
    // would point at whatever the new text happens to hold there.
    idx.write_file(prepared(Vec::new())).unwrap();
    assert!(idx.superseded_areas_for_path(&path).unwrap().is_empty());
}

/// A file edited since it was read is served nothing: the rectangles
/// describe a rendition of a document that is no longer on disk, and would
/// land on whatever is drawn there now.
#[test]
fn superseded_areas_are_withheld_for_a_changed_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut idx = SemanticIndex::create(root, "m", 3, EmbeddingEngine::Candle, Some(root)).unwrap();

    let path = root.join("paper.pdf");
    fs::write(&path, "pdf bytes").unwrap();
    let full_text = "x = 1".to_string();
    idx.write_file(PreparedFile {
        path: path.clone(),
        full_text: full_text.clone(),
        retained: RetainedExtraction {
            reading_regions: vec![formula_region(0, full_text.len())],
            ..Default::default()
        },
        chunks: vec![(
            Chunk {
                file_path: path.clone(),
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
            vec![1.0, 0.0, 0.0],
        )],
    })
    .unwrap();
    assert_eq!(idx.superseded_areas_for_path(&path).unwrap().len(), 1);

    fs::write(&path, "a different document entirely").unwrap();
    assert!(idx.superseded_areas_for_path(&path).unwrap().is_empty());
    // And a document the index has never read has none to give.
    assert!(idx
        .superseded_areas_for_path(&root.join("unknown.pdf"))
        .unwrap()
        .is_empty());
}

#[test]
fn indexed_document_for_path_serves_stored_text_and_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut idx = SemanticIndex::create(root, "m", 3, EmbeddingEngine::Candle, Some(root)).unwrap();

    let path = root.join("doc.txt");
    fs::write(&path, "the quick brown fox").unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        path: path.clone(),
        full_text: "the quick brown fox".to_string(),
        chunks: vec![(
            Chunk {
                file_path: path.clone(),
                text: "the quick brown fox".to_string(),
                byte_range: ByteRange { start: 0, end: 19 },
                origin: SourceOrigin::PdfPage {
                    page: 2,
                    bbox: None,
                },
            },
            vec![1.0, 0.0, 0.0],
        )],
    })
    .unwrap();

    // Happy path: stored text and a chunk-granular source map come back, and
    // the map resolves a match offset to the chunk's page.
    let (text, map) = idx
        .indexed_document_for_path(&path)
        .unwrap()
        .expect("indexed document should be served");
    assert_eq!(text, "the quick brown fox");
    match map.resolve_range(ByteRange { start: 4, end: 9 }) {
        Some(SourceOrigin::PdfPage { page, .. }) => assert_eq!(page, 2),
        other => panic!("expected page-2 origin, got {other:?}"),
    }

    // Stale on-disk change: identity no longer matches, so the caller is told
    // to extract live rather than be served vanished content.
    fs::write(&path, "completely different content now").unwrap();
    assert!(idx.indexed_document_for_path(&path).unwrap().is_none());

    // Absent file: nothing to serve.
    assert!(idx
        .indexed_document_for_path(&root.join("missing.txt"))
        .unwrap()
        .is_none());
}

#[test]
fn indexed_document_for_path_none_when_full_text_missing_or_inconsistent() {
    // Simulates a row written before schema v4: chunks exist but full_text is
    // NULL, so grep must fall back to live extraction.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut idx = SemanticIndex::create(root, "m", 3, EmbeddingEngine::Candle, Some(root)).unwrap();

    let path = root.join("legacy.txt");
    fs::write(&path, "legacy body").unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        path: path.clone(),
        full_text: "legacy body".to_string(),
        chunks: vec![(
            Chunk {
                file_path: path.clone(),
                text: "legacy body".to_string(),
                byte_range: ByteRange { start: 0, end: 11 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![1.0, 0.0, 0.0],
        )],
    })
    .unwrap();

    // Force the column back to NULL to emulate a pre-v4 row.
    idx.conn
        .execute("UPDATE files SET full_text = NULL", [])
        .unwrap();

    assert!(idx.indexed_document_for_path(&path).unwrap().is_none());

    // Older rebuilds coerced that NULL to an empty string while retaining
    // chunks. This is still missing text, not a genuinely empty document.
    idx.conn
        .execute("UPDATE files SET full_text = ''", [])
        .unwrap();

    assert!(idx.indexed_document_for_path(&path).unwrap().is_none());
}

#[test]
fn test_rename_directory_rekeys_descendant_chunks_and_keeps_embeddings() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut idx = SemanticIndex::create(root, "m", 3, EmbeddingEngine::Candle, Some(root)).unwrap();

    let old_dir = root.join("old");
    fs::create_dir(&old_dir).unwrap();
    let old_file = old_dir.join("paper.txt");
    fs::write(&old_file, "hello world").unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: old_file.clone(),
        chunks: vec![(
            Chunk {
                file_path: old_file.clone(),
                text: "hello world".to_string(),
                byte_range: ByteRange { start: 0, end: 11 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![1.0, 0.0, 0.0],
        )],
    })
    .unwrap();

    let new_dir = root.join("new");
    fs::rename(&old_dir, &new_dir).unwrap();
    idx.rename_file(&old_dir, &new_dir).unwrap();

    // The descendant's chunk survives under the renamed directory.
    let new_file = new_dir.join("paper.txt");
    let results = idx.query(&[1.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].file_path, canon(&new_file));

    // Keyed under the new path only.
    idx.remove_file(&old_file).unwrap();
    assert_eq!(idx.query(&[1.0, 0.0, 0.0], 1).unwrap().len(), 1);
    idx.remove_file(&new_file).unwrap();
    assert_eq!(idx.query(&[1.0, 0.0, 0.0], 1).unwrap().len(), 0);
}

#[test]
fn test_to_rel_abs_path() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let index = SemanticIndex {
        conn: Connection::open_in_memory().unwrap(),
        dimension: 1,
        active_root: None,
        active_root_id: None,
    };

    let abs = root.join("subdir/file.txt");
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, "hello").unwrap();
    let rel = index.path_key_for_existing_path(&abs);
    assert_eq!(rel, canon(&abs));

    let abs2 = index.key_to_display_path(&abs.to_string_lossy());
    assert_eq!(abs2, abs);

    // Path outside root
    let outside = Path::new("/other/file.txt");
    let rel_outside = index.path_key_for_known_path(outside);
    assert_eq!(rel_outside, outside);
}

#[test]
fn test_write_file_pdf_origin() {
    let dir = tempdir().unwrap();
    let mut idx = SemanticIndex::create(dir.path(), "m", 1, EmbeddingEngine::Candle, None).unwrap();

    let path = dir.path().join("test.pdf");
    fs::write(&path, "page content").unwrap();
    let prepared = PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: path.clone(),
        chunks: vec![(
            Chunk {
                file_path: path.clone(),
                text: "page content".to_string(),
                byte_range: ByteRange { start: 0, end: 12 },
                origin: SourceOrigin::PdfPage {
                    page: 5,
                    bbox: Some(BoundingBox {
                        x: 1.0,
                        y: 2.0,
                        width: 3.0,
                        height: 4.0,
                    }),
                },
            },
            vec![1.0],
        )],
    };

    idx.write_file(prepared).unwrap();

    let results = idx.query(&[1.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    match &results[0].origin {
        SourceOrigin::PdfPage { page, bbox } => {
            assert_eq!(*page, 5);
            let b = bbox.as_ref().unwrap();
            assert_eq!(b.x, 1.0);
            assert_eq!(b.y, 2.0);
        }
        _ => panic!("Expected PdfPage origin"),
    }
}

#[test]
fn test_write_file_is_atomic_on_failure() {
    let dir = tempdir().unwrap();
    let mut idx = SemanticIndex::create(dir.path(), "m", 1, EmbeddingEngine::Candle, None).unwrap();

    let path = dir.path().join("test.txt");
    fs::write(&path, "original").unwrap();
    let original = PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: path.clone(),
        chunks: vec![(
            Chunk {
                file_path: path.clone(),
                text: "original".to_string(),
                byte_range: ByteRange { start: 0, end: 8 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![1.0],
        )],
    };
    idx.write_file(original).unwrap();

    let replacement = PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: path.clone(),
        chunks: vec![(
            Chunk {
                file_path: path.clone(),
                text: "replacement".to_string(),
                byte_range: ByteRange { start: 0, end: 11 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![1.0, 2.0],
        )],
    };
    assert!(idx.write_file(replacement).is_err());

    let results = idx.query(&[1.0], 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_text, "original");
}
