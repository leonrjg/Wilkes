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

use std::path::Path;

use crate::extract::image::{digest_pixels, NativeImage};
use crate::extract::pdf::mupdf;
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

    let mut png = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut png),
        pixels.as_raw(),
        pixels.width(),
        pixels.height(),
        image::ExtendedColorType::Rgb8,
    )
    .map_err(|e| anyhow::anyhow!("encoding {} as PNG failed: {e}", image.id))?;

    Ok(FigurePixels {
        width: pixels.width(),
        height: pixels.height(),
        source_width,
        source_height,
        png,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BoundingBox, ImageAnalysisStatus, ImageTransform};

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
