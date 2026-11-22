use super::*;

#[test]
fn managed_backup_is_a_self_verifying_sqlite_snapshot() {
    let dir = tempdir().unwrap();
    let managed_sources = dir.path().join("managed_sources");
    std::fs::create_dir(&managed_sources).unwrap();
    std::fs::write(managed_sources.join("source.txt"), b"retained source").unwrap();
    std::fs::write(dir.path().join("workspace.json"), b"{\"version\":1}").unwrap();
    let index = SemanticIndex::create(
        dir.path(),
        "backup-test-model",
        2,
        EmbeddingEngine::Candle,
        None,
    )
    .unwrap();
    let space = index.embedding_space_identity().unwrap().id().0;
    drop(index);

    let destination = dir.path().join("backup");
    let backup = backup_managed_directory(dir.path(), "corpus-test", &space, &destination).unwrap();

    assert_eq!(backup.format, "wilkes-managed-corpus-backup/v1");
    assert_eq!(backup.corpus_id, "corpus-test");
    assert_eq!(backup.embedding_space_id, space);
    assert!(destination.join("semantic_index.db").is_file());
    assert!(destination.join("backup-manifest.json").is_file());
    assert!(backup
        .files
        .iter()
        .any(|file| file.path == "managed_sources/source.txt"));
    for file in &backup.files {
        let path = destination.join(&file.path);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), file.byte_len);
        assert_eq!(
            wilkes_core::embed::identity::sha256_file(&path).unwrap(),
            file.sha256
        );
    }

    let mismatch = dir.path().join("wrong-space-backup");
    let error =
        backup_managed_directory(dir.path(), "corpus-test", "space-not-the-index", &mismatch)
            .unwrap_err();
    assert!(error.to_string().contains("EMBEDDING_SPACE_MISMATCH"));
    assert!(!mismatch.exists());
}

#[test]
fn interrupted_snapshot_placement_leaves_no_temporary_source() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("source.txt");
    std::fs::write(&source, b"immutable source").unwrap();
    let digest = wilkes_core::embed::identity::sha256_file(&source).unwrap();
    let obstructing_destination = dir
        .path()
        .join("managed_sources")
        .join(digest)
        .join("source.txt");
    std::fs::create_dir_all(&obstructing_destination).unwrap();

    AppContext::retain_managed_snapshot(dir.path(), &source)
        .expect_err("a directory cannot be published as an immutable source file");

    let imports = dir.path().join("managed_sources").join(".imports");
    assert_eq!(
        std::fs::read_dir(imports).unwrap().count(),
        0,
        "a failed atomic placement must clean its staging file"
    );
}

/// `FileType::detect` is the one gate on what a corpus admits, and it
/// admits a document whatever `supported_extensions` says — that list is
/// the *text* extensions, and document extensions are stripped from it on
/// load. The managed importer used to compare the raw extension against
/// the list itself, a second copy of the rule that could only refuse:
/// every managed PDF and EPUB import failed.
#[tokio::test]
async fn test_managed_import_admits_a_document_the_extension_list_omits() {
    let dir = tempdir().unwrap();
    let settings = Settings {
        supported_extensions: vec!["txt".to_string()],
        ..Settings::default()
    };
    std::fs::write(
        dir.path().join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    let managed = scoped_ctx(
        dir.path(),
        serde_json::json!({
            "kind": "application_managed",
            "owner": "underdog",
            "purpose": "semantic-corpus",
            "corpus_key": "store-id",
        }),
    );
    assert!(
        !managed
            .get_settings()
            .await
            .supported_extensions
            .iter()
            .any(|extension| extension.eq_ignore_ascii_case("pdf")),
        "a document extension is not storable in the text list"
    );

    let document = dir.path().join("paper.pdf");
    std::fs::write(&document, b"%PDF-1.7\n").unwrap();
    let admitted = managed
        .import_managed_document(
            "store-id".to_string(),
            "key-pdf".to_string(),
            document.clone(),
            None,
            serde_json::json!({ "kind": "path", "path": document }),
        )
        .await
        .expect_err("this temporary workspace holds no index to import into");
    assert!(
        !admitted.to_string().contains("Unsupported managed import"),
        "a PDF must pass the admission gate: {admitted}"
    );

    let refused = dir.path().join("archive.bin");
    std::fs::write(&refused, b"not a corpus file").unwrap();
    let refusal = managed
        .import_managed_document(
            "store-id".to_string(),
            "key-bin".to_string(),
            refused.clone(),
            None,
            serde_json::json!({ "kind": "path", "path": refused }),
        )
        .await
        .expect_err("a file no reader admits is still refused");
    assert!(
        refusal.to_string().contains("Unsupported managed import"),
        "{refusal}"
    );
}

/// What a list of documents is sent: the retained document's first page at
/// the size asked for, found by snapshot id alone. An id the corpus never
/// retained answers nothing rather than some other document's picture.
#[tokio::test]
async fn a_retained_document_serves_its_first_page_as_a_cover() {
    let dir = tempdir().unwrap();
    let managed_sources = dir.path().join("managed_sources");
    std::fs::create_dir(&managed_sources).unwrap();
    let document = managed_sources.join("book.pdf");
    std::fs::write(
        &document,
        b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
              2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
              3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 400]>>endobj\n\
              trailer<</Root 1 0 R>>\n",
    )
    .unwrap();
    let mut index = SemanticIndex::create(
        dir.path(),
        "m",
        2,
        wilkes_core::types::EmbeddingEngine::Candle,
        Some(&managed_sources),
    )
    .unwrap();
    let managed = index
        .write_file_with_recipe(
            wilkes_core::embed::index::db::PreparedFile {
                retained: Default::default(),
                path: document.clone(),
                full_text: "words".to_string(),
                chunks: vec![(
                    wilkes_core::embed::index::chunk::Chunk {
                        file_path: document.clone(),
                        text: "words".to_string(),
                        byte_range: wilkes_core::types::ByteRange { start: 0, end: 5 },
                        origin: wilkes_core::types::SourceOrigin::TextFile { line: 1, col: 1 },
                    },
                    vec![1.0, 0.0],
                )],
            },
            &ExtractionRecipe::new(100, 0),
            Some(Path::new("managed_sources/book.pdf")),
            Some(&serde_json::json!({ "kind": "path" })),
            true,
            false,
            Some("job-1"),
        )
        .unwrap();
    let managed_ctx = scoped_ctx(
        dir.path(),
        serde_json::json!({
            "kind": "application_managed",
            "owner": "underdog",
            "purpose": "semantic-corpus",
            "corpus_key": "store-id",
        }),
    );
    managed_ctx.index.lock().lock().unwrap().replace(index);

    let cover = managed_ctx
        .managed_document_cover(managed.snapshot_id.as_str().to_string(), 120)
        .await
        .unwrap();
    assert_eq!((cover.width, cover.height), (90, 120));
    assert_eq!(cover.media_type, "image/png");
    assert!(cover.bytes.starts_with(b"\x89PNG"));

    assert!(managed_ctx
        .managed_document_cover("snapshot-never-retained".to_string(), 120)
        .await
        .is_err());
    assert!(managed_ctx
        .managed_document_cover(managed.snapshot_id.as_str().to_string(), 0)
        .await
        .is_err());
}
