use super::*;

#[test]
fn test_embed_operation_as_str() {
    assert_eq!(EmbedOperation::Download.as_str(), "Download");
    assert_eq!(EmbedOperation::Build.as_str(), "Build");
}

/// Continuing is over what was never reached. The failure is deliberately
/// not in it: a document that broke the reader breaks it again, and a
/// continuation that swept failures up would re-attempt it forever.
#[tokio::test]
async fn continuing_selects_only_the_documents_never_reached() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("corpus");
    let (_saved, _broke, never) = journal_a_stopped_job(&ctx, &root);

    let remaining = ctx
        .job_documents_with_outcome(&root, DocumentOutcome::Pending)
        .unwrap();
    assert_eq!(remaining.documents, vec![never]);
    assert!(
        remaining.continues.is_some(),
        "a continuation names the job it carries on from"
    );
}

/// Retrying is its own set, and its own act.
#[tokio::test]
async fn retrying_selects_only_the_documents_that_failed() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("corpus");
    let (_saved, broke, _never) = journal_a_stopped_job(&ctx, &root);

    let failed = ctx
        .job_documents_with_outcome(&root, DocumentOutcome::Failed)
        .unwrap();
    assert_eq!(failed.documents, vec![broke]);
    assert!(failed.continues.is_some());
}

/// The activity view reports the work that was saved, the failure with its
/// error, and the fact that there is more to do — after the process that
/// ran the job is gone.
#[tokio::test]
async fn activity_reports_saved_failed_and_remaining_after_the_job_ended() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("corpus");
    let (saved, broke, never) = journal_a_stopped_job(&ctx, &root);

    let activity = ctx.index_activity(root.clone()).await.unwrap();
    let job = activity.job.expect("the stopped job is reported");
    assert_eq!(job.state, JobState::Cancelled);
    assert!(job.state.stopped_early());
    assert_eq!(job.counts.saved(), 1);
    assert_eq!(job.counts.failed, 1);
    assert_eq!(job.counts.pending, 1);
    assert!(job.has_remaining_work());

    let by_path = |p: &Path| {
        activity
            .documents
            .iter()
            .find(|d| d.path == p)
            .unwrap_or_else(|| panic!("{} is missing from the activity view", p.display()))
            .clone()
    };
    assert_eq!(by_path(&saved).outcome, DocumentOutcome::Indexed);
    assert_eq!(by_path(&never).outcome, DocumentOutcome::Pending);
    let failed = by_path(&broke);
    assert_eq!(failed.outcome, DocumentOutcome::Failed);
    assert_eq!(
        failed.error.as_deref(),
        Some("mupdf: broken xref"),
        "the error is kept verbatim, not summarised away"
    );
}

/// Coverage is a claim about an index, so a workspace without one says so
/// rather than reporting every root as holding nothing — "not indexed" and
/// "there is no index" lead to different actions.
#[tokio::test]
async fn coverage_refuses_when_the_workspace_has_no_index() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("corpus");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), "hello").unwrap();

    let err = ctx.index_coverage(vec![root]).await.unwrap_err();
    assert!(err.contains("No semantic index found"), "{err}");
}

/// Neither action invents work. Offering "continue" or "retry" for a root
/// that has nothing to continue or retry is how a button that does nothing
/// gets shipped.
#[tokio::test]
async fn continuing_and_retrying_refuse_when_there_is_nothing_to_do() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("corpus");
    let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
    {
        let journal = ctx.job_journal().unwrap();
        let mut guard = journal.lock().unwrap();
        let job = guard.begin(&root, &[root.join("a.txt")]).unwrap();
        guard
            .note_outcome(
                job,
                &root.join("a.txt"),
                DocumentOutcome::Indexed,
                None,
                Some(1),
            )
            .unwrap();
        guard.finish(job, JobState::Completed, None).unwrap();
    }

    let err = Arc::clone(&ctx)
        .continue_index_job(root.to_string_lossy().into_owned(), selected.clone())
        .await
        .unwrap_err();
    assert!(err.contains("no remaining work"), "{err}");

    let err = Arc::clone(&ctx)
        .retry_failed_documents(root.to_string_lossy().into_owned(), selected)
        .await
        .unwrap_err();
    assert!(err.contains("No documents failed"), "{err}");
}

