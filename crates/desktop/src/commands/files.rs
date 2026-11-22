use crate::*;

pub(crate) async fn list_files_for_ctx(
    ctx: Arc<AppContext>,
    root: String,
    collection_id: Option<String>,
    tag_ids: Vec<String>,
    collection_expression: Option<String>,
) -> Result<FileListResponse, String> {
    ctx.list_files_filtered(
        root.into(),
        collection_id.as_deref(),
        &tag_ids,
        collection_expression.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}

pub(crate) async fn open_file_for_ctx(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<wilkes_core::types::PreviewData, String> {
    ctx.open_file(path.into()).await.map_err(|e| e.to_string())
}

pub(crate) async fn rename_file_for_path(
    ctx: Arc<AppContext>,
    path: String,
    new_name: String,
) -> Result<String, String> {
    ctx.rename_file(path.into(), new_name)
        .await
        .map(|path| path.display().to_string())
        .map_err(|e| e.to_string())
}

/// On macOS the `trash` crate defaults to driving Finder over AppleScript, which fails unless the
/// user grants the app Apple-events automation permission. `NSFileManager::trashItemAtURL` performs
/// the same move into the volume's Trash without that permission.
#[cfg(target_os = "macos")]
pub(crate) fn move_to_trash(path: &std::path::Path) -> Result<(), trash::Error> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};

    let mut context = trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn move_to_trash(path: &std::path::Path) -> Result<(), trash::Error> {
    trash::delete(path)
}

pub(crate) async fn trash_file_for_path(path: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = PathBuf::from(path);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("Cannot trash {}: {error}", path.display()))?;
        if !metadata.file_type().is_file() {
            return Err(format!("Cannot trash non-file path: {}", path.display()));
        }
        move_to_trash(&path)
            .map_err(|error| format!("Failed to move {} to Trash: {error}", path.display()))
    })
    .await
    .map_err(|error| format!("Trash operation failed: {error}"))?
}

pub(crate) async fn import_files_into_current_root_for_ctx(
    ctx: Arc<AppContext>,
    paths: Vec<String>,
    root: String,
    folder: Option<String>,
    mode: wilkes_api::commands::files::FileImportMode,
) -> Result<Vec<String>, String> {
    let paths = paths.into_iter().map(PathBuf::from).collect();
    ctx.import_files_into_current_root(paths, root.into(), folder, mode)
        .await
        .map(|paths| {
            paths
                .into_iter()
                .map(|path| path.display().to_string())
                .collect()
        })
        .map_err(|e| e.to_string())
}

pub(crate) async fn move_file_to_root_for_ctx(
    ctx: Arc<AppContext>,
    path: String,
    target_root: String,
) -> Result<String, String> {
    ctx.ensure_writable().map_err(|error| error.to_string())?;
    let supported_extensions = ctx.get_settings().await.supported_extensions;
    let old = PathBuf::from(path);
    let mut moved = wilkes_api::commands::files::move_files_into_root(
        vec![old.clone()],
        target_root.into(),
        supported_extensions,
    )
    .await
    .map_err(|e| e.to_string())?;
    let new = moved.pop().unwrap_or_default();
    ctx.rekey_research_path(&old, &new)
        .map_err(|e| e.to_string())?;
    ctx.rekey_index_path(&old, &new)
        .map_err(|e| e.to_string())?;
    Ok(new.display().to_string())
}

