//! The text of every library document, kept current whether or not the
//! document is embedded.
//!
//! **Why this is its own store.** Exact search over a paged document (PDF,
//! EPUB, …) needs the document's extracted text, and extracting it is the
//! expensive part of the search. That text used to live only in the semantic
//! index's `files.full_text`, which exists for a document only once it has
//! been embedded, in a database that cannot exist at all without an embedding
//! model. A root that was never embedded therefore re-extracted every PDF on
//! every exact search, which is not viable for a large library.
//!
//! This store owns *the text exact search reads*, for every document of every
//! library root, independently of embeddings:
//!
//! - It lives in its own database beside the semantic index, so a semantic
//!   build (which publishes by replacing its file), a model change or an index
//!   deletion never takes it with them.
//! - Rows are keyed by canonical path and guarded by on-disk identity (size
//!   and modification time), exactly as the semantic index guards its own, so
//!   text that no longer matches the file is never served.
//! - It is written by one owner only, [`upkeep`], which reconciles it against
//!   the library and against the semantic index. Search only reads it.
//!
//! **Which text a row holds.** The best reading available without inference:
//! the semantic index's rendition when it holds a current one for the file —
//! that reading may include text recognized inside images, and copying it
//! costs no extraction — and otherwise the document's own glyphs, read with
//! [`crate::extract::native_text_registry`], the same read exact search used
//! to perform live. A row records which of the two it holds ([`TextSource`]),
//! so upkeep can tell when a better or newer reading has become available.
//!
//! **Source maps.** A native reading's map is word-granular, and stored whole
//! so a match served from here resolves to exactly the origin (page and word
//! box) the live read would have produced. It is stored in a compact binary
//! encoding ([`encode_source_map`]): a word's box shares its line's `y` and
//! height with the word before it, and those repeats are elided, then the
//! whole map is deflated. A copied semantic rendition carries the
//! chunk-granular map the semantic index reconstructs, the same fidelity index
//! -backed exact search always had for embedded documents.

pub mod upkeep;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use tracing::warn;

use crate::metadata::cache::FileIdentity;
use crate::types::{
    BoundingBox, ByteRange, SourceMap, SourceOrigin, SourceSegment, TextProvenance,
};

/// Bumped whenever the table layout or the source-map encoding changes. The
/// store is derived data — every row can be produced again from the library —
/// so a store written under another version is dropped and refilled rather
/// than migrated.
const SCHEMA_VERSION: i64 = 1;

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(3);

fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join("document_text.db")
}

/// Which reading a row holds. Stored as text so a row can be compared against
/// what upkeep would write now without decoding anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextSource {
    /// The document's own glyphs, read under this extractor recipe. A recipe
    /// change (extraction improved) makes every native row stale.
    Native { recipe: String },
    /// A copy of the semantic index's rendition of the file, as it stood when
    /// the semantic row was written at `indexed_at_ms`.
    Semantic { indexed_at_ms: i64 },
}

impl TextSource {
    /// The native reading this build of Wilkes produces.
    pub fn current_native() -> Self {
        Self::Native {
            recipe: crate::embed::identity::EXTRACTOR_RECIPE_VERSION.to_string(),
        }
    }

    fn to_stored(&self) -> String {
        match self {
            Self::Native { recipe } => format!("native:{recipe}"),
            Self::Semantic { indexed_at_ms } => format!("semantic:{indexed_at_ms}"),
        }
    }

    fn from_stored(stored: &str) -> Option<Self> {
        if let Some(recipe) = stored.strip_prefix("native:") {
            return Some(Self::Native {
                recipe: recipe.to_string(),
            });
        }
        let indexed_at_ms = stored.strip_prefix("semantic:")?.parse().ok()?;
        Some(Self::Semantic { indexed_at_ms })
    }
}

/// What a row says about itself, without its text: enough to decide whether
/// upkeep has anything to do for the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredStamp {
    pub identity: FileIdentity,
    pub source: TextSource,
    /// The reading failed for this identity. Upkeep does not retry until the
    /// file changes; exact search reads such a file live, and reports the
    /// failure the way it always has.
    pub failed: bool,
}

