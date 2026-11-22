//! The library queries behind the tools: document text and outlines, search,
//! metadata, listings, related documents and the citation graph.

use super::*;

pub(super) async fn get_document_text_for_mcp(
    mcp: &WilkesMcp,
    params: GetDocumentTextParams,
) -> Result<GetDocumentTextResponse, String> {
    let scope = mcp.scope(params.workspace.as_deref()).await?;
    let (path, default_page) = match (&mcp.context, params.path.as_ref()) {
        (_, Some(path)) if !path.trim().is_empty() => (PathBuf::from(path), None),
        (_, Some(_)) => return Err("Document path cannot be empty.".to_string()),
        (McpContext::Session(_), None) if scope.named().is_some() => {
            return Err(WORKSPACE_DOCUMENT_PATH_REQUIRED.to_string());
        }
        (McpContext::Session(context), None) => {
            let active_doc = context
                .snapshot()
                .active_doc
                .ok_or_else(|| "No active Wilkes document is available.".to_string())?;
            (PathBuf::from(active_doc.path), active_doc.page)
        }
        (McpContext::Library(_), None) => {
            return Err(EXTERNAL_DOCUMENT_PATH_REQUIRED.to_string());
        }
    };

    if params.page.is_some() && params.page_range.is_some() {
        return Err("Pass either page or page_range, not both.".to_string());
    }
    if !mcp.is_path_allowed(&path, &scope).await {
        return Err(mcp_access_error(&path, &scope));
    }
    get_document_text_at_path(path, default_page, params)
}

pub(super) async fn get_document_outline_for_mcp(
    mcp: &WilkesMcp,
    params: GetDocumentOutlineParams,
) -> Result<GetDocumentOutlineResponse, String> {
    let scope = mcp.scope(params.workspace.as_deref()).await?;
    let path = resolve_document_path(mcp, params.path, &scope).await?;
    let outline_path = path.clone();
    let declared_outline = tokio::task::spawn_blocking(move || {
        let registry = wilkes_core::extract::production_registry();
        wilkes_core::extract::document_outline(&outline_path, &registry)
    })
    .await
    .map_err(|error| format!("Document outline task panicked: {error}"))?
    .map_err(|error| format!("Failed to read outline from {}: {error:#}", path.display()))?;

    Ok(GetDocumentOutlineResponse {
        path: display_path(&path),
        outline: declared_outline.entries,
        extraction: declared_outline.diagnostics,
    })
}

pub(super) fn get_document_text_at_path(
    path: PathBuf,
    default_page: Option<u32>,
    params: GetDocumentTextParams,
) -> Result<GetDocumentTextResponse, String> {
    let started_at = Instant::now();
    let page_range = match (params.page, params.page_range.as_deref()) {
        (Some(page), None) => Some((page, page)),
        (None, Some(range)) => Some(parse_page_range(range)?),
        (None, None) => default_page.map(|page| (page, page)),
        (Some(_), Some(_)) => return Err("Pass either page or page_range, not both.".to_string()),
    };
    let read_started_at = Instant::now();
    let text = reader::read_text_range(&path, page_range, None, None).map_err(|err| {
        info!(
            path = %path.display(),
            page_range = ?page_range,
            error = %err,
            elapsed_ms = started_at.elapsed().as_millis(),
            "chat: get_document_text failed"
        );
        format!("Failed to extract text from {}: {err:#}", path.display())
    })?;
    let read_elapsed_ms = read_started_at.elapsed().as_millis();
    let max_chars = params
        .max_chars
        .unwrap_or(DEFAULT_TEXT_CHAR_LIMIT)
        .min(MAX_TEXT_CHAR_LIMIT);
    let excerpt = reader::limit_excerpt(&text, max_chars);
    let response = GetDocumentTextResponse {
        path: display_path(&path),
        page: page_range.and_then(|(start, end)| (start == end).then_some(start)),
        page_range: page_range.and_then(|(start, end)| {
            (start != end).then_some(PageRange {
                start: start.min(end),
                end: start.max(end),
            })
        }),
        text: excerpt.text,
        truncated: excerpt.truncated,
    };
    let serialized_bytes = serde_json::to_vec(&response).map(|bytes| bytes.len()).ok();
    info!(
        path = %path.display(),
        page_range = ?page_range,
        read_elapsed_ms,
        elapsed_ms = started_at.elapsed().as_millis(),
        extracted_bytes = text.len(),
        response_text_bytes = response.text.len(),
        response_serialized_bytes = ?serialized_bytes,
        truncated = response.truncated,
        "chat: get_document_text completed"
    );
    Ok(response)
}

