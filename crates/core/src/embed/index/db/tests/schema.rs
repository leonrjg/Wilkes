use super::*;

#[test]
fn test_open_missing_error() {
    let dir = tempfile::tempdir().unwrap();
    let res = SemanticIndex::open(dir.path(), "any", 0);
    assert!(res.is_err());
}

#[test]
fn test_create_and_open() {
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new("/search/root");
    let model = "test-model";
    let dim = 128;
    let engine = EmbeddingEngine::Candle;

    // Create
    let idx = SemanticIndex::create(dir.path(), model, dim, engine, Some(root)).unwrap();
    assert_eq!(idx.status().model_id, model);
    assert_eq!(idx.dimension, dim);
    assert!(idx.active_root.is_some());
    assert!(idx.active_root_id.is_some());
    drop(idx);

    // Open
    let idx2 = SemanticIndex::open(dir.path(), model, dim).unwrap();
    assert_eq!(idx2.status().model_id, model);
    assert_eq!(idx2.dimension, dim);
    assert!(idx2.active_root.is_none());
}

#[test]
fn test_open_v4_normalizes_empty_full_text_only_when_chunks_exist() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let legacy_path = root.join("legacy.txt");
    let empty_path = root.join("empty.txt");
    fs::write(&legacy_path, "legacy body").unwrap();
    fs::write(&empty_path, "").unwrap();

    let mut idx = SemanticIndex::create(root, "m", 1, EmbeddingEngine::Candle, Some(root)).unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        path: legacy_path.clone(),
        full_text: String::new(),
        chunks: vec![(test_chunk(&legacy_path, "legacy body"), vec![1.0])],
    })
    .unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        path: empty_path.clone(),
        full_text: String::new(),
        chunks: Vec::new(),
    })
    .unwrap();
    idx.conn
        .execute(
            "UPDATE meta SET value = '4' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    drop(idx);

    let idx = SemanticIndex::open(root, "m", 1).unwrap();
    let stored_text = |path: &Path| -> Option<String> {
        idx.conn
            .query_row(
                "SELECT full_text FROM files WHERE file_path = ?1",
                params![canon(path).to_string_lossy()],
                |row| row.get(0),
            )
            .unwrap()
    };

    assert_eq!(stored_text(&legacy_path), None);
    assert_eq!(stored_text(&empty_path), Some(String::new()));
    // Kept, where it used to be filtered away for naming no artifacts.
    assert!(idx.embedding_metadata().unwrap().exact_identity.is_some());
    assert_eq!(
        idx.conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        // The ladder runs to the end, whatever the end currently is: a
        // literal here says "migrate as far as 10", which is a different
        // claim and one that goes stale on the next migration.
        SCHEMA_VERSION.to_string()
    );
}

/// An unresolved artifact revision is recorded, not refused.
///
/// It names provenance the runtime could not fingerprint — SBERT keeps its
/// weights outside the cache root this reads — and provenance does not
/// decide whether an index can be read. Refusing meant such an engine
/// could not build an index at all, which is a large price for a field
/// nothing compares any more.
#[test]
fn creating_an_index_with_an_unresolved_identity_records_it() {
    let dir = tempdir().unwrap();
    let unresolved =
        EmbeddingSpaceIdentity::for_runtime(EmbeddingEngine::Candle, "placeholder-model", 1);
    let index = SemanticIndex::create_exact(dir.path(), &unresolved, None)
        .expect("an unresolved identity is recorded, not refused");
    assert!(db_path(dir.path()).exists());

    // Recorded verbatim, so a reader can still see what it was.
    let recorded = index
        .embedding_metadata()
        .unwrap()
        .exact_identity
        .expect("the identity is kept");
    assert!(!recorded.is_resolved());
    assert_eq!(recorded.artifact_revision, unresolved.artifact_revision);

    // And it is the same space as the resolved runtime for that model:
    // same engine, same model, same width.
    let resolved = EmbeddingSpaceIdentity::with_artifact_revision(
        EmbeddingEngine::Candle,
        "placeholder-model",
        1,
        "artifact-sha256:whatever".to_string(),
    );
    index.validate_embedding_space(&resolved).unwrap();
}