/// A partial build publishes into the index it finds, and an index built
/// with another model is not one it can publish into: it would replace
/// the whole index with its few documents. So it is refused up front.
#[tokio::test]
async fn a_partial_build_is_refused_when_the_index_has_another_model() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("corpus");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), "hello").unwrap();
    std::fs::create_dir_all(ctx.data_dir.join("models")).unwrap();
    index_root_with_mock_model(&ctx, &root).await;

    let same = SelectedEmbedder {
        engine: EmbeddingEngine::Candle,
        model: EmbedderModel("mock-model".to_string()),
        dimension: 384,
    };
    ctx.ensure_subset_build_matches_index(&root, &same)
        .await
        .expect("the model the index was built with may add to it");

    let other = SelectedEmbedder {
        model: EmbedderModel("other-model".to_string()),
        ..same.clone()
    };
    let err = ctx
        .ensure_subset_build_matches_index(&root, &other)
        .await
        .unwrap_err();
    assert!(err.contains("would replace the whole index"), "{err}");
    assert!(
        err.contains("mock-model") && err.contains("other-model"),
        "{err}"
    );
}

/// Nothing missing is nothing to do, said as such rather than started.
#[tokio::test]
async fn indexing_missing_documents_refuses_when_none_are_missing() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("corpus");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), "hello").unwrap();
    std::fs::create_dir_all(ctx.data_dir.join("models")).unwrap();
    index_root_with_mock_model(&ctx, &root).await;

    let err = Arc::clone(&ctx)
        .index_missing_documents(
            root.to_string_lossy().into_owned(),
            SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("mock-model".to_string()),
                dimension: 384,
            },
        )
        .await
        .unwrap_err();
    assert!(err.contains("already in the semantic index"), "{err}");
}

/// A root nobody has indexed has no job to continue, and says so rather
/// than starting one silently.
#[tokio::test]
async fn continuing_a_root_with_no_recorded_job_reports_that() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("never-indexed");
    let err = Arc::clone(&ctx)
        .continue_index_job(
            root.to_string_lossy().into_owned(),
            SelectedEmbedder::default_for(EmbeddingEngine::Candle),
        )
        .await
        .unwrap_err();
    assert!(err.contains("No indexing job has been recorded"), "{err}");
}

/// Deleting a root's index deletes the report about it. A history that
/// outlived its index would offer to continue into a database that is gone.
#[tokio::test]
async fn deleting_a_root_forgets_its_job_history() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("corpus");
    std::fs::create_dir_all(&root).unwrap();
    journal_a_stopped_job(&ctx, &root);
    assert!(ctx
        .index_activity(root.clone())
        .await
        .unwrap()
        .job
        .is_some());

    // The coverage has to exist for deleting it to mean anything.
    drop(
        SemanticIndex::create(
            &ctx.data_dir,
            "activity-test-model",
            2,
            EmbeddingEngine::Candle,
            Some(&root),
        )
        .unwrap(),
    );

    ctx.delete_index(Some(root.clone())).await.unwrap();
    assert!(
        ctx.index_activity(root.clone())
            .await
            .unwrap()
            .job
            .is_none(),
        "the job history goes with the coverage it describes"
    );
}

