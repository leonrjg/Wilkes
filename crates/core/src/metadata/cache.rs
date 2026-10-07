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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};

use super::doi::normalize_doi;
use super::MetadataField;
use crate::types::{DocumentMetadata, MetadataSourcePreference, ProviderWork};

const SCHEMA_VERSION: i64 = 6;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

const FIELD_TITLE: &str = MetadataField::Title.as_str();
const FIELD_EXTRACTED_AT_MS: &str = MetadataField::ExtractedAtMs.as_str();
const FIELD_AUTHOR: &str = MetadataField::Author.as_str();
const FIELD_DOI: &str = MetadataField::Doi.as_str();
const FIELD_PUBLICATION_DATE: &str = MetadataField::PublicationDate.as_str();
const FIELD_PAPER_ID: &str = MetadataField::PaperId.as_str();
const FIELD_YEAR: &str = MetadataField::Year.as_str();
const FIELD_VENUE: &str = MetadataField::Venue.as_str();
const FIELD_CITATION_COUNT: &str = MetadataField::CitationCount.as_str();
const FIELD_EXTERNAL_IDS_JSON: &str = MetadataField::ExternalIdsJson.as_str();
const FIELD_CACHED_AT_MS: &str = MetadataField::CachedAtMs.as_str();

fn cache_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("file_metadata.db")
}

/// Provenance of a cached metadata row. `Zotero` rows carry authoritative
/// library data; `File` rows carry only file-based extraction; a `Provider` row
/// carries what one literature provider's `lookup` said, under that provider's
/// id. The distinction drives invalidation when an integration is toggled.
///
/// The stored string for a provider is its id as the registry names it
/// (`openalex`, `semantic_scholar`, `custom:crossref`), which is also what the
/// rows written before providers were manifests already hold — so no stored
/// row needed migrating.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataSource {
    File,
    Zotero,
    Provider(String),
}

impl MetadataSource {
    pub fn as_str(&self) -> &str {
        match self {
            MetadataSource::File => "file",
            MetadataSource::Zotero => "zotero",
            MetadataSource::Provider(id) => id,
        }
    }

    pub fn from_stored(source: &str) -> Self {
        match source {
            "file" => MetadataSource::File,
            "zotero" => MetadataSource::Zotero,
            id => MetadataSource::Provider(id.to_string()),
        }
    }

    pub fn provider(id: &str) -> Self {
        Self::from_stored(id)
    }
}

