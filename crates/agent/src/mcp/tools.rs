//! The tools themselves, and the downloads they run.

use super::*;

#[tool_router(vis = "pub(super)")]
impl WilkesMcp {
    #[tool(
        description = "Read bookmarks (quotes and notes), saved_works (literature results kept for later, each with where it landed if added to the library), tags, or recent search_history from a workspace. Offset is zero-based and limit is 1..500 (default 50). Search history covers the most recent 1000 entries. Use list_smart_collections for collections.",
        annotations(read_only_hint = true)
    )]
    pub(super) async fn read_library(
        &self,
        Parameters(params): Parameters<ReadLibraryParams>,
    ) -> CallToolResult {
        let result: Result<serde_json::Value,String> = async {
        let limit=params.limit.unwrap_or(50);
        if !(1..=500).contains(&limit) { return Err("limit must be 1..500".into()); }
        let scope=self.scope(params.workspace.as_deref()).await?;
        let value=scope.require_search("No research library is available")?.read_library(params.kind).await?;
        let entries=value.as_array().ok_or("Research library returned an invalid list")?;
        let items:Vec<_>=entries.iter().skip(params.offset).take(limit).collect();
        let next=params.offset.saturating_add(items.len());
        Ok(serde_json::json!({"items":items,"total":entries.len(),"next_offset":if next<entries.len(){Some(next)}else{None}}))
    }.await;
        match result {
            Ok(value) => structured(value),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Edit the user's Wilkes research library: add/update/remove bookmarks; update the note of or remove saved works; create/update/delete tags and smart collections; tag documents; rename a file while preserving its index and research links; refresh document metadata. Use only for user-requested changes. Paths must resolve inside the named workspace's library roots. Managed read_only workspaces refuse every edit. Existing files are never overwritten. Bookmark page/line numbers are 1-based. Collection expressions use Wilkes's existing CEL filter schema."
    )]
    pub(super) async fn edit_library(
        &self,
        Parameters(mut params): Parameters<EditLibraryParams>,
    ) -> CallToolResult {
        let result:Result<serde_json::Value,String> = async {
        let scope=self.scope(params.workspace.as_deref()).await?;
        if scope.is_read_only() {return Err("MANAGED_WORKSPACE_PROTECTED: this workspace is owned by another application and can only be read".into());}
        if let crate::library::LibraryEdit::RenameFile {path,..} = &params.edit {
            if !path.is_file() { return Err("rename_file requires a document file, not a library root or directory".into()); }
        }
        let roots=self.library_roots(&scope).await;
        for path in params.edit.paths_mut() {
            let canonical=path.canonicalize().map_err(|e|format!("{}: {e}",path.display()))?;
            if !is_within_roots(&canonical,&roots) {return Err(format!("{} is outside this workspace's library roots",path.display()));}
            *path=canonical;
        }
        scope.require_search("No research library is available")?.edit_library(params.edit).await
    }.await;
        match result {
            Ok(value) => structured(value),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "List the current Wilkes context: every workspace and its id, the configured library roots and active root of the workspace read, the document currently visible in Wilkes, and any files explicitly added to an in-app chat. Read this first to learn the workspace ids other tools accept.",
        annotations(read_only_hint = true)
    )]
    pub(super) async fn list_context(
        &self,
        Parameters(params): Parameters<WorkspaceParams>,
    ) -> CallToolResult {
        let scope = match self.scope(params.workspace.as_deref()).await {
            Ok(scope) => scope,
            Err(message) => return CallToolResult::error(vec![ContentBlock::text(message)]),
        };
        let workspaces = match self.workspace_descriptors().await {
            Ok(workspaces) => workspaces,
            Err(message) => return CallToolResult::error(vec![ContentBlock::text(message)]),
        };
        let snapshot = self.context_snapshot(&scope).await;
        let roots = self.library_roots(&scope).await;
        let literature_providers = self.literature_provider_ids(&scope).await;
        structured(ListContextResponse::from_snapshot(
            snapshot.root,
            snapshot.active_doc,
            snapshot.context_files,
            roots,
            workspaces,
            scope.named(),
            literature_providers,
        ))
    }

    #[tool(
        description = "List saved Wilkes smart collections and their IDs for collection-scoped searches. Collections are per workspace; omit workspace for the active one.",
        annotations(read_only_hint = true)
    )]
    pub(super) async fn list_smart_collections(
        &self,
        Parameters(params): Parameters<WorkspaceParams>,
    ) -> CallToolResult {
        let scope = match self.scope(params.workspace.as_deref()).await {
            Ok(scope) => scope,
            Err(message) => return CallToolResult::error(vec![ContentBlock::text(message)]),
        };
        match scope.search() {
            Some(search) => match search.list_smart_collections().await {
                Ok(collections) => structured(collections),
                Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
            },
            None => structured(Vec::<wilkes_core::types::SmartCollection>::new()),
        }
    }

    #[tool(
        description = "Read Wilkes-extracted document text from any configured Wilkes library root. Reads the active workspace unless workspace names another. External MCP clients must pass path; an in-app chat may omit it to use that chat's active document. Use page for one PDF page or page_range in \"N-M\" format (for example, \"7-9\") for an inclusive PDF page range.",
        annotations(read_only_hint = true)
    )]
    pub(super) async fn get_document_text(
        &self,
        Parameters(params): Parameters<GetDocumentTextParams>,
    ) -> CallToolResult {
        match get_document_text_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Read a document's declared outline (PDF bookmarks or Markdown headings) without requiring the semantic index. Reads the active workspace unless workspace names another. External MCP clients must pass path; an in-app chat may omit it to use that chat's active document. An empty outline means the document declares no table of contents.",
        annotations(read_only_hint = true)
    )]
    pub(super) async fn get_document_outline(
        &self,
        Parameters(params): Parameters<GetDocumentOutlineParams>,
    ) -> CallToolResult {
        match get_document_outline_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Find documents semantically related to a document in Wilkes. Searches the active workspace unless workspace names another. External MCP clients must pass path; an in-app chat may omit it to use that chat's active document. Set scope='all' to search that workspace's whole library; otherwise results are limited to its current root.",
        annotations(read_only_hint = true)
    )]
    pub(super) async fn get_related_documents(
        &self,
        Parameters(params): Parameters<GetRelatedDocumentsParams>,
    ) -> CallToolResult {
        match get_related_documents_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Read the citation graph of the documents in Wilkes: which documents cite which, by DOI, and which works outside the scope its documents cite most (with title, authors, year and venue), for finding key papers and gaps in a library. Reads the current root of the active workspace unless workspace names another; scope='all' reads that workspace's whole library. coverage says how many documents have a DOI and how many have had their references fetched: a document whose references_fetched is false has unknown, not zero, outgoing citations.",
        annotations(read_only_hint = true)
    )]
    pub(super) async fn get_citation_graph(
        &self,
        Parameters(params): Parameters<GetCitationGraphParams>,
    ) -> CallToolResult {
        match get_citation_graph_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Read full bibliographic metadata for a document in any configured Wilkes library root: title, author, DOI, publication date, and any enrichment from an enabled literature provider (under `providers`, keyed by provider id). Reads the active workspace unless workspace names another. External MCP clients must pass path; an in-app chat may omit it to use that chat's active document.",
        annotations(read_only_hint = true)
    )]
    pub(super) async fn get_file_metadata(
        &self,
        Parameters(params): Parameters<GetFileMetadataParams>,
    ) -> CallToolResult {
        match get_file_metadata_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "List documents in the Wilkes library with their title, author, and DOI. Lists the active workspace unless workspace names another. Set scope='all' to list that workspace's whole library; otherwise lists its current root. Use this to browse what documents exist rather than searching their contents.",
        annotations(read_only_hint = true)
    )]
    pub(super) async fn list_documents(
        &self,
        Parameters(params): Parameters<ListDocumentsParams>,
    ) -> CallToolResult {
        match list_documents_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Search Wilkes-readable documents, including direct filename, cached-title and cached-author matches. mode defaults to 'combined', which finds the query text as written and passages about the same subject in different words; each returned document carries evidence=['exact_phrase'], ['related_passage'], or both, so you can tell which. Set mode='exact' for literal/regex matching only (is_regex applies to this mode alone), or mode='semantic' for meaning-based content search only; both narrow what combined already covers, so prefer the default unless the user asked for one of them. Each returned match has kind='content', 'filename', 'title', or 'author'. Searches the active workspace unless workspace names another. Set scope='all' to search every configured library root of that workspace; omit scope to search its current root. If the user asks about a specific document, set file to that document path; omit file only for corpus-wide searches. A search answers within a time budget: complete=false means the scan did not reach the end of the search root and the matches are those found so far. Anything the search could not do, such as a combined search whose semantic lane was unavailable, is reported in notes; stats.errors carries only faults.",
        annotations(read_only_hint = true)
    )]
    pub(super) async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> CallToolResult {
        match search_documents_for_mcp(self, params).await {
            Ok(response) => structured(response),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Search scholarly literature, or an open learning catalogue of textbooks, courses or documentation, using an enabled provider. Pass provider as an id from list_context's literature_providers, for example 'openalex', 'semantic_scholar', a catalogue such as 'libretexts', or a user-defined 'custom:<id>'.",
        annotations(read_only_hint = true)
    )]
    pub(super) async fn literature_search(
        &self,
        Parameters(params): Parameters<LiteratureSearchParams>,
    ) -> CallToolResult {
        let query = params.query.trim().to_string();
        if query.is_empty() {
            return CallToolResult::error(vec![ContentBlock::text(
                "Literature search query cannot be empty.",
            )]);
        }
        let limit = params
            .limit
            .unwrap_or(DEFAULT_SEARCH_MAX_RESULTS)
            .clamp(1, 100);

        // One lookup replaces what was an arm per provider. Whether the id
        // names a compiled-in client or a manifest the user wrote is not
        // something this tool can see, or needs to.
        let registry = self.literature_registry().await;
        let source = match registry.literature_for_search(&params.provider) {
            Ok(source) => Arc::clone(source),
            Err(error) => {
                return CallToolResult::error(vec![ContentBlock::text(error.to_string())])
            }
        };
        match source.search(&query, limit, 1).await {
            Ok(results) => {
                let view = results.iter().map(LiteratureResultView::from).collect();
                {
                    let mut cache = self.literature_results.lock().unwrap();
                    if cache.len() + results.len() > LITERATURE_CACHE_CAPACITY {
                        cache.clear();
                    }
                    for result in results {
                        cache.insert((params.provider.clone(), result.id.clone()), result);
                    }
                }
                structured(LiteratureSearchResponse {
                    query,
                    provider: params.provider,
                    results: view,
                })
            }
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
        }
    }

    #[tool(
        description = "Download files into the current Wilkes root of the active workspace, or of the workspace named by workspace. Use only when the user asks to import or download files. Pass every file in one call, as the list downloads: they are fetched at the same time, so one call for several files is much faster than one call each. Name each file either by a url the user gave you, or by provider and result_id of a literature_search result whose downloadable is true; Wilkes finds the file itself. The answer lists, in the order asked, each download's files or its error; one failing does not stop the others. A result may come back as several files, in a folder of their own. Pass filename to choose the saved name, or the folder's when there are several; existing files are never overwritten, and a name already taken by a different file gets a number, as in 'paper (2).pdf'. A url the site refuses with HTTP 401 or 403 is retried once, automatically, through the agent browser, which is signed in wherever the user signed in in Wilkes' browser; pass through_browser=true to go there directly. The agent browser fetches one file at a time. If the browser gets a sign-in or check page instead of the file, ask the user to open Agent browser from the browser button's menu in Wilkes, deal with it there, and try again. Refused for a workspace list_context reports as read_only, which another application owns.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    pub(super) async fn download(
        &self,
        Parameters(params): Parameters<DownloadParams>,
    ) -> CallToolResult {
        if params.downloads.is_empty() || params.downloads.len() > MAX_DOWNLOADS_PER_CALL {
            return CallToolResult::error(vec![ContentBlock::text(format!(
                "downloads takes 1 to {MAX_DOWNLOADS_PER_CALL} files; {} were given.",
                params.downloads.len()
            ))]);
        }
        let scope = match self.scope(params.workspace.as_deref()).await {
            Ok(scope) => scope,
            Err(message) => return CallToolResult::error(vec![ContentBlock::text(message)]),
        };
        if scope.is_read_only() {
            return CallToolResult::error(vec![ContentBlock::text(
                "MANAGED_WORKSPACE_PROTECTED: this workspace is owned by another application and can only be read",
            )]);
        }
        let root = match self.current_root(&scope).await {
            Ok(root) => root,
            Err(message) => {
                return CallToolResult::error(vec![ContentBlock::text(message)]);
            }
        };
        let max_download_bytes = match scope.search() {
            Some(search) => search.max_download_bytes().await,
            // A standalone chat session has no application settings service.
            None => wilkes_core::types::DEFAULT_MAX_DOWNLOAD_BYTES,
        };
        // In the order asked, whichever finishes first. Downloads through the
        // agent browser still take their turn there, one at a time.
        let outcomes: Vec<DownloadOutcome> = stream::iter(params.downloads)
            .map(|item| async {
                let result = match self
                    .download_one(&scope, &root, max_download_bytes, &item)
                    .await
                {
                    Ok(acquired) => DownloadResult::Acquired(acquired),
                    Err(error) => DownloadResult::Failed { error },
                };
                DownloadOutcome {
                    url: item.url,
                    provider: item.provider,
                    result_id: item.result_id,
                    result,
                }
            })
            .buffered(CONCURRENT_DOWNLOADS)
            .collect()
            .await;
        let errors: Option<Vec<String>> = outcomes
            .iter()
            .map(|outcome| match outcomes.len() {
                1 => outcome.error().map(str::to_string),
                _ => outcome
                    .error()
                    .map(|error| format!("{}: {error}", outcome.asked_for())),
            })
            .collect();
        if let Some(errors) = errors {
            // Nothing reached the library: an error, so a client that only
            // looks at that flag still sees it, with every reason.
            return CallToolResult::error(vec![ContentBlock::text(errors.join("\n"))]);
        }
        structured(DownloadResponse {
            downloads: outcomes,
        })
    }

    #[tool(
        description = "Open a web page in the agent browser and return its text. The agent browser is Wilkes' own, signed in wherever the user signed in in Wilkes' browser, so it reads pages a plain fetch cannot, such as articles behind the user's library sign-in. The user can watch it by opening Agent browser from the browser button's menu in Wilkes. If the page asks to sign in or to pass a check, ask the user to open Agent browser there, deal with it, and try again. For a file such as a PDF, use download with through_browser=true instead."
    )]
    pub(super) async fn read_web_page(
        &self,
        Parameters(params): Parameters<ReadWebPageParams>,
    ) -> CallToolResult {
        let browser = match self.require_browser() {
            Ok(browser) => browser,
            Err(message) => return CallToolResult::error(vec![ContentBlock::text(message)]),
        };
        match browser.read(&params.url).await {
            Ok(page) => {
                let max_chars = params
                    .max_chars
                    .unwrap_or(DEFAULT_TEXT_CHAR_LIMIT)
                    .min(MAX_TEXT_CHAR_LIMIT);
                let excerpt = reader::limit_excerpt(&page.text, max_chars);
                structured(ReadWebPageResponse {
                    url: page.url,
                    title: page.title,
                    text: excerpt.text,
                    truncated: excerpt.truncated,
                })
            }
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Save a literature_search result to the user's saved works in Wilkes, a list of works to read or add later. Name the result by provider and result_id from literature_search; note is optional. Saving a work already saved, or the same DOI from another provider, returns the one held. Use only when the user asks to save or remember a result. Remove or annotate saved works with edit_library, list them with read_library kind saved_works. Refused for a workspace list_context reports as read_only."
    )]
    pub(super) async fn save_literature_result(
        &self,
        Parameters(params): Parameters<SaveLiteratureResultParams>,
    ) -> CallToolResult {
        let result: Result<serde_json::Value, String> = async {
            let scope = self.scope(params.workspace.as_deref()).await?;
            if scope.is_read_only() {
                return Err("MANAGED_WORKSPACE_PROTECTED: this workspace is owned by another application and can only be read".into());
            }
            let work = self.cached_literature_result(&params.provider, &params.result_id)?;
            scope
                .require_search("No research library is available")?
                .save_literature_result(params.provider, work, params.note)
                .await
        }
        .await;
        match result {
            Ok(value) => structured(value),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }
    #[tool(
        description = "List the custom literature integrations the user stored, each a manifest describing a web service Wilkes can search and fetch from: id, provider id for literature_search, name and enabled. Neither manifest text nor secrets are returned; to change an integration, the user gives you its manifest. Set authoring_guide=true to get the manifest format's guide, which you need before writing or editing a manifest.",
        annotations(read_only_hint = true)
    )]
    pub(super) async fn read_custom_integrations(
        &self,
        Parameters(params): Parameters<ReadCustomIntegrationsParams>,
    ) -> CallToolResult {
        let result: Result<serde_json::Value, String> = async {
            let integrations = self
                .integrations_service()
                .await?
                .custom_integrations()
                .await?;
            let mut response = serde_json::json!({ "integrations": integrations });
            if params.authoring_guide.unwrap_or(false) {
                response["authoring_guide"] = serde_json::Value::String(
                    wilkes_core::integrations::custom::manifest::authoring_prompt(),
                );
            }
            Ok(response)
        }
        .await;
        match result {
            Ok(value) => structured(value),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Run a draft custom integration manifest's search once, without saving it, and report the request, the start of the raw response, the results it made of it and any field that did not project. Use it to check a manifest before save_custom_integration. A draft whose id is a stored integration runs with that integration's stored secrets only if it contacts no origin and runs no program the stored manifest does not; otherwise it runs without secrets (stored_secrets=false)."
    )]
    pub(super) async fn probe_custom_integration(
        &self,
        Parameters(params): Parameters<ProbeCustomIntegrationParams>,
    ) -> CallToolResult {
        let result = async {
            self.integrations_service()
                .await?
                .probe_custom_integration(params.manifest, params.query)
                .await
        }
        .await;
        match result {
            Ok(probe) => structured(ProbeCustomIntegrationResponse {
                ok: probe.report.ok,
                error: probe.report.error,
                stored_secrets: probe.stored_secrets,
                request_url: probe.report.request_url,
                raw_response: probe.report.raw_response,
                issues: probe.report.issues,
                results: probe
                    .report
                    .results
                    .iter()
                    .map(LiteratureResultView::from)
                    .collect(),
            }),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }

    #[tool(
        description = "Add a custom literature integration, or replace the manifest of the one with the same id. Use only when the user asks to add or change an integration. Pass the whole manifest; an invalid one is refused with every problem. A replaced integration keeps the user's name for it, and keeps its stored secrets only if the new manifest contacts no origin and runs no program the old one did not. A new integration is off unless enabled=true. Saving never allows a program to run and never sets a secret: the answer lists missing_secrets and programs not yet allowed, which the user sets in Settings → Integrations."
    )]
    pub(super) async fn save_custom_integration(
        &self,
        Parameters(params): Parameters<SaveCustomIntegrationParams>,
    ) -> CallToolResult {
        let result = async {
            self.integrations_service()
                .await?
                .save_custom_integration(params.manifest, params.enabled)
                .await
        }
        .await;
        match result {
            Ok(value) => structured(value),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }
}

