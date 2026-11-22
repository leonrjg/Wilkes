use super::*;
use std::sync::Mutex as StdMutex;
use wilkes_core::types::{EmbedderModel, EmbeddingEngine};

struct NoopEmitter;

impl EventEmitter for NoopEmitter {
    fn emit(&self, _name: &str, _payload: serde_json::Value) {}
}

impl ViewEventSink for NoopEmitter {
    fn emit_to(&self, _audience: EventAudience<'_>, _name: &str, _payload: serde_json::Value) {}
}

struct RecordingEmitter(Arc<StdMutex<Vec<String>>>);

#[tokio::test]
async fn concurrent_root_registration_persists_in_the_owning_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    let (manager, _events, _worker_loop) = WorkspaceManager::new(
        dir.path().to_path_buf(),
        settings.clone(),
        Arc::new(NoopEmitter),
    )
    .unwrap();
    let ctx = manager.active();
    let a = ctx.data_dir.join("uploads-a");
    let b = ctx.data_dir.join("uploads-b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (first, second) =
        tokio::join!(ctx.register_library_root(&a), ctx.register_library_root(&b));
    first.unwrap();
    second.unwrap();
    ctx.register_library_root(&a).await.unwrap();
    let stored = ctx.get_settings().await;
    assert_eq!(stored.recent_dirs.len(), 2);
    assert!(stored.recent_dirs.contains(&a.canonicalize().unwrap()));
    assert!(stored.recent_dirs.contains(&b.canonicalize().unwrap()));
    assert!(stored.last_directory.is_none());
    assert!(stored.favorites.is_empty());
    let manifest = read_manifest(&ctx.workspace_path).unwrap();
    assert_eq!(manifest.recent_roots, stored.recent_dirs);
    assert!(
        !settings.exists(),
        "root registration must only write the workspace manifest"
    );
    manager.shutdown_all().await;
}

impl EventEmitter for RecordingEmitter {
    fn emit(&self, name: &str, _payload: serde_json::Value) {
        self.0.lock().unwrap().push(name.to_string());
    }
}

/// A workspace's event is recorded as `name@view,view` (`*` marking the
/// default), so a test can say where it went.
impl ViewEventSink for RecordingEmitter {
    fn emit_to(&self, audience: EventAudience<'_>, name: &str, _payload: serde_json::Value) {
        self.0.lock().unwrap().push(format!(
            "{name}@{}{}",
            audience.views.join(","),
            if audience.default { "*" } else { "" }
        ));
    }
}

/// Deleting is not writing into a corpus, so a managed one is deletable
/// exactly like a user workspace — and its hidden projections go with it,
/// because a projection of a corpus that no longer exists is nothing.
#[tokio::test]
async fn deleting_a_managed_corpus_removes_its_projections_and_its_files() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
    let corpus = manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-delete".to_string(),
            embedding: SelectedEmbedder::default(),
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .unwrap();
    let parent_semantic = read_manifest(&workspace_manifest_path(dir.path(), &corpus.corpus_id))
        .unwrap()
        .semantic
        .unwrap();
    let projection = manager
        .ensure_projection_workspace(
            &corpus.corpus_id,
            "underdog",
            "store-delete",
            &parent_semantic,
            &SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("projection-model".to_string()),
                dimension: 2,
            },
        )
        .unwrap();
    assert_ne!(projection, corpus.corpus_id);

    let state = manager.delete(&corpus.corpus_id).await.unwrap();

    assert!(
        !state
            .workspaces
            .iter()
            .any(|workspace| workspace.id == corpus.corpus_id),
        "the corpus is gone from the listing"
    );
    let registry = load_registry(dir.path()).unwrap();
    assert!(!registry.workspace_ids.contains(&corpus.corpus_id));
    assert!(
        !registry.workspace_ids.contains(&projection),
        "the projection went with the corpus that owned it"
    );
    assert!(!workspace_root(dir.path(), &corpus.corpus_id).exists());
    assert!(!workspace_root(dir.path(), &projection).exists());

    manager.shutdown_all().await;
}

/// The registry's own invariant is that the active workspace is one it
/// names. Deleting it would move the user's window as a side effect of a
/// delete, so it is refused and the caller switches first.
#[tokio::test]
async fn the_active_workspace_is_refused_and_left_intact() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
    let active = manager.active_workspace_id().unwrap();

    let error = manager.delete(&active).await.unwrap_err().to_string();
    assert!(
        error.contains("open in a window cannot be deleted"),
        "unexpected refusal: {error}"
    );
    assert!(workspace_root(dir.path(), &active).exists());
    assert_eq!(manager.active_workspace_id().unwrap(), active);

    // And unknown ids are refused rather than quietly succeeding.
    assert!(manager.delete("not-a-workspace").await.is_err());

    manager.shutdown_all().await;
}

/// A workspace deleted while the caller was reading it: the context this
/// manager opened for it is shut down before its files go, and the
/// manager will not hand out another one afterwards.
#[tokio::test]
async fn deleting_a_workspace_retires_the_context_opened_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
    let created = manager.create("Second".to_string()).await.unwrap();
    manager.context_for(&created.id).await.unwrap();
    assert!(manager.contexts.lock().contains_key(&created.id));

    manager.delete(&created.id).await.unwrap();

    assert!(!manager.contexts.lock().contains_key(&created.id));
    assert!(!workspace_root(dir.path(), &created.id).exists());
    assert!(
        manager.context_for(&created.id).await.is_err(),
        "a deleted workspace cannot be opened again"
    );

    manager.shutdown_all().await;
}

/// The canonical corpus opens its index and loads no model.
///
/// This is the payoff, and it is the symptom that started the whole
/// investigation: a Wilkes log showing `load_embedder start:
/// model=AllMiniLML6V2` on a store whose settings named a different model
/// entirely. The canonical corpus was being ensured with an embedder it
/// had no use for, chosen when the corpus was created and unreachable
/// from any setting since.
#[tokio::test]
async fn the_canonical_corpus_opens_an_index_and_loads_no_model() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
    let corpus = manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-index-only".to_string(),
            embedding: SelectedEmbedder::default(),
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .unwrap();

    let context = manager.context_for(&corpus.corpus_id).await.unwrap();
    context
        .ensure_managed_index()
        .await
        .expect("an index with no model behind it");

    // An index, so passages have somewhere to live.
    assert!(
        dir.path()
            .join("workspaces")
            .join(&corpus.corpus_id)
            .join("semantic_index.db")
            .exists()
            || workspace_root(dir.path(), &corpus.corpus_id)
                .join("semantic_index.db")
                .exists(),
        "the canonical corpus has an index"
    );
    // And no model. Nothing installed it, nothing loaded it, and no worker
    // was asked for one — which is why this test can run at all without a
    // downloadable embedder.
    assert!(
        !context.has_embedder(),
        "the canonical corpus holds no embedder"
    );

    manager.shutdown_all().await;
}

