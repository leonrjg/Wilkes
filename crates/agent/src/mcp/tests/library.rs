use super::*;

#[tokio::test]
async fn literature_results_cached_by_one_request_are_found_by_the_next() {
    // The server is stateless: every request builds a fresh WilkesMcp, so a
    // search and the download that follows it never share an instance.
    let cache = LiteratureResultCache::default();
    let build = || {
        WilkesMcp::new(
            McpContext::Library(ExternalMcpContext::default()),
            PathBuf::new(),
            None,
        )
        .with_literature_results(cache.clone())
    };
    let searched = build();
    searched.literature_results.lock().unwrap().insert(
        ("openalex".to_string(), "W1".to_string()),
        LiteratureSearchResult {
            id: "W1".to_string(),
            doi: None,
            title: None,
            year: None,
            publication_date: None,
            venue: None,
            citation_count: None,
            is_open_access: true,
            pdf_url: Some("https://example.org/a.pdf".to_string()),
            landing_page_url: None,
            image_url: None,
            open_access_status: None,
            license: None,
            authors: None,
            publisher: None,
            language: None,
            file_format: None,
            file_size: None,
            summary: None,
            subject: None,
            kind: None,
            pages: None,
            recall_score: None,
            acquisition: LiteratureAcquisition::Direct,
        },
    );
    let downloading = build();
    assert!(downloading
        .literature_results
        .lock()
        .unwrap()
        .contains_key(&("openalex".to_string(), "W1".to_string())));
}

#[tokio::test]
async fn research_edits_are_workspace_scoped_and_reject_protected_workspaces() {
    let root = tempdir().unwrap();
    let mut service = fake_search_with_root(root.path().into());
    Arc::get_mut(&mut service).unwrap().read_only = true;
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(service)),
    );
    let reply = mcp
        .edit_library(Parameters(EditLibraryParams {
            workspace: None,
            edit: crate::library::LibraryEdit::CreateTag {
                name: "test".into(),
                color: None,
            },
        }))
        .await;
    assert_eq!(reply.is_error, Some(true));
    assert!(serde_json::to_string(&reply)
        .unwrap()
        .contains("MANAGED_WORKSPACE_PROTECTED"));
}

#[tokio::test]
async fn research_edits_confine_canonical_paths_and_refuse_root_renames() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let file = root.path().join("notes.md");
    std::fs::write(&file, "notes").unwrap();
    let other = outside.path().join("outside.md");
    std::fs::write(&other, "outside").unwrap();
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(root.path().into()))),
    );
    let edit = |path: PathBuf| EditLibraryParams {
        workspace: None,
        edit: crate::library::LibraryEdit::RenameFile {
            path,
            new_name: "new.md".into(),
        },
    };
    let accepted = mcp.edit_library(Parameters(edit(file.clone()))).await;
    assert_ne!(accepted.is_error, Some(true));
    assert_eq!(
        accepted.structured_content.unwrap()["path"],
        file.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    assert_eq!(
        mcp.edit_library(Parameters(edit(other.clone())))
            .await
            .is_error,
        Some(true)
    );
    assert_eq!(
        mcp.edit_library(Parameters(edit(root.path().into())))
            .await
            .is_error,
        Some(true)
    );
    #[cfg(unix)]
    {
        let link = root.path().join("link.md");
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert_eq!(
            mcp.edit_library(Parameters(edit(link))).await.is_error,
            Some(true)
        );
    }
    assert!(file.exists());
    assert!(other.exists());
}

