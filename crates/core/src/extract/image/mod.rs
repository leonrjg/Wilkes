//! Enrichment of the native raster images a PDF embeds.
//!
//! Two facts are established about each image, and they stay separate: the
//! literal text a recognizer transcribes from it, and a description of what it
//! shows. Both are merged into the one canonical reading at the position the
//! page drew the image, each byte carrying whether it was transcribed or
//! generated. Nothing here decides whether an image is a *figure* — no caption
//! matching, no `Fig.` heuristic — and nothing here is computed at search
//! time. This is versioned extraction, and every input to it is named in the
//! extraction recipe.
//!
//! The stages have one owner each and no fallbacks between them: a recognizer
//! failure is a visible partial result, never a second engine's turn.

pub mod describe;
pub mod ocr;
pub mod serialize;

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::types::{
    BoundingBox, ExtractedImage, ExtractionDiagnostics, ImageAnalysisStatus, ImageTransform,
    OcrAdmission,
};

use describe::FigureDescriber;
use ocr::OcrEngine;

/// The decoded pixels of one native image block, held only for as long as
/// analysis needs them.
///
/// Deliberately not part of [`ExtractedImage`]: the pixels are already in the
/// PDF, and a rendition that carried a second copy of every figure would grow
/// the index by the size of the library's artwork to answer questions the
/// digest already answers.
pub struct NativeImage {
    pub pixels: image::RgbImage,
}

// ── Technical limits ────────────────────────────────────────────────────────
//
// These exist to stop pathological work, not to decide what a figure is. Each
// is a fixed number, versioned with the recipe, reported in diagnostics when
// it fires, and covered by a test — so a limit that starts acting as a
// semantic filter is visible as one rather than being discovered later in a
// reading that quietly lost its diagrams.

/// Bumped whenever a limit below changes. Part of the analyzer identity, so a
/// document extracted under one set of limits never mixes with another.
pub const LIMITS_VERSION: &str = "image-limits-v1";

/// Above this the decode itself is the cost, and no figure in a document needs
/// it: 50 megapixels is a 10,000 x 5,000 image.
pub const MAX_DECODED_PIXELS: u64 = 50_000_000;

/// Under this an image cannot carry a legible label at any resolution. Two
/// pixels is not a minimum-size *figure* rule — it rejects the degenerate, not
/// the small — and the ordinary small image, a logo included, goes through
/// analysis like any other.
pub const MIN_DIMENSION_PIXELS: u32 = 2;

