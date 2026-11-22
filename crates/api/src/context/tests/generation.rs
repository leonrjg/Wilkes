use super::*;

#[tokio::test]
async fn document_summary_uses_the_shared_delta_and_terminal_event_contract() {
    let dir = tempdir().unwrap();
    // Desktop documents normally live outside the app-support data dir.
    // Filesystem confinement belongs to the HTTP uploads boundary.
    let documents = tempdir().unwrap();
    let path = documents.path().join("paper.txt");
    std::fs::write(&path, "A source document with a result.").unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let ctx = generation_ctx(dir.path(), Arc::clone(&events));
    enable_generation(&ctx, "mock-generator").await;
    *ctx.generator.lock() = Some(Arc::new(MockGenerator::scripted(["A complete summary."])));

    Arc::clone(&ctx)
        .summarize_document("summary-request".to_string(), path)
        .await
        .unwrap();

    let events = events.lock().unwrap();
    let streamed = events
        .iter()
        .filter(|(name, _)| name == "generation-stream")
        .map(|(_, payload)| {
            serde_json::from_value::<GenerationStreamEvent>(payload.clone()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        streamed,
        vec![
            GenerationStreamEvent::Delta {
                request_id: "summary-request".to_string(),
                task: GenerationTask::DocumentSummary,
                delta: "A ".to_string(),
            },
            GenerationStreamEvent::Delta {
                request_id: "summary-request".to_string(),
                task: GenerationTask::DocumentSummary,
                delta: "complete ".to_string(),
            },
            GenerationStreamEvent::Delta {
                request_id: "summary-request".to_string(),
                task: GenerationTask::DocumentSummary,
                delta: "summary.".to_string(),
            },
            GenerationStreamEvent::Completed {
                request_id: "summary-request".to_string(),
                task: GenerationTask::DocumentSummary,
                text: "A complete summary.".to_string(),
            },
        ]
    );
}

#[tokio::test]
async fn search_results_summary_uses_its_own_correlated_task() {
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let ctx = generation_ctx(dir.path(), Arc::clone(&events));
    enable_generation(&ctx, "mock-generator").await;
    *ctx.generator.lock() = Some(Arc::new(MockGenerator::scripted([
        "The leading studies agree on the measured outcome [1].",
    ])));
    let input = SearchResultsSummaryInput {
        query: "agreement".to_string(),
        sources: vec![
            wilkes_core::generate::tasks::search_results_summary::SearchResultsSummarySource {
                title: "paper.pdf".to_string(),
            },
        ],
        passages: vec![
            wilkes_core::generate::tasks::search_results_summary::SearchResultsSummaryPassage {
                text: "The leading studies agree on the measured outcome.".to_string(),
                source_index: 0,
            },
        ],
    };

    Arc::clone(&ctx)
        .summarize_search_results("results-request".to_string(), input)
        .await
        .unwrap();

    let events = events.lock().unwrap();
    assert!(events.iter().any(|(name, payload)| {
        name == "generation-stream"
            && matches!(
                serde_json::from_value::<GenerationStreamEvent>(payload.clone()).unwrap(),
                GenerationStreamEvent::Completed {
                    request_id,
                    task: GenerationTask::SearchResultsSummary,
                    text,
                } if request_id == "results-request"
                    && text == "The leading studies agree on the measured outcome [1]."
            )
    }));
}

#[tokio::test]
async fn generation_task_failure_emits_one_correlated_failed_terminal_event() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("empty.txt");
    std::fs::write(&path, "  ").unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let ctx = generation_ctx(dir.path(), Arc::clone(&events));
    enable_generation(&ctx, "mock-generator").await;
    *ctx.generator.lock() = Some(Arc::new(MockGenerator::scripted(["unused"])));

    let error = Arc::clone(&ctx)
        .summarize_document("failed-request".to_string(), path)
        .await
        .unwrap_err();
    assert!(error.contains("document has no extractable text"));

    let events = events.lock().unwrap();
    let streamed = events
        .iter()
        .filter(|(name, _)| name == "generation-stream")
        .map(|(_, payload)| {
            serde_json::from_value::<GenerationStreamEvent>(payload.clone()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(streamed.len(), 1);
    assert!(matches!(
        &streamed[0],
        GenerationStreamEvent::Failed {
            request_id,
            task: GenerationTask::DocumentSummary,
            error,
        } if request_id == "failed-request"
            && error.contains("document has no extractable text")
    ));
}

/// The generation install must not borrow the embed event stream: that
/// stream globally sets "indexing", and this path emits no `embed-done` to
/// ever clear it. Failure or success, generation reports on its own events.
#[tokio::test]
async fn generation_load_never_touches_the_embed_event_stream() {
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let ctx = generation_ctx(dir.path(), Arc::clone(&events));
    enable_generation(&ctx, "not/a-real-model").await;

    let err = ctx.load_generator().await.unwrap_err();
    assert!(
        err.to_string().contains("not a known generation model"),
        "{err:#}"
    );

    let events = events.lock().unwrap();
    assert!(
        !events.iter().any(|(name, _)| name.starts_with("embed-")),
        "generation emitted an embed event: {events:?}"
    );
    assert!(events
        .iter()
        .any(|(name, payload)| name == "generation-error"
            && payload["message"]
                .as_str()
                .is_some_and(|m| m.contains("not a known generation model"))));
}

/// Two settings changes in quick succession each spawn a load. The one that
/// is no longer current must leave the generator alone, whatever order the
/// two finish in — readiness only checks that *some* generator is attached,
/// so an overwrite here silently runs the wrong model.
#[tokio::test]
async fn a_superseded_load_does_not_attach_its_generator() {
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let ctx = generation_ctx(dir.path(), Arc::clone(&events));
    enable_generation(&ctx, "not/a-real-model").await;

    // Hold the load lock so the load below queues, then claim a newer epoch
    // on its behalf — exactly what a second `load_generator` call does.
    let held = ctx.generator_load_lock.lock().await;
    let load = {
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move { ctx.load_generator().await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    ctx.generator_epoch.fetch_add(1, Ordering::SeqCst);
    drop(held);

    assert_eq!(load.await.unwrap().unwrap(), GeneratorLoad::Superseded);
    assert!(ctx.generator.lock().is_none());
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|(name, _)| name == "generation-error"),
        "a superseded load must not report a failure"
    );
}

#[tokio::test]
async fn unloading_supersedes_a_generation_load_already_in_flight() {
    let dir = tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let ctx = generation_ctx(dir.path(), events);
    enable_generation(&ctx, "not/a-real-model").await;

    let held = ctx.generator_load_lock.lock().await;
    let load = {
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move { ctx.load_generator().await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    ctx.unload_generator();
    drop(held);

    assert_eq!(load.await.unwrap().unwrap(), GeneratorLoad::Superseded);
    assert!(ctx.generator.lock().is_none());
}
