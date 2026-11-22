use super::*;

#[test]
fn semantic_index_errors_include_enablement_and_exact_search_guidance() {
    for message in [
        "No semantic index found. Build the index first.",
        "Semantic index is currently being built. Please wait.",
        "The global semantic index has no searchable documents.",
    ] {
        let enriched = with_semantic_index_guidance(message.to_string());
        assert!(enriched.starts_with(message));
        assert!(enriched.contains("The user can enable the semantic index"));
        assert!(enriched.contains("mode='exact'"));
        assert!(enriched.contains("in the meantime"));
    }

    let unrelated = "Search root does not exist.".to_string();
    assert_eq!(with_semantic_index_guidance(unrelated.clone()), unrelated);
}

#[test]
fn an_omitted_mode_searches_wording_and_meaning_together() {
    // The tool used to make the caller choose between the terminology and
    // the problem before it had seen either. Omitting mode now asks for
    // both, and a regular expression is not part of that question.
    let params: SearchParams = serde_json::from_value(serde_json::json!({
        "query": "instrumental variables weak identification",
        "is_regex": true,
    }))
    .unwrap();
    assert_eq!(params.mode, SearchModeParam::Combined);

    let dir = tempdir().unwrap();
    let (query, _) = build_search_query(dir.path().to_path_buf(), params, 1024 * 1024).unwrap();

    assert_eq!(query.mode, SearchMode::Hybrid);
    assert!(
        !query.is_regex,
        "a regular expression describes wording, and belongs to exact search"
    );
}

#[tokio::test]
async fn a_reduced_combined_search_says_what_it_could_not_reach() {
    // The exact lane ran and its matches are the answer, so the missing
    // semantic lane is a note about this search rather than a fault in it:
    // reported as an error, a caller reads the call as failed and runs it
    // again, and it reduces again.
    let response = combined_search_response(collected(
        SearchStats {
            errors: vec![
                "Combined search found exact matches only — related passages need the semantic index: No semantic index found. Build the index first."
                    .to_string(),
            ],
            ..SearchStats::default()
        },
        true,
    ))
    .await;

    assert!(
        response.stats.errors.is_empty(),
        "{:?}",
        response.stats.errors
    );
    assert_eq!(response.notes.len(), 1);
    assert!(response.notes[0].ends_with(COMBINED_INDEX_GUIDANCE));
    // The guidance must not send the caller back to run the exact search
    // it already has the results of.
    assert!(!response.notes[0].contains("mode='exact'"));
    assert!(response.complete);
}

#[tokio::test]
async fn a_combined_search_keeps_its_faults_as_errors() {
    let response = combined_search_response(collected(
        SearchStats {
            errors: vec!["/corpus/broken.pdf: failed to extract".to_string()],
            ..SearchStats::default()
        },
        true,
    ))
    .await;

    assert_eq!(response.stats.errors.len(), 1);
    assert!(response.notes.is_empty());
}

#[tokio::test]
async fn a_search_that_ran_out_of_budget_says_the_scan_is_unfinished() {
    // Silence here would pass a partial scan off as the corpus's answer.
    let response = combined_search_response(collected(SearchStats::default(), false)).await;

    assert!(!response.complete);
    assert_eq!(response.notes.len(), 1);
    assert!(response.notes[0].contains("stopped after"));
}

#[test]
fn builds_bounded_exact_search_query() {
    let dir = tempdir().unwrap();
    let explicit_root = dir.path().join("root");
    let (query, max_files) = build_search_query(
        explicit_root.clone(),
        SearchParams {
            query: "  IO programming  ".to_string(),
            mode: SearchModeParam::Exact,
            scope: None,
            root: None,
            file: None,
            max_results: Some(500),
            case_sensitive: Some(true),
            is_regex: Some(true),
            context_lines: Some(100),
            collection_id: None,
            workspace: None,
        },
        23 * 1024 * 1024,
    )
    .unwrap();

    assert_eq!(query.pattern, "IO programming");
    assert_eq!(query.mode, SearchMode::Grep);
    assert_eq!(query.root, explicit_root);
    assert_eq!(query.max_results, MAX_SEARCH_MAX_RESULTS);
    assert_eq!(max_files, MAX_SEARCH_MAX_RESULTS);
    assert!(query.case_sensitive);
    assert!(query.is_regex);
    assert_eq!(query.context_lines, MAX_SEARCH_CONTEXT_LINES);
    assert!(query.respect_gitignore);
    assert_eq!(query.max_file_size, 23 * 1024 * 1024);
}