#[tokio::test]
async fn save_literature_result_saves_the_whole_cached_result() {
    let root = tempdir().unwrap();
    let service = fake_search_with_root(root.path().into());
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(service.clone())),
    );
    let mut work: LiteratureSearchResult = serde_json::from_value(serde_json::json!({
        "id": "W1",
        "title": "A Study",
        "is_open_access": true,
    }))
    .unwrap();
    work.pdf_url = Some("https://example.org/a.pdf".into());
    mcp.literature_results
        .lock()
        .unwrap()
        .insert(("openalex".into(), "W1".into()), work);
    let params = |result_id: &str| SaveLiteratureResultParams {
        provider: "openalex".into(),
        result_id: result_id.into(),
        note: Some("for chapter 2".into()),
        workspace: None,
    };

    let reply = mcp.save_literature_result(Parameters(params("W1"))).await;
    assert_ne!(reply.is_error, Some(true));
    // The URL the model never saw is in what was saved.
    assert_eq!(
        reply.structured_content.unwrap()["work"]["pdf_url"],
        "https://example.org/a.pdf"
    );
    assert_eq!(
        *service.saved.lock().unwrap(),
        vec![("openalex".into(), "W1".into(), Some("for chapter 2".into()))]
    );

    let unknown = mcp.save_literature_result(Parameters(params("W2"))).await;
    assert_eq!(unknown.is_error, Some(true));
    assert_eq!(service.saved.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn save_literature_result_is_refused_for_protected_workspaces() {
    let root = tempdir().unwrap();
    let mut service = fake_search_with_root(root.path().into());
    Arc::get_mut(&mut service).unwrap().read_only = true;
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(service.clone())),
    );
    let reply = mcp
        .save_literature_result(Parameters(SaveLiteratureResultParams {
            provider: "openalex".into(),
            result_id: "W1".into(),
            note: None,
            workspace: None,
        }))
        .await;
    assert_eq!(reply.is_error, Some(true));
    assert!(serde_json::to_string(&reply)
        .unwrap()
        .contains("MANAGED_WORKSPACE_PROTECTED"));
    assert!(service.saved.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_saved_manifest_is_listed_with_the_authoring_guide_but_not_its_text() {
    let root = tempdir().unwrap();
    let service = fake_search_with_root(root.path().into());
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(service.clone())),
    );
    let saved = mcp
        .save_custom_integration(Parameters(SaveCustomIntegrationParams {
            manifest: "id = \"fake\"".into(),
            enabled: Some(true),
        }))
        .await;
    assert_ne!(saved.is_error, Some(true));
    assert_eq!(
        *service.saved_manifests.lock().unwrap(),
        vec![("id = \"fake\"".to_string(), Some(true))]
    );

    let read = mcp
        .read_custom_integrations(Parameters(ReadCustomIntegrationsParams {
            authoring_guide: Some(true),
        }))
        .await
        .structured_content
        .unwrap();
    assert_eq!(read["integrations"][0]["provider"], "custom:fake");
    assert!(read["integrations"][0].get("manifest").is_none());
    assert_eq!(
        read["authoring_guide"],
        wilkes_core::integrations::custom::manifest::authoring_prompt()
    );
    // There is no way to name one integration and ask for its manifest.
    assert!(serde_json::from_value::<ReadCustomIntegrationsParams>(
        serde_json::json!({ "id": "fake" })
    )
    .is_err());
}

#[tokio::test]
async fn a_chat_can_search_a_provider_saved_after_it_started() {
    let root = tempdir().unwrap();
    let service = fake_search_with_root(root.path().into());
    let mcp = WilkesMcp::new(
        McpContext::Session(ContextStateHandle::default()),
        root.path().into(),
        Some(single_workspace(service.clone())),
    );
    assert!(!mcp
        .literature_registry()
        .await
        .enabled_literature_ids()
        .contains(&"custom:later"));

    service
        .integrations
        .lock()
        .unwrap()
        .custom
        .push(wilkes_core::types::CustomIntegrationConfig {
            id: "later".to_string(),
            name: None,
            enabled: true,
            manifest: r#"manifest_version = 2
id = "later"
name = "Later"

[http]
base_url = "https://later.test"

[capabilities.search]
path = "/search?q={query}"

[capabilities.search.fields]
id = "id"
title = "title"
"#
            .to_string(),
            secrets: Default::default(),
        });

    assert!(mcp
        .literature_registry()
        .await
        .enabled_literature_ids()
        .contains(&"custom:later"));
}

#[test]
fn only_reading_custom_integrations_skips_the_prompt() {
    assert!(WILKES_MCP_TOOL_NAMES.contains(&"read_custom_integrations"));
    assert!(!WILKES_MCP_TOOL_NAMES.contains(&"probe_custom_integration"));
    assert!(!WILKES_MCP_TOOL_NAMES.contains(&"save_custom_integration"));
}

#[tokio::test]
async fn research_reads_page_without_losing_the_total() {
    let root = tempdir().unwrap();
    let mcp = WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(root.path().into()))),
    );
    let reply = mcp
        .read_library(Parameters(ReadLibraryParams {
            kind: crate::library::LibraryKind::Tags,
            workspace: None,
            offset: 1,
            limit: Some(1),
        }))
        .await
        .structured_content
        .unwrap();
    assert_eq!(reply["items"][0]["id"], "two");
    assert_eq!(reply["total"], 2);
    assert!(reply["next_offset"].is_null());
}
