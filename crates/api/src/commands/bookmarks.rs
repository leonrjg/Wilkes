use std::path::Path;

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

pub async fn add(path: &Path, new_bookmark: NewBookmark) -> anyhow::Result<Bookmark> {
    let mut bookmarks = load(path).await?;
    let bookmark = Bookmark {
        id: uuid::Uuid::new_v4().to_string(),
        path: new_bookmark.path,
        origin: new_bookmark.origin,
        quote: new_bookmark.quote,
        created_at: chrono::Utc::now().to_rfc3339(),
        note: None,
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
            note: Some("ignored in v1".to_string()),
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
    async fn remove_deletes_matching_bookmark() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        let bookmark = add(&path, new_bookmark()).await.unwrap();

        remove(&path, &bookmark.id).await.unwrap();

        assert!(load(&path).await.unwrap().is_empty());
    }
}
