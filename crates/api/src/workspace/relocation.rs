//! One-time upload relocation during workspace adoption. The recorded mapping
//! survives a restart after the old directory has disappeared. Reapplying it
//! is harmless: destination paths are outside every old prefix.

use std::path::{Path, PathBuf};

use anyhow::Context;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use super::{atomic_write_json, WorkspaceManifest};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct UploadRelocation {
    old: PathBuf,
    new: PathBuf,
}

pub(super) fn plan(
    data_dir: &Path,
    settings_path: &Path,
    workspace_dir: &Path,
) -> anyhow::Result<Vec<UploadRelocation>> {
    // Match the canonical spelling used by search and index keys even when
    // the workspace directory has not been created yet (e.g. /var vs /private/var).
    let ancestor = workspace_dir
        .ancestors()
        .find(|path| path.exists())
        .context("workspace destination has no existing ancestor")?;
    let destination = ancestor
        .canonicalize()?
        .join(workspace_dir.strip_prefix(ancestor)?)
        .join("uploads");
    let mut mappings = Vec::new();
    let mut sources = vec![data_dir.join("uploads")];
    if let Some(config_dir) = settings_path.parent() {
        let source = config_dir.join("uploads");
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    for source in sources {
        if source.exists() || destination.exists() {
            let canonical = if source.exists() {
                Some(source.canonicalize()?)
            } else if let Some(parent) = source.parent().filter(|parent| parent.exists()) {
                Some(parent.canonicalize()?.join("uploads"))
            } else {
                None
            };
            if let Some(canonical) = canonical {
                if canonical != source {
                    mappings.push(UploadRelocation {
                        old: canonical,
                        new: destination.clone(),
                    });
                }
            }
            mappings.push(UploadRelocation {
                old: source,
                new: destination.clone(),
            });
        }
    }
    Ok(mappings)
}

fn remap(path: &mut PathBuf, mappings: &[UploadRelocation]) {
    for mapping in mappings {
        if let Ok(relative) = path.strip_prefix(&mapping.old) {
            *path = if relative.as_os_str().is_empty() {
                mapping.new.clone()
            } else {
                mapping.new.join(relative)
            };
            break;
        }
    }
}

pub(super) fn remap_manifest(manifest: &mut WorkspaceManifest, mappings: &[UploadRelocation]) {
    for path in manifest
        .favorites
        .iter_mut()
        .chain(manifest.recent_roots.iter_mut())
        .chain(manifest.active_root.iter_mut())
    {
        remap(path, mappings);
    }
}

// Only structured path fields are rewritten. Prose, queries, notes and model
// replies may mention an old filename and must retain their original text.
fn remap_json(value: &mut serde_json::Value, mappings: &[UploadRelocation]) -> bool {
    let mut changed = false;
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                if matches!(
                    key.as_str(),
                    "path" | "file_path" | "root" | "root_path" | "cwd"
                ) {
                    if let Some(stored) = value.as_str() {
                        let mut path = PathBuf::from(stored);
                        remap(&mut path, mappings);
                        if path != Path::new(stored) {
                            *value =
                                serde_json::to_value(path).expect("path serializes as a string");
                            changed = true;
                        }
                    }
                } else {
                    changed |= remap_json(value, mappings);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                changed |= remap_json(value, mappings);
            }
        }
        _ => {}
    }
    changed
}