#[tokio::test]
async fn a_corpus_without_an_index_reports_no_embedding_space() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings, Arc::clone(&events)).unwrap();
    let embedding = SelectedEmbedder::default();
    let status = manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-empty".to_string(),
            embedding: embedding.clone(),
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .unwrap();
    // The canonical corpus holds no vectors and names no coordinate
    // system. It owns sources, extraction, chunking and passage identity;
    // its projections own the spaces, one model each.
    assert_eq!(status.embedding_space_id, None);
    assert!(status.embedding_space_identity.is_none());
    assert!(status.spaces.is_empty());
    // Not ready only because nothing has opened an index for it yet.
    // Readiness here is membership, never coordinates.
    assert!(!status.ready);

    // The absent space is reported as an explicit null, not by dropping
    // the key: a client reading this API can tell "no space yet" apart
    // from a field it forgot to handle.
    let wire = serde_json::to_value(&status).unwrap();
    assert_eq!(wire["embedding_space_id"], serde_json::Value::Null);
    assert_eq!(wire["embedding_space_identity"], serde_json::Value::Null);

    let root = workspace_root(dir.path(), &status.corpus_id);
    let index = SemanticIndex::create(
        &root,
        embedding.model.model_id(),
        embedding.dimension,
        embedding.engine,
        None,
    )
    .unwrap();
    let space = index.embedding_space_identity().unwrap().id().0;
    drop(index);

    // And it still reports none once an index exists. The index is where
    // passages live; the space it was created under is an artifact of the
    // schema, not something this corpus serves, and publishing it is what
    // made a consumer pin a coordinate system it could not change and a
    // model it never chose.
    let status = manager
        .managed_workspace_status(&status.corpus_id)
        .await
        .unwrap();
    assert_eq!(status.embedding_space_id, None);
    assert!(status.spaces.is_empty());
    assert!(status.ready, "an open index is a ready canonical corpus");
    let _ = space;
}

/// Writes one managed document into a corpus index the way an admission
/// does, so a test can move a corpus's membership generation without a
/// downloadable embedding model.
fn admit(
    index: &mut wilkes_core::embed::index::SemanticIndex,
    path: &Path,
    text: &str,
    vector: Vec<f32>,
    recipe: &wilkes_core::embed::ExtractionRecipe,
) {
    std::fs::write(path, text).unwrap();
    index
        .write_file_with_recipe(
            wilkes_core::embed::index::db::PreparedFile {
                retained: Default::default(),
                path: path.to_path_buf(),
                full_text: text.to_string(),
                chunks: vec![(
                    wilkes_core::embed::index::chunk::Chunk {
                        file_path: path.to_path_buf(),
                        text: text.to_string(),
                        byte_range: wilkes_core::types::ByteRange {
                            start: 0,
                            end: text.len(),
                        },
                        origin: wilkes_core::types::SourceOrigin::TextFile { line: 1, col: 1 },
                    },
                    vector,
                )],
            },
            recipe,
            None,
            None,
            true,
            false,
            Some(&format!("admission-{}", path.display())),
        )
        .unwrap();
}

/// The invariant the whole multi-space design exists to hold: one corpus
/// owns membership, its projections are internal, and a projection that
/// has not indexed the corpus's current membership cannot be routed to.
#[tokio::test]
async fn a_projection_is_internal_and_may_not_serve_membership_it_lacks() {
    let dir = tempfile::tempdir().unwrap();
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) = WorkspaceManager::new(
        dir.path().to_path_buf(),
        dir.path().join("global-settings.json"),
        Arc::clone(&events),
    )
    .unwrap();
    let embedding = SelectedEmbedder {
        engine: EmbeddingEngine::Candle,
        model: EmbedderModel("primary-model".to_string()),
        dimension: 2,
    };
    let corpus = manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-spaces".to_string(),
            embedding: embedding.clone(),
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .unwrap();
    let corpus_id = corpus.corpus_id.clone();
    let canonical_root = workspace_root(dir.path(), &corpus_id);
    let canonical_sources = canonical_root.join("managed_sources");
    let recipe = wilkes_core::embed::ExtractionRecipe::new(600, 128);
    let document = canonical_sources.join("document.txt");
    let mut canonical = SemanticIndex::create(
        &canonical_root,
        embedding.model.model_id(),
        embedding.dimension,
        embedding.engine,
        Some(&canonical_sources),
    )
    .unwrap();
    admit(
        &mut canonical,
        &document,
        "canonical passage",
        vec![1.0, 0.0],
        &recipe,
    );

    // The projection's manifest is created by the same code the ensure
    // endpoint uses; only its vectors are supplied here, because computing
    // them for real needs a model this test cannot download.
    let secondary = SelectedEmbedder {
        engine: EmbeddingEngine::Candle,
        model: EmbedderModel("secondary-model".to_string()),
        dimension: 2,
    };
    let manifest = read_manifest(&workspace_manifest_path(dir.path(), &corpus_id)).unwrap();
    let projection_id = manager
        .ensure_projection_workspace(
            &corpus_id,
            "underdog",
            "store-spaces",
            manifest.semantic.as_ref().unwrap(),
            &secondary,
        )
        .unwrap();
    assert_eq!(
        manager
            .ensure_projection_workspace(
                &corpus_id,
                "underdog",
                "store-spaces",
                manifest.semantic.as_ref().unwrap(),
                &secondary,
            )
            .unwrap(),
        projection_id,
        "one embedder under one corpus is one projection, however often it is ensured"
    );
    let projection_root = workspace_root(dir.path(), &projection_id);
    let projection_sources = projection_root.join("managed_sources");
    let mut projection = SemanticIndex::create(
        &projection_root,
        secondary.model.model_id(),
        secondary.dimension,
        secondary.engine,
        Some(&projection_sources),
    )
    .unwrap();

    // A projection is an implementation detail: it never appears beside
    // the user's own workspaces, and it is not a corpus of its own.
    let state = manager.state().await.unwrap();
    assert!(
        !state
            .workspaces
            .iter()
            .any(|workspace| workspace.id == projection_id),
        "an embedding projection is not a workspace the user has"
    );

    let status = manager.managed_workspace_status(&corpus_id).await.unwrap();
    let projection_space = status
        .spaces
        .iter()
        .find(|space| space.workspace_id == projection_id)
        .expect("the projection is listed as a space of its corpus")
        .clone();
    // Every space is a projection now: the canonical corpus contributes
    // none, because it holds no vectors to name one with.
    assert!(
        status.spaces.iter().all(|space| !space.primary),
        "the canonical corpus is not one of its own spaces"
    );
    assert!(
        status
            .spaces
            .iter()
            .all(|space| space.workspace_id != corpus_id),
        "the corpus does not appear in its own space list"
    );
    assert_ne!(
        projection_space.indexed_generation, status.corpus_generation,
        "an empty projection has not indexed the document the corpus admitted"
    );
    assert!(!projection_space.ready);

    // Refused by generation, not by absence: the space exists, is named
    // correctly, and still may not answer a query.
    let error = manager
        .managed_space_context(&corpus_id, &projection_space.embedding_space_id)
        .await
        .err()
        .expect("a projection behind the corpus must not serve");
    assert!(
        format!("{error:#}").contains("EMBEDDING_SPACE_STALE"),
        "{error:#}"
    );

    // Catching up is the canonical rendition re-embedded, not re-extracted:
    // the chunk structure comes from the corpus, only the coordinates are
    // the projection's own.
    let (mut prepared, canonical_recipe_id) = canonical
        .managed_file_structure_for_reembedding(&document, &document)
        .unwrap()
        .expect("the canonical rendition is admitted");
    assert_eq!(
        canonical_recipe_id,
        recipe.id(),
        "the recipe comes from the rendition, not from the projection"
    );
    prepared.chunks[0].1 = vec![0.0, 1.0];
    projection
        .write_file_with_recipe_id(
            prepared,
            &canonical_recipe_id,
            None,
            None,
            true,
            false,
            Some("projected"),
        )
        .unwrap();

    let status = manager.managed_workspace_status(&corpus_id).await.unwrap();
    let caught_up = status
        .spaces
        .iter()
        .find(|space| space.workspace_id == projection_id)
        .unwrap();
    assert_eq!(caught_up.indexed_generation, status.corpus_generation);
    assert!(caught_up.ready);
    // Two models are two coordinate systems under one corpus. Compared
    // against the embedders themselves rather than against a canonical
    // space, which no longer exists to compare with.
    let space_of = |selected: &SelectedEmbedder| {
        wilkes_core::embed::identity::EmbeddingSpaceIdentity::for_test(
            selected.engine,
            selected.model.model_id(),
            selected.dimension,
        )
        .id()
        .0
    };
    assert_eq!(caught_up.embedding_space_id, space_of(&secondary));
    assert_ne!(caught_up.embedding_space_id, space_of(&embedding));
    assert!(
        manager
            .managed_space_context(&corpus_id, &caught_up.embedding_space_id)
            .await
            .is_ok(),
        "a projection at the corpus generation serves"
    );

    // Admitting one more document puts the corpus ahead again, and the
    // projection stops serving until it follows.
    admit(
        &mut canonical,
        &canonical_sources.join("second.txt"),
        "a second canonical passage",
        vec![0.5, 0.5],
        &recipe,
    );
    let status = manager.managed_workspace_status(&corpus_id).await.unwrap();
    let lagging = status
        .spaces
        .iter()
        .find(|space| space.workspace_id == projection_id)
        .unwrap();
    assert!(!lagging.ready);
    assert!(
        manager
            .managed_space_context(&corpus_id, &lagging.embedding_space_id)
            .await
            .is_err(),
        "membership that only the corpus has is membership no projection may answer for"
    );

    // A projection that is behind is work owed, and the pass that owes it
    // reports per space: this environment has no embedder to load, so the
    // lagging space comes back as a named failure rather than as an error
    // that would have stopped every other space from catching up too.
    let failures = manager.catch_up_corpus(&corpus_id, None).await.unwrap();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].0, lagging.embedding_space_id);
}

