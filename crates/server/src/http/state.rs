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
    UploadWritePlan {
        dest,
        create_parent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use tempfile::tempdir;

    struct FakeFs {
        canonical: HashMap<PathBuf, PathBuf>,
    }

    #[async_trait]
    impl ServerFs for FakeFs {
        async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            self.canonical
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing"))
        }

        async fn read(&self, _path: &Path) -> io::Result<Vec<u8>> {
            Ok(Vec::new())
        }

        async fn write(&self, _path: &Path, _data: &[u8]) -> io::Result<()> {
            Ok(())
        }

        async fn create_dir_all(&self, _path: &Path) -> io::Result<()> {
            Ok(())
        }

        async fn remove_file(&self, _path: &Path) -> io::Result<()> {
            Ok(())
        }

        async fn remove_dir_all(&self, _path: &Path) -> io::Result<()> {
            Ok(())
        }

        async fn dir_size(&self, _path: &Path) -> anyhow::Result<u64> {
            Ok(0)
        }
    }

    #[test]
    fn sanitize_relative_upload_path_strips_traversal_and_separators() {
        let path = sanitize_relative_upload_path("../foo//bar\\..\\baz");
        assert_eq!(path, PathBuf::from("foo/bar/baz"));
    }

    #[test]
    fn validate_delete_target_rejects_root_and_accepts_file_targets() {
        let err = validate_delete_target(Path::new("")).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        let target = match validate_delete_target(Path::new("nested/file.txt")) {
            Ok(target) => target,
            Err(_) => panic!("expected delete target validation to succeed"),
        };
        assert_eq!(target.canonical, PathBuf::from("nested/file.txt"));
        assert_eq!(target.kind, DeleteKind::File);
    }

    #[test]
    fn upload_write_plan_sets_destination_and_parent() {
        let plan = upload_write_plan(Path::new("/uploads"), Path::new("dir/file.txt"));
        assert_eq!(plan.dest, PathBuf::from("/uploads/dir/file.txt"));
        assert_eq!(plan.create_parent, Some(PathBuf::from("/uploads/dir")));
    }

    #[test]
    fn mime_for_path_covers_common_variants() {
        assert_eq!(mime_for_path(Path::new("a.pdf")), "application/pdf");
        assert_eq!(mime_for_path(Path::new("a.md")), "text/plain");
        assert_eq!(mime_for_path(Path::new("a.html")), "text/html");
        assert_eq!(mime_for_path(Path::new("a.json")), "application/json");
        assert_eq!(mime_for_path(Path::new("a.png")), "image/png");
        assert_eq!(mime_for_path(Path::new("a.jpg")), "image/jpeg");
        assert_eq!(
            mime_for_path(Path::new("a.bin")),
            "application/octet-stream"
        );
    }

    #[tokio::test]
    async fn asset_and_search_plans_enforce_upload_boundaries() {
        let dir = tempdir().unwrap();
        let uploads = dir.path().join("uploads");
        let file = uploads.join("docs/note.txt");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "hello").unwrap();

        let fake = FakeFs {
            canonical: HashMap::from([
                (uploads.clone(), uploads.canonicalize().unwrap()),
                (file.clone(), file.canonicalize().unwrap()),
            ]),
        };

        let asset = match asset_access_plan(&file, &uploads, &fake).await {
            Ok(asset) => asset,
            Err(_) => panic!("expected asset access to succeed"),
        };
        assert_eq!(asset.canonical, file.canonicalize().unwrap());
        assert_eq!(asset.content_type, "text/plain");

        let outside = dir.path().join("outside.txt");
        let denied = match asset_access_plan(&outside, &uploads, &fake).await {
            Ok(_) => panic!("expected asset access to fail"),
            Err(err) => err,
        };
        assert_eq!(denied.0, StatusCode::NOT_FOUND);

        let search_root =
            match confined_root_for_search(&uploads.to_string_lossy(), &uploads, &fake).await {
                Ok(path) => path,
                Err(_) => panic!("expected search root confinement to succeed"),
            };
        assert_eq!(search_root, uploads.canonicalize().unwrap());
    }

    #[tokio::test]
    async fn asset_and_search_plans_reject_outside_roots() {
        let dir = tempdir().unwrap();
        let uploads = dir.path().join("uploads");
        std::fs::create_dir_all(&uploads).unwrap();
        let file = uploads.join("docs/note.txt");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "hello").unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();

        let fake = FakeFs {
            canonical: HashMap::from([
                (uploads.clone(), uploads.canonicalize().unwrap()),
                (file.clone(), file.canonicalize().unwrap()),
                (outside.clone(), outside.canonicalize().unwrap()),
            ]),
        };

        let err = confined_root_for_search(&outside.to_string_lossy(), &uploads, &fake)
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn asset_and_search_plans_reject_missing_paths() {
        let dir = tempdir().unwrap();
        let uploads = dir.path().join("uploads");
        std::fs::create_dir_all(&uploads).unwrap();
        let missing = uploads.join("missing.txt");

        let fake = FakeFs {
            canonical: HashMap::from([(uploads.clone(), uploads.canonicalize().unwrap())]),
        };

        let asset_err = asset_access_plan(&missing, &uploads, &fake)
            .await
            .unwrap_err();
        assert_eq!(asset_err.0, StatusCode::NOT_FOUND);

        let search_err = confined_root_for_search(&missing.to_string_lossy(), &uploads, &fake)
            .await
            .unwrap_err();
        assert_eq!(search_err.0, StatusCode::NOT_FOUND);
    }
}