pub(super) fn parse_page_range(value: &str) -> Result<(u32, u32), String> {
    let invalid = || {
        format!(
            "Invalid page_range {value:?}. Use \"N-M\" with positive 1-based page numbers, for example \"1-2\"."
        )
    };
    let (start, end) = value.trim().split_once('-').ok_or_else(&invalid)?;
    if end.contains('-') {
        return Err(invalid());
    }
    let start = start.trim().parse::<u32>().map_err(|_| invalid())?;
    let end = end.trim().parse::<u32>().map_err(|_| invalid())?;
    if start == 0 || end == 0 {
        return Err(invalid());
    }
    Ok((start.min(end), start.max(end)))
}

pub(super) async fn search_documents_for_mcp(
    mcp: &WilkesMcp,
    mut params: SearchParams,
) -> Result<SearchResponse, String> {
    let scope = mcp.scope(params.workspace.as_deref()).await?;
    match &mcp.context {
        McpContext::Session(context) if scope.named().is_none() => {
            search_documents(context, scope.search(), &mcp.cwd, params).await
        }
        // Naming a workspace makes the call library-scoped even inside a chat:
        // the chat's own root and context files belong to another library.
        _ => {
            let root = match params.root.as_ref() {
                Some(root) if !root.trim().is_empty() => PathBuf::from(root),
                Some(_) => return Err("Search root cannot be empty.".to_string()),
                None => mcp.current_root(&scope).await?,
            };
            if !is_within_roots(&root, &mcp.library_roots(&scope).await) {
                return Err(mcp_access_error(&root, &scope));
            }
            if let Some(file) = params.file.as_ref() {
                let file = PathBuf::from(file);
                if !mcp.is_path_allowed(&file, &scope).await {
                    return Err(mcp_access_error(&file, &scope));
                }
            }
            params.root = Some(root.to_string_lossy().into_owned());
            search_documents(
                &ContextStateHandle::default(),
                scope.search(),
                Path::new(""),
                params,
            )
            .await
        }
    }
}

