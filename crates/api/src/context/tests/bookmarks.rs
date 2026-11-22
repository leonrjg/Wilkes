use super::*;

#[tokio::test]
async fn saved_work_acquisition_follows_a_renamed_file() {
    let (dir, ctx) = test_ctx();
    let work: LiteratureSearchResult = serde_json::from_value(serde_json::json!({
        "id": "W1",
        "title": "A Study",
        "is_open_access": true,
    }))
    .unwrap();
    ctx.save_work(NewSavedWork {
        provider: "openalex".into(),
        work: work.clone(),
        note: None,
    })
    .unwrap();
    let landed = dir.path().join("study.pdf");
    std::fs::write(&landed, b"pdf").unwrap();
    ctx.mark_saved_work_acquired("openalex", &work, landed.clone())
        .unwrap()
        .expect("the saved work is marked");

    let renamed = dir.path().join("renamed.pdf");
    std::fs::rename(&landed, &renamed).unwrap();
    let identity = FileIdentity::for_path(&renamed).unwrap();
    ctx.metadata_cache()
        .expect("cache opens")
        .lock()
        .unwrap()
        .upsert(
            &renamed,
            identity,
            &DocumentMetadata::default(),
            MetadataSource::File,
        )
        .unwrap();

    let works = ctx.list_saved_works().await.unwrap();
    assert_eq!(works.len(), 1);
    assert_eq!(works[0].acquired.as_ref().unwrap().path, renamed);
}

#[tokio::test]
async fn test_bookmark_methods_round_trip() {
    let (_dir, ctx) = test_ctx();

    let bookmark = ctx
        .add_bookmark(wilkes_core::types::NewBookmark {
            path: "/tmp/example.pdf".into(),
            origin: SourceOrigin::PdfPage {
                page: 2,
                bbox: None,
            },
            text_range: None,
            quote: "important".to_string(),
            note: None,
            rects: Vec::new(),
        })
        .await
        .unwrap();

    assert_eq!(ctx.list_bookmarks().await.unwrap().len(), 1);
    assert!(bookmark.note.is_none());

    let noted = ctx
        .update_bookmark_note(&bookmark.id, Some("a note".to_string()))
        .await
        .unwrap();
    assert_eq!(noted.note.as_deref(), Some("a note"));
    assert_eq!(
        ctx.list_bookmarks().await.unwrap()[0].note.as_deref(),
        Some("a note")
    );

    ctx.remove_bookmark(&bookmark.id).await.unwrap();
    assert!(ctx.list_bookmarks().await.unwrap().is_empty());
}

#[tokio::test]
async fn test_cluster_bookmarks_embeds_passages_and_reuses_cache() {
    let (_dir, ctx) = test_ctx();
    let calls = Arc::new(AtomicUsize::new(0));
    *ctx.embedder.lock() = Some(Arc::new(TopicEmbedder {
        calls: Arc::clone(&calls),
    }));

    let mut bookmark_ids = Vec::new();
    for quote in [
        "cat behavior",
        "cat nutrition",
        "cat genetics",
        "quantum fields",
        "particle physics",
        "wave functions",
    ] {
        let bookmark = ctx
            .add_bookmark(NewBookmark {
                path: PathBuf::from("/tmp/paper.pdf"),
                origin: SourceOrigin::PdfPage {
                    page: 1,
                    bbox: None,
                },
                text_range: None,
                quote: quote.into(),
                note: None,
                rects: Vec::new(),
            })
            .await
            .unwrap();
        bookmark_ids.push(bookmark.id);
    }
    let query = BookmarkClustersQuery {
        bookmark_ids: bookmark_ids.clone(),
        granularity: wilkes_core::types::BookmarkClusterGranularity::Balanced,
    };

    let first = ctx.clone().cluster_bookmarks(query.clone()).await.unwrap();
    assert_eq!(first.clusters.len(), 2);
    assert_eq!(first.clusters[0].bookmark_ids, bookmark_ids[..3]);
    assert_eq!(first.clusters[1].bookmark_ids, bookmark_ids[3..]);
    assert!(first.unclustered_bookmark_ids.is_empty());
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    let second = ctx.clone().cluster_bookmarks(query.clone()).await.unwrap();
    assert_eq!(second.clusters.len(), 2);
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "the second request should use persisted bookmark embeddings"
    );

    let mut more_granular_query = query.clone();
    more_granular_query.granularity = wilkes_core::types::BookmarkClusterGranularity::More;
    ctx.clone()
        .cluster_bookmarks(more_granular_query)
        .await
        .unwrap();
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "changing granularity should reuse persisted bookmark embeddings"
    );

    ctx.update_bookmark_note(&bookmark_ids[0], Some("updated".into()))
        .await
        .unwrap();
    ctx.cluster_bookmarks(query).await.unwrap();
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "editing one note should embed only that cache miss"
    );
}