fn remap_database(path: &Path, mappings: &[UploadRelocation]) -> anyhow::Result<()> {
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(3))?;
    let tx = conn.transaction()?;
    // Explicit columns across the historical schemas. This changes paths
    // without upgrading the index or touching vectors (including schema v1).
    for (table, column, json) in [
        ("files", "file_path", false),
        ("chunks", "file_path", false),
        ("indexed_roots", "path", false),
        ("document_refs", "path", false),
        ("bookmarks", "payload_json", true),
        ("search_log", "query_json", true),
    ] {
        let exists = tx
            .prepare(&format!(
                "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
            ))?
            .exists([column])?;
        if !exists {
            continue;
        }
        let rows = tx
            .prepare(&format!(
                "SELECT rowid, {column} FROM {table} WHERE {column} IS NOT NULL"
            ))?
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (id, stored) in rows {
            let updated = if json {
                let mut value = serde_json::from_str(&stored)?;
                if !remap_json(&mut value, mappings) {
                    continue;
                }
                serde_json::to_string(&value)?
            } else {
                let mut path = PathBuf::from(&stored);
                remap(&mut path, mappings);
                if path == Path::new(&stored) {
                    continue;
                }
                path.to_string_lossy().into_owned()
            };
            tx.execute(
                &format!("UPDATE {table} SET {column} = ?1 WHERE rowid = ?2"),
                params![updated, id],
            )?;
        }
    }
    let has_meta = tx
        .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta'")?
        .exists([])?;
    if has_meta {
        for key in ["root_path", "root_canonical_path"] {
            let rows = tx
                .prepare("SELECT value FROM meta WHERE key = ?1")?
                .query_map([key], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for stored in rows {
                let mut path = PathBuf::from(&stored);
                remap(&mut path, mappings);
                if path != Path::new(&stored) {
                    tx.execute(
                        "UPDATE meta SET value = ?1 WHERE key = ?2",
                        params![path.to_string_lossy(), key],
                    )?;
                }
            }
        }
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn remap_stores(
    workspace_dir: &Path,
    mappings: &[UploadRelocation],
) -> anyhow::Result<()> {
    if mappings.is_empty() {
        return Ok(());
    }
    for name in [
        "semantic_index.db",
        "semantic_index.db.tmp",
        "semantic_index.db.replacement-backup",
        "file_metadata.db",
        "research.db",
    ] {
        let path = workspace_dir.join(name);
        if path.exists() {
            remap_database(&path, mappings)
                .with_context(|| format!("relocate upload references in {}", path.display()))?;
        }
    }
    for name in [
        "bookmarks.json",
        "bookmarks.json.migrated",
        "chat-conversations.json",
        "semantic_index.status.json",
    ] {
        let path = workspace_dir.join(name);
        if path.exists() {
            let mut value = serde_json::from_slice(&std::fs::read(&path)?)
                .with_context(|| format!("read upload references in {}", path.display()))?;
            if remap_json(&mut value, mappings) {
                atomic_write_json(&path, &value)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{
        initialize_workspace_registry, migration_plan_path, read_manifest, workspace_manifest_path,
        MigrationPlan,
    };

    #[test]
    fn relocation_preserves_a_real_semantic_index_and_its_vectors() {
        use wilkes_core::embed::index::{chunk::Chunk, db::PreparedFile, SemanticIndex};
        use wilkes_core::types::{ByteRange, EmbeddingEngine, SourceOrigin};
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("uploads");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let file = old.join("paper.txt");
        std::fs::write(&file, "hello world").unwrap();
        let mut index =
            SemanticIndex::create(&workspace, "m", 3, EmbeddingEngine::Candle, Some(&old)).unwrap();
        index.activate_root(&old).unwrap();
        index
            .write_file(PreparedFile {
                path: file.clone(),
                full_text: "hello world".into(),
                retained: Default::default(),
                chunks: vec![(
                    Chunk {
                        file_path: file,
                        text: "hello world".into(),
                        byte_range: ByteRange { start: 0, end: 11 },
                        origin: SourceOrigin::TextFile { line: 1, col: 1 },
                    },
                    vec![1.0, 0.0, 0.0],
                )],
            })
            .unwrap();
        drop(index);
        let new = workspace.join("uploads");
        std::fs::rename(&old, &new).unwrap();
        remap_stores(
            &workspace,
            &plan(dir.path(), &dir.path().join("settings.json"), &workspace).unwrap(),
        )
        .unwrap();
        let index = SemanticIndex::open(&workspace, "m", 3).unwrap();
        let results = index.query(&[1.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].file_path,
            new.join("paper.txt").canonicalize().unwrap()
        );
        assert_eq!(results[0].chunk_text, "hello world");
        assert_eq!(index.status_for_root(Some(&new)).indexed_files, 1);
    }

    #[test]
    fn fresh_migration_relocates_uploads_from_a_separate_config_directory() {
        let data = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let settings = config.path().join("settings.json");
        let old = config.path().join("uploads");
        std::fs::create_dir_all(old.join("nested")).unwrap();
        std::fs::write(old.join("nested/doc.txt"), "document").unwrap();
        let external = config.path().join("uploads-other");
        std::fs::create_dir_all(&external).unwrap();
        atomic_write_json(
            &settings,
            &serde_json::json!({
                "bookmarked_dirs": [old.canonicalize().unwrap(), &external],
                "recent_dirs": [old.join("nested")], "last_directory": old.join("nested")
            }),
        )
        .unwrap();
        let id = initialize_workspace_registry(data.path(), &settings).unwrap();
        let new = data
            .path()
            .canonicalize()
            .unwrap()
            .join("workspaces")
            .join(&id)
            .join("uploads");
        let manifest = read_manifest(&workspace_manifest_path(data.path(), &id)).unwrap();
        assert_eq!(manifest.favorites, vec![new.clone(), external]);
        assert_eq!(manifest.active_root, Some(new.join("nested")));
        assert_eq!(manifest.recent_roots, vec![new.join("nested")]);
        assert!(new.join("nested/doc.txt").is_file());
        assert!(!old.exists());
    }

    #[test]
    fn migration_relocates_upload_roots_and_references_and_resumes_after_the_move() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().canonicalize().unwrap();
        let settings = data.join("settings.json");
        let old = data.join("uploads");
        let nested = old.join("café");
        let external = data.join("uploads-other");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        let file = nested.join("paper.txt");
        std::fs::write(&file, "retained bytes").unwrap();
        atomic_write_json(
            &settings,
            &serde_json::json!({
                "bookmarked_dirs": [&old, &external], "recent_dirs": [&nested],
                "last_directory": &nested, "theme": "Dark"
            }),
        )
        .unwrap();
        atomic_write_json(
            &data.join("bookmarks.json"),
            &serde_json::json!([
                {"path": &file, "quote": file.to_string_lossy(), "note": "preserve this"}
            ]),
        )
        .unwrap();
        let conn = Connection::open(data.join("semantic_index.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
            INSERT INTO meta VALUES ('schema_version', '1');
            CREATE TABLE chunks (id INTEGER PRIMARY KEY, file_path TEXT, chunk_text TEXT);
            CREATE TABLE vec_chunks (id INTEGER PRIMARY KEY, embedding BLOB);
            INSERT INTO vec_chunks VALUES (7, X'01020304');",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meta VALUES ('root_path', ?1)",
            [old.to_string_lossy()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chunks VALUES (7, ?1, 'unchanged text')",
            [file.to_string_lossy()],
        )
        .unwrap();
        drop(conn);

        // Simulate interruption with the directory already moved and the old
        // paths still in every store. The persisted map must finish the work.
        let id = "migration-test";
        let workspace = data.join("workspaces").join(id);
        std::fs::create_dir_all(&workspace).unwrap();
        let mappings = plan(&data, &settings, &workspace).unwrap();
        let mut manifest = WorkspaceManifest::new(id.into(), "Default".into());
        manifest.favorites = vec![old.clone(), external.clone()];
        manifest.recent_roots = vec![nested.clone()];
        manifest.active_root = Some(nested.clone());
        atomic_write_json(
            &migration_plan_path(&data),
            &MigrationPlan {
                workspace_id: id.into(),
                manifest,
                upload_relocations: mappings,
            },
        )
        .unwrap();
        std::fs::rename(&old, workspace.join("uploads")).unwrap();
        assert_eq!(initialize_workspace_registry(&data, &settings).unwrap(), id);
        let destination = workspace.join("uploads");
        let relocated_file = destination.join("café/paper.txt");
        assert_eq!(
            std::fs::read_to_string(&relocated_file).unwrap(),
            "retained bytes"
        );
        let manifest = read_manifest(&workspace_manifest_path(&data, id)).unwrap();
        assert_eq!(manifest.favorites, vec![destination.clone(), external]);
        assert_eq!(manifest.recent_roots, vec![destination.join("café")]);
        assert_eq!(manifest.active_root, Some(destination.join("café")));
        let conn = Connection::open(workspace.join("semantic_index.db")).unwrap();
        let stored: String = conn
            .query_row("SELECT file_path FROM chunks WHERE id=7", [], |r| r.get(0))
            .unwrap();
        assert_eq!(PathBuf::from(stored), relocated_file);
        let schema: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(schema, "1");
        let vector: Vec<u8> = conn
            .query_row("SELECT embedding FROM vec_chunks WHERE id=7", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(vector, vec![1, 2, 3, 4]);
        let bookmark: serde_json::Value =
            serde_json::from_slice(&std::fs::read(workspace.join("bookmarks.json")).unwrap())
                .unwrap();
        assert_eq!(bookmark[0]["path"], serde_json::json!(relocated_file));
        assert_eq!(bookmark[0]["quote"], file.to_string_lossy().as_ref());
        // Replaying each store relocation is idempotent.
        remap_stores(&workspace, &plan(&data, &settings, &workspace).unwrap()).unwrap();
    }

    #[test]
    fn database_relocation_preserves_document_ids_and_bookmark_notes() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("research.db");
        let old = PathBuf::from("/old/é/uploads");
        let new = PathBuf::from("/new/uploads");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE document_refs (id TEXT PRIMARY KEY, path TEXT UNIQUE);
            CREATE TABLE bookmarks (id TEXT PRIMARY KEY, payload_json TEXT);
            CREATE TABLE search_log (id TEXT PRIMARY KEY, query_json TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO document_refs VALUES ('document-id', ?1)",
            [old.join("doc.txt").to_string_lossy()],
        )
        .unwrap();
        let payload =
            serde_json::json!({"path": old.join("doc.txt"), "note": old.to_string_lossy()});
        conn.execute(
            "INSERT INTO bookmarks VALUES ('bookmark-id', ?1)",
            [payload.to_string()],
        )
        .unwrap();
        conn.execute("INSERT INTO search_log VALUES ('search-id', ?1)", [serde_json::json!({"root": &old, "scope": {"path": old.join("doc.txt")}, "pattern": old.to_string_lossy()}).to_string()]).unwrap();
        drop(conn);
        let mappings = vec![UploadRelocation {
            old: old.clone(),
            new: new.clone(),
        }];
        remap_database(&db, &mappings).unwrap();
        remap_database(&db, &mappings).unwrap();
        let conn = Connection::open(db).unwrap();
        let path: String = conn
            .query_row(
                "SELECT path FROM document_refs WHERE id='document-id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(PathBuf::from(path), new.join("doc.txt"));
        let stored: String = conn
            .query_row(
                "SELECT payload_json FROM bookmarks WHERE id='bookmark-id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let bookmark: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(bookmark["path"], serde_json::json!(new.join("doc.txt")));
        assert_eq!(bookmark["note"], old.to_string_lossy().as_ref());
        let stored: String = conn
            .query_row("SELECT query_json FROM search_log", [], |r| r.get(0))
            .unwrap();
        let query: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(
            query["scope"]["path"],
            serde_json::json!(new.join("doc.txt"))
        );
        assert_eq!(query["pattern"], old.to_string_lossy().as_ref());
    }
}