pub(super) async fn search_documents(
    context: &ContextStateHandle,
    search: Option<Arc<dyn SearchService>>,
    cwd: &Path,
    mut params: SearchParams,
) -> Result<SearchResponse, String> {
    let search =
        search.ok_or_else(|| "Wilkes search is not available in this session.".to_string())?;
    let mode = params.mode;
    let root = match params.root.take() {
        Some(root) => PathBuf::from(root),
        None => match context.search_root() {
            Some(root) => root,
            None => search
                .clone()
                .default_root()
                .await
                .unwrap_or_else(|| cwd.to_path_buf()),
        },
    };
    let max_file_size = search.clone().max_search_file_size().await;
    let (query, max_files) = build_search_query(root, params, max_file_size)?;
    let root = display_path(&query.root);
    let file = match &query.scope {
        wilkes_core::types::SearchScope::Corpus | wilkes_core::types::SearchScope::All => None,
        wilkes_core::types::SearchScope::File { path } => Some(display_path(path)),
    };
    let query_text = query.pattern.clone();
    let mut collected = search
        .clone()
        .search(query, max_files)
        .await
        // Only a semantic-only search fails for want of an index. Combined
        // reduces instead, and says so through its notes below.
        .map_err(|message| match mode {
            SearchModeParam::Semantic => with_semantic_index_guidance(message),
            SearchModeParam::Exact | SearchModeParam::Combined => message,
        })?;
    // An unavailable semantic lane is the one thing in here that is not a
    // fault: the combined search reduced to exact matching and found what it
    // found. Reported as an error it reads as a failed call, and a client that
    // reads it that way retries a search that will reduce the same way again.
    let mut notes = Vec::new();
    if mode == SearchModeParam::Combined {
        let (reductions, errors) = collected
            .stats
            .errors
            .into_iter()
            .partition::<Vec<_>, _>(|error| reports_unavailable_index(error));
        notes.extend(
            reductions
                .into_iter()
                .map(|reduction| format!("{reduction} {COMBINED_INDEX_GUIDANCE}")),
        );
        collected.stats.errors = errors;
    }
    if !collected.complete {
        notes.push(format!(
            "The scan stopped after {}s without reaching the end of the search root, and these are the matches found by then. Narrow the search with file or collection_id, or ask the user to build the semantic index, which reads each document once instead of per search.",
            crate::search::AGENT_SEARCH_BUDGET.as_secs()
        ));
    }

    let mut matches = Vec::with_capacity(collected.files.len());
    for file in collected.files {
        let path = file.path.clone();
        let mut response = SearchFileResponse::from(file);
        // Best-effort bibliographic enrichment from the same cache the file list
        // uses. Enrichment must never fail the search itself, but a real error
        // (cache lock, access boundary) is an anomaly worth surfacing rather
        // than silently dropping — so leave the fields null and log it.
        match search.clone().document_metadata(path.clone()).await {
            Ok(metadata) => {
                response.title = metadata.title.or(response.title);
                response.author = metadata.author;
                response.doi = metadata.doi;
            }
            Err(error) => {
                info!(path = %path.display(), %error, "search: metadata enrichment skipped");
            }
        }
        matches.push(response);
    }

    Ok(SearchResponse {
        query: query_text,
        mode,
        root,
        file,
        matches,
        stats: collected.stats,
        truncated: collected.truncated,
        complete: collected.complete,
        notes,
    })
}

/// Resolve the document path for a single-document tool: an explicit `path`
/// when given, otherwise the session's active document. Enforces the same
/// read-access boundary every other document tool uses.
pub(super) async fn resolve_document_path(
    mcp: &WilkesMcp,
    path: Option<String>,
    scope: &WorkspaceScope,
) -> Result<PathBuf, String> {
    let explicit = path
        .as_ref()
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from);
    let path = match (&mcp.context, explicit) {
        (_, Some(path)) => path,
        // The active document belongs to the workspace Wilkes is showing, so a
        // call that names another workspace cannot inherit it.
        (McpContext::Session(_), None) if scope.named().is_some() => {
            return Err(WORKSPACE_DOCUMENT_PATH_REQUIRED.to_string());
        }
        (McpContext::Session(context), None) => context
            .snapshot()
            .active_doc
            .map(|document| PathBuf::from(document.path))
            .ok_or_else(|| "No active document is available; pass path explicitly.".to_string())?,
        (McpContext::Library(_), None) => {
            return Err(EXTERNAL_DOCUMENT_PATH_REQUIRED.to_string());
        }
    };
    if !mcp.is_path_allowed(&path, scope).await {
        return Err(mcp_access_error(&path, scope));
    }
    Ok(path)
}

pub(super) async fn get_file_metadata_for_mcp(
    mcp: &WilkesMcp,
    params: GetFileMetadataParams,
) -> Result<GetFileMetadataResponse, String> {
    let scope = mcp.scope(params.workspace.as_deref()).await?;
    let path = resolve_document_path(mcp, params.path, &scope).await?;
    let search =
        scope.require_search("Wilkes document metadata is not available in this session.")?;
    let metadata = search.document_metadata(path.clone()).await?;
    Ok(GetFileMetadataResponse {
        path: display_path(&path),
        metadata,
    })
}