#[tokio::test]
async fn work_in_the_build_slot_is_stopped_by_cancel_within_the_deadline() {
    let (_dir, ctx) = test_ctx();
    let runner = Arc::clone(&ctx);
    let run = tokio::spawn(async move { runner.run_as_embed_task(until_cancelled).await });
    until_build_running(&ctx).await;

    let asked = std::time::Instant::now();
    ctx.cancel_embed().await;
    let result = tokio::time::timeout(CANCEL_DEADLINE, run)
        .await
        .expect("the cancelled work outlived the cancel deadline")
        .unwrap();
    assert!(result.is_err(), "cancelled work must not report success");
    assert!(asked.elapsed() < CANCEL_DEADLINE);
    tokio::time::timeout(CANCEL_DEADLINE, async {
        while ctx.embed_task_is_running() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the build slot was not released after the cancel");
}

#[tokio::test]
async fn work_waits_for_the_build_slot_rather_than_being_refused() {
    let (_dir, ctx) = test_ctx();
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let first_runner = Arc::clone(&ctx);
    let first = tokio::spawn(async move {
        first_runner
            .run_as_embed_task(|_| async move {
                let _ = released.await;
                Ok(())
            })
            .await
    });
    until_build_running(&ctx).await;

    let second_started = Arc::new(AtomicBool::new(false));
    let second_runner = Arc::clone(&ctx);
    let started = Arc::clone(&second_started);
    let second = tokio::spawn(async move {
        second_runner
            .run_as_embed_task(move |_| {
                started.store(true, Ordering::SeqCst);
                async { Ok(()) }
            })
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !second_started.load(Ordering::SeqCst),
        "the second build started while the first held the slot"
    );

    release.send(()).unwrap();
    first.await.unwrap().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), second)
        .await
        .expect("the second build never got the slot")
        .unwrap()
        .unwrap();
    assert!(second_started.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_caller_that_goes_away_cancels_its_build() {
    let (_dir, ctx) = test_ctx();
    let stopped = Arc::new(AtomicBool::new(false));
    let runner = Arc::clone(&ctx);
    let saw_flag = Arc::clone(&stopped);
    let run = tokio::spawn(async move {
        runner
            .run_as_embed_task(move |cancel_flag| async move {
                let result = until_cancelled(cancel_flag).await;
                saw_flag.store(true, Ordering::SeqCst);
                result
            })
            .await
    });
    until_build_running(&ctx).await;

    run.abort();
    tokio::time::timeout(CANCEL_DEADLINE, async {
        while !stopped.load(Ordering::SeqCst) || ctx.embed_task_is_running() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a build whose caller went away kept running");
}

#[tokio::test]
async fn test_embed_task_helpers_track_state() {
    let (_dir, ctx) = test_ctx();
    assert!(!ctx.embed_task_is_running());

    *ctx.embed_task.lock() = Some(running_embed_task());
    assert!(ctx.embed_task_is_running());

    ctx.clear_embed_task();
    assert!(!ctx.embed_task_is_running());
    assert!(ctx.embed_task.lock().is_none());
}

#[tokio::test]
async fn test_prepare_build_index_happy_path() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("file.txt"), "hello").unwrap();

    let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
    let plan = ctx
        .prepare_build_index(&root.to_string_lossy(), &selected, None)
        .await
        .unwrap();

    assert_eq!(plan.root_path, root);
    assert_eq!(
        plan.chunk_size,
        ctx.get_settings().await.semantic.chunk_size
    );
    assert_eq!(
        plan.chunk_overlap,
        ctx.get_settings().await.semantic.chunk_overlap
    );
    assert_eq!(
        plan.supported_extensions,
        ctx.get_settings().await.supported_extensions
    );
    assert_eq!(
        plan.location,
        EmbeddingLocation::local_from_settings(&ctx.get_settings().await, EmbeddingEngine::Candle)
    );
}

#[tokio::test]
async fn test_prepare_build_index_rejects_running_task() {
    let (_dir, ctx) = test_ctx();
    *ctx.embed_task.lock() = Some(running_embed_task());

    let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
    let err = ctx
        .prepare_build_index("/tmp", &selected, None)
        .await
        .unwrap_err();

    assert!(err.contains("already in progress"));
}

#[tokio::test]
async fn test_prepare_build_index_validates_root_path() {
    let (_dir, ctx) = test_ctx();
    let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);

    let missing = ctx
        .prepare_build_index("/definitely/missing/path", &selected, None)
        .await
        .unwrap_err();
    assert!(missing.contains("Index root not found"));

    let file_dir = tempdir().unwrap();
    let file_path = file_dir.path().join("not_a_dir");
    std::fs::write(&file_path, "hello").unwrap();
    let not_dir = ctx
        .prepare_build_index(&file_path.to_string_lossy(), &selected, None)
        .await
        .unwrap_err();
    assert!(not_dir.contains("not a directory"));

    let empty_dir = tempdir().unwrap();
    let empty = ctx
        .prepare_build_index(&empty_dir.path().to_string_lossy(), &selected, None)
        .await
        .unwrap_err();
    assert!(empty.contains("No supported files found"));
}

#[tokio::test]
async fn test_prepare_download_model_happy_path_and_running_guard() {
    let (dir, ctx) = test_ctx();
    let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("file.txt"), "hello").unwrap();
    ctx.update_settings(serde_json::json!({ "last_directory": root }))
        .await
        .unwrap();

    let plan = ctx.prepare_download_model(&selected).await.unwrap();
    assert_eq!(
        plan.location,
        EmbeddingLocation::local_from_settings(&ctx.get_settings().await, EmbeddingEngine::Candle)
    );

    *ctx.embed_task.lock() = Some(running_embed_task());
    let err = ctx.prepare_download_model(&selected).await.unwrap_err();
    assert!(err.contains("already in progress"));
}

#[test]
fn test_build_index_options_and_cleanup_partial_files() {
    let dir = tempdir().unwrap();
    let plan = BuildIndexPlan {
        root_path: dir.path().join("root"),
        location: EmbeddingLocation::Local {
            device: "cpu".to_string(),
            batch_size: 16,
        },
        chunk_size: 123,
        chunk_overlap: 45,
        supported_extensions: vec!["rs".to_string(), "txt".to_string()],
        documents: None,
        continues: None,
    };
    let (tx, _rx) = mpsc::channel(1);
    let options = AppContext::build_index_options(
        dir.path().join("models"),
        dir.path().to_path_buf(),
        &plan,
        BuildReporter::without_journal(tx),
        Arc::new(AtomicBool::new(false)),
        vec![dir.path().join("root/a.txt")],
        BuildScope::WholeRoot,
    );

    assert_eq!(options.model_dir, dir.path().join("models"));
    assert_eq!(options.index_dir, dir.path().to_path_buf());
    assert_eq!(options.chunk_size, 123);
    assert_eq!(options.chunk_overlap, 45);
    assert_eq!(options.supported_extensions, vec!["rs", "txt"]);
    assert_eq!(options.documents, vec![dir.path().join("root/a.txt")]);
    assert_eq!(options.scope, BuildScope::WholeRoot);

    for suffix in [".tmp", ".tmp-wal", ".tmp-shm"] {
        std::fs::write(dir.path().join(format!("semantic_index.db{suffix}")), "x").unwrap();
    }
    AppContext::cleanup_partial_index_files(dir.path());
    for suffix in [".tmp", ".tmp-wal", ".tmp-shm"] {
        assert!(!dir
            .path()
            .join(format!("semantic_index.db{suffix}"))
            .exists());
    }
}

#[tokio::test]
async fn test_emit_progress_helpers() {
    let dir = tempdir().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: Arc::clone(&captured),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths::resolve(dir.path()),
        emitter,
    );
    let progress = EmbedProgress::Build(wilkes_core::models::progress::IndexBuildProgress {
        files_processed: 1,
        total_files: 2,
        job_id: None,
        document: None,
        stage: None,
        outcome: None,
        done: false,
    });
    ctx.emit_progress_event(&progress);

    // A build's count goes out twice: on its own stream, and on the one
    // every rewrite of the index reports on — the indexing toast's.
    let events = captured.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].0, "embed-progress");
    assert_eq!(events[1].0, "reindex-progress");
    assert_eq!(events[1].1, serde_json::json!({ "done": 1, "total": 2 }));
}