#[test]
fn v9_unresolved_identity_migrates_to_legacy_metadata_and_remains_locally_usable() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let file = root.join("legacy.txt");
    fs::write(&file, "legacy body").unwrap();
    let unresolved =
        EmbeddingSpaceIdentity::for_runtime(EmbeddingEngine::Candle, "legacy-model", 1);
    // Creating an index with an unresolved identity is refused now, so
    // reproduce the v9 on-disk shape the way it actually arose: a valid
    // index whose meta rows carry the placeholder a v9 runtime wrote.
    let mut index = SemanticIndex::create_exact(
        root,
        &EmbeddingSpaceIdentity::for_test(EmbeddingEngine::Candle, "legacy-model", 1),
        Some(root),
    )
    .unwrap();
    index
        .write_file(PreparedFile {
            retained: Default::default(),
            path: file.clone(),
            full_text: "legacy body".to_string(),
            chunks: vec![(test_chunk(&file, "legacy body"), vec![1.0])],
        })
        .unwrap();
    index
        .conn
        .execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('embedding_space_id', ?1)",
            params![unresolved.id().0],
        )
        .unwrap();
    index
        .conn
        .execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('embedding_space_identity_json', ?1)",
            params![serde_json::to_string(&unresolved).unwrap()],
        )
        .unwrap();
    index
        .conn
        .execute_batch(
            "DELETE FROM meta WHERE key = 'index_embedding_metadata_json';
                 UPDATE meta SET value = '9' WHERE key = 'schema_version';",
        )
        .unwrap();
    drop(index);

    let migrated = SemanticIndex::open(root, "legacy-model", 1).unwrap();
    let metadata = migrated.embedding_metadata().unwrap();
    assert_eq!(metadata.engine, EmbeddingEngine::Candle);
    assert_eq!(metadata.model_id, "legacy-model");
    assert_eq!(metadata.dimension, 1);
    assert!(metadata.exact_identity.is_some());
    assert_eq!(migrated.query(&[1.0], 1).unwrap().len(), 1);

    // The legacy index named the model but not the artifacts, which used
    // to make it locally readable and managed-unusable at once — two
    // verdicts about one index, from two functions. There is one now, and
    // it says the same model at the same width is the same space.
    let current = EmbeddingSpaceIdentity::with_artifact_revision(
        EmbeddingEngine::Candle,
        "legacy-model",
        1,
        "artifact-sha256:current".to_string(),
    );
    migrated.validate_embedding_space(&current).unwrap();

    // A different width is still a different space, and still refused.
    let wider = EmbeddingSpaceIdentity::with_artifact_revision(
        EmbeddingEngine::Candle,
        "legacy-model",
        2,
        "artifact-sha256:current".to_string(),
    );
    assert!(migrated.validate_embedding_space(&wider).is_err());

    // Whole-file adoption still needs the source hash to match; that is
    // structure, not coordinates, and nothing here relaxed it.
    assert!(migrated
        .verified_file_for_adoption(
            "unused",
            &ExtractionRecipe::new(100, 0),
            &current.id(),
            &file,
        )
        .unwrap()
        .is_none());
}

#[test]
fn v9_resolved_identity_migrates_without_losing_exact_evidence() {
    let dir = tempdir().unwrap();
    let identity = EmbeddingSpaceIdentity::with_artifact_revision(
        EmbeddingEngine::Fastembed,
        "exact-model",
        3,
        "artifact-sha256:resolved".to_string(),
    );
    let index = SemanticIndex::create_exact(dir.path(), &identity, None).unwrap();
    index
        .conn
        .execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('embedding_space_id', ?1)",
            params![identity.id().0],
        )
        .unwrap();
    index
        .conn
        .execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('embedding_space_identity_json', ?1)",
            params![serde_json::to_string(&identity).unwrap()],
        )
        .unwrap();
    index
        .conn
        .execute_batch(
            "DELETE FROM meta WHERE key = 'index_embedding_metadata_json';
                 UPDATE meta SET value = '9' WHERE key = 'schema_version';",
        )
        .unwrap();
    drop(index);

    let migrated = SemanticIndex::open_exact(dir.path(), &identity).unwrap();
    assert_eq!(
        migrated.embedding_metadata().unwrap().exact_identity,
        Some(identity)
    );
}