pub(super) async fn list_documents_for_mcp(
    mcp: &WilkesMcp,
    params: ListDocumentsParams,
) -> Result<ListDocumentsResponse, String> {
    let scope = mcp.scope(params.workspace.as_deref()).await?;
    let search =
        scope.require_search("Wilkes document listing is not available in this session.")?;
    let all = params.scope == Some(SearchScopeParam::All);
    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_DOCUMENTS_LIMIT)
        .clamp(1, MAX_LIST_DOCUMENTS_LIMIT);

    let roots: Vec<PathBuf> = if all {
        let roots = mcp.library_roots(&scope).await;
        if roots.is_empty() {
            return Err(no_roots_error(&scope));
        }
        roots
    } else {
        let root = match params.root.as_ref() {
            Some(root) if !root.trim().is_empty() => PathBuf::from(root),
            Some(_) => return Err("List-documents root cannot be empty.".to_string()),
            None => mcp.current_root(&scope).await?,
        };
        // The external, library-scoped server must not list outside configured
        // roots, and neither may a call that reaches past its own workspace; a
        // session server already trusts its own root.
        if (matches!(mcp.context, McpContext::Library(_)) || scope.named().is_some())
            && !is_within_roots(&root, &mcp.library_roots(&scope).await)
        {
            return Err(mcp_access_error(&root, &scope));
        }
        vec![root]
    };

    let display_root = if all {
        "all".to_string()
    } else {
        display_path(&roots[0])
    };

    let mut documents = Vec::new();
    let mut truncated = false;
    'outer: for root in roots {
        let listed = search.clone().list_documents(root).await?;
        for entry in listed.files {
            if documents.len() >= limit {
                truncated = true;
                break 'outer;
            }
            documents.push(DocumentSummaryResponse::from(entry));
        }
    }

    Ok(ListDocumentsResponse {
        root: display_root,
        scope: if all {
            SearchScopeParamResponse::All
        } else {
            SearchScopeParamResponse::CurrentRoot
        },
        documents,
        truncated,
    })
}

pub(super) async fn get_citation_graph_for_mcp(
    mcp: &WilkesMcp,
    params: GetCitationGraphParams,
) -> Result<GetCitationGraphResponse, String> {
    let limit = params.limit.unwrap_or(25);
    if !(1..=200).contains(&limit) {
        return Err("limit must be 1..200".into());
    }
    let scope = mcp.scope(params.workspace.as_deref()).await?;
    let search = scope.require_search("Wilkes citation graphs are not available.")?;
    let root = match params.scope {
        Some(SearchScopeParam::All) => None,
        None => {
            let root = match params.root.as_ref() {
                Some(root) if !root.trim().is_empty() => PathBuf::from(root),
                Some(_) => return Err("Citation graph root cannot be empty.".to_string()),
                None => mcp.current_root(&scope).await?,
            };
            if !is_within_roots(&root, &mcp.library_roots(&scope).await) {
                return Err(mcp_access_error(&root, &scope));
            }
            Some(root)
        }
    };
    let graph = search
        .citation_graph(wilkes_core::types::CitationGraphQuery {
            root: root.clone(),
            min_external_citations: params.min_external_citations.unwrap_or(2),
        })
        .await?;

    let mut cites = vec![0usize; graph.documents.len()];
    for edge in &graph.edges {
        cites[edge.source] += 1;
    }
    let mut order: Vec<usize> = (0..graph.documents.len()).collect();
    order.sort_by(|&a, &b| {
        graph.documents[b]
            .cited_by
            .len()
            .cmp(&graph.documents[a].cited_by.len())
            .then_with(|| graph.documents[a].doi.cmp(&graph.documents[b].doi))
    });
    let mut position = vec![0usize; graph.documents.len()];
    for (new, &old) in order.iter().enumerate() {
        position[old] = new;
    }
    let documents = order
        .iter()
        .map(|&index| {
            let document = &graph.documents[index];
            let first = &document.entries[0];
            CitationGraphDocumentSummary {
                doi: document.doi.clone(),
                title: first.title.clone(),
                publication_date: document
                    .entries
                    .iter()
                    .find_map(|entry| entry.publication_date.clone()),
                paths: document
                    .entries
                    .iter()
                    .map(|entry| entry.path.to_string_lossy().into_owned())
                    .collect(),
                cited_by_in_scope: document.cited_by.len(),
                cites_in_scope: cites[index],
                references_fetched: document.references_fetched,
            }
        })
        .collect();
    let edges = graph
        .edges
        .iter()
        .map(|edge| [position[edge.source], position[edge.target]])
        .collect();
    let external: Vec<ExternalCitedWorkSummary> = graph
        .external
        .into_iter()
        .take(limit)
        .map(|work| ExternalCitedWorkSummary {
            cited_by_in_scope: work.cited_by.len(),
            library_path: work
                .library_entry
                .map(|entry| entry.path.to_string_lossy().into_owned()),
            reference: work.reference,
        })
        .collect();
    Ok(GetCitationGraphResponse {
        scope: if root.is_some() {
            SearchScopeParamResponse::CurrentRoot
        } else {
            SearchScopeParamResponse::All
        },
        root: root.map(|root| root.to_string_lossy().into_owned()),
        coverage: graph.coverage,
        documents,
        edges,
        external_returned: external.len(),
        external,
        external_total: graph.external_total,
        without_doi: graph.without_doi.len(),
    })
}

