use super::*;

/// Vectors are reusable by content, across indexes that share a space.
///
/// This is the reuse the managed corpus was missing: a library the user
/// had already indexed under the very model a projection needed, and the
/// projection re-embedded every passage anyway. The lookup is by text
/// hash, which is the direct evidence — the extraction recipe was a proxy
/// for it, and one that differed for reasons that had nothing to do with
/// whether the text came out the same.
#[test]
fn vectors_are_adoptable_by_text_across_indexes_in_one_space() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let file = root.join("shared.txt");
    fs::write(&file, "shared body").unwrap();

    let identity = EmbeddingSpaceIdentity::with_artifact_revision(
        EmbeddingEngine::Candle,
        "model",
        1,
        "artifact-sha256:one".to_string(),
    );
    let mut source = SemanticIndex::create_exact(root, &identity, Some(root)).unwrap();
    // The recipe-carrying write, which is what every real build uses. The
    // bare `write_file` records no chunk identity, so its rows carry no
    // text hash and nothing can be adopted from them.
    source
        .write_file_with_recipe(
            PreparedFile {
                retained: Default::default(),
                path: file.clone(),
                full_text: "shared body".to_string(),
                chunks: vec![
                    (test_chunk(&file, "shared body"), vec![0.5]),
                    (test_chunk(&file, "another passage"), vec![0.25]),
                ],
            },
            &ExtractionRecipe::new(100, 0),
            None,
            None,
            false,
            false,
            None,
        )
        .unwrap();

    let wanted = vec![
        sha256_bytes(b"shared body"),
        sha256_bytes(b"another passage"),
        sha256_bytes(b"never embedded anywhere"),
    ];
    let found = source.vectors_for_text_hashes(&wanted).unwrap();

    assert_eq!(found.get(&wanted[0]), Some(&vec![0.5]));
    assert_eq!(found.get(&wanted[1]), Some(&vec![0.25]));
    // A miss is absent, not an error: the caller embeds what it did not
    // find, so a partial hit is a partial saving.
    assert!(!found.contains_key(&wanted[2]));
    assert_eq!(found.len(), 2);

    // An index built by a different install of the same model is the same
    // space, so its vectors are readable here. This is the case the old
    // identity refused: same engine, same model, same width, different
    // fingerprint.
    let elsewhere = EmbeddingSpaceIdentity::with_artifact_revision(
        EmbeddingEngine::Candle,
        "model",
        1,
        "unresolved-runtime-cache-not-materialized".to_string(),
    );
    assert_eq!(identity.id(), elsewhere.id());
    source.validate_embedding_space(&elsewhere).unwrap();

    assert!(source.vectors_for_text_hashes(&[]).unwrap().is_empty());
}

/// A consumer that stored a snapshot id can find the retained bytes with
/// it and nothing else — no path, no rowid, no root.
///
/// The negative half is the point of the route: an id this corpus never
/// retained answers nothing rather than falling through to some file that
/// happens to be lying around, because a wrong document is worse than an
/// error to a caller that cannot tell the difference.
#[test]
fn a_retained_snapshot_is_found_by_its_id_alone() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("managed_sources");
    fs::create_dir_all(&root).unwrap();
    let document = root.join("document.txt");
    fs::write(&document, "east north").unwrap();
    let mut index =
        SemanticIndex::create(dir.path(), "m", 2, EmbeddingEngine::Candle, Some(&root)).unwrap();
    let recipe = ExtractionRecipe::new(100, 0);
    let managed = index
        .write_file_with_recipe(
            PreparedFile {
                retained: Default::default(),
                path: document.clone(),
                full_text: "east north".to_string(),
                chunks: vec![(test_chunk(&document, "east"), vec![3.0, 0.0])],
            },
            &recipe,
            Some(Path::new("managed_sources/document.txt")),
            Some(&serde_json::json!({"kind": "path"})),
            true,
            false,
            Some("job-1"),
        )
        .unwrap();

    let source = index
        .managed_snapshot_source(managed.snapshot_id.as_str())
        .unwrap()
        .expect("the corpus retained it");
    assert_eq!(
        source.managed_snapshot_relative_path,
        "managed_sources/document.txt"
    );
    assert_eq!(source.source_sha256, managed.source_sha256);

    assert!(
        index
            .managed_snapshot_source("snapshot-this-corpus-never-saw")
            .unwrap()
            .is_none(),
        "an unknown id is empty-handed, not approximately answered"
    );
}