pub(crate) async fn get_file_metadata_for_path(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<DocumentMetadata, String> {
    ctx.get_file_metadata(path.into())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn open_path(path: String) -> Result<(), String> {
    validate_open_target(&path)?;
    SystemDesktopPlatform.open_target(&path)?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn reveal_path(path: String) -> Result<(), String> {
    validate_reveal_target(&path)?;
    SystemDesktopPlatform.reveal_target(&path)?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn trash_file(path: String, window: tauri::WebviewWindow) -> Result<(), String> {
    // Path-based rather than routed through the context, so the read-only gate
    // has to be asked for here: the workspace it would delete from is the
    // active one either way.
    window_context(&window)
        .ensure_writable()
        .map_err(|error| error.to_string())?;
    trash_file_for_path(path).await
}

#[tauri::command]
pub(crate) async fn pick_directory(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    app.dialog().file().pick_folder(move |path| {
        let _ = tx.send(path.map(|p| {
            let selected = p.to_string();
            std::fs::canonicalize(&selected)
                .unwrap_or_else(|_| selected.clone().into())
                .display()
                .to_string()
        }));
    });
    Ok(rx.await.unwrap_or(None))
}

#[tauri::command]
pub(crate) async fn list_files(
    root: String,
    collection_id: Option<String>,
    tag_ids: Option<Vec<String>>,
    collection_expression: Option<String>,
    window: tauri::WebviewWindow,
) -> Result<FileListResponse, String> {
    list_files_for_ctx(
        window_context(&window),
        root,
        collection_id,
        tag_ids.unwrap_or_default(),
        collection_expression,
    )
    .await
}

#[tauri::command]
pub(crate) async fn open_file(
    path: String,
    window: tauri::WebviewWindow,
) -> Result<wilkes_core::types::PreviewData, String> {
    open_file_for_ctx(window_context(&window), path).await
}

#[tauri::command]
pub(crate) async fn rename_file(
    path: String,
    new_name: String,
    window: tauri::WebviewWindow,
) -> Result<String, String> {
    rename_file_for_path(window_context(&window), path, new_name).await
}

/// Imports files into the current root, or into `folder` beneath it — created
/// if absent. The folder is for things that are not one file: a course is
/// forty PDFs that belong together, and loose in the root they are neither
/// attributable nor importable alongside a second course, which collides with
/// them on names like `lecture5.pdf`.
#[tauri::command]
pub(crate) async fn import_files(
    paths: Vec<String>,
    root: String,
    folder: Option<String>,
    mode: wilkes_api::commands::files::FileImportMode,
    window: tauri::WebviewWindow,
) -> Result<Vec<String>, String> {
    import_files_into_current_root_for_ctx(window_context(&window), paths, root, folder, mode).await
}

#[tauri::command]
pub(crate) async fn read_clipboard_files() -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(|| match clipboard_files::read() {
        Ok(paths) => paths
            .into_iter()
            .map(|path| {
                path.into_os_string()
                    .into_string()
                    .map_err(|_| "A copied file path is not valid UTF-8".to_string())
            })
            .collect(),
        Err(clipboard_files::Error::NoFiles) => Ok(Vec::new()),
        Err(clipboard_files::Error::SystemError(error)) => {
            Err(format!("Could not read copied files: {error}"))
        }
    })
    .await
    .map_err(|error| format!("Could not read copied files: {error}"))?
}

#[tauri::command]
pub(crate) async fn move_file(
    path: String,
    target_root: String,
    window: tauri::WebviewWindow,
) -> Result<String, String> {
    move_file_to_root_for_ctx(window_context(&window), path, target_root).await
}

#[tauri::command]
pub(crate) async fn create_directory(
    parent: String,
    name: String,
    window: tauri::WebviewWindow,
) -> Result<String, String> {
    window_context(&window)
        .ensure_writable()
        .map_err(|error| error.to_string())?;
    wilkes_api::commands::files::create_directory(parent.into(), name)
        .await
        .map(|path| path.display().to_string())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn list_directories(path: String) -> Result<Vec<String>, String> {
    let mut entries = tokio::fs::read_dir(&path)
        .await
        .map_err(|error| format!("Could not read directory {path}: {error}"))?;
    let mut directories = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Could not read directory {path}: {error}"))?
    {
        let file_type = entry
            .file_type()
            .await
            .map_err(|error| format!("Could not inspect {}: {error}", entry.path().display()))?;
        if file_type.is_dir() {
            directories.push(entry.path().display().to_string());
        }
    }
    directories.sort_by_key(|directory| directory.to_lowercase());
    Ok(directories)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PathKind {
    Directory,
    File,
}

/// Classify dropped paths so the UI can tell a folder (a candidate root) from a
/// file (a candidate import) before it commits to either. Symlinks are followed
/// so a link to a folder classifies as one.
#[tauri::command]
pub(crate) async fn path_kinds(paths: Vec<String>) -> Result<Vec<PathKind>, String> {
    let mut kinds = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|error| format!("Could not inspect {path}: {error}"))?;
        kinds.push(if metadata.is_dir() {
            PathKind::Directory
        } else {
            PathKind::File
        });
    }
    Ok(kinds)
}

#[tauri::command]
pub(crate) async fn get_file_metadata(
    path: String,
    window: tauri::WebviewWindow,
) -> Result<DocumentMetadata, String> {
    get_file_metadata_for_path(window_context(&window), path).await
}