pub(super) async fn get_related_documents_for_mcp(
    mcp: &WilkesMcp,
    mut params: GetRelatedDocumentsParams,
) -> Result<GetRelatedDocumentsResponse, String> {
    let scope = mcp.scope(params.workspace.as_deref()).await?;
    let path = resolve_document_path(mcp, params.path.clone(), &scope).await?;
    params.path = Some(path.to_string_lossy().into_owned());

    match &mcp.context {
        McpContext::Session(context) if scope.named().is_none() => {
            get_related_documents(context, scope.search(), &mcp.cwd, params).await
        }
        _ => {
            let root = match params.root.as_ref() {
                Some(root) if !root.trim().is_empty() => PathBuf::from(root),
                Some(_) => return Err("Related-documents root cannot be empty.".to_string()),
                None => mcp.current_root(&scope).await?,
            };
            if !is_within_roots(&root, &mcp.library_roots(&scope).await) {
                return Err(mcp_access_error(&root, &scope));
            }
            params.root = Some(root.to_string_lossy().into_owned());
            get_related_documents(
                &ContextStateHandle::default(),
                scope.search(),
                Path::new(""),
                params,
            )
            .await
        }
    }
}

pub(super) async fn get_related_documents(
    context: &ContextStateHandle,
    search: Option<Arc<dyn SearchService>>,
    cwd: &Path,
    mut params: GetRelatedDocumentsParams,
) -> Result<GetRelatedDocumentsResponse, String> {
    let search = search.ok_or_else(|| {
        "Wilkes related-document search is not available in this session.".to_string()
    })?;
    let path = match params.path.take() {
        Some(path) if !path.trim().is_empty() => PathBuf::from(path),
        Some(_) => return Err("Document path cannot be empty.".to_string()),
        None => context
            .snapshot()
            .active_doc
            .map(|document| PathBuf::from(document.path))
            .ok_or_else(|| "No active document is available; pass path explicitly.".to_string())?,
    };
    let root = match params.root.take() {
        Some(root) if !root.trim().is_empty() => PathBuf::from(root),
        Some(_) => return Err("Related-documents root cannot be empty.".to_string()),
        None => match context.search_root() {
            Some(root) => root,
            None => search
                .clone()
                .default_root()
                .await
                .unwrap_or_else(|| cwd.to_path_buf()),
        },
    };
    let scope = if params.scope == Some(SearchScopeParam::All) {
        wilkes_core::types::SearchScope::All
    } else {
        wilkes_core::types::SearchScope::Corpus
    };
    let query = wilkes_core::types::RelatedDocumentsQuery {
        root: root.clone(),
        path: path.clone(),
        scope: scope.clone(),
        limit: Some(
            params
                .limit
                .unwrap_or(DEFAULT_RELATED_DOCUMENTS_LIMIT)
                .clamp(1, MAX_RELATED_DOCUMENTS_LIMIT),
        ),
        collection_id: params.collection_id,
    };
    let documents = search
        .related_documents(query)
        .await
        .map_err(with_semantic_index_guidance)?;

    Ok(GetRelatedDocumentsResponse {
        path: display_path(&path),
        root: display_path(&root),
        scope: if scope == wilkes_core::types::SearchScope::All {
            SearchScopeParamResponse::All
        } else {
            SearchScopeParamResponse::CurrentRoot
        },
        documents,
    })
}