/// Ensuring a space that is already level is a status read and nothing
/// else.
///
/// The endpoint behind this is idempotent and callers put to it freely —
/// on a timer, in one case. Every call used to sweep the corpus, offering
/// each admitted source to the projection in turn, and the projection took
/// nothing from any of them. This environment can load no model, so a
/// sweep is an error: `Ok` here is the proof that none was attempted.
#[tokio::test]
async fn ensuring_a_level_space_sweeps_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) = WorkspaceManager::new(
        dir.path().to_path_buf(),
        dir.path().join("global-settings.json"),
        Arc::clone(&events),
    )
    .unwrap();
    let embedding = SelectedEmbedder {
        engine: EmbeddingEngine::Candle,
        model: EmbedderModel("primary-model".to_string()),
        dimension: 2,
    };
    let corpus = manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-ensure-level".to_string(),
            embedding: embedding.clone(),
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .unwrap();
    let corpus_id = corpus.corpus_id.clone();
    let canonical_root = workspace_root(dir.path(), &corpus_id);
    let canonical_sources = canonical_root.join("managed_sources");
    let recipe = wilkes_core::embed::ExtractionRecipe::new(600, 128);
    let document = canonical_sources.join("document.txt");
    let mut canonical = SemanticIndex::create(
        &canonical_root,
        embedding.model.model_id(),
        embedding.dimension,
        embedding.engine,
        Some(&canonical_sources),
    )
    .unwrap();
    admit(
        &mut canonical,
        &document,
        "canonical passage",
        vec![1.0, 0.0],
        &recipe,
    );

    // The projection is built and brought level by hand, because computing
    // its vectors for real needs a model this test cannot download.
    let secondary = SelectedEmbedder {
        engine: EmbeddingEngine::Candle,
        model: EmbedderModel("secondary-model".to_string()),
        dimension: 2,
    };
    let manifest = read_manifest(&workspace_manifest_path(dir.path(), &corpus_id)).unwrap();
    let projection_id = manager
        .ensure_projection_workspace(
            &corpus_id,
            "underdog",
            "store-ensure-level",
            manifest.semantic.as_ref().unwrap(),
            &secondary,
        )
        .unwrap();
    let projection_root = workspace_root(dir.path(), &projection_id);
    let mut projection = SemanticIndex::create(
        &projection_root,
        secondary.model.model_id(),
        secondary.dimension,
        secondary.engine,
        Some(&projection_root.join("managed_sources")),
    )
    .unwrap();
    let (mut prepared, canonical_recipe_id) = canonical
        .managed_file_structure_for_reembedding(&document, &document)
        .unwrap()
        .expect("the canonical rendition is admitted");
    prepared.chunks[0].1 = vec![0.0, 1.0];
    projection
        .write_file_with_recipe_id(
            prepared,
            &canonical_recipe_id,
            None,
            None,
            true,
            false,
            Some("projected"),
        )
        .unwrap();

    let ensured = manager
        .ensure_managed_space(EnsureManagedEmbeddingSpace {
            corpus_id: corpus_id.clone(),
            embedding: secondary.clone(),
        })
        .await
        .expect("a level space is ensured without catching anything up");
    assert_eq!(ensured.workspace_id, projection_id);
    assert!(ensured.ready);
    let status = manager.managed_workspace_status(&corpus_id).await.unwrap();
    assert_eq!(ensured.indexed_generation, status.corpus_generation);

    // And a space that is genuinely behind still owes the sweep, which
    // this environment cannot perform: the skip is the level check, not a
    // blanket refusal to catch up.
    admit(
        &mut canonical,
        &canonical_sources.join("second.txt"),
        "a second canonical passage",
        vec![0.5, 0.5],
        &recipe,
    );
    assert!(
        manager
            .ensure_managed_space(EnsureManagedEmbeddingSpace {
                corpus_id: corpus_id.clone(),
                embedding: secondary,
            })
            .await
            .is_err(),
        "a projection behind its corpus is work owed, not work skipped"
    );
}

#[test]
fn only_spaces_level_with_the_corpus_are_named_as_holding_it() {
    let space = |id: &str, ready: bool, generation: &str| ManagedEmbeddingSpaceStatus {
        embedding_space_id: id.to_string(),
        embedding_space_identity: EmbeddingSpaceIdentity::for_test(
            wilkes_core::types::EmbeddingEngine::Candle,
            id,
            2,
        ),
        ready,
        indexed_generation: generation.to_string(),
        workspace_id: format!("workspace-{id}"),
        primary: false,
    };
    let spaces = ManagedSpaces {
        corpus_generation: "now".to_string(),
        spaces: vec![
            space("space-level", true, "now"),
            space("space-behind", true, "before"),
            space("space-embedding", false, "now"),
        ],
    };
    assert_eq!(spaces.level_space_ids(), vec!["space-level".to_string()]);
}