#[test]
fn interrupted_replacement_restores_the_preserved_index() {
    let dir = tempdir().unwrap();
    let index = SemanticIndex::create(
        dir.path(),
        "preserved-model",
        1,
        EmbeddingEngine::Candle,
        None,
    )
    .unwrap();
    drop(index);
    let final_path = db_path(dir.path());
    let backup_path = replacement_backup_path(dir.path());
    fs::rename(&final_path, &backup_path).unwrap();

    let restored = SemanticIndex::open(dir.path(), "preserved-model", 1).unwrap();
    assert_eq!(restored.status().model_id, "preserved-model");
    assert!(final_path.exists());
    assert!(!backup_path.exists());
}

#[test]
fn corrupt_published_replacement_rolls_back_to_the_preserved_index() {
    let dir = tempdir().unwrap();
    let index = SemanticIndex::create(
        dir.path(),
        "preserved-model",
        1,
        EmbeddingEngine::Candle,
        None,
    )
    .unwrap();
    drop(index);
    let final_path = db_path(dir.path());
    let backup_path = replacement_backup_path(dir.path());
    fs::rename(&final_path, &backup_path).unwrap();
    fs::write(&final_path, b"incomplete replacement").unwrap();

    let restored = SemanticIndex::open(dir.path(), "preserved-model", 1).unwrap();
    assert_eq!(restored.status().model_id, "preserved-model");
    assert!(!backup_path.exists());
}

/// An index built before regions were kept opens, migrates, and answers
/// that it knows of no superseded areas -- which is the same answer as
/// "this document has none", and the right one: what a recognizer read
/// cannot be recovered from a column, so those documents read exactly as
/// they did until they are next extracted.
#[test]
fn an_index_from_before_regions_were_kept_opens_and_offers_none() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut index =
        SemanticIndex::create(root, "legacy-model", 1, EmbeddingEngine::Candle, Some(root))
            .unwrap();

    let path = root.join("paper.pdf");
    fs::write(&path, "pdf bytes").unwrap();
    index
        .write_file(PreparedFile {
            path: path.clone(),
            full_text: "x = 1".to_string(),
            retained: Default::default(),
            chunks: vec![(
                Chunk {
                    file_path: path.clone(),
                    text: "x = 1".to_string(),
                    byte_range: ByteRange { start: 0, end: 5 },
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: None,
                    },
                },
                vec![1.0],
            )],
        })
        .unwrap();
    // v10 is before either kept anything: no regions table, and no
    // retained extraction on the file row.
    index
        .conn
        .execute_batch(
            "ALTER TABLE files DROP COLUMN retained_extraction_json;
                 UPDATE meta SET value = '10' WHERE key = 'schema_version';",
        )
        .unwrap();
    drop(index);

    let migrated = SemanticIndex::open(root, "legacy-model", 1).unwrap();
    assert!(migrated
        .superseded_areas_for_path(&path)
        .unwrap()
        .is_empty());
    // And a file nobody has retained anything for reads as the empty
    // default rather than failing.
    let retained = migrated.retained_extraction_for_key("nothing").unwrap();
    assert!(retained.reading_regions.is_empty());
    assert!(retained.images.is_none(), "unknown, not none");
}