/// A rendition is readable for projection whatever the runtime would
/// extract under today, and it says which recipe it is.
///
/// This read used to take the caller's recipe and refuse when it differed
/// from the rendition's. That made every runtime recipe change — a
/// recognizer installed, an extractor version bumped — permanent: the
/// projection could not be brought level, and the import key bound to the
/// old recipe could not be rebound, because nothing deletes one.
#[test]
fn a_rendition_is_read_under_its_own_recipe_not_the_runtimes() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("managed_sources");
    fs::create_dir_all(&root).unwrap();
    let document = root.join("document.txt");
    fs::write(&document, "canonical passage").unwrap();
    let admitted_under = ExtractionRecipe::new(100, 0);
    let mut index = SemanticIndex::create(
        dir.path(),
        "primary",
        2,
        EmbeddingEngine::Candle,
        Some(&root),
    )
    .unwrap();
    index
        .write_file_with_recipe(
            PreparedFile {
                retained: Default::default(),
                path: document.clone(),
                chunks: vec![(test_chunk(&document, "canonical passage"), vec![1.0, 0.0])],
                full_text: "canonical passage".to_string(),
            },
            &admitted_under,
            Some(Path::new("managed_sources/document.txt")),
            None,
            true,
            false,
            Some("canonical-import"),
        )
        .unwrap();

    // What a runtime with different settings would produce. Nothing here
    // consults it, which is the point.
    let runtime_would_use = ExtractionRecipe::new(600, 128);
    assert_ne!(admitted_under.id(), runtime_would_use.id());

    let (prepared, recipe_id) = index
        .managed_file_structure_for_reembedding(&document, &document)
        .unwrap()
        .expect("the rendition is admitted");
    assert_eq!(
        recipe_id,
        admitted_under.id(),
        "the recipe is the rendition's own"
    );
    assert_eq!(prepared.full_text, "canonical passage");

    // And the same value reaches the import path, which adopts a held
    // rendition only when this runtime would produce it: the reply to
    // "what is already here" has to say what recipe it is under, or the
    // decision degrades to "something is here", which is what let a
    // recipe change mean nothing.
    let held = index
        .managed_document_for_path(&document)
        .unwrap()
        .expect("the document is held");
    assert_eq!(held.extraction_recipe_id, admitted_under.id());
    assert_ne!(held.extraction_recipe_id, runtime_would_use.id());

    // And the membership read names the rendition, so a caller can key a
    // catch-up on the thing it is catching up to.
    let admitted = index.managed_admitted_renditions().unwrap();
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].0, index.path_key_for_known_path(&document));
    assert!(!admitted[0].1.is_empty(), "the rendition id is carried");
}

#[test]
fn managed_projection_reuses_structure_and_generation_but_not_vectors() {
    let canonical_dir = tempdir().unwrap();
    let canonical_root = canonical_dir.path().join("managed_sources");
    fs::create_dir_all(&canonical_root).unwrap();
    let document = canonical_root.join("document.txt");
    fs::write(&document, "canonical passage").unwrap();
    let recipe = ExtractionRecipe::new(100, 0);
    let mut canonical = SemanticIndex::create(
        canonical_dir.path(),
        "primary",
        2,
        EmbeddingEngine::Candle,
        Some(&canonical_root),
    )
    .unwrap();
    let imported = canonical
        .write_file_with_recipe(
            PreparedFile {
                retained: Default::default(),
                path: document.clone(),
                chunks: vec![(test_chunk(&document, "canonical passage"), vec![1.0, 0.0])],
                full_text: "canonical passage".to_string(),
            },
            &recipe,
            Some(Path::new("managed_sources/document.txt")),
            None,
            true,
            false,
            Some("canonical-import"),
        )
        .unwrap();

    let projection_dir = tempdir().unwrap();
    let projection_root = projection_dir.path().join("managed_sources");
    fs::create_dir_all(&projection_root).unwrap();
    let mut projection = SemanticIndex::create(
        projection_dir.path(),
        "secondary",
        2,
        EmbeddingEngine::Candle,
        Some(&projection_root),
    )
    .unwrap();
    let (mut prepared, canonical_recipe_id) = canonical
        .managed_file_structure_for_reembedding(&document, &document)
        .unwrap()
        .unwrap();
    assert_eq!(
        canonical_recipe_id,
        recipe.id(),
        "the read hands back the rendition's own recipe"
    );
    assert_eq!(prepared.full_text, "canonical passage");
    assert_eq!(prepared.chunks.len(), 1);
    assert!(prepared.chunks[0].1.is_empty());
    prepared.chunks[0].1 = vec![0.0, 1.0];
    projection
        .write_file_with_recipe(
            prepared,
            &recipe,
            None,
            Some(&serde_json::json!({"kind": "managed_corpus_projection"})),
            true,
            false,
            Some("projection-import"),
        )
        .unwrap();

    assert_eq!(
        canonical.managed_snapshot_sha256().unwrap(),
        projection.managed_snapshot_sha256().unwrap(),
        "membership generation excludes model-specific coordinates"
    );
    assert_eq!(
        projection.query(&[0.0, 1.0], 1).unwrap()[0].chunk_text,
        "canonical passage",
        "the projection stores its own vectors over canonical chunks"
    );

    // The corpus retains the document; the projection retains coordinates
    // for it and no bytes at all. Both indexes know the same snapshot id,
    // so the id cannot be what tells them apart — only asking the right
    // one does. A consumer's snapshot read must therefore be addressed by
    // corpus and never routed by an embedding pin, which is what sends it
    // here, to the index that correctly answers that it has nothing.
    let snapshot = imported.snapshot_id.as_str();
    assert!(
        canonical
            .managed_snapshot_source(snapshot)
            .unwrap()
            .is_some(),
        "the corpus retained the document it imported"
    );
    assert!(
        projection
            .managed_snapshot_source(snapshot)
            .unwrap()
            .is_none(),
        "a projection is the canonical rendition re-embedded, not a second \
             copy of the book"
    );
}

