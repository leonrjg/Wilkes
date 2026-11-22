use super::*;

#[tokio::test]
async fn export_file_chunks_returns_extraction_ordered_chunks_with_vectors() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("export-root");
    std::fs::create_dir_all(&root).unwrap();
    let document = root.join("paper.txt");
    std::fs::write(&document, "chunk export passages").unwrap();
    let settings = Settings {
        last_directory: Some(root.clone()),
        ..Default::default()
    };
    std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

    // No index yet: loud, actionable error.
    let err = ctx
        .export_file_chunks(root.clone(), document.clone())
        .await
        .unwrap_err();
    assert!(err.contains("Semantic index unavailable"), "{err}");

    let mut index = SemanticIndex::create(
        dir.path(),
        "export-model",
        2,
        EmbeddingEngine::Candle,
        Some(&root),
    )
    .unwrap();
    // Written out of extraction order on purpose: the export must sort
    // by byte range, not row id.
    let chunks = vec![
        (
            wilkes_core::embed::index::chunk::Chunk {
                file_path: document.clone(),
                text: "second passage".to_string(),
                byte_range: ByteRange { start: 50, end: 64 },
                origin: SourceOrigin::TextFile { line: 5, col: 1 },
            },
            vec![0.0, 1.0],
        ),
        (
            wilkes_core::embed::index::chunk::Chunk {
                file_path: document.clone(),
                text: "first passage".to_string(),
                byte_range: ByteRange { start: 0, end: 13 },
                origin: SourceOrigin::TextFile { line: 1, col: 1 },
            },
            vec![1.0, 0.0],
        ),
    ];
    // Written the way indexing writes, so the passages carry the refs the
    // export names them by.
    index
        .write_file_with_recipe(
            wilkes_core::embed::index::db::PreparedFile {
                retained: Default::default(),
                full_text: String::new(),
                path: document.clone(),
                chunks,
            },
            &ExtractionRecipe::new(600, 128),
            None,
            None,
            false,
            false,
            None,
        )
        .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

    let export = ctx
        .export_file_chunks(root.clone(), document.clone())
        .await
        .unwrap();
    assert_eq!(export.dimension, Some(2));
    assert_eq!(export.model_id, None, "no live embedder loaded");
    assert_eq!(export.chunks.len(), 2);
    assert_eq!(export.chunks[0].chunk.ordinal, 0);
    assert_eq!(export.chunks[0].chunk.text, "first passage");
    assert_eq!(export.chunks[0].chunk.byte_range.start, 0);
    assert_eq!(export.chunks[0].embedding, vec![1.0, 0.0]);
    assert_eq!(export.chunks[1].chunk.ordinal, 1);
    assert_eq!(export.chunks[1].chunk.text, "second passage");
    // The export names passages the way every other route does, and its
    // refs are the ones `chunks/resolve` answers for.
    assert_ne!(
        export.chunks[0].chunk.chunk_ref,
        export.chunks[1].chunk.chunk_ref
    );
    assert!(!export.chunks[0].chunk.text_sha256.is_empty());
    assert!(
        export.outline.is_empty(),
        "the document declares no headings"
    );

    // Files outside the library are refused.
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, "secret").unwrap();
    let err = ctx
        .export_file_chunks(root.clone(), outside)
        .await
        .unwrap_err();
    assert!(!err.is_empty());
}

#[tokio::test]
async fn outline_export_reads_declared_headings_without_a_semantic_index() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("outline-only-root");
    std::fs::create_dir_all(&root).unwrap();
    let document = root.join("notes.md");
    std::fs::write(&document, "# One\nbody\n## Two\nmore body\n").unwrap();
    let settings = Settings {
        last_directory: Some(root.clone()),
        ..Default::default()
    };
    std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

    // No semantic index or embedder is installed. A declared outline is
    // source structure, not an embedding export, so this still succeeds.
    let export = ctx
        .export_file_outline(root.clone(), document.clone())
        .await
        .unwrap();

    assert_eq!(export.file_path, std::fs::canonicalize(&document).unwrap());
    assert_eq!(export.outline.len(), 2);
    assert_eq!(export.outline[0].title, "One");
    assert_eq!(export.outline[0].level, 0);
    assert_eq!(
        export.outline[0].anchor,
        wilkes_core::types::OutlineAnchor::TextOffset
    );
    assert_eq!(export.outline[1].title, "Two");
    assert_eq!(export.outline[1].level, 1);
    assert_eq!(export.extraction.pages, 0);

    let outside = dir.path().join("outside.md");
    std::fs::write(&outside, "# Not in the library\n").unwrap();
    let err = ctx.export_file_outline(root, outside).await.unwrap_err();
    assert!(err.contains("not in the library"), "{err}");
}