#[tokio::test]
async fn test_forward_embed_progress_emits_events() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let emitter: Arc<dyn EventEmitter> = Arc::new(MockEmitter {
        events: Arc::clone(&captured),
    });
    let (tx, rx) = mpsc::channel(2);

    let forward = tokio::spawn(AppContext::forward_embed_progress(Arc::clone(&emitter), rx));
    tx.send(EmbedProgress::Download(
        wilkes_core::models::progress::DownloadProgress {
            bytes_received: 3,
            total_bytes: 9,
            done: false,
        },
    ))
    .await
    .unwrap();
    drop(tx);
    forward.await.unwrap();

    let events = captured.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "embed-progress");
}

#[tokio::test]
async fn test_open_built_index_error_and_store_loaded_state() {
    let (dir, ctx) = test_ctx();
    let err = match ctx
        .open_built_index(
            dir.path().to_path_buf(),
            wilkes_core::embed::EmbeddingSpaceIdentity::for_runtime(
                EmbeddingEngine::Candle,
                "missing-model",
                384,
            ),
        )
        .await
    {
        Ok(_) => panic!("expected open_built_index to fail"),
        Err(err) => err,
    };
    assert!(!err.is_empty());

    let data_dir = ctx.data_dir.clone();
    let index = SemanticIndex::create(
        &data_dir,
        "mock-model",
        384,
        EmbeddingEngine::Candle,
        Some(dir.path()),
    )
    .unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
    let index_arc = ctx.restore_store_loaded_state(Arc::clone(&embedder), index);

    assert!(ctx.embedder.lock().is_some());
    assert!(ctx.index.lock().lock().unwrap().is_some());
    assert!(index_arc.lock().unwrap().is_some());
}

#[tokio::test]
async fn test_probe_and_load_downloaded_model_with_installer_branches() {
    let (_dir, ctx) = test_ctx();

    let install_fail: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
        install_calls: Arc::new(AtomicUsize::new(0)),
        build_calls: Arc::new(AtomicUsize::new(0)),
        available: true,
        install_should_fail: true,
        build_should_fail: false,
    });
    let err = ctx
        .probe_and_load_downloaded_model_with(install_fail)
        .await
        .unwrap_err();
    assert!(err.contains("Failed to probe model dimensions"));

    let build_fail: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
        install_calls: Arc::new(AtomicUsize::new(0)),
        build_calls: Arc::new(AtomicUsize::new(0)),
        available: true,
        install_should_fail: false,
        build_should_fail: true,
    });
    let err = ctx
        .probe_and_load_downloaded_model_with(build_fail)
        .await
        .unwrap_err();
    assert!(err.contains("build failed"));

    let ok: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
        install_calls: Arc::new(AtomicUsize::new(0)),
        build_calls: Arc::new(AtomicUsize::new(0)),
        available: true,
        install_should_fail: false,
        build_should_fail: false,
    });
    ctx.probe_and_load_downloaded_model_with(ok).await.unwrap();
    assert!(ctx.embedder.lock().is_some());
}