/// A first-time consumer creates a corpus, imports into it and searches,
/// and is refused because searching needs a projection it was never told
/// about. The refusal is where it learns that, so it says so.
#[tokio::test]
async fn a_pin_the_corpus_lacks_is_refused_with_the_step_that_creates_one() {
    let dir = tempfile::tempdir().unwrap();
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) = WorkspaceManager::new(
        dir.path().to_path_buf(),
        dir.path().join("global-settings.json"),
        Arc::clone(&events),
    )
    .unwrap();
    let corpus = manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-no-space".to_string(),
            embedding: SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("primary-model".to_string()),
                dimension: 2,
            },
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .unwrap();
    let error = manager
        .managed_space_context(&corpus.corpus_id, "space-the-caller-imagined")
        .await
        .err()
        .expect("a space the corpus does not hold must not serve");
    let message = format!("{error:#}");
    assert!(message.contains("EMBEDDING_SPACE_MISMATCH"), "{message}");
    assert!(
        message.contains("corpus=none, request=space-the-caller-imagined"),
        "{message}"
    );
    assert!(message.contains("PUT /api/corpora/spaces"), "{message}");
}

/// A projection build belongs to the manager, not to the request that
/// asked for it. A client that gave up used to take the build with it —
/// half a corpus embedded, nothing to resume it, and a status that showed
/// neither a build nor a failure. The build now runs to its end whoever
/// is still waiting, is counted among the corpus's pending builds while it
/// does, and a second caller waits for it rather than starting another.
#[tokio::test]
async fn a_projection_build_outlives_the_request_that_started_it() {
    let dir = tempfile::tempdir().unwrap();
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) = WorkspaceManager::new(
        dir.path().to_path_buf(),
        dir.path().join("global-settings.json"),
        Arc::clone(&events),
    )
    .unwrap();
    let corpus = manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-detached-build".to_string(),
            embedding: SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("primary-model".to_string()),
                dimension: 2,
            },
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .unwrap();
    let corpus_id = corpus.corpus_id.clone();
    let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let finished = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (release, released) = tokio::sync::watch::channel(false);
    let job = {
        let started = Arc::clone(&started);
        let finished = Arc::clone(&finished);
        move || {
            let started = Arc::clone(&started);
            let finished = Arc::clone(&finished);
            let mut released = released.clone();
            async move {
                started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                released
                    .wait_for(|go| *go)
                    .await
                    .map_err(|e| e.to_string())?;
                finished.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        }
    };

    let request = {
        let manager = Arc::clone(&manager);
        let corpus_id = corpus_id.clone();
        let job = job.clone();
        tokio::spawn(async move {
            manager
                .run_projection_build(&corpus_id, "projection-a", job)
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while started.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the build started");
    assert_eq!(manager.projection_builds_in_flight(&corpus_id), 1);
    assert_eq!(manager.projection_builds_in_flight("another-corpus"), 0);

    // The client gives up.
    request.abort();
    let _ = request.await;
    assert_eq!(
        manager.projection_builds_in_flight(&corpus_id),
        1,
        "the build went with the request that started it"
    );

    // A second caller joins the build in flight instead of starting one.
    let second = {
        let manager = Arc::clone(&manager);
        let corpus_id = corpus_id.clone();
        let job = job.clone();
        tokio::spawn(async move {
            manager
                .run_projection_build(&corpus_id, "projection-a", job)
                .await
        })
    };
    tokio::task::yield_now().await;
    assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 1);

    release.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), second)
        .await
        .expect("the second caller was answered")
        .unwrap()
        .unwrap();
    assert!(
        finished.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "the abandoned build ran to its end"
    );
    assert_eq!(manager.projection_builds_in_flight(&corpus_id), 0);
}

/// The cheap path has to stay cheap, because it is the one that runs after
/// every import: a corpus whose projections are all level must decide that
/// from the membership digests alone, without loading a single model.
#[tokio::test]
async fn catching_up_a_level_corpus_loads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) = WorkspaceManager::new(
        dir.path().to_path_buf(),
        dir.path().join("global-settings.json"),
        Arc::clone(&events),
    )
    .unwrap();
    let embedding = SelectedEmbedder {
        engine: EmbeddingEngine::Candle,
        model: EmbedderModel("primary-model".to_string()),
        dimension: 2,
    };
    let corpus = manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-level".to_string(),
            embedding: embedding.clone(),
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .unwrap();
    let root = workspace_root(dir.path(), &corpus.corpus_id);
    let recipe = wilkes_core::embed::ExtractionRecipe::new(600, 128);
    let mut canonical = SemanticIndex::create(
        &root,
        embedding.model.model_id(),
        embedding.dimension,
        embedding.engine,
        Some(&root.join("managed_sources")),
    )
    .unwrap();
    admit(
        &mut canonical,
        &root.join("managed_sources").join("document.txt"),
        "canonical passage",
        vec![1.0, 0.0],
        &recipe,
    );
    // No projection exists, so nothing is behind — and a corpus with one
    // that is level takes the same path.
    assert!(manager
        .catch_up_corpus(&corpus.corpus_id, None)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn managed_backup_restores_over_only_an_empty_same_store_corpus() {
    let source = tempfile::tempdir().unwrap();
    let source_settings = source.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (source_manager, _events, _worker_loop) = WorkspaceManager::new(
        source.path().to_path_buf(),
        source_settings,
        Arc::clone(&events),
    )
    .unwrap();
    let embedding = SelectedEmbedder::default();
    let source_status = source_manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-restore".to_string(),
            embedding: embedding.clone(),
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .unwrap();
    let source_root = workspace_root(source.path(), &source_status.corpus_id);
    std::fs::write(
        source_root.join("managed_sources").join("retained.txt"),
        b"retained across restore",
    )
    .unwrap();
    let index = SemanticIndex::create(
        &source_root,
        embedding.model.model_id(),
        embedding.dimension,
        embedding.engine,
        None,
    )
    .unwrap();
    let space = index.embedding_space_identity().unwrap().id().0;
    drop(index);
    let source_backup = source.path().join("managed_backups").join("exported");
    crate::context::backup_managed_directory(
        &source_root,
        &source_status.corpus_id,
        &space,
        &source_backup,
    )
    .unwrap();

    let target = tempfile::tempdir().unwrap();
    let (target_manager, _events, _worker_loop) = WorkspaceManager::new(
        target.path().to_path_buf(),
        target.path().join("global-settings.json"),
        events,
    )
    .unwrap();
    let empty = target_manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-restore".to_string(),
            embedding,
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .unwrap();
    assert_ne!(empty.corpus_id, source_status.corpus_id);
    let target_backup = target.path().join("managed_backups").join("inbox");
    std::fs::create_dir_all(target_backup.join("managed_sources")).unwrap();
    for name in [
        "workspace.json",
        "semantic_index.db",
        "backup-manifest.json",
    ] {
        std::fs::copy(source_backup.join(name), target_backup.join(name)).unwrap();
    }
    std::fs::copy(
        source_backup.join("managed_sources").join("retained.txt"),
        target_backup.join("managed_sources").join("retained.txt"),
    )
    .unwrap();
    std::fs::write(target_backup.join("unlisted.txt"), b"not in manifest").unwrap();
    let inventory_error = target_manager
        .restore_managed_workspace("inbox", &source_status.corpus_id, &space, "store-restore")
        .await
        .unwrap_err();
    assert!(inventory_error
        .to_string()
        .contains("contents do not match the manifest inventory"));
    std::fs::remove_file(target_backup.join("unlisted.txt")).unwrap();

    let restored = target_manager
        .restore_managed_workspace("inbox", &source_status.corpus_id, &space, "store-restore")
        .await
        .unwrap();
    assert_eq!(restored.corpus_id, source_status.corpus_id);
    // The restored corpus names no space, like every canonical corpus: the
    // backup carries its passages and their identity, which is what it is
    // for. `space` is still what addresses the backup itself.
    assert_eq!(restored.embedding_space_id, None);
    assert!(restored.ready);
    // Restore replaces the pre-created corpus rather than adding a second
    // one: the listing shows the user's own workspace and exactly one
    // managed corpus.
    let listed = target_manager.state().await.unwrap().workspaces;
    assert_eq!(listed.len(), 2);
    assert_eq!(
        listed
            .iter()
            .filter(|workspace| workspace.read_only)
            .count(),
        1
    );
    let retried = target_manager
        .restore_managed_workspace("inbox", &source_status.corpus_id, &space, "store-restore")
        .await
        .unwrap();
    assert_eq!(retried.corpus_id, restored.corpus_id);
    assert!(retried.ready);
    assert_eq!(
        std::fs::read(
            workspace_root(target.path(), &source_status.corpus_id)
                .join("managed_sources")
                .join("retained.txt")
        )
        .unwrap(),
        b"retained across restore"
    );
    source_manager.shutdown_all().await;
    target_manager.shutdown_all().await;
}

#[test]
fn a_workspace_event_reaches_only_the_view_showing_it() {
    let views = Arc::new(RwLock::new(Views {
        default: "a".to_string(),
        shown: std::collections::HashMap::from([("main".to_string(), "a".to_string())]),
    }));
    let names = Arc::new(StdMutex::new(Vec::new()));
    let inner: Arc<dyn ViewEventSink> = Arc::new(RecordingEmitter(Arc::clone(&names)));
    let a = scoped_events("a", Arc::clone(&views), Arc::clone(&inner));
    let b = scoped_events("b", Arc::clone(&views), Arc::clone(&inner));
    let c = scoped_events("c", Arc::clone(&views), inner);

    a.emit("a-live", serde_json::Value::Null);
    // Nobody shows b yet: its events go nowhere.
    b.emit("b-hidden", serde_json::Value::Null);
    views.write().bind("second", "b");
    b.emit("b-shown", serde_json::Value::Null);
    // The second window is focused: b is now the default.
    views.write().default = "b".to_string();
    b.emit("b-default", serde_json::Value::Null);
    a.emit("a-still-shown", serde_json::Value::Null);
    c.emit("c-hidden", serde_json::Value::Null);

    assert_eq!(
        *names.lock().unwrap(),
        vec![
            "a-live@main*",
            "b-shown@second",
            "b-default@second*",
            "a-still-shown@main",
        ]
    );
}

#[tokio::test]
async fn fresh_registry_creates_one_empty_default_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("global-settings.json");
    let id = initialize_workspace_registry(dir.path(), &settings_path).unwrap();
    let state = read_workspace_state(dir.path(), &settings_path)
        .await
        .unwrap();
    assert_eq!(state.workspaces.len(), 1);
    assert_eq!(state.workspaces[0].name, "Default");
    assert!(state.workspaces[0].roots.is_empty());
    assert_eq!(state.active_workspace_id, id);
    assert_eq!(state.active_workspace_id, state.workspaces[0].id);
    // A workspace that declares no semantic block still reports what it
    // would embed with, inherited from global settings.
    assert_eq!(
        state.workspaces[0].embedding,
        wilkes_core::types::Settings::default().semantic.selected
    );
}