#[test]
fn semantic_search_ignores_regex_flag() {
    let dir = tempdir().unwrap();
    let (query, _) = build_search_query(
        dir.path().to_path_buf(),
        SearchParams {
            query: "definitions".to_string(),
            mode: SearchModeParam::Semantic,
            scope: None,
            root: None,
            file: None,
            max_results: None,
            case_sensitive: None,
            is_regex: Some(true),
            context_lines: None,
            collection_id: None,
            workspace: None,
        },
        0,
    )
    .unwrap();

    assert_eq!(query.mode, SearchMode::Semantic);
    assert_eq!(query.root, dir.path());
    assert!(!query.is_regex);
    assert_eq!(query.max_results, DEFAULT_SEARCH_MAX_RESULTS);
}

#[test]
fn builds_file_scoped_search_query() {
    let dir = tempdir().unwrap();
    let (query, _) = build_search_query(
        dir.path().to_path_buf(),
        SearchParams {
            query: "definitions".to_string(),
            mode: SearchModeParam::Exact,
            scope: None,
            root: None,
            file: Some("paper.pdf".to_string()),
            max_results: None,
            case_sensitive: None,
            is_regex: None,
            context_lines: None,
            collection_id: None,
            workspace: None,
        },
        0,
    )
    .unwrap();

    assert_eq!(
        query.scope,
        SearchScope::File {
            path: PathBuf::from("paper.pdf")
        }
    );
}

#[test]
fn file_takes_precedence_over_all_scope() {
    let dir = tempdir().unwrap();
    let (query, _) = build_search_query(
        dir.path().to_path_buf(),
        SearchParams {
            query: "definitions".to_string(),
            mode: SearchModeParam::Exact,
            scope: Some(SearchScopeParam::All),
            root: None,
            file: Some("paper.pdf".to_string()),
            max_results: None,
            case_sensitive: None,
            is_regex: None,
            context_lines: None,
            collection_id: None,
            workspace: None,
        },
        0,
    )
    .unwrap();

    assert_eq!(
        query.scope,
        SearchScope::File {
            path: PathBuf::from("paper.pdf")
        }
    );
}

#[test]
fn builds_all_scoped_search_query() {
    let dir = tempfile::tempdir().unwrap();
    let (query, _) = build_search_query(
        dir.path().to_path_buf(),
        SearchParams {
            query: "across the library".into(),
            mode: SearchModeParam::Exact,
            scope: Some(SearchScopeParam::All),
            root: None,
            file: None,
            max_results: None,
            case_sensitive: None,
            is_regex: None,
            context_lines: None,
            collection_id: None,
            workspace: None,
        },
        0,
    )
    .unwrap();

    assert_eq!(query.scope, SearchScope::All);
}

#[tokio::test]
async fn related_documents_uses_active_document_and_all_scope() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("library");
    let active = root.join("paper.md");
    let service = Arc::new(FakeSearch {
        saved: Mutex::default(),
        saved_manifests: Mutex::default(),
        integrations: Mutex::default(),
        read_only: false,
        last_query: Mutex::new(None),
        last_related_query: Mutex::new(None),
        default_root: None,
        library_roots: Vec::new(),
        response: Mutex::new(None),
        related_response: Mutex::new(Some(Vec::new())),
        documents: Mutex::new(None),
        metadata: Mutex::new(None),
    });
    let context = ContextStateHandle::default();
    context.set_search_root(Some(root.to_string_lossy().into_owned()));
    context.set_active_doc(Some(active.to_string_lossy().into_owned()), None);

    let response = get_related_documents(
        &context,
        Some(service.clone()),
        dir.path(),
        GetRelatedDocumentsParams {
            path: None,
            scope: Some(SearchScopeParam::All),
            root: None,
            limit: Some(100),
            collection_id: None,
            workspace: None,
        },
    )
    .await
    .unwrap();

    let query = service.last_related_query.lock().unwrap().clone().unwrap();
    assert_eq!(query.path, active);
    assert_eq!(query.root, root);
    assert_eq!(query.scope, SearchScope::All);
    assert_eq!(query.limit, Some(MAX_RELATED_DOCUMENTS_LIMIT));
    assert_eq!(response.scope, SearchScopeParamResponse::All);
    assert!(response.documents.is_empty());
}

