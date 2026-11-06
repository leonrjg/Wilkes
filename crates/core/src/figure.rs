//! Serving a document's own pictures: which passage a figure belongs to, and
//! the pixels themselves.
//!
//! Two things live here and they are deliberately separate. The **link** is
//! arithmetic over the reading — a chunk's byte range against an image's
//! anchor — and decides nothing about what a figure is *called*. The **render**
//! re-derives pixels from the source the rendition was extracted from and
//! checks them against the digest that rendition recorded.
//!
//! Nothing here matches a caption, normalizes a label, or resolves "Figure
//! 3.2". A consumer that hands a model the passage and the pictures drawn in it
//! has given the model the caption too — it is page text, a few hundred bytes
//! from the anchor — and the model reads it better than a `Fig.` heuristic
//! would. The extraction module's rule that nothing decides what a figure is
//! survives intact on both sides of this boundary.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest, Sha256};
use tracing::warn;

use crate::extract::document::mupdf;
use crate::extract::document::surrogate::FileIdentity;
use crate::extract::image::{digest_pixels, NativeImage};
use crate::types::{ByteRange, RegionOrigin, RetainedImage};

/// How a figure bears on a passage. Ordered strongest first, which is the
/// order a caller with a budget should spend it in.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum FigureRelation {
    /// The passage *is* this figure's enrichment text: `Image description: …`.
    /// The model already has the words, and this is where it should be handed
    /// the picture instead of the paraphrase.
    Block,
    /// The picture sits inside this passage — the anchor is within the chunk.
    /// Holds for every figure, analyzed or not.
    DrawnIn,
    /// The picture is outside the passage but within the window the caller
    /// asked for, which reaches a figure printed a page over from the prose
    /// that discusses it. Ranked by distance and never a default.
    Near,
}

/// One figure's bearing on one passage.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FigureLink {
    pub relation: FigureRelation,
    /// Bytes between the anchor and the passage; zero whenever the anchor is
    /// inside it. What ranks a [`FigureRelation::Near`] hit.
    pub byte_distance: usize,
}

/// Whether this figure bears on this passage, and how.
///
/// `window` is how far outside the chunk an anchor may fall and still count,
/// in bytes of the reading. Zero admits only figures the passage contains,
/// which is the default the pipeline ships with: a window is a widening the
/// caller opts into, and a wrong extra figure spends budget that the nearest
/// one had first claim on.
///
/// A zero-width anchor on a chunk boundary belongs to the passages on either
/// side of it, so both endpoints count. That is not a tie-break: the chunker
/// gives an enrichment block its own structural run, so an analyzed image's
/// anchor sits exactly at the seam between the prose it interrupts and its own
/// block, and both of those are true statements about it.
pub fn link(image: &RetainedImage, chunk: &ByteRange, window: usize) -> Option<FigureLink> {
    // The passage is this image's own enrichment text. Tested first because a
    // chunk inside the block is also within reach of the anchor at its edge,
    // and "this is the figure's text" is the stronger thing to say.
    if let Some(range) = image.reading_range.as_ref() {
        if chunk.start >= range.start && chunk.end <= range.end {
            return Some(FigureLink {
                relation: FigureRelation::Block,
                byte_distance: 0,
            });
        }
    }

    // Everything below needs a position. A rendition extracted before anchors
    // existed has none, and reports nothing rather than guessing from a page.
    let anchor = image.reading_anchor?;
    if anchor >= chunk.start && anchor <= chunk.end {
        return Some(FigureLink {
            relation: FigureRelation::DrawnIn,
            byte_distance: 0,
        });
    }

    let distance = if anchor < chunk.start {
        chunk.start - anchor
    } else {
        anchor - chunk.end
    };
    (distance <= window && window > 0).then_some(FigureLink {
        relation: FigureRelation::Near,
        byte_distance: distance,
    })
}

/// One figure, encoded.
///
/// `Debug` prints the bytes' length rather than the bytes: a failed assertion
/// in a test should not be a megabyte of PNG.
pub struct FigurePixels {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// What the source decoded to before any downscale — the dimensions the
    /// digest is over.
    pub source_width: u32,
    pub source_height: u32,
}

impl std::fmt::Debug for FigurePixels {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FigurePixels")
            .field("png_bytes", &self.png.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .field("source_width", &self.source_width)
            .field("source_height", &self.source_height)
            .finish()
    }
}