/// A managed corpus is listed, says it is read-only, and can be activated
/// so the user can search it. Only the writes stay refused.
#[tokio::test]
async fn managed_workspace_is_idempotent_listed_and_protected() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
    let request = EnsureManagedWorkspace {
        owner: "underdog".to_string(),
        corpus_key: "store-1".to_string(),
        embedding: SelectedEmbedder::default(),
        chunk_size: 600,
        chunk_overlap: 128,
    };
    let first = manager
        .ensure_managed_workspace(request.clone())
        .await
        .unwrap();
    let second = manager.ensure_managed_workspace(request).await.unwrap();
    assert_eq!(first.corpus_id, second.corpus_id);
    let state = manager.state().await.unwrap();
    assert_eq!(
        state.workspaces.len(),
        2,
        "the corpus is listed beside the user's own workspace"
    );
    let corpus = state
        .workspaces
        .iter()
        .find(|workspace| workspace.id == first.corpus_id)
        .expect("the managed corpus is listed");
    assert!(corpus.read_only);
    assert_eq!(corpus.managed_by.as_deref(), Some("underdog"));
    assert!(state
        .workspaces
        .iter()
        .any(|workspace| !workspace.read_only && workspace.managed_by.is_none()));

    assert!(manager
        .rename(&first.corpus_id, "Visible".to_string())
        .await
        .unwrap_err()
        .to_string()
        .contains("MANAGED_WORKSPACE_PROTECTED"));

    // Activation is what makes it searchable: every read path the UI has
    // answers for the active workspace.
    let switched = manager
        .switch_view(PRIMARY_VIEW, &first.corpus_id)
        .await
        .unwrap();
    assert_eq!(switched.active_workspace_id, first.corpus_id);
    assert!(manager.active().is_read_only());
    assert_eq!(
        manager.active().ensure_writable().unwrap_err().code(),
        Some(wilkes_core::consumer::ConsumerErrorCode::ManagedWorkspaceProtected)
    );

    // A different embedder is not a mismatch. The canonical corpus holds
    // no vectors, so its recorded embedder decides nothing about what a
    // consumer may ask for; models are chosen per projection. Comparing it
    // here meant this endpoint refused, for the life of the corpus, any
    // request naming a model other than the one it happened to be created
    // with — and a consumer has only its live setting to send.
    let mut other_model = SelectedEmbedder::default();
    other_model.dimension += 1;
    manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-1".to_string(),
            embedding: other_model,
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .expect("a different model is a different projection, not a refusal");

    // Different chunking still is a mismatch, and always will be: every
    // `chunk_ref` a consumer has cited derives from it, and re-chunking is
    // the one thing here nobody can recompute their way out of.
    assert!(manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-1".to_string(),
            embedding: SelectedEmbedder::default(),
            chunk_size: 900,
            chunk_overlap: 128,
        })
        .await
        .unwrap_err()
        .to_string()
        .contains("MANAGED_WORKSPACE_CONFIGURATION_MISMATCH"));
    manager.shutdown_all().await;
}