#[tokio::test]
async fn search_documents_maps_service_results() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("paper.pdf");
    let default_root = dir.path().join("active-root");
    let live_root = dir.path().join("live-ui-root");
    let service = Arc::new(FakeSearch {
        saved: Mutex::default(),
        saved_manifests: Mutex::default(),
        integrations: Mutex::default(),
        read_only: false,
        last_query: Mutex::new(None),
        last_related_query: Mutex::new(None),
        default_root: Some(default_root.clone()),
        library_roots: Vec::new(),
        response: Mutex::new(Some(CollectedSearch {
            files: vec![FileMatches {
                path: path.clone(),
                file_type: FileType::Paged,
                title: None,
                field_matches: vec![
                    SearchFieldMatch {
                        field: SearchField::Filename,
                        matched_text: "paper".into(),
                        context_before: String::new(),
                        context_after: ".pdf".into(),
                    },
                    SearchFieldMatch {
                        field: SearchField::Author,
                        matched_text: "IO".into(),
                        context_before: "Ada L".into(),
                        context_after: "vecchi".into(),
                    },
                ],
                matches: vec![Match {
                    text_range: None,
                    matched_text: "IO programming".to_string(),
                    context_before: "before ".to_string(),
                    context_after: " after".to_string(),
                    origin: SourceOrigin::PdfPage {
                        page: 3,
                        bbox: None,
                    },
                    score: Some(0.91),
                }],
                evidence: Vec::new(),
            }],
            stats: SearchStats {
                files_scanned: 1,
                total_matches: 2,
                catalog_elapsed_ms: 0,
                elapsed_ms: 4,
                indexed_pdf_reads: 0,
                live_pdf_fallbacks: 0,
                index_unavailable_fallbacks: 0,
                errors: Vec::new(),
                hyde_documents: Vec::new(),
            },
            truncated: false,
            complete: true,
        })),
        related_response: Mutex::new(None),
        documents: Mutex::new(None),
        metadata: Mutex::new(None),
    });
    let context = ContextStateHandle::default();
    context.set_search_root(Some(live_root.to_string_lossy().into_owned()));

    let response = search_documents(
        &context,
        Some(service.clone()),
        dir.path(),
        SearchParams {
            query: "IO".to_string(),
            mode: SearchModeParam::Semantic,
            scope: None,
            root: None,
            file: None,
            max_results: Some(3),
            case_sensitive: None,
            is_regex: None,
            context_lines: None,
            collection_id: None,
            workspace: None,
        },
    )
    .await
    .unwrap();

    let captured = service.last_query.lock().unwrap().clone().unwrap();
    assert_eq!(captured.mode, SearchMode::Semantic);
    assert_eq!(captured.max_results, 3);
    assert_eq!(captured.max_file_size, 23 * 1024 * 1024);
    assert_eq!(captured.root, live_root);
    assert_eq!(response.mode, SearchModeParam::Semantic);
    assert_eq!(response.root, display_path(&live_root));
    assert_eq!(response.matches.len(), 1);
    assert_eq!(response.matches[0].path, display_path(&path));
    assert_eq!(
        response.matches[0].matches[0].kind,
        SearchMatchKindResponse::Filename
    );
    assert_eq!(response.matches[0].matches[0].text, "paper.pdf");
    assert_eq!(response.matches[0].matches[0].page, None);
    assert_eq!(
        response.matches[0].matches[1].kind,
        SearchMatchKindResponse::Author
    );
    assert_eq!(response.matches[0].matches[1].text, "Ada LIOvecchi");
    assert_eq!(response.matches[0].matches[1].page, None);
    assert_eq!(
        response.matches[0].matches[2].kind,
        SearchMatchKindResponse::Content
    );
    assert_eq!(response.matches[0].matches[2].page, Some(3));
    assert_eq!(
        response.matches[0].matches[2].text,
        "before IO programming after"
    );
    let serialized = serde_json::to_value(&response.matches[0].matches[2]).unwrap();
    assert_eq!(serialized["kind"], "content");
    assert!(serialized.get("context_before").is_none());
    assert!(serialized.get("context_after").is_none());
}