/// Browsing a root answers two questions at once, and the test holds both:
/// which documents Wilkes serves there (its rules, not the caller's), and
/// how much of each one the index can actually export.
#[tokio::test]
async fn library_file_export_lists_served_documents_with_indexed_passage_counts() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("browse-root");
    std::fs::create_dir_all(&root).unwrap();
    let indexed = root.join("indexed.txt");
    let unindexed = root.join("unindexed.txt");
    std::fs::write(&indexed, "alpha passage").unwrap();
    std::fs::write(&unindexed, "beta passage").unwrap();
    let settings = Settings {
        last_directory: Some(root.clone()),
        ..Default::default()
    };
    std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

    // No index at all: the documents are still there to be seen, and every
    // count is zero rather than the listing being an error.
    let export = ctx.export_library_files(root.clone()).await.unwrap();
    assert_eq!(export.root, root.canonicalize().unwrap());
    assert_eq!(export.files.len(), 2);
    assert!(export.files.iter().all(|file| file.chunk_count == 0));

    let mut index = SemanticIndex::create(
        dir.path(),
        "browse-model",
        2,
        EmbeddingEngine::Candle,
        Some(&root),
    )
    .unwrap();
    index
        .write_file(wilkes_core::embed::index::db::PreparedFile {
            retained: Default::default(),
            full_text: String::new(),
            path: indexed.clone(),
            chunks: vec![(
                wilkes_core::embed::index::chunk::Chunk {
                    file_path: indexed.clone(),
                    text: "alpha passage".to_string(),
                    byte_range: ByteRange { start: 0, end: 13 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0, 0.0],
            )],
        })
        .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

    let export = ctx.export_library_files(root.clone()).await.unwrap();
    let by_path: std::collections::HashMap<_, _> = export
        .files
        .iter()
        .map(|file| (file.path.clone(), file.chunk_count))
        .collect();
    assert_eq!(
        by_path.get(&indexed.canonicalize().unwrap()),
        Some(&1),
        "the indexed document reports the passages an export would return"
    );
    assert_eq!(
        by_path.get(&unindexed.canonicalize().unwrap()),
        Some(&0),
        "a served but unindexed document is listed, not hidden"
    );
    // Ascending by path, so a consumer rendering the list does not have to
    // decide the order Wilkes already knows.
    let mut sorted = export
        .files
        .iter()
        .map(|f| f.path.clone())
        .collect::<Vec<_>>();
    let unsorted = sorted.clone();
    sorted.sort();
    assert_eq!(unsorted, sorted);

    // A directory outside the library is refused, exactly as the chunk
    // export refuses a file outside it.
    let outside = dir.path().join("outside-root");
    std::fs::create_dir_all(&outside).unwrap();
    let err = ctx.export_library_files(outside).await.unwrap_err();
    assert!(err.contains("not in the library"), "{err}");
}