#[tokio::test]
async fn test_finish_build_index_starts_directory_watcher_and_persists_state() {
    let (dir, ctx) = test_ctx();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let data_dir = ctx.data_dir.clone();
    let index = SemanticIndex::create(
        &data_dir,
        "build-model",
        384,
        EmbeddingEngine::Candle,
        Some(&root),
    )
    .unwrap();
    let plan = BuildIndexPlan {
        root_path: root.clone(),
        location: EmbeddingLocation::Local {
            device: "cpu".to_string(),
            batch_size: 16,
        },
        chunk_size: 64,
        chunk_overlap: 8,
        supported_extensions: vec!["txt".to_string()],
        documents: None,
        continues: None,
    };
    let selected = SelectedEmbedder {
        engine: EmbeddingEngine::Candle,
        model: EmbedderModel("build-model".to_string()),
        dimension: 384,
    };
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder {
        model_id: "build-model".to_string(),
        ..MockEmbedder::default()
    });

    ctx.finish_build_index(&plan, &selected, &data_dir, embedder)
        .await
        .unwrap();

    assert!(ctx.embedder.lock().is_some());
    assert!(ctx.index.lock().lock().unwrap().is_some());
    assert!(ctx.directory_watcher.lock().is_some());

    let settings = ctx.get_settings().await;
    assert!(settings.semantic.enabled);
    assert_eq!(
        settings.semantic.index_path,
        Some(data_dir.join("semantic_index.db"))
    );

    ctx.stop_directory_watcher();
    assert!(ctx.directory_watcher.lock().is_none());
    drop(index);
}

#[tokio::test]
async fn test_start_build_watcher_failure_leaves_no_watcher() {
    let (_dir, ctx) = test_ctx();
    let index = SemanticIndex::create(
        &ctx.data_dir,
        "watch-model",
        384,
        EmbeddingEngine::Candle,
        None,
    )
    .unwrap();
    let index_arc = Arc::new(Mutex::new(Some(index)));
    let missing_root = PathBuf::from("/definitely/missing/watcher/root");
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());

    ctx.start_build_watcher(
        missing_root,
        index_arc,
        embedder,
        IndexingConfig {
            chunk_size: 64,
            chunk_overlap: 8,
            supported_extensions: vec!["txt".to_string()],
        },
    );

    assert!(ctx.directory_watcher.lock().is_none());
}

#[tokio::test]
async fn test_spawn_build_index_task_cancellation_cleans_up_without_emitting_done() {
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: Arc::clone(&events),
    });
    let (ctx, _rx, loop_fut) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: dir.path().join("missing-worker"),
            data_dir: dir.path().to_path_buf(),
        },
        emitter,
    );
    let _loop_handle = tokio::spawn(loop_fut);
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let stale = ctx.data_dir.join("semantic_index.db.tmp");
    std::fs::write(&stale, "stale").unwrap();

    let plan = BuildIndexPlan {
        root_path: root,
        location: EmbeddingLocation::Local {
            device: "cpu".to_string(),
            batch_size: 16,
        },
        chunk_size: 64,
        chunk_overlap: 8,
        supported_extensions: vec!["txt".to_string()],
        documents: None,
        continues: None,
    };
    let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    let handle = Arc::clone(&ctx).spawn_build_index_task(
        plan,
        selected,
        cancel,
        Arc::clone(&cancel_flag),
        Arc::new(BuildSettlement::new()),
    );

    handle.await.unwrap().unwrap();

    assert!(cancel_flag.load(Ordering::Relaxed));
    assert!(!stale.exists());

    let events = events.lock().unwrap();
    assert!(events.iter().any(|(name, payload)| {
        name == "embed-error" && payload["operation"] == "Build" && payload["message"] == ""
    }));
    assert!(events.iter().any(|(name, payload)| {
        name == "manager-event" && payload == &serde_json::json!("ReindexingCancelled")
    }));
    assert!(events.iter().all(|(name, payload)| name != "manager-event"
        || payload != &serde_json::json!("ReindexingDone")));
}

