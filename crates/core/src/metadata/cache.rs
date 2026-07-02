//! Persistent cache of extracted document metadata, keyed by file identity.
//!
//! Extracting document metadata (e.g. a PDF's publication date) means opening
//! and parsing each file, which is far too expensive to redo on every file
//! listing. This cache stores the result once per `(size, modified-time)`
//! identity so repeated listings are cheap.
//!
//! It is the single owner of *file identity*: the mapping from a content
//! fingerprint `(size_bytes, modified_at_ms)` to a path. A rename preserves the
//! fingerprint but changes the path, so identity lets callers re-key an existing
//! row instead of re-extracting identical content.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};

use crate::types::DocumentMetadata;

const SCHEMA_VERSION: i64 = 2;

fn cache_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("file_metadata.db")
}

/// Provenance of a cached metadata row. `Zotero` rows carry authoritative
/// library data; `File` rows carry only file-based extraction. The distinction
/// drives invalidation when the Zotero integration is toggled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataSource {
    File,
    Zotero,
}

impl MetadataSource {
    fn as_str(self) -> &'static str {
        match self {
            MetadataSource::File => "file",
            MetadataSource::Zotero => "zotero",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "zotero" => MetadataSource::Zotero,
            _ => MetadataSource::File,
        }
    }
}

/// A cached row reconstructed for re-processing (e.g. Zotero backfill).
#[derive(Clone, Debug)]
pub struct CachedRow {
    pub path: std::path::PathBuf,
    pub identity: FileIdentity,
    pub source: MetadataSource,
    pub metadata: DocumentMetadata,
}

/// A file's content fingerprint. Preserved across renames, invalidated on edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileIdentity {
    pub size_bytes: i64,
    pub modified_at_ms: i64,
}

impl FileIdentity {
    /// Derive an identity from filesystem metadata. Returns `None` when the
    /// modified time is unavailable or cannot be represented.
    pub fn from_fs(size_bytes: u64, modified: Option<SystemTime>) -> Option<Self> {
        let modified_at_ms = modified
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok())?;
        Some(Self {
            size_bytes: i64::try_from(size_bytes).unwrap_or(i64::MAX),
            modified_at_ms,
        })
    }
}

pub struct MetadataCache {
    conn: Connection,
}