impl From<&MetadataSourcePreference> for MetadataSource {
    fn from(preference: &MetadataSourcePreference) -> Self {
        match preference {
            MetadataSourcePreference::File => MetadataSource::File,
            MetadataSourcePreference::Zotero => MetadataSource::Zotero,
            MetadataSourcePreference::Provider(id) => MetadataSource::Provider(id.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MetadataFieldValue {
    pub source: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct CachedMetadata {
    pub metadata: DocumentMetadata,
    pub conflicts: HashMap<String, Vec<MetadataFieldValue>>,
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
            size_bytes: Self::clamp_size(size_bytes),
            modified_at_ms,
        })
    }

    /// Derive an identity by stat-ing `path` on disk. Returns `None` when the
    /// file is unreadable or its modified time cannot be represented. This is
    /// the canonical way to fingerprint a file that must be read from disk.
    pub fn for_path(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Self::from_fs(meta.len(), meta.modified().ok())
    }

    /// Derive an identity from an already-listed [`FileEntry`], reusing the
    /// `size_bytes`/`modified_at_ms` it already carries instead of re-stat-ing.
    /// Returns `None` when the entry has no modified time.
    pub fn from_entry(entry: &crate::types::FileEntry) -> Option<Self> {
        entry.modified_at_ms.map(|modified_at_ms| Self {
            size_bytes: Self::clamp_size(entry.size_bytes),
            modified_at_ms,
        })
    }

    /// Clamp a `u64` byte count into the `i64` the identity stores. Kept in one
    /// place so every constructor treats oversized files identically.
    fn clamp_size(size_bytes: u64) -> i64 {
        i64::try_from(size_bytes).unwrap_or(i64::MAX)
    }
}

pub struct MetadataCache {
    conn: Connection,
}

fn component_count(path: &Path) -> usize {
    path.components().count()
}

fn preferred_alias(canonical: &Path, aliases: &[PathBuf], preferred_roots: &[PathBuf]) -> PathBuf {
    let mut choices: Vec<(usize, usize, usize, PathBuf)> = Vec::new();

    for (root_idx, root) in preferred_roots.iter().enumerate() {
        if !root.exists() {
            continue;
        }

        if let Ok(root_canonical) = std::fs::canonicalize(root) {
            if let Ok(relative) = canonical.strip_prefix(&root_canonical) {
                let preferred = root.join(relative);
                if preferred.exists() {
                    choices.push((component_count(relative), root_idx, 0, preferred));
                }
            }
        }

        for alias in aliases {
            if let Ok(relative) = alias.strip_prefix(root) {
                choices.push((component_count(relative), root_idx, 1, alias.clone()));
            }
        }
    }

    choices.sort();
    choices
        .into_iter()
        .next()
        .map(|(_, _, _, path)| path)
        .unwrap_or_else(|| aliases[0].clone())
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
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT)
            .context("Failed to configure metadata cache busy timeout")?;

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
            conn.execute_batch(
                "DROP TABLE IF EXISTS document_citations;
                 DROP TABLE IF EXISTS file_metadata;
                 DROP TABLE IF EXISTS files;
                 DROP TABLE IF EXISTS meta;",
            )?;
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
            CREATE TABLE IF NOT EXISTS files (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path      TEXT    NOT NULL UNIQUE,
                size_bytes     INTEGER NOT NULL,
                modified_at_ms INTEGER NOT NULL,
                indexed_at_ms  INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS file_metadata (
                id      INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                key     TEXT NOT NULL,
                value   TEXT NOT NULL,
                source  TEXT NOT NULL DEFAULT 'file'
            );
            CREATE INDEX IF NOT EXISTS idx_files_identity
                ON files(size_bytes, modified_at_ms);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_file_metadata_file_key_source
                ON file_metadata(file_id, key, source);
            CREATE INDEX IF NOT EXISTS idx_file_metadata_source
                ON file_metadata(source);
            CREATE INDEX IF NOT EXISTS idx_file_metadata_key_value
                ON file_metadata(key, value);
            CREATE TABLE IF NOT EXISTS document_citations (
                source_doi TEXT NOT NULL,
                target_doi TEXT NOT NULL,
                PRIMARY KEY (source_doi, target_doi)
            );
            CREATE INDEX IF NOT EXISTS idx_document_citations_target
                ON document_citations(target_doi);
            PRAGMA foreign_keys = ON;
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

    fn file_id(&self, path: &Path) -> anyhow::Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM files WHERE file_path = ?1",
                params![Self::key(path)],
                |row| row.get(0),
            )
            .optional()?)
    }

    fn upsert_metadata_value(
        &self,
        file_id: i64,
        key: &str,
        value: Option<&str>,
        source: &MetadataSource,
    ) -> anyhow::Result<()> {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(());
        };
        self.conn.execute(
            "INSERT INTO file_metadata (file_id, key, value, source)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(file_id, key, source) DO UPDATE SET value = excluded.value",
            params![file_id, key, value, source.as_str()],
        )?;
        Ok(())
    }

    fn upsert_metadata_i64(
        &self,
        file_id: i64,
        key: &str,
        value: Option<i64>,
        source: &MetadataSource,
    ) -> anyhow::Result<()> {
        self.upsert_metadata_value(
            file_id,
            key,
            value.map(|v| v.to_string()).as_deref(),
            source,
        )
    }

    fn metadata_value(
        &self,
        file_id: i64,
        key: &str,
        source: &MetadataSource,
    ) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM file_metadata
                 WHERE file_id = ?1 AND key = ?2 AND source = ?3",
                params![file_id, key, source.as_str()],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Every provider that has written a record for this file, in the fixed
    /// order of [`crate::integrations::builtin::precedence`], so which one wins
    /// a value the primary source lacks never depends on which answered first.
    fn provider_sources(&self, file_id: i64) -> anyhow::Result<Vec<MetadataSource>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT source FROM file_metadata
             WHERE file_id = ?1 AND source NOT IN (?2, ?3)",
        )?;
        let mut sources = stmt
            .query_map(
                params![
                    file_id,
                    MetadataSource::File.as_str(),
                    MetadataSource::Zotero.as_str()
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        sources.sort_by(|a, b| {
            crate::integrations::builtin::precedence(a)
                .cmp(&crate::integrations::builtin::precedence(b))
        });
        Ok(sources
            .iter()
            .map(|source| MetadataSource::from_stored(source))
            .collect())
    }

    fn source_preference(
        &self,
        file_id: i64,
        primary: &MetadataSource,
    ) -> anyhow::Result<Vec<MetadataSource>> {
        let mut sources = vec![primary.clone()];
        for source in [MetadataSource::File, MetadataSource::Zotero]
            .into_iter()
            .chain(self.provider_sources(file_id)?)
        {
            if !sources.contains(&source) {
                sources.push(source);
            }
        }
        Ok(sources)
    }

    fn preferred_metadata_value(
        &self,
        file_id: i64,
        key: &str,
        primary: &MetadataSource,
    ) -> anyhow::Result<Option<String>> {
        for source in self.source_preference(file_id, primary)? {
            if let Some(value) = self.metadata_value(file_id, key, &source)? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    fn metadata_conflicts(
        &self,
        file_id: i64,
    ) -> anyhow::Result<HashMap<String, Vec<MetadataFieldValue>>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, source, value FROM file_metadata
             WHERE file_id = ?1 AND value <> ''
             ORDER BY key, source",
        )?;
        let rows = stmt
            .query_map(params![file_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    MetadataFieldValue {
                        source: row.get(1)?,
                        value: row.get(2)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut by_key: HashMap<String, Vec<MetadataFieldValue>> = HashMap::new();
        for (key, value) in rows {
            by_key.entry(key).or_default().push(value);
        }
        by_key.retain(|_, values| values.len() > 1);
        Ok(by_key)
    }

    fn document_metadata_for_file(
        &self,
        file_id: i64,
        primary: &MetadataSource,
    ) -> anyhow::Result<Option<CachedMetadata>> {
        let has_metadata: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM file_metadata WHERE file_id = ?1 LIMIT 1",
                params![file_id],
                |row| row.get(0),
            )
            .optional()?;
        if has_metadata.is_none() {
            return Ok(None);
        }

        let doi = self.preferred_metadata_value(file_id, FIELD_DOI, primary)?;
        let mut providers = std::collections::BTreeMap::new();
        for source in self.provider_sources(file_id)? {
            if let Some(work) = self.provider_work(file_id, &source, doi.as_deref())? {
                providers.insert(source.as_str().to_string(), work);
            }
        }

        Ok(Some(CachedMetadata {
            metadata: DocumentMetadata {
                title: self.preferred_metadata_value(file_id, FIELD_TITLE, primary)?,
                author: self.preferred_metadata_value(file_id, FIELD_AUTHOR, primary)?,
                doi,
                created_at: self.preferred_metadata_value(
                    file_id,
                    FIELD_PUBLICATION_DATE,
                    primary,
                )?,
                providers,
            },
            conflicts: self.metadata_conflicts(file_id)?,
        }))
    }

    /// What one provider recorded about this file, if it recorded a work at
    /// all. A provider with no `paper_id` row has contributed no work.
    fn provider_work(
        &self,
        file_id: i64,
        source: &MetadataSource,
        doi: Option<&str>,
    ) -> anyhow::Result<Option<ProviderWork>> {
        let Some(work_id) = self.metadata_value(file_id, FIELD_PAPER_ID, source)? else {
            return Ok(None);
        };
        Ok(Some(ProviderWork {
            doi: doi.map(str::to_string).unwrap_or_default(),
            work_id,
            title: self.metadata_value(file_id, FIELD_TITLE, source)?,
            year: self
                .metadata_value(file_id, FIELD_YEAR, source)?
                .and_then(|value| value.parse::<i64>().ok()),
            publication_date: self.metadata_value(file_id, FIELD_PUBLICATION_DATE, source)?,
            venue: self.metadata_value(file_id, FIELD_VENUE, source)?,
            citation_count: self
                .metadata_value(file_id, FIELD_CITATION_COUNT, source)?
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or_default(),
            external_ids: self
                .metadata_value(file_id, FIELD_EXTERNAL_IDS_JSON, source)?
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default(),
            cached_at_ms: self
                .metadata_value(file_id, FIELD_CACHED_AT_MS, source)?
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or_default(),
        }))
    }

    /// Return cached metadata for `path` only if the stored identity still
    /// matches — i.e. the file has not been edited since extraction.
    pub fn get_valid(
        &self,
        path: &Path,
        identity: FileIdentity,
    ) -> anyhow::Result<Option<DocumentMetadata>> {
        Ok(self
            .get_valid_with_primary(path, identity, MetadataSource::Zotero)?
            .map(|cached| cached.metadata))
    }

    pub fn get_valid_with_primary(
        &self,
        path: &Path,
        identity: FileIdentity,
        primary: MetadataSource,
    ) -> anyhow::Result<Option<CachedMetadata>> {
        let Some(file_id) = self
            .conn
            .query_row(
                "SELECT id FROM files
                 WHERE file_path = ?1 AND size_bytes = ?2 AND modified_at_ms = ?3",
                params![
                    Self::key(path),
                    identity.size_bytes,
                    identity.modified_at_ms
                ],
                |row| row.get(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        self.document_metadata_for_file(file_id, &primary)
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
            "SELECT file_path FROM files
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
    /// after a rename.
    ///
    /// Multiple absolute strings that canonicalize to the same file count as one
    /// logical candidate: cloud-file providers can expose the same file through
    /// aliases such as a friendly mounted path and a provider storage path. When
    /// that happens, `preferred_roots` chooses the spelling closest to an app
    /// root. Returns `None` when no row exists, or when more than one distinct
    /// on-disk file shares the fingerprint (ambiguous, e.g. duplicate content).
    pub fn find_current_path(
        &self,
        identity: FileIdentity,
        preferred_roots: &[PathBuf],
    ) -> anyhow::Result<Option<PathBuf>> {
        let mut stmt = self.conn.prepare(
            "SELECT file_path FROM files
             WHERE size_bytes = ?1 AND modified_at_ms = ?2",
        )?;
        let candidates = stmt
            .query_map(
                params![identity.size_bytes, identity.modified_at_ms],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut logical_files: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
        for candidate in candidates {
            let path = PathBuf::from(candidate);
            if !path.exists() {
                continue;
            }
            let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            logical_files.entry(canonical).or_default().push(path);
        }
        if logical_files.len() == 1 {
            let (canonical, mut aliases) = logical_files.pop_first().expect("len checked");
            aliases.sort();
            Ok(Some(preferred_alias(&canonical, &aliases, preferred_roots)))
        } else {
            Ok(None)
        }
    }

    /// Re-key an existing row from `old` to `new` without re-extracting. Any
    /// stale row already occupying `new` is removed first.
    pub fn rename(&self, old: &Path, new: &Path) -> anyhow::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM files WHERE file_path = ?1",
            params![Self::key(new)],
        )?;
        tx.execute(
            "UPDATE files SET file_path = ?1 WHERE file_path = ?2",
            params![Self::key(new), Self::key(old)],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Merge `metadata` into the cached row for `path` (inserting when absent).
    ///
    /// This is a true upsert, not a replace: a re-derivation must never *lose*
    /// data it happens not to reproduce. Two rules govern the merge, applied
    /// per field:
    ///
    /// * **Empty never overwrites non-empty.** A `NULL`/blank incoming field
    ///   leaves the stored value intact, so re-extracting a file whose title no
    ///   longer parses — or resolving against a Zotero that is momentarily down
    ///   or has since dropped the item — keeps the previously stored value.
    /// * **File extraction never clobbers Zotero.** Once a row is
    ///   `Zotero`-sourced its fields are authoritative; a later `File`-sourced
    ///   write may only fill fields the Zotero record left blank, never replace
    ///   them. The `source` tag is therefore sticky: it upgrades `File → Zotero`
    ///   but never downgrades (only [`invalidate_zotero`](Self::invalidate_zotero)
    ///   clears it).
    ///
    /// Identity (`size_bytes`/`modified_at_ms`) and `extracted_at_ms` always
    /// take the incoming values: they describe the file as seen right now.
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
            "INSERT INTO files (file_path, size_bytes, modified_at_ms, indexed_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(file_path) DO UPDATE SET
                size_bytes = excluded.size_bytes,
                modified_at_ms = excluded.modified_at_ms,
                indexed_at_ms = excluded.indexed_at_ms",
            params![
                Self::key(path),
                identity.size_bytes,
                identity.modified_at_ms,
                now_ms
            ],
        )?;
        let file_id = self
            .file_id(path)?
            .context("metadata cache file row missing after upsert")?;
        let normalized_metadata_doi = metadata.doi.as_deref().and_then(normalize_doi);

        self.upsert_metadata_i64(file_id, FIELD_EXTRACTED_AT_MS, Some(now_ms), &source)?;
        match &source {
            // A provider's rows are what that provider said about the work —
            // nothing the file or Zotero said, and nothing another provider
            // said, however the composed `metadata` was assembled.
            MetadataSource::Provider(id) => {
                if let Some(work) = metadata.providers.get(id) {
                    self.write_provider_work(file_id, work, &source)?;
                    self.upsert_metadata_value(
                        file_id,
                        FIELD_DOI,
                        normalize_doi(&work.doi).as_deref(),
                        &source,
                    )?;
                }
            }
            MetadataSource::File | MetadataSource::Zotero => {
                self.upsert_metadata_value(
                    file_id,
                    FIELD_TITLE,
                    metadata.title.as_deref(),
                    &source,
                )?;
                self.upsert_metadata_value(
                    file_id,
                    FIELD_AUTHOR,
                    metadata.author.as_deref(),
                    &source,
                )?;
                self.upsert_metadata_value(
                    file_id,
                    FIELD_DOI,
                    normalized_metadata_doi.as_deref(),
                    &source,
                )?;
                self.upsert_metadata_value(
                    file_id,
                    FIELD_PUBLICATION_DATE,
                    metadata.created_at.as_deref(),
                    &source,
                )?;
            }
        }
        Ok(())
    }

    /// The eight rows that make up one provider's record of one work.
    fn write_provider_work(
        &self,
        file_id: i64,
        work: &ProviderWork,
        source: &MetadataSource,
    ) -> anyhow::Result<()> {
        self.upsert_metadata_value(file_id, FIELD_PAPER_ID, Some(&work.work_id), source)?;
        self.upsert_metadata_value(file_id, FIELD_TITLE, work.title.as_deref(), source)?;
        self.upsert_metadata_i64(file_id, FIELD_YEAR, work.year, source)?;
        self.upsert_metadata_value(
            file_id,
            FIELD_PUBLICATION_DATE,
            work.publication_date.as_deref(),
            source,
        )?;
        self.upsert_metadata_value(file_id, FIELD_VENUE, work.venue.as_deref(), source)?;
        self.upsert_metadata_i64(
            file_id,
            FIELD_CITATION_COUNT,
            Some(work.citation_count),
            source,
        )?;
        self.upsert_metadata_value(
            file_id,
            FIELD_EXTERNAL_IDS_JSON,
            Some(&serde_json::to_string(&work.external_ids)?),
            source,
        )?;
        self.upsert_metadata_i64(file_id, FIELD_CACHED_AT_MS, Some(work.cached_at_ms), source)?;
        Ok(())
    }

    /// Reconstruct all rows with the given provenance, for re-processing (e.g.
    /// upgrading `File` rows to `Zotero` after the integration is enabled).
    pub fn list_by_source(&self, source: MetadataSource) -> anyhow::Result<Vec<CachedRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT f.id, f.file_path, f.size_bytes, f.modified_at_ms
             FROM files f
             JOIN file_metadata m ON m.file_id = f.id
             WHERE m.source = ?1",
        )?;
        let rows = stmt
            .query_map(params![source.as_str()], |row| {
                let path: String = row.get(1)?;
                Ok((
                    row.get::<_, i64>(0)?,
                    std::path::PathBuf::from(path),
                    FileIdentity {
                        size_bytes: row.get(2)?,
                        modified_at_ms: row.get(3)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(file_id, path, identity)| {
                Ok(CachedRow {
                    path,
                    identity,
                    source: source.clone(),
                    metadata: self
                        .document_metadata_for_file(file_id, &MetadataSource::Zotero)?
                        .map(|cached| cached.metadata)
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    /// Drop all Zotero-sourced rows. Used when the integration is disabled so
    /// those files revert to file-based extraction on the next listing.
    pub fn invalidate_zotero(&self) -> anyhow::Result<usize> {
        let affected_files: usize = self.conn.query_row(
            "SELECT COUNT(DISTINCT file_id) FROM file_metadata WHERE source = ?1",
            params![MetadataSource::Zotero.as_str()],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "DELETE FROM file_metadata WHERE source = ?1",
            params![MetadataSource::Zotero.as_str()],
        )?;
        Ok(affected_files)
    }

    /// Drop everything one provider recorded about any file: the rows its
    /// `lookup` wrote, and only those. The DOI row is left, because it was
    /// never the provider's to remove — the file or Zotero said it first.
    pub fn invalidate_provider(&self, provider: &str) -> anyhow::Result<usize> {
        let source = MetadataSource::provider(provider);
        let keys = params![
            source.as_str(),
            FIELD_PAPER_ID,
            FIELD_TITLE,
            FIELD_YEAR,
            FIELD_PUBLICATION_DATE,
            FIELD_VENUE,
            FIELD_CITATION_COUNT,
            FIELD_EXTERNAL_IDS_JSON,
            FIELD_CACHED_AT_MS,
        ];
        let affected_files: usize = self.conn.query_row(
            "SELECT COUNT(DISTINCT file_id) FROM file_metadata
             WHERE source = ?1 AND key IN (?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            keys,
            |row| row.get(0),
        )?;
        self.conn.execute(
            "DELETE FROM file_metadata
             WHERE source = ?1 AND key IN (?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            keys,
        )?;
        Ok(affected_files)
    }

    /// Every cached path, regardless of provenance. Backs the manual "refresh
    /// metadata" action, which re-derives each known file in place (merging
    /// through [`upsert`](Self::upsert)) rather than clearing and repopulating —
    /// so no row is ever deleted along the way.
    pub fn all_paths(&self) -> anyhow::Result<Vec<PathBuf>> {
        let mut stmt = self.conn.prepare("SELECT file_path FROM files")?;
        let paths = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|r| r.map(PathBuf::from))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(paths)
    }

    /// Remove any cached row for `path`.
    pub fn remove(&self, path: &Path) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM files WHERE file_path = ?1",
            params![Self::key(path)],
        )?;
        Ok(())
    }

    /// The most recently cached record `provider` made for a work with this
    /// DOI, from whichever file holds it.
    pub fn get_provider_by_doi(
        &self,
        provider: &str,
        doi: &str,
    ) -> anyhow::Result<Option<ProviderWork>> {
        let source = MetadataSource::provider(provider);
        let row = self
            .conn
            .query_row(
                "SELECT f.id
                 FROM files f
                 JOIN file_metadata doi
                    ON doi.file_id = f.id AND doi.key = ?1
                        AND doi.value = ?2 COLLATE NOCASE
                 JOIN file_metadata work
                    ON work.file_id = f.id AND work.key = ?3 AND work.source = ?4
                 LEFT JOIN file_metadata cached
                    ON cached.file_id = f.id AND cached.key = ?5 AND cached.source = ?4
                 ORDER BY CAST(cached.value AS INTEGER) DESC
                 LIMIT 1",
                params![
                    FIELD_DOI,
                    doi,
                    FIELD_PAPER_ID,
                    source.as_str(),
                    FIELD_CACHED_AT_MS
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(row.and_then(|file_id| {
            self.document_metadata_for_file(file_id, &source)
                .ok()
                .flatten()
                .and_then(|cached| cached.metadata.providers.get(provider).cloned())
        }))
    }

    /// Record `work` as `provider`'s answer for every cached file that carries
    /// its DOI. Returns how many files were updated.
    pub fn upsert_provider_by_doi(
        &self,
        provider: &str,
        work: &ProviderWork,
    ) -> anyhow::Result<usize> {
        let source = MetadataSource::provider(provider);
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT file_id FROM file_metadata
             WHERE key = ?1 AND value = ?2 COLLATE NOCASE",
        )?;
        let file_ids = stmt
            .query_map(params![FIELD_DOI, work.doi], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        for file_id in &file_ids {
            self.write_provider_work(*file_id, work, &source)?;
        }
        Ok(file_ids.len())
    }

    /// The DOI recorded for `path`, from any source, if the file is cached.
    /// Identity is not checked: a DOI is stable metadata, and the citation
    /// graph is keyed on it rather than on file content.
    pub fn doi_for_path(&self, path: &Path) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT fm.value
                 FROM files f
                 JOIN file_metadata fm ON fm.file_id = f.id AND fm.key = ?2
                 WHERE f.file_path = ?1
                 LIMIT 1",
                params![Self::key(path), FIELD_DOI],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Replace the stored outgoing citation edges for `source_doi` with
    /// `target_dois`. Delete-then-insert so a re-enrichment reflects the
    /// current reference list rather than accumulating stale edges. Edges are
    /// stored by normalized DOI only — no provider identifier and no `file_id`,
    /// so an edge to a paper not yet in the library remains valid and resolves
    /// the moment that paper is added.
    pub fn replace_citations(
        &self,
        source_doi: &str,
        target_dois: &[String],
    ) -> anyhow::Result<usize> {
        let Some(source) = normalize_doi(source_doi) else {
            return Ok(0);
        };
        self.conn.execute(
            "DELETE FROM document_citations WHERE source_doi = ?1 COLLATE NOCASE",
            params![source],
        )?;
        let mut inserted = 0;
        for target in target_dois {
            let Some(target) = normalize_doi(target) else {
                continue;
            };
            if target == source {
                continue;
            }
            inserted += self.conn.execute(
                "INSERT OR IGNORE INTO document_citations (source_doi, target_doi)
                 VALUES (?1, ?2)",
                params![source, target],
            )?;
        }
        Ok(inserted)
    }

    /// Citation neighbours of `doi` that resolve to a document in the library.
    /// `references` are documents `doi` cites; `cited_by` are documents that
    /// cite `doi`. The library intersection happens here, at read time, by
    /// joining edges against the DOIs of cached files — so a document added
    /// after the edge was stored still lights up.
    pub fn citation_links(&self, doi: &str) -> anyhow::Result<CitationLinkPaths> {
        let Some(doi) = normalize_doi(doi) else {
            return Ok(CitationLinkPaths::default());
        };
        Ok(CitationLinkPaths {
            references: self.linked_paths(
                "SELECT DISTINCT f.file_path
                 FROM document_citations c
                 JOIN file_metadata dm ON dm.key = ?1
                    AND dm.value = c.target_doi COLLATE NOCASE
                 JOIN files f ON f.id = dm.file_id
                 WHERE c.source_doi = ?2 COLLATE NOCASE",
                &doi,
            )?,
            cited_by: self.linked_paths(
                "SELECT DISTINCT f.file_path
                 FROM document_citations c
                 JOIN file_metadata dm ON dm.key = ?1
                    AND dm.value = c.source_doi COLLATE NOCASE
                 JOIN files f ON f.id = dm.file_id
                 WHERE c.target_doi = ?2 COLLATE NOCASE",
                &doi,
            )?,
        })
    }

    /// Every outgoing DOI for a work, whether or not it currently resolves to
    /// a file in the library. Results are canonicalized, deduplicated
    /// case-insensitively, and sorted for a stable UI order.
    pub fn citation_references(&self, doi: &str) -> anyhow::Result<Vec<String>> {
        let Some(doi) = normalize_doi(doi) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT lower(target_doi)
             FROM document_citations
             WHERE source_doi = ?1 COLLATE NOCASE
             ORDER BY lower(target_doi)",
        )?;
        let references = stmt
            .query_map(params![doi], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(references)
    }

    fn linked_paths(&self, sql: &str, doi: &str) -> anyhow::Result<Vec<PathBuf>> {
        let mut stmt = self.conn.prepare(sql)?;
        let paths = stmt
            .query_map(params![FIELD_DOI, doi], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(PathBuf::from)
            .collect();
        Ok(paths)
    }
}

/// Library paths of a document's citation neighbours, both edge directions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CitationLinkPaths {
    pub references: Vec<PathBuf>,
    pub cited_by: Vec<PathBuf>,
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
            ..DocumentMetadata::default()
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

    fn doc_with_doi(doi: &str) -> DocumentMetadata {
        DocumentMetadata {
            doi: Some(doi.into()),
            ..DocumentMetadata::default()
        }
    }

    #[test]
    fn citation_links_resolve_both_directions_by_doi() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        let a = Path::new("/docs/a.pdf");
        let b = Path::new("/docs/b.pdf");
        cache
            .upsert(a, id, &doc_with_doi("10.1/A"), MetadataSource::File)
            .unwrap();
        cache
            .upsert(b, id, &doc_with_doi("10.1/B"), MetadataSource::File)
            .unwrap();
        assert_eq!(cache.doi_for_path(a).unwrap().as_deref(), Some("10.1/a"));
        assert_eq!(cache.doi_for_path(b).unwrap().as_deref(), Some("10.1/b"));

        // A cites B (and a paper not in the library, which is stored but does
        // not resolve to a path).
        cache
            .replace_citations("10.1/a", &["10.1/b".into(), "10.1/absent".into()])
            .unwrap();

        let from_a = cache.citation_links("10.1/a").unwrap();
        assert_eq!(from_a.references, vec![b.to_path_buf()]);
        assert!(from_a.cited_by.is_empty());
        assert_eq!(
            cache.citation_references("10.1/A").unwrap(),
            vec!["10.1/absent".to_string(), "10.1/b".to_string()]
        );

        let from_b = cache.citation_links("10.1/b").unwrap();
        assert_eq!(from_b.cited_by, vec![a.to_path_buf()]);
        assert!(from_b.references.is_empty());

        // Late binding: the absent reference resolves once its document lands.
        cache
            .upsert(
                Path::new("/docs/c.pdf"),
                id,
                &doc_with_doi("10.1/absent"),
                MetadataSource::File,
            )
            .unwrap();
        assert_eq!(cache.citation_links("10.1/a").unwrap().references.len(), 2);
    }

    #[test]
    fn citation_links_and_replacement_accept_legacy_mixed_case_rows() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        let a = Path::new("/docs/a.pdf");
        let b = Path::new("/docs/b.pdf");
        cache
            .upsert(a, id, &doc_with_doi("10.1/a"), MetadataSource::File)
            .unwrap();
        cache
            .upsert(b, id, &doc_with_doi("10.1/b"), MetadataSource::File)
            .unwrap();

        // Simulate rows written before DOI canonicalization was introduced.
        cache
            .conn
            .execute(
                "UPDATE file_metadata SET value = '10.1/B'
                 WHERE file_id = (SELECT id FROM files WHERE file_path = ?1)
                   AND key = ?2",
                params![MetadataCache::key(b), FIELD_DOI],
            )
            .unwrap();
        cache
            .conn
            .execute(
                "INSERT INTO document_citations (source_doi, target_doi)
                 VALUES ('10.1/A', '10.1/B')",
                [],
            )
            .unwrap();
        cache
            .conn
            .execute(
                "INSERT INTO document_citations (source_doi, target_doi)
                 VALUES ('10.1/a', '10.1/b')",
                [],
            )
            .unwrap();

        assert_eq!(
            cache.citation_links("10.1/a").unwrap().references,
            vec![b.to_path_buf()]
        );
        assert_eq!(
            cache.citation_links("10.1/b").unwrap().cited_by,
            vec![a.to_path_buf()]
        );
        assert_eq!(
            cache.citation_references("10.1/a").unwrap(),
            vec!["10.1/b".to_string()]
        );

        cache
            .replace_citations("10.1/a", &["10.1/C".into()])
            .unwrap();
        let stored = cache
            .conn
            .query_row(
                "SELECT source_doi, target_doi FROM document_citations
                 WHERE source_doi = '10.1/a' COLLATE NOCASE",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap();
        assert_eq!(stored, ("10.1/a".into(), "10.1/c".into()));
    }

    #[test]
    fn replace_citations_is_idempotent_and_prunes() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        let b = Path::new("/docs/b.pdf");
        cache
            .upsert(b, id, &doc_with_doi("10.1/b"), MetadataSource::File)
            .unwrap();

        cache
            .replace_citations("10.1/a", &["10.1/b".into(), "10.1/b".into()])
            .unwrap();
        assert_eq!(
            cache.citation_links("10.1/a").unwrap().references,
            vec![b.to_path_buf()]
        );

        // Re-running with an empty list clears the edge; a self-edge is ignored.
        cache
            .replace_citations("10.1/a", &["10.1/a".into()])
            .unwrap();
        assert!(cache
            .citation_links("10.1/a")
            .unwrap()
            .references
            .is_empty());
    }

    #[test]
    fn test_open_waits_for_transient_database_lock() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        cache.conn.execute_batch("BEGIN EXCLUSIVE").unwrap();

        let path = dir.path().to_path_buf();
        let handle = std::thread::spawn(move || MetadataCache::open(&path));
        std::thread::sleep(Duration::from_millis(100));

        cache.conn.execute_batch("COMMIT").unwrap();
        handle.join().unwrap().unwrap();
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
        assert_eq!(cache.find_current_path(id, &[]).unwrap(), Some(new.clone()));

        // A second on-disk file sharing the fingerprint makes it ambiguous:
        // never guess.
        let dup = dir.path().join("dup.pdf");
        std::fs::write(&dup, b"content").unwrap();
        cache
            .upsert(&dup, id, &sample(), MetadataSource::File)
            .unwrap();
        assert!(cache.find_current_path(id, &[]).unwrap().is_none());

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
        assert!(cache.find_current_path(missing_id, &[]).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_find_current_path_collapses_aliases_and_prefers_configured_root() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 100,
            modified_at_ms: 42,
        };
        let storage_root = dir.path().join("storage");
        let friendly_root = dir.path().join("friendly");
        std::fs::create_dir_all(&storage_root).unwrap();
        std::fs::create_dir_all(&friendly_root).unwrap();

        let storage_path = storage_root.join("doc.pdf");
        let friendly_path = friendly_root.join("doc.pdf");
        std::fs::write(&storage_path, b"content").unwrap();
        std::os::unix::fs::symlink(&storage_path, &friendly_path).unwrap();
        cache
            .upsert(&storage_path, id, &sample(), MetadataSource::File)
            .unwrap();
        cache
            .upsert(&friendly_path, id, &sample(), MetadataSource::File)
            .unwrap();

        assert_eq!(
            cache.find_current_path(id, &[friendly_root]).unwrap(),
            Some(friendly_path)
        );
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
    fn test_invalidate_provider_keeps_file_metadata() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        let path = Path::new("/doc.pdf");
        cache
            .upsert(
                path,
                id,
                &DocumentMetadata {
                    title: Some("Title".into()),
                    doi: Some("10.1145/1".into()),
                    ..DocumentMetadata::default()
                },
                MetadataSource::File,
            )
            .unwrap();
        cache
            .upsert(
                path,
                id,
                &DocumentMetadata {
                    providers: [(
                        "semantic_scholar".to_string(),
                        ProviderWork {
                            doi: "10.1145/1".into(),
                            work_id: "abc".into(),
                            title: None,
                            year: None,
                            publication_date: None,
                            venue: None,
                            citation_count: 3,
                            external_ids: Default::default(),
                            cached_at_ms: 42,
                        },
                    )]
                    .into(),
                    ..DocumentMetadata::default()
                },
                MetadataSource::provider("semantic_scholar"),
            )
            .unwrap();

        assert_eq!(cache.invalidate_provider("semantic_scholar").unwrap(), 1);
        let metadata = cache.get_valid(path, id).unwrap().unwrap();
        assert_eq!(metadata.title.as_deref(), Some("Title"));
        assert!(metadata.providers.is_empty());
    }

    #[test]
    fn test_list_by_source_and_all_paths() {
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

        // `all_paths` returns every row regardless of provenance — the set a
        // refresh re-derives in place.
        let mut paths = cache.all_paths().unwrap();
        paths.sort();
        assert_eq!(
            paths,
            vec![PathBuf::from("/f.pdf"), PathBuf::from("/z.pdf")]
        );
    }

    /// The core invariant behind the fix: re-deriving a Zotero-backed file when
    /// Zotero yields nothing (item removed, or API down) must not erase the
    /// stored library data. A `File`-sourced write with empty fields is a no-op
    /// on a `Zotero` row; the row keeps its data and its `zotero` provenance.
    #[test]
    fn test_upsert_file_pass_does_not_erase_zotero_row() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        let path = Path::new("/doc.pdf");
        cache
            .upsert(path, id, &sample(), MetadataSource::Zotero)
            .unwrap();

        let empty = DocumentMetadata {
            title: None,
            author: None,
            doi: None,
            created_at: None,
            ..DocumentMetadata::default()
        };
        cache
            .upsert(path, id, &empty, MetadataSource::File)
            .unwrap();

        assert_eq!(cache.get_valid(path, id).unwrap(), Some(sample()));
        // Provenance stayed Zotero, so a later `invalidate_zotero` still targets it.
        let zotero_rows = cache.list_by_source(MetadataSource::Zotero).unwrap();
        assert_eq!(zotero_rows.len(), 1);
        assert_eq!(zotero_rows[0].path, path);
    }

    /// Blank fields never overwrite stored values, but real ones do; and a
    /// `File` write may still backfill a field the Zotero record left empty.
    #[test]
    fn test_upsert_merges_field_by_field() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        let path = Path::new("/doc.pdf");

        // Zotero row missing a publication date but carrying a title.
        let partial_zotero = DocumentMetadata {
            title: Some("Zotero Title".into()),
            author: Some("Zotero Author".into()),
            doi: None,
            created_at: None,
            ..DocumentMetadata::default()
        };
        cache
            .upsert(path, id, &partial_zotero, MetadataSource::Zotero)
            .unwrap();

        // File extraction: a different title (must NOT clobber Zotero's) plus a
        // publication date the Zotero record lacked (must backfill).
        let file_based = DocumentMetadata {
            title: Some("File Title".into()),
            author: None,
            doi: None,
            created_at: Some("2021-05".into()),
            ..DocumentMetadata::default()
        };
        cache
            .upsert(path, id, &file_based, MetadataSource::File)
            .unwrap();

        let merged = cache.get_valid(path, id).unwrap().unwrap();
        assert_eq!(merged.title.as_deref(), Some("Zotero Title"));
        assert_eq!(merged.author.as_deref(), Some("Zotero Author"));
        assert_eq!(merged.created_at.as_deref(), Some("2021-05"));

        // A subsequent Zotero write is authoritative: it overwrites where it has
        // a value, but a blank field still leaves the prior value intact.
        let newer_zotero = DocumentMetadata {
            title: Some("Newer Zotero Title".into()),
            author: None,
            doi: Some("10.1/x".into()),
            created_at: None,
            ..DocumentMetadata::default()
        };
        cache
            .upsert(path, id, &newer_zotero, MetadataSource::Zotero)
            .unwrap();
        let merged = cache.get_valid(path, id).unwrap().unwrap();
        assert_eq!(merged.title.as_deref(), Some("Newer Zotero Title"));
        assert_eq!(merged.author.as_deref(), Some("Zotero Author"));
        assert_eq!(merged.doi.as_deref(), Some("10.1/x"));
        assert_eq!(merged.created_at.as_deref(), Some("2021-05"));
    }

    #[test]
    fn test_primary_source_selects_display_value_and_reports_conflicts() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        let path = Path::new("/doc.pdf");

        cache
            .upsert(
                path,
                id,
                &DocumentMetadata {
                    title: Some("File Title".into()),
                    ..DocumentMetadata::default()
                },
                MetadataSource::File,
            )
            .unwrap();
        cache
            .upsert(
                path,
                id,
                &DocumentMetadata {
                    title: Some("Zotero Title".into()),
                    ..DocumentMetadata::default()
                },
                MetadataSource::Zotero,
            )
            .unwrap();

        let file_primary = cache
            .get_valid_with_primary(path, id, MetadataSource::File)
            .unwrap()
            .unwrap();
        assert_eq!(file_primary.metadata.title.as_deref(), Some("File Title"));
        assert_eq!(
            file_primary
                .conflicts
                .get(FIELD_TITLE)
                .map(|values| values.len()),
            Some(2)
        );

        let zotero_primary = cache
            .get_valid_with_primary(path, id, MetadataSource::Zotero)
            .unwrap()
            .unwrap();
        assert_eq!(
            zotero_primary.metadata.title.as_deref(),
            Some("Zotero Title")
        );

        let missing_primary = cache
            .get_valid_with_primary(path, id, MetadataSource::provider("openalex"))
            .unwrap()
            .unwrap();
        assert_eq!(
            missing_primary.metadata.title.as_deref(),
            Some("File Title")
        );
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

    fn work(doi: &str, id: &str, citations: i64) -> ProviderWork {
        ProviderWork {
            doi: doi.into(),
            work_id: id.into(),
            title: None,
            year: None,
            publication_date: None,
            venue: None,
            citation_count: citations,
            external_ids: Default::default(),
            cached_at_ms: 42,
        }
    }

    #[test]
    fn test_provider_data_round_trips_through_file_metadata() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let mut external_ids = std::collections::HashMap::new();
        external_ids.insert("DOI".to_string(), serde_json::json!("10.1145/1"));
        let paper = ProviderWork {
            doi: "10.1145/1".into(),
            work_id: "abc".into(),
            title: Some("Paper".into()),
            year: Some(2026),
            publication_date: Some("2026-07-06".into()),
            venue: Some("Venue".into()),
            citation_count: 7,
            external_ids,
            cached_at_ms: 42,
        };
        let metadata = DocumentMetadata {
            doi: Some("10.1145/1".into()),
            providers: [("semantic_scholar".to_string(), paper.clone())].into(),
            ..DocumentMetadata::default()
        };
        let identity = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 2,
        };
        let path = Path::new("/doc.pdf");

        cache
            .upsert(
                path,
                identity,
                &DocumentMetadata {
                    doi: Some("10.1145/1".into()),
                    ..DocumentMetadata::default()
                },
                MetadataSource::File,
            )
            .unwrap();
        cache
            .upsert(
                path,
                identity,
                &metadata,
                MetadataSource::provider("semantic_scholar"),
            )
            .unwrap();
        let keys = cache
            .conn
            .prepare("SELECT key FROM file_metadata WHERE source = ?1 ORDER BY key")
            .unwrap()
            .query_map(params!["semantic_scholar"], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(keys.iter().all(|key| !key.starts_with("semantic_scholar_")));
        assert!(keys.contains(&FIELD_CITATION_COUNT.to_string()));

        assert_eq!(
            cache
                .get_valid(path, identity)
                .unwrap()
                .and_then(|m| m.providers.get("semantic_scholar").cloned()),
            Some(paper.clone())
        );
        assert_eq!(
            cache
                .get_provider_by_doi("semantic_scholar", "10.1145/1")
                .unwrap(),
            Some(paper)
        );
        // Another provider has said nothing about this work.
        assert_eq!(
            cache.get_provider_by_doi("openalex", "10.1145/1").unwrap(),
            None
        );
    }

    #[test]
    fn test_provider_upsert_updates_all_cached_rows_for_doi() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let identity = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 2,
        };
        for path in [Path::new("/a.pdf"), Path::new("/b.pdf")] {
            cache
                .upsert(
                    path,
                    identity,
                    &DocumentMetadata {
                        doi: Some("10.1145/1".into()),
                        ..DocumentMetadata::default()
                    },
                    MetadataSource::File,
                )
                .unwrap();
        }

        assert_eq!(
            cache
                .upsert_provider_by_doi("openalex", &work("10.1145/1", "abc", 11))
                .unwrap(),
            2
        );
        assert_eq!(
            cache
                .get_valid(Path::new("/a.pdf"), identity)
                .unwrap()
                .and_then(|m| m.providers.get("openalex").map(|p| p.citation_count)),
            Some(11)
        );
    }

    /// Two providers describing one file are two independent records, and
    /// invalidating one leaves the other.
    #[test]
    fn test_providers_are_independent_records_of_one_file() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let identity = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 2,
        };
        let path = Path::new("/doc.pdf");
        cache
            .upsert(
                path,
                identity,
                &DocumentMetadata {
                    doi: Some("10.1145/1".into()),
                    ..DocumentMetadata::default()
                },
                MetadataSource::File,
            )
            .unwrap();
        cache
            .upsert_provider_by_doi("openalex", &work("10.1145/1", "W1", 5))
            .unwrap();
        cache
            .upsert_provider_by_doi("custom:crossref", &work("10.1145/1", "c1", 9))
            .unwrap();

        let metadata = cache.get_valid(path, identity).unwrap().unwrap();
        assert_eq!(metadata.providers.len(), 2);
        assert_eq!(metadata.providers["openalex"].work_id, "W1");
        assert_eq!(metadata.providers["custom:crossref"].citation_count, 9);

        assert_eq!(cache.invalidate_provider("openalex").unwrap(), 1);
        let metadata = cache.get_valid(path, identity).unwrap().unwrap();
        assert_eq!(
            metadata.providers.keys().collect::<Vec<_>>(),
            vec!["custom:crossref"]
        );
    }

    /// A provider's upsert writes that provider's record and nothing else, so a
    /// composed record that happens to carry another provider's work cannot be
    /// written under the wrong source.
    #[test]
    fn test_a_provider_upsert_writes_only_its_own_record() {
        let dir = tempdir().unwrap();
        let cache = MetadataCache::open(dir.path()).unwrap();
        let identity = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 2,
        };
        let path = Path::new("/doc.pdf");
        let composed = DocumentMetadata {
            title: Some("From the file".into()),
            doi: Some("10.1145/1".into()),
            providers: [
                ("openalex".to_string(), work("10.1145/1", "W1", 5)),
                ("semantic_scholar".to_string(), work("10.1145/1", "s1", 6)),
            ]
            .into(),
            ..DocumentMetadata::default()
        };
        cache
            .upsert(
                path,
                identity,
                &composed,
                MetadataSource::provider("openalex"),
            )
            .unwrap();

        let metadata = cache.get_valid(path, identity).unwrap().unwrap();
        assert_eq!(
            metadata.providers.keys().collect::<Vec<_>>(),
            vec!["openalex"]
        );
        // The file's title was not the provider's to write.
        assert_eq!(metadata.title, None);
    }
}