#[tokio::test]
async fn search_documents_prefers_explicit_root_over_default_root() {
    let dir = tempdir().unwrap();
    let explicit_root = dir.path().join("explicit");
    let context = ContextStateHandle::default();
    context.set_search_root(Some(
        dir.path()
            .join("context-root")
            .to_string_lossy()
            .into_owned(),
    ));
    let service = Arc::new(FakeSearch {
        saved: Mutex::default(),
        saved_manifests: Mutex::default(),
        integrations: Mutex::default(),
        read_only: false,
        last_query: Mutex::new(None),
        last_related_query: Mutex::new(None),
        default_root: Some(dir.path().join("active-root")),
        library_roots: Vec::new(),
        response: Mutex::new(Some(CollectedSearch {
            files: Vec::new(),
            stats: SearchStats::default(),
            truncated: false,
            complete: true,
        })),
        related_response: Mutex::new(None),
        documents: Mutex::new(None),
        metadata: Mutex::new(None),
    });

    let response = search_documents(
        &context,
        Some(service.clone()),
        Path::new("/fallback"),
        SearchParams {
            query: "multi-turn".to_string(),
            mode: SearchModeParam::Exact,
            scope: None,
            root: Some(explicit_root.to_string_lossy().into_owned()),
            file: None,
            max_results: None,
            case_sensitive: None,
            is_regex: None,
            context_lines: None,
            collection_id: None,
            workspace: None,
        },
    )
    .await
    .unwrap();

    let captured = service.last_query.lock().unwrap().clone().unwrap();
    assert_eq!(captured.root, explicit_root);
    assert_eq!(response.root, display_path(&captured.root));
}

#[tokio::test]
async fn search_documents_reports_missing_service() {
    let context = ContextStateHandle::default();
    let err = search_documents(
        &context,
        None,
        Path::new("/tmp"),
        SearchParams {
            query: "anything".to_string(),
            mode: SearchModeParam::Exact,
            scope: None,
            root: None,
            file: None,
            max_results: None,
            case_sensitive: None,
            is_regex: None,
            context_lines: None,
            collection_id: None,
            workspace: None,
        },
    )
    .await
    .unwrap_err();

    assert!(err.contains("not available"));
}

#[tokio::test]
async fn search_results_are_enriched_with_metadata() {
    let live_root = tempdir().unwrap();
    let path = live_root.path().join("hit.pdf");
    std::fs::write(&path, b"hit").unwrap();
    let service = fake_search_with_root(live_root.path().to_path_buf());
    *service.response.lock().unwrap() = Some(CollectedSearch {
        files: vec![FileMatches {
            path: path.clone(),
            file_type: FileType::Paged,
            title: None,
            field_matches: Vec::new(),
            matches: vec![Match {
                text_range: None,
                matched_text: "hit".into(),
                context_before: String::new(),
                context_after: String::new(),
                origin: SourceOrigin::PdfPage {
                    page: 1,
                    bbox: None,
                },
                score: Some(0.5),
            }],
            evidence: Vec::new(),
        }],
        stats: SearchStats::default(),
        truncated: false,
        complete: true,
    });
    *service.metadata.lock().unwrap() = Some(DocumentMetadata {
        title: Some("Hit Paper".into()),
        author: Some("Bo".into()),
        doi: Some("10.1/hit".into()),
        created_at: None,
        providers: Default::default(),
    });
    let context = ContextStateHandle::default();

    let response = search_documents(
        &context,
        Some(service),
        Path::new("/fallback"),
        SearchParams {
            query: "hit".into(),
            mode: SearchModeParam::Exact,
            scope: None,
            root: Some(live_root.path().to_string_lossy().into_owned()),
            file: None,
            max_results: None,
            case_sensitive: None,
            is_regex: None,
            context_lines: None,
            collection_id: None,
            workspace: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.matches.len(), 1);
    assert_eq!(response.matches[0].title.as_deref(), Some("Hit Paper"));
    assert_eq!(response.matches[0].author.as_deref(), Some("Bo"));
    assert_eq!(response.matches[0].doi.as_deref(), Some("10.1/hit"));
}
