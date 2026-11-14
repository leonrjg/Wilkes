//! Feeds and the items they have listed, in the workspace's research
//! database. The items are the seen record: an item is new when its link key
//! is not among its feed's rows, and a row is never deleted while its feed
//! exists.

use std::collections::HashSet;
use std::path::PathBuf;

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use wilkes_core::feeds::FeedSource;
use wilkes_core::types::LiteratureSearchResult;

use super::ResearchStore;

/// A feed, with what its last refresh found.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FeedRecord {
    pub id: String,
    pub name: String,
    pub source: FeedSource,
    /// The library root its files go under.
    pub root: PathBuf,
    /// The folder under `root`, `/`-separated.
    pub folder: String,
    pub auto_import: bool,
    pub created_at_ms: i64,
    pub last_attempt_at_ms: Option<i64>,
    pub last_success_at_ms: Option<i64>,
    pub last_error: Option<String>,
    /// The source asked to be left alone until then.
    pub retry_after_ms: Option<i64>,
    /// Every item the last refresh returned was new, so older ones may have
    /// gone by unseen.
    pub may_have_missed: bool,
    /// Items waiting in the inbox.
    #[serde(default)]
    pub inbox_count: i64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedItemStatus {
    /// Waiting for the user.
    Inbox,
    Imported,
    Dismissed,
    /// Known and never shown: beyond the newest on a first refresh.
    Seen,
}

impl FeedItemStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Imported => "imported",
            Self::Dismissed => "dismissed",
            Self::Seen => "seen",
        }
    }

    fn parse(value: &str) -> rusqlite::Result<Self> {
        Ok(match value {
            "inbox" => Self::Inbox,
            "imported" => Self::Imported,
            "dismissed" => Self::Dismissed,
            "seen" => Self::Seen,
            other => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    format!("unknown feed item status {other:?}").into(),
                ))
            }
        })
    }
}

/// One item a feed listed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FeedItemRecord {
    pub id: String,
    pub feed_id: String,
    pub link_key: String,
    pub link: String,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    /// RFC 3339, as the source gave it; `None` when it gave no real date, and
    /// the item is shown by `first_seen_at_ms` instead.
    pub published: Option<String>,
    pub first_seen_at_ms: i64,
    pub status: FeedItemStatus,
    /// The page shows only the start of a paid post.
    pub preview_only: bool,
    /// The item as a literature result: a provider's own, or one written for
    /// a post from what its source said.
    pub work: LiteratureSearchResult,
    pub imported_path: Option<PathBuf>,
    pub last_error: Option<String>,
}

const FEED_COLUMNS: &str = "id, name, source_json, root, folder, auto_import, created_at_ms,
    last_attempt_at_ms, last_success_at_ms, last_error, retry_after_ms, may_have_missed,
    (SELECT COUNT(*) FROM feed_items i WHERE i.feed_id = feeds.id AND i.status = 'inbox')";

const ITEM_COLUMNS: &str = "id, feed_id, link_key, link, doi, arxiv_id, published,
    first_seen_at_ms, status, preview_only, work_json, imported_path, last_error";

fn json_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn feed_row(row: &Row<'_>) -> rusqlite::Result<FeedRecord> {
    let source: String = row.get(2)?;
    Ok(FeedRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        source: serde_json::from_str(&source).map_err(json_error)?,
        root: PathBuf::from(row.get::<_, String>(3)?),
        folder: row.get(4)?,
        auto_import: row.get::<_, i64>(5)? != 0,
        created_at_ms: row.get(6)?,
        last_attempt_at_ms: row.get(7)?,
        last_success_at_ms: row.get(8)?,
        last_error: row.get(9)?,
        retry_after_ms: row.get(10)?,
        may_have_missed: row.get::<_, i64>(11)? != 0,
        inbox_count: row.get(12)?,
    })
}

fn item_row(row: &Row<'_>) -> rusqlite::Result<FeedItemRecord> {
    let work: String = row.get(10)?;
    Ok(FeedItemRecord {
        id: row.get(0)?,
        feed_id: row.get(1)?,
        link_key: row.get(2)?,
        link: row.get(3)?,
        doi: row.get(4)?,
        arxiv_id: row.get(5)?,
        published: row.get(6)?,
        first_seen_at_ms: row.get(7)?,
        status: FeedItemStatus::parse(&row.get::<_, String>(8)?)?,
        preview_only: row.get::<_, i64>(9)? != 0,
        work: serde_json::from_str(&work).map_err(json_error)?,
        imported_path: row.get::<_, Option<String>>(11)?.map(PathBuf::from),
        last_error: row.get(12)?,
    })
}

