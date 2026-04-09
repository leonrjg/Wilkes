use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;
use wilkes_api::context::EventEmitter;

use super::errors::{err, server_err, ErrorBody};
use axum::http::StatusCode;
use axum::Json;

pub struct AppState {
    pub ctx: Arc<wilkes_api::context::AppContext>,
    pub uploads_dir: PathBuf,
    pub events_tx: broadcast::Sender<(String, serde_json::Value)>,
}

pub struct BroadcastEmitter {
    pub tx: broadcast::Sender<(String, serde_json::Value)>,
}

impl EventEmitter for BroadcastEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        let _ = self.tx.send((name.to_string(), payload));
    }
}

#[async_trait]
pub trait ServerFs: Send + Sync {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    async fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    async fn remove_file(&self, path: &Path) -> io::Result<()>;
    async fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
    async fn dir_size(&self, path: &Path) -> anyhow::Result<u64>;
}

pub struct TokioServerFs;

#[async_trait]
impl ServerFs for TokioServerFs {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        tokio::fs::canonicalize(path).await
    }

    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        tokio::fs::read(path).await
    }

    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        tokio::fs::write(path, data).await
    }

    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        tokio::fs::create_dir_all(path).await
    }

    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        tokio::fs::remove_file(path).await
    }

    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        tokio::fs::remove_dir_all(path).await
    }

    async fn dir_size(&self, path: &Path) -> anyhow::Result<u64> {
        dir_size(path).await
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteTarget {
    pub canonical: PathBuf,
    pub kind: DeleteKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteKind {
    File,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedAsset {
    pub canonical: PathBuf,
    pub content_type: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadWritePlan {
    pub dest: PathBuf,
    pub create_parent: Option<PathBuf>,
}

fn sanitize_component(component: &str) -> Option<&str> {
    if component.is_empty() || component == "." || component == ".." {
        None
    } else {
        Some(component)
    }
}

pub fn sanitize_relative_upload_path(raw: &str) -> PathBuf {
    raw.split(['/', '\\'])
        .filter_map(sanitize_component)
        .collect()
}

pub fn validate_delete_target(rel: &Path) -> Result<DeleteTarget, (StatusCode, Json<ErrorBody>)> {
    if rel.as_os_str().is_empty() {
        return Err(err(
            "Cannot delete uploads root via this endpoint; use DELETE /api/upload/all",
        ));
    }

    Ok(DeleteTarget {
        canonical: rel.to_path_buf(),
        kind: DeleteKind::File,
    })
}

pub async fn asset_access_plan(
    path: &Path,
    uploads_dir: &Path,
    fs: &dyn ServerFs,
) -> Result<AuthorizedAsset, (StatusCode, Json<ErrorBody>)> {
    let canonical_uploads = fs
        .canonicalize(uploads_dir)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    let canonical = fs.canonicalize(path).await.map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: "File not found".into(),
            }),
        )
    })?;
    if !canonical.starts_with(&canonical_uploads) {
        return Err(err("Access denied"));
    }

    Ok(AuthorizedAsset {
        canonical: canonical.clone(),
        content_type: mime_for_path(&canonical),
    })
}

pub async fn confined_root_for_search(
    raw: &str,
    uploads_dir: &Path,
    fs: &dyn ServerFs,
) -> Result<PathBuf, (StatusCode, Json<ErrorBody>)> {
    let candidate = PathBuf::from(raw);
    let canonical_uploads = fs
        .canonicalize(uploads_dir)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    let canonical = fs.canonicalize(&candidate).await.map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: "Path not found".into(),
            }),
        )
    })?;
    if !canonical.starts_with(&canonical_uploads) {
        return Err(err("Access denied: path outside uploads directory"));
    }
    Ok(canonical)
}

pub async fn dir_size(path: &Path) -> anyhow::Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let meta = entry.metadata().await?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    Ok(total)
}

pub fn mime_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("pdf") => "application/pdf",
        Some("txt") | Some("md") | Some("rst") => "text/plain",
        Some("html") | Some("htm") => "text/html",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "application/octet-stream",
    }
}

pub fn upload_write_plan(uploads_dir: &Path, rel: &Path) -> UploadWritePlan {
    let dest = uploads_dir.join(rel);
    let create_parent = dest.parent().map(PathBuf::from);
    UploadWritePlan { dest, create_parent }
}