impl WilkesMcp {
    /// Put the one file, or the files of the one result, `item` names into
    /// `root`. A saved work is marked as in the library once its files are.
    pub(super) async fn download_one(
        &self,
        scope: &WorkspaceScope,
        root: &Path,
        max_download_bytes: u64,
        item: &DownloadItem,
    ) -> Result<AcquiredFiles, String> {
        // No progress sink: an MCP call is one request and one reply, with
        // nowhere to render a bar between them.
        let through_browser = item.through_browser.unwrap_or(false);
        let one = |file| AcquiredFiles {
            folder: None,
            files: vec![file],
        };
        let filename = item.filename.clone();
        match (&item.url, &item.provider, &item.result_id) {
            (Some(url), None, None) if through_browser => self
                .download_through_browser(url, root, filename, max_download_bytes)
                .await
                .map(one),
            (Some(url), None, None) => self
                .download_url(url, root, filename, max_download_bytes)
                .await
                .map(one),
            (None, Some(_), Some(_)) if through_browser => {
                Err("through_browser takes a url, not a literature result.".to_string())
            }
            (None, Some(provider), Some(result_id)) => {
                let result = self.cached_literature_result(provider, result_id)?;
                let acquired = acquire_result(
                    &self.integrations().await,
                    self.literature_mirror().await.as_deref(),
                    provider,
                    &result,
                    root,
                    filename,
                    None,
                    max_download_bytes,
                )
                .await?;
                if let (Some(search), Some(first)) = (scope.search(), acquired.files.first()) {
                    // The files are in the library whether or not the mark is
                    // written, so a failure here is reported, not returned.
                    if let Err(message) = search
                        .mark_literature_acquired(
                            provider.clone(),
                            result,
                            PathBuf::from(&first.path),
                        )
                        .await
                    {
                        error!("marking a saved work as acquired failed: {message}");
                    }
                }
                Ok(acquired)
            }
            _ => Err(
                "Pass either url, or provider together with result_id, but not both.".to_string(),
            ),
        }
    }

