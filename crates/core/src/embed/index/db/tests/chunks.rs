use super::*;

#[test]
fn test_f32_slice_to_bytes() {
    let v = vec![1.0f32, -2.5f32];
    let bytes = f32_slice_to_bytes(&v);
    assert_eq!(bytes.len(), 8);
    assert_eq!(bytes[0..4], 1.0f32.to_le_bytes());
    assert_eq!(bytes[4..8], (-2.5f32).to_le_bytes());
}

/// A passage is linked to the pictures drawn in it, by arithmetic over the
/// reading and nothing else. The one with no analysis links exactly as the
/// described one does, which is the whole point of anchoring every image.
#[test]
fn figures_link_to_the_passages_they_were_drawn_into() {
    fn figure(id: &str, anchor: usize, range: Option<(usize, usize)>) -> RetainedImage {
        RetainedImage {
            id: id.to_string(),
            page: 1,
            origin: crate::types::RegionOrigin::Embedded,
            bbox: BoundingBox {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            transform: crate::types::ImageTransform {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                e: 0.0,
                f: 0.0,
            },
            pixel_width: 10,
            pixel_height: 10,
            image_sha256: format!("{id}-digest"),
            reading_range: range.map(|(start, end)| ByteRange { start, end }),
            reading_anchor: Some(anchor),
            status: crate::types::ImageAnalysisStatus::Complete,
            has_description: range.is_some(),
        }
    }

    let dir = tempdir().unwrap();
    let root = dir.path().join("managed_sources");
    fs::create_dir_all(&root).unwrap();
    let document = root.join("document.txt");
    fs::write(&document, "east north").unwrap();
    let mut index =
        SemanticIndex::create(dir.path(), "m", 2, EmbeddingEngine::Candle, Some(&root)).unwrap();

    // Two passages, tiling a ten-byte reading: "east" then "north".
    let mut first = test_chunk(&document, "east");
    first.byte_range = ByteRange { start: 0, end: 5 };
    let mut second = test_chunk(&document, "north");
    second.byte_range = ByteRange { start: 5, end: 10 };

    let managed = index
        .write_file_with_recipe(
            PreparedFile {
                retained: RetainedExtraction {
                    images: Some(vec![
                        // Drawn inside the first passage, nothing established
                        // about it — no range, and it links all the same.
                        figure("p1-i0", 2, None),
                        // Its enrichment block *is* the second passage.
                        figure("p1-i1", 5, Some((5, 10))),
                        // Off the end of both.
                        figure("p1-i2", 400, None),
                    ]),
                    ..Default::default()
                },
                path: document.clone(),
                full_text: "east north".to_string(),
                chunks: vec![(first, vec![3.0, 0.0]), (second, vec![0.0, 4.0])],
            },
            &ExtractionRecipe::new(100, 0),
            Some(Path::new("managed_sources/document.txt")),
            None,
            true,
            false,
            Some("job-figures"),
        )
        .unwrap();

    let refs: Vec<ChunkRef> = managed
        .chunks
        .iter()
        .map(|chunk| chunk.chunk_ref.clone())
        .collect();
    let linked = index.figures_for_chunk_refs(&refs, 0).unwrap();
    assert_eq!(linked.len(), 2);
    assert!(linked.iter().all(|entry| entry.inventory_known));

    let first = &linked[0];
    assert_eq!(
        first
            .figures
            .iter()
            .map(|f| (f.image.id.as_str(), f.link.relation))
            .collect::<Vec<_>>(),
        vec![
            ("p1-i0", FigureRelation::DrawnIn),
            ("p1-i1", FigureRelation::DrawnIn),
        ],
        "the undescribed picture drawn inside it, and the one anchored at \
             its closing edge — a zero-width point on a seam is in both passages"
    );

    let second = &linked[1];
    assert_eq!(
        second
            .figures
            .iter()
            .map(|f| (f.image.id.as_str(), f.link.relation))
            .collect::<Vec<_>>(),
        vec![("p1-i1", FigureRelation::Block)],
        "the passage that *is* an enrichment block says so, rather than \
             reporting the weaker relation it also satisfies"
    );

    // The far figure needs a window, and says how far it is when asked.
    let widened = index.figures_for_chunk_refs(&refs[..1], 500).unwrap();
    let far = widened[0]
        .figures
        .iter()
        .find(|f| f.image.id == "p1-i2")
        .expect("in reach of a 500-byte window");
    assert_eq!(far.link.relation, FigureRelation::Near);
    assert_eq!(far.link.byte_distance, 395);

    // And the pixels of one of them are addressable by any passage of the
    // document, without the caller holding a path.
    let source = index
        .figure_source(FigureDocument::Chunk(&refs[0]), "p1-i1")
        .unwrap()
        .expect("the inventory holds it");
    assert_eq!(
        source.managed_snapshot_relative_path.as_deref(),
        Some("managed_sources/document.txt"),
        "served from the immutable snapshot, not from wherever the file went"
    );
}

#[test]
fn managed_neighbors_are_the_adjacent_passages_of_the_same_rendition() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("managed_sources");
    fs::create_dir_all(&root).unwrap();
    let words = ["one", "two", "three", "four", "five"];
    let write = |index: &mut SemanticIndex, name: &str| {
        let document = root.join(name);
        fs::write(&document, words.join(" ")).unwrap();
        let mut start = 0;
        let chunks = words
            .iter()
            .map(|word| {
                let mut chunk = test_chunk(&document, word);
                chunk.byte_range = ByteRange {
                    start,
                    end: start + word.len() + 1,
                };
                start += word.len() + 1;
                (chunk, vec![1.0, 0.0])
            })
            .collect();
        index
            .write_file_with_recipe(
                PreparedFile {
                    retained: Default::default(),
                    path: document.clone(),
                    full_text: words.join(" "),
                    chunks,
                },
                &ExtractionRecipe::new(100, 0),
                Some(&Path::new("managed_sources").join(name)),
                None,
                true,
                false,
                Some(name),
            )
            .unwrap()
    };
    let mut index =
        SemanticIndex::create(dir.path(), "m", 2, EmbeddingEngine::Candle, Some(&root)).unwrap();
    let book = write(&mut index, "book.txt");
    let refs: Vec<ChunkRef> = book.chunks.iter().map(|c| c.chunk_ref.clone()).collect();

    let texts =
        |chunks: &[ManagedChunkData]| chunks.iter().map(|c| c.text.clone()).collect::<Vec<_>>();
    let around = index
        .managed_chunk_neighbors(
            &[
                refs[2].clone(),
                refs[0].clone(),
                refs[4].clone(),
                refs[2].clone(),
            ],
            2,
            1,
        )
        .unwrap();
    assert_eq!(
        around.len(),
        4,
        "one entry per requested ref, repeats included"
    );
    assert_eq!(around[0].of, refs[2]);
    assert_eq!(
        texts(&around[0].before),
        vec!["one", "two"],
        "reading order"
    );
    assert_eq!(texts(&around[0].after), vec!["four"]);
    assert_eq!(
        around[0]
            .before
            .iter()
            .map(|c| c.ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        around[0].before[1].chunk_ref, refs[1],
        "each neighbour carries its own ref"
    );
    assert!(around[1].before.is_empty(), "the document starts there");
    assert_eq!(texts(&around[1].after), vec!["two"]);
    assert_eq!(texts(&around[2].before), vec!["three", "four"]);
    assert!(around[2].after.is_empty(), "the document ends there");
    assert_eq!(texts(&around[3].before), texts(&around[0].before));

    // A window wider than the document is the document.
    let wide = index.managed_chunk_neighbors(&refs[1..2], 0, 50).unwrap();
    assert_eq!(texts(&wide[0].after), vec!["three", "four", "five"]);
    assert!(wide[0].before.is_empty());

    assert!(index
        .managed_chunk_neighbors(&[ChunkRef("chunk-missing".to_string())], 1, 1)
        .unwrap_err()
        .to_string()
        .contains("CHUNK_REF_NOT_FOUND"));
}

#[test]
fn managed_refs_resolve_and_aggregate_without_exposing_rowids() {
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
                chunks: vec![
                    (test_chunk(&document, "east"), vec![3.0, 0.0]),
                    (test_chunk(&document, "north"), vec![0.0, 4.0]),
                ],
            },
            &recipe,
            Some(Path::new("managed_sources/document.txt")),
            Some(&serde_json::json!({"kind": "path"})),
            true,
            false,
            Some("job-1"),
        )
        .unwrap();

    assert_eq!(managed.chunks.len(), 2);
    assert_eq!(index.managed_embedding_work_totals().unwrap(), (0, 2));
    assert!(index
        .managed_document_for_import_key(
            "job-1",
            &managed.source_sha256,
            &managed.extraction_recipe_id,
        )
        .unwrap()
        .is_some());
    assert!(
        index
            .managed_document_for_import_key(
                "job-1",
                "different-source",
                &managed.extraction_recipe_id,
            )
            .unwrap_err()
            .to_string()
            .contains("IDEMPOTENCY_KEY_CONFLICT")
    );
    assert_ne!(managed.chunks[0].chunk_ref, managed.chunks[1].chunk_ref);
    let groups = vec![vec![
        managed.chunks[0].chunk_ref.clone(),
        managed.chunks[1].chunk_ref.clone(),
    ]];
    let accumulated = index.accumulate_chunk_refs(&groups).unwrap();
    assert_eq!(accumulated[0].member_count, 2);
    // (3,0) and (0,4) sum to (1,1) only because each member is
    // L2-normalized first. Without that, the longer passage's vector norm
    // would out-vote the other and the aggregate would report the region
    // as mostly the first passage on the strength of a magnitude.
    assert_eq!(accumulated[0].sum, vec![1.0, 1.0]);
    // A group of no chunks is not a vector, and answering with a zero one
    // would be a well-formed reply to a question nobody can have meant.
    assert!(
        index.accumulate_chunk_refs(&[vec![]]).is_err(),
        "empty group"
    );
    let resolved = index
        .managed_chunks_for_refs(&groups[0])
        .expect("stable refs resolve");
    assert_eq!(resolved[0].text, "east");
    assert!(index
        .accumulate_chunk_refs(&[vec![ChunkRef("chunk-missing".to_string())]])
        .unwrap_err()
        .to_string()
        .contains("CHUNK_REF_NOT_FOUND"));
}