/// One document's cover, encoded.
///
/// `Debug` prints the bytes' length for the reason [`FigurePixels`] does.
pub struct CoverPixels {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl std::fmt::Debug for CoverPixels {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoverPixels")
            .field("png_bytes", &self.png.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

/// How many bytes of covers one installation keeps.
///
/// A budget because a changed file leaves its old covers behind under a key
/// nothing asks for again, so without one the directory only grows. Thousands
/// of list pictures and a few dozen enlarged ones fit, and what falls out costs
/// one render the next time it is asked for.
const COVER_CACHE_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// Covers written between sweeps of the directory. The first write of a process
/// sweeps too, which catches up with whatever an earlier run left.
const COVER_SWEEP_INTERVAL: usize = 64;

static COVERS_WRITTEN: AtomicUsize = AtomicUsize::new(0);

/// A document's cover as PNG bytes, drawn once for each size it is asked for.
///
/// Keyed on the file's identity, as a surrogate is, so a document replaced in
/// place is drawn again and nothing has to be told that it changed.
pub fn cached_cover(cache_dir: &Path, source: &Path, max_edge: u32) -> anyhow::Result<Vec<u8>> {
    let entry = cache_dir.join(cover_file_name(source, max_edge)?);
    match std::fs::read(&entry) {
        Ok(png) => return Ok(png),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(anyhow::anyhow!(
                "reading cached cover {} failed: {error}",
                entry.display()
            ))
        }
    }

    let cover = render_cover(source, max_edge)?;

    // Through a temporary file, because two lists can ask for one cover at once
    // and a reader must never find half of one under the final name.
    std::fs::create_dir_all(cache_dir)?;
    let mut temporary = tempfile::NamedTempFile::new_in(cache_dir)?;
    temporary.write_all(&cover.png)?;
    temporary.persist(&entry)?;

    if COVERS_WRITTEN
        .fetch_add(1, Ordering::Relaxed)
        .is_multiple_of(COVER_SWEEP_INTERVAL)
    {
        if let Err(error) = sweep_covers(cache_dir, COVER_CACHE_BUDGET_BYTES) {
            warn!(
                "could not sweep the cover cache {}: {error}",
                cache_dir.display()
            );
        }
    }
    Ok(cover.png)
}

/// The width and height a PNG declares, read from its header alone.
///
/// For a caller that holds a cover from [`cached_cover`] and has to say how
/// big it is: the cache keeps the encoded bytes and nothing beside them, and
/// the header is the one place the size is written down.
pub fn png_dimensions(png: &[u8]) -> anyhow::Result<(u32, u32)> {
    image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png)
        .into_dimensions()
        .map_err(|error| anyhow::anyhow!("reading the PNG header failed: {error}"))
}

fn cover_file_name(source: &Path, max_edge: u32) -> anyhow::Result<String> {
    let identity = FileIdentity::of(source)?;
    let mut hasher = Sha256::new();
    hasher.update(identity.path.as_os_str().as_encoded_bytes());
    hasher.update(identity.modified_ms.to_le_bytes());
    hasher.update(identity.size_bytes.to_le_bytes());
    hasher.update(max_edge.to_le_bytes());
    Ok(format!("{:x}.png", hasher.finalize()))
}

/// Bring the cache under `budget`, oldest writes first.
///
/// Oldest written rather than least recently read: telling them apart would
/// mean a write for every cover a list shows, to save the one render a wrongly
/// evicted cover costs.
fn sweep_covers(cache_dir: &Path, budget: u64) -> std::io::Result<()> {
    let mut held = Vec::new();
    for entry in std::fs::read_dir(cache_dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        held.push((metadata.modified()?, metadata.len(), entry.path()));
    }
    let mut total: u64 = held.iter().map(|(_, length, _)| *length).sum();
    held.sort();
    for (_, length, path) in held {
        if total <= budget {
            break;
        }
        std::fs::remove_file(path)?;
        total -= length;
    }
    Ok(())
}

/// The pixels of one retained figure, from the document it was extracted from.
///
/// Re-derived rather than stored: the pixels are already in the PDF, and the
/// index keeping a second copy of every figure would grow it by the size of
/// the library's artwork. What the index keeps is the digest, and this is
/// where that earns its place — a picture that does not hash to what the
/// rendition recorded is refused, because a mismatch means the source and the
/// rendition have come apart and the wrong picture is worse than none.
pub fn render_figure(
    source: &Path,
    image: &RetainedImage,
    max_edge: Option<u32>,
) -> anyhow::Result<FigurePixels> {
    anyhow::ensure!(
        source.exists(),
        "the source this rendition was extracted from is not where it was left: {}",
        source.display()
    );

    let decoded: NativeImage = match image.origin {
        RegionOrigin::Embedded => {
            mupdf::decode_embedded_image(source, &image.id)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "{} draws no image {} — this is not the document the rendition was \
                     extracted from",
                    source.display(),
                    image.id
                )
            })?
        }
        // Not a block to be found: a typeset area is the page's own drawing,
        // and the bbox recorded at extraction is the whole address.
        RegionOrigin::Typeset => mupdf::render_page_area(source, image.page, &image.bbox)?,
    };

    if !image.image_sha256.is_empty() {
        let digest = digest_pixels(&decoded.pixels);
        anyhow::ensure!(
            digest == image.image_sha256,
            "image {} re-derives to {digest}, but the rendition recorded {}. The source and \
             the rendition have come apart; serving these pixels would be serving a different \
             picture than the one that was read",
            image.id,
            image.image_sha256
        );
    }

    let source_width = decoded.pixels.width();
    let source_height = decoded.pixels.height();

    // Downscaled after verification, never before: the digest is over what the
    // page draws, and a resampled copy hashes to nothing in particular.
    let pixels = match max_edge {
        Some(edge) if edge > 0 && source_width.max(source_height) > edge => {
            let scale = f64::from(edge) / f64::from(source_width.max(source_height));
            let width = ((f64::from(source_width) * scale).round() as u32).max(1);
            let height = ((f64::from(source_height) * scale).round() as u32).max(1);
            image::imageops::resize(
                &decoded.pixels,
                width,
                height,
                image::imageops::FilterType::Lanczos3,
            )
        }
        _ => decoded.pixels,
    };

    let png = encode_png(&pixels).map_err(|e| anyhow::anyhow!("encoding {}: {e:#}", image.id))?;

    Ok(FigurePixels {
        width: pixels.width(),
        height: pixels.height(),
        source_width,
        source_height,
        png,
    })
}