#[test]
fn exact_whole_document_adoption_survives_source_index_deletion_and_rebuild() {
    let recipe = ExtractionRecipe::new(100, 0);
    let target_dir = tempdir().unwrap();
    let target_root = target_dir.path().join("managed_sources");
    fs::create_dir_all(&target_root).unwrap();
    let target_path = target_root.join("document.txt");
    fs::write(&target_path, "retained body").unwrap();

    let adopted = {
        let source_dir = tempdir().unwrap();
        let source_path = source_dir.path().join("document.txt");
        fs::write(&source_path, "retained body").unwrap();
        let mut source = SemanticIndex::create(
            source_dir.path(),
            "m",
            2,
            EmbeddingEngine::Candle,
            Some(source_dir.path()),
        )
        .unwrap();
        source
            .write_file_with_recipe(
                PreparedFile {
                    retained: Default::default(),
                    path: source_path,
                    full_text: "retained body".to_string(),
                    chunks: vec![
                        (test_chunk(&target_path, "retained"), vec![1.0, 0.0]),
                        (test_chunk(&target_path, "body"), vec![0.0, 1.0]),
                    ],
                },
                &recipe,
                None,
                None,
                false,
                false,
                None,
            )
            .unwrap();
        let space = source.embedding_space_id().unwrap();
        assert!(source
            .verified_file_for_adoption(
                &sha256_file(&target_path).unwrap(),
                &ExtractionRecipe::new(50, 0),
                &space,
                &target_path,
            )
            .unwrap()
            .is_none());
        assert!(source
            .verified_file_for_adoption(
                &sha256_file(&target_path).unwrap(),
                &recipe,
                &EmbeddingSpaceIdentity::for_runtime(EmbeddingEngine::Candle, "other-model", 2,)
                    .id(),
                &target_path,
            )
            .unwrap()
            .is_none());
        source
            .verified_file_for_adoption(
                &sha256_file(&target_path).unwrap(),
                &recipe,
                &space,
                &target_path,
            )
            .unwrap()
            .expect("exact rendition is adoptable")
    };

    let mut target = SemanticIndex::create(
        target_dir.path(),
        "m",
        2,
        EmbeddingEngine::Candle,
        Some(&target_root),
    )
    .unwrap();
    let first = target
        .write_file_with_recipe(
            adopted,
            &recipe,
            Some(Path::new("managed_sources/document.txt")),
            Some(&serde_json::json!({"kind": "wilkes_file"})),
            true,
            true,
            Some("adoption-job"),
        )
        .unwrap();
    let refs: Vec<_> = first
        .chunks
        .iter()
        .map(|chunk| chunk.chunk_ref.clone())
        .collect();
    assert_eq!(target.managed_embedding_work_totals().unwrap(), (2, 0));
    assert_eq!(target.managed_chunks_for_refs(&refs).unwrap().len(), 2);

    let rebuilt_dir = tempdir().unwrap();
    let rebuilt_root = rebuilt_dir.path().join("managed_sources");
    fs::create_dir_all(&rebuilt_root).unwrap();
    let rebuilt_path = rebuilt_root.join("document.txt");
    fs::write(&rebuilt_path, "retained body").unwrap();
    let mut rebuilt = SemanticIndex::create(
        rebuilt_dir.path(),
        "m",
        2,
        EmbeddingEngine::Candle,
        Some(&rebuilt_root),
    )
    .unwrap();
    let second = rebuilt
        .write_file_with_recipe(
            PreparedFile {
                retained: Default::default(),
                path: rebuilt_path.clone(),
                full_text: "retained body".to_string(),
                chunks: vec![
                    (test_chunk(&rebuilt_path, "retained"), vec![1.0, 0.0]),
                    (test_chunk(&rebuilt_path, "body"), vec![0.0, 1.0]),
                ],
            },
            &recipe,
            Some(Path::new("managed_sources/document.txt")),
            None,
            true,
            false,
            None,
        )
        .unwrap();
    assert_eq!(
        refs,
        second
            .chunks
            .iter()
            .map(|chunk| chunk.chunk_ref.clone())
            .collect::<Vec<_>>()
    );
}