#[test]
fn test_chunk_similarity_answers_both_directions_and_leaves_the_probe_as_given() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut idx = SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
    let doc = root.join("doc.txt");
    fs::write(&doc, "content").unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: doc.clone(),
        chunks: vec![
            (test_chunk(&doc, "east"), vec![5.0, 0.0]),
            (test_chunk(&doc, "north"), vec![0.0, 1.0]),
        ],
    })
    .unwrap();
    let ids: Vec<i64> = idx
        .topic_chunks_for_file(root, &doc)
        .unwrap()
        .iter()
        .map(|chunk| chunk.chunk_id)
        .collect();

    let found = idx
        .chunk_similarity(
            &[
                SimilarityProbe {
                    vector: vec![1.0, 0.0],
                    scope: vec![],
                },
                SimilarityProbe {
                    vector: vec![0.0, 1.0],
                    scope: vec![],
                },
                // Half a unit vector: a caller sending the unnormalized
                // mean of a group wants the mean of the group's cosines
                // back, so nothing here may renormalize it.
                SimilarityProbe {
                    vector: vec![0.5, 0.0],
                    scope: ids.clone(),
                },
            ],
            &ids,
        )
        .unwrap();

    assert_eq!(found.probes[0].nearest_chunk_id, Some(ids[0]));
    assert!((found.probes[0].similarity.unwrap() - 1.0).abs() < 1e-5);
    assert_eq!(found.probes[1].nearest_chunk_id, Some(ids[1]));
    // The chunk's own norm never enters: `east` is five units long and
    // still reads 1.0 against the direction it points in.
    assert_eq!(found.chunks[0].probe, 0);
    assert!((found.chunks[0].similarity - 1.0).abs() < 1e-5);
    assert_eq!(found.chunks[1].probe, 1);
    // mean over {east, north} of dot((0.5, 0), ·) = (0.5 + 0) / 2.
    let mean = found.probes[2].scope_mean.unwrap();
    assert!((mean - 0.25).abs() < 1e-5, "{mean}");
    assert_eq!(found.probes[2].scope_size, 2);
    assert!(found.probes[0].scope_mean.is_none(), "no scope, no mean");
}

