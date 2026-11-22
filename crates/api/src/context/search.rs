//! Search: the agent's search service, search roots and queries, related
//! documents and folder tidying.

use super::*;

#[async_trait::async_trait]
impl wilkes_agent::search::SearchService for AppContext {
    async fn read_library(
        self: Arc<Self>,
        kind: wilkes_agent::library::LibraryKind,
    ) -> Result<serde_json::Value, String> {
        use wilkes_agent::library::LibraryKind;
        let result: anyhow::Result<serde_json::Value> = async {
            Ok(match kind {
                LibraryKind::Bookmarks => serde_json::to_value(self.list_bookmarks().await?)?,
                LibraryKind::SavedWorks => serde_json::to_value(self.list_saved_works().await?)?,
                LibraryKind::Tags => serde_json::to_value(self.list_tags()?)?,
                LibraryKind::SearchHistory => serde_json::to_value(self.list_search_log(1000)?)?,
            })
        }
        .await;
        result.map_err(|e| format!("{e:#}"))
    }

    async fn edit_library(
        self: Arc<Self>,
        edit: wilkes_agent::library::LibraryEdit,
    ) -> Result<serde_json::Value, String> {
        use wilkes_agent::library::LibraryEdit as Edit;
        use wilkes_core::types::*;
        // Enforced here as well as at MCP: no adapter can write into an
        // application-managed corpus through the research-library interface.
        self.ensure_writable().map_err(|e| e.to_string())?;
        let result: anyhow::Result<serde_json::Value> = async {
            let value = match edit {
                Edit::AddBookmark {
                    path,
                    location,
                    quote,
                    note,
                } => serde_json::to_value(
                    self.add_bookmark(NewBookmark {
                        path,
                        origin: location.into_origin().map_err(anyhow::Error::msg)?,
                        quote,
                        note,
                        text_range: None,
                        rects: vec![],
                    })
                    .await?,
                )?,
                Edit::UpdateBookmarkNote { id, note } => {
                    serde_json::to_value(self.update_bookmark_note(&id, note).await?)?
                }
                Edit::RemoveBookmark { id } => {
                    self.remove_bookmark(&id).await?;
                    serde_json::json!({"removed":id})
                }
                Edit::UpdateSavedWorkNote { id, note } => {
                    serde_json::to_value(self.update_saved_work_note(&id, note)?)?
                }
                Edit::RemoveSavedWork { id } => {
                    self.remove_saved_work(&id)?;
                    serde_json::json!({"removed":id})
                }
                Edit::CreateTag { name, color } => {
                    serde_json::to_value(self.create_tag(NewTag { name, color })?)?
                }
                Edit::UpdateTag { id, name, color } => {
                    serde_json::to_value(self.update_tag(&id, UpdateTag { name, color })?)?
                }
                Edit::DeleteTag { id } => {
                    self.delete_tag(&id)?;
                    serde_json::json!({"removed":id})
                }
                Edit::TagDocuments {
                    paths,
                    add_tag_ids,
                    remove_tag_ids,
                } => {
                    self.update_document_tags(DocumentTagUpdate {
                        paths,
                        add_tag_ids,
                        remove_tag_ids,
                    })?;
                    serde_json::json!({"updated":true})
                }
                Edit::CreateCollection { name, expression } => serde_json::to_value(
                    self.create_collection(NewSmartCollection { name, expression })?,
                )?,
                Edit::UpdateCollection {
                    id,
                    name,
                    expression,
                } => serde_json::to_value(
                    self.update_collection(&id, UpdateSmartCollection { name, expression })?,
                )?,
                Edit::DeleteCollection { id } => {
                    self.delete_collection(&id)?;
                    serde_json::json!({"removed":id})
                }
                Edit::RenameFile { path, new_name } => {
                    serde_json::json!({"path":self.rename_file(path,new_name).await?})
                }
                Edit::RefreshMetadata { path } => {
                    serde_json::to_value(self.resolve_file_metadata(path).await?)?
                }
            };
            self.events
                .emit("research-state-updated", serde_json::json!({"kind":"mcp"}));
            Ok(value)
        }
        .await;
        result.map_err(|e| format!("{e:#}"))
    }

    async fn save_literature_result(
        self: Arc<Self>,
        provider: String,
        work: LiteratureSearchResult,
        note: Option<String>,
    ) -> Result<serde_json::Value, String> {
        self.ensure_writable().map_err(|e| e.to_string())?;
        let result: anyhow::Result<serde_json::Value> = (|| {
            Ok(serde_json::to_value(self.save_work(NewSavedWork {
                provider,
                work,
                note,
            })?)?)
        })();
        result.map_err(|e| format!("{e:#}"))
    }

