use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use ignore::WalkBuilder;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, ToSql};
use tracing::{error, info};

use crate::consumer::{consumer_error, ConsumerErrorCode};
use crate::extract::ExtractorRegistry;
#[cfg(test)]
use crate::figure::FigureRelation;
use crate::figure::{self, FigureLink};
use crate::metadata::cache::FileIdentity;
use crate::types::{
    BoundingBox, ByteRange, ChunkTopicMember, EmbeddingEngine, FileType, IndexStatus,
    IndexingConfig, ReadingRegion, RelatedDocument, RetainedExtraction, RetainedImage, SourceMap,
    SourceOrigin, SourceSegment, SupersededArea, RETAINED_EXTRACTION_VERSION,
};
use crate::{consumer_bail, consumer_ensure};

use super::super::Embedder;
use super::chunk::{chunk_content, ensure_chunks_reconstruct, Chunk};
use super::job::{BuildReporter, DocumentOutcome, DocumentStage};
use crate::embed::identity::{
    chunk_ref, rendition_id, sha256_bytes, sha256_file, snapshot_id, ChunkDescriptor,
};
use crate::embed::{
    ChunkRef, DocumentSnapshotId, EmbeddingSpaceId, EmbeddingSpaceIdentity, ExtractionRecipe,
    IndexEmbeddingMetadata, RenditionId,
};

mod build;
mod chunks;
mod files;
mod managed;
mod query;
mod roots;
mod schema;
mod status;
mod topics;
mod vector;

pub use self::build::*;
pub use self::chunks::*;
pub use self::topics::*;
use self::vector::*;

fn system_time_ms(value: SystemTime) -> Option<i64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

// ── sqlite-vec extension loading ──────────────────────────────────────────────

fn load_sqlite_vec() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        // sqlite3_vec_init is declared as fn() but sqlite3_auto_extension expects
        // the full 3-argument extension init signature. transmute bridges the gap;
        // this is the canonical pattern shown in the sqlite-vec crate's own tests.
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *const std::ffi::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )));
    });
}

// ── File path of the SQLite DB ────────────────────────────────────────────────

fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join("semantic_index.db")
}

fn replacement_backup_path(data_dir: &Path) -> PathBuf {
    data_dir.join("semantic_index.db.replacement-backup")
}

fn remove_sqlite_sidecars(path: &Path) {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let mut shm = path.as_os_str().to_owned();
    shm.push("-shm");
    let _ = std::fs::remove_file(wal);
    let _ = std::fs::remove_file(shm);
}

/// Complete or roll back a replacement interrupted after the old database was
/// moved aside. A fully created temporary database is renamed before the
/// backup is removed, so every crash point retains at least one complete copy.
fn recover_interrupted_index_replacement(data_dir: &Path) -> anyhow::Result<()> {
    let final_path = db_path(data_dir);
    let backup_path = replacement_backup_path(data_dir);
    if !backup_path.exists() {
        return Ok(());
    }
    if !final_path.exists() {
        std::fs::rename(&backup_path, &final_path).with_context(|| {
            format!(
                "Failed to restore interrupted index replacement from {}",
                backup_path.display()
            )
        })?;
        return Ok(());
    }

    let replacement_is_complete = (|| -> anyhow::Result<bool> {
        let conn = Connection::open(&final_path)?;
        configure_connection(&conn, &final_path)?;
        let quick_check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        let schema_version: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(quick_check == "ok"
            && schema_version
                .as_deref()
                .and_then(|version| version.parse::<i64>().ok())
                == Some(SCHEMA_VERSION))
    })()
    .unwrap_or(false);

    if replacement_is_complete {
        std::fs::remove_file(&backup_path).with_context(|| {
            format!(
                "Failed to finish index replacement by removing {}",
                backup_path.display()
            )
        })?;
    } else {
        std::fs::remove_file(&final_path).with_context(|| {
            format!(
                "Failed to discard incomplete replacement {}",
                final_path.display()
            )
        })?;
        remove_sqlite_sidecars(&final_path);
        std::fs::rename(&backup_path, &final_path).with_context(|| {
            format!(
                "Failed to roll back index replacement from {}",
                backup_path.display()
            )
        })?;
    }
    Ok(())
}

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(3);
const SCHEMA_VERSION: i64 = 12;