/// Append the fall-back-to-exact guidance to a message that reports an
/// unavailable semantic index, and leave every other message alone. Only a
/// semantic-only search gets this sentence: the combined mode has already run
/// its exact lane, so telling it to run an exact search would be telling it to
/// repeat one it has the results of.
pub(super) fn with_semantic_index_guidance(message: String) -> String {
    if reports_unavailable_index(&message) {
        format!("{message} {SEMANTIC_INDEX_GUIDANCE}")
    } else {
        message
    }
}

/// Whether a message reports an unavailable semantic index rather than a
/// fault. One predicate, because the two callers — the semantic mode, which
/// fails, and the combined mode, which reduces — must agree on which messages
/// are about the index or the same message becomes an error in one and a note
/// in the other.
pub(super) fn reports_unavailable_index(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "no semantic index",
        "semantic index is not ready",
        "semantic index is currently being built",
        "semantic index has no searchable documents",
        "semantic index is not built",
        "semantic search requires a loaded embedder and built index",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(super) fn build_search_query(
    root: PathBuf,
    params: SearchParams,
    max_file_size: u64,
) -> Result<(wilkes_core::types::SearchQuery, usize), String> {
    let mode = params.mode;
    let pattern = params.query.trim().to_string();
    if pattern.is_empty() {
        return Err("Search query cannot be empty.".to_string());
    }

    let max_results = params
        .max_results
        .unwrap_or(DEFAULT_SEARCH_MAX_RESULTS)
        .clamp(1, MAX_SEARCH_MAX_RESULTS);
    let context_lines = params
        .context_lines
        .unwrap_or(DEFAULT_SEARCH_CONTEXT_LINES)
        .min(MAX_SEARCH_CONTEXT_LINES);
    Ok((
        wilkes_core::types::SearchQuery {
            pattern,
            is_regex: mode == SearchModeParam::Exact && params.is_regex.unwrap_or(false),
            case_sensitive: params.case_sensitive.unwrap_or(false),
            root,
            max_results,
            respect_gitignore: true,
            max_file_size,
            context_lines,
            mode: match mode {
                SearchModeParam::Exact => wilkes_core::types::SearchMode::Grep,
                SearchModeParam::Semantic => wilkes_core::types::SearchMode::Semantic,
                SearchModeParam::Combined => wilkes_core::types::SearchMode::Hybrid,
            },
            scope: if let Some(path) = params.file {
                wilkes_core::types::SearchScope::File {
                    path: PathBuf::from(path),
                }
            } else if params.scope == Some(SearchScopeParam::All) {
                wilkes_core::types::SearchScope::All
            } else {
                wilkes_core::types::SearchScope::Corpus
            },
            supported_extensions: Vec::new(),
            collection_id: params.collection_id,
            tag_ids: Vec::new(),
        },
        max_results,
    ))
}

pub(super) fn structured(value: impl Serialize) -> CallToolResult {
    match serde_json::to_value(value) {
        Ok(value) => match serde_yaml_ng::to_string(&value) {
            Ok(yaml) => {
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(yaml)];
                result
            }
            Err(err) => CallToolResult::error(vec![ContentBlock::text(format!(
                "Failed to format Wilkes MCP response: {err}"
            ))]),
        },
        Err(err) => CallToolResult::error(vec![ContentBlock::text(format!(
            "Failed to serialize Wilkes MCP response: {err}"
        ))]),
    }
}

pub(super) fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
