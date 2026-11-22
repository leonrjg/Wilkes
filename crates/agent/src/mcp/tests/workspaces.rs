use super::*;

#[test]
fn list_context_includes_root_and_first_three_files() {
    let dir = tempdir().unwrap();
    for name in ["04.txt", "02.txt", "01.txt", "03.txt"] {
        std::fs::write(dir.path().join(name), name).unwrap();
    }
    let context = ContextStateHandle::default();
    context.set_search_root(Some(dir.path().to_string_lossy().into_owned()));
    let snapshot = context.snapshot();
    let response = ListContextResponse::from_snapshot(
        snapshot.root,
        snapshot.active_doc,
        snapshot.context_files,
        vec![dir.path().to_path_buf()],
        Vec::new(),
        None,
        Vec::new(),
    );

    assert_eq!(response.current_root.as_deref(), dir.path().to_str());
    assert_eq!(
        response.roots,
        vec![dir.path().to_string_lossy().into_owned()]
    );
    assert_eq!(response.first_files.len(), 3);
    assert!(response.first_files[0].ends_with("01.txt"));
    assert!(response.first_files[1].ends_with("02.txt"));
    assert!(response.first_files[2].ends_with("03.txt"));
}

#[tokio::test]
async fn list_context_reports_every_workspace_and_defaults_to_the_active_one() {
    let active = tempdir().unwrap();
    let other = tempdir().unwrap();
    let (catalog, _) = two_workspaces(active.path().to_path_buf(), other.path().to_path_buf());
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(Arc::clone(&catalog) as Arc<dyn WorkspaceCatalog>),
    );

    let response = serde_json::to_value(
        mcp.list_context(Parameters(WorkspaceParams { workspace: None }))
            .await,
    )
    .unwrap();
    let context = &response["structuredContent"];
    assert_eq!(context["workspaces"].as_array().unwrap().len(), 2);
    assert_eq!(context["workspaces"][0]["id"], "active-id");
    assert_eq!(context["workspaces"][0]["active"], true);
    assert_eq!(context["workspaces"][1]["id"], "other-id");
    assert_eq!(context["workspaces"][1]["active"], false);
    assert_eq!(context["workspace"]["id"], "active-id");
    assert_eq!(context["current_root"], display_path(active.path()));

    // Naming a workspace describes that workspace's roots instead.
    let response = serde_json::to_value(
        mcp.list_context(Parameters(WorkspaceParams {
            workspace: Some("other-id".to_string()),
        }))
        .await,
    )
    .unwrap();
    let context = &response["structuredContent"];
    assert_eq!(context["workspace"]["id"], "other-id");
    assert_eq!(context["current_root"], display_path(other.path()));
    assert_eq!(
        catalog.resolutions(),
        vec![None, Some("other-id".to_string())]
    );
}

#[tokio::test]
async fn named_workspace_reads_its_own_library_without_activating_it() {
    let active = tempdir().unwrap();
    let other = tempdir().unwrap();
    let document = other.path().join("paper.txt");
    std::fs::write(&document, "text from the other workspace").unwrap();
    let (catalog, _) = two_workspaces(active.path().to_path_buf(), other.path().to_path_buf());
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(Arc::clone(&catalog) as Arc<dyn WorkspaceCatalog>),
    );

    // The active workspace's roots do not admit it...
    let denied = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(document.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap_err();
    assert!(denied.contains("not in a configured Wilkes library root"));

    // ...and naming the workspace that owns it does.
    let response = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(document.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: Some("other-id".to_string()),
        },
    )
    .await
    .unwrap();
    assert_eq!(response.text, "text from the other workspace");
    assert_eq!(
        catalog.resolutions(),
        vec![None, Some("other-id".to_string())]
    );
}

#[tokio::test]
async fn refusal_names_the_workspace_the_path_was_checked_against() {
    let active = tempdir().unwrap();
    let other = tempdir().unwrap();
    let stray = tempdir().unwrap();
    let document = stray.path().join("paper.txt");
    std::fs::write(&document, "outside every workspace").unwrap();
    let (catalog, _) = two_workspaces(active.path().to_path_buf(), other.path().to_path_buf());
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(catalog),
    );

    let error = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(document.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: Some("other-id".to_string()),
        },
    )
    .await
    .unwrap_err();
    assert!(
        error.contains("library root of workspace other-id"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn unknown_workspace_fails_the_call_instead_of_reading_the_active_one() {
    let active = tempdir().unwrap();
    let document = active.path().join("paper.txt");
    std::fs::write(&document, "active workspace text").unwrap();
    let other = tempdir().unwrap();
    let (catalog, _) = two_workspaces(active.path().to_path_buf(), other.path().to_path_buf());
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(catalog),
    );

    let error = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(document.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: Some("missing-id".to_string()),
        },
    )
    .await
    .unwrap_err();
    assert!(
        error.contains("Workspace missing-id is not available"),
        "unexpected error: {error}"
    );

    let listing = list_documents_for_mcp(
        &mcp,
        ListDocumentsParams {
            root: None,
            scope: None,
            limit: None,
            workspace: Some("missing-id".to_string()),
        },
    )
    .await
    .unwrap_err();
    assert!(listing.contains("Workspace missing-id is not available"));
}

#[tokio::test]
async fn chat_session_naming_a_workspace_leaves_its_own_context_behind() {
    let chat_root = tempdir().unwrap();
    let other = tempdir().unwrap();
    let context_file = tempdir().unwrap();
    let added = context_file.path().join("added.txt");
    std::fs::write(&added, "explicitly added to the chat").unwrap();

    let context = ContextStateHandle::default();
    context.set_search_root(Some(chat_root.path().to_string_lossy().into_owned()));
    context.add_context(added.to_string_lossy().into_owned(), None);
    let (catalog, _) = two_workspaces(chat_root.path().to_path_buf(), other.path().to_path_buf());
    let mcp = WilkesMcp::new(
        McpContext::Session(context),
        chat_root.path().to_path_buf(),
        Some(catalog),
    );

    // The chat's own context file is readable while the call stays in the
    // chat's workspace.
    let response = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(added.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(response.text, "explicitly added to the chat");

    // Naming another workspace makes the call that workspace's, so the
    // chat's own context no longer admits the path.
    let error = get_document_text_for_mcp(
        &mcp,
        GetDocumentTextParams {
            path: Some(added.to_string_lossy().into_owned()),
            page: None,
            page_range: None,
            max_chars: None,
            workspace: Some("other-id".to_string()),
        },
    )
    .await
    .unwrap_err();
    assert!(
        error.contains("library root of workspace other-id"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn a_server_without_workspace_access_refuses_a_named_workspace() {
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        None,
    );
    let error = match mcp.scope(Some("any-id")).await {
        Ok(_) => panic!("expected a named workspace to be refused"),
        Err(error) => error,
    };
    assert!(
        error.contains("no Wilkes workspace access"),
        "unexpected error: {error}"
    );
    let scope = match mcp.scope(None).await {
        Ok(scope) => scope,
        Err(error) => panic!("expected an unnamed scope to resolve: {error}"),
    };
    assert!(scope.search().is_none());
}
