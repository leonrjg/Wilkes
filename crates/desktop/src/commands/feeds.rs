use crate::*;

/// Every feed with its status, flags and inbox count.
#[tauri::command]
pub(crate) async fn feeds_list(
    window: tauri::WebviewWindow,
) -> Result<Vec<wilkes_api::research::FeedRecord>, String> {
    window_context(&window)
        .feeds_list()
        .map_err(|error| format!("{error:#}"))
}

/// The feeds an address offers, each read to be sure it is one.
#[tauri::command]
pub(crate) async fn feed_discover(
    address: String,
    window: tauri::WebviewWindow,
) -> Result<Vec<wilkes_api::context::feeds::OfferedFeed>, String> {
    window_context(&window)
        .feed_discover(&address)
        .await
        .map_err(|error| format!("{error:#}"))
}

/// Add a feed and refresh it. "Follow this" calls it with the listing on screen.
#[tauri::command]
pub(crate) async fn feed_add(
    feed: wilkes_api::context::feeds::NewFeed,
    window: tauri::WebviewWindow,
) -> Result<wilkes_api::research::FeedRecord, String> {
    window_context(&window)
        .feed_add(feed)
        .await
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub(crate) async fn feed_update(
    id: String,
    update: wilkes_api::context::feeds::FeedUpdate,
    window: tauri::WebviewWindow,
) -> Result<wilkes_api::research::FeedRecord, String> {
    window_context(&window)
        .feed_update(&id, update)
        .await
        .map_err(|error| format!("{error:#}"))
}

/// Remove a feed and its items; `delete_files` deletes what it imported too.
#[tauri::command]
pub(crate) async fn feed_remove(
    id: String,
    delete_files: bool,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    window_context(&window)
        .feed_remove(&id, delete_files)
        .await
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub(crate) async fn feeds_refresh(window: tauri::WebviewWindow) -> Result<(), String> {
    window_context(&window)
        .feeds_refresh()
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub(crate) async fn feeds_refresh_cancel(window: tauri::WebviewWindow) -> Result<(), String> {
    window_context(&window).feeds_refresh_cancel();
    Ok(())
}

/// Items by status and feed, newest first, paged.
#[tauri::command]
pub(crate) async fn feed_items(
    query: wilkes_api::context::feeds::FeedItemsQuery,
    window: tauri::WebviewWindow,
) -> Result<wilkes_api::context::feeds::FeedItemsPage, String> {
    window_context(&window)
        .feed_items(query)
        .map_err(|error| format!("{error:#}"))
}

/// Import or dismiss one item.
#[tauri::command]
pub(crate) async fn feed_item_set_status(
    id: String,
    action: wilkes_api::context::feeds::FeedItemAction,
    window: tauri::WebviewWindow,
) -> Result<wilkes_api::research::FeedItemRecord, String> {
    window_context(&window)
        .feed_item_set_status(&id, action)
        .await
        .map_err(|error| format!("{error:#}"))
}