#[test]
fn test_chunk_similarity_refuses_stale_ids_and_wrong_dimensions() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut idx = SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
    let doc = root.join("doc.txt");
    fs::write(&doc, "content").unwrap();
    idx.write_file(PreparedFile {
        retained: Default::default(),
        full_text: String::new(),
        path: doc.clone(),
        chunks: vec![(test_chunk(&doc, "only"), vec![1.0, 0.0])],
    })
    .unwrap();
    let id = idx.topic_chunks_for_file(root, &doc).unwrap()[0].chunk_id;
    let probe = |scope: Vec<i64>| SimilarityProbe {
        vector: vec![1.0, 0.0],
        scope,
    };

    // A stale id in the searched set, and a stale id hiding in a scope,
    // are the same failure: a reading over the ids that happened to
    // survive, with nothing in the reply to say which.
    assert!(idx
        .chunk_similarity(&[probe(vec![])], &[id, 999_999])
        .is_err());
    let error = idx
        .chunk_similarity(&[probe(vec![999_999])], &[id])
        .expect_err("a stale scope id must refuse");
    assert!(format!("{error:#}").contains("999999"), "{error:#}");

    let error = idx
        .chunk_similarity(
            &[SimilarityProbe {
                vector: vec![1.0, 0.0, 0.0],
                scope: vec![],
            }],
            &[id],
        )
        .expect_err("a probe from another space must refuse");
    assert!(format!("{error:#}").contains("dimension"), "{error:#}");
}

