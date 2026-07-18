use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _};
use cel_interpreter::{Context, Program, Value};
use rusqlite::{params, Connection, OptionalExtension};
use wilkes_core::metadata::cache::FileIdentity;
use wilkes_core::types::{
    Bookmark, CollectionValidation, DocumentTagUpdate, FileEntry, NewBookmark, NewSmartCollection,
    NewTag, SearchLogEntry, SearchLogStatus, SearchQuery, SmartCollection, Tag,
    UpdateSmartCollection, UpdateTag,
};

const FILTER_SCHEMA_VERSION: i64 = 1;
const MAX_EXPRESSION_BYTES: usize = 8 * 1024;

pub struct ResearchStore {
    conn: Connection,
    programs: HashMap<String, (i64, Program)>,
}

pub struct SearchLogTracker {
    store: std::sync::Arc<std::sync::Mutex<ResearchStore>>,
    id: String,
    finished: bool,
    started: std::time::Instant,
    result_count: usize,
}

impl SearchLogTracker {
    pub fn new(store: std::sync::Arc<std::sync::Mutex<ResearchStore>>, id: String) -> Self {
        Self {
            store,
            id,
            finished: false,
            started: std::time::Instant::now(),
            result_count: 0,
        }
    }

    pub fn observe(&mut self, matches: usize) {
        self.result_count += matches;
    }

    pub fn finish(
        &mut self,
        status: SearchLogStatus,
        result_count: usize,
        duration_ms: u64,
        error: Option<String>,
    ) {
        if self.finished {
            return;
        }
        if let Ok(mut store) = self.store.lock() {
            let _ = store.finish_search_log(
                &self.id,
                status,
                result_count.max(self.result_count),
                duration_ms,
                error,
            );
        }
        self.finished = true;
    }
}

impl Drop for SearchLogTracker {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(
                SearchLogStatus::Cancelled,
                self.result_count,
                self.started.elapsed().as_millis() as u64,
                None,
            );
        }
    }
}

impl ResearchStore {
    pub fn open(data_dir: &Path, legacy_bookmarks: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let mut store = Self {
            conn: Connection::open(data_dir.join("research.db"))?,
            programs: HashMap::new(),
        };
        store.migrate()?;
        store.import_legacy_bookmarks(legacy_bookmarks)?;
        Ok(store)
    }