    async fn mark_literature_acquired(
        self: Arc<Self>,
        provider: String,
        work: LiteratureSearchResult,
        path: PathBuf,
    ) -> Result<(), String> {
        self.mark_saved_work_acquired(&provider, &work, path)
            .map(|_| ())
            .map_err(|e| format!("{e:#}"))
    }

    async fn default_root(self: Arc<Self>) -> Option<PathBuf> {
        self.get_settings().await.last_directory
    }

    async fn library_roots(self: Arc<Self>) -> Vec<PathBuf> {
        let settings = self.get_settings().await;
        library_roots(&settings).0
    }

    async fn max_search_file_size(self: Arc<Self>) -> u64 {
        self.get_settings().await.max_file_size
    }

    async fn max_download_bytes(self: Arc<Self>) -> u64 {
        self.get_settings().await.max_download_bytes
    }

    fn is_read_only(&self) -> bool {
        AppContext::is_read_only(self)
    }

    async fn list_smart_collections(self: Arc<Self>) -> Result<Vec<SmartCollection>, String> {
        self.list_collections().map_err(|e| e.to_string())
    }

    async fn integrations(self: Arc<Self>) -> wilkes_core::types::IntegrationsSettings {
        self.get_settings().await.integrations
    }

    async fn custom_integrations(self: Arc<Self>) -> Result<serde_json::Value, String> {
        let integrations = self.get_settings().await.integrations;
        serde_json::to_value(crate::commands::integrations::custom::stored_integrations(
            &integrations,
        ))
        .map_err(|e| format!("{e:#}"))
    }

    async fn probe_custom_integration(
        self: Arc<Self>,
        manifest: String,
        query: String,
    ) -> Result<wilkes_agent::search::IntegrationProbe, String> {
        let integrations = self.get_settings().await.integrations;
        crate::commands::integrations::custom::agent_probe(manifest, query, &integrations)
            .await
            .map_err(|e| format!("{e:#}"))
    }

    async fn save_custom_integration(
        self: Arc<Self>,
        manifest: String,
        enabled: Option<bool>,
    ) -> Result<serde_json::Value, String> {
        self.save_custom_integration_manifest(manifest, enabled)
            .await
            .and_then(|saved| Ok(serde_json::to_value(saved)?))
            .map_err(|e| format!("{e:#}"))
    }

    fn catalogue_mirror(&self) -> Option<PathBuf> {
        Some(self.catalogue_dir.clone())
    }

    async fn search(
        self: Arc<Self>,
        query: SearchQuery,
        max_files: usize,
    ) -> Result<wilkes_agent::search::CollectedSearch, String> {
        let handle = self.start_search_as(query, "agent").await?;
        let mut files = Vec::new();
        let mut truncated = false;
        // An agent holds one request open per call and gives up on it, so this
        // search answers within a budget or says it was cut short. The
        // interface has no such bound: it streams.
        let (stats, complete) = handle
            .run_within(
                Some(wilkes_agent::search::AGENT_SEARCH_BUDGET),
                |file_matches| {
                    if files.len() < max_files {
                        files.push(file_matches);
                    } else {
                        truncated = true;
                    }
                    async { true }
                },
            )
            .await;

        Ok(wilkes_agent::search::CollectedSearch {
            files,
            stats,
            truncated,
            complete,
        })
    }

    async fn related_documents(
        self: Arc<Self>,
        query: RelatedDocumentsQuery,
    ) -> Result<Vec<RelatedDocument>, String> {
        AppContext::related_documents(self, query).await
    }

    async fn list_documents(self: Arc<Self>, root: PathBuf) -> Result<FileListResponse, String> {
        self.list_files(root).await.map_err(|e| e.to_string())
    }

    async fn citation_graph(
        self: Arc<Self>,
        query: CitationGraphQuery,
    ) -> Result<CitationGraph, String> {
        AppContext::citation_graph(self, query).await
    }

    async fn document_metadata(self: Arc<Self>, path: PathBuf) -> Result<DocumentMetadata, String> {
        self.document_metadata_full(path)
            .await
            .map_err(|e| e.to_string())
    }
}

pub(super) struct SemanticRuntime {
    pub(super) embedder: Arc<dyn Embedder>,
    pub(super) index: Arc<Mutex<Option<SemanticIndex>>>,
    pub(super) indexing: IndexingConfig,
}

/// The coordinate system one workspace's index carries, as a consumer route
/// has to see it before it answers about passages.
///
/// Three states rather than an `Option<String>`, because "there is no index"
/// and "there is an index that cannot say which space it is" are different
/// answers with different remedies: build one, and rebuild one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexSpace {
    /// The workspace holds no index. Embedding text still works here; nothing
    /// that addresses a passage does.
    Absent,
    /// An index migrated from before schema v10: its `exact_identity` is
    /// absent and its `chunk_ref` is null on every row. Its vectors remain
    /// usable by Wilkes locally, where the engine/model/dimension tuple
    /// matching the runtime is enough — that tuple is not, and never was,
    /// vector-compatibility proof for a consumer.
    Unverified,
    /// The index knows exactly which space it is, so its passages can be
    /// named.
    Exact(String),
}

