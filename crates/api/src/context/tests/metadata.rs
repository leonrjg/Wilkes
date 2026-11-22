use super::*;

#[tokio::test]
async fn a_title_one_work_bears_gives_its_doi() {
    let source = TitleSearchSource(Some(vec![
        ("Story Embeddings", "10.1/other"),
        (
            "Aspect-based Document Similarity for Research Papers",
            "10.18653/v1/2020.coling-main.545",
        ),
    ]));
    let doi = doi_by_title(
        &source,
        Path::new("/library/ostendorff.pdf"),
        "Aspect-based Document Similarity for Research Papers",
        None,
    )
    .await;
    assert_eq!(doi.as_deref(), Some("10.18653/v1/2020.coling-main.545"));
}

#[tokio::test]
async fn a_title_two_works_bear_gives_no_doi() {
    let source = TitleSearchSource(Some(vec![
        ("The narrative arc", "10.1126/sciadv.aba2196"),
        ("The narrative arc", "10.29025/2079-6021-2022-4-17-34"),
    ]));
    let doi = doi_by_title(
        &source,
        Path::new("/library/boyd.pdf"),
        "The narrative arc",
        None,
    )
    .await;
    assert_eq!(doi, None);
}

#[tokio::test]
async fn a_failed_title_search_gives_no_doi() {
    let source = TitleSearchSource(None);
    let doi = doi_by_title(
        &source,
        Path::new("/library/boyd.pdf"),
        "The narrative arc",
        None,
    )
    .await;
    assert_eq!(doi, None);
}

#[tokio::test]
async fn test_zotero_disable_invalidates_cached_zotero_rows() {
    let (_dir, ctx) = test_ctx();
    let cache = ctx.metadata_cache().expect("cache opens");
    let id = FileIdentity {
        size_bytes: 1,
        modified_at_ms: 1,
    };
    {
        let guard = cache.lock().unwrap();
        guard
            .upsert(
                Path::new("/f.pdf"),
                id,
                &DocumentMetadata::default(),
                MetadataSource::File,
            )
            .unwrap();
        guard
            .upsert(
                Path::new("/z.pdf"),
                id,
                &DocumentMetadata::default(),
                MetadataSource::Zotero,
            )
            .unwrap();
    }

    let mut before = Settings::default();
    before.integrations.zotero.enabled = true;
    let after = Settings::default(); // Zotero disabled.

    ctx.on_zotero_settings_maybe_changed(&before, &after);

    let guard = cache.lock().unwrap();
    assert!(
        guard.get_valid(Path::new("/z.pdf"), id).unwrap().is_none(),
        "zotero-sourced row should be dropped on disable"
    );
    assert!(
        guard.get_valid(Path::new("/f.pdf"), id).unwrap().is_some(),
        "file-sourced row should survive"
    );
}