    fn migrate(&mut self) -> anyhow::Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        anyhow::ensure!(
            version <= 1,
            "research database schema {version} is newer than this app supports"
        );
        self.conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS schema_migrations (
               version INTEGER PRIMARY KEY, applied_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS document_refs (
               id TEXT PRIMARY KEY, path TEXT NOT NULL UNIQUE,
               size_bytes INTEGER, modified_at_ms INTEGER, last_seen_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS tags (
               id TEXT PRIMARY KEY, name TEXT NOT NULL, normalized_name TEXT NOT NULL UNIQUE,
               color TEXT, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS document_tags (
               document_id TEXT NOT NULL REFERENCES document_refs(id) ON DELETE CASCADE,
               tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE RESTRICT,
               created_at_ms INTEGER NOT NULL,
               PRIMARY KEY(document_id, tag_id)
             );
             CREATE TABLE IF NOT EXISTS smart_collections (
               id TEXT PRIMARY KEY, name TEXT NOT NULL, expression TEXT NOT NULL,
               filter_schema_version INTEGER NOT NULL, revision INTEGER NOT NULL,
               created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS bookmarks (
               id TEXT PRIMARY KEY, payload_json TEXT NOT NULL, created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS search_log (
               id TEXT PRIMARY KEY, query_json TEXT NOT NULL,
               collection_name TEXT, collection_revision INTEGER, initiated_by TEXT NOT NULL,
               started_at_ms INTEGER NOT NULL, completed_at_ms INTEGER,
               result_count INTEGER NOT NULL DEFAULT 0, duration_ms INTEGER,
               status TEXT NOT NULL, error_message TEXT
             );
             CREATE INDEX IF NOT EXISTS search_log_started_at ON search_log(started_at_ms DESC);",
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at_ms) VALUES(1, ?1)",
            [now_ms()],
        )?;
        self.conn.pragma_update(None, "user_version", 1)?;
        self.conn.execute(
            "UPDATE search_log SET status='cancelled', completed_at_ms=?1
             WHERE status='running'",
            [now_ms()],
        )?;
        Ok(())
    }

    fn import_legacy_bookmarks(&mut self, path: &Path) -> anyhow::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let already_imported: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM bookmarks", [], |r| r.get(0))?;
        if already_imported > 0 {
            return Ok(());
        }
        let bytes = std::fs::read(path)?;
        let bookmarks: Vec<Bookmark> = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse legacy bookmarks at {}", path.display()))?;
        let tx = self.conn.transaction()?;
        for bookmark in &bookmarks {
            tx.execute(
                "INSERT INTO bookmarks(id, payload_json, created_at) VALUES(?1, ?2, ?3)",
                params![
                    bookmark.id,
                    serde_json::to_string(bookmark)?,
                    bookmark.created_at
                ],
            )?;
        }
        let imported: i64 = tx.query_row("SELECT COUNT(*) FROM bookmarks", [], |r| r.get(0))?;
        anyhow::ensure!(
            imported == bookmarks.len() as i64,
            "bookmark migration count mismatch"
        );
        tx.commit()?;
        let mut backup = path.with_extension("json.migrated");
        if backup.exists() {
            backup = path.with_extension(format!("json.migrated-{}", uuid::Uuid::new_v4()));
        }
        std::fs::rename(path, backup)?;
        Ok(())
    }

    pub fn list_bookmarks(&self) -> anyhow::Result<Vec<Bookmark>> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload_json FROM bookmarks ORDER BY created_at")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn add_bookmark(&mut self, new: NewBookmark) -> anyhow::Result<Bookmark> {
        let note = new
            .note
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let bookmark = Bookmark {
            id: uuid::Uuid::new_v4().to_string(),
            identity: FileIdentity::for_path(&new.path),
            path: new.path,
            origin: new.origin,
            text_range: new.text_range,
            quote: new.quote,
            created_at: chrono::Utc::now().to_rfc3339(),
            note,
            rects: new.rects,
        };
        self.conn.execute(
            "INSERT INTO bookmarks(id, payload_json, created_at) VALUES(?1, ?2, ?3)",
            params![
                bookmark.id,
                serde_json::to_string(&bookmark)?,
                bookmark.created_at
            ],
        )?;
        Ok(bookmark)
    }

