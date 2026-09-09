//! A book, rendered to a PDF the reader can draw.
//!
//! **Why this exists.** The reader renders pages through pdf.js, out of the
//! file's own bytes, and pdf.js opens exactly one of the formats this backend
//! reads. An EPUB, a MOBI or an FB2 has no pages in its bytes at all — MuPDF
//! invents them, in this process, from the geometry in [`super::format`]. So a
//! book is laid out once and written out as a PDF holding those same pages,
//! and the reader draws that.
//!
//! **Why it is coherent with the index.** The surrogate is written from
//! [`super::mupdf::open_document`], which is the same guarded, pinned open
//! extraction uses. Both therefore descend from one geometry constant, so page
//! 37 of the reading is page 37 of the surrogate and a bounding box the index
//! stored lands on the words it was taken from. Measured over 1,738 pages of
//! EPUB, MOBI and FB2: page bounds identical on every one, and the extracted
//! text identical on all but two, where one missing-glyph box and one heading's
//! line breaks differ.
//!
//! **What does not survive.** A document writer is a device pipeline: it
//! replays each page's drawing operations and nothing else. Links and the
//! outline are not drawing operations, so the surrogate has neither, and they
//! are carried beside it — read here from the same open document, and handed
//! to the reader as facts rather than as something it could find in the file.
//!
//! Rendering runs in the host. That is deliberate and within the worker
//! invariant, which places mupdf rendering and image decode in the host by
//! design; what may not run here is inference, and none of this is.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use mupdf::{DestinationKind, DocumentWriter, Matrix};
use tracing::{debug, warn};

use crate::types::{BoundingBox, SurrogateLink, SurrogateOutline, SurrogatePageLinks};

/// The write options the surrogate is produced under.
///
/// Not cosmetic. MuPDF's default write options compress nothing and collect no
/// garbage, which for a 198-page book produced a 365 MB file where these
/// produce 4 MB, and took 8.2 seconds where these take 0.17.
const WRITE_OPTIONS: &str = "compress,compress-fonts,compress-images,garbage=4,sanitize";

/// How much rendered book is held in memory.
///
/// A budget rather than a count, because the sizes are not comparable: a
/// 442-page novel renders to 6 MB and a 196-page comic to 48 MB, so "two
/// documents" is either 12 MB or 96 MB depending on what the user opened. The
/// most recent rendering is always kept even when it exceeds the budget on its
/// own — evicting the document being read to satisfy a cache would be the one
/// thing a cache must not do.
const CACHE_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// A book as the reader receives it: pages it can draw, and the two things a
/// device-stream conversion could not carry.
#[derive(Clone)]
pub struct Surrogate {
    /// A PDF holding the laid-out pages. `Arc` because it crosses to the
    /// interface whole and there is no reason to copy megabytes per request.
    pub bytes: Arc<Vec<u8>>,
    pub outline: Vec<SurrogateOutline>,
    pub links: Vec<SurrogatePageLinks>,
}

/// A cached rendering is keyed on the file's identity, never on its path
/// alone: a book edited or replaced in place must not be served as the copy
/// rendered before it changed.
#[derive(Clone, PartialEq, Eq)]
struct Key {
    path: PathBuf,
    modified_ms: i64,
    size_bytes: u64,
}

impl Key {
    fn of(path: &Path) -> anyhow::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|since| i64::try_from(since.as_millis()).ok())
            .unwrap_or(0);
        Ok(Self {
            path: path.to_path_buf(),
            modified_ms,
            size_bytes: metadata.len(),
        })
    }
}

/// Most-recently-used last, as the reader's own document cache is ordered.
static CACHE: Mutex<Vec<(Key, Surrogate)>> = Mutex::new(Vec::new());

/// The rendering of this book, from the cache when it is there.
///
/// Refuses a PDF. A PDF's pages are in its own bytes and the reader fetches
/// them from the file, so a surrogate would be a second copy of a document
/// that already has one — and re-encoding it can simply fail, as it does for a
/// PDF carrying JBIG2 images the writer cannot reproduce. The caller that
/// mistakenly asked would then see a rendering error for a document that opens
/// perfectly well.
pub fn surrogate(path: &Path) -> anyhow::Result<Surrogate> {
    anyhow::ensure!(
        super::format::PagedFormat::for_path(path) != Some(super::format::PagedFormat::Pdf),
        "{} is a PDF; its pages are its own and the reader reads them from the file",
        path.display()
    );
    let key = Key::of(path)?;
    {
        let mut cache = lock();
        if let Some(position) = cache.iter().position(|(cached, _)| *cached == key) {
            let entry = cache.remove(position);
            let found = entry.1.clone();
            cache.push(entry);
            debug!("surrogate: serving {} from cache", path.display());
            return Ok(found);
        }
    }

    let rendered = render(path)?;

    let mut cache = lock();
    // Another caller may have rendered the same book while this one was
    // working. Keeping the newer entry rather than both is what bounds this.
    cache.retain(|(cached, _)| *cached != key);
    cache.push((key, rendered.clone()));
    // Oldest first, and never the one just rendered.
    while cache.len() > 1
        && cache
            .iter()
            .map(|(_, held)| held.bytes.len())
            .sum::<usize>()
            > CACHE_BUDGET_BYTES
    {
        cache.remove(0);
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PDF already has pages; asking for a rendering of one is a caller
    /// mistake, and it must say so rather than spend a document's worth of
    /// work — or fail re-encoding artwork, which is how this was found.
    #[test]
    fn a_pdf_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("paper.pdf");
        std::fs::write(&path, b"%PDF-1.4").unwrap();
        let error = match surrogate(&path) {
            Ok(_) => panic!("a PDF must not be rendered to a surrogate"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("is a PDF"), "{error}");
    }

    /// The budget evicts, but never the rendering just produced: a comic alone
    /// exceeds it, and dropping it would mean re-rendering the document the
    /// user is reading on every request for its pages.
    #[test]
    fn the_cache_keeps_the_newest_rendering_even_when_it_alone_exceeds_the_budget() {
        let mut cache: Vec<(Key, Surrogate)> = Vec::new();
        let entry = |name: &str, size: usize| {
            (
                Key {
                    path: PathBuf::from(name),
                    modified_ms: 0,
                    size_bytes: 0,
                },
                Surrogate {
                    bytes: Arc::new(vec![0u8; size]),
                    outline: Vec::new(),
                    links: Vec::new(),
                },
            )
        };
        cache.push(entry("novel.epub", 6 * 1024 * 1024));
        cache.push(entry("comic.cbz", CACHE_BUDGET_BYTES + 1));
        while cache.len() > 1
            && cache
                .iter()
                .map(|(_, held)| held.bytes.len())
                .sum::<usize>()
                > CACHE_BUDGET_BYTES
        {
            cache.remove(0);
        }
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].0.path, PathBuf::from("comic.cbz"));
    }

    /// And a file this backend does not read at all is refused before anything
    /// is opened.
    #[test]
    fn an_unadmitted_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"hello").unwrap();
        assert!(matches!(surrogate(&path), Err(_)));
    }
}

