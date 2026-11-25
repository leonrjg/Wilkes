use crate::*;

pub(crate) fn active_searches_state(app: &AppHandle) -> Arc<ActiveSearches> {
    app.state::<Arc<ActiveSearches>>().inner().clone()
}

/// Searches in flight, by search id, with the window each was started from.
pub(crate) struct ActiveSearches(pub(crate) Mutex<HashMap<String, (String, JoinHandle<()>)>>);

impl ActiveSearches {
    /// Stops the searches one window started, as that window leaves its
    /// workspace or closes. Another window's searches are its own.
    pub(crate) fn cancel_view(&self, view: &str) {
        self.0.lock().unwrap().retain(|_, (owner, handle)| {
            if owner == view {
                handle.abort();
                false
            } else {
                true
            }
        });
    }
}

pub(crate) trait SearchEventSink: Send + Sync + Clone + 'static {
    fn emit_event(&self, name: &str, payload: serde_json::Value);
}

impl SearchEventSink for AppHandle {
    fn emit_event(&self, name: &str, payload: serde_json::Value) {
        let _ = Emitter::emit(self, name, &payload);
    }
}

pub(crate) async fn search_for_ctx<E>(
    ctx: Arc<AppContext>,
    active_searches: Arc<ActiveSearches>,
    view: String,
    emitter: E,
    query: wilkes_core::types::SearchQuery,
    search_id: Option<String>,
) -> Result<String, String>
where
    E: SearchEventSink,
{
    let handle = Arc::clone(&ctx).start_search_as(query, "ui").await?;

    let search_id = search_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let id = search_id.clone();
    let emitter_for_task = emitter.clone();
    let active_searches_for_task = Arc::clone(&active_searches);

    let forwarder: JoinHandle<()> = tokio::spawn(async move {
        let stats = handle
            .run(|fm| {
                let emitter = emitter_for_task.clone();
                let id = id.clone();
                async move {
                    let payload = serde_json::to_value(&fm).unwrap_or_default();
                    emitter.emit_event(&format!("search-result-{}", id), payload);
                    true
                }
            })
            .await;

        emitter_for_task.emit_event(
            &format!("search-complete-{}", id),
            serde_json::to_value(&stats).unwrap_or_default(),
        );

        active_searches_for_task.0.lock().unwrap().remove(&id);
    });

    active_searches
        .0
        .lock()
        .unwrap()
        .insert(search_id.clone(), (view, forwarder));

    Ok(search_id)
}

pub(crate) fn cancel_search_for_ctx(active_searches: Arc<ActiveSearches>, search_id: &str) {
    if let Some((_, handle)) = active_searches.0.lock().unwrap().remove(search_id) {
        handle.abort();
    }
}

#[tauri::command]
pub(crate) async fn search(
    query: wilkes_core::types::SearchQuery,
    search_id: Option<String>,
    app: AppHandle,
    window: tauri::Window,
) -> Result<String, String> {
    let ctx = window_context(&window);
    let active_searches = active_searches_state(&app);
    search_for_ctx(
        ctx,
        active_searches,
        window.label().to_string(),
        app.clone(),
        query,
        search_id,
    )
    .await
}

#[tauri::command]
pub(crate) async fn cancel_search(search_id: String, app: AppHandle) -> Result<(), String> {
    cancel_search_for_ctx(active_searches_state(&app), &search_id);
    Ok(())
}

#[tauri::command]
pub(crate) async fn related_documents(
    query: wilkes_core::types::RelatedDocumentsQuery,
    window: tauri::Window,
) -> Result<Vec<wilkes_core::types::RelatedDocument>, String> {
    window_context(&window).related_documents(query).await
}

#[tauri::command]
pub(crate) async fn tidy_folder(
    folder: String,
    window: tauri::Window,
) -> Result<wilkes_core::types::TidyPlan, String> {
    window_context(&window).tidy_folder(folder.into()).await
}

#[tauri::command]
pub(crate) async fn reading_order(
    query: wilkes_core::types::ReadingOrderQuery,
    window: tauri::Window,
) -> Result<wilkes_core::types::ReadingOrder, String> {
    window_context(&window).reading_order(query).await
}

#[tauri::command]
pub(crate) async fn citation_links(
    query: wilkes_core::types::CitationLinksQuery,
    window: tauri::Window,
) -> Result<wilkes_core::types::CitationLinks, String> {
    window_context(&window).citation_links(query).await
}

#[tauri::command]
pub(crate) async fn citation_graph(
    query: wilkes_core::types::CitationGraphQuery,
    window: tauri::Window,
) -> Result<wilkes_core::types::CitationGraph, String> {
    window_context(&window).citation_graph(query).await
}

#[tauri::command]
pub(crate) async fn fetch_missing_citations(
    query: wilkes_core::types::CitationGraphQuery,
    window: tauri::Window,
) -> Result<usize, String> {
    window_context(&window).fetch_missing_citations(query).await
}