/// The `files f` rows whose stored text can be served: text is present, and
/// empty only for a document that produced no passages. Empty text alongside
/// passages is a legacy row and holds nothing.
const SERVABLE_RENDITION: &str = "f.full_text IS NOT NULL
    AND (f.full_text <> '' OR NOT EXISTS (SELECT 1 FROM chunks c WHERE c.file_id = f.id))";

/// When a file's servable rendition was written, and the on-disk identity it
/// was written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenditionStamp {
    pub size_bytes: i64,
    pub modified_at_ms: i64,
    pub indexed_at_ms: i64,
}

fn configure_connection(conn: &Connection, path: &Path) -> anyhow::Result<()> {
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)
        .with_context(|| format!("Failed to configure busy timeout for {}", path.display()))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .with_context(|| format!("Failed to enable foreign keys for {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests;

// ── Indexed chunk (query result) ──────────────────────────────────────────────

pub struct IndexedChunk {
    pub file_path: PathBuf,
    pub chunk_text: String,
    /// Byte range into `ExtractedContent.text`.
    pub extraction_byte_range: ByteRange,
    pub origin: SourceOrigin,
    pub score: f32,
}

/// A complete indexed chunk row used by corpus-wide consumers such as the
/// topic cloud. Unlike `IndexedChunk`, this is not an ANN result and therefore
/// carries the stored vector and stable database ids.
#[derive(Clone, Debug)]
pub struct TopicChunkData {
    pub chunk_id: i64,
    pub file_id: i64,
    pub file_path: PathBuf,
    pub chunk_text: String,
    /// The stable identity of this passage, absent only on an index built
    /// before schema v10 — which is why an export that names passages refuses
    /// such an index rather than reporting nulls.
    pub chunk_ref: Option<ChunkRef>,
    pub text_sha256: Option<String>,
    pub extraction_byte_range: ByteRange,
    pub origin: SourceOrigin,
    pub embedding: Vec<f32>,
}

/// The figures one passage bears on, and how.
#[derive(Clone, Debug)]
pub struct ChunkFigures {
    pub chunk_ref: ChunkRef,
    /// Whether this passage's rendition has an image inventory at all.
    ///
    /// `false` is *unknown* — a rendition retained before inventories existed
    /// — and a consumer must not read the empty list beside it as "this
    /// passage draws nothing". Re-extracting the document is what settles it.
    pub inventory_known: bool,
    pub figures: Vec<LinkedFigure>,
}

/// One figure, with its bearing on the passage that asked.
#[derive(Clone, Debug)]
pub struct LinkedFigure {
    pub image: RetainedImage,
    pub link: FigureLink,
}

/// Where a figure's pixels are to be found, for a caller that wants them.
#[derive(Clone, Debug)]
pub struct FigureSource {
    /// The immutable snapshot when the corpus retained one, relative to the
    /// data directory; the caller resolves it, being the one that knows where
    /// that is. Preferred over `file_path` because it cannot have been edited
    /// since the rendition was built.
    pub managed_snapshot_relative_path: Option<String>,
    /// The path as indexed. The only address for an ordinary workspace file,
    /// and the fallback for a managed one imported before snapshots.
    pub file_path: PathBuf,
    pub image: RetainedImage,
}

/// Where one imported document's retained bytes are.
///
/// No `file_path` beside it, unlike [`FigureSource`]: there is nothing to fall
/// back *to*. A caller asking by snapshot id is asking for the copy the corpus
/// retained, and the original it was taken from is not that copy — it is a
/// file on some machine that may since have become a different document.
#[derive(Clone, Debug)]
pub struct ManagedSnapshotSource {
    /// Relative to the data directory; the caller resolves it, being the one
    /// that knows where that is.
    pub managed_snapshot_relative_path: String,
    /// What the retained bytes hashed to when they were retained, so a caller
    /// can check what it was served against what it recorded.
    pub source_sha256: String,
}

/// How a caller names the document a figure belongs to.
///
/// Two spellings because two consumers exist: a managed consumer holds chunk
/// refs and no path at all, and the export surface is addressed by root and
/// path. Both name one file row.
#[derive(Clone, Copy, Debug)]
pub enum FigureDocument<'a> {
    Path(&'a Path),
    Chunk(&'a ChunkRef),
}

/// Columns [`managed_chunk_from_row`] reads, in the order it reads them.
const MANAGED_CHUNK_COLUMNS: &str = "chunk_ref, chunk_idx, chunk_text, text_sha256,
        byte_start, byte_end, origin_type, page, line, col,
        bbox_x, bbox_y, bbox_w, bbox_h";

fn managed_chunk_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ManagedChunkData> {
    let origin = source_origin_from_parts(
        &row.get::<_, String>(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
    )
    .ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(
            6,
            "origin_type".to_string(),
            rusqlite::types::Type::Text,
        )
    })?;
    Ok(ManagedChunkData {
        chunk_ref: ChunkRef(row.get(0)?),
        ordinal: row.get::<_, i64>(1)? as usize,
        text: row.get(2)?,
        text_sha256: row.get(3)?,
        extraction_byte_range: ByteRange {
            start: row.get::<_, i64>(4)? as usize,
            end: row.get::<_, i64>(5)? as usize,
        },
        origin,
    })
}

/// The passages either side of one named passage, as
/// [`SemanticIndex::managed_chunk_neighbors`] answers for it.
#[derive(Clone, Debug)]
pub struct ManagedChunkNeighbors {
    pub of: ChunkRef,
    pub before: Vec<ManagedChunkData>,
    pub after: Vec<ManagedChunkData>,
}

/// Stable, vector-free passage export used by managed-corpus consumers.
#[derive(Clone, Debug)]
pub struct ManagedChunkData {
    pub chunk_ref: ChunkRef,
    pub ordinal: usize,
    pub text: String,
    pub text_sha256: String,
    pub extraction_byte_range: ByteRange,
    pub origin: SourceOrigin,
}

#[derive(Clone, Debug)]
pub struct ManagedDocumentData {
    pub source_sha256: String,
    pub snapshot_id: DocumentSnapshotId,
    pub extraction_recipe_id: String,
    pub rendition_id: RenditionId,
    pub extracted_content_sha256: String,
    pub chunks: Vec<ManagedChunkData>,
}

/// The unnormalized sum of the L2-normalized vectors of one named group, and
/// how many there were.
///
/// The sum rather than the mean, because the mean is derivable from the sum
/// and the reverse is not: a caller partitioning a large group across several
/// requests adds the sums and the counts and normalizes exactly once.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkAccumulation {
    pub sum: Vec<f32>,
    pub member_count: usize,
}

#[derive(Clone, Debug)]
struct IndexedFileRecord {
    path_key: PathBuf,
    identity: FileIdentity,
}

struct FileSemanticIdentity {
    source_sha256: String,
    snapshot_id: DocumentSnapshotId,
    extraction_recipe_id: String,
    rendition_id: RenditionId,
    extracted_content_sha256: String,
    embedding_reused_chunks: Option<usize>,
    embedding_computed_chunks: Option<usize>,
    idempotency_key: Option<String>,
    chunk_descriptors: Vec<ChunkDescriptor>,
    managed_snapshot_relative_path: Option<String>,
    original_source_provenance_json: Option<String>,
    admission_state: Option<String>,
}

#[derive(Clone, Copy)]
pub enum SemanticQueryScope<'a> {
    Corpus,
    Root(&'a Path),
    File(&'a Path),
}

// ── SemanticIndex ─────────────────────────────────────────────────────────────

pub struct SemanticIndex {
    conn: Connection,
    dimension: usize,
    active_root: Option<PathBuf>,
    active_root_id: Option<i64>,
}

impl SemanticIndex {
    /// Whether this failure ends the build rather than skipping a batch.
    ///
    /// A dead worker does. The batch loop below is the one place in the build
    /// that submits repeatedly, and a batch it skipped would be followed by
    /// another submit — which, with no process left, *starts one*. That is a
    /// worker started inside a loop, and it is how a cancel comes to need one
    /// kill per batch instead of one.
    ///
    /// Asked of the error's own type. It used to be asked by matching the
    /// first characters of the message, which meant every rewording of an
    /// unrelated log line was a chance to turn a fatal error into a skipped
    /// batch, silently.
    ///
    /// Both fault kinds, not just a gone worker. A worker that answered with a
    /// failure will answer the next batch the same way — a model that would
    /// not load does not load on the second attempt either — and a build that
    /// skipped every batch would finish reporting success over an empty index.
    fn is_fatal_embedder_error(err: &anyhow::Error) -> bool {
        crate::worker::fault::is_worker_fault(err)
    }

    pub fn embedding_metadata(&self) -> anyhow::Result<IndexEmbeddingMetadata> {
        let json: String = self.conn.query_row(
            "SELECT value FROM meta WHERE key = 'index_embedding_metadata_json'",
            [],
            |row| row.get(0),
        )?;
        let metadata: IndexEmbeddingMetadata = serde_json::from_str(&json)?;
        metadata.validate()?;
        Ok(metadata)
    }

    pub fn embedding_space_identity(&self) -> anyhow::Result<EmbeddingSpaceIdentity> {
        self.embedding_metadata()?
            .exact_identity
            .ok_or_else(|| anyhow::anyhow!("INDEX_EMBEDDING_IDENTITY_UNVERIFIED"))
    }

    pub fn embedding_space_id(&self) -> anyhow::Result<EmbeddingSpaceId> {
        Ok(self.embedding_space_identity()?.id())
    }

    pub fn validate_embedding_space(
        &self,
        expected: &EmbeddingSpaceIdentity,
    ) -> anyhow::Result<()> {
        let actual = self.embedding_space_identity()?;
        // Ids, not the whole struct. The id is the comparability key — engine,
        // model, width — and comparing structs made every provenance field a
        // veto: an index and a runtime naming the same model at the same width
        // were declared different spaces because one recorded a different
        // model-cache fingerprint. That is the check this file had two of.
        consumer_ensure!(
            actual.id() == expected.id(),
            ConsumerErrorCode::EmbeddingSpaceMismatch,
            "index={} ({} {} @{}), runtime={} ({} {} @{})",
            actual.id().as_str(),
            actual.engine.as_str(),
            actual.model_id,
            actual.dimension,
            expected.id().as_str(),
            expected.engine.as_str(),
            expected.model_id,
            expected.dimension,
        );
        Ok(())
    }

    pub fn managed_completeness(&self) -> anyhow::Result<(usize, usize, usize)> {
        let ready_documents = self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE admission_state = 'ready'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let required_chunks = self.conn.query_row(
            "SELECT COUNT(*) FROM chunks c JOIN files f ON f.id = c.file_id
             WHERE f.admission_state = 'ready'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let embedded_chunks = self.conn.query_row(
            "SELECT COUNT(*) FROM chunks c JOIN files f ON f.id = c.file_id
             JOIN vec_chunks v ON v.rowid = c.id
             WHERE f.admission_state = 'ready'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        Ok((ready_documents, required_chunks, embedded_chunks))
    }

    /// Stable identity of the managed corpus membership represented by this
    /// projection. Embeddings and rowids are deliberately excluded: two
    /// embedding spaces are synchronized when they contain the same ready
    /// renditions and stable chunks, not when their coordinates match.
    pub fn managed_snapshot_sha256(&self) -> anyhow::Result<String> {
        fn field(target: &mut Vec<u8>, value: &str) {
            target.extend_from_slice(&(value.len() as u64).to_be_bytes());
            target.extend_from_slice(value.as_bytes());
        }

        let mut bytes = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT f.source_sha256, f.rendition_id, c.chunk_idx,
                    c.chunk_ref, c.text_sha256
               FROM files f
               JOIN chunks c ON c.file_id = f.id
              WHERE f.admission_state = 'ready'
              ORDER BY f.source_sha256, f.rendition_id, c.chunk_idx",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (source, rendition, ordinal, chunk_ref, text_sha256) = row?;
            field(&mut bytes, &source);
            field(&mut bytes, &rendition);
            field(&mut bytes, &ordinal.to_string());
            field(&mut bytes, &chunk_ref);
            field(&mut bytes, &text_sha256);
        }
        Ok(sha256_bytes(&bytes))
    }

    pub fn managed_embedding_work_totals(&self) -> anyhow::Result<(usize, usize)> {
        let (reused, computed) = self.conn.query_row(
            "SELECT COALESCE(SUM(embedding_reused_chunks), 0),
                    COALESCE(SUM(embedding_computed_chunks), 0)
             FROM files WHERE admission_state = 'ready'",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        Ok((reused as usize, computed as usize))
    }

    /// Open an existing index. Returns `Err` if no index exists at `data_dir` or
    /// if `model_id` in the stored metadata mismatches the parameter.
    /// The dimension is read from the DB; callers can inspect it via `status()`.
    pub fn open(
        data_dir: &Path,
        model_id: &str,
        expected_dimension: usize,
    ) -> anyhow::Result<Self> {
        load_sqlite_vec();
        recover_interrupted_index_replacement(data_dir)?;

        let path = db_path(data_dir);
        anyhow::ensure!(
            path.exists(),
            "No semantic index found at {}",
            path.display()
        );

        let conn = Connection::open(&path)
            .with_context(|| format!("Failed to open index at {}", path.display()))?;
        configure_connection(&conn, &path)?;

        // Require the sqlite-vec schema. A missing vec_chunks table means the index
        // was built before this migration; the caller should rebuild.
        let has_vec_table: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='vec_chunks'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        anyhow::ensure!(
            has_vec_table,
            "Index uses legacy schema (no vec_chunks table); rebuild the index"
        );
        let has_files_table: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='files'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        anyhow::ensure!(
            has_files_table,
            "Index uses legacy schema (no files table); rebuild the index"
        );
        // Content-addressed vector lookup reads this, and an index built
        // before it existed would otherwise scan every chunk on every import.
        // Not a schema version: an index is not identity, and creating it
        // costs nothing where it is already there.
        if let Err(error) = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_chunks_text_sha256
                 ON chunks(text_sha256) WHERE text_sha256 IS NOT NULL;",
        ) {
            tracing::debug!("could not ensure the chunk text index: {error}");
        }

        let mut schema_version: i64 = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(s.parse::<i64>().unwrap_or(0))
                },
            )
            .unwrap_or(0);
        if schema_version == 2 {
            Self::migrate_v2_to_v3(&conn)?;
            schema_version = 3;
        }
        if schema_version == 3 {
            Self::migrate_v3_to_v4(&conn)?;
            schema_version = 4;
        }
        if schema_version == 4 {
            Self::migrate_v4_to_v5(&conn)?;
            schema_version = 5;
        }
        if schema_version == 5 {
            Self::migrate_v5_to_v6(&conn)?;
            schema_version = 6;
        }
        if schema_version == 6 {
            Self::migrate_v6_to_v7(&conn)?;
            schema_version = 7;
        }
        if schema_version == 7 {
            Self::migrate_v7_to_v8(&conn)?;
            schema_version = 8;
        }
        if schema_version == 8 {
            Self::migrate_v8_to_v9(&conn)?;
            schema_version = 9;
        }
        if schema_version == 9 {
            Self::migrate_v9_to_v10(&conn)?;
            schema_version = 10;
        }
        if schema_version == 10 {
            Self::migrate_v10_to_v11(&conn)?;
            schema_version = 11;
        }
        if schema_version == 11 {
            Self::migrate_v11_to_v12(&conn)?;
            schema_version = 12;
        }
        anyhow::ensure!(
            schema_version == SCHEMA_VERSION,
            "Index schema version {} is not supported (expected {}); rebuild the index",
            schema_version,
            SCHEMA_VERSION
        );

        let stored_model_id: String = conn
            .query_row("SELECT value FROM meta WHERE key = 'model_id'", [], |row| {
                row.get(0)
            })
            .context("Index is missing model_id metadata")?;

        anyhow::ensure!(
            stored_model_id == model_id,
            "Index was built with model '{}' but requested is '{}'; rebuild the index",
            stored_model_id,
            model_id
        );

        let stored_dimension: usize = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'dimension'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(s.parse::<usize>().unwrap_or(0))
                },
            )
            .unwrap_or(0);

        anyhow::ensure!(
            stored_dimension == expected_dimension,
            "Index dimension mismatch: stored={}, expected={}. Rebuild the index.",
            stored_dimension,
            expected_dimension
        );

        Ok(Self {
            conn,
            dimension: stored_dimension,
            active_root: None,
            active_root_id: None,
        })
    }

    pub fn open_exact(data_dir: &Path, expected: &EmbeddingSpaceIdentity) -> anyhow::Result<Self> {
        let index = Self::open(data_dir, &expected.model_id, expected.dimension)?;
        index.validate_embedding_space(expected)?;
        Ok(index)
    }

    pub fn open_for_maintenance(data_dir: &Path) -> anyhow::Result<Self> {
        load_sqlite_vec();
        recover_interrupted_index_replacement(data_dir)?;
        let path = db_path(data_dir);
        anyhow::ensure!(
            path.exists(),
            "No semantic index found at {}",
            path.display()
        );
        let conn = Connection::open(&path)
            .with_context(|| format!("Failed to open index at {}", path.display()))?;
        configure_connection(&conn, &path)?;
        let mut schema_version: i64 = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(s.parse::<i64>().unwrap_or(0))
                },
            )
            .unwrap_or(0);
        if schema_version == 2 {
            Self::migrate_v2_to_v3(&conn)?;
            schema_version = 3;
        }
        if schema_version == 3 {
            Self::migrate_v3_to_v4(&conn)?;
            schema_version = 4;
        }
        if schema_version == 4 {
            Self::migrate_v4_to_v5(&conn)?;
            schema_version = 5;
        }
        if schema_version == 5 {
            Self::migrate_v5_to_v6(&conn)?;
            schema_version = 6;
        }
        if schema_version == 6 {
            Self::migrate_v6_to_v7(&conn)?;
            schema_version = 7;
        }
        if schema_version == 7 {
            Self::migrate_v7_to_v8(&conn)?;
            schema_version = 8;
        }
        if schema_version == 8 {
            Self::migrate_v8_to_v9(&conn)?;
            schema_version = 9;
        }
        if schema_version == 9 {
            Self::migrate_v9_to_v10(&conn)?;
            schema_version = 10;
        }
        if schema_version == 10 {
            Self::migrate_v10_to_v11(&conn)?;
            schema_version = 11;
        }
        if schema_version == 11 {
            Self::migrate_v11_to_v12(&conn)?;
            schema_version = 12;
        }
        anyhow::ensure!(
            schema_version == SCHEMA_VERSION,
            "Index schema version {} is not supported (expected {}); rebuild the index",
            schema_version,
            SCHEMA_VERSION
        );
        let dimension = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'dimension'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(s.parse::<usize>().unwrap_or(0))
                },
            )
            .unwrap_or(0);
        Ok(Self {
            conn,
            dimension,
            active_root: None,
            active_root_id: None,
        })
    }

    /// Create a new empty index at the specified path.
    ///
    /// Test-only: production code creates an index from the identity of the
    /// embedder that will fill it, never from a model name alone.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn create_at_path(
        path: &Path,
        model_id: &str,
        dimension: usize,
        engine: EmbeddingEngine,
        root_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Self::create_at_path_exact(
            path,
            &EmbeddingSpaceIdentity::for_test(engine, model_id, dimension),
            root_path,
        )
    }

    fn create_at_path_exact(
        path: &Path,
        embedding_identity: &EmbeddingSpaceIdentity,
        root_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        // An unresolved artifact revision is recorded, not refused. It names
        // provenance the runtime could not fingerprint — SBERT keeps its
        // weights outside this cache root — and provenance no longer decides
        // whether an index can be read: the space id is engine, model and
        // width, all three of which are known here. Refusing meant an engine
        // whose weights live elsewhere could not build an index at all.
        if !embedding_identity.is_resolved() {
            tracing::debug!(
                "creating an index with unresolved artifact revision '{}' for {} {}",
                embedding_identity.artifact_revision,
                embedding_identity.engine.as_str(),
                embedding_identity.model_id
            );
        }
        load_sqlite_vec();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if path.exists() {
            std::fs::remove_file(path)?;
        }

        // Remove orphaned WAL/SHM files for this specific path.
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let mut shm = path.as_os_str().to_owned();
        shm.push("-shm");
        let _ = std::fs::remove_file(wal);
        let _ = std::fs::remove_file(shm);

        let conn = Connection::open(path)
            .with_context(|| format!("Failed to create index at {}", path.display()))?;
        configure_connection(&conn, path)?;

        Self::create_schema(&conn, embedding_identity)?;

        let mut index = Self {
            conn,
            dimension: embedding_identity.dimension,
            active_root: None,
            active_root_id: None,
        };
        if let Some(root) = root_path {
            index.activate_root(root)?;
        }
        Ok(index)
    }

    /// Create a new empty index at `data_dir` (schema only, no files indexed).
    /// Removes any existing index at that path.
    ///
    /// Test-only, for the same reason as [`Self::create_at_path`].
    #[cfg(any(test, feature = "test-utils"))]
    pub fn create(
        data_dir: &Path,
        model_id: &str,
        dimension: usize,
        engine: EmbeddingEngine,
        root_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Self::create_at_path(&db_path(data_dir), model_id, dimension, engine, root_path)
    }

    pub fn create_exact(
        data_dir: &Path,
        identity: &EmbeddingSpaceIdentity,
        root_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Self::create_at_path_exact(&db_path(data_dir), identity, root_path)
    }
}

#[allow(clippy::too_many_arguments)]
fn source_origin_from_parts(
    origin_type: &str,
    page: Option<i64>,
    line: Option<i64>,
    col: Option<i64>,
    bbox_x: Option<f64>,
    bbox_y: Option<f64>,
    bbox_w: Option<f64>,
    bbox_h: Option<f64>,
) -> Option<SourceOrigin> {
    match origin_type {
        "text_file" => Some(SourceOrigin::TextFile {
            line: line.unwrap_or(0) as u32,
            col: col.unwrap_or(0) as u32,
        }),
        "pdf_page" => {
            let bbox = match (bbox_x, bbox_y, bbox_w, bbox_h) {
                (Some(x), Some(y), Some(w), Some(h)) => Some(BoundingBox {
                    x: x as f32,
                    y: y as f32,
                    width: w as f32,
                    height: h as f32,
                }),
                _ => None,
            };
            Some(SourceOrigin::PdfPage {
                page: page.unwrap_or(0) as u32,
                bbox,
            })
        }
        _ => None,
    }
}