/// One document's text, ready for exact search.
pub struct StoredDocument {
    pub text: String,
    pub source_map: SourceMap,
}

pub struct DocumentTextIndex {
    conn: Mutex<Connection>,
}

impl DocumentTextIndex {
    /// Open the store in `data_dir`, creating it when absent and starting it
    /// over when it was written under another schema version.
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("Failed to create {}", data_dir.display()))?;
        let path = db_path(data_dir);
        let conn = Connection::open(&path)
            .with_context(|| format!("Failed to open {}", path.display()))?;
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS meta (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )?;
        let stored_version: Option<i64> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|value| value.parse().ok());
        if stored_version != Some(SCHEMA_VERSION) {
            if stored_version.is_some() {
                warn!(
                    "document text index at {} is schema {stored_version:?}, expected {SCHEMA_VERSION}; starting it over",
                    path.display()
                );
            }
            conn.execute_batch("DROP TABLE IF EXISTS documents;")?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS documents (
                 path           TEXT PRIMARY KEY,
                 size_bytes     INTEGER NOT NULL,
                 modified_at_ms INTEGER NOT NULL,
                 source         TEXT NOT NULL,
                 full_text      TEXT,
                 source_map     BLOB,
                 error          TEXT,
                 written_at_ms  INTEGER NOT NULL
             );",
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The key a path is stored under. A path that still exists is
    /// canonicalized; one that does not (a removal) canonicalizes its parent,
    /// which is how it was keyed while it existed.
    pub fn key_for(path: &Path) -> PathBuf {
        if let Ok(canonical) = std::fs::canonicalize(path) {
            return canonical;
        }
        match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => std::fs::canonicalize(parent)
                .unwrap_or_else(|_| parent.to_path_buf())
                .join(name),
            _ => path.to_path_buf(),
        }
    }

    fn key_string(path: &Path) -> String {
        Self::key_for(path).to_string_lossy().into_owned()
    }

    /// The document's stored text, or `None` when the store cannot serve it:
    /// no row, a failed reading, or a file that changed on disk since its row
    /// was written. Stale text is never served.
    pub fn document_for_path(&self, path: &Path) -> anyhow::Result<Option<StoredDocument>> {
        let key = Self::key_string(path);
        let row = self
            .conn()
            .query_row(
                "SELECT size_bytes, modified_at_ms, full_text, source_map
                   FROM documents WHERE path = ?1",
                params![key],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                    ))
                },
            )
            .optional()?;
        // Decoded after the lock is released: concurrent searches only
        // serialize on the copy, not on inflating the map.
        let Some((size_bytes, modified_at_ms, Some(text), map)) = row else {
            return Ok(None);
        };
        let Some(identity) = FileIdentity::for_path(path) else {
            return Ok(None);
        };
        if identity.size_bytes != size_bytes || identity.modified_at_ms != modified_at_ms {
            return Ok(None);
        }
        let source_map = match map {
            Some(bytes) => decode_source_map(&bytes)
                .with_context(|| format!("stored source map of {}", path.display()))?,
            None => SourceMap {
                segments: Vec::new(),
            },
        };
        Ok(Some(StoredDocument { text, source_map }))
    }

    /// Every row's stamp, by key.
    pub fn stamps(&self) -> anyhow::Result<HashMap<PathBuf, StoredStamp>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT path, size_bytes, modified_at_ms, source, error IS NOT NULL FROM documents",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(path, size_bytes, modified_at_ms, source, failed)| {
                let source = TextSource::from_stored(&source)?;
                Some((
                    PathBuf::from(path),
                    StoredStamp {
                        identity: FileIdentity {
                            size_bytes,
                            modified_at_ms,
                        },
                        source,
                        failed,
                    },
                ))
            })
            .collect())
    }

    /// One row's stamp.
    pub fn stamp(&self, path: &Path) -> anyhow::Result<Option<StoredStamp>> {
        let key = Self::key_string(path);
        let row = self
            .conn()
            .query_row(
                "SELECT size_bytes, modified_at_ms, source, error IS NOT NULL
                   FROM documents WHERE path = ?1",
                params![key],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                },
            )
            .optional()?;
        Ok(
            row.and_then(|(size_bytes, modified_at_ms, source, failed)| {
                Some(StoredStamp {
                    identity: FileIdentity {
                        size_bytes,
                        modified_at_ms,
                    },
                    source: TextSource::from_stored(&source)?,
                    failed,
                })
            }),
        )
    }

    /// Store a reading of `path` taken while it had `identity`.
    pub fn write_text(
        &self,
        path: &Path,
        identity: FileIdentity,
        source: &TextSource,
        text: &str,
        source_map: &SourceMap,
    ) -> anyhow::Result<()> {
        let key = Self::key_string(path);
        let map = encode_source_map(source_map)?;
        self.conn().execute(
            "INSERT OR REPLACE INTO documents
                 (path, size_bytes, modified_at_ms, source, full_text, source_map, error, written_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
            params![
                key,
                identity.size_bytes,
                identity.modified_at_ms,
                source.to_stored(),
                text,
                map,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Record that reading `path` under `identity` failed, so upkeep does not
    /// try again until the file changes.
    pub fn write_failure(
        &self,
        path: &Path,
        identity: FileIdentity,
        source: &TextSource,
        error: &str,
    ) -> anyhow::Result<()> {
        let key = Self::key_string(path);
        self.conn().execute(
            "INSERT OR REPLACE INTO documents
                 (path, size_bytes, modified_at_ms, source, full_text, source_map, error, written_at_ms)
             VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6)",
            params![
                key,
                identity.size_bytes,
                identity.modified_at_ms,
                source.to_stored(),
                error,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Remove the rows of these keys (as returned by [`Self::stamps`]).
    pub fn remove_keys(&self, keys: &[PathBuf]) -> anyhow::Result<usize> {
        if keys.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut removed = 0;
        {
            let mut stmt = tx.prepare("DELETE FROM documents WHERE path = ?1")?;
            for key in keys {
                removed += stmt.execute(params![key.to_string_lossy()])?;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    /// Move a row from one key to another, keeping its text. Used for renames,
    /// whose identity — and therefore whose reading — did not change.
    pub fn rename_key(&self, old_key: &Path, new_key: &Path) -> anyhow::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM documents WHERE path = ?1",
            params![new_key.to_string_lossy()],
        )?;
        let moved = tx.execute(
            "UPDATE documents SET path = ?2 WHERE path = ?1",
            params![old_key.to_string_lossy(), new_key.to_string_lossy()],
        )?;
        tx.commit()?;
        Ok(moved > 0)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ── Source-map encoding ──────────────────────────────────────────────────────
//
// One version byte, a segment count, then per segment:
//
//   flags   u8      see the FLAG_* constants
//   start   zigzag  delta from the previous segment's end (maps of overlapping
//                   chunks go backwards, so it is signed)
//   len     varint  end - start
//   origin          TextFile: line, col (varints)
//                   PdfPage:  page (varint, elided when FLAG_SAME_PAGE), then
//                             when FLAG_BBOX: x, [y], width, [height] as raw
//                             f32 bits — y and height elided when equal to the
//                             previous box's, which they are for every word of
//                             a line after the first
//   provenance      only for PROVENANCE_JSON: length-prefixed JSON
//
// Boxes are stored bit-exact, never rounded: a match served from here must
// resolve to the same box a live read produces, because the interface compares
// boxes for equality (a bookmark is recognized by its origin).
//
// The whole sequence is then deflated.

const MAP_ENCODING_VERSION: u8 = 1;

const FLAG_TEXT_FILE: u8 = 1 << 0;
const FLAG_BBOX: u8 = 1 << 1;
const FLAG_SAME_PAGE: u8 = 1 << 2;
const FLAG_SAME_Y: u8 = 1 << 3;
const FLAG_SAME_HEIGHT: u8 = 1 << 4;
const PROVENANCE_SHIFT: u8 = 5;
const PROVENANCE_MASK: u8 = 0b11 << PROVENANCE_SHIFT;
const PROVENANCE_NATIVE: u8 = 0;
const PROVENANCE_UNRECORDED: u8 = 1;
const PROVENANCE_JSON: u8 = 2;

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_zigzag(out: &mut Vec<u8>, value: i64) {
    put_varint(out, ((value << 1) ^ (value >> 63)) as u64);
}

fn put_f32(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(&value.to_bits().to_le_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> anyhow::Result<u8> {
        let byte = *self
            .bytes
            .get(self.at)
            .ok_or_else(|| anyhow::anyhow!("source map truncated"))?;
        self.at += 1;
        Ok(byte)
    }

    fn varint(&mut self) -> anyhow::Result<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        anyhow::bail!("source map varint overflows")
    }

    fn zigzag(&mut self) -> anyhow::Result<i64> {
        let raw = self.varint()?;
        Ok(((raw >> 1) as i64) ^ -((raw & 1) as i64))
    }

    fn f32(&mut self) -> anyhow::Result<f32> {
        let end = self.at + 4;
        let bytes: [u8; 4] = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| anyhow::anyhow!("source map truncated"))?
            .try_into()?;
        self.at = end;
        Ok(f32::from_bits(u32::from_le_bytes(bytes)))
    }

    fn slice(&mut self, len: usize) -> anyhow::Result<&'a [u8]> {
        let end = self.at + len;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| anyhow::anyhow!("source map truncated"))?;
        self.at = end;
        Ok(slice)
    }
}

pub fn encode_source_map(map: &SourceMap) -> anyhow::Result<Vec<u8>> {
    let mut raw = Vec::with_capacity(map.segments.len() * 12 + 8);
    raw.push(MAP_ENCODING_VERSION);
    put_varint(&mut raw, map.segments.len() as u64);

    let mut prev_end: i64 = 0;
    let mut prev_page: Option<u32> = None;
    let mut prev_y: Option<u32> = None;
    let mut prev_height: Option<u32> = None;

    for segment in &map.segments {
        let start = segment.text_range.start as i64;
        let end = segment.text_range.end as i64;
        let provenance_json = match &segment.provenance {
            TextProvenance::Native => None,
            TextProvenance::Unrecorded => None,
            other => Some(serde_json::to_vec(other)?),
        };
        let provenance_tag = match &segment.provenance {
            TextProvenance::Native => PROVENANCE_NATIVE,
            TextProvenance::Unrecorded => PROVENANCE_UNRECORDED,
            _ => PROVENANCE_JSON,
        };
        let mut flags = provenance_tag << PROVENANCE_SHIFT;
        let mut body = Vec::with_capacity(24);
        put_zigzag(&mut body, start - prev_end);
        put_zigzag(&mut body, end - start);
        match &segment.origin {
            SourceOrigin::TextFile { line, col } => {
                flags |= FLAG_TEXT_FILE;
                put_varint(&mut body, u64::from(*line));
                put_varint(&mut body, u64::from(*col));
            }
            SourceOrigin::PdfPage { page, bbox } => {
                if prev_page == Some(*page) {
                    flags |= FLAG_SAME_PAGE;
                } else {
                    put_varint(&mut body, u64::from(*page));
                    prev_page = Some(*page);
                }
                if let Some(bbox) = bbox {
                    flags |= FLAG_BBOX;
                    put_f32(&mut body, bbox.x);
                    let y = bbox.y.to_bits();
                    if prev_y == Some(y) {
                        flags |= FLAG_SAME_Y;
                    } else {
                        put_f32(&mut body, bbox.y);
                        prev_y = Some(y);
                    }
                    put_f32(&mut body, bbox.width);
                    let height = bbox.height.to_bits();
                    if prev_height == Some(height) {
                        flags |= FLAG_SAME_HEIGHT;
                    } else {
                        put_f32(&mut body, bbox.height);
                        prev_height = Some(height);
                    }
                }
            }
        }
        raw.push(flags);
        raw.extend_from_slice(&body);
        if let Some(json) = provenance_json {
            put_varint(&mut raw, json.len() as u64);
            raw.extend_from_slice(&json);
        }
        prev_end = end;
    }

    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&raw)?;
    Ok(encoder.finish()?)
}

pub fn decode_source_map(bytes: &[u8]) -> anyhow::Result<SourceMap> {
    let mut raw = Vec::new();
    flate2::read::DeflateDecoder::new(bytes).read_to_end(&mut raw)?;
    let mut reader = Reader { bytes: &raw, at: 0 };
    let version = reader.byte()?;
    anyhow::ensure!(
        version == MAP_ENCODING_VERSION,
        "unknown source map encoding {version}"
    );
    let count = reader.varint()? as usize;
    // Every segment takes at least three bytes; a count the payload cannot
    // hold is corruption, not a reason to allocate.
    anyhow::ensure!(
        count <= raw.len(),
        "source map count {count} exceeds payload"
    );
    let mut segments = Vec::with_capacity(count);

    let mut prev_end: i64 = 0;
    let mut prev_page: Option<u32> = None;
    let mut prev_y: Option<f32> = None;
    let mut prev_height: Option<f32> = None;

    for _ in 0..count {
        let flags = reader.byte()?;
        let start = prev_end + reader.zigzag()?;
        let end = start + reader.zigzag()?;
        anyhow::ensure!(
            start >= 0 && end >= start,
            "source map range {start}..{end} is invalid"
        );
        let origin = if flags & FLAG_TEXT_FILE != 0 {
            SourceOrigin::TextFile {
                line: u32::try_from(reader.varint()?)?,
                col: u32::try_from(reader.varint()?)?,
            }
        } else {
            let page = if flags & FLAG_SAME_PAGE != 0 {
                prev_page.ok_or_else(|| anyhow::anyhow!("repeated page with no page before"))?
            } else {
                let page = u32::try_from(reader.varint()?)?;
                prev_page = Some(page);
                page
            };
            let bbox = if flags & FLAG_BBOX != 0 {
                let x = reader.f32()?;
                let y = if flags & FLAG_SAME_Y != 0 {
                    prev_y.ok_or_else(|| anyhow::anyhow!("repeated y with no box before"))?
                } else {
                    let y = reader.f32()?;
                    prev_y = Some(y);
                    y
                };
                let width = reader.f32()?;
                let height = if flags & FLAG_SAME_HEIGHT != 0 {
                    prev_height
                        .ok_or_else(|| anyhow::anyhow!("repeated height with no box before"))?
                } else {
                    let height = reader.f32()?;
                    prev_height = Some(height);
                    height
                };
                Some(BoundingBox {
                    x,
                    y,
                    width,
                    height,
                })
            } else {
                None
            };
            SourceOrigin::PdfPage { page, bbox }
        };
        let provenance = match (flags & PROVENANCE_MASK) >> PROVENANCE_SHIFT {
            PROVENANCE_NATIVE => TextProvenance::Native,
            PROVENANCE_UNRECORDED => TextProvenance::Unrecorded,
            PROVENANCE_JSON => {
                let len = reader.varint()? as usize;
                serde_json::from_slice(reader.slice(len)?)?
            }
            other => anyhow::bail!("unknown provenance tag {other}"),
        };
        segments.push(SourceSegment {
            text_range: ByteRange {
                start: start as usize,
                end: end as usize,
            },
            origin,
            provenance,
        });
        prev_end = end;
    }
    Ok(SourceMap { segments })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::types::RegionKind;
    use tempfile::tempdir;

    fn word(start: usize, end: usize, page: u32, x: f32, y: f32) -> SourceSegment {
        SourceSegment {
            text_range: ByteRange { start, end },
            origin: SourceOrigin::PdfPage {
                page,
                bbox: Some(BoundingBox {
                    x,
                    y,
                    width: 21.375,
                    height: 11.0,
                }),
            },
            provenance: TextProvenance::Native,
        }
    }

    /// Every shape a map can take — words sharing a line, a page break, a
    /// boxless segment, text-file origins, overlapping chunk ranges and each
    /// kind of provenance — survives the encoding bit for bit.
    #[test]
    fn a_source_map_round_trips_exactly() {
        let map = SourceMap {
            segments: vec![
                word(0, 5, 1, 72.0, 700.125),
                word(6, 11, 1, 99.5, 700.125),
                word(12, 20, 1, 130.0, 688.0),
                word(21, 24, 2, 72.0, 700.125),
                SourceSegment {
                    text_range: ByteRange { start: 25, end: 40 },
                    origin: SourceOrigin::PdfPage {
                        page: 2,
                        bbox: None,
                    },
                    provenance: TextProvenance::Unrecorded,
                },
                // A chunk map overlaps: this one starts before the last ended.
                SourceSegment {
                    text_range: ByteRange { start: 30, end: 52 },
                    origin: SourceOrigin::PdfPage {
                        page: 3,
                        bbox: Some(BoundingBox {
                            x: 0.1,
                            y: f32::MIN_POSITIVE,
                            width: 1e-7,
                            height: 612.0,
                        }),
                    },
                    provenance: TextProvenance::ImageOcr {
                        image_id: "img-1".to_string(),
                        confidence: Some(0.75),
                        kind: RegionKind::default(),
                    },
                },
                SourceSegment {
                    text_range: ByteRange { start: 52, end: 60 },
                    origin: SourceOrigin::PdfPage {
                        page: 3,
                        bbox: None,
                    },
                    provenance: TextProvenance::ImageDescription {
                        image_id: "img-2".to_string(),
                        analyzer_id: "describer".to_string(),
                    },
                },
                SourceSegment {
                    text_range: ByteRange { start: 60, end: 64 },
                    origin: SourceOrigin::TextFile { line: 9, col: 3 },
                    provenance: TextProvenance::Native,
                },
            ],
        };

        let decoded = decode_source_map(&encode_source_map(&map).unwrap()).unwrap();

        assert_eq!(decoded.segments.len(), map.segments.len());
        for (got, want) in decoded.segments.iter().zip(&map.segments) {
            assert_eq!(got.text_range, want.text_range);
            assert_eq!(got.origin, want.origin);
            assert_eq!(got.provenance, want.provenance);
        }
    }

    #[test]
    fn an_empty_map_round_trips() {
        let map = SourceMap {
            segments: Vec::new(),
        };
        let decoded = decode_source_map(&encode_source_map(&map).unwrap()).unwrap();
        assert!(decoded.segments.is_empty());
    }

    #[test]
    fn a_corrupt_map_is_an_error_not_a_panic() {
        assert!(decode_source_map(b"not deflate at all").is_err());
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&[MAP_ENCODING_VERSION, 200]).unwrap();
        assert!(decode_source_map(&encoder.finish().unwrap()).is_err());
    }

    /// Repeated line geometry is what keeps a word-granular map affordable:
    /// a page of words on shared lines must cost well under the raw
    /// 28-bytes-a-word the fields would take unencoded.
    #[test]
    fn words_on_shared_lines_encode_compactly() {
        let mut segments = Vec::new();
        let mut at = 0;
        for line in 0..50 {
            for column in 0..12 {
                segments.push(word(
                    at,
                    at + 6,
                    1,
                    72.0 + column as f32 * 37.25,
                    700.0 - line as f32 * 13.5,
                ));
                at += 7;
            }
        }
        let count = segments.len();
        let encoded = encode_source_map(&SourceMap { segments }).unwrap();
        assert!(
            encoded.len() < count * 14,
            "{} bytes for {count} words",
            encoded.len()
        );
    }

    pub(crate) fn sample_map(len: usize) -> SourceMap {
        SourceMap {
            segments: vec![SourceSegment {
                text_range: ByteRange { start: 0, end: len },
                origin: SourceOrigin::PdfPage {
                    page: 1,
                    bbox: None,
                },
                provenance: TextProvenance::Native,
            }],
        }
    }

    #[test]
    fn stored_text_is_served_only_while_the_file_is_unchanged() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(&dir.path().join("data")).unwrap();
        let pdf = dir.path().join("paper.pdf");
        std::fs::write(&pdf, "%PDF-1.7 bytes").unwrap();
        let identity = FileIdentity::for_path(&pdf).unwrap();

        store
            .write_text(
                &pdf,
                identity,
                &TextSource::current_native(),
                "alpha beta",
                &sample_map(10),
            )
            .unwrap();
        let served = store.document_for_path(&pdf).unwrap().unwrap();
        assert_eq!(served.text, "alpha beta");
        assert_eq!(served.source_map.segments.len(), 1);

        let stamp = store.stamp(&pdf).unwrap().unwrap();
        assert_eq!(stamp.identity, identity);
        assert_eq!(stamp.source, TextSource::current_native());
        assert!(!stamp.failed);

        // A different size is a different file.
        std::fs::write(&pdf, "%PDF-1.7 different bytes").unwrap();
        assert!(store.document_for_path(&pdf).unwrap().is_none());
    }

    #[test]
    fn a_failed_reading_is_remembered_but_never_served() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(dir.path()).unwrap();
        let pdf = dir.path().join("broken.pdf");
        std::fs::write(&pdf, "%PDF-1.7 broken").unwrap();
        let identity = FileIdentity::for_path(&pdf).unwrap();

        store
            .write_failure(&pdf, identity, &TextSource::current_native(), "no xref")
            .unwrap();

        assert!(store.document_for_path(&pdf).unwrap().is_none());
        assert!(store.stamp(&pdf).unwrap().unwrap().failed);
    }

    #[test]
    fn rows_are_renamed_and_removed_by_key() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(dir.path()).unwrap();
        let old = dir.path().join("old.pdf");
        std::fs::write(&old, "%PDF-1.7 bytes").unwrap();
        let identity = FileIdentity::for_path(&old).unwrap();
        store
            .write_text(
                &old,
                identity,
                &TextSource::current_native(),
                "text",
                &sample_map(4),
            )
            .unwrap();

        let new = dir.path().join("new.pdf");
        std::fs::rename(&old, &new).unwrap();
        let old_key = DocumentTextIndex::key_for(&old);
        let new_key = DocumentTextIndex::key_for(&new);
        assert!(store.rename_key(&old_key, &new_key).unwrap());
        assert_eq!(store.document_for_path(&new).unwrap().unwrap().text, "text");

        let stamps = store.stamps().unwrap();
        assert_eq!(
            stamps.keys().cloned().collect::<Vec<_>>(),
            vec![new_key.clone()]
        );
        assert_eq!(store.remove_keys(&[new_key]).unwrap(), 1);
        assert!(store.stamps().unwrap().is_empty());
    }

    #[test]
    fn a_store_from_another_schema_starts_over() {
        let dir = tempdir().unwrap();
        {
            let store = DocumentTextIndex::open(dir.path()).unwrap();
            let pdf = dir.path().join("a.pdf");
            std::fs::write(&pdf, "%PDF").unwrap();
            store
                .write_text(
                    &pdf,
                    FileIdentity::for_path(&pdf).unwrap(),
                    &TextSource::current_native(),
                    "t",
                    &sample_map(1),
                )
                .unwrap();
            store
                .conn()
                .execute(
                    "UPDATE meta SET value = '0' WHERE key = 'schema_version'",
                    [],
                )
                .unwrap();
        }
        let reopened = DocumentTextIndex::open(dir.path()).unwrap();
        assert!(reopened.stamps().unwrap().is_empty());
    }

    #[test]
    fn text_sources_round_trip_through_storage() {
        for source in [
            TextSource::current_native(),
            TextSource::Semantic {
                indexed_at_ms: 1_700_000_000_123,
            },
        ] {
            assert_eq!(TextSource::from_stored(&source.to_stored()), Some(source));
        }
        assert_eq!(TextSource::from_stored("garbage"), None);
    }
}