/// Renamed from `…_preserves_probe_scale`, which pinned the opposite rule:
/// a probe of magnitude 0.5 scored 0.5 against a chunk pointing the same
/// way, so the field called `similarity` was cosine × ‖probe‖ and the
/// shared `min_similarity` floor filtered a short probe harder than a long
/// one. No caller wants that quantity — Underdog reads the value as a
/// cosine and two of its thresholds are calibrated as cosines — and every
/// live probe is engine-normalized, so the rule was invisible rather than
/// load-bearing.
#[test]
fn test_managed_chunk_search_covers_corpus_and_scores_cosine() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut idx = SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
    for (name, text, vector) in [
        ("east.txt", "east", vec![4.0, 0.0]),
        ("north.txt", "north", vec![0.0, 2.0]),
    ] {
        let path = root.join(name);
        fs::write(&path, text).unwrap();
        idx.write_file_with_recipe(
            PreparedFile {
                retained: Default::default(),
                full_text: text.to_string(),
                path: path.clone(),
                chunks: vec![(test_chunk(&path, text), vector)],
            },
            &ExtractionRecipe::new(100, 0),
            Some(Path::new(name)),
            None,
            true,
            false,
            None,
        )
        .unwrap();
    }

    let found = idx.managed_chunk_search(&[vec![0.5, 0.0]], 8, 0.1).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].len(), 1, "minimum similarity filters north");
    assert_eq!(found[0][0].ordinal, 0);
    assert!(
        (found[0][0].similarity - 1.0).abs() < 1e-5,
        "a probe pointing the same way as the chunk scores 1, whatever its magnitude"
    );
    let unit = idx.managed_chunk_search(&[vec![1.0, 0.0]], 8, 0.1).unwrap();
    assert!(
        (unit[0][0].similarity - found[0][0].similarity).abs() < 1e-6,
        "probe magnitude does not move the score"
    );
    idx.managed_chunk_search(&[vec![0.0, 0.0]], 8, 0.0)
        .expect_err("a probe with no direction is not a query");
    assert!(!found[0][0].chunk_ref.as_str().is_empty());
    assert!(!found[0][0].snapshot_id.as_str().is_empty());
    assert!(!found[0][0].rendition_id.as_str().is_empty());

    assert!(idx.managed_chunk_search(&[vec![1.0]], 8, 0.0).is_err());
    assert!(idx.managed_chunk_search(&[vec![1.0, 0.0]], 0, 0.0).is_err());
}

/// Search runs on the vector table's nearest-neighbour scan, which returns
/// equal distances in its own order. Which of several equal passages
/// falls inside `top_k` must still be the documented rule — rendition,
/// then ordinal — or two corpora holding the same passages could answer
/// the same query differently.
#[test]
fn test_managed_chunk_search_breaks_ties_at_top_k_by_rendition() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut idx = SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
    for at in 0..7 {
        let name = format!("same-{at}.txt");
        let text = format!("same direction {at}");
        let path = root.join(&name);
        fs::write(&path, &text).unwrap();
        idx.write_file_with_recipe(
            PreparedFile {
                retained: Default::default(),
                full_text: text.clone(),
                path: path.clone(),
                chunks: vec![(test_chunk(&path, &text), vec![1.0, 0.0])],
            },
            &ExtractionRecipe::new(100, 0),
            Some(Path::new(&name)),
            None,
            true,
            false,
            None,
        )
        .unwrap();
    }
    let path = root.join("other.txt");
    fs::write(&path, "other").unwrap();
    idx.write_file_with_recipe(
        PreparedFile {
            retained: Default::default(),
            full_text: "other".to_string(),
            path: path.clone(),
            chunks: vec![(test_chunk(&path, "other"), vec![0.6, 0.8])],
        },
        &ExtractionRecipe::new(100, 0),
        Some(Path::new("other.txt")),
        None,
        true,
        false,
        None,
    )
    .unwrap();

    let all = idx
        .managed_chunk_search(&[vec![1.0, 0.0]], 8, -1.0)
        .unwrap();
    assert_eq!(all[0].len(), 8, "every passage is in reach");
    let mut tied: Vec<String> = all[0][..7]
        .iter()
        .map(|hit| hit.rendition_id.as_str().to_string())
        .collect();
    let sorted = {
        let mut sorted = tied.clone();
        sorted.sort();
        sorted
    };
    assert_eq!(tied, sorted, "equal scores are ordered by rendition");
    assert!((all[0][7].similarity - 0.6).abs() < 1e-5);

    for top_k in 1..=7 {
        let some = idx
            .managed_chunk_search(&[vec![1.0, 0.0]], top_k, -1.0)
            .unwrap();
        let got: Vec<&str> = some[0]
            .iter()
            .map(|hit| hit.rendition_id.as_str())
            .collect();
        tied.truncate(7);
        let want: Vec<&str> = tied[..top_k].iter().map(String::as_str).collect();
        assert_eq!(
            got, want,
            "top_k={top_k} keeps the first renditions of the tie"
        );
    }
}