impl MetadataCache {
    /// Open the cache at `data_dir`, creating it if necessary. If the on-disk
    /// schema version differs, the table is dropped and recreated: it is only a
    /// cache, so rebuilding is always safe.
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        let path = cache_path(data_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create data dir for metadata cache: {}",
                    parent.display()
                )
            })?;
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("Failed to open metadata cache at {}", path.display()))?;

        let stored_version: Option<i64> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(s.parse::<i64>().unwrap_or(0))
                },
            )
            .optional()
            .unwrap_or(None);

        if stored_version != Some(SCHEMA_VERSION) {
            conn.execute_batch("DROP TABLE IF EXISTS file_metadata; DROP TABLE IF EXISTS meta;")?;
        }
        Self::create_schema(&conn)?;

        Ok(Self { conn })
    }

    fn create_schema(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS file_metadata (
                path             TEXT PRIMARY KEY,
                size_bytes       INTEGER NOT NULL,
                modified_at_ms   INTEGER NOT NULL,
                extracted_at_ms  INTEGER NOT NULL,
                source           TEXT NOT NULL DEFAULT 'file',
                title            TEXT,
                author           TEXT,
                doi              TEXT,
                publication_date TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_file_metadata_identity
                ON file_metadata(size_bytes, modified_at_ms);
            ",
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    fn key(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// Return cached metadata for `path` only if the stored identity still
    /// matches — i.e. the file has not been edited since extraction.
    pub fn get_valid(
        &self,
        path: &Path,
        identity: FileIdentity,
    ) -> anyhow::Result<Option<DocumentMetadata>> {
        let row = self
            .conn
            .query_row(
                "SELECT title, author, doi, publication_date
                 FROM file_metadata
                 WHERE path = ?1 AND size_bytes = ?2 AND modified_at_ms = ?3",
                params![
                    Self::key(path),
                    identity.size_bytes,
                    identity.modified_at_ms
                ],
                |row| {
                    Ok(DocumentMetadata {
                        title: row.get(0)?,
                        author: row.get(1)?,
                        doi: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Find the prior path of a file that was renamed *to* `current`: the unique
    /// cached row sharing this identity whose path differs from `current` and no
    /// longer exists on disk. On-disk existence is the ground truth that
    /// disambiguates when the destination path already carries a (stale) row of
    /// its own — a plain fingerprint match would see two rows and give up.
    /// Returns `None` when there is no such row, or when more than one still-
    /// missing candidate remains (genuinely ambiguous, e.g. duplicate content).
    pub fn find_rename_source(
        &self,
        current: &Path,
        identity: FileIdentity,
    ) -> anyhow::Result<Option<PathBuf>> {
        let mut stmt = self.conn.prepare(
            "SELECT path FROM file_metadata
             WHERE size_bytes = ?1 AND modified_at_ms = ?2",
        )?;
        let candidates = stmt
            .query_map(
                params![identity.size_bytes, identity.modified_at_ms],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let current_key = Self::key(current);
        let mut sources = candidates
            .into_iter()
            .filter(|p| *p != current_key && !Path::new(p).exists())
            .collect::<Vec<_>>();
        if sources.len() == 1 {
            Ok(Some(PathBuf::from(sources.pop().expect("len checked"))))
        } else {
            Ok(None)
        }
    }

    /// Find the current on-disk path for content identified by `identity`: the
    /// unique cached row sharing this fingerprint whose path still exists on
    /// disk. Mirrors [`find_rename_source`](Self::find_rename_source) for
    /// callers that hold a *stale* path (e.g. a bookmark) and need the live one
    /// after a rename. Returns `None` when no such row exists, or when more than
    /// one on-disk file shares the fingerprint (ambiguous, e.g. duplicate
    /// content), so callers never silently re-point to the wrong file.
    pub fn find_current_path(&self, identity: FileIdentity) -> anyhow::Result<Option<PathBuf>> {
        let mut stmt = self.conn.prepare(
            "SELECT path FROM file_metadata
             WHERE size_bytes = ?1 AND modified_at_ms = ?2",
        )?;
        let candidates = stmt
            .query_map(
                params![identity.size_bytes, identity.modified_at_ms],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut existing = candidates
            .into_iter()
            .filter(|p| Path::new(p).exists())
            .collect::<Vec<_>>();
        if existing.len() == 1 {
            Ok(Some(PathBuf::from(existing.pop().expect("len checked"))))
        } else {
            Ok(None)
        }
    }

    /// Re-key an existing row from `old` to `new` without re-extracting. Any
    /// stale row already occupying `new` is removed first.
    pub fn rename(&self, old: &Path, new: &Path) -> anyhow::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM file_metadata WHERE path = ?1",
            params![Self::key(new)],
        )?;
        tx.execute(
            "UPDATE file_metadata SET path = ?1 WHERE path = ?2",
            params![Self::key(new), Self::key(old)],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Insert or replace the cached metadata for `path` at the given identity,
    /// tagging it with its provenance.
    pub fn upsert(
        &self,
        path: &Path,
        identity: FileIdentity,
        metadata: &DocumentMetadata,
        source: MetadataSource,
    ) -> anyhow::Result<()> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(0);
        self.conn.execute(
            "INSERT OR REPLACE INTO file_metadata
                (path, size_bytes, modified_at_ms, extracted_at_ms, source,
                 title, author, doi, publication_date)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                Self::key(path),
                identity.size_bytes,
                identity.modified_at_ms,
                now_ms,
                source.as_str(),
                metadata.title,
                metadata.author,
                metadata.doi,
                metadata.created_at,
            ],
        )?;
        Ok(())
    }

    /// Reconstruct all rows with the given provenance, for re-processing (e.g.
    /// upgrading `File` rows to `Zotero` after the integration is enabled).
    pub fn list_by_source(&self, source: MetadataSource) -> anyhow::Result<Vec<CachedRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, size_bytes, modified_at_ms, source, title, author, doi, publication_date
             FROM file_metadata WHERE source = ?1",
        )?;
        let rows = stmt
            .query_map(params![source.as_str()], |row| {
                let path: String = row.get(0)?;
                Ok(CachedRow {
                    path: std::path::PathBuf::from(path),
                    identity: FileIdentity {
                        size_bytes: row.get(1)?,
                        modified_at_ms: row.get(2)?,
                    },
                    source: MetadataSource::from_str(&row.get::<_, String>(3)?),
                    metadata: DocumentMetadata {
                        title: row.get(4)?,
                        author: row.get(5)?,
                        doi: row.get(6)?,
                        created_at: row.get(7)?,
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Drop all Zotero-sourced rows. Used when the integration is disabled so
    /// those files revert to file-based extraction on the next listing.
    pub fn invalidate_zotero(&self) -> anyhow::Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM file_metadata WHERE source = ?1",
            params![MetadataSource::Zotero.as_str()],
        )?)
    }

    /// Drop every cached row. Backs the manual "refresh metadata" action.
    pub fn clear(&self) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM file_metadata", [])?;
        Ok(())
    }

    /// Drop only file-sourced rows, preserving authoritative Zotero rows. Backs
    /// a metadata refresh performed while Zotero is unreachable: file-based rows
    /// can always be re-extracted locally, but Zotero rows cannot be rebuilt
    /// until the library is reachable again, so clearing them would silently
    /// destroy metadata with no way to recover it.
    pub fn clear_file_rows(&self) -> anyhow::Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM file_metadata WHERE source = ?1",
            params![MetadataSource::File.as_str()],
        )?)
    }

    /// Remove any cached row for `path`.
    pub fn remove(&self, path: &Path) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM file_metadata WHERE path = ?1",
            params![Self::key(path)],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample() -> DocumentMetadata {
        DocumentMetadata {
            title: Some("Title".into()),
            author: Some("Author".into()),
            doi: Some("10.1/x".into()),
            created_at: Some("2021-05".into()),
        }
    }

    #[test]
    fn test_upsert_and_get_valid() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 100,
            modified_at_ms: 42,
        };
        let path = Path::new("/docs/a.pdf");

        assert!(cache.get_valid(path, id).unwrap().is_none());
        cache
            .upsert(path, id, &sample(), MetadataSource::File)
            .unwrap();
        assert_eq!(cache.get_valid(path, id).unwrap(), Some(sample()));
    }

    #[test]
    fn test_get_valid_invalidated_when_identity_changes() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 100,
            modified_at_ms: 42,
        };
        let path = Path::new("/docs/a.pdf");
        cache
            .upsert(path, id, &sample(), MetadataSource::File)
            .unwrap();

        let edited = FileIdentity {
            size_bytes: 100,
            modified_at_ms: 99,
        };
        assert!(cache.get_valid(path, edited).unwrap().is_none());
    }

    #[test]
    fn test_find_rename_source_resolves_when_destination_already_cached() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 100,
            modified_at_ms: 42,
        };

        // `old` was renamed away and no longer exists on disk. `new` exists and
        // already carries a stale row of its own (e.g. from a prior reindex),
        // so a plain fingerprint match would see two rows and give up.
        let old = dir.path().join("old.pdf");
        let new = dir.path().join("new.pdf");
        std::fs::write(&new, b"content").unwrap();
        cache
            .upsert(&old, id, &sample(), MetadataSource::File)
            .unwrap();
        cache
            .upsert(&new, id, &sample(), MetadataSource::File)
            .unwrap();

        assert_eq!(
            cache.find_rename_source(&new, id).unwrap(),
            Some(old.clone())
        );
    }

    #[test]
    fn test_find_rename_source_none_when_ambiguous_or_present() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 100,
            modified_at_ms: 42,
        };
        let new = dir.path().join("new.pdf");
        std::fs::write(&new, b"content").unwrap();

        // No other row: nothing to rekey from.
        cache
            .upsert(&new, id, &sample(), MetadataSource::File)
            .unwrap();
        assert!(cache.find_rename_source(&new, id).unwrap().is_none());

        // Two distinct missing candidates sharing the fingerprint: genuinely
        // ambiguous, so callers must not treat it as a rename.
        cache
            .upsert(
                &dir.path().join("gone_a.pdf"),
                id,
                &sample(),
                MetadataSource::File,
            )
            .unwrap();
        cache
            .upsert(
                &dir.path().join("gone_b.pdf"),
                id,
                &sample(),
                MetadataSource::File,
            )
            .unwrap();
        assert!(cache.find_rename_source(&new, id).unwrap().is_none());
    }

    #[test]
    fn test_find_current_path_resolves_and_rejects_ambiguity() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 100,
            modified_at_ms: 42,
        };

        // Content was renamed old -> new: the cache now carries a row at `new`
        // (as the metadata fill / watcher re-keys it), and the old path is gone
        // (never written to disk here), leaving `new` as the only live match.
        let new = dir.path().join("new.pdf");
        std::fs::write(&new, b"content").unwrap();
        cache
            .upsert(&new, id, &sample(), MetadataSource::File)
            .unwrap();

        // A stale holder of `old` resolves forward to the live `new`.
        assert_eq!(cache.find_current_path(id).unwrap(), Some(new.clone()));

        // A second on-disk file sharing the fingerprint makes it ambiguous:
        // never guess.
        let dup = dir.path().join("dup.pdf");
        std::fs::write(&dup, b"content").unwrap();
        cache
            .upsert(&dup, id, &sample(), MetadataSource::File)
            .unwrap();
        assert!(cache.find_current_path(id).unwrap().is_none());

        // No on-disk file with the fingerprint (row still points at the missing
        // path) yields nothing rather than a dead path.
        let missing_id = FileIdentity {
            size_bytes: 7,
            modified_at_ms: 9,
        };
        cache
            .upsert(
                &dir.path().join("ghost.pdf"),
                missing_id,
                &sample(),
                MetadataSource::File,
            )
            .unwrap();
        assert!(cache.find_current_path(missing_id).unwrap().is_none());
    }

    #[test]
    fn test_rename_rekeys_without_reextract() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 100,
            modified_at_ms: 42,
        };
        let old = Path::new("/docs/old.pdf");
        let new = Path::new("/docs/new.pdf");
        cache
            .upsert(old, id, &sample(), MetadataSource::File)
            .unwrap();

        cache.rename(old, new).unwrap();

        assert!(cache.get_valid(old, id).unwrap().is_none());
        assert_eq!(cache.get_valid(new, id).unwrap(), Some(sample()));
    }

    #[test]
    fn test_remove() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        let path = Path::new("/docs/a.pdf");
        cache
            .upsert(path, id, &sample(), MetadataSource::File)
            .unwrap();
        cache.remove(path).unwrap();
        assert!(cache.get_valid(path, id).unwrap().is_none());
    }

    #[test]
    fn test_invalidate_zotero_keeps_file_rows() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        cache
            .upsert(Path::new("/f.pdf"), id, &sample(), MetadataSource::File)
            .unwrap();
        cache
            .upsert(Path::new("/z.pdf"), id, &sample(), MetadataSource::Zotero)
            .unwrap();

        assert_eq!(cache.invalidate_zotero().unwrap(), 1);
        assert!(cache.get_valid(Path::new("/z.pdf"), id).unwrap().is_none());
        assert!(cache.get_valid(Path::new("/f.pdf"), id).unwrap().is_some());
    }

    #[test]
    fn test_list_by_source_and_clear() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 7,
            modified_at_ms: 9,
        };
        cache
            .upsert(Path::new("/f.pdf"), id, &sample(), MetadataSource::File)
            .unwrap();
        cache
            .upsert(Path::new("/z.pdf"), id, &sample(), MetadataSource::Zotero)
            .unwrap();

        let file_rows = cache.list_by_source(MetadataSource::File).unwrap();
        assert_eq!(file_rows.len(), 1);
        assert_eq!(file_rows[0].path, Path::new("/f.pdf"));
        assert_eq!(file_rows[0].identity, id);

        cache.clear().unwrap();
        assert!(cache
            .list_by_source(MetadataSource::File)
            .unwrap()
            .is_empty());
        assert!(cache
            .list_by_source(MetadataSource::Zotero)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_clear_file_rows_keeps_zotero_rows() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        cache
            .upsert(Path::new("/f.pdf"), id, &sample(), MetadataSource::File)
            .unwrap();
        cache
            .upsert(Path::new("/z.pdf"), id, &sample(), MetadataSource::Zotero)
            .unwrap();

        assert_eq!(cache.clear_file_rows().unwrap(), 1);
        assert!(cache.get_valid(Path::new("/f.pdf"), id).unwrap().is_none());
        assert!(cache.get_valid(Path::new("/z.pdf"), id).unwrap().is_some());
    }

    #[test]
    fn test_schema_rebuild_on_version_mismatch() {
        let dir = tempdir().unwrap();
        {
            let conn = Connection::open(cache_path(dir.path())).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta VALUES ('schema_version', '0');",
            )
            .unwrap();
        }
        // Opening should not error and should produce a usable cache.
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        cache
            .upsert(Path::new("/x"), id, &sample(), MetadataSource::File)
            .unwrap();
        assert_eq!(
            cache.get_valid(Path::new("/x"), id).unwrap(),
            Some(sample())
        );
    }
}
