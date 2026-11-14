//! Keeping the document text index current: the store's only writer.
//!
//! Two entry points, one per kind of evidence that the store may be behind:
//!
//! - [`reconcile_library`] compares the store against a full listing of the
//!   library and against the semantic index. It is what runs at startup, when
//!   the library's roots change, and when the semantic index gains renditions
//!   (a build finishing). Files whose row already matches cost one `stat` and
//!   a map lookup, so a reconcile over an unchanged library does no reading.
//! - [`apply_changes`] handles one filesystem watcher batch, touching only the
//!   files the batch names (and the descendants of directories it says
//!   appeared), and re-keys renames instead of reading them again.
//!
//! For each document the decision is the same ([`plan_document`]): copy the
//! semantic index's rendition when it holds a current one the store does not
//! already have; otherwise read the document's glyphs, unless the store
//! already holds a reading of this exact file. Nothing here runs inference:
//! copies read rows, and fresh readings use
//! [`crate::extract::native_text_registry`].
//!
//! **Cancellation.** A cancel is a flag, checked before each document. It
//! returns control at once; a document already being read finishes in the
//! background and is the only work the cancel does not stop.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use tracing::{error, info};

use super::{DocumentTextIndex, StoredStamp, TextSource};
use crate::directory_watcher::{collect_supported_files, DirectoryChangeBatch};
use crate::embed::index::RenditionStamp;
use crate::embed::index::SemanticIndex;
use crate::extract::ExtractorRegistry;
use crate::metadata::cache::FileIdentity;
use crate::types::FileType;

/// The semantic index as the host holds it.
pub type SemanticHandle = Arc<Mutex<Option<SemanticIndex>>>;

/// What one upkeep pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpkeepReport {
    /// Renditions copied from the semantic index.
    pub copied: usize,
    /// Documents read for their glyphs.
    pub extracted: usize,
    /// Documents whose reading failed (recorded, not retried until changed).
    pub failed: usize,
    /// Rows re-keyed to a renamed file.
    pub renamed: usize,
    /// Rows dropped because their document left the library.
    pub removed: usize,
    /// The pass stopped early; some documents were not visited.
    pub cancelled: bool,
}

impl UpkeepReport {
    pub fn changed_anything(&self) -> bool {
        self.copied + self.extracted + self.failed + self.renamed + self.removed > 0
    }
}

/// The library as one full listing saw it.
pub struct LibraryDocuments {
    /// Every paged document under the library's roots.
    pub paths: Vec<PathBuf>,
    /// Roots that could not be listed. Rows under them are kept: a root that
    /// is briefly unreachable has not been emptied.
    pub unlisted_roots: Vec<PathBuf>,
}

/// The paged documents a watcher batch touched.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ChangedDocuments {
    /// Documents that exist and may be new, edited or renamed-to.
    pub changed: Vec<PathBuf>,
    /// Paths that no longer exist, files or directories.
    pub removed: Vec<PathBuf>,
}

impl ChangedDocuments {
    /// The documents of `batch` this store keeps text for.
    ///
    /// A directory that appeared is expanded into its documents, whatever
    /// else the batch holds: a folder moved into the library from elsewhere
    /// arrives as that folder alone, and its books would otherwise wait for
    /// the next full reconcile. Expanding costs a walk and a `stat` per file;
    /// only documents whose identity moved are read. The watched root itself
    /// is not expanded — an event on it says only that something inside
    /// changed, and that something has its own event.
    pub fn from_batch(
        batch: &DirectoryChangeBatch,
        supported_extensions: &[String],
        max_file_size: u64,
    ) -> Self {
        let mut candidates = batch.changed.clone();
        for dir in &batch.appeared_dirs {
            if dir != &batch.root {
                collect_supported_files(dir, supported_extensions, &mut candidates);
            }
        }
        let mut seen = HashSet::new();
        let changed = candidates
            .into_iter()
            .filter(|path| is_paged_document(path, supported_extensions, max_file_size))
            .filter(|path| seen.insert(path.clone()))
            .collect();
        Self {
            changed,
            removed: batch.removed.clone(),
        }
    }
}

fn is_paged_document(path: &Path, supported_extensions: &[String], max_file_size: u64) -> bool {
    if FileType::detect(path, supported_extensions) != Some(FileType::Paged) {
        return false;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    meta.is_file() && (max_file_size == 0 || meta.len() <= max_file_size)
}

/// What a document needs.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Work {
    Copy {
        path: PathBuf,
        identity: FileIdentity,
    },
    Extract {
        path: PathBuf,
        identity: FileIdentity,
    },
}