/// The whole alpha library — its databases, their companion files and the
/// roots the user had open — arrives in one Default workspace, and the
/// global settings stop being a second answer to what is open.
#[tokio::test]
async fn an_alpha_library_is_adopted_into_a_default_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let library = library.canonicalize().unwrap();
    for entry in ["research.db", "semantic_index.db"] {
        rusqlite::Connection::open(dir.path().join(entry))
            .unwrap()
            .execute_batch("CREATE TABLE fixture (id INTEGER PRIMARY KEY);")
            .unwrap();
    }
    std::fs::create_dir_all(dir.path().join("uploads")).unwrap();
    std::fs::write(dir.path().join("uploads").join("paper.pdf"), b"pdf").unwrap();
    let mut legacy_settings =
        serde_json::to_value(wilkes_core::types::Settings::default()).unwrap();
    legacy_settings["favorites"] = serde_json::json!([&library]);
    legacy_settings["last_directory"] = serde_json::json!(&library);
    legacy_settings["semantic"]["chunk_size"] = serde_json::json!(777);
    legacy_settings["theme"] = serde_json::json!("Dark");
    std::fs::write(
        &settings_path,
        serde_json::to_vec(&legacy_settings).unwrap(),
    )
    .unwrap();

    let id = initialize_workspace_registry(dir.path(), &settings_path).unwrap();

    let workspace_dir = workspace_root(dir.path(), &id);
    for entry in ["research.db", "semantic_index.db"] {
        assert!(workspace_dir.join(entry).exists(), "{entry} did not move");
        assert!(!dir.path().join(entry).exists(), "{entry} was left behind");
    }
    assert!(workspace_dir.join("uploads").join("paper.pdf").exists());
    assert!(!dir.path().join(".workspace-migration.json").exists());

    let state = read_workspace_state(dir.path(), &settings_path)
        .await
        .unwrap();
    assert_eq!(state.active_workspace_id, id);
    assert_eq!(state.workspaces.len(), 1);
    assert_eq!(state.workspaces[0].name, "Default");
    assert_eq!(state.workspaces[0].roots, vec![library.clone()]);
    assert_eq!(state.workspaces[0].active_root, Some(library));

    let manifest = read_manifest(&workspace_manifest_path(dir.path(), &id)).unwrap();
    assert_eq!(manifest.semantic.unwrap().chunk_size, 777);

    let settings: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
    assert_eq!(settings.get("theme").unwrap(), "Dark");
    for key in ["favorites", "last_directory", "semantic"] {
        assert!(settings.get(key).is_none(), "{key} was left in settings");
    }
}

/// A migration killed between two moves is finished by the next start,
/// into the workspace it had already committed to.
#[test]
fn an_interrupted_migration_resumes_into_the_same_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let id = "11111111-2222-3333-4444-555555555555".to_string();
    let mut manifest = WorkspaceManifest::new(id.clone(), "Default".to_string());
    manifest.active_root = Some(dir.path().join("library"));
    let workspace_dir = workspace_root(dir.path(), &id);
    std::fs::create_dir_all(&workspace_dir).unwrap();
    // Half moved before the interruption, half still in place.
    std::fs::write(workspace_dir.join("research.db"), b"legacy").unwrap();
    std::fs::write(dir.path().join("semantic_index.db"), b"legacy").unwrap();
    atomic_write_json(
        &migration_plan_path(dir.path()),
        // The journal shape written before upload relocation was added.
        &serde_json::json!({ "workspace_id": &id, "manifest": manifest }),
    )
    .unwrap();

    let resumed = initialize_workspace_registry(dir.path(), &settings_path).unwrap();

    assert_eq!(resumed, id);
    assert!(workspace_dir.join("semantic_index.db").exists());
    assert!(!dir.path().join("semantic_index.db").exists());
    assert!(!dir.path().join(".workspace-migration.json").exists());
    assert_eq!(
        read_manifest(&workspace_manifest_path(dir.path(), &id))
            .unwrap()
            .active_root,
        Some(dir.path().join("library"))
    );
}

/// Two libraries and no way to tell which one the user means: the
/// migration stops rather than picking, and nothing has moved when it does.
#[test]
fn ambiguous_legacy_copies_refuse_to_migrate() {
    let data_dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let settings_path = config_dir.path().join("settings.json");
    std::fs::write(data_dir.path().join("research.db"), b"data").unwrap();
    std::fs::write(config_dir.path().join("research.db"), b"config").unwrap();

    let error = initialize_workspace_registry(data_dir.path(), &settings_path).unwrap_err();

    assert!(error.to_string().contains("refusing to choose"));
    assert!(!data_dir.path().join("workspaces.json").exists());
    assert!(data_dir.path().join("research.db").exists());
    assert!(config_dir.path().join("research.db").exists());
}

/// The manager starts on an alpha install instead of refusing to: the
/// adoption is the first thing it does.
#[tokio::test]
async fn the_manager_starts_on_an_alpha_install() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    std::fs::write(dir.path().join("research.db"), b"legacy").unwrap();
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);

    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();

    let state = manager.state().await.unwrap();
    assert_eq!(state.workspaces.len(), 1);
    assert!(workspace_root(dir.path(), &state.active_workspace_id)
        .join("research.db")
        .exists());
    manager.active().shutdown().await;
}

#[tokio::test]
async fn renaming_a_workspace_persists_its_manifest_name() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
    let id = manager.state().await.unwrap().active_workspace_id;

    let renamed = manager
        .rename(&id, "  Research  ".to_string())
        .await
        .unwrap();

    assert_eq!(renamed.name, "Research");
    assert_eq!(
        manager.state().await.unwrap().workspaces[0].name,
        "Research"
    );
    assert_eq!(
        read_manifest(&workspace_manifest_path(dir.path(), &id))
            .unwrap()
            .name,
        "Research"
    );
    manager.active().shutdown().await;
}

#[tokio::test]
async fn switching_restores_workspace_roots_and_database_directory() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
    let first = manager.state().await.unwrap().active_workspace_id;
    let first_root = dir.path().join("library-a");
    std::fs::create_dir_all(&first_root).unwrap();
    let first_root = first_root.canonicalize().unwrap();
    manager
        .active()
        .update_settings(serde_json::json!({
            "favorites": [first_root.clone()],
            "last_directory": first_root.clone(),
            "semantic": {
                "enabled": false,
                "index_path": null,
                "chunk_size": 777,
            },
        }))
        .await
        .unwrap();
    std::fs::write(
        manager.active().data_dir.join("research.db"),
        b"workspace-a",
    )
    .unwrap();
    std::fs::write(
        manager.active().data_dir.join("semantic_index.db"),
        b"workspace-a-index",
    )
    .unwrap();

    let second = manager.create("Second".to_string()).await.unwrap();
    manager.switch_view(PRIMARY_VIEW, &second.id).await.unwrap();
    assert!(manager.active().get_settings().await.favorites.is_empty());
    assert!(!manager.active().data_dir.join("research.db").exists());
    assert!(!manager.active().data_dir.join("semantic_index.db").exists());

    manager.switch_view(PRIMARY_VIEW, &first).await.unwrap();
    let restored = manager.active().get_settings().await;
    assert_eq!(restored.favorites, vec![first_root.clone()]);
    assert_eq!(restored.last_directory, Some(first_root));
    assert_eq!(restored.semantic.chunk_size, 777);
    assert_eq!(
        std::fs::read(manager.active().data_dir.join("research.db")).unwrap(),
        b"workspace-a"
    );
    assert_eq!(
        std::fs::read(manager.active().data_dir.join("semantic_index.db")).unwrap(),
        b"workspace-a-index"
    );
    manager.active().shutdown().await;
}

#[tokio::test]
async fn a_non_active_workspace_is_reachable_without_activating_it() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
    let first = manager.state().await.unwrap().active_workspace_id;
    let second = manager.create("Second".to_string()).await.unwrap();

    // The point of the whole mechanism: a context for a workspace nobody
    // activated, reading that workspace's own directory.
    let scoped = manager.context_for(&second.id).await.unwrap();
    assert_eq!(scoped.data_dir, workspace_root(dir.path(), &second.id));
    assert_ne!(scoped.data_dir, manager.active().data_dir);

    // And nothing moved: the registry still names the first workspace, and
    // the active context is the one it always was.
    assert_eq!(
        manager.state().await.unwrap().active_workspace_id,
        first,
        "opening a workspace must not activate it"
    );
    assert_eq!(
        manager.active().data_dir,
        workspace_root(dir.path(), &first)
    );

    // Asked twice, the same context — the index handles are opened once.
    let again = manager.context_for(&second.id).await.unwrap();
    assert!(Arc::ptr_eq(&scoped, &again));

    // Naming the active workspace is not a second context for it: that
    // would put two contexts on one set of databases.
    let active = manager.context_for(&first).await.unwrap();
    assert!(Arc::ptr_eq(&active, &manager.active()));

    // Showing the workspace takes on the context already open for it —
    // one context per workspace — and retires the one it leaves.
    manager.switch_view(PRIMARY_VIEW, &second.id).await.unwrap();
    assert!(Arc::ptr_eq(&manager.active(), &scoped));
    assert!(active.is_shutting_down());
    assert_eq!(
        manager.active().data_dir,
        workspace_root(dir.path(), &second.id)
    );
    assert_eq!(
        manager.contexts.lock().keys().cloned().collect::<Vec<_>>(),
        vec![second.id.clone()]
    );

    manager.active().shutdown().await;
}