impl IndexSpace {
    /// The id to report, which exists only when the index can prove it.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Exact(id) => Some(id.as_str()),
            Self::Absent | Self::Unverified => None,
        }
    }
}

pub struct AppContext {
    /// Workspace-owned databases.
    pub data_dir: PathBuf,
    /// Shared Python environment, managed backups, saved chat conversations,
    /// and other installation data.
    pub shared_data_dir: PathBuf,
    /// The one model cache for the installation: `shared_data_dir/models`.
    ///
    /// Its own directory rather than the installation root, and derived here
    /// rather than at each call site, because every consumer of it must name
    /// the same path — the embedding-space identity is derived from the cache
    /// root's contents, so two roots are two identities for one model.
    pub model_dir: PathBuf,
    /// The one catalogue mirror for the installation:
    /// `shared_data_dir/catalogue`.
    ///
    /// Shared rather than workspace-owned because its contents are not this
    /// workspace's: it is a copy of what four public catalogues publish, and
    /// the same copy answers every workspace. Per-workspace, an installation
    /// paid for a sync once per workspace and held one table per workspace to
    /// show for it, and a consumer that synced under one workspace found an
    /// empty mirror under the next.
    pub catalogue_dir: PathBuf,
    /// The one store of document covers for the installation:
    /// `shared_data_dir/covers`.
    ///
    /// Shared because a cover belongs to a file, not to a workspace, and two
    /// workspaces listing one library should not each draw it.
    pub cover_cache_dir: PathBuf,
    pub settings_path: PathBuf,
    pub workspace_path: PathBuf,
    pub bookmarks_path: PathBuf,
    pub(super) embedder: PLMutex<Option<Arc<dyn Embedder>>>,
    pub(super) index: PLMutex<Arc<Mutex<Option<SemanticIndex>>>>,
    /// Persistent cache of extracted document metadata, opened lazily. Shared
    /// with the index watcher so renames re-key rather than re-extract.
    pub(super) metadata_cache: PLMutex<Option<Arc<Mutex<MetadataCache>>>>,
    /// The durable record of what each indexing job did with each document,
    /// opened on first use. Its own database beside the index, because a build
    /// publishes by replacing the index file.
    pub(super) job_journal: PLMutex<Option<Arc<Mutex<IndexJobJournal>>>>,
    pub(super) research_store: PLMutex<Option<Arc<Mutex<ResearchStore>>>>,
    /// The text of every library document, for exact search; opened on first
    /// use. Its own database, kept whether or not anything is embedded — see
    /// [`wilkes_core::text_index`].
    pub(super) text_index: PLMutex<Option<Arc<DocumentTextIndex>>>,
    pub(super) text_index_upkeep: TextIndexUpkeep,
    pub(super) reading_order: ReadingOrderState,
    /// The feeds' refresh runner, its request gate and its timer.
    pub(super) feed_upkeep: feeds::FeedUpkeep,
    pub(super) directory_watcher: PLMutex<Option<DirectoryWatcher>>,
    pub(super) embed_task: PLMutex<Option<EmbedTaskHandle>>,
    pub(super) embed_cancel_in_progress: AtomicBool,
    /// Notified when a stopped build has finished winding down — the tail the
    /// cancellation deliberately does not wait for. Only the exit path waits
    /// on it, and only so the build's finished documents reach disk.
    pub(super) winddown: Arc<tokio::sync::Notify>,
    pub(super) shutting_down: AtomicBool,
    pub worker_manager: WorkerManager,
    /// Generation runs in its own process with its own manager. Sharing the
    /// embedding manager would evict a multi-gigabyte model on every
    /// alternation, because the worker caches exactly one model and restarts on
    /// role change.
    pub generate_manager: WorkerManager,
    /// The worker that recognizes the text drawn inside images. Its own, for
    /// the reason `new` gives.
    pub recognize_manager: WorkerManager,
    pub(super) generator: PLMutex<Option<Arc<dyn Generator>>>,
    /// Serialises `load_generator`, so two settings changes cannot download and
    /// attach concurrently.
    pub(super) generator_load_lock: tokio::sync::Mutex<()>,
    /// Serializes analyzer loads against each other, so that two settings
    /// edits in flight cannot install the older one's analyzer last.
    ///
    /// Not a memory bound, though it was described as one for as long as the
    /// weights were read here. They are read in the recognition worker now,
    /// and what this assembles is the address of one — so a lock held per
    /// context bounded nothing, least of all across the several contexts the
    /// host opens. What bounds resident weights is `worker::residency`, which
    /// is process-wide because that is the scope the memory is scarce in.
    pub(super) image_analyzer_load_lock: tokio::sync::Mutex<()>,
    /// Claimed by each load before it queues behind the lock. Only the newest
    /// claim may assign the generator; an older one that finishes later would
    /// otherwise attach a model the user has already switched away from.
    pub(super) generator_epoch: AtomicU64,
    /// At most one labelling run in flight: a newer `cluster_bookmarks` call
    /// makes the previous run's results describe a partition nobody is looking
    /// at any more.
    pub(super) cluster_label_task: PLMutex<Option<JoinHandle<()>>>,
    /// Root and document clouds can be visible together. Each request owns one
    /// cancellation lifecycle; closing or redrawing one surface must not stop
    /// the other.
    pub(super) topic_operations: PLMutex<HashMap<String, TopicOperation>>,
    /// Exactly one retained root tree and one current-document tree. This
    /// prevents viewer navigation from evicting the library tree without
    /// turning the O(n²) cache into unbounded history.
    pub(super) topic_tree_caches: PLMutex<TopicTreeCaches>,
    /// Serialises cache misses so rapid redraw requests cannot build duplicate
    /// O(n²) trees concurrently.
    pub(super) topic_tree_build_lock: tokio::sync::Mutex<()>,
    /// Incremented before every semantic-index mutation. A build captures this
    /// value and may install its tree only if the value is still current.
    pub(super) semantic_index_revision: AtomicU64,
    /// Completion caches, Rocchio feedback, and inspector history are scoped to
    /// this application session and share the single core completion pipeline.
    pub(super) completion_session: PLMutex<CompletionSession>,
    pub(super) completion_operations: PLMutex<HashMap<String, CompletionOperation>>,
    /// The search compositions in flight, by request id, each with the flag
    /// that stops its decode.
    pub(super) compose_operations: PLMutex<HashMap<String, Arc<AtomicBool>>>,
    pub(super) events: Arc<dyn EventEmitter>,
    pub(super) settings_lock: tokio::sync::Mutex<()>,
    pub(super) managed_runtime_lock: tokio::sync::Mutex<()>,
    pub(super) managed_import_lock: tokio::sync::Mutex<()>,
    pub(super) managed_pending_imports: AtomicU64,
    pub(super) bookmarks_lock: tokio::sync::Mutex<()>,
    /// What a cancellation does to embedding on a remote host: the remote
    /// counterpart of sealing this context's workers.
    pub(super) remote_seal: Arc<RemoteSeal>,
    /// The embedder that answers another instance's request for a model by
    /// name, kept so that consecutive requests reuse it. It is a handle on
    /// this context's own embed worker, so a model this workspace also uses is
    /// the one already resident there. Locked across a load, so concurrent
    /// requests for a new model load it once.
    pub(super) model_embedder: tokio::sync::Mutex<Option<ModelEmbedder>>,
}