fn lock() -> std::sync::MutexGuard<'static, Vec<(Key, Surrogate)>> {
    CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn render(path: &Path) -> anyhow::Result<Surrogate> {
    let started = Instant::now();
    let doc = super::mupdf::open_document(path)?;
    let page_count = doc.page_count()?;

    // MuPDF's `convert_to_pdf` is not usable here: for a reflowable source it
    // puts the whole document on every page, which produced a 9.5 GB file for
    // a 442-page book and coordinates in the millions. Replaying each page
    // onto a writer's device — what `mutool convert` does — is correct and two
    // orders of magnitude smaller.
    let directory = tempfile::tempdir()?;
    let out = directory.path().join("surrogate.pdf");
    let out_str = out
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 temporary path"))?;
    {
        let mut writer = DocumentWriter::new(out_str, "pdf", WRITE_OPTIONS)?;
        for index in 0..page_count {
            let page = doc.load_page(index)?;
            let device = writer.begin_page(page.bounds()?)?;
            page.run(&device, &Matrix::IDENTITY)?;
            writer.end_page(device)?;
        }
    }
    let bytes = std::fs::read(&out)?;

    let outline = match doc.outlines() {
        Ok(entries) => convert_outline(&entries),
        Err(error) => {
            // A book without a readable outline is a book without a table of
            // contents affordance, which is a loss and not a failure: the
            // pages are still right. Never silently, though.
            warn!(
                "the outline of {} could not be read; it will open without a table of contents: {error}",
                path.display()
            );
            Vec::new()
        }
    };

    let mut links = Vec::new();
    for index in 0..page_count {
        let page = doc.load_page(index)?;
        let on_page: Vec<SurrogateLink> = page
            .links()?
            .map(|link| SurrogateLink {
                bbox: BoundingBox {
                    x: link.bounds.x0,
                    y: link.bounds.y0,
                    width: (link.bounds.x1 - link.bounds.x0).max(0.0),
                    height: (link.bounds.y1 - link.bounds.y0).max(0.0),
                },
                // MuPDF resolves an internal target itself and leaves `dest`
                // empty for an external one, so which kind a link is comes
                // from the document rather than from parsing its URI here.
                page: link.dest.as_ref().map(|dest| dest.loc.page_number + 1),
                offset_y: link.dest.as_ref().and_then(|dest| anchor_top(dest.kind)),
                url: match link.dest {
                    Some(_) => None,
                    None => Some(link.uri.clone()),
                },
            })
            .collect();
        if !on_page.is_empty() {
            links.push(SurrogatePageLinks {
                page: index as u32 + 1,
                links: on_page,
            });
        }
    }

    debug!(
        "surrogate: rendered {} ({page_count} pages, {} bytes, {} outline entries) in {:?}",
        path.display(),
        bytes.len(),
        outline.len(),
        started.elapsed()
    );
    Ok(Surrogate {
        bytes: Arc::new(bytes),
        outline,
        links,
    })
}

/// Where on the target page a destination points, when it says.
///
/// `Fit` and the widths pin no vertical position, and answering 0 for them
/// would be inventing a top-of-page anchor the document did not ask for.
fn anchor_top(kind: DestinationKind) -> Option<f32> {
    match kind {
        DestinationKind::XYZ { top, .. } => top,
        DestinationKind::FitH { top } | DestinationKind::FitBH { top } => Some(top),
        DestinationKind::FitR { top, .. } => Some(top),
        _ => None,
    }
}

fn convert_outline(entries: &[mupdf::Outline]) -> Vec<SurrogateOutline> {
    entries
        .iter()
        .map(|entry| SurrogateOutline {
            title: entry.title.clone(),
            page: entry.dest.as_ref().map(|dest| dest.loc.page_number + 1),
            offset_y: entry.dest.as_ref().and_then(|dest| anchor_top(dest.kind)),
            // As with links: a resolved destination means an internal target,
            // and only an unresolved one leaves the URI standing for itself.
            url: match entry.dest {
                Some(_) => None,
                None => entry.uri.clone(),
            },
            items: convert_outline(&entry.down),
        })
        .collect()
}