/// A picture of a whole document: its first page, fitted inside `max_edge`.
///
/// Nothing decides whether that page is a cover. It is what the document opens
/// on, which is the same thing for a book that has one, and the first thing a
/// reader would see for one that does not.
pub fn render_cover(source: &Path, max_edge: u32) -> anyhow::Result<CoverPixels> {
    let pixels = mupdf::render_cover(source, max_edge)?.pixels;
    Ok(CoverPixels {
        png: encode_png(&pixels)?,
        width: pixels.width(),
        height: pixels.height(),
    })
}

fn encode_png(pixels: &image::RgbImage) -> anyhow::Result<Vec<u8>> {
    let mut png = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut png),
        pixels.as_raw(),
        pixels.width(),
        pixels.height(),
        image::ExtendedColorType::Rgb8,
    )
    .map_err(|e| anyhow::anyhow!("encoding as PNG failed: {e}"))?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BoundingBox, ImageAnalysisStatus, ImageTransform};

    const COMIC: &[u8] = include_bytes!("../examples/fixtures/comic-reader.cbz");

    fn write_comic(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("comic.cbz");
        std::fs::write(&path, COMIC).unwrap();
        path
    }

    fn entries(dir: &Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect()
    }

    #[test]
    fn a_cover_is_drawn_once_and_then_read_back() {
        let library = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let path = write_comic(library.path());

        let drawn = cached_cover(cache.path(), &path, 64).unwrap();
        assert_eq!(&drawn[1..4], b"PNG");
        let stored = entries(cache.path());
        assert_eq!(stored.len(), 1);

        // Not a PNG, so only the cache can have answered with it.
        std::fs::write(&stored[0], b"from the cache").unwrap();
        assert_eq!(
            cached_cover(cache.path(), &path, 64).unwrap(),
            b"from the cache"
        );
    }

    #[test]
    fn each_size_of_a_cover_is_its_own_entry() {
        let library = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let path = write_comic(library.path());

        cached_cover(cache.path(), &path, 64).unwrap();
        cached_cover(cache.path(), &path, 128).unwrap();

        assert_eq!(entries(cache.path()).len(), 2);
    }

    #[test]
    fn a_document_replaced_in_place_is_drawn_again() {
        let library = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let path = write_comic(library.path());

        cached_cover(cache.path(), &path, 64).unwrap();
        let stored = entries(cache.path());
        std::fs::write(&stored[0], b"the old cover").unwrap();

        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();

        assert_ne!(
            cached_cover(cache.path(), &path, 64).unwrap(),
            b"the old cover"
        );
    }

    #[test]
    fn a_document_that_cannot_be_drawn_leaves_nothing_in_the_cache() {
        let library = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let path = library.path().join("broken.pdf");
        std::fs::write(&path, "not a pdf").unwrap();

        assert!(cached_cover(cache.path(), &path, 64).is_err());
        assert!(entries(cache.path()).is_empty());
    }

    #[test]
    fn a_sweep_removes_the_oldest_covers_until_the_cache_is_within_budget() {
        let cache = tempfile::tempdir().unwrap();
        for (name, age_seconds) in [("old", 300), ("middle", 200), ("new", 100)] {
            let path = cache.path().join(name);
            std::fs::write(&path, [0u8; 10]).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(age_seconds),
                )
                .unwrap();
        }

        sweep_covers(cache.path(), 20).unwrap();

        assert!(!cache.path().join("old").exists());
        assert!(cache.path().join("middle").exists());
        assert!(cache.path().join("new").exists());
    }

    fn figure(anchor: Option<usize>, range: Option<(usize, usize)>) -> RetainedImage {
        RetainedImage {
            id: "p1-i0".to_string(),
            page: 1,
            origin: RegionOrigin::Embedded,
            bbox: BoundingBox {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            transform: ImageTransform {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                e: 0.0,
                f: 0.0,
            },
            pixel_width: 10,
            pixel_height: 10,
            image_sha256: "digest".to_string(),
            reading_range: range.map(|(start, end)| ByteRange { start, end }),
            reading_anchor: anchor,
            status: ImageAnalysisStatus::Complete,
            has_description: false,
        }
    }

    fn chunk(start: usize, end: usize) -> ByteRange {
        ByteRange { start, end }
    }

    /// The relation that covers every figure: a picture nothing was
    /// established about is placed by its anchor like any other.
    #[test]
    fn a_figure_anchored_inside_a_passage_is_drawn_in_it() {
        let undescribed = figure(Some(150), None);
        assert_eq!(
            link(&undescribed, &chunk(100, 200), 0),
            Some(FigureLink {
                relation: FigureRelation::DrawnIn,
                byte_distance: 0,
            })
        );
    }

    /// The passage that *is* the enrichment text says so, rather than
    /// reporting the weaker relation it also satisfies.
    #[test]
    fn a_passage_inside_the_enrichment_block_is_that_figures_text() {
        let described = figure(Some(100), Some((101, 400)));
        assert_eq!(
            link(&described, &chunk(120, 300), 0).map(|l| l.relation),
            Some(FigureRelation::Block)
        );
    }

    /// The chunker gives an enrichment block its own structural run, so an
    /// analyzed image's anchor lands on the seam. Both sides are true.
    #[test]
    fn an_anchor_on_a_boundary_belongs_to_the_passages_on_both_sides() {
        let described = figure(Some(100), Some((101, 400)));
        assert_eq!(
            link(&described, &chunk(0, 100), 0).map(|l| l.relation),
            Some(FigureRelation::DrawnIn),
            "the prose ending where the picture sits"
        );
        assert_eq!(
            link(&described, &chunk(101, 400), 0).map(|l| l.relation),
            Some(FigureRelation::Block),
            "and the block itself"
        );
    }

    /// Zero is the shipped default and it admits nothing outside the passage.
    #[test]
    fn without_a_window_a_figure_outside_the_passage_is_not_linked() {
        let elsewhere = figure(Some(5_000), None);
        assert_eq!(link(&elsewhere, &chunk(100, 200), 0), None);
    }

    /// With one, the hit carries the distance that ranks it — nearest first is
    /// what keeps a wrong extra figure from displacing the right one.
    #[test]
    fn a_window_admits_the_nearest_figures_and_says_how_far_they_are() {
        let above = figure(Some(60), None);
        let below = figure(Some(320), None);
        assert_eq!(
            link(&above, &chunk(100, 200), 500),
            Some(FigureLink {
                relation: FigureRelation::Near,
                byte_distance: 40,
            })
        );
        assert_eq!(
            link(&below, &chunk(100, 200), 500),
            Some(FigureLink {
                relation: FigureRelation::Near,
                byte_distance: 120,
            })
        );
        assert_eq!(
            link(&below, &chunk(100, 200), 100),
            None,
            "and refuses what falls outside it"
        );
    }

    /// A rendition extracted before anchors existed reports nothing rather
    /// than falling back to the page, which would link every figure on a page
    /// to every one of its chunks.
    #[test]
    fn a_figure_with_no_anchor_reports_nothing() {
        let legacy = figure(None, None);
        assert_eq!(link(&legacy, &chunk(100, 200), 10_000), None);
    }
}