impl AppContext {
    pub fn is_semantic_ready(&self) -> bool {
        self.embedder.lock().is_some()
            && self
                .index
                .lock()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_some()
    }
    /// Which coordinate system this workspace's index carries.
    ///
    /// Read from the index's own stored metadata rather than from the
    /// embedder that happens to be loaded: the vectors were produced by
    /// whatever built the index, and it is those the caller is about to be
    /// told something about.
    pub fn index_space(&self) -> Result<IndexSpace, ConsumerError> {
        let index_arc = self.index.lock().clone();
        let guard = index_arc
            .lock()
            .map_err(|_| "Semantic index lock was poisoned")?;
        let Some(index) = guard.as_ref() else {
            return Ok(IndexSpace::Absent);
        };
        // An index that cannot be asked what it is is a fault, not a state:
        // answering "unverified" here would report a rebuildable index where
        // there is an unreadable one.
        let metadata = index
            .embedding_metadata()
            .map_err(|error| ConsumerError::from_anyhow(&error))?;
        Ok(match metadata.exact_identity {
            Some(identity) => IndexSpace::Exact(identity.id().0),
            None => IndexSpace::Unverified,
        })
    }

    // ── Search ────────────────────────────────────────────────────────────────
    pub(super) fn canonicalize_search_root(root: &Path) -> Result<PathBuf, String> {
        std::fs::canonicalize(root).map_err(|err| {
            format!(
                "Search root does not exist or cannot be accessed: {} ({err})",
                root.display()
            )
        })
    }
    pub(super) fn canonicalize_supported_file(
        root: &Path,
        path: &Path,
        supported_extensions: &[String],
        label: &str,
    ) -> Result<(PathBuf, FileType), String> {
        let requested = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let file = std::fs::canonicalize(&requested).map_err(|err| {
            format!(
                "{label} file does not exist or cannot be accessed: {} ({err})",
                requested.display()
            )
        })?;
        if !file.is_file() {
            return Err(format!("{label} file is not a file: {}", file.display()));
        }
        let file_type = FileType::detect(&file, supported_extensions)
            .ok_or_else(|| format!("{label} file type is not supported: {}", file.display()))?;
        Ok((file, file_type))
    }
    pub(super) fn prepare_search_query(
        mut query: SearchQuery,
        supported_extensions: Vec<String>,
        library_roots: &[PathBuf],
    ) -> Result<SearchQuery, String> {
        query.supported_extensions = supported_extensions;
        if query.scope != SearchScope::All {
            query.root = Self::canonicalize_search_root(&query.root)?;
            Self::ensure_path_in_library(&query.root, library_roots, "Search root")?;
        }

        if let SearchScope::File { path } = &query.scope {
            let (file, _) = Self::canonicalize_supported_file(
                &query.root,
                path,
                &query.supported_extensions,
                "Search",
            )?;
            Self::ensure_path_in_library(&file, library_roots, "Search file")?;
            query.scope = SearchScope::File { path: file };
        }

        Ok(query)
    }
    pub(super) fn ensure_path_in_library(
        path: &Path,
        library_roots: &[PathBuf],
        label: &str,
    ) -> Result<(), String> {
        if library_roots.iter().any(|root| path.starts_with(root)) {
            return Ok(());
        }
        Err(format!("{label} is not in the library: {}", path.display()))
    }
    pub(super) fn ensure_no_active_embed_task(&self, message: &str) -> Result<(), String> {
        if self.embed_cancel_in_progress.load(Ordering::Acquire) {
            return Err(message.to_string());
        }
        let mut guard = self.embed_task.lock();
        if let Some(task) = guard.as_ref() {
            if !task.join.is_finished() {
                return Err(message.to_string());
            }
            *guard = None;
        }
        Ok(())
    }
    pub(super) async fn prepare_semantic_runtime(
        self: &Arc<Self>,
        root: &Path,
        settings: &Settings,
    ) -> Result<SemanticRuntime, String> {
        self.ensure_no_active_embed_task("Semantic index is currently being built. Please wait.")?;

        let embedder = self
            .embedder
            .lock()
            .clone()
            .ok_or_else(|| "No semantic index found. Build the index first.".to_string())?;
        let index_arc = self.index.lock().clone();
        let query_root_canonical =
            std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let root_ready = {
            let guard = index_arc.lock().unwrap_or_else(|p| p.into_inner());
            match guard.as_ref() {
                Some(idx) => {
                    let status = idx.status_for_root(Some(&query_root_canonical));
                    status.indexed_files > 0 && status.total_chunks > 0
                }
                None => false,
            }
        };

        if !root_ready {
            // Saying so is the whole answer. A search that discovered an
            // unindexed root used to start indexing it, which is hours of
            // inference nobody asked for — reachable from an MCP tool call,
            // where there is no window to watch it in and no button to stop
            // it. Building is an act the user performs, from the interface
            // that offers it; `useSemanticStore.refreshCoverage` records the
            // same separation on the other side.
            return Err(format!(
                "Semantic index is not ready for search root {}.",
                root.display()
            ));
        }

        Ok(SemanticRuntime {
            embedder,
            index: index_arc,
            indexing: IndexingConfig {
                chunk_size: settings.semantic.chunk_size,
                chunk_overlap: settings.semantic.chunk_overlap,
                supported_extensions: settings.supported_extensions.clone(),
            },
        })
    }
    pub(super) async fn prepare_global_semantic_runtime(
        self: &Arc<Self>,
        settings: &Settings,
    ) -> Result<SemanticRuntime, String> {
        self.ensure_no_active_embed_task("Semantic index is currently being built. Please wait.")?;
        let embedder = self
            .embedder
            .lock()
            .clone()
            .ok_or_else(|| "No semantic index found. Build the index first.".to_string())?;
        let index = self.index.lock().clone();
        let ready = index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|idx| {
                let status = idx.status();
                status.indexed_files > 0 && status.total_chunks > 0
            });
        if !ready {
            return Err("The global semantic index has no searchable documents.".to_string());
        }
        Ok(SemanticRuntime {
            embedder,
            index,
            indexing: IndexingConfig {
                chunk_size: settings.semantic.chunk_size,
                chunk_overlap: settings.semantic.chunk_overlap,
                supported_extensions: settings.supported_extensions.clone(),
            },
        })
    }
    /// Resolve semantic state (if needed) and start the search. Handles both
    /// Grep and Semantic modes; callers do not branch on mode.
    pub async fn start_search(self: Arc<Self>, query: SearchQuery) -> Result<SearchHandle, String> {
        self.start_search_as(query, "app").await
    }
    pub async fn start_search_as(
        self: Arc<Self>,
        mut query: SearchQuery,
        initiated_by: &str,
    ) -> Result<SearchHandle, String> {
        let settings = self.settings().await;
        let (resolved_library_roots, library_root_errors) = library_roots(&settings);
        query = Self::prepare_search_query(
            query,
            settings.supported_extensions.clone(),
            &resolved_library_roots,
        )?;

        let eligibility_roots = match &query.scope {
            SearchScope::All => resolved_library_roots.clone(),
            SearchScope::File { path } => resolved_library_roots
                .iter()
                .filter(|root| path.starts_with(root))
                .max_by_key(|root| root.components().count())
                .cloned()
                .map(|root| vec![root])
                .unwrap_or_else(|| vec![query.root.clone()]),
            SearchScope::Corpus => vec![query.root.clone()],
        };
        let mut documents = Vec::new();
        let mut catalog_errors = Vec::new();
        let mut seen_paths = std::collections::HashSet::new();
        let catalog_started = std::time::Instant::now();
        // A `File` scope names exactly one document, so the catalog step
        // resolves that path directly instead of enumerating the root.
        let only_path = match &query.scope {
            SearchScope::File { path } => Some(path.clone()),
            _ => None,
        };
        for root in &eligibility_roots {
            match self
                .list_files_filtered_with_ignore(
                    root.clone(),
                    query.collection_id.as_deref(),
                    &query.tag_ids,
                    None,
                    query.respect_gitignore && only_path.is_none(),
                    only_path.clone(),
                )
                .await
            {
                Ok(listed) => {
                    if let SearchScope::File { path } = &query.scope {
                        if let Some(omitted) =
                            listed.omitted.iter().find(|entry| entry.file.path == *path)
                        {
                            if omitted.reason == OmittedFileReason::TooLarge {
                                catalog_errors.push(format!(
                                    "Search file exceeds the configured maximum size ({} bytes > {} bytes): {}",
                                    omitted.file.size_bytes,
                                    settings.max_file_size,
                                    path.display()
                                ));
                            }
                        }
                    }
                    for entry in listed.files {
                        if seen_paths.insert(entry.path.clone()) {
                            documents.push(SearchDocument {
                                path: entry.path,
                                file_type: entry.file_type,
                                title: entry.title,
                                author: entry.author,
                            });
                        }
                    }
                }
                Err(error) => catalog_errors.push(format!("{}: {error:#}", root.display())),
            }
        }
        let catalog_elapsed_ms = catalog_started.elapsed().as_millis() as u64;

        let mut semantic_indexing = None;
        if query.scope == SearchScope::All {
            catalog_errors.splice(0..0, library_root_errors);
        }
        let (embedder, index) = match query.mode {
            SearchMode::Semantic => {
                let runtime = if query.scope == SearchScope::All {
                    self.prepare_global_semantic_runtime(&settings).await?
                } else {
                    self.prepare_semantic_runtime(&query.root, &settings)
                        .await?
                };
                semantic_indexing = Some(runtime.indexing);
                (Some(runtime.embedder), Some(runtime.index))
            }
            SearchMode::Grep => (None, None),
            // The combined mode's exact lane needs nothing, so an unavailable
            // semantic lane reduces the search rather than failing it. This is
            // the one place that knows *why* it is unavailable, so it is the
            // one place that says so — the reduction reaches the user as a
            // search error, never as a quietly shorter result list.
            SearchMode::Hybrid => {
                let prepared = if query.scope == SearchScope::All {
                    self.prepare_global_semantic_runtime(&settings).await
                } else {
                    self.prepare_semantic_runtime(&query.root, &settings).await
                };
                match prepared {
                    Ok(runtime) => {
                        semantic_indexing = Some(runtime.indexing);
                        (Some(runtime.embedder), Some(runtime.index))
                    }
                    Err(reason) => {
                        warn!("combined search has no semantic lane: {reason}");
                        catalog_errors.push(format!(
                            "Combined search found exact matches only — related passages need the semantic index: {reason}"
                        ));
                        (None, None)
                    }
                }
            }
        };

        let log = {
            let store = self.research_store().map_err(|e| e.to_string())?;
            let id = store
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .start_search_log(&query, initiated_by)
                .map_err(|e| e.to_string())?;
            Some(SearchLogTracker::new(store, id))
        };

        // Query-vector enhancement is a semantic-only concern. HyDE additionally
        // needs the loaded generator; hand it over only when HyDE is on so the
        // provider does not hold a generator reference it will never use.
        let retrieval = settings.retrieval.clone();
        let generator = if query.mode.uses_semantic_index() && retrieval.hyde.enabled {
            self.generator.lock().clone()
        } else {
            None
        };

        let primary_metadata_source = metadata_source_preference(&settings.primary_metadata_source);
        Ok(start_search(
            query,
            documents,
            catalog_errors,
            embedder,
            index,
            semantic_indexing,
            log,
            retrieval,
            generator,
            self.stored_text(&settings),
        )
        .with_catalog_elapsed_ms(catalog_elapsed_ms)
        .with_metadata(self.metadata_cache(), primary_metadata_source))
    }
    pub(super) async fn eligible_paths_for_collection(
        &self,
        collection_id: &str,
        roots: &[PathBuf],
    ) -> Result<std::collections::HashSet<PathBuf>, String> {
        self.eligible_paths_for_filters(Some(collection_id), &[], roots)
            .await
    }
    pub(super) async fn eligible_paths_for_filters(
        &self,
        collection_id: Option<&str>,
        tag_ids: &[String],
        roots: &[PathBuf],
    ) -> Result<std::collections::HashSet<PathBuf>, String> {
        let mut all = std::collections::HashSet::new();
        for root in roots {
            let listed = self
                .list_files(root.clone())
                .await
                .map_err(|e| e.to_string())?;
            let mut eligible = listed
                .files
                .iter()
                .filter(|entry| {
                    tag_ids
                        .iter()
                        .all(|id| entry.tags.iter().any(|tag| tag.id == *id))
                })
                .map(|entry| entry.path.clone())
                .collect::<std::collections::HashSet<_>>();
            if let Some(collection_id) = collection_id {
                let store = self.research_store().map_err(|e| e.to_string())?;
                let collection_paths = store
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .eligible_paths(collection_id, root, &listed.files)
                    .map_err(|e| e.to_string())?;
                eligible.retain(|path| collection_paths.contains(path));
            }
            all.extend(eligible);
        }
        Ok(all)
    }
    pub async fn related_documents(
        self: Arc<Self>,
        mut query: RelatedDocumentsQuery,
    ) -> Result<Vec<RelatedDocument>, String> {
        const DEFAULT_LIMIT: usize = 8;
        const MAX_LIMIT: usize = 25;

        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        query.root = Self::canonicalize_search_root(&query.root)?;
        Self::ensure_path_in_library(&query.root, &library_roots, "Related documents root")?;
        let (source_path, _) = Self::canonicalize_supported_file(
            &query.root,
            &query.path,
            &settings.supported_extensions,
            "Related documents",
        )?;
        Self::ensure_path_in_library(&source_path, &library_roots, "Related documents file")?;
        let runtime = self
            .prepare_semantic_runtime(&query.root, &settings)
            .await?;
        let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let supported_extensions = settings.supported_extensions.clone();

        let root = query.root.clone();
        let listing_roots = if query.scope == SearchScope::All {
            library_roots
        } else {
            vec![root.clone()]
        };
        let eligible_paths = match query.collection_id.as_deref() {
            Some(collection_id) => Some(
                self.eligible_paths_for_collection(collection_id, &listing_roots)
                    .await?,
            ),
            None => None,
        };
        let mut related = tokio::task::spawn_blocking(move || {
            let guard = runtime.index.lock().unwrap_or_else(|p| p.into_inner());
            let idx = guard
                .as_ref()
                .ok_or_else(|| "No semantic index found. Build the index first.".to_string())?;
            idx.related_documents_filtered(
                &query.root,
                &source_path,
                limit,
                &supported_extensions,
                query.scope == SearchScope::All,
                eligible_paths.as_ref(),
            )
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("related documents task panicked: {e}"))??;

        // Use the canonical file-list metadata pipeline so every document list
        // renders and sorts the same record shape.
        let mut entries = std::collections::HashMap::new();
        for listing_root in listing_roots {
            let listed = self
                .list_files(listing_root)
                .await
                .map_err(|e| e.to_string())?;
            entries.extend(
                listed
                    .files
                    .into_iter()
                    .map(|entry| (entry.path.clone(), entry)),
            );
        }
        for document in &mut related {
            if let Some(entry) = entries.get(&document.entry.path) {
                document.entry = entry.clone();
            }
        }
        Ok(related)
    }
    /// Where each document directly inside `folder` would sit better, among
    /// the other folders of the library. Moves nothing: the plan is the
    /// user's to review, and moving is the existing move's job.
    ///
    /// A destination is judged by its indexed documents and by its name (see
    /// [`wilkes_core::embed::tidy`]); documents are compared by the same
    /// whole-document vectors as [`Self::related_documents`].
    pub async fn tidy_folder(self: Arc<Self>, folder: PathBuf) -> Result<TidyPlan, String> {
        use std::collections::{BTreeMap, BTreeSet};
        use wilkes_core::embed::tidy::{
            folder_centroid, is_close_call, rank_folders, FolderProfile,
        };
        /// The few best destinations offered for each document. Past these
        /// the ranking is noise, and the existing move reaches any folder.
        const OFFERED: usize = 5;

        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        let folder = std::fs::canonicalize(&folder).map_err(|err| {
            format!(
                "Folder does not exist or cannot be accessed: {} ({err})",
                folder.display()
            )
        })?;
        Self::ensure_path_in_library(&folder, &library_roots, "Folder to tidy")?;

        // Every folder of the library, and the documents directly in each.
        let mut folders: BTreeSet<PathBuf> = BTreeSet::new();
        let mut documents: BTreeMap<PathBuf, Vec<FileEntry>> = BTreeMap::new();
        let mut seen = HashSet::new();
        for root in &library_roots {
            let listed = self
                .list_files(root.clone())
                .await
                .map_err(|e| e.to_string())?;
            folders.insert(root.clone());
            folders.extend(listed.directories);
            for entry in listed.files {
                if !seen.insert(entry.path.clone()) {
                    continue;
                }
                let Some(parent) = entry.path.parent() else {
                    continue;
                };
                documents
                    .entry(parent.to_path_buf())
                    .or_default()
                    .push(entry);
            }
        }
        // A folder that holds only folders is a shelf, not a place to file a
        // document; the folders on it are offered instead.
        let destinations: Vec<PathBuf> = folders
            .iter()
            .filter(|candidate| **candidate != folder)
            .filter(|candidate| {
                documents.contains_key(*candidate)
                    || !folders
                        .iter()
                        .any(|other| other.parent() == Some(candidate.as_path()))
            })
            .cloned()
            .collect();
        let sources = documents.remove(&folder).unwrap_or_default();
        if sources.is_empty() {
            return Err(format!(
                "There are no documents directly in {} to tidy",
                folder.display()
            ));
        }
        if destinations.is_empty() {
            return Err(
                "There is no other folder in the library to sort these documents into".to_string(),
            );
        }

        let runtime = self.prepare_global_semantic_runtime(&settings).await?;
        let mut wanted: Vec<PathBuf> = sources.iter().map(|entry| entry.path.clone()).collect();
        for destination in &destinations {
            if let Some(entries) = documents.get(destination) {
                wanted.extend(entries.iter().map(|entry| entry.path.clone()));
            }
        }
        let index = Arc::clone(&runtime.index);
        let vectors = tokio::task::spawn_blocking(move || {
            let guard = index.lock().unwrap_or_else(|p| p.into_inner());
            let idx = guard
                .as_ref()
                .ok_or_else(|| "No semantic index found. Build the index first.".to_string())?;
            idx.document_vectors(&wanted).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("Tidy-up task panicked: {e}"))??;

        // Names are embedded in one call, as a query is: a name describes
        // what belongs in the folder.
        let names: Vec<String> = destinations
            .iter()
            .map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .ok_or_else(|| format!("Folder has no name: {}", path.display()))
            })
            .collect::<Result<_, _>>()?;
        let name_vectors = Self::embed_with(Arc::clone(&runtime.embedder), names, EmbedRole::Query)
            .await?
            .vectors;

        let mut profiles = Vec::with_capacity(destinations.len());
        let mut plan_destinations = Vec::with_capacity(destinations.len());
        for (path, name) in destinations.iter().zip(name_vectors) {
            let members = documents.get(path).map(Vec::as_slice).unwrap_or_default();
            let indexed: Vec<&[f32]> = members
                .iter()
                .filter_map(|entry| vectors.get(&entry.path).map(Vec::as_slice))
                .collect();
            plan_destinations.push(TidyDestination {
                path: path.clone(),
                documents: members.len(),
                indexed_documents: indexed.len(),
            });
            profiles.push(FolderProfile {
                name,
                content: folder_centroid(indexed),
            });
        }

        let mut suggestions = Vec::new();
        let mut unindexed = Vec::new();
        for entry in sources {
            let Some(vector) = vectors.get(&entry.path) else {
                unindexed.push(entry);
                continue;
            };
            let ranking = rank_folders(vector, &profiles);
            let close_call = is_close_call(&ranking);
            suggestions.push(TidySuggestion {
                entry,
                ranking: ranking
                    .into_iter()
                    .take(OFFERED)
                    .map(|placement| TidyPlacement {
                        destination: destinations[placement.folder].clone(),
                        score: placement.score,
                    })
                    .collect(),
                close_call,
            });
        }
        Ok(TidyPlan {
            folder,
            destinations: plan_destinations,
            suggestions,
            unindexed,
        })
    }
}