/// The export is the only place that holds both the outline's locators and
/// the chunk list, so it is where the two are joined — a consumer given the
/// raw locators would have to re-derive this mapping from the chunks it was
/// just handed.
#[tokio::test]
async fn export_resolves_declared_headings_onto_chunk_ordinals() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("outline-root");
    std::fs::create_dir_all(&root).unwrap();
    let document = root.join("notes.md");
    let text = "# One\nalpha body\n## One point one\nbeta body\n";
    std::fs::write(&document, text).unwrap();
    let settings = Settings {
        last_directory: Some(root.clone()),
        ..Default::default()
    };
    std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

    let second_heading = text.find("## One").unwrap();
    let mut index = SemanticIndex::create(
        dir.path(),
        "outline-model",
        2,
        EmbeddingEngine::Candle,
        Some(&root),
    )
    .unwrap();
    index
        .write_file_with_recipe(
            wilkes_core::embed::index::db::PreparedFile {
                retained: Default::default(),
                full_text: text.to_string(),
                path: document.clone(),
                chunks: vec![
                    (
                        wilkes_core::embed::index::chunk::Chunk {
                            file_path: document.clone(),
                            text: "# One\nalpha body".to_string(),
                            byte_range: ByteRange {
                                start: 0,
                                end: second_heading,
                            },
                            origin: SourceOrigin::TextFile { line: 1, col: 1 },
                        },
                        vec![1.0, 0.0],
                    ),
                    (
                        wilkes_core::embed::index::chunk::Chunk {
                            file_path: document.clone(),
                            text: "## One point one\nbeta body".to_string(),
                            byte_range: ByteRange {
                                start: second_heading,
                                end: text.len(),
                            },
                            origin: SourceOrigin::TextFile { line: 3, col: 1 },
                        },
                        vec![0.0, 1.0],
                    ),
                ],
            },
            &ExtractionRecipe::new(600, 128),
            None,
            None,
            false,
            false,
            None,
        )
        .unwrap();
    *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

    let export = ctx
        .export_file_chunks(root.clone(), document.clone())
        .await
        .unwrap();
    let outline: Vec<(&str, u32, usize)> = export
        .outline
        .iter()
        .map(|e| (e.title.as_str(), e.level, e.chunk_ordinal))
        .collect();
    assert_eq!(
        outline,
        vec![("One", 0, 0), ("One point one", 1, 1)],
        "each heading lands on the chunk its section starts in"
    );
    // The locator the document expressed survives alongside the resolution.
    assert_eq!(export.outline[1].byte_offset, Some(second_heading));
    assert_eq!(export.outline[1].page, None);
}

/// A section that starts after the last chunk starts nowhere — a bookmark
/// into a scanned appendix, say. Dropping it keeps every surviving entry a
/// position a consumer can act on.
#[test]
fn outline_entries_that_resolve_past_the_document_are_dropped() {
    let chunk = |ordinal: usize, start: usize, end: usize, page: u32| ExportedChunk {
        chunk: ChunkExport {
            chunk_ref: ChunkRef(format!("chunk-{ordinal}")),
            ordinal,
            text: String::new(),
            text_sha256: String::new(),
            byte_range: ByteRange { start, end },
            origin: SourceOrigin::PdfPage { page, bbox: None },
        },
        embedding: Vec::new(),
    };
    let chunks = vec![chunk(0, 0, 10, 1), chunk(1, 10, 20, 4)];
    let entry = |page: Option<u32>, byte_offset: Option<usize>| wilkes_core::types::OutlineEntry {
        title: "T".to_string(),
        level: 0,
        page,
        byte_offset,
        anchor: wilkes_core::types::OutlineAnchor::Page,
    };

    // A bookmark on a page with no extracted text resolves forward to the
    // next page that has some, which is where its section's text begins.
    let resolved = resolve_outline(&[entry(Some(2), None)], &chunks);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].chunk_ordinal, 1);

    // Past the end: nothing to point at.
    assert!(resolve_outline(&[entry(Some(9), None)], &chunks).is_empty());
    assert!(resolve_outline(&[entry(None, Some(999))], &chunks).is_empty());
    // No locator at all is not a position either.
    assert!(resolve_outline(&[entry(None, None)], &chunks).is_empty());
}

/// A paginated entry is placed by its page and its title, never by an
/// offset into a reading these chunks do not belong to.
///
/// The outline is read without an analyzer; these chunks come from an
/// index built with one, where recognized formulas and tables displaced
/// every offset after the first of them. The byte offsets below are
/// therefore deliberately wrong for this rendition — as a real one would
/// be — and resolution must not consult them.
#[test]
fn a_paginated_entry_is_placed_by_page_and_title_not_by_a_stale_offset() {
    let chunk = |ordinal: usize, page: u32, text: &str| ExportedChunk {
        chunk: ChunkExport {
            chunk_ref: ChunkRef(format!("chunk-{ordinal}")),
            ordinal,
            text: text.to_string(),
            text_sha256: String::new(),
            // Ranges that would resolve every entry below to chunk 0 if
            // the offset were consulted at all.
            byte_range: ByteRange {
                start: ordinal * 1000,
                end: ordinal * 1000 + 1000,
            },
            origin: SourceOrigin::PdfPage { page, bbox: None },
        },
        embedding: Vec::new(),
    };
    let chunks = vec![
        chunk(0, 3, "Prose that opens the page."),
        chunk(
            1,
            3,
            "2.1 Methods and materials, in which the work is set out.",
        ),
        chunk(2, 4, "Appendix A, which is on the next page."),
    ];
    let entry = |title: &str, page: u32| wilkes_core::types::OutlineEntry {
        title: title.to_string(),
        level: 0,
        page: Some(page),
        byte_offset: Some(0),
        anchor: wilkes_core::types::OutlineAnchor::DestinationCoordinate,
    };

    // The title is found on its page: the section starts in that chunk,
    // not in the first chunk of the page and not where the offset points.
    let resolved = resolve_outline(&[entry("2.1 Methods", 3)], &chunks);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].chunk_ordinal, 1);
    // The locator the outline expressed survives alongside the resolution,
    // stale as it is against this rendition.
    assert_eq!(resolved[0].byte_offset, Some(0));

    // A title nothing on the page matches — renumbered, restyled, set as
    // an image — falls back to the page, which is where it landed before.
    let renamed = resolve_outline(&[entry("Chapter the Second", 3)], &chunks);
    assert_eq!(renamed[0].chunk_ordinal, 0);

    // And a title that only appears on a *later* page does not pull the
    // section forward to it: the page is the bound.
    let elsewhere = resolve_outline(&[entry("Appendix A", 3)], &chunks);
    assert_eq!(elsewhere[0].chunk_ordinal, 0);
}

