use super::*;

#[tokio::test]
async fn reads_active_document_when_path_is_omitted() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("active.txt");
    std::fs::write(&path, "active document text").unwrap();
    let context = ContextStateHandle::default();
    context.set_active_doc(Some(path.to_string_lossy().into_owned()), None);
    let mcp = WilkesMcp::new(McpContext::Session(context), dir.path().to_path_buf(), None);

    let response = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: None,
            page: None,
            page_range: None,
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.text, "active document text");
    assert!(!response.truncated);
}

#[tokio::test]
async fn document_outline_reads_active_document_without_a_semantic_index() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("active.md");
    std::fs::write(&path, "# Overview\ntext\n## Details\nmore text\n").unwrap();
    let context = ContextStateHandle::default();
    context.set_active_doc(Some(path.to_string_lossy().into_owned()), None);
    let mcp = WilkesMcp::new(McpContext::Session(context), dir.path().to_path_buf(), None);

    let response = get_document_outline_for_mcp(
        &mcp,
        GetDocumentOutlineParams {
            path: None,
            workspace: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.path, display_path(&path));
    assert_eq!(response.outline.len(), 2);
    assert_eq!(response.outline[0].title, "Overview");
    assert_eq!(response.outline[0].level, 0);
    assert_eq!(response.outline[1].title, "Details");
    assert_eq!(response.outline[1].level, 1);
    assert_eq!(response.extraction.pages, 0);
}

#[tokio::test]
async fn reads_explicit_context_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("context.txt");
    std::fs::write(&path, "context document text").unwrap();
    let context = ContextStateHandle::default();
    context.add_context(path.to_string_lossy().into_owned(), None);
    let mcp = WilkesMcp::new(McpContext::Session(context), dir.path().to_path_buf(), None);

    let response = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(path.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.text, "context document text");
}

#[tokio::test]
async fn reads_document_nested_in_current_root() {
    let dir = tempdir().unwrap();
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let path = nested.join("document.txt");
    std::fs::write(&path, "root document text").unwrap();
    let context = ContextStateHandle::default();
    context.set_search_root(Some(dir.path().to_string_lossy().into_owned()));
    let mcp = WilkesMcp::new(McpContext::Session(context), dir.path().to_path_buf(), None);

    let response = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(path.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.text, "root document text");
}

#[tokio::test]
async fn in_app_mcp_reads_document_from_another_library_root() {
    let dir = tempdir().unwrap();
    let current_root = dir.path().join("current");
    let other_root = dir.path().join("other");
    std::fs::create_dir(&current_root).unwrap();
    std::fs::create_dir(&other_root).unwrap();
    let path = other_root.join("document.txt");
    std::fs::write(&path, "other library root text").unwrap();

    let context = ContextStateHandle::default();
    context.set_search_root(Some(current_root.to_string_lossy().into_owned()));
    let search = Arc::new(FakeSearch {
        saved: Mutex::default(),
        saved_manifests: Mutex::default(),
        integrations: Mutex::default(),
        read_only: false,
        last_query: Mutex::new(None),
        last_related_query: Mutex::new(None),
        default_root: Some(current_root.clone()),
        library_roots: vec![current_root.clone(), other_root],
        response: Mutex::new(None),
        related_response: Mutex::new(None),
        documents: Mutex::new(None),
        metadata: Mutex::new(None),
    });
    let mcp = WilkesMcp::new(
        McpContext::Session(context),
        current_root,
        Some(single_workspace(search)),
    );

    let scope = mcp.scope(None).await.unwrap();
    let resolved = resolve_document_path(&mcp, Some(path.to_string_lossy().into_owned()), &scope)
        .await
        .unwrap();
    assert_eq!(resolved, path);

    let response = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(path.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.text, "other library root text");
}

#[tokio::test]
async fn denies_file_outside_library_roots_and_context_with_guidance() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("library");
    let sibling = dir.path().join("library-other");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&sibling).unwrap();
    let denied = sibling.join("denied.txt");
    std::fs::write(&denied, "denied").unwrap();
    let context = ContextStateHandle::default();
    context.set_search_root(Some(root.to_string_lossy().into_owned()));
    let mcp = WilkesMcp::new(
        McpContext::Session(context),
        root.clone(),
        Some(single_workspace(fake_search_with_root(root))),
    );

    let err = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(denied.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap_err();

    assert_eq!(
        err,
        format!(
            "{} is not in a configured Wilkes library root or this chat's context. Open its containing directory as a Wilkes root or add the file to the chat context.",
            denied.display()
        )
    );
}