    /// Fetch `url` into `root`. A site that refuses this client access (HTTP
    /// 401 or 403) is asked once more through the agent browser, where there
    /// is one: such a site often serves the same address to the browser the
    /// user is signed in with.
    pub(super) async fn download_url(
        &self,
        url: &str,
        root: &Path,
        filename: Option<String>,
        max_download_bytes: u64,
    ) -> Result<wilkes_core::acquire::DownloadResponse, String> {
        let direct = download_to_root(
            root,
            wilkes_core::acquire::DownloadParams {
                url: url.to_string(),
                filename: filename.clone(),
            },
            None,
            max_download_bytes,
        )
        .await;
        match direct {
            Err(refused) if refused.access_refused() && self.browser.is_some() => {
                info!(
                    status = ?refused.status,
                    "download refused access; retrying once through the agent browser"
                );
                self.download_through_browser(url, root, filename, max_download_bytes)
                    .await
                    .map_err(|error| {
                        format!("{refused} Retried through the agent browser: {error}")
                    })
            }
            other => other.map_err(String::from),
        }
    }

    /// Download `url` in the agent browser and store it in `root` by the rules
    /// every download keeps.
    pub(super) async fn download_through_browser(
        &self,
        url: &str,
        root: &Path,
        filename: Option<String>,
        max_download_bytes: u64,
    ) -> Result<wilkes_core::acquire::DownloadResponse, String> {
        let staged = self.require_browser()?.download(url).await?;
        let name = filename.unwrap_or_else(|| staged.suggested_name.clone());
        store_file_in_root(
            root,
            staged.file(),
            &name,
            Some(max_download_bytes),
            "agent browser",
        )
    }

    /// The service custom integrations are read and written through, the
    /// active workspace's because integration settings are the installation's.
    pub(super) async fn integrations_service(&self) -> Result<Arc<dyn SearchService>, String> {
        self.scope(None)
            .await?
            .require_search("Custom integrations are not available in this session")
    }
}