    pub fn remove_bookmark(&mut self, id: &str) -> anyhow::Result<()> {
        self.conn
            .execute("DELETE FROM bookmarks WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn update_bookmark_note(
        &mut self,
        id: &str,
        note: Option<String>,
    ) -> anyhow::Result<Bookmark> {
        let payload: String = self
            .conn
            .query_row(
                "SELECT payload_json FROM bookmarks WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("bookmark not found: {id}"))?;
        let mut bookmark: Bookmark = serde_json::from_str(&payload)?;
        bookmark.note = note
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.conn.execute(
            "UPDATE bookmarks SET payload_json = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(&bookmark)?],
        )?;
        Ok(bookmark)
    }

    pub fn list_tags(&self) -> anyhow::Result<Vec<Tag>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, color FROM tags ORDER BY normalized_name")?;
        let rows = stmt.query_map([], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                color: r.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn create_tag(&mut self, new: NewTag) -> anyhow::Result<Tag> {
        let name = normalize_name(&new.name)?;
        let tag = Tag {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            color: new.color,
        };
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO tags(id, name, normalized_name, color, created_at_ms, updated_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?5)",
            params![tag.id, tag.name, tag.name.to_lowercase(), tag.color, now],
        )?;
        Ok(tag)
    }

    pub fn update_tag(&mut self, id: &str, update: UpdateTag) -> anyhow::Result<Tag> {
        let name = normalize_name(&update.name)?;
        let changed = self.conn.execute(
            "UPDATE tags SET name=?2, normalized_name=?3, color=?4, updated_at_ms=?5 WHERE id=?1",
            params![id, name, name.to_lowercase(), update.color, now_ms()],
        )?;
        anyhow::ensure!(changed == 1, "tag not found: {id}");
        Ok(Tag {
            id: id.to_string(),
            name,
            color: update.color,
        })
    }

    pub fn delete_tag(&mut self, id: &str) -> anyhow::Result<()> {
        let needle = format!("%{id}%");
        let used: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM smart_collections WHERE expression LIKE ?1",
            [needle],
            |r| r.get(0),
        )?;
        anyhow::ensure!(used == 0, "tag is referenced by a smart collection");
        self.conn
            .execute("DELETE FROM document_tags WHERE tag_id=?1", [id])?;
        self.conn.execute("DELETE FROM tags WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn update_document_tags(&mut self, update: DocumentTagUpdate) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        for path in update.paths {
            let path_text = path.to_string_lossy().into_owned();
            let document_id: Option<String> = tx
                .query_row(
                    "SELECT id FROM document_refs WHERE path=?1",
                    [&path_text],
                    |r| r.get(0),
                )
                .optional()?;
            let document_id = document_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            tx.execute(
                "INSERT OR IGNORE INTO document_refs(id,path,last_seen_at_ms) VALUES(?1,?2,?3)",
                params![document_id, path_text, now_ms()],
            )?;
            for tag_id in &update.add_tag_ids {
                tx.execute(
                    "INSERT OR IGNORE INTO document_tags(document_id,tag_id,created_at_ms) VALUES(?1,?2,?3)",
                    params![document_id, tag_id, now_ms()],
                )?;
            }
            for tag_id in &update.remove_tag_ids {
                tx.execute(
                    "DELETE FROM document_tags WHERE document_id=?1 AND tag_id=?2",
                    params![document_id, tag_id],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn enrich_files(&mut self, entries: &mut [FileEntry]) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        let now = now_ms();
        for entry in entries.iter() {
            let path = entry.path.to_string_lossy().into_owned();
            let existing: Option<String> = tx
                .query_row("SELECT id FROM document_refs WHERE path=?1", [&path], |r| {
                    r.get(0)
                })
                .optional()?;
            if existing.is_none() {
                let mut stmt = tx.prepare(
                    "SELECT id,path FROM document_refs WHERE size_bytes=?1 AND modified_at_ms=?2",
                )?;
                let candidates: Vec<(String, String)> = stmt
                    .query_map(
                        params![entry.size_bytes as i64, entry.modified_at_ms],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?
                    .collect::<Result<_, _>>()?;
                let stale: Vec<_> = candidates
                    .into_iter()
                    .filter(|(_, old)| !Path::new(old).exists())
                    .collect();
                if stale.len() == 1 {
                    tx.execute(
                        "UPDATE document_refs SET path=?2,size_bytes=?3,modified_at_ms=?4,last_seen_at_ms=?5 WHERE id=?1",
                        params![stale[0].0, path, entry.size_bytes as i64, entry.modified_at_ms, now],
                    )?;
                    continue;
                }
            }
            tx.execute(
                "INSERT INTO document_refs(id,path,size_bytes,modified_at_ms,last_seen_at_ms)
                 VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(path) DO UPDATE SET size_bytes=excluded.size_bytes,
                   modified_at_ms=excluded.modified_at_ms,last_seen_at_ms=excluded.last_seen_at_ms",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    path,
                    entry.size_bytes as i64,
                    entry.modified_at_ms,
                    now
                ],
            )?;
        }
        tx.commit()?;

        let mut stmt = self.conn.prepare(
            "SELECT d.path,t.id,t.name,t.color FROM document_refs d
             JOIN document_tags dt ON dt.document_id=d.id JOIN tags t ON t.id=dt.tag_id
             ORDER BY t.normalized_name",
        )?;
        let mut by_path: HashMap<PathBuf, Vec<Tag>> = HashMap::new();
        let rows = stmt.query_map([], |r| {
            Ok((
                PathBuf::from(r.get::<_, String>(0)?),
                Tag {
                    id: r.get(1)?,
                    name: r.get(2)?,
                    color: r.get(3)?,
                },
            ))
        })?;
        for row in rows {
            let (path, tag) = row?;
            by_path.entry(path).or_default().push(tag);
        }
        for entry in entries {
            entry.tags = by_path.remove(&entry.path).unwrap_or_default();
        }
        Ok(())
    }

    pub fn rekey_document(&mut self, old: &Path, new: &Path) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        let old_path = old.to_string_lossy().into_owned();
        let new_path = new.to_string_lossy().into_owned();
        let old_id: Option<String> = tx
            .query_row(
                "SELECT id FROM document_refs WHERE path=?1",
                [&old_path],
                |r| r.get(0),
            )
            .optional()?;
        let Some(old_id) = old_id else {
            return Ok(());
        };
        let target_id: Option<String> = tx
            .query_row(
                "SELECT id FROM document_refs WHERE path=?1",
                [&new_path],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(target_id) = target_id.filter(|id| id != &old_id) {
            tx.execute(
                "INSERT OR IGNORE INTO document_tags(document_id,tag_id,created_at_ms)
                 SELECT ?1,tag_id,created_at_ms FROM document_tags WHERE document_id=?2",
                params![old_id, target_id],
            )?;
            tx.execute("DELETE FROM document_refs WHERE id=?1", [target_id])?;
        }
        tx.execute(
            "UPDATE document_refs SET path=?2,last_seen_at_ms=?3 WHERE id=?1",
            params![old_id, new_path, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn validate_collection(expression: &str) -> CollectionValidation {
        match validate_program(expression) {
            Ok(_) => CollectionValidation {
                valid: true,
                error: None,
            },
            Err(error) => CollectionValidation {
                valid: false,
                error: Some(error.to_string()),
            },
        }
    }

    pub fn list_collections(&self) -> anyhow::Result<Vec<SmartCollection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,name,expression,filter_schema_version,revision,created_at_ms,updated_at_ms
             FROM smart_collections ORDER BY lower(name)",
        )?;
        let rows = stmt.query_map([], collection_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn create_collection(
        &mut self,
        new: NewSmartCollection,
    ) -> anyhow::Result<SmartCollection> {
        validate_program(&new.expression)?;
        let name = normalize_name(&new.name)?;
        let now = now_ms();
        let item = SmartCollection {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            expression: new.expression,
            filter_schema_version: FILTER_SCHEMA_VERSION,
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.conn.execute(
            "INSERT INTO smart_collections(id,name,expression,filter_schema_version,revision,created_at_ms,updated_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![item.id,item.name,item.expression,item.filter_schema_version,item.revision,item.created_at_ms,item.updated_at_ms],
        )?;
        Ok(item)
    }

    pub fn update_collection(
        &mut self,
        id: &str,
        update: UpdateSmartCollection,
    ) -> anyhow::Result<SmartCollection> {
        validate_program(&update.expression)?;
        let name = normalize_name(&update.name)?;
        let changed = self.conn.execute(
            "UPDATE smart_collections SET name=?2,expression=?3,revision=revision+1,updated_at_ms=?4 WHERE id=?1",
            params![id,name,update.expression,now_ms()],
        )?;
        anyhow::ensure!(changed == 1, "smart collection not found: {id}");
        self.programs.remove(id);
        self.collection(id)?
            .ok_or_else(|| anyhow!("smart collection not found: {id}"))
    }

    pub fn delete_collection(&mut self, id: &str) -> anyhow::Result<()> {
        self.conn
            .execute("DELETE FROM smart_collections WHERE id=?1", [id])?;
        self.programs.remove(id);
        Ok(())
    }

    pub fn collection(&self, id: &str) -> anyhow::Result<Option<SmartCollection>> {
        self.conn.query_row(
            "SELECT id,name,expression,filter_schema_version,revision,created_at_ms,updated_at_ms
             FROM smart_collections WHERE id=?1",
            [id], collection_from_row,
        ).optional().map_err(Into::into)
    }

    pub fn eligible_paths(
        &mut self,
        collection_id: &str,
        root: &Path,
        entries: &[FileEntry],
    ) -> anyhow::Result<HashSet<PathBuf>> {
        let collection = self
            .collection(collection_id)?
            .ok_or_else(|| anyhow!("smart collection not found: {collection_id}"))?;
        let needs_compile = !matches!(
            self.programs.get(collection_id),
            Some((revision, _)) if *revision == collection.revision
        );
        if needs_compile {
            self.programs.insert(
                collection_id.to_string(),
                (
                    collection.revision,
                    validate_program(&collection.expression)?,
                ),
            );
        }
        let (_, program) = self.programs.get(collection_id).expect("inserted above");
        eligible_paths_for_program(program, root, entries)
    }

    pub fn eligible_paths_for_expression(
        &self,
        expression: &str,
        root: &Path,
        entries: &[FileEntry],
    ) -> anyhow::Result<HashSet<PathBuf>> {
        let program = validate_program(expression)?;
        eligible_paths_for_program(&program, root, entries)
    }
}

fn eligible_paths_for_program(
    program: &Program,
    root: &Path,
    entries: &[FileEntry],
) -> anyhow::Result<HashSet<PathBuf>> {
    let mut eligible = HashSet::new();
    for entry in entries {
        if evaluate(program, root, entry)? {
            eligible.insert(entry.path.clone());
        }
    }
    Ok(eligible)
}

impl ResearchStore {
    pub fn start_search_log(
        &mut self,
        query: &SearchQuery,
        initiated_by: &str,
    ) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let collection = query
            .collection_id
            .as_deref()
            .map(|id| self.collection(id))
            .transpose()?
            .flatten();
        self.conn.execute(
            "INSERT INTO search_log(id,query_json,collection_name,collection_revision,initiated_by,started_at_ms,status)
             VALUES(?1,?2,?3,?4,?5,?6,'running')",
            params![id,serde_json::to_string(query)?,collection.as_ref().map(|c| &c.name),collection.as_ref().map(|c| c.revision),initiated_by,now_ms()],
        )?;
        Ok(id)
    }

    pub fn finish_search_log(
        &mut self,
        id: &str,
        status: SearchLogStatus,
        result_count: usize,
        duration_ms: u64,
        error: Option<String>,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE search_log SET completed_at_ms=?2,result_count=?3,duration_ms=?4,status=?5,error_message=?6 WHERE id=?1",
            params![id,now_ms(),result_count as i64,duration_ms as i64,status_text(&status),error],
        )?;
        self.conn.execute(
            "DELETE FROM search_log WHERE id IN (SELECT id FROM search_log ORDER BY started_at_ms DESC LIMIT -1 OFFSET 1000)",
            [],
        )?;
        Ok(())
    }

    pub fn list_search_log(&self, limit: usize) -> anyhow::Result<Vec<SearchLogEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,query_json,collection_name,collection_revision,initiated_by,started_at_ms,
                    completed_at_ms,result_count,duration_ms,status,error_message
             FROM search_log ORDER BY started_at_ms DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit.clamp(1, 1000) as i64], |r| {
            let query_json: String = r.get(1)?;
            Ok((
                r.get::<_, String>(0)?,
                query_json,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, String>(9)?,
                r.get(10)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                query_json,
                collection_name,
                collection_revision,
                initiated_by,
                started_at_ms,
                completed_at_ms,
                result_count,
                duration_ms,
                status,
                error_message,
            ) = row?;
            Ok(SearchLogEntry {
                id,
                query: serde_json::from_str(&query_json)?,
                collection_name,
                collection_revision,
                initiated_by,
                started_at_ms,
                completed_at_ms,
                result_count: result_count as usize,
                duration_ms: duration_ms.map(|v| v as u64),
                status: parse_status(&status)?,
                error_message,
            })
        })
        .collect()
    }

    pub fn delete_search_log(&mut self, id: &str) -> anyhow::Result<()> {
        self.conn
            .execute("DELETE FROM search_log WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn clear_search_log(&mut self) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM search_log", [])?;
        Ok(())
    }
}

fn collection_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SmartCollection> {
    Ok(SmartCollection {
        id: row.get(0)?,
        name: row.get(1)?,
        expression: row.get(2)?,
        filter_schema_version: row.get(3)?,
        revision: row.get(4)?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
    })
}

fn validate_program(expression: &str) -> anyhow::Result<Program> {
    anyhow::ensure!(
        !expression.trim().is_empty(),
        "collection expression cannot be empty"
    );
    anyhow::ensure!(
        expression.len() <= MAX_EXPRESSION_BYTES,
        "collection expression is too long"
    );
    let program = Program::compile(expression).map_err(|e| anyhow!(e.to_string()))?;
    let sample = FileEntry {
        path: PathBuf::from("/library/example.pdf"),
        size_bytes: 1,
        file_type: wilkes_core::types::FileType::Pdf,
        extension: "pdf".into(),
        created_at_ms: None,
        modified_at_ms: None,
        title: Some("Example".into()),
        author: Some("Author".into()),
        publication_date: Some("2024-01".into()),
        citation_count: Some(1),
        metadata_conflicts: Default::default(),
        tags: Vec::new(),
    };
    match program.execute(&context_for(Path::new("/library"), &sample)) {
        Ok(Value::Bool(_)) => Ok(program),
        Ok(value) => Err(anyhow!(
            "collection expression must return bool, got {value:?}"
        )),
        Err(error) => Err(anyhow!("invalid collection expression: {error}")),
    }
}

fn evaluate(program: &Program, root: &Path, entry: &FileEntry) -> anyhow::Result<bool> {
    match program.execute(&context_for(root, entry)) {
        Ok(Value::Bool(value)) => Ok(value),
        Ok(value) => Err(anyhow!(
            "collection expression returned {value:?}, expected bool"
        )),
        // A validated expression can still be undefined for an individual
        // document (for example `citation_count > 1` when citation_count is
        // null). Missing per-document data means the document does not match;
        // it must not abort the whole listing and leave stale UI state behind.
        Err(_) => Ok(false),
    }
}

fn context_for(root: &Path, entry: &FileEntry) -> Context<'static> {
    let mut context = Context::default();
    context.add_variable_from_value(
        "tags",
        entry.tags.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
    );
    context.add_variable_from_value("title", option_value(entry.title.as_deref()));
    context.add_variable_from_value("author", option_value(entry.author.as_deref()));
    let year = entry
        .publication_date
        .as_deref()
        .and_then(|v| v.get(..4))
        .and_then(|v| v.parse::<i64>().ok());
    context.add_variable_from_value(
        "publication_year",
        year.map(Value::Int).unwrap_or(Value::Null),
    );
    context.add_variable_from_value(
        "citation_count",
        entry.citation_count.map(Value::Int).unwrap_or(Value::Null),
    );
    context.add_variable_from_value(
        "file_type",
        match entry.file_type {
            wilkes_core::types::FileType::Pdf => "pdf",
            _ => "text",
        }
        .to_string(),
    );
    context.add_variable_from_value("extension", entry.extension.clone());
    context.add_variable_from_value("root", root.to_string_lossy().into_owned());
    context.add_variable_from_value("path", entry.path.to_string_lossy().into_owned());
    context
}

fn option_value(value: Option<&str>) -> Value {
    value
        .map(|v| Value::String(std::sync::Arc::new(v.to_string())))
        .unwrap_or(Value::Null)
}

fn normalize_name(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    anyhow::ensure!(!value.is_empty(), "name cannot be empty");
    anyhow::ensure!(value.chars().count() <= 100, "name is too long");
    Ok(value.to_string())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn status_text(status: &SearchLogStatus) -> &'static str {
    match status {
        SearchLogStatus::Running => "running",
        SearchLogStatus::Completed => "completed",
        SearchLogStatus::Cancelled => "cancelled",
        SearchLogStatus::Failed => "failed",
    }
}
fn parse_status(value: &str) -> anyhow::Result<SearchLogStatus> {
    Ok(match value {
        "running" => SearchLogStatus::Running,
        "completed" => SearchLogStatus::Completed,
        "cancelled" => SearchLogStatus::Cancelled,
        "failed" => SearchLogStatus::Failed,
        _ => return Err(anyhow!("unknown search log status: {value}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn tags_and_collection_membership_round_trip() {
        let dir = tempdir().unwrap();
        let mut store =
            ResearchStore::open(dir.path(), &dir.path().join("bookmarks.json")).unwrap();
        let tag = store
            .create_tag(NewTag {
                name: "Reviewed".into(),
                color: None,
            })
            .unwrap();
        let path = dir.path().join("paper.pdf");
        std::fs::write(&path, b"pdf").unwrap();
        let mut entries = vec![FileEntry {
            path: path.clone(),
            size_bytes: 3,
            file_type: wilkes_core::types::FileType::Pdf,
            extension: "pdf".into(),
            created_at_ms: None,
            modified_at_ms: None,
            title: None,
            author: None,
            publication_date: None,
            citation_count: None,
            metadata_conflicts: Default::default(),
            tags: vec![],
        }];
        store.enrich_files(&mut entries).unwrap();
        store
            .update_document_tags(DocumentTagUpdate {
                paths: vec![path.clone()],
                add_tag_ids: vec![tag.id.clone()],
                remove_tag_ids: vec![],
            })
            .unwrap();
        store.enrich_files(&mut entries).unwrap();
        let collection = store
            .create_collection(NewSmartCollection {
                name: "Reviewed".into(),
                expression: format!("'{}' in tags", tag.id),
            })
            .unwrap();
        let eligible = store
            .eligible_paths(&collection.id, dir.path(), &entries)
            .unwrap();
        assert!(eligible.contains(&path));
    }

    #[test]
    fn migrates_legacy_bookmarks_once_and_disables_json() {
        let dir = tempdir().unwrap();
        let legacy = dir.path().join("bookmarks.json");
        std::fs::write(&legacy, r#"[{"id":"old","path":"/tmp/paper.pdf","origin":{"PdfPage":{"page":1,"bbox":null}},"quote":"quote","created_at":"2026-01-01T00:00:00Z"}]"#).unwrap();
        let store = ResearchStore::open(dir.path(), &legacy).unwrap();
        assert_eq!(store.list_bookmarks().unwrap().len(), 1);
        assert!(!legacy.exists());
        assert!(dir.path().join("bookmarks.json.migrated").exists());
        drop(store);
        let reopened = ResearchStore::open(dir.path(), &legacy).unwrap();
        assert_eq!(reopened.list_bookmarks().unwrap().len(), 1);
    }

    #[test]
    fn search_log_records_effective_query_and_completion() {
        let dir = tempdir().unwrap();
        let mut store =
            ResearchStore::open(dir.path(), &dir.path().join("bookmarks.json")).unwrap();
        let query = SearchQuery {
            pattern: "methods".into(),
            is_regex: false,
            case_sensitive: false,
            root: dir.path().into(),
            max_results: 20,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: Default::default(),
            scope: Default::default(),
            supported_extensions: vec!["pdf".into()],
            collection_id: None,
            tag_ids: Vec::new(),
        };
        let id = store.start_search_log(&query, "test").unwrap();
        store
            .finish_search_log(&id, SearchLogStatus::Completed, 7, 12, None)
            .unwrap();
        let entries = store.list_search_log(10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].query.pattern, "methods");
        assert_eq!(entries[0].result_count, 7);
        assert_eq!(entries[0].status, SearchLogStatus::Completed);
    }

    #[test]
    fn collection_comparison_treats_missing_metadata_as_non_match() {
        let dir = tempdir().unwrap();
        let mut store =
            ResearchStore::open(dir.path(), &dir.path().join("bookmarks.json")).unwrap();
        let collection = store
            .create_collection(NewSmartCollection {
                name: "Cited".into(),
                expression: "citation_count > 1".into(),
            })
            .unwrap();
        let entry = |name: &str, citation_count| FileEntry {
            path: dir.path().join(name),
            size_bytes: 1,
            file_type: wilkes_core::types::FileType::Pdf,
            extension: "pdf".into(),
            created_at_ms: None,
            modified_at_ms: None,
            title: None,
            author: None,
            publication_date: None,
            citation_count,
            metadata_conflicts: Default::default(),
            tags: vec![],
        };
        let entries = vec![entry("missing.pdf", None), entry("cited.pdf", Some(2))];

        let eligible = store
            .eligible_paths(&collection.id, dir.path(), &entries)
            .unwrap();

        assert!(!eligible.contains(&dir.path().join("missing.pdf")));
        assert!(eligible.contains(&dir.path().join("cited.pdf")));
    }

    #[test]
    fn draft_expression_uses_collection_evaluator_without_persisting() {
        let dir = tempdir().unwrap();
        let store = ResearchStore::open(dir.path(), &dir.path().join("bookmarks.json")).unwrap();
        let entry = |name: &str, extension: &str| FileEntry {
            path: dir.path().join(name),
            size_bytes: 1,
            file_type: wilkes_core::types::FileType::PlainText,
            extension: extension.into(),
            created_at_ms: None,
            modified_at_ms: None,
            title: None,
            author: None,
            publication_date: None,
            citation_count: None,
            metadata_conflicts: Default::default(),
            tags: vec![],
        };
        let entries = vec![entry("notes.md", "md"), entry("data.txt", "txt")];

        let eligible = store
            .eligible_paths_for_expression("extension == 'md'", dir.path(), &entries)
            .unwrap();

        assert_eq!(eligible, HashSet::from([dir.path().join("notes.md")]));
        assert!(store.list_collections().unwrap().is_empty());
    }
}