/// Decide what, if anything, one document needs.
///
/// - A current semantic rendition the store does not hold is copied: it is
///   at least the glyph reading, may hold more (text recognized in images),
///   and costs no extraction.
/// - Otherwise a row for this exact file is kept if it is a copied rendition
///   (still a reading of these bytes, even if the semantic index has since
///   dropped it) or a native reading under the current recipe, failed or not.
/// - Anything else is read.
fn plan_document(
    path: &Path,
    identity: FileIdentity,
    stored: Option<&StoredStamp>,
    rendition: Option<RenditionStamp>,
) -> Option<Work> {
    let stored = stored.filter(|stamp| stamp.identity == identity);
    let rendition = rendition.filter(|stamp| {
        stamp.size_bytes == identity.size_bytes && stamp.modified_at_ms == identity.modified_at_ms
    });
    if let Some(rendition) = rendition {
        let wanted = TextSource::Semantic {
            indexed_at_ms: rendition.indexed_at_ms,
        };
        if stored.is_some_and(|stamp| stamp.source == wanted) {
            return None;
        }
        return Some(Work::Copy {
            path: path.to_path_buf(),
            identity,
        });
    }
    match stored.map(|stamp| &stamp.source) {
        Some(TextSource::Semantic { .. }) => None,
        Some(native) if *native == TextSource::current_native() => None,
        _ => Some(Work::Extract {
            path: path.to_path_buf(),
            identity,
        }),
    }
}

/// Bring the store in line with a full listing of the library.
pub fn reconcile_library(
    store: &DocumentTextIndex,
    semantic: Option<&SemanticHandle>,
    library: &LibraryDocuments,
    threads: usize,
    cancel: &AtomicBool,
) -> anyhow::Result<UpkeepReport> {
    let stored = store.stamps()?;
    let renditions = match semantic {
        Some(handle) => {
            let guard = handle.lock().unwrap_or_else(|p| p.into_inner());
            match guard.as_ref() {
                Some(index) => index.rendition_stamps()?,
                None => HashMap::new(),
            }
        }
        None => HashMap::new(),
    };

    let mut report = UpkeepReport::default();
    let mut listed = HashSet::with_capacity(library.paths.len());
    let mut work = Vec::new();
    for path in &library.paths {
        let key = DocumentTextIndex::key_for(path);
        listed.insert(key.clone());
        let Some(identity) = FileIdentity::for_path(path) else {
            continue;
        };
        if let Some(item) = plan_document(
            path,
            identity,
            stored.get(&key),
            renditions.get(&key).copied(),
        ) {
            work.push(item);
        }
    }

    let gone: Vec<PathBuf> = stored
        .keys()
        .filter(|key| !listed.contains(*key))
        .filter(|key| {
            !library
                .unlisted_roots
                .iter()
                .any(|root| key.starts_with(DocumentTextIndex::key_for(root)))
        })
        .cloned()
        .collect();
    report.removed = store.remove_keys(&gone)?;

    perform(store, semantic, work, threads, cancel, &mut report);
    Ok(report)
}

/// Apply one watcher batch.
pub fn apply_changes(
    store: &DocumentTextIndex,
    semantic: Option<&SemanticHandle>,
    changes: &ChangedDocuments,
    threads: usize,
    cancel: &AtomicBool,
) -> anyhow::Result<UpkeepReport> {
    let mut report = UpkeepReport::default();

    // Rows under a removed path, file or directory, that are really gone.
    // Held back until the changed files have had the chance to claim them as
    // the old side of a rename.
    let mut orphaned: HashMap<PathBuf, StoredStamp> = HashMap::new();
    if !changes.removed.is_empty() {
        let removed_roots: Vec<PathBuf> = changes
            .removed
            .iter()
            .map(|path| DocumentTextIndex::key_for(path))
            .collect();
        orphaned = store
            .stamps()?
            .into_iter()
            .filter(|(key, _)| removed_roots.iter().any(|root| key.starts_with(root)))
            .filter(|(key, _)| !key.exists())
            .collect();
    }

    let mut work = Vec::new();
    for path in &changes.changed {
        let Some(identity) = FileIdentity::for_path(path) else {
            continue;
        };
        let key = DocumentTextIndex::key_for(path);
        let mut stored = store.stamp(path)?;
        if stored.is_none() {
            // A rename: the same bytes under the same name, whose old path
            // this batch removed. Keep the reading instead of taking it again.
            let renamed_from = orphaned
                .iter()
                .find(|(old, stamp)| {
                    stamp.identity == identity && old.file_name() == key.file_name()
                })
                .map(|(old, _)| old.clone());
            if let Some(old) = renamed_from {
                if store.rename_key(&old, &key)? {
                    report.renamed += 1;
                }
                stored = orphaned.remove(&old);
            }
        }
        let rendition = rendition_stamp(semantic, path);
        if let Some(item) = plan_document(path, identity, stored.as_ref(), rendition) {
            work.push(item);
        }
    }

    let gone: Vec<PathBuf> = orphaned.into_keys().collect();
    report.removed = store.remove_keys(&gone)?;

    perform(store, semantic, work, threads, cancel, &mut report);
    Ok(report)
}