#[cfg(unix)]
#[tokio::test]
async fn denies_symlink_in_current_root_that_resolves_outside_it() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    let root = dir.path().join("library");
    std::fs::create_dir(&root).unwrap();
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, "outside").unwrap();
    let link = root.join("linked.txt");
    symlink(&outside, &link).unwrap();
    let context = ContextStateHandle::default();
    context.set_search_root(Some(root.to_string_lossy().into_owned()));
    let mcp = WilkesMcp::new(
        McpContext::Session(context),
        root.clone(),
        Some(single_workspace(fake_search_with_root(root))),
    );

    let err = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(link.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap_err();

    assert!(err.contains("not in a configured Wilkes library root"));
}

#[tokio::test]
async fn limits_text_on_character_boundary() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("unicode.txt");
    std::fs::write(&path, "aé日b").unwrap();
    let context = ContextStateHandle::default();
    context.add_context(path.to_string_lossy().into_owned(), None);
    let mcp = WilkesMcp::new(McpContext::Session(context), dir.path().to_path_buf(), None);

    let response = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(path.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: Some(3),
            workspace: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.text, "aé日");
    assert!(response.truncated);
}

#[tokio::test]
async fn explicit_page_range_overrides_active_document_page() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("active.txt");
    std::fs::write(&path, "active document text").unwrap();
    let context = ContextStateHandle::default();
    context.set_active_doc(Some(path.to_string_lossy().into_owned()), Some(3));
    let mcp = WilkesMcp::new(McpContext::Session(context), dir.path().to_path_buf(), None);

    let response = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: None,
            page: None,
            page_range: Some("1-5".to_string()),
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.page, None);
    assert_eq!(response.page_range, Some(PageRange { start: 1, end: 5 }));
    assert_eq!(response.text, "active document text");
}

#[tokio::test]
async fn list_documents_reports_titles_dois_and_truncation() {
    let library = tempdir().unwrap();
    let service = fake_search_with_root(library.path().to_path_buf());
    *service.documents.lock().unwrap() = Some(FileListResponse {
        files: vec![
            doc_entry(library.path().join("a.pdf"), "Alpha", Some("10.1/a")),
            doc_entry(library.path().join("b.pdf"), "Beta", None),
            doc_entry(library.path().join("c.pdf"), "Gamma", Some("10.1/c")),
        ],
        omitted: Vec::new(),
        directories: Vec::new(),
    });
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(service)),
    );

    let response = list_documents_for_mcp(
        &mcp,
        ListDocumentsParams {
            root: None,
            scope: None,
            limit: Some(2),
            workspace: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.documents.len(), 2);
    assert!(response.truncated);
    assert_eq!(response.documents[0].title.as_deref(), Some("Alpha"));
    assert_eq!(response.documents[0].doi.as_deref(), Some("10.1/a"));
    assert_eq!(response.documents[1].title.as_deref(), Some("Beta"));
    assert_eq!(response.documents[1].doi, None);
}

#[tokio::test]
async fn get_file_metadata_returns_full_record() {
    let library = tempdir().unwrap();
    let doc = library.path().join("paper.pdf");
    std::fs::write(&doc, b"paper").unwrap();
    let service = fake_search_with_root(library.path().to_path_buf());
    *service.metadata.lock().unwrap() = Some(DocumentMetadata {
        title: Some("Deep Nets".into()),
        author: Some("Ada".into()),
        doi: Some("10.1/deep".into()),
        created_at: Some("2024-05".into()),
        providers: Default::default(),
    });
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(service)),
    );

    let response = get_file_metadata_for_mcp(
        &mcp,
        GetFileMetadataParams {
            path: Some(doc.to_string_lossy().into_owned()),
            workspace: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.path, display_path(&doc));
    assert_eq!(response.metadata.title.as_deref(), Some("Deep Nets"));
    assert_eq!(response.metadata.author.as_deref(), Some("Ada"));
    assert_eq!(response.metadata.doi.as_deref(), Some("10.1/deep"));
}

#[tokio::test]
async fn get_file_metadata_requires_explicit_path_without_active_document() {
    let library = tempdir().unwrap();
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(
            library.path().to_path_buf(),
        ))),
    );

    let error = get_file_metadata_for_mcp(
        &mcp,
        GetFileMetadataParams {
            path: None,
            workspace: None,
        },
    )
    .await
    .unwrap_err();
    assert!(error.contains("pass path explicitly"));
}
