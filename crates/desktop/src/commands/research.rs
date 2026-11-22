use crate::*;

pub(crate) async fn list_bookmarks_for_ctx(ctx: Arc<AppContext>) -> Result<Vec<Bookmark>, String> {
    ctx.list_bookmarks().await.map_err(|e| e.to_string())
}

pub(crate) async fn add_bookmark_for_ctx(
    ctx: Arc<AppContext>,
    bookmark: NewBookmark,
) -> Result<Bookmark, String> {
    ctx.add_bookmark(bookmark).await.map_err(|e| e.to_string())
}

pub(crate) async fn remove_bookmark_for_ctx(
    ctx: Arc<AppContext>,
    id: String,
) -> Result<(), String> {
    ctx.remove_bookmark(&id).await.map_err(|e| e.to_string())
}

pub(crate) async fn update_bookmark_note_for_ctx(
    ctx: Arc<AppContext>,
    id: String,
    note: Option<String>,
) -> Result<Bookmark, String> {
    ctx.update_bookmark_note(&id, note)
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn cluster_bookmarks_for_ctx(
    ctx: Arc<AppContext>,
    query: BookmarkClustersQuery,
) -> Result<BookmarkClustersResult, String> {
    ctx.cluster_bookmarks(query).await
}

pub(crate) async fn chunk_topics_for_ctx(
    ctx: Arc<AppContext>,
    request_id: String,
    query: ChunkTopicsQuery,
) -> Result<ChunkTopicsResult, String> {
    ctx.chunk_topics(request_id, query).await
}

pub(crate) fn cancel_chunk_topics_for_ctx(ctx: Arc<AppContext>, request_id: &str) {
    ctx.cancel_chunk_topics(request_id);
}

#[tauri::command]
pub(crate) async fn list_bookmarks(window: tauri::WebviewWindow) -> Result<Vec<Bookmark>, String> {
    list_bookmarks_for_ctx(window_context(&window)).await
}

#[tauri::command]
pub(crate) async fn add_bookmark(
    bookmark: NewBookmark,
    window: tauri::WebviewWindow,
) -> Result<Bookmark, String> {
    add_bookmark_for_ctx(window_context(&window), bookmark).await
}

#[tauri::command]
pub(crate) async fn remove_bookmark(
    id: String,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    remove_bookmark_for_ctx(window_context(&window), id).await
}

#[tauri::command]
pub(crate) async fn update_bookmark_note(
    id: String,
    note: Option<String>,
    window: tauri::WebviewWindow,
) -> Result<Bookmark, String> {
    update_bookmark_note_for_ctx(window_context(&window), id, note).await
}

#[tauri::command]
pub(crate) async fn cluster_bookmarks(
    query: BookmarkClustersQuery,
    window: tauri::WebviewWindow,
) -> Result<BookmarkClustersResult, String> {
    cluster_bookmarks_for_ctx(window_context(&window), query).await
}

#[tauri::command]
pub(crate) async fn chunk_topics(
    request_id: String,
    query: ChunkTopicsQuery,
    window: tauri::WebviewWindow,
) -> Result<ChunkTopicsResult, String> {
    chunk_topics_for_ctx(window_context(&window), request_id, query).await
}

#[tauri::command]
pub(crate) async fn cancel_chunk_topics(
    request_id: String,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    cancel_chunk_topics_for_ctx(window_context(&window), &request_id);
    Ok(())
}

#[tauri::command]
pub(crate) async fn list_saved_works(
    window: tauri::WebviewWindow,
) -> Result<Vec<SavedWork>, String> {
    window_context(&window)
        .list_saved_works()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn save_work(
    work: NewSavedWork,
    window: tauri::WebviewWindow,
) -> Result<SavedWork, String> {
    window_context(&window)
        .save_work(work)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn remove_saved_work(
    id: String,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    window_context(&window)
        .remove_saved_work(&id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn update_saved_work_note(
    id: String,
    note: Option<String>,
    window: tauri::WebviewWindow,
) -> Result<SavedWork, String> {
    window_context(&window)
        .update_saved_work_note(&id, note)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn mark_saved_work_acquired(
    provider: String,
    work: LiteratureSearchResult,
    path: PathBuf,
    window: tauri::WebviewWindow,
) -> Result<Option<SavedWork>, String> {
    window_context(&window)
        .mark_saved_work_acquired(&provider, &work, path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn list_tags(window: tauri::WebviewWindow) -> Result<Vec<Tag>, String> {
    window_context(&window)
        .list_tags()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn create_tag(tag: NewTag, window: tauri::WebviewWindow) -> Result<Tag, String> {
    window_context(&window)
        .create_tag(tag)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn update_tag(
    id: String,
    tag: UpdateTag,
    window: tauri::WebviewWindow,
) -> Result<Tag, String> {
    window_context(&window)
        .update_tag(&id, tag)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn delete_tag(id: String, window: tauri::WebviewWindow) -> Result<(), String> {
    window_context(&window)
        .delete_tag(&id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn update_document_tags(
    update: DocumentTagUpdate,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    window_context(&window)
        .update_document_tags(update)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn list_smart_collections(
    window: tauri::WebviewWindow,
) -> Result<Vec<SmartCollection>, String> {
    window_context(&window)
        .list_collections()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn create_smart_collection(
    collection: NewSmartCollection,
    window: tauri::WebviewWindow,
) -> Result<SmartCollection, String> {
    window_context(&window)
        .create_collection(collection)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn update_smart_collection(
    id: String,
    collection: UpdateSmartCollection,
    window: tauri::WebviewWindow,
) -> Result<SmartCollection, String> {
    window_context(&window)
        .update_collection(&id, collection)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn delete_smart_collection(
    id: String,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    window_context(&window)
        .delete_collection(&id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn validate_smart_collection(
    expression: String,
) -> Result<CollectionValidation, String> {
    Ok(wilkes_api::research::ResearchStore::validate_collection(
        &expression,
    ))
}

#[tauri::command]
pub(crate) async fn list_search_log(
    limit: Option<usize>,
    window: tauri::WebviewWindow,
) -> Result<Vec<SearchLogEntry>, String> {
    window_context(&window)
        .list_search_log(limit.unwrap_or(100))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn delete_search_log(
    id: String,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    window_context(&window)
        .delete_search_log(&id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn clear_search_log(window: tauri::WebviewWindow) -> Result<(), String> {
    window_context(&window)
        .clear_search_log()
        .map_err(|e| e.to_string())
}