/// Neighbours weigh what the passages asked for weigh, so a window counts
/// against the same reply bound, and an empty window is not a request.
#[tokio::test]
async fn resolve_counts_a_neighbor_window_against_the_reply_bound() {
    let (_dir, ctx) = test_ctx();
    let refs = |count: usize| -> Vec<ChunkRef> {
        (0..count).map(|n| ChunkRef(format!("chunk-{n}"))).collect()
    };
    let one_each_side = NeighborWindow {
        before: 1,
        after: 1,
    };

    let empty = ctx
        .resolve_chunks(
            refs(1),
            Some(NeighborWindow {
                before: 0,
                after: 0,
            }),
        )
        .await
        .unwrap_err();
    assert!(empty.message().contains("omit `neighbors`"), "{empty}");

    let over = ctx
        .resolve_chunks(refs(171), Some(one_each_side))
        .await
        .unwrap_err();
    assert!(over.message().contains("up to 513 passages"), "{over}");

    // At the bound the request is admitted, and goes on to the index —
    // which this context does not have.
    let admitted = ctx
        .resolve_chunks(refs(170), Some(one_each_side))
        .await
        .unwrap_err();
    assert!(!admitted.message().contains("most one reply"), "{admitted}");
}

/// A text probe reaches the embedder in the query role and lands where
/// the caller put it, beside vectors the caller already held.
#[tokio::test]
async fn text_probes_resolve_in_place_beside_vector_probes() {
    let (_dir, ctx) = test_ctx();
    *ctx.embedder.lock() = Some(Arc::new(MockEmbedder::default()) as Arc<dyn Embedder>);

    let mut held = vec![0.0f32; 384];
    held[7] = 1.0;
    let resolved = ctx
        .resolve_search_probes(vec![
            SearchProbeInput::Text("what teaches this".to_string()),
            SearchProbeInput::Vector(held.clone()),
            SearchProbeInput::Text("and this".to_string()),
        ])
        .await
        .expect("probes resolve");

    assert_eq!(resolved.len(), 3);
    assert_eq!(resolved[1], held, "a held vector passes through untouched");
    assert_eq!(resolved[0].len(), 384);
    assert_eq!(resolved[2].len(), 384);
}

/// Without a text probe there is nothing to embed, so a search over held
/// vectors must not require a live embedder.
#[tokio::test]
async fn vector_only_probes_need_no_embedder() {
    let (_dir, ctx) = test_ctx();
    assert!(ctx.embedder.lock().is_none());
    let resolved = ctx
        .resolve_search_probes(vec![SearchProbeInput::Vector(vec![1.0, 0.0])])
        .await
        .expect("vectors need no model");
    assert_eq!(resolved, vec![vec![1.0, 0.0]]);
}

#[tokio::test]
async fn an_empty_text_probe_is_refused() {
    let (_dir, ctx) = test_ctx();
    *ctx.embedder.lock() = Some(Arc::new(MockEmbedder::default()) as Arc<dyn Embedder>);
    let error = ctx
        .resolve_search_probes(vec![SearchProbeInput::Text("   ".to_string())])
        .await
        .expect_err("an empty probe names nothing");
    assert!(error.contains("empty"), "{error}");
}