#[cfg(unix)]
#[tokio::test]
async fn test_spawn_build_index_task_failure_emits_cancelled_without_done() {
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: Arc::clone(&events),
    });
    let worker_bin = dir.path().join("worker.sh");
    write_executable(
        &worker_bin,
        r#"#!/bin/sh
read req
echo '{"Error":"Model '\''DefinitelyNotARealFastembedModel'\'' is not supported by fastembed"}'
exit 0
"#,
    );
    let (ctx, _rx, loop_fut) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin,
            data_dir: dir.path().to_path_buf(),
        },
        emitter,
    );
    let _loop_handle = tokio::spawn(loop_fut);
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();

    let plan = BuildIndexPlan {
        root_path: root,
        location: EmbeddingLocation::Local {
            device: "cpu".to_string(),
            batch_size: 16,
        },
        chunk_size: 64,
        chunk_overlap: 8,
        supported_extensions: vec!["txt".to_string()],
        documents: None,
        continues: None,
    };
    let selected = SelectedEmbedder {
        engine: EmbeddingEngine::Fastembed,
        model: EmbedderModel("DefinitelyNotARealFastembedModel".to_string()),
        dimension: 384,
    };
    let cancel = CancellationToken::new();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    let handle = Arc::clone(&ctx).spawn_build_index_task(
        plan,
        selected,
        cancel,
        Arc::clone(&cancel_flag),
        Arc::new(BuildSettlement::new()),
    );

    handle.await.unwrap().unwrap();

    assert!(!cancel_flag.load(Ordering::Relaxed));

    let events = events.lock().unwrap();
    assert!(events.iter().any(|(name, payload)| {
        name == "embed-error"
            && payload["operation"] == "Build"
            && payload["message"]
                .as_str()
                .is_some_and(|msg| msg.contains("is not supported by fastembed"))
    }));
    assert!(events.iter().any(|(name, payload)| {
        name == "manager-event" && payload == &serde_json::json!("ReindexingCancelled")
    }));
    assert!(events.iter().all(|(name, payload)| {
        name != "manager-event" || payload != &serde_json::json!("ReindexingDone")
    }));
}

#[tokio::test]
async fn test_cancel_embed() {
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: events.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        },
        emitter,
    );

    let cancel = CancellationToken::new();
    let join = tokio::spawn(async {
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        Ok(())
    });
    *ctx.embed_task.lock() = Some(EmbedTaskHandle {
        operation: EmbedOperation::Download,
        cancel,
        cancel_flag: Arc::new(AtomicBool::new(false)),
        join: BuildHandle::new(join),
        settlement: Arc::new(BuildSettlement::new()),
    });

    ctx.cancel_embed().await; // Should not panic
    assert!(ctx.embed_task.lock().is_none());

    let events_guard = events.lock().unwrap();
    assert!(events_guard.iter().any(|(name, payload)| {
        name == "embed-error" && payload["operation"] == "Download" && payload["message"] == ""
    }));
}

/// The promise the user is given when they press cancel, as a test that
/// fails if it is ever taken back: the cancellation is over when they ask
/// for it, not when the work they cancelled agrees to end.
///
/// The build here never ends, which is the case that matters. A real build
/// runs on a blocking thread — through the page it is rendering, an
/// integrity check and a merge — and none of that is abortable, so a
/// cancel that waited for it waited for an unbounded amount of work with
/// "Cancelling…" on screen.
#[tokio::test]
async fn cancelling_a_build_that_will_not_end_still_ends_the_cancellation() {
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: Arc::clone(&events),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: dir.path().to_path_buf(),
        },
        emitter,
    );

    let cancel_flag = Arc::new(AtomicBool::new(false));
    let wedged = tokio::spawn(async {
        std::future::pending::<()>().await;
        Ok(())
    });
    *ctx.embed_task.lock() = Some(EmbedTaskHandle {
        operation: EmbedOperation::Build,
        cancel: CancellationToken::new(),
        cancel_flag: Arc::clone(&cancel_flag),
        join: BuildHandle::new(wedged),
        settlement: Arc::new(BuildSettlement::new()),
    });

    let started = std::time::Instant::now();
    ctx.cancel_embed().await;
    let took = started.elapsed();

    assert!(
        took < CANCEL_DEADLINE,
        "the cancellation took {took:?}, which is longer than the {CANCEL_DEADLINE:?} it \
             promises: something on this path is waiting for work it cannot kill"
    );
    assert!(
        cancel_flag.load(Ordering::Relaxed),
        "the build was not told"
    );
    assert!(
        ctx.worker_manager.is_sealed() && ctx.recognize_manager.is_sealed(),
        "a killed worker the next submit can start again is not a killed worker"
    );
    // Told by the canceller, because the build that was cancelled is in no
    // position to say anything.
    let events = events.lock().unwrap();
    assert!(events.iter().any(|(name, payload)| {
        name == "embed-error" && payload["operation"] == "Build" && payload["message"] == ""
    }));
    assert!(events.iter().any(|(name, payload)| {
        name == "manager-event" && payload == &serde_json::json!("ReindexingCancelled")
    }));
}