#[tokio::test]
async fn shutting_down_releases_every_workspace_this_manager_opened() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
    let second = manager.create("Second".to_string()).await.unwrap();
    let scoped = manager.context_for(&second.id).await.unwrap();

    manager.shutdown_all().await;

    // Both, not just the active one: a context opened for another
    // workspace owns workers and watchers of its own.
    assert!(scoped.is_shutting_down());
    assert!(manager.active().is_shutting_down());
    // The default's context stays as the one a late request is given.
    assert_eq!(manager.contexts.lock().len(), 1);
}

#[tokio::test]
async fn an_unknown_workspace_is_an_error_rather_than_a_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();

    let error = match manager.context_for("no-such-workspace").await {
        Ok(_) => panic!("an unknown workspace must not open a context"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("Unknown workspace"));

    manager.active().shutdown().await;
}

#[tokio::test]
async fn the_catalog_lists_every_workspace_and_resolves_each_without_switching() {
    use wilkes_agent::search::WorkspaceCatalog;

    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
    let active_id = manager.state().await.unwrap().active_workspace_id;
    let second = manager.create("Second".to_string()).await.unwrap();

    let listed = manager.workspaces().await.unwrap();
    assert_eq!(listed.len(), 2);
    let active = listed
        .iter()
        .find(|workspace| workspace.active)
        .expect("exactly one workspace is active");
    assert_eq!(active.id, active_id);
    assert!(listed
        .iter()
        .any(|workspace| workspace.id == second.id && !workspace.active));

    // An unnamed call reads the active workspace; a named one reads that
    // workspace and leaves the active one where it was.
    let unnamed = manager.search_for(None).await.unwrap();
    assert!(
        std::ptr::addr_eq(
            Arc::as_ptr(&unnamed).cast::<u8>(),
            Arc::as_ptr(&manager.active()).cast::<u8>()
        ),
        "an unnamed call must read the active workspace's own context"
    );
    let named = manager.search_for(Some(&second.id)).await.unwrap();
    assert!(!std::ptr::addr_eq(
        Arc::as_ptr(&named).cast::<u8>(),
        Arc::as_ptr(&manager.active()).cast::<u8>()
    ));
    assert_eq!(
        manager.state().await.unwrap().active_workspace_id,
        active_id,
        "resolving a workspace must not activate it"
    );

    let error = manager.search_for(Some("no-such-workspace")).await;
    assert!(error
        .err()
        .is_some_and(|error| error.contains("no-such-workspace")));

    manager.shutdown_all().await;
}

/// The scope table, at the resolver rather than at each route.
///
/// One object addresses every consumer request, so the cases a caller can
/// put to it are worth stating once, here, rather than being discovered
/// per route.
#[tokio::test]
async fn an_unnamed_scope_answers_from_the_active_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
    let active = manager.state().await.unwrap().active_workspace_id;

    let index = manager
        .consumer_index(&ConsumerScope::default())
        .await
        .unwrap();

    assert_eq!(index.context().data_dir, manager.active().data_dir);
    assert_eq!(manager.active_workspace_id().unwrap(), active);
    // No index has been built, so there is no space to report — and a
    // route that is about to name passages says why rather than
    // returning an empty answer that reads like a complete one.
    assert_eq!(index.embedding_space_id(), None);
    assert!(index.addressable_space_id().is_err());

    manager.shutdown_all().await;
}

#[tokio::test]
async fn a_pin_no_index_can_prove_is_refused_rather_than_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();

    let error = manager
        .consumer_index(&ConsumerScope {
            workspace_id: None,
            expected_embedding_space_id: Some("space-the-caller-imagined".to_string()),
        })
        .await
        .unwrap_err();

    assert_eq!(
        error.code(),
        Some(ConsumerErrorCode::EmbeddingSpaceMismatch)
    );
    manager.shutdown_all().await;
}

#[tokio::test]
async fn an_unknown_id_names_no_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();

    let error = manager
        .consumer_index(&ConsumerScope {
            workspace_id: Some("no-such-workspace".to_string()),
            expected_embedding_space_id: None,
        })
        .await
        .unwrap_err();

    assert_eq!(
        error.code(),
        Some(ConsumerErrorCode::ManagedWorkspaceNotFound)
    );
    manager.shutdown_all().await;
}

/// A corpus with nothing in it has no coordinate system to disagree
/// about; once it has one, a request that declines to name a space is
/// asking to be served whatever happens to be there, which is the thing
/// a pinned consumer surface exists to prevent.
/// A canonical corpus answers unpinned, because it names no space to
/// disagree about.
///
/// It used to stop answering the moment it held an index: the index's own
/// space became the corpus's, and every unpinned request was then refused
/// with `EMBEDDING_SPACE_MISMATCH: corpus=…, request=none`. That was a
/// coordinate system asserting itself over questions that never touch
/// coordinates — listing a library's files, resolving a passage's text —
/// and it is gone with the vectors. A projection still requires its pin,
/// because a projection is nothing but a space.
#[tokio::test]
async fn a_canonical_corpus_answers_unpinned_because_it_names_no_space() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
    let embedding = SelectedEmbedder::default();
    let corpus = manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-scope".to_string(),
            embedding: embedding.clone(),
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .unwrap();
    let scope = ConsumerScope {
        workspace_id: Some(corpus.corpus_id.clone()),
        expected_embedding_space_id: None,
    };

    assert!(manager.consumer_index(&scope).await.is_ok());

    let root = workspace_root(dir.path(), &corpus.corpus_id);
    let index = SemanticIndex::create(
        &root,
        embedding.model.model_id(),
        embedding.dimension,
        embedding.engine,
        None,
    )
    .unwrap();
    drop(index);

    // Still answers, and answers with no space, which is the truth about
    // what it holds.
    let resolved = manager
        .consumer_index(&scope)
        .await
        .expect("an unpinned canonical request is answerable");
    // Debug is where this type says which space it turned out to be, and
    // for a canonical corpus the answer is none.
    assert!(format!("{resolved:?}").contains("Absent"), "{resolved:?}");

    manager.shutdown_all().await;
}