/// Why an image was not analyzed, or `None` when nothing technical stands in
/// the way.
pub fn technical_limit(width: u32, height: u32) -> Option<String> {
    if width < MIN_DIMENSION_PIXELS || height < MIN_DIMENSION_PIXELS {
        return Some(format!("degenerate size {width}x{height}"));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_DECODED_PIXELS {
        return Some(format!(
            "{pixels} pixels exceeds the {MAX_DECODED_PIXELS} decode limit"
        ));
    }
    None
}

/// A native image block found on a page, before analysis.
pub struct DiscoveredImage {
    pub id: String,
    pub page: u32,
    pub bbox: BoundingBox,
    pub transform: ImageTransform,
    pub decoded: Option<NativeImage>,
    /// Set when a technical limit rejected the image before it was decoded, or
    /// when decoding failed.
    pub rejected: Option<String>,
}

impl DiscoveredImage {
    pub fn digest(&self) -> String {
        match &self.decoded {
            Some(decoded) => {
                let mut hasher = Sha256::new();
                hasher.update(decoded.pixels.width().to_le_bytes());
                hasher.update(decoded.pixels.height().to_le_bytes());
                hasher.update(decoded.pixels.as_raw());
                format!("{:x}", hasher.finalize())
            }
            None => String::new(),
        }
    }
}

/// One word the page draws natively, with the box it was drawn in.
///
/// Some PDFs set a diagram's labels as real glyphs over the picture. Those
/// glyphs are the document's own and are better evidence than any
/// transcription of them, so a transcription that repeats them is dropped.
/// Geometry chooses the candidates — the words drawn inside this image's own
/// bounds, and no others — and the text decides the match, because two labels
/// on one page can read the same and only the one under the picture is a
/// duplicate of it.
pub struct NativeTextOnPage {
    pub page: u32,
    pub text: String,
    pub bbox: BoundingBox,
}

/// Everything the analyzer needs that is not the image itself.
///
/// The native words are grouped by page once per document rather than
/// rescanned per image: a long document has hundreds of thousands of them and
/// a page has hundreds.
#[derive(Default)]
pub struct AnalysisContext {
    native_text: HashMap<u32, Vec<NativeTextOnPage>>,
}

impl AnalysisContext {
    pub fn new(native_text: Vec<NativeTextOnPage>) -> Self {
        let mut by_page: HashMap<u32, Vec<NativeTextOnPage>> = HashMap::new();
        for word in native_text {
            by_page.entry(word.page).or_default().push(word);
        }
        Self {
            native_text: by_page,
        }
    }

    /// The words the page draws inside `bbox`, joined in reading order and
    /// folded for comparison. A transcription whose comparison form occurs in
    /// this string is text the document already carries as its own glyphs.
    pub fn native_text_within(&self, page: u32, bbox: &BoundingBox) -> String {
        let Some(words) = self.native_text.get(&page) else {
            return String::new();
        };
        let mut joined = String::new();
        for word in words {
            let centre = crate::types::Point {
                x: word.bbox.x + word.bbox.width / 2.0,
                y: word.bbox.y + word.bbox.height / 2.0,
            };
            if !ocr::contains(bbox, &centre) {
                continue;
            }
            let comparable = ocr::normalize_for_comparison(&word.text);
            if comparable.is_empty() {
                continue;
            }
            if !joined.is_empty() {
                joined.push(' ');
            }
            joined.push_str(&comparable);
        }
        joined
    }
}

/// One configured way to enrich native images.
///
/// Sync on purpose. Extraction is a synchronous contract with many callers —
/// indexing, the watcher, exact-search fallback, MCP reads, summaries and
/// export all reach it — and making it async to suit one analyzer would push
/// a runtime requirement onto every one of them. The analyzers that need to
/// talk to a server do so with a blocking client, which is what a batch
/// extraction pass wants anyway.
pub trait ImageAnalyzer: Send + Sync {
    /// The recipe this analyzer is: models, revisions, prompts, thresholds,
    /// preprocessing, limits and serialization version. Enters the extraction
    /// identity verbatim, so anything that changes the bytes must change this.
    fn identity(&self) -> String;

    /// Analyze the discovered images of one document, in order.
    fn analyze(
        &self,
        images: &mut [ExtractedImage],
        discovered: &[DiscoveredImage],
        context: &AnalysisContext,
        diagnostics: &mut ExtractionDiagnostics,
    );
}

/// The analyzer Wilkes runs when one is configured: one recognizer, and a
/// describer if there is one.
pub struct NativeImageAnalyzer {
    ocr: Box<dyn OcrEngine>,
    describer: Option<Box<dyn FigureDescriber>>,
}

impl NativeImageAnalyzer {
    pub fn new(ocr: Box<dyn OcrEngine>, describer: Option<Box<dyn FigureDescriber>>) -> Self {
        Self { ocr, describer }
    }
}

impl ImageAnalyzer for NativeImageAnalyzer {
    fn identity(&self) -> String {
        format!(
            "{}+{}+{}+{}",
            LIMITS_VERSION,
            serialize::SERIALIZATION_VERSION,
            self.ocr.identity(),
            self.describer
                .as_ref()
                .map_or_else(|| "no-describer".to_string(), |d| d.identity()),
        )
    }

    fn analyze(
        &self,
        images: &mut [ExtractedImage],
        discovered: &[DiscoveredImage],
        context: &AnalysisContext,
        diagnostics: &mut ExtractionDiagnostics,
    ) {
        let identity = self.identity();
        for (image, found) in images.iter_mut().zip(discovered) {
            image.analyzer_identity = identity.clone();

            let Some(decoded) = &found.decoded else {
                let reason = found
                    .rejected
                    .clone()
                    .unwrap_or_else(|| "not decoded".to_string());
                diagnostics.native_images_skipped_technical_limit += 1;
                image.status = ImageAnalysisStatus::SkippedTechnicalLimit { reason };
                continue;
            };

            diagnostics.native_images_analyzed += 1;
            let mut failures = Vec::new();

            // The recognizer runs whether or not a describer is configured:
            // the transcription is a fact about the document, and a missing
            // describer is a fact about this machine.
            match self.ocr.spot(&decoded.pixels) {
                Ok(regions) => {
                    diagnostics.images_ocr_succeeded += 1;
                    image.ocr_regions = ocr::place_and_admit(
                        regions,
                        &image.transform,
                        &image.bbox,
                        image.pixel_width,
                        image.pixel_height,
                        image.page,
                        context,
                        self.ocr.admission_threshold(),
                    );
                }
                Err(error) => {
                    diagnostics.images_ocr_failed += 1;
                    warn!("image {}: recognition failed: {error:#}", image.id);
                    failures.push(format!("recognition: {error}"));
                }
            }

            for region in &image.ocr_regions {
                match region.admission {
                    OcrAdmission::Accepted => diagnostics.ocr_regions_accepted += 1,
                    OcrAdmission::RejectedLowConfidence => {
                        diagnostics.ocr_regions_rejected_low_confidence += 1
                    }
                    OcrAdmission::DeduplicatedAgainstNativeText => {
                        diagnostics.ocr_regions_deduplicated_against_native_text += 1
                    }
                }
            }

            match &self.describer {
                None => diagnostics.images_description_not_configured += 1,
                Some(describer) => {
                    let accepted: Vec<_> = image.accepted_ocr().cloned().collect();
                    match describer.describe(&decoded.pixels, &accepted) {
                        Ok(description) => {
                            diagnostics.images_description_succeeded += 1;
                            image.description = Some(description);
                        }
                        Err(error) => {
                            diagnostics.images_description_failed += 1;
                            warn!("image {}: description failed: {error:#}", image.id);
                            failures.push(format!("description: {error}"));
                        }
                    }
                }
            }

            image.status = if failures.is_empty() {
                ImageAnalysisStatus::Complete
            } else {
                ImageAnalysisStatus::Partial { failures }
            };
        }
    }
}

/// Turn discovered blocks into the images of a rendition, and run the
/// configured analyzer over them.
///
/// With no analyzer the images are still enumerated, digested and counted —
/// the reading gains nothing, and the diagnostics say exactly that, which is
/// the difference between a document with no figures and a machine with no
/// recognizer installed.
pub fn analyze(
    discovered: &[DiscoveredImage],
    context: &AnalysisContext,
    analyzer: Option<&dyn ImageAnalyzer>,
    diagnostics: &mut ExtractionDiagnostics,
) -> Vec<ExtractedImage> {
    diagnostics.native_images_found = discovered.len() as u32;

    let mut images: Vec<ExtractedImage> = discovered
        .iter()
        .map(|found| {
            let (pixel_width, pixel_height) = found
                .decoded
                .as_ref()
                .map_or((0, 0), |decoded| {
                    (decoded.pixels.width(), decoded.pixels.height())
                });
            ExtractedImage {
                id: found.id.clone(),
                page: found.page,
                bbox: found.bbox.clone(),
                transform: found.transform,
                pixel_width,
                pixel_height,
                image_sha256: found.digest(),
                reading_range: None,
                ocr_regions: Vec::new(),
                description: None,
                analyzer_identity: String::new(),
                status: match &found.rejected {
                    Some(reason) => ImageAnalysisStatus::SkippedTechnicalLimit {
                        reason: reason.clone(),
                    },
                    None => ImageAnalysisStatus::Complete,
                },
            }
        })
        .collect();

    match analyzer {
        Some(analyzer) => analyzer.analyze(&mut images, discovered, context, diagnostics),
        None => {
            // No analyzer: the skipped ones are still skipped, and the rest
            // were never looked at. Neither is a success.
            for image in &mut images {
                if matches!(image.status, ImageAnalysisStatus::SkippedTechnicalLimit { .. }) {
                    diagnostics.native_images_skipped_technical_limit += 1;
                } else {
                    image.status = ImageAnalysisStatus::Partial {
                        failures: vec!["no image analyzer configured".to_string()],
                    };
                }
            }
        }
    }

    debug!(
        "images: {} found, {} analyzed, {} skipped, {} ocr ok, {} ocr failed, \
         {} regions accepted, {} rejected, {} deduplicated",
        diagnostics.native_images_found,
        diagnostics.native_images_analyzed,
        diagnostics.native_images_skipped_technical_limit,
        diagnostics.images_ocr_succeeded,
        diagnostics.images_ocr_failed,
        diagnostics.ocr_regions_accepted,
        diagnostics.ocr_regions_rejected_low_confidence,
        diagnostics.ocr_regions_deduplicated_against_native_text,
    );
    images
}

/// Decode one native image block's pixels, or say why not.
///
/// `samples` is the pixmap's raw component data, `components` its count per
/// pixel. Only the technical limits decide here; nothing about what the
/// picture contains.
pub fn decode(
    width: u32,
    height: u32,
    components: usize,
    stride: usize,
    samples: &[u8],
) -> Result<NativeImage, String> {
    if let Some(reason) = technical_limit(width, height) {
        return Err(reason);
    }
    if components == 0 {
        return Err("pixmap has no components".to_string());
    }
    let expected = stride
        .checked_mul(height as usize)
        .ok_or_else(|| "pixmap dimensions overflow".to_string())?;
    if samples.len() < expected {
        return Err(format!(
            "pixmap holds {} bytes, {expected} expected",
            samples.len()
        ));
    }

    let mut pixels = image::RgbImage::new(width, height);
    for y in 0..height as usize {
        let row = &samples[y * stride..y * stride + width as usize * components];
        for x in 0..width as usize {
            let pixel = &row[x * components..(x + 1) * components];
            // Grayscale, RGB and CMYK-as-4 all arrive here; anything with an
            // alpha channel has it last. Wilkes reads what is drawn, so an
            // alpha channel is ignored rather than composited against a
            // background it would have to invent.
            let rgb = match components {
                1 | 2 => [pixel[0], pixel[0], pixel[0]],
                _ => [pixel[0], pixel[1], pixel[2]],
            };
            pixels.put_pixel(x as u32, y as u32, image::Rgb(rgb));
        }
    }
    Ok(NativeImage { pixels })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degenerate_and_enormous_images_are_rejected_before_decoding() {
        assert!(technical_limit(1, 500).is_some());
        assert!(technical_limit(500, 0).is_some());
        assert!(technical_limit(20_000, 20_000).is_some());
    }

    /// The limits reject the degenerate and the pathological, and nothing
    /// else. A logo-sized image is analyzed like any other, because deciding
    /// it is not a figure is exactly what this phase does not do.
    #[test]
    fn ordinary_and_small_images_pass_the_limits() {
        assert!(technical_limit(1559, 499).is_none());
        assert!(technical_limit(16, 16).is_none());
        assert!(technical_limit(2, 2).is_none());
    }

    #[test]
    fn decoding_reads_rgb_and_greyscale_pixmaps() {
        let rgb = decode(2, 2, 3, 6, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12])
            .expect("rgb decodes");
        assert_eq!(rgb.pixels.get_pixel(0, 0).0, [1, 2, 3]);
        assert_eq!(rgb.pixels.get_pixel(1, 0).0, [4, 5, 6]);
        assert_eq!(rgb.pixels.get_pixel(1, 1).0, [10, 11, 12]);

        let grey = decode(2, 2, 1, 2, &[9, 200, 30, 40]).expect("greyscale decodes");
        assert_eq!(grey.pixels.get_pixel(0, 0).0, [9, 9, 9]);
        assert_eq!(grey.pixels.get_pixel(1, 0).0, [200, 200, 200]);
        assert_eq!(grey.pixels.get_pixel(0, 1).0, [30, 30, 30]);
    }

    /// A pixmap whose row stride exceeds its visible width — MuPDF pads — is
    /// read by stride, not by width times components.
    #[test]
    fn decoding_respects_the_row_stride() {
        let padded = decode(2, 2, 3, 8, &[1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0])
            .expect("padded decodes");
        assert_eq!(padded.pixels.get_pixel(0, 0).0, [1, 2, 3]);
        assert_eq!(padded.pixels.get_pixel(1, 0).0, [4, 5, 6]);
        assert_eq!(padded.pixels.get_pixel(0, 1).0, [7, 8, 9]);
        assert_eq!(padded.pixels.get_pixel(1, 1).0, [10, 11, 12]);
    }

    #[test]
    fn a_short_pixmap_is_an_error_rather_than_a_partial_image() {
        assert!(decode(4, 4, 3, 12, &[0; 8]).is_err());
    }
}