impl ResearchStore {
    pub fn insert_feed(&mut self, feed: &FeedRecord) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO feeds(id, name, source_json, root, folder, auto_import, created_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                feed.id,
                feed.name,
                serde_json::to_string(&feed.source)?,
                feed.root.to_string_lossy(),
                feed.folder,
                feed.auto_import as i64,
                feed.created_at_ms,
            ],
        )?;
        Ok(())
    }

    pub fn feeds(&self) -> anyhow::Result<Vec<FeedRecord>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FEED_COLUMNS} FROM feeds ORDER BY name COLLATE NOCASE, created_at_ms"
        ))?;
        let rows = stmt.query_map([], feed_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn feed(&self, id: &str) -> anyhow::Result<Option<FeedRecord>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {FEED_COLUMNS} FROM feeds WHERE id = ?1"),
                [id],
                feed_row,
            )
            .optional()?)
    }

    /// Change a feed's name, folder or auto-import switch; a `None` leaves
    /// that one as it is.
    pub fn update_feed(
        &mut self,
        id: &str,
        name: Option<&str>,
        folder: Option<&str>,
        auto_import: Option<bool>,
    ) -> anyhow::Result<FeedRecord> {
        let changed = self.conn.execute(
            "UPDATE feeds SET name = COALESCE(?2, name), folder = COALESCE(?3, folder),
               auto_import = COALESCE(?4, auto_import) WHERE id = ?1",
            params![id, name, folder, auto_import.map(|on| on as i64)],
        )?;
        anyhow::ensure!(changed == 1, "no feed with id {id}");
        self.feed(id)?
            .ok_or_else(|| anyhow::anyhow!("no feed with id {id}"))
    }

    /// Remove a feed and every item it listed, returning what it was.
    pub fn remove_feed(&mut self, id: &str) -> anyhow::Result<Option<FeedRecord>> {
        let feed = self.feed(id)?;
        // Items are deleted with their feed by the foreign key's cascade.
        self.conn.execute("DELETE FROM feeds WHERE id = ?1", [id])?;
        Ok(feed)
    }

    pub fn feed_attempted(&mut self, id: &str, at_ms: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE feeds SET last_attempt_at_ms = ?2 WHERE id = ?1",
            params![id, at_ms],
        )?;
        Ok(())
    }

    /// A refresh that failed: its error is shown, and nothing stored is
    /// touched. `retry_after_ms` is set only when the source asked for it.
    pub fn feed_failed(
        &mut self,
        id: &str,
        error: &str,
        retry_after_ms: Option<i64>,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE feeds SET last_error = ?2, retry_after_ms = ?3 WHERE id = ?1",
            params![id, error, retry_after_ms],
        )?;
        Ok(())
    }

    pub fn feed_succeeded(
        &mut self,
        id: &str,
        at_ms: i64,
        may_have_missed: bool,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE feeds SET last_success_at_ms = ?2, last_error = NULL, retry_after_ms = NULL,
               may_have_missed = ?3 WHERE id = ?1",
            params![id, at_ms, may_have_missed as i64],
        )?;
        Ok(())
    }

    /// The link keys a feed has listed: its seen record.
    pub fn feed_item_keys(&self, feed_id: &str) -> anyhow::Result<HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT link_key FROM feed_items WHERE feed_id = ?1")?;
        let rows = stmt.query_map([feed_id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Store one new item, in a transaction of its own. A link key the feed
    /// already listed is refused by the unique index.
    pub fn insert_feed_item(&mut self, item: &FeedItemRecord) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            &format!("INSERT INTO feed_items({ITEM_COLUMNS}) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"),
            params![
                item.id,
                item.feed_id,
                item.link_key,
                item.link,
                item.doi,
                item.arxiv_id,
                item.published,
                item.first_seen_at_ms,
                item.status.as_str(),
                item.preview_only as i64,
                serde_json::to_string(&item.work)?,
                item.imported_path.as_ref().map(|path| path.to_string_lossy().into_owned()),
                item.last_error,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn feed_item(&self, id: &str) -> anyhow::Result<Option<FeedItemRecord>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {ITEM_COLUMNS} FROM feed_items WHERE id = ?1"),
                [id],
                item_row,
            )
            .optional()?)
    }

    /// Items by status and feed, newest first by publication date or, for an
    /// item with none, by when it was first seen.
    pub fn feed_items(
        &self,
        status: Option<FeedItemStatus>,
        feed_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<FeedItemRecord>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM feed_items
             WHERE (?1 IS NULL OR status = ?1) AND (?2 IS NULL OR feed_id = ?2)
             ORDER BY COALESCE(published,
                 strftime('%Y-%m-%dT%H:%M:%SZ', first_seen_at_ms / 1000, 'unixepoch')) DESC,
               first_seen_at_ms DESC, id
             LIMIT ?3 OFFSET ?4"
        ))?;
        let rows = stmt.query_map(
            params![
                status.map(FeedItemStatus::as_str),
                feed_id,
                limit as i64,
                offset as i64
            ],
            item_row,
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn inbox_total(&self) -> anyhow::Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM feed_items WHERE status = 'inbox'",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn set_feed_item_status(&mut self, id: &str, status: FeedItemStatus) -> anyhow::Result<()> {
        let changed = self.conn.execute(
            "UPDATE feed_items SET status = ?2 WHERE id = ?1",
            params![id, status.as_str()],
        )?;
        anyhow::ensure!(changed == 1, "no feed item with id {id}");
        Ok(())
    }

    pub fn feed_item_imported(&mut self, id: &str, path: &std::path::Path) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE feed_items SET status = 'imported', imported_path = ?2, last_error = NULL
             WHERE id = ?1",
            params![id, path.to_string_lossy()],
        )?;
        Ok(())
    }

    /// An item that could not be imported keeps its error and stays where it
    /// was.
    pub fn feed_item_failed(&mut self, id: &str, error: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE feed_items SET last_error = ?2 WHERE id = ?1",
            params![id, error],
        )?;
        Ok(())
    }

    pub fn feed_item_preview_only(&mut self, id: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE feed_items SET preview_only = 1, last_error = NULL WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    /// Of `dois`, those a saved work already holds.
    pub fn saved_work_dois(&self, dois: &[String]) -> anyhow::Result<HashSet<String>> {
        let mut held = HashSet::new();
        let mut stmt = self
            .conn
            .prepare_cached("SELECT 1 FROM saved_works WHERE doi = ?1 LIMIT 1")?;
        for doi in dois {
            if stmt.exists([doi])? {
                held.insert(doi.clone());
            }
        }
        Ok(held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wilkes_core::types::{LiteratureAcquisition, NewSavedWork};

    fn work(id: &str, doi: Option<&str>) -> LiteratureSearchResult {
        LiteratureSearchResult {
            id: id.into(),
            doi: doi.map(str::to_string),
            title: Some(id.into()),
            year: None,
            publication_date: None,
            venue: None,
            citation_count: None,
            is_open_access: false,
            pdf_url: None,
            landing_page_url: None,
            image_url: None,
            open_access_status: None,
            license: None,
            authors: None,
            publisher: None,
            language: None,
            file_format: None,
            file_size: None,
            summary: None,
            subject: None,
            kind: None,
            pages: None,
            recall_score: None,
            acquisition: LiteratureAcquisition::None,
        }
    }

    fn feed(id: &str) -> FeedRecord {
        FeedRecord {
            id: id.into(),
            name: "Blog".into(),
            source: FeedSource::Syndication {
                url: "https://example.org/feed".into(),
            },
            root: "/library".into(),
            folder: "Feeds/Blog".into(),
            auto_import: false,
            created_at_ms: 1,
            last_attempt_at_ms: None,
            last_success_at_ms: None,
            last_error: None,
            retry_after_ms: None,
            may_have_missed: false,
            inbox_count: 0,
        }
    }

    fn item(id: &str, feed_id: &str, key: &str, published: Option<&str>, seen: i64) -> FeedItemRecord {
        FeedItemRecord {
            id: id.into(),
            feed_id: feed_id.into(),
            link_key: key.into(),
            link: key.into(),
            doi: None,
            arxiv_id: None,
            published: published.map(str::to_string),
            first_seen_at_ms: seen,
            status: FeedItemStatus::Inbox,
            preview_only: false,
            work: work(id, None),
            imported_path: None,
            last_error: None,
        }
    }

    /// A schema-5 database, as the release before feeds left it, with a saved
    /// work in it.
    fn schema_five(dir: &std::path::Path) {
        let conn = rusqlite::Connection::open(dir.join("research.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE saved_works (
               id TEXT PRIMARY KEY, provider TEXT NOT NULL, work_id TEXT NOT NULL,
               doi TEXT, payload_json TEXT NOT NULL, saved_at TEXT NOT NULL,
               UNIQUE(provider, work_id)
             );
             PRAGMA user_version = 5;",
        )
        .unwrap();
    }

    #[test]
    fn schema_five_opens_as_six_with_its_saved_works_untouched() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = ResearchStore::open(dir.path(), &dir.path().join("none.json")).unwrap();
            store
                .save_work(NewSavedWork {
                    provider: "openalex".into(),
                    work: work("W1", Some("10.1/abc")),
                    note: None,
                })
                .unwrap();
        }
        let conn = rusqlite::Connection::open(dir.path().join("research.db")).unwrap();
        conn.pragma_update(None, "user_version", 5).unwrap();
        conn.execute_batch("DROP TABLE feed_items; DROP TABLE feeds;").unwrap();
        drop(conn);

        let store = ResearchStore::open(dir.path(), &dir.path().join("none.json")).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 6);
        assert_eq!(store.list_saved_works().unwrap().len(), 1);
        assert_eq!(
            store.saved_work_dois(&["10.1/abc".into(), "10.1/zzz".into()]).unwrap(),
            HashSet::from(["10.1/abc".to_string()])
        );
        assert!(store.feeds().unwrap().is_empty());
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        schema_five(dir.path());
        let conn = rusqlite::Connection::open(dir.path().join("research.db")).unwrap();
        conn.pragma_update(None, "user_version", 7).unwrap();
        drop(conn);
        assert!(ResearchStore::open(dir.path(), &dir.path().join("none.json")).is_err());
    }

    #[test]
    fn a_link_key_is_listed_once_per_feed_and_items_go_with_their_feed() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ResearchStore::open(dir.path(), &dir.path().join("none.json")).unwrap();
        store.insert_feed(&feed("f1")).unwrap();
        store.insert_feed(&feed("f2")).unwrap();
        store
            .insert_feed_item(&item("i1", "f1", "https://example.org/a", None, 10))
            .unwrap();
        assert!(store
            .insert_feed_item(&item("i2", "f1", "https://example.org/a", None, 11))
            .is_err());
        // Another feed may list the same link.
        store
            .insert_feed_item(&item("i3", "f2", "https://example.org/a", None, 12))
            .unwrap();
        assert_eq!(store.feed("f1").unwrap().unwrap().inbox_count, 1);
        assert_eq!(store.inbox_total().unwrap(), 2);

        store.remove_feed("f1").unwrap();
        assert!(store.feed_item("i1").unwrap().is_none());
        assert!(store.feed_item("i3").unwrap().is_some());
    }

    #[test]
    fn items_are_newest_first_by_date_or_by_when_they_were_first_seen() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ResearchStore::open(dir.path(), &dir.path().join("none.json")).unwrap();
        store.insert_feed(&feed("f")).unwrap();
        // 2026-11-01 and 2026-11-03 as first-seen times, in ms.
        store
            .insert_feed_item(&item("old", "f", "k1", Some("2026-10-01T00:00:00Z"), 1_793_491_200_000))
            .unwrap();
        store
            .insert_feed_item(&item("undated", "f", "k2", None, 1_793_664_000_000))
            .unwrap();
        store
            .insert_feed_item(&item("new", "f", "k3", Some("2026-11-02T00:00:00Z"), 1_793_491_200_000))
            .unwrap();
        let order: Vec<String> = store
            .feed_items(Some(FeedItemStatus::Inbox), None, 0, 10)
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(order, vec!["undated", "new", "old"]);

        store.set_feed_item_status("new", FeedItemStatus::Dismissed).unwrap();
        assert_eq!(store.feed_items(Some(FeedItemStatus::Inbox), Some("f"), 0, 10).unwrap().len(), 2);
        assert_eq!(store.feed_item_keys("f").unwrap().len(), 3);
    }
}