fn rendition_stamp(semantic: Option<&SemanticHandle>, path: &Path) -> Option<RenditionStamp> {
    let handle = semantic?;
    let guard = handle.lock().unwrap_or_else(|p| p.into_inner());
    match guard.as_ref()?.rendition_stamp(path) {
        Ok(stamp) => stamp,
        Err(e) => {
            error!(
                "[TextIndex] semantic rendition of {}: {e:#}",
                path.display()
            );
            None
        }
    }
}

/// Copies first — they read rows and are over in moments — then the glyph
/// readings, in parallel on a pool of `threads` that leaves the rest of the
/// machine to whatever the user is doing.
fn perform(
    store: &DocumentTextIndex,
    semantic: Option<&SemanticHandle>,
    work: Vec<Work>,
    threads: usize,
    cancel: &AtomicBool,
    report: &mut UpkeepReport,
) {
    let mut extract = Vec::new();
    for item in work {
        match item {
            Work::Copy { path, identity } => {
                if cancel.load(Ordering::Relaxed) {
                    report.cancelled = true;
                    return;
                }
                match copy_rendition(store, semantic, &path, identity) {
                    Ok(true) => report.copied += 1,
                    // The rendition went stale between planning and copying.
                    Ok(false) => extract.push((path, identity)),
                    Err(e) => error!("[TextIndex] copy {}: {e:#}", path.display()),
                }
            }
            Work::Extract { path, identity } => extract.push((path, identity)),
        }
    }
    if extract.is_empty() {
        return;
    }

    info!("[TextIndex] reading {} document(s)", extract.len());
    let registry = crate::extract::native_text_registry();
    let extracted = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let cancelled = AtomicBool::new(false);
    let run = || {
        extract.par_iter().for_each(|(path, identity)| {
            if cancel.load(Ordering::Relaxed) {
                cancelled.store(true, Ordering::Relaxed);
                return;
            }
            match read_native(store, &registry, path, *identity) {
                Ok(Reading::Stored) => {
                    extracted.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Reading::Failed) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Reading::Moved) => {}
                Err(e) => error!("[TextIndex] store {}: {e:#}", path.display()),
            }
        });
    };
    match rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .thread_name(|i| format!("text-index-{i}"))
        .build()
    {
        Ok(pool) => pool.install(run),
        Err(e) => {
            error!("[TextIndex] reading pool: {e:#}");
            return;
        }
    }
    report.extracted += extracted.into_inner();
    report.failed += failed.into_inner();
    report.cancelled |= cancelled.into_inner();
}

/// Copy the semantic index's current rendition of `path`. `Ok(false)` when it
/// no longer has one for these bytes.
fn copy_rendition(
    store: &DocumentTextIndex,
    semantic: Option<&SemanticHandle>,
    path: &Path,
    identity: FileIdentity,
) -> anyhow::Result<bool> {
    let Some(handle) = semantic else {
        return Ok(false);
    };
    // Stamp and text under one lock, so the stamp names the text copied.
    let copied = {
        let guard = handle.lock().unwrap_or_else(|p| p.into_inner());
        let Some(index) = guard.as_ref() else {
            return Ok(false);
        };
        let Some(stamp) = index.rendition_stamp(path)? else {
            return Ok(false);
        };
        if stamp.size_bytes != identity.size_bytes
            || stamp.modified_at_ms != identity.modified_at_ms
        {
            return Ok(false);
        }
        index
            .indexed_document_for_path(path)?
            .map(|(text, map)| (stamp, text, map))
    };
    let Some((stamp, text, map)) = copied else {
        return Ok(false);
    };
    store.write_text(
        path,
        identity,
        &TextSource::Semantic {
            indexed_at_ms: stamp.indexed_at_ms,
        },
        &text,
        &map,
    )?;
    Ok(true)
}

