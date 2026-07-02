use std::path::Path;

use wilkes_core::metadata::cache::FileIdentity;
use wilkes_core::types::{Bookmark, NewBookmark};

pub async fn load(path: &Path) -> anyhow::Result<Vec<Bookmark>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let json = tokio::fs::read_to_string(path).await?;
    Ok(serde_json::from_str(&json)?)
}

pub async fn save(path: &Path, bookmarks: &[Bookmark]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, serde_json::to_string_pretty(bookmarks)?).await?;
    Ok(())
}

/// Normalize free-text note input: trim, and treat an empty result as "no note"
/// so the model never stores a blank string (clearing a note ⇒ `None`).
fn normalize_note(note: Option<String>) -> Option<String> {
    note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
}

/// Content fingerprint of the file being bookmarked, stat-ed on disk. `None`
/// when the file is unreadable — the bookmark is still created, it just can't
/// be re-pointed if later renamed.
fn identity_for(path: &Path) -> Option<FileIdentity> {
    let meta = std::fs::metadata(path).ok()?;
    FileIdentity::from_fs(meta.len(), meta.modified().ok())
}

pub async fn add(path: &Path, new_bookmark: NewBookmark) -> anyhow::Result<Bookmark> {
    let mut bookmarks = load(path).await?;
    let identity = identity_for(&new_bookmark.path);
    let bookmark = Bookmark {
        id: uuid::Uuid::new_v4().to_string(),
        path: new_bookmark.path,
        origin: new_bookmark.origin,
        quote: new_bookmark.quote,
        created_at: chrono::Utc::now().to_rfc3339(),
        note: normalize_note(new_bookmark.note),
        rects: new_bookmark.rects,
        identity,
    };
    bookmarks.push(bookmark.clone());
    save(path, &bookmarks).await?;
    Ok(bookmark)
}

pub async fn remove(path: &Path, id: &str) -> anyhow::Result<()> {
    let mut bookmarks = load(path).await?;
    bookmarks.retain(|bookmark| bookmark.id != id);
    save(path, &bookmarks).await
}

/// Set (or clear, when `note` is empty/`None`) the note on an existing bookmark.
/// Errors if no bookmark matches `id` so a stale UI can surface the mismatch
/// rather than silently no-op.
pub async fn update_note(path: &Path, id: &str, note: Option<String>) -> anyhow::Result<Bookmark> {
    let mut bookmarks = load(path).await?;
    let bookmark = bookmarks
        .iter_mut()
        .find(|bookmark| bookmark.id == id)
        .ok_or_else(|| anyhow::anyhow!("bookmark not found: {id}"))?;
    bookmark.note = normalize_note(note);
    let updated = bookmark.clone();
    save(path, &bookmarks).await?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wilkes_core::types::{BoundingBox, SourceOrigin};

    fn new_bookmark() -> NewBookmark {
        NewBookmark {
            path: "/tmp/example.pdf".into(),
            origin: SourceOrigin::PdfPage {
                page: 3,
                bbox: Some(BoundingBox {
                    x: 1.0,
                    y: 2.0,
                    width: 30.0,
                    height: 4.0,
                }),
            },
            quote: "important passage".to_string(),
            note: None,
            rects: Vec::new(),
        }
    }

    #[tokio::test]
    async fn load_missing_returns_empty() {
        let dir = tempdir().unwrap();
        let bookmarks = load(&dir.path().join("bookmarks.json")).await.unwrap();
        assert!(bookmarks.is_empty());
    }

    #[tokio::test]
    async fn add_persists_bookmark_without_note() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");

        let bookmark = add(&path, new_bookmark()).await.unwrap();

        assert!(!bookmark.id.is_empty());
        assert_eq!(bookmark.quote, "important passage");
        assert!(bookmark.note.is_none());
        assert_eq!(load(&path).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn add_captures_file_identity_when_readable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");

        // Point the bookmark at a real file so its identity can be stat-ed.
        let file = dir.path().join("doc.pdf");
        std::fs::write(&file, b"content").unwrap();
        let mut nb = new_bookmark();
        nb.path = file.clone();

        let bookmark = add(&path, nb).await.unwrap();
        let expected = std::fs::metadata(&file)
            .ok()
            .and_then(|m| FileIdentity::from_fs(m.len(), m.modified().ok()));
        assert!(expected.is_some());
        assert_eq!(bookmark.identity, expected);
    }

    #[tokio::test]
    async fn add_leaves_identity_none_when_file_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        // new_bookmark() points at a nonexistent /tmp path.
        let bookmark = add(&path, new_bookmark()).await.unwrap();
        assert!(bookmark.identity.is_none());
    }

    #[tokio::test]
    async fn add_honors_and_trims_supplied_note() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");

        let mut nb = new_bookmark();
        nb.note = Some("  a note  ".to_string());
        let bookmark = add(&path, nb).await.unwrap();

        assert_eq!(bookmark.note.as_deref(), Some("a note"));
    }

    #[tokio::test]
    async fn update_note_sets_and_clears() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        let bookmark = add(&path, new_bookmark()).await.unwrap();

        let updated = update_note(&path, &bookmark.id, Some("  my note ".to_string()))
            .await
            .unwrap();
        assert_eq!(updated.note.as_deref(), Some("my note"));
        assert_eq!(
            load(&path).await.unwrap()[0].note.as_deref(),
            Some("my note")
        );

        // Blank input clears the note back to None.
        let cleared = update_note(&path, &bookmark.id, Some("   ".to_string()))
            .await
            .unwrap();
        assert!(cleared.note.is_none());
        assert!(load(&path).await.unwrap()[0].note.is_none());
    }

    #[tokio::test]
    async fn update_note_errors_on_unknown_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        add(&path, new_bookmark()).await.unwrap();

        let result = update_note(&path, "does-not-exist", Some("x".to_string())).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn remove_deletes_matching_bookmark() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        let bookmark = add(&path, new_bookmark()).await.unwrap();

        remove(&path, &bookmark.id).await.unwrap();

        assert!(load(&path).await.unwrap().is_empty());
    }
}