/// A projection is how a corpus holds a second space, not a second thing
/// to address. Reachable directly, it would be a way to be handed vectors
/// without having named the space they are in.
#[tokio::test]
async fn an_internal_projection_is_not_addressable_as_a_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
    let embedding = SelectedEmbedder::default();
    let corpus = manager
        .ensure_managed_workspace(EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-projection".to_string(),
            embedding: embedding.clone(),
            chunk_size: 600,
            chunk_overlap: 128,
        })
        .await
        .unwrap();
    let parent_semantic = read_manifest(&workspace_manifest_path(dir.path(), &corpus.corpus_id))
        .unwrap()
        .semantic
        .unwrap();
    let projection = manager
        .ensure_projection_workspace(
            &corpus.corpus_id,
            "underdog",
            "store-projection",
            &parent_semantic,
            &embedding,
        )
        .unwrap();

    let error = manager
        .consumer_index(&ConsumerScope {
            workspace_id: Some(projection),
            expected_embedding_space_id: None,
        })
        .await
        .unwrap_err();

    assert_eq!(
        error.code(),
        Some(ConsumerErrorCode::ManagedWorkspaceNotFound)
    );
    manager.shutdown_all().await;
}

/// Switching away from a workspace frees its context, and with it every
/// database it opened.
///
/// The context used to be held for good by the task forwarding its worker
/// managers' events, so each switch kept the retired workspace's index,
/// metadata, journal and research databases open until the process ran
/// out of file descriptors.
#[tokio::test]
async fn switching_away_frees_the_retired_context() {
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("global-settings.json");
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
    let first = manager.state().await.unwrap().active_workspace_id;
    let second = manager.create("Second".to_string()).await.unwrap().id;
    manager.switch_view(PRIMARY_VIEW, &second).await.unwrap();
    // Open a database, as the interface does on arrival.
    manager.active().list_bookmarks().await.unwrap();
    let retired = Arc::downgrade(&manager.active());

    manager.switch_view(PRIMARY_VIEW, &first).await.unwrap();

    // Its restore task may still be finishing; nothing else should hold it.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while retired.strong_count() > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        retired.strong_count(),
        0,
        "the retired workspace's context is still alive after the switch"
    );
    manager.shutdown_all().await;
}

async fn manager_with_two_workspaces(dir: &Path) -> (Arc<WorkspaceManager>, String, String) {
    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) =
        WorkspaceManager::new(dir.to_path_buf(), dir.join("global-settings.json"), events).unwrap();
    let first = manager.state().await.unwrap().active_workspace_id;
    let second = manager.create("Second".to_string()).await.unwrap().id;
    (manager, first, second)
}

/// Two windows, two workspaces: each window's requests reach its own
/// workspace, and each window is told which one is its own.
#[tokio::test]
async fn two_views_show_two_workspaces_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let (manager, first, second) = manager_with_two_workspaces(dir.path()).await;

    manager.open_view("second-window", &second).await.unwrap();

    assert_eq!(
        manager.view_context(PRIMARY_VIEW).data_dir,
        workspace_root(dir.path(), &first)
    );
    assert_eq!(
        manager.view_context("second-window").data_dir,
        workspace_root(dir.path(), &second)
    );
    // A window that shows no workspace (the standalone reader) is
    // answered from the default, which opening a window does not move.
    assert!(Arc::ptr_eq(
        &manager.view_context("document"),
        &manager.active()
    ));
    assert_eq!(manager.active_workspace_id().unwrap(), first);

    let own = manager.state_for_view("second-window").await.unwrap();
    assert_eq!(own.active_workspace_id, second);
    assert_eq!(own.open_elsewhere, vec![first.clone()]);
    let main = manager.state_for_view(PRIMARY_VIEW).await.unwrap();
    assert_eq!(main.active_workspace_id, first);
    assert_eq!(main.open_elsewhere, vec![second.clone()]);

    manager.shutdown_all().await;
}

/// A workspace is shown by one window at most: the other window's own
/// editors and tabs for it would be written over.
#[tokio::test]
async fn a_workspace_shown_in_one_view_is_refused_to_another() {
    let dir = tempfile::tempdir().unwrap();
    let (manager, first, second) = manager_with_two_workspaces(dir.path()).await;
    manager.open_view("second-window", &second).await.unwrap();

    let error = manager
        .switch_view(PRIMARY_VIEW, &second)
        .await
        .unwrap_err();
    let shown = error.downcast_ref::<WorkspaceShownElsewhere>().unwrap();
    assert_eq!(shown.view, "second-window");
    let error = manager.open_view("third-window", &first).await.unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<WorkspaceShownElsewhere>()
            .unwrap()
            .view,
        PRIMARY_VIEW
    );
    // Nothing moved.
    assert_eq!(
        manager.view_workspace_id(PRIMARY_VIEW).as_deref(),
        Some(first.as_str())
    );
    assert_eq!(
        manager.view_showing(&second).as_deref(),
        Some("second-window")
    );

    manager.shutdown_all().await;
}

/// Focusing a window moves the default to its workspace; a workspace
/// nothing holds any more is shut down when its window closes.
#[tokio::test]
async fn focus_moves_the_default_and_closing_retires_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let (manager, first, second) = manager_with_two_workspaces(dir.path()).await;
    manager.open_view("second-window", &second).await.unwrap();
    let second_ctx = manager.view_context("second-window");

    manager.focus_view("second-window").await.unwrap();
    assert_eq!(manager.active_workspace_id().unwrap(), second);
    assert!(Arc::ptr_eq(&manager.active(), &second_ctx));
    // Still shown by the main window, so still open.
    let first_ctx = manager.view_context(PRIMARY_VIEW);
    assert!(!first_ctx.is_shutting_down());

    // The default's window closing leaves its context open: it is still
    // what a request naming no workspace is answered from.
    manager.close_view("second-window").await.unwrap();
    assert!(!second_ctx.is_shutting_down());
    assert_eq!(manager.view_showing(&second), None);

    // Until another window is focused.
    manager.focus_view(PRIMARY_VIEW).await.unwrap();
    assert!(second_ctx.is_shutting_down());
    assert!(!manager.contexts.lock().contains_key(&second));
    assert_eq!(manager.active_workspace_id().unwrap(), first);

    manager.shutdown_all().await;
}

/// A workspace a window shows cannot be deleted from under it, though it
/// is not the default.
#[tokio::test]
async fn a_workspace_shown_in_a_window_is_not_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let (manager, _first, second) = manager_with_two_workspaces(dir.path()).await;
    manager.open_view("second-window", &second).await.unwrap();

    let error = manager.delete(&second).await.unwrap_err().to_string();
    assert!(error.contains("open in a window"), "{error}");
    assert!(workspace_root(dir.path(), &second).exists());

    manager.close_view("second-window").await.unwrap();
    manager.delete(&second).await.unwrap();
    assert!(!workspace_root(dir.path(), &second).exists());

    manager.shutdown_all().await;
}

/// The default workspace is what the primary view shows at the next
/// launch; other windows are not reopened.
#[tokio::test]
async fn the_focused_workspace_is_shown_again_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (manager, _first, second) = manager_with_two_workspaces(dir.path()).await;
    manager.open_view("second-window", &second).await.unwrap();
    manager.focus_view("second-window").await.unwrap();
    manager.shutdown_all().await;
    drop(manager);

    let events: Arc<dyn ViewEventSink> = Arc::new(NoopEmitter);
    let (manager, _events, _worker_loop) = WorkspaceManager::new(
        dir.path().to_path_buf(),
        dir.path().join("global-settings.json"),
        events,
    )
    .unwrap();
    assert_eq!(manager.active_workspace_id().unwrap(), second);
    assert_eq!(
        manager.view_workspace_id(PRIMARY_VIEW).as_deref(),
        Some(second.as_str())
    );
    assert_eq!(manager.view_showing(&second).as_deref(), Some(PRIMARY_VIEW));

    manager.shutdown_all().await;
}
