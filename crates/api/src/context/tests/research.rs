use super::*;

#[tokio::test]
async fn mcp_library_edits_round_trip_through_the_existing_research_store() {
    use wilkes_agent::{
        library::{LibraryEdit as Edit, LibraryKind, Location},
        search::SearchService,
    };
    let dir = tempdir().unwrap();
    let (ctx, _rx, _loop) = AppContext::new(
        dir.path().into(),
        dir.path().join("settings.json"),
        WorkerPaths {
            python_override: None,
            python_package_dir: "pkg".into(),
            requirements_path: "r".into(),
            uv_bin: "v".into(),
            worker_bin: "w".into(),
            data_dir: dir.path().into(),
        },
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );
    let tag = ctx
        .clone()
        .edit_library(Edit::CreateTag {
            name: "Read next".into(),
            color: None,
        })
        .await
        .unwrap();
    let tags = ctx.clone().read_library(LibraryKind::Tags).await.unwrap();
    assert_eq!(tags[0]["id"], tag["id"]);
    let collection = ctx
        .clone()
        .edit_library(Edit::CreateCollection {
            name: "Everything".into(),
            expression: "true".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        ctx.list_collections().unwrap()[0].id,
        collection["id"].as_str().unwrap()
    );
    let path = dir.path().join("note.md");
    std::fs::write(&path, "A passage worth keeping").unwrap();
    let bookmark = ctx
        .clone()
        .edit_library(Edit::AddBookmark {
            path: path.clone(),
            location: Location::Text { line: 1, col: 0 },
            quote: "A passage worth keeping".into(),
            note: None,
        })
        .await
        .unwrap();
    ctx.clone()
        .edit_library(Edit::UpdateBookmarkNote {
            id: bookmark["id"].as_str().unwrap().into(),
            note: Some("Review this".into()),
        })
        .await
        .unwrap();
    let renamed = ctx
        .clone()
        .edit_library(Edit::RenameFile {
            path,
            new_name: "renamed.md".into(),
        })
        .await
        .unwrap();
    assert!(std::path::Path::new(renamed["path"].as_str().unwrap()).is_file());
    let bookmarks = ctx
        .clone()
        .read_library(LibraryKind::Bookmarks)
        .await
        .unwrap();
    assert_eq!(bookmarks[0]["note"], "Review this");
    assert_eq!(bookmarks[0]["path"], renamed["path"]);
    ctx.clone()
        .edit_library(Edit::RemoveBookmark {
            id: bookmark["id"].as_str().unwrap().into(),
        })
        .await
        .unwrap();
    ctx.clone()
        .edit_library(Edit::DeleteTag {
            id: tag["id"].as_str().unwrap().into(),
        })
        .await
        .unwrap();
    ctx.clone()
        .edit_library(Edit::DeleteCollection {
            id: collection["id"].as_str().unwrap().into(),
        })
        .await
        .unwrap();
    assert!(ctx.list_bookmarks().await.unwrap().is_empty());
    assert!(ctx.list_tags().unwrap().is_empty());
    assert!(ctx.list_collections().unwrap().is_empty());
}