/// The build is still winding down — publishing what it indexed — and
/// until it has, the next one may not start on top of it and the workers
/// stay sealed. This is the cost of not waiting, and it is paid where the
/// user is not watching.
#[tokio::test]
async fn a_build_that_has_not_wound_down_holds_the_next_one_off() {
    let (_dir, ctx) = test_ctx();
    let wedged = tokio::spawn(async {
        std::future::pending::<()>().await;
        Ok(())
    });
    *ctx.embed_task.lock() = Some(EmbedTaskHandle {
        operation: EmbedOperation::Build,
        cancel: CancellationToken::new(),
        cancel_flag: Arc::new(AtomicBool::new(false)),
        join: BuildHandle::new(wedged),
        settlement: Arc::new(BuildSettlement::new()),
    });

    ctx.cancel_embed().await;

    assert!(
        ctx.embed_task_is_running(),
        "the wind-down is still running"
    );
    assert!(ctx.worker_manager.is_sealed());

    // And when it finally ends, everything it held is given back without
    // anyone having to ask.
    ctx.clear_embed_task();
    assert!(!ctx.embed_task_is_running());
    assert!(
        !ctx.worker_manager.is_sealed() && !ctx.recognize_manager.is_sealed(),
        "the seal outlived the cancellation it belonged to"
    );
}

/// Two settlers, one verdict. Whoever gets there first speaks for the
/// build: the canceller when the user cancelled, the build itself when it
/// finished on its own.
#[test]
fn a_build_is_settled_once() {
    let settlement = BuildSettlement::new();
    assert!(settlement.claim());
    assert!(!settlement.claim());
    assert!(!settlement.claim());
}

#[tokio::test]
async fn test_prepare_build_index_rejects_retry_while_cancel_in_progress() {
    let dir = tempdir().unwrap();
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: dir.path().to_path_buf(),
        },
        emitter,
    );

    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();
    let join = tokio::spawn(async move {
        cancel_for_task.cancelled().await;
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        Ok(())
    });
    *ctx.embed_task.lock() = Some(EmbedTaskHandle {
        operation: EmbedOperation::Build,
        cancel,
        cancel_flag: Arc::new(AtomicBool::new(false)),
        join: BuildHandle::new(join),
        settlement: Arc::new(BuildSettlement::new()),
    });

    let cancel_ctx = Arc::clone(&ctx);
    let cancel_task = tokio::spawn(async move {
        cancel_ctx.cancel_embed().await;
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let err = ctx
        .prepare_build_index(&root.to_string_lossy(), &selected, None)
        .await
        .unwrap_err();
    // Named for what it is: the cancel itself returned at once, and what
    // holds the next build off is the stopped one finishing its writing.
    assert_eq!(
        err,
        "The cancelled build is still saving what it indexed. Try again in a moment."
    );

    cancel_task.await.unwrap();
}

#[tokio::test]
async fn test_delete_index() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        data_dir.clone(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        },
        emitter,
    );

    std::fs::write(data_dir.join("semantic_index.db"), "fake db").unwrap();
    let res = ctx.delete_index(None).await;
    assert!(res.is_ok());
}

#[tokio::test]
async fn test_get_index_status() {
    let dir = tempdir().unwrap();
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        },
        emitter,
    );

    let res = ctx.get_index_status(None).await;
    assert!(res.is_err()); // No index exists
}

#[tokio::test]
async fn test_start_build_index_already_in_progress() {
    let dir = tempdir().unwrap();
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        },
        emitter,
    );

    // Mock a task in progress
    let cancel = CancellationToken::new();
    let join = tokio::spawn(async { Ok(()) });
    *ctx.embed_task.lock() = Some(EmbedTaskHandle {
        operation: EmbedOperation::Build,
        cancel,
        cancel_flag: Arc::new(AtomicBool::new(false)),
        join: BuildHandle::new(join),
        settlement: Arc::new(BuildSettlement::new()),
    });

    let res = ctx
        .start_build_index(
            "root".to_string(),
            SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("m".to_string()),
                dimension: 384,
            },
        )
        .await;

    assert!(res.is_err());
    assert_eq!(res.unwrap_err(), "A build is already in progress.");
}

#[tokio::test]
async fn test_start_build_index_root_not_found() {
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: events.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        WorkerPaths::resolve(dir.path()),
        emitter,
    );

    let res = ctx
        .start_build_index(
            "/non/existent/path/for/sure/12345".to_string(),
            SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("m".to_string()),
                dimension: 384,
            },
        )
        .await;

    assert!(res.is_err());
    assert!(res.unwrap_err().contains("Index root not found"));
    assert!(events.lock().unwrap().iter().any(|(name, payload)| {
        name == "embed-error"
            && payload["operation"] == "Build"
            && payload["message"]
                .as_str()
                .is_some_and(|msg| msg.contains("Index root not found"))
    }));
}