/// v11 -> v12 carries the rows over rather than dropping them: they are a
/// rendition's page positions and cannot be recovered without running a
/// recognizer again. What they cannot gain is an image inventory, which is
/// why it migrates as `None` — unknown — and not as an empty list.
#[test]
fn the_v12_migration_folds_the_regions_in_and_leaves_the_inventory_unknown() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut index =
        SemanticIndex::create(root, "legacy-model", 1, EmbeddingEngine::Candle, Some(root))
            .unwrap();

    let path = root.join("paper.pdf");
    fs::write(&path, "pdf bytes").unwrap();
    let full_text = "Page formula: x = 1".to_string();
    index
        .write_file(PreparedFile {
            path: path.clone(),
            full_text: full_text.clone(),
            retained: Default::default(),
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
                vec![1.0],
            )],
        })
        .unwrap();

    // Put the store back into v11: the table, one row in it, and the
    // sidecar column gone.
    let file_id: i64 = index
        .conn
        .query_row("SELECT id FROM files LIMIT 1", [], |row| row.get(0))
        .unwrap();
    index
        .conn
        .execute_batch(
            "CREATE TABLE reading_regions (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                    area_id    TEXT    NOT NULL,
                    ordinal    INTEGER NOT NULL,
                    page       INTEGER NOT NULL,
                    bbox_x     REAL    NOT NULL,
                    bbox_y     REAL    NOT NULL,
                    bbox_w     REAL    NOT NULL,
                    bbox_h     REAL    NOT NULL,
                    byte_start INTEGER NOT NULL,
                    byte_end   INTEGER NOT NULL
                 );
                 ALTER TABLE files DROP COLUMN retained_extraction_json;
                 UPDATE meta SET value = '11' WHERE key = 'schema_version';",
        )
        .unwrap();
    index
        .conn
        .execute(
            "INSERT INTO reading_regions (file_id, area_id, ordinal, page,
                                              bbox_x, bbox_y, bbox_w, bbox_h,
                                              byte_start, byte_end)
                 VALUES (?1, 'p1-i0', 0, 1, 10.0, 20.0, 300.0, 24.0, 0, ?2)",
            params![file_id, full_text.len() as i64],
        )
        .unwrap();
    drop(index);

    let migrated = SemanticIndex::open(root, "legacy-model", 1).unwrap();
    let retained = migrated.retained_extraction_for_file(file_id).unwrap();
    assert_eq!(
        retained.reading_regions.len(),
        1,
        "the region survived the fold"
    );
    assert_eq!(retained.reading_regions[0].area_id, "p1-i0");
    assert_eq!(retained.reading_regions[0].page, 1);
    assert!(
        retained.images.is_none(),
        "a migrated rendition has no inventory and must not claim it draws nothing"
    );
    // The superseded area still resolves against the text it was cut from.
    assert_eq!(migrated.superseded_areas_for_path(&path).unwrap().len(), 1);
}

#[test]
fn test_open_legacy_schema() {
    let dir = tempdir().unwrap();
    let path = db_path(dir.path());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);")
        .unwrap();
    // Missing vec_chunks table
    drop(conn);

    let res = SemanticIndex::open(dir.path(), "any", 0);
    match res {
        Err(e) => assert!(e.to_string().contains("legacy schema")),
        Ok(_) => panic!("Expected legacy schema error"),
    }
}

#[test]
fn test_open_dimension_mismatch() {
    let dir = tempdir().unwrap();
    let model = "m1";
    let engine = EmbeddingEngine::Candle;

    // Create with dim 128
    SemanticIndex::create(dir.path(), model, 128, engine, None).unwrap();

    // Try open with dim 256
    let res = SemanticIndex::open(dir.path(), model, 256);
    match res {
        Err(e) => assert!(e.to_string().contains("dimension mismatch")),
        Ok(_) => panic!("Expected dimension mismatch error"),
    }
}

#[test]
fn test_open_schema_version_mismatch() {
    let dir = tempdir().unwrap();
    let path = db_path(dir.path());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
            INSERT INTO meta VALUES ('schema_version', '1'); -- expected 2
            CREATE TABLE vec_chunks (id INTEGER PRIMARY KEY);
            CREATE TABLE files (id INTEGER PRIMARY KEY);
        ",
    )
    .unwrap();
    drop(conn);

    let res = SemanticIndex::open(dir.path(), "any", 0);
    match res {
        Err(e) => assert!(e.to_string().contains("schema version 1 is not supported")),
        Ok(_) => panic!("Expected schema version error"),
    }
}