enum Reading {
    Stored,
    Failed,
    /// The file changed while it was being read; its own event will bring it
    /// back, and nothing was stored for bytes that no longer exist.
    Moved,
}

fn read_native(
    store: &DocumentTextIndex,
    registry: &ExtractorRegistry,
    path: &Path,
    identity: FileIdentity,
) -> anyhow::Result<Reading> {
    let Some(extractor) = registry.find(path, None) else {
        return Ok(Reading::Moved);
    };
    let result = extractor.extract(path);
    if FileIdentity::for_path(path) != Some(identity) {
        return Ok(Reading::Moved);
    }
    let source = TextSource::current_native();
    match result {
        Ok(content) => {
            store.write_text(path, identity, &source, &content.text, &content.source_map)?;
            Ok(Reading::Stored)
        }
        Err(e) => {
            info!("[TextIndex] could not read {}: {e:#}", path.display());
            store.write_failure(path, identity, &source, &format!("{e:#}"))?;
            Ok(Reading::Failed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::index::chunk::Chunk;
    use crate::embed::index::db::PreparedFile;
    use crate::types::{ByteRange, EmbeddingEngine, SourceOrigin};
    use tempfile::tempdir;

    /// A one-page PDF that typesets `text` in Helvetica.
    pub(crate) fn write_pdf(path: &Path, text: &str) {
        let stream = format!("BT\n/F1 12 Tf\n50 80 Td\n({text}) Tj\nET\n");
        let objects = [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 144] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n".to_string(),
            format!(
                "4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
                stream.len(),
                stream
            ),
            "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n"
                .to_string(),
        ];
        let mut pdf = "%PDF-1.4\n".to_string();
        let mut offsets = Vec::new();
        for object in &objects {
            offsets.push(pdf.len());
            pdf.push_str(object);
        }
        let xref_offset = pdf.len();
        pdf.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            offsets.len() + 1
        ));
        for offset in offsets {
            pdf.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        ));
        std::fs::write(path, pdf).unwrap();
    }

    fn library(paths: &[&Path]) -> LibraryDocuments {
        LibraryDocuments {
            paths: paths.iter().map(|p| p.to_path_buf()).collect(),
            unlisted_roots: Vec::new(),
        }
    }

    fn semantic_with(root: &Path, path: &Path, text: &str) -> SemanticHandle {
        let mut index = SemanticIndex::create(
            &root.join("semantic"),
            "m",
            3,
            EmbeddingEngine::Candle,
            Some(root),
        )
        .unwrap();
        index
            .write_file(PreparedFile {
                retained: Default::default(),
                path: path.to_path_buf(),
                full_text: text.to_string(),
                chunks: vec![(
                    Chunk {
                        file_path: path.to_path_buf(),
                        text: text.to_string(),
                        byte_range: ByteRange {
                            start: 0,
                            end: text.len(),
                        },
                        origin: SourceOrigin::PdfPage {
                            page: 1,
                            bbox: None,
                        },
                    },
                    vec![1.0, 0.0, 0.0],
                )],
            })
            .unwrap();
        Arc::new(Mutex::new(Some(index)))
    }

    #[test]
    fn an_unembedded_library_is_read_once_and_then_left_alone() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(&dir.path().join("data")).unwrap();
        let pdf = dir.path().join("book.pdf");
        write_pdf(&pdf, "Hello Library");
        let cancel = AtomicBool::new(false);

        let first = reconcile_library(&store, None, &library(&[&pdf]), 2, &cancel).unwrap();
        assert_eq!(first.extracted, 1);
        let stored = store.document_for_path(&pdf).unwrap().unwrap();
        assert!(stored.text.contains("Hello Library"), "{:?}", stored.text);
        assert!(!stored.source_map.segments.is_empty());

        let second = reconcile_library(&store, None, &library(&[&pdf]), 2, &cancel).unwrap();
        assert_eq!(second, UpkeepReport::default());
    }

    /// The point of the store: what it serves is what a live read produces,
    /// text and word boxes alike.
    #[test]
    fn a_stored_native_reading_equals_the_live_one() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(dir.path()).unwrap();
        let pdf = dir.path().join("book.pdf");
        write_pdf(&pdf, "Exactly the same words");
        reconcile_library(&store, None, &library(&[&pdf]), 1, &AtomicBool::new(false)).unwrap();

        let live = crate::extract::native_text_registry()
            .find(&pdf, None)
            .unwrap()
            .extract(&pdf)
            .unwrap();
        let stored = store.document_for_path(&pdf).unwrap().unwrap();
        assert_eq!(stored.text, live.text);
        assert_eq!(
            stored.source_map.segments.len(),
            live.source_map.segments.len()
        );
        for (got, want) in stored
            .source_map
            .segments
            .iter()
            .zip(&live.source_map.segments)
        {
            assert_eq!(got.text_range, want.text_range);
            assert_eq!(got.origin, want.origin);
            assert_eq!(got.provenance, want.provenance);
        }
    }

    #[test]
    fn an_edited_document_is_read_again() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(dir.path()).unwrap();
        let pdf = dir.path().join("book.pdf");
        write_pdf(&pdf, "First edition");
        let cancel = AtomicBool::new(false);
        reconcile_library(&store, None, &library(&[&pdf]), 1, &cancel).unwrap();

        write_pdf(&pdf, "Second edition, revised");
        let report = reconcile_library(&store, None, &library(&[&pdf]), 1, &cancel).unwrap();
        assert_eq!(report.extracted, 1);
        let stored = store.document_for_path(&pdf).unwrap().unwrap();
        assert!(stored.text.contains("revised"), "{:?}", stored.text);
    }

    #[test]
    fn a_semantic_rendition_is_copied_instead_of_read() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(&dir.path().join("data")).unwrap();
        // Not a readable PDF: a glyph reading would fail, so a stored text
        // proves it came from the semantic index.
        let pdf = dir.path().join("scan.pdf");
        std::fs::write(&pdf, "%PDF-1.7 opaque").unwrap();
        let semantic = semantic_with(dir.path(), &pdf, "text recognized in the scan");
        let cancel = AtomicBool::new(false);

        let report =
            reconcile_library(&store, Some(&semantic), &library(&[&pdf]), 1, &cancel).unwrap();
        assert_eq!(report.copied, 1);
        assert_eq!(report.extracted, 0);
        let stored = store.document_for_path(&pdf).unwrap().unwrap();
        assert_eq!(stored.text, "text recognized in the scan");

        // Nothing to do while neither side changes.
        let again =
            reconcile_library(&store, Some(&semantic), &library(&[&pdf]), 1, &cancel).unwrap();
        assert_eq!(again, UpkeepReport::default());

        // A copied rendition outlives the semantic index that produced it:
        // it is still a reading of these bytes.
        *semantic.lock().unwrap() = None;
        let without =
            reconcile_library(&store, Some(&semantic), &library(&[&pdf]), 1, &cancel).unwrap();
        assert_eq!(without, UpkeepReport::default());
        assert!(store.document_for_path(&pdf).unwrap().is_some());
    }

    #[test]
    fn a_native_reading_is_upgraded_when_a_rendition_appears() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(&dir.path().join("data")).unwrap();
        let pdf = dir.path().join("book.pdf");
        write_pdf(&pdf, "Glyphs only");
        let cancel = AtomicBool::new(false);
        reconcile_library(&store, None, &library(&[&pdf]), 1, &cancel).unwrap();

        let semantic = semantic_with(dir.path(), &pdf, "Glyphs only plus a figure's text");
        let report =
            reconcile_library(&store, Some(&semantic), &library(&[&pdf]), 1, &cancel).unwrap();
        assert_eq!(report.copied, 1);
        assert_eq!(
            store.document_for_path(&pdf).unwrap().unwrap().text,
            "Glyphs only plus a figure's text"
        );
    }

    #[test]
    fn an_unreadable_document_is_recorded_and_not_retried() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(&dir.path().join("data")).unwrap();
        let pdf = dir.path().join("broken.pdf");
        std::fs::write(&pdf, "%PDF-1.7 not really").unwrap();
        let cancel = AtomicBool::new(false);

        let first = reconcile_library(&store, None, &library(&[&pdf]), 1, &cancel).unwrap();
        assert_eq!(first.failed, 1);
        assert!(store.document_for_path(&pdf).unwrap().is_none());

        let second = reconcile_library(&store, None, &library(&[&pdf]), 1, &cancel).unwrap();
        assert_eq!(second, UpkeepReport::default());
    }

    #[test]
    fn documents_that_left_the_library_are_dropped_unless_their_root_was_unreadable() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(&dir.path().join("data")).unwrap();
        let kept_root = dir.path().join("offline");
        std::fs::create_dir_all(&kept_root).unwrap();
        let kept = kept_root.join("kept.pdf");
        let dropped = dir.path().join("dropped.pdf");
        write_pdf(&kept, "Kept");
        write_pdf(&dropped, "Dropped");
        let cancel = AtomicBool::new(false);
        reconcile_library(&store, None, &library(&[&kept, &dropped]), 1, &cancel).unwrap();

        let report = reconcile_library(
            &store,
            None,
            &LibraryDocuments {
                paths: Vec::new(),
                unlisted_roots: vec![kept_root],
            },
            1,
            &cancel,
        )
        .unwrap();
        assert_eq!(report.removed, 1);
        assert!(store.document_for_path(&kept).unwrap().is_some());
        assert!(store.document_for_path(&dropped).unwrap().is_none());
    }

    #[test]
    fn a_cancelled_pass_reads_nothing_more() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(&dir.path().join("data")).unwrap();
        let pdf = dir.path().join("book.pdf");
        write_pdf(&pdf, "Never read");

        let report =
            reconcile_library(&store, None, &library(&[&pdf]), 1, &AtomicBool::new(true)).unwrap();
        assert!(report.cancelled);
        assert_eq!(report.extracted, 0);
        assert!(store.stamps().unwrap().is_empty());
    }

    #[test]
    fn a_watched_rename_keeps_the_reading() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(&dir.path().join("data")).unwrap();
        let old = dir.path().join("old.pdf");
        write_pdf(&old, "Renamed book");
        let cancel = AtomicBool::new(false);
        reconcile_library(&store, None, &library(&[&old]), 1, &cancel).unwrap();

        let new = dir.path().join("shelf").join("old.pdf");
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::rename(&old, &new).unwrap();
        let report = apply_changes(
            &store,
            None,
            &ChangedDocuments {
                changed: vec![new.clone()],
                removed: vec![old.clone()],
            },
            1,
            &cancel,
        )
        .unwrap();

        assert_eq!(report.renamed, 1);
        assert_eq!(report.extracted, 0);
        assert_eq!(report.removed, 0);
        assert!(store
            .document_for_path(&new)
            .unwrap()
            .unwrap()
            .text
            .contains("Renamed book"));
        assert_eq!(store.stamps().unwrap().len(), 1);
    }

    #[test]
    fn a_watched_directory_removal_drops_its_documents() {
        let dir = tempdir().unwrap();
        let store = DocumentTextIndex::open(&dir.path().join("data")).unwrap();
        let shelf = dir.path().join("shelf");
        std::fs::create_dir_all(&shelf).unwrap();
        let a = shelf.join("a.pdf");
        let b = shelf.join("b.pdf");
        write_pdf(&a, "A");
        write_pdf(&b, "B");
        let cancel = AtomicBool::new(false);
        reconcile_library(&store, None, &library(&[&a, &b]), 1, &cancel).unwrap();

        // Keys were taken while the shelf existed.
        let shelf_key = DocumentTextIndex::key_for(&shelf);
        std::fs::remove_dir_all(&shelf).unwrap();
        let report = apply_changes(
            &store,
            None,
            &ChangedDocuments {
                changed: Vec::new(),
                removed: vec![shelf_key],
            },
            1,
            &cancel,
        )
        .unwrap();
        assert_eq!(report.removed, 2);
        assert!(store.stamps().unwrap().is_empty());
    }

    #[test]
    fn a_moved_in_folder_is_expanded_into_its_documents() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let shelf = root.join("shelf");
        std::fs::create_dir_all(shelf.join("nested")).unwrap();
        let book = shelf.join("nested").join("book.epub");
        std::fs::write(&book, "not really an epub").unwrap();
        std::fs::write(shelf.join("notes.txt"), "plain text is grep's own").unwrap();

        let changes = ChangedDocuments::from_batch(
            &DirectoryChangeBatch {
                root: root.clone(),
                changed: Vec::new(),
                removed: Vec::new(),
                appeared_dirs: vec![root.clone(), shelf.clone()],
            },
            &["txt".to_string()],
            0,
        );
        assert_eq!(changes.changed, vec![book]);
    }
}