#[tokio::test]
async fn test_start_download_model_already_in_progress() {
    let dir = tempdir().unwrap();
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        },
        emitter,
    );

    // Mock a task in progress
    let cancel = CancellationToken::new();
    let join = tokio::spawn(async { Ok(()) });
    *ctx.embed_task.lock() = Some(EmbedTaskHandle {
        operation: EmbedOperation::Build,
        cancel,
        cancel_flag: Arc::new(AtomicBool::new(false)),
        join: BuildHandle::new(join),
        settlement: Arc::new(BuildSettlement::new()),
    });

    let res = ctx
        .start_download_model(SelectedEmbedder {
            engine: EmbeddingEngine::Candle,
            model: EmbedderModel("m".to_string()),
            dimension: 384,
        })
        .await;

    assert!(res.is_err());
    assert_eq!(res.unwrap_err(), "A build is already in progress.");
}

#[tokio::test]
async fn test_delete_index_operation() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let settings_path = dir.path().join("settings.json");
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        data_dir.clone(),
        settings_path,
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: data_dir.clone(),
        },
        emitter,
    );

    // Create a fake index
    std::fs::write(data_dir.join("semantic_index.db"), "fake db").unwrap();
    // Note: delete_index currently only removes the .db file.
    std::fs::write(
        data_dir.join("semantic_index.status.json"),
        r#"{"model_id": "m", "dimension": 1, "engine": "Candle"}"#,
    )
    .unwrap();

    ctx.delete_index(None).await.unwrap();
    assert!(!data_dir.join("semantic_index.db").exists());

    let settings = ctx.get_settings().await;
    assert!(settings.semantic.index_path.is_none());
}

#[tokio::test]
async fn test_get_index_status_not_found() {
    let dir = tempdir().unwrap();
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: dir.path().to_path_buf(),
        },
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );

    let res = ctx.get_index_status(None).await;
    assert!(res.is_err());
}

#[tokio::test]
async fn test_cancel_embed_operation() {
    let dir = tempdir().unwrap();
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: dir.path().to_path_buf(),
        },
        emitter,
    );
    std::fs::write(dir.path().join("file.txt"), "hello").unwrap();

    // Start a fake build index
    ctx.clone()
        .start_build_index(
            dir.path().to_string_lossy().to_string(),
            SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("m".to_string()),
                dimension: 384,
            },
        )
        .await
        .unwrap();

    // Immediately cancel
    ctx.cancel_embed().await;

    assert!(ctx.embed_task.lock().is_none());
    assert!(!ctx.get_worker_status().active);
}

#[tokio::test]
async fn test_start_download_model_error() {
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: events.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            uv_bin: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: dir.path().to_path_buf(),
        },
        emitter,
    );
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("file.txt"), "hello").unwrap();
    ctx.update_settings(serde_json::json!({ "last_directory": root }))
        .await
        .unwrap();

    // Requesting download of non-existent model should eventually emit error
    ctx.clone()
        .start_download_model(SelectedEmbedder {
            engine: EmbeddingEngine::Fastembed,
            model: EmbedderModel("invalid-model".to_string()),
            dimension: 384,
        })
        .await
        .unwrap();

    // Wait for task to fail
    let mut found = false;
    for _ in 0..10 {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let events_guard = events.lock().unwrap();
        if events_guard.iter().any(|e| e.0 == "embed-error") {
            found = true;
            break;
        }
    }
    assert!(found);
}

#[tokio::test]
async fn test_prepare_build_index_rejects_file_root() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("file.txt");
    std::fs::write(&file_path, "hello").unwrap();

    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("settings.json"),
        WorkerPaths::resolve(dir.path()),
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );

    let err = ctx
        .prepare_build_index(
            file_path.to_str().unwrap(),
            &SelectedEmbedder::default_for(EmbeddingEngine::Candle),
            None,
        )
        .await
        .unwrap_err();
    assert!(err.contains("is not a directory"));
}

#[tokio::test]
async fn test_spawn_download_model_error() {
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let emitter = Arc::new(MockEmitter {
        events: events.clone(),
    });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().to_path_buf(),
        dir.path().join("s.json"),
        WorkerPaths::resolve(dir.path()),
        emitter,
    );

    let plan = DownloadModelPlan {
        location: EmbeddingLocation::Local {
            device: "cpu".to_string(),
            batch_size: 16,
        },
    };
    let selected = SelectedEmbedder {
        engine: EmbeddingEngine::Candle,
        model: EmbedderModel("invalid".to_string()),
        dimension: 0,
    };

    let join = ctx.spawn_download_model_task(plan, selected);
    let _ = join.await;

    let events_guard = events.lock().unwrap();
    assert!(events_guard.iter().any(|(name, _)| name == "embed-error"));
}
