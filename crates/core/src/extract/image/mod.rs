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
//!
//! # No model in this path runs in the host
//!
//! The recognizers, the formula reader and the layout detector are all
//! addressed over the worker protocol — [`worker_ocr::attach`] and
//! [`worker_layout::attach`] — and none of them is loaded here. What stays in
//! this process is everything that is not inference: which images exist, where
//! each region lands on the page, whether it clears admission, whether the
//! document already draws it as glyphs, what a reading is cached under, and
//! the recipe the whole thing is recorded under.
//!
//! The reason is the kill path. Reading a corpus is hours of inference, and
//! inference that has stopped making progress can only be stopped by killing
//! the process running it. A model loaded here would be one the user could
//! stop only by quitting the application, and a fault in it would take the
//! application with it. So [`build_analyzer`] is cheap by construction: it
//! resolves names, opens a cache and attaches proxies, and the first thing to
//! load a model is a worker serving its first request.

pub mod cache;
#[cfg(test)]
mod corpus;
pub mod describe;
/// What recognizers exist and how one is addressed, engine by engine.
pub mod dispatch;
/// The layout detector: which areas of a page are worth a recognizer call.
/// Behind `recognize-onnx` because that is the runtime its graph needs — the
/// same one the ONNX recognizer already uses.
#[cfg(feature = "recognize-onnx")]
pub mod doclayout;
#[cfg(feature = "recognize-onnx")]
pub mod granite_docling;
pub mod ocr;
/// The external door for description: whatever the user has pulled into
/// Ollama, asked with the same prompt as the first-class path.
pub mod ollama;
#[cfg(feature = "recognize-onnx")]
pub mod onnx_vlm;
/// The production recognizer. Behind the `candle` feature because that is the
/// runtime it uses — the one already pinned, with no second inference
/// dependency added to reach it.
#[cfg(feature = "candle")]
pub mod paddleocr_vl;
pub mod serialize;
#[cfg(feature = "recognize-onnx")]
pub mod texify;
#[cfg(all(feature = "recognize-vision", target_os = "macos"))]
pub mod vision;
/// The layout detector as the host addresses it, over the worker protocol.
pub mod worker_layout;
/// The recognizer as the host addresses it, over the worker protocol.
pub mod worker_ocr;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::Context as _;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::types::{
    BoundingBox, ExtractedImage, ExtractionDiagnostics, ImageAnalysisStatus, ImageScope,
    ImageTransform, OcrAdmission, RegionKind, RegionOrigin,
};

use describe::FigureDescriber;
use ocr::{ImageRecognition, OcrEngine};

/// The images of one document, handed to a recognizer.
///
/// Paths, not pixels. A document's worth of figures inlined as base64 would be
/// a single JSON line of a hundred megabytes, held whole on both sides of the
/// pipe; a path is a path. It costs nothing in speed either way — recognition
/// is minutes per image and encoding is milliseconds — so the choice is about
/// what the protocol does on a fifty-figure document, which is to stay the
/// same size.
///
/// PNG because it is lossless, and files because every language opens a file:
/// the recognizer serving this need not be Rust, and a Rust-shaped payload
/// would have made that a fiction.
///
/// The files belong to the caller, which writes them somewhere it owns and
/// removes them when the batch is done, killed or not.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RecognitionRequest {
    /// One PNG per image, in the order the results must come back.
    pub image_paths: Vec<std::path::PathBuf>,
}

/// One rendered page, staged for the layout detector in the worker.
///
/// One page rather than a document's worth. Detection is a quarter of a second
/// a page against minutes an image for recognition, and the host discovers a
/// page's typeset areas before it can decide what to render next — so batching
/// here would mean rendering every page of a document up front and holding
/// them, to save a hop that costs milliseconds.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct LayoutRequest {
    pub image_path: std::path::PathBuf,
}

/// One [`LayoutRegion`] as it crosses the worker pipe.
///
/// `label` is a `String` here and a `&'static str` there. The static is the
/// point of the real type — a label is one of the detector's own classes, and
/// the diagnostics count them by identity — so the crossing back is a lookup
/// in that vocabulary, and a label that is not in it is an error rather than a
/// region nobody can account for.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct WireLayoutRegion {
    pub label: String,
    pub score: f32,
    pub bbox: BoundingBox,
}

impl WireLayoutRegion {
    pub fn from_region(region: &LayoutRegion) -> Self {
        Self {
            label: region.label.to_string(),
            score: region.score,
            bbox: region.bbox.clone(),
        }
    }

    /// Resolve back into the detector's vocabulary, or say which label did not
    /// belong to it.
    pub fn into_region(self) -> anyhow::Result<LayoutRegion> {
        #[cfg(feature = "recognize-onnx")]
        {
            let label = doclayout::label_of(&self.label).ok_or_else(|| {
                anyhow::anyhow!(
                    "the layout detector returned the class {:?}, which this build does not know",
                    self.label
                )
            })?;
            Ok(LayoutRegion {
                label,
                kind: doclayout::kind_of(label),
                score: self.score,
                bbox: self.bbox,
            })
        }
        #[cfg(not(feature = "recognize-onnx"))]
        {
            anyhow::bail!(
                "a layout region arrived for the class {:?}, but this build has no layout \
                 detector and therefore no vocabulary to resolve it against",
                self.label
            )
        }
    }
}

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

/// Check one installed artifact against the size and digest its module
/// declares.
///
/// Shared by every model that pins a revision, because "the file on disk is
/// the file the recipe names" is one question with one answer, and two copies
/// of it are two chances to check it differently.
pub(super) fn verify_artifact(
    path: &std::path::Path,
    size_bytes: u64,
    sha256: &str,
) -> anyhow::Result<()> {
    let found = std::fs::metadata(path)?.len();
    anyhow::ensure!(
        found == size_bytes,
        "{} is {found} bytes, {size_bytes} expected",
        path.display()
    );
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    let digest = format!("{:x}", hasher.finalize());
    anyhow::ensure!(
        digest == sha256,
        "{} hashes to {digest}, {sha256} expected",
        path.display()
    );
    Ok(())
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

/// One area a layout detector marked out on a page.
///
/// Coordinates are fractions of the page — x and width against its width, y
/// and height against its height, origin top-left — not pixels of whatever
/// render the detector was shown. The caller chooses the render scale and the
/// detector must not make that its business.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutRegion {
    /// The detector's own class name, verbatim. Carried beside `kind` because
    /// several classes map to one kind and one maps to none: "forty inline
    /// formulas" and "forty display formulas" are different facts about a
    /// document, and the diagnostics report the class.
    pub label: &'static str,
    /// What Wilkes reads this as, or `None` when nothing does.
    pub kind: Option<RegionKind>,
    pub score: f32,
    pub bbox: BoundingBox,
}

/// Something that can say what a rendered page holds and where.
///
/// One method and one answer. Everything about *which* areas are then rendered
/// and read belongs to the caller, and everything about whether the reading is
/// admitted belongs to the admission rules — a detector that decided either
/// would be a second router.
pub trait LayoutModel: Send + Sync {
    /// The detector's recipe, as it enters the extraction identity: the graph,
    /// its revision, the size it is fed and the score it believes.
    fn identity(&self) -> String;

    /// The square, in pixels, a page must be rendered at to be detected on.
    ///
    /// Asked rather than assumed: a detector trained on one input size is
    /// answering a different question when fed another, and the caller that
    /// rasterizes the page has no business knowing which model is behind this.
    fn input_side(&self) -> u32;

    /// Mark out one rendered page. The render must be the size this detector
    /// declares; it is not this method's job to resize a page for a model that
    /// was trained on a particular one.
    fn detect(&self, page: &image::RgbImage) -> anyhow::Result<Vec<LayoutRegion>>;

    /// Let go of whatever this keeps resident, without disabling it.
    ///
    /// The same contract as [`OcrEngine::release`] and for the same reason: an
    /// index build detects on every page of the corpus in one pass and then
    /// embeds for as long again, and a detection graph resident through a pass
    /// with no pages for it is megabytes held to save a load. A later `detect`
    /// works exactly as before, at the cost of that load.
    fn release(&self);
}

/// An image found on a page, before analysis: a raster the PDF embeds, or an
/// area the page typesets that Wilkes rendered so a recognizer could read it.
pub struct DiscoveredImage {
    pub id: String,
    pub page: u32,
    /// Which of those two this is. It travels with the region rather than
    /// being inferred from the id, because it decides an admission rule and a
    /// label, and a rule read off a string is a rule waiting to be broken by a
    /// rename.
    pub origin: RegionOrigin,
    pub bbox: BoundingBox,
    pub transform: ImageTransform,
    pub decoded: Option<NativeImage>,
    /// Set when a technical limit rejected the image before it was decoded, or
    /// when decoding failed.
    pub rejected: Option<String>,
    /// Set when the configured [`ImageScope`] does not read pictures of this
    /// origin. The pixels were never asked for, so `decoded` is `None` and
    /// `digest` answers empty — which is the honest record of a picture
    /// nobody looked at.
    ///
    /// Separate from `rejected` because the two are different facts, and the
    /// diagnostics count them separately for the same reason.
    pub withheld_by_scope: bool,
    /// What the layout detector called this area, for the areas a page
    /// typesets. `None` for a raster the document embeds, which nothing has
    /// classified before it is read.
    ///
    /// This is what picks the recognizer. A formula and a page are different
    /// inputs and are read by different models — see
    /// [`NativeImageAnalyzer::route`] — so the classification has to travel
    /// with the region rather than be recovered from its transcription, which
    /// would be a routing decision made after the routing.
    pub kind: Option<RegionKind>,
}

/// The digest of decoded pixels: dimensions then samples, so two images of
/// the same bytes at different shapes cannot collide.
///
/// One function because it is a contract, not a convenience. The annotation
/// cache is addressed by this value and a re-derived picture is checked
/// against it, so a second implementation drifting by a byte would look like
/// every figure in the library having changed.
pub fn digest_pixels(pixels: &image::RgbImage) -> String {
    let mut hasher = Sha256::new();
    hasher.update(pixels.width().to_le_bytes());
    hasher.update(pixels.height().to_le_bytes());
    hasher.update(pixels.as_raw());
    format!("{:x}", hasher.finalize())
}

impl DiscoveredImage {
    pub fn digest(&self) -> String {
        match &self.decoded {
            Some(decoded) => digest_pixels(&decoded.pixels),
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

/// Which of the two recognizers a region goes to.
///
/// Not a fallback pair. A formula with no formula reader is a misconfiguration
/// and is recorded as one — sending it to the page reader instead would be a
/// second way to read a formula, producing a reading nobody chose and no
/// recipe describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Formula,
    Page,
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

/// The one image analyzer this process reads documents with.
///
/// A single owner, because the invariant is exactly that: every consumer of a
/// rendition — indexing, watcher updates, exact-search fallback, MCP reads,
/// summaries, export — must see the same enrichment. Passing an analyzer per
/// call site is what lets one consumer enrich a document and another not, and
/// then write both answers into one index under recipes that disagree.
///
/// One at a time, not one forever: the setting that decides it is editable
/// while the app runs, and a lock that could only be written once would make
/// turning the feature on a restart. Replacing it is safe because a reading
/// records the recipe that produced it — extraction identity is what keeps
/// the two answers apart, and it is already the mechanism that re-reads
/// documents when the recipe moves.
///
/// Unset means no enrichment, which is a configuration and not a failure:
/// images are still found, digested and counted, and the diagnostics say the
/// analysis did not run.
static CONFIGURED_ANALYZER: RwLock<Option<Arc<dyn ImageAnalyzer>>> = RwLock::new(None);

/// Install this process's analyzer, replacing whatever it was.
pub fn configure(analyzer: Option<Arc<dyn ImageAnalyzer>>) {
    let named = analyzer.as_ref().map(|analyzer| analyzer.identity());
    *CONFIGURED_ANALYZER
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = analyzer;
    match named {
        Some(identity) => info!("image analyzer configured: {identity}"),
        None => info!("no image analyzer configured"),
    }
}

/// This process's analyzer, or none.
pub fn configured() -> Option<Arc<dyn ImageAnalyzer>> {
    CONFIGURED_ANALYZER
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[cfg(feature = "candle")]
/// Build the analyzer the settings describe, or `None` when they describe
/// none.
///
/// Cheap, and deliberately so: it resolves names and opens a cache, and loads
/// nothing. The 1.9 GB is loaded by the worker, on its first image, and stays
/// resident there — so attaching an analyzer no longer costs the host a model
/// load, and a fault while recognizing no longer costs it the application.
///
/// A recognizer that is enabled but that this build does not ship is an error,
/// not a silent disable: the user asked for enrichment, and a reading that
/// quietly omits it is indistinguishable from one that found no text. Whether
/// the weights are actually on disk is a separate question, answered per
/// recognizer by [`recognizer_catalogue`] before the toggle is offered.
///
/// `model_dir` is the installation's model cache; `cache_dir` is where the
/// annotation cache lives. Two parameters because they are two directories:
/// the annotations are keyed by recipe and outlive any one checkpoint, and
/// filing them under the cache root would put them inside what a model
/// uninstall removes.
pub fn build_analyzer(
    recognizers: crate::worker::manager::WorkerManager,
    model_dir: &std::path::Path,
    cache_dir: &std::path::Path,
    settings: &crate::types::ImageAnalysisSettings,
    ollama_url: &str,
) -> anyhow::Result<Option<Arc<dyn ImageAnalyzer>>> {
    if !settings.enabled {
        return Ok(None);
    }
    let engine = settings.engine;
    let model_id = settings
        .model
        .clone()
        .unwrap_or_else(|| engine.default_model().to_string());
    anyhow::ensure!(
        dispatch::installed(engine, &model_id, model_dir)?,
        "the '{model_id}' recognizer is enabled but not installed"
    );
    // The catalogue holds formula readers beside page readers, so the setting
    // can now name one. It is an error rather than a quiet substitution: a
    // formula reader emits one whole-crop region and would read every page of
    // the library as a single failed expression, which afterwards is
    // indistinguishable from a library with no text in its pictures.
    anyhow::ensure!(
        dispatch::role(engine, &model_id, model_dir)? == dispatch::RecognizerRole::Page,
        "'{model_id}' reads formulas, not pages, and cannot be the page recognizer"
    );
    let scratch = cache_dir.join("recognition-scratch");
    let layout = attach_layout_detector(recognizers.clone(), model_dir, &scratch)?;
    let recognizer = worker_ocr::attach(
        recognizers.clone(),
        engine,
        &model_id,
        model_dir.to_path_buf(),
        scratch.clone(),
        settings.device.as_deref().unwrap_or("auto"),
    )
    .context("could not address the image recognizer")?;

    let describer: Option<Box<dyn FigureDescriber>> = match settings.describer_model.trim() {
        "" => None,
        model => Some(Box::new(ollama::OllamaDescriber::new(ollama_url, model)?)),
    };

    let mut analyzer = NativeImageAnalyzer::new(recognizer, describer, settings.scope, layout);
    if let Some(formula) = attach_formula_reader(
        recognizers,
        model_dir,
        &scratch,
        settings.device.as_deref().unwrap_or("auto"),
    )? {
        analyzer = analyzer.with_formula_reader(formula);
    }
    Ok(Some(Arc::new(
        analyzer.with_cache(cache::AnnotationCache::open(cache_dir)?),
    )))
}

/// Address the recognizer for formulas, or `None` when this installation has
/// none.
///
/// Optional, and found by role rather than by name: it is one row of the
/// recognizer catalogue like the page readers, and a build that ships no such
/// row or an installation that has not downloaded it is a configuration rather
/// than an error. Absent, the areas the detector called formulas go to the
/// page reader — see [`NativeImageAnalyzer::route`] for what that costs.
///
/// Attaching is cheap and loads nothing: the weights are loaded by the worker,
/// on its first crop. Nothing is loaded here because nothing in this path is
/// ever loaded here — see [`dispatch`]'s invariant.
#[cfg(feature = "candle")]
fn attach_formula_reader(
    recognizers: crate::worker::manager::WorkerManager,
    model_dir: &std::path::Path,
    scratch: &std::path::Path,
    device: &str,
) -> anyhow::Result<Option<Box<dyn OcrEngine>>> {
    let Some(model) = dispatch::formula_model(model_dir) else {
        tracing::info!("this build ships no formula recognizer");
        return Ok(None);
    };
    if !model.is_cached {
        tracing::warn!(
            "the formula recognizer '{}' is not installed; the areas the detector marks out \
             as formulas will go to the page recognizer instead",
            model.model_id
        );
        return Ok(None);
    }
    Ok(Some(
        worker_ocr::attach(
            recognizers,
            model.engine,
            &model.model_id,
            model_dir.to_path_buf(),
            scratch.to_path_buf(),
            device,
        )
        .context("could not address the formula recognizer")?,
    ))
}

/// Address the layout detector, or `None` when this installation has none.
///
/// Optional, like the formula reader and for the same reason: an installation
/// that has not downloaded it is a configuration, not a failure to start. What
/// it costs is stated where it is offered, because it cannot be inferred from
/// the reading afterwards — without a detector nothing a page *typesets* is
/// marked out, so a document full of mathematics reads exactly like one with
/// none. Rasters the PDF embeds are unaffected; those are found by the
/// extractor, not by this.
///
/// Attaching is cheap and loads nothing. The graph is loaded by the worker, on
/// its first page — see [`dispatch`]'s invariant for why not here.
#[cfg(feature = "candle")]
fn attach_layout_detector(
    recognizers: crate::worker::manager::WorkerManager,
    model_dir: &std::path::Path,
    scratch: &std::path::Path,
) -> anyhow::Result<Option<Box<dyn LayoutModel>>> {
    #[cfg(feature = "recognize-onnx")]
    {
        if !doclayout::is_installed(model_dir) {
            tracing::warn!(
                "the layout detector is not installed; nothing a page typesets will be \
                 marked out, so formulas and ruled tables will not be read"
            );
            return Ok(None);
        }
        Ok(Some(
            worker_layout::attach(
                recognizers,
                model_dir.to_path_buf(),
                scratch.to_path_buf(),
            )
            .context("could not address the layout detector")?,
        ))
    }
    #[cfg(not(feature = "recognize-onnx"))]
    {
        let (_, _, _) = (recognizers, model_dir, scratch);
        tracing::info!("this build has no layout detector compiled in");
        Ok(None)
    }
}

/// What the layout detector is, where it came from, and under what terms.
#[cfg(feature = "recognize-onnx")]
pub fn layout_detector_inventory() -> crate::types::RecognizerInventory {
    doclayout::inventory()
}

/// Whether the layout detector's artifacts are on disk.
#[cfg(feature = "recognize-onnx")]
pub fn layout_detector_installed(model_dir: &std::path::Path) -> bool {
    doclayout::is_installed(model_dir)
}

/// Download and verify the layout detector.
#[cfg(feature = "recognize-onnx")]
pub fn install_layout_detector(
    model_dir: &std::path::Path,
    progress: Option<crate::models::progress::ProgressTx>,
) -> anyhow::Result<()> {
    doclayout::install(model_dir, progress)
}

/// What a recognizer is, where it came from, and under what terms.
///
/// Static: this describes the recipe, not the machine, so it answers before
/// anything is installed. That is the point of it — the terms and the size are
/// disclosed where the download is offered, rather than after the bytes have
/// arrived.
pub fn recognizer_inventory(
    engine: dispatch::RecognitionEngine,
    model_id: &str,
) -> anyhow::Result<crate::types::RecognizerInventory> {
    dispatch::inventory(engine, model_id)
}

/// What every recognizer this build can read with is, described, alongside the
/// engines the build actually compiled in.
pub fn recognizer_catalogue(data_dir: &std::path::Path) -> dispatch::RecognizerCatalogue {
    dispatch::RecognizerCatalogue {
        engines: dispatch::RecognitionEngine::supported_engines(),
        models: dispatch::list_models(data_dir),
        #[cfg(feature = "recognize-onnx")]
        detector: Some(dispatch::InstallableModelStatus {
            inventory: doclayout::inventory(),
            is_installed: doclayout::is_installed(data_dir),
        }),
        #[cfg(not(feature = "recognize-onnx"))]
        detector: None,
    }
}

/// Download and verify a recognizer.
pub fn install_recognizer(
    engine: dispatch::RecognitionEngine,
    model_id: &str,
    data_dir: &std::path::Path,
    progress: Option<crate::models::progress::ProgressTx>,
) -> anyhow::Result<()> {
    dispatch::install(engine, model_id, data_dir, progress)
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

    /// Whether the rasters a PDF embeds are read, as against only the areas
    /// the page typesets as a formula or a ruled table.
    ///
    /// Asked *before* the pixels are decoded, by the backend that would decode
    /// them, so a picture this analyzer would not read costs neither a decode
    /// nor a document's worth of resident artwork. Required rather than
    /// defaulted: an analyzer that did not answer would be one whose cost the
    /// backend had to guess at, and the guess that is safe for the reading is
    /// the expensive one.
    fn reads_embedded_images(&self) -> bool;

    /// The layout detector this analyzer routes with, or `None` when it has
    /// none and therefore marks out nothing a page typesets.
    ///
    /// Asked by the backend before a page is rendered, so a configuration with
    /// no detector does not pay a render per page to be told nothing. There is
    /// no second answer here — no font rule, no rule-stack scan — because a
    /// second way to decide the same question is exactly what this replaced.
    fn layout(&self) -> Option<&dyn LayoutModel>;

    /// Analyze the discovered images of one document, in order.
    fn analyze(
        &self,
        images: &mut [ExtractedImage],
        discovered: &[DiscoveredImage],
        context: &AnalysisContext,
        diagnostics: &mut ExtractionDiagnostics,
    );

    /// Let go of whatever this analyzer keeps resident, without disabling it.
    ///
    /// The index build reads every figure in the corpus in one pass and then
    /// embeds for as long again; this is how it says the first pass is over.
    /// A later `analyze` works exactly as before — what it costs is a reload,
    /// once, instead of a recognizer resident through a pass that has no
    /// images for it.
    ///
    /// Required rather than defaulted: an analyzer that holds a model out of
    /// process is the ordinary case, and one that silently kept it would look
    /// identical to one that had nothing to release.
    fn release(&self);
}

/// The analyzer Wilkes runs when one is configured: one recognizer, a
/// describer if there is one, and a cache if there is somewhere to keep it.
pub struct NativeImageAnalyzer {
    ocr: Box<dyn OcrEngine>,
    describer: Option<Box<dyn FigureDescriber>>,
    cache: Option<cache::AnnotationCache>,
    /// Which of a document's pictures this analyzer is spent on. Part of
    /// [`Self::identity`], because it decides which of them reach the reading
    /// and two readings taken under different scopes are different readings.
    scope: ImageScope,
    /// What marks out the areas a page typesets. In the identity for the same
    /// reason as the recognizer: it decides which areas are read, and two
    /// readings routed by different detectors are different readings.
    layout: Option<Box<dyn LayoutModel>>,
    /// The recognizer for formulas, which is not the one for pages.
    ///
    /// Measured rather than assumed: a page parser handed a crop of one
    /// expression comes apart — ten inline crops through granite-docling
    /// produced no admissible region, four of them looping to the token cap —
    /// while a model trained on cropped expressions reads them. So a formula
    /// goes to [`texify`] and everything else to the page reader, and both are
    /// in the identity because both decide what a reading says.
    formula: Option<Box<dyn OcrEngine>>,
}

impl NativeImageAnalyzer {
    /// The scope is a constructor argument rather than a builder step: it
    /// changes what a reading contains, and a call site that could omit it
    /// would eventually be one that did.
    pub fn new(
        ocr: Box<dyn OcrEngine>,
        describer: Option<Box<dyn FigureDescriber>>,
        scope: ImageScope,
        layout: Option<Box<dyn LayoutModel>>,
    ) -> Self {
        Self {
            ocr,
            describer,
            cache: None,
            scope,
            layout,
            formula: None,
        }
    }

    /// Read the areas the detector called formulas with `formula` instead of
    /// the page reader.
    ///
    /// A builder step rather than a constructor argument because the two are
    /// not peers: a configuration with no detector marks out no formulas and
    /// has nothing for this to read. [`dispatch::build_analyzer`] supplies it
    /// exactly when it supplies a detector.
    pub fn with_formula_reader(mut self, formula: Box<dyn OcrEngine>) -> Self {
        self.formula = Some(formula);
        self
    }

    /// Which recognizer reads this region.
    ///
    /// The detector's own word, never the transcription's: what a region *is*
    /// was settled when it was marked out, and deciding it again from what
    /// came back would be routing after the routing.
    ///
    /// With no formula reader installed, formulas go to the page reader. That
    /// is a worse reading and not a silent one — it is in [`Self::identity`],
    /// so a library read this way is re-read if a formula reader is installed
    /// later, and it was warned about when the analyzer was built. Measured on
    /// this repository's corpus, ten inline crops through granite-docling
    /// yielded no admissible region and four decodes ran to the token cap, so
    /// what this mostly buys is the cost of trying.
    fn route(&self, found: &DiscoveredImage) -> Route {
        match (found.origin, found.kind) {
            (RegionOrigin::Typeset, Some(RegionKind::Formula)) if self.formula.is_some() => {
                Route::Formula
            }
            _ => Route::Page,
        }
    }

    /// Keep annotations under `data_dir`, so a second reading of the same
    /// document does not run the models again.
    pub fn with_cache(mut self, cache: cache::AnnotationCache) -> Self {
        self.cache = Some(cache);
        self
    }

    fn cache_key(&self, image: &ExtractedImage) -> cache::AnnotationKey {
        cache::AnnotationKey {
            analyzer_identity: self.identity(),
            page: image.page,
            image_sha256: image.image_sha256.clone(),
            pixel_width: image.pixel_width,
            pixel_height: image.pixel_height,
            bbox: image.bbox.clone(),
            transform: image.transform,
        }
    }
}

impl ImageAnalyzer for NativeImageAnalyzer {
    fn identity(&self) -> String {
        format!(
            "{}+{}+{}+{}+{}+{}+{}+{}",
            LIMITS_VERSION,
            ocr::MAPPING_VERSION,
            ocr::ADMISSION_RULES_VERSION,
            serialize::SERIALIZATION_VERSION,
            match self.scope {
                ImageScope::TypesetOnly => "scope-typeset",
                ImageScope::TypesetAndEmbedded => "scope-typeset-and-embedded",
            },
            self.layout
                .as_ref()
                .map_or_else(|| "no-detector".to_string(), |layout| layout.identity()),
            self.ocr.identity(),
            self.describer
                .as_ref()
                .map_or_else(|| "no-describer".to_string(), |d| d.identity()),
        ) + &self.formula.as_ref().map_or_else(
            || "+no-formula-reader".to_string(),
            |f| format!("+{}", f.identity()),
        )
    }

    fn reads_embedded_images(&self) -> bool {
        matches!(self.scope, ImageScope::TypesetAndEmbedded)
    }

    fn layout(&self) -> Option<&dyn LayoutModel> {
        self.layout.as_deref()
    }

    /// Forwarded to the recognizer, which is the only part of this analyzer
    /// that holds anything: the cache is files, and a describer is a server
    /// this process does not own.
    fn release(&self) {
        self.ocr.release();
        if let Some(formula) = &self.formula {
            formula.release();
        }
        if let Some(layout) = &self.layout {
            layout.release();
        }
    }

    fn analyze(
        &self,
        images: &mut [ExtractedImage],
        discovered: &[DiscoveredImage],
        context: &AnalysisContext,
        diagnostics: &mut ExtractionDiagnostics,
    ) {
        let identity = self.identity();

        // Pass one settles everything settleable without the recognizer: what
        // a technical limit rejected, and what a previous reading under this
        // same recipe already answered. What is left is the batch — and only
        // that, so re-reading a document whose figures are all cached sends
        // nothing at all.
        let mut pending: Vec<usize> = Vec::new();
        for (index, (image, found)) in images.iter_mut().zip(discovered).enumerate() {
            image.analyzer_identity = identity.clone();

            // Asked first, because it is the only one of these that is a
            // statement about the configuration rather than about the image.
            if found.withheld_by_scope {
                let reason = withheld_reason(image.origin);
                if image.origin == RegionOrigin::Embedded {
                    diagnostics.native_images_not_read += 1;
                }
                debug!("image {} not read: {reason}", image.id);
                image.status = ImageAnalysisStatus::NotRead { reason };
                continue;
            }

            if found.decoded.is_none() {
                let reason = found
                    .rejected
                    .clone()
                    .unwrap_or_else(|| "not decoded".to_string());
                if image.origin == RegionOrigin::Embedded {
                    diagnostics.native_images_skipped_technical_limit += 1;
                }
                debug!("image {} skipped: {reason}", image.id);
                image.status = ImageAnalysisStatus::SkippedTechnicalLimit { reason };
                continue;
            }

            // A cached annotation is the same analysis, already done. It is
            // still counted as analyzed: what the reading contains is what a
            // full analysis established, and a consumer reading the counts is
            // asking about the document, not about this machine's disk.
            let key = self.cache_key(image);
            if let Some(cached) = self.cache.as_ref().and_then(|cache| cache.get(&key)) {
                if image.origin == RegionOrigin::Embedded {
                    diagnostics.native_images_analyzed += 1;
                }
                diagnostics.images_ocr_succeeded += 1;
                image.ocr_regions = cached.ocr_regions;
                image.description = cached.description;
                image.status = cached.status;
                count_regions(image, diagnostics);
                if image.origin == RegionOrigin::Typeset {
                    // The description stage does not apply, cached or not.
                } else if image.description.is_some() {
                    diagnostics.images_description_succeeded += 1;
                } else if self.describer.is_none() {
                    diagnostics.images_description_not_configured += 1;
                }
                debug!("image {} was already analyzed under this recipe", image.id);
                continue;
            }

            pending.push(index);
        }

        if pending.is_empty() {
            return;
        }

        // One call per recognizer, one wait each. The loop that used to be
        // here — ask, wait minutes, ask again — was work the host could not be
        // stopped in the middle of, and killing the recognizer only made it
        // ask again for the next image, spawning a replacement. The
        // recognizer's own loop runs inside the process that can be killed.
        //
        // Two calls rather than one because there are two recognizers, and a
        // batch is the unit that crosses into one of them. They are kept in
        // separate lists rather than interleaved so neither model is loaded to
        // answer nothing.
        let mut routed: Vec<(usize, Route)> = Vec::with_capacity(pending.len());
        for index in &pending {
            routed.push((*index, self.route(&discovered[*index])));
        }

        let mut read: HashMap<usize, std::result::Result<ImageRecognition, String>> =
            HashMap::with_capacity(routed.len());
        for route in [Route::Formula, Route::Page] {
            let group: Vec<usize> = routed
                .iter()
                .filter(|(_, r)| *r == route)
                .map(|(index, _)| *index)
                .collect();
            if group.is_empty() {
                continue;
            }
            // `route` only says `Formula` when there is a formula reader, so
            // this cannot be `None`. Stated as an expectation rather than
            // handled, because a second answer here would be a second router.
            let engine: &dyn OcrEngine = match route {
                Route::Formula => self
                    .formula
                    .as_deref()
                    .expect("route() says Formula only when a formula reader is attached"),
                Route::Page => self.ocr.as_ref(),
            };

            let batch: Vec<image::RgbImage> = group
                .iter()
                .map(|index| {
                    discovered[*index]
                        .decoded
                        .as_ref()
                        .expect("a pending image is a decoded one")
                        .pixels
                        .clone()
                })
                .collect();
            info!(
                "recognizing {} {} with {}",
                batch.len(),
                match route {
                    Route::Formula => "formula(s)",
                    Route::Page => "image(s)",
                },
                engine.identity()
            );
            let started = std::time::Instant::now();
            let spotted = engine.spot_batch(&batch);
            drop(batch);

            // A batch fails as a batch: one recognizer, one request, one
            // outcome. Every image it covered is a partial result carrying the
            // reason, not a complete analysis that happened to find nothing —
            // a killed recognizer must not read as a document whose figures
            // have no text in them.
            match spotted {
                Ok(spotted) => {
                    info!(
                        "recognized {} region(s) in {:.1}s",
                        spotted.len(),
                        started.elapsed().as_secs_f32()
                    );
                    for (index, one) in group.into_iter().zip(spotted) {
                        read.insert(index, Ok(one));
                    }
                }
                Err(error) => {
                    let reason = format!("{error:#}");
                    warn!("recognition failed for {} image(s): {reason}", group.len());
                    for index in group {
                        read.insert(index, Err(reason.clone()));
                    }
                }
            }
        }

        for (index, route) in routed {
            let decoded = discovered[index]
                .decoded
                .as_ref()
                .expect("a pending image is a decoded one");
            let image = &mut images[index];
            let key = self.cache_key(image);
            if image.origin == RegionOrigin::Embedded {
                diagnostics.native_images_analyzed += 1;
            }
            let mut failures = Vec::new();

            let threshold = match route {
                Route::Formula => self
                    .formula
                    .as_ref()
                    .expect("route() says Formula only when a formula reader is attached")
                    .admission_threshold(),
                Route::Page => self.ocr.admission_threshold(),
            };
            match read.get(&index).expect("every routed image was answered") {
                Ok(read) => {
                    diagnostics.images_ocr_succeeded += 1;
                    diagnostics.regions_unroutable += read.unroutable;
                    diagnostics.regions_marked_not_text += read.not_text;
                    image.ocr_regions = ocr::place_and_admit(
                        read.regions.clone(),
                        &image.transform,
                        &image.bbox,
                        image.pixel_width,
                        image.pixel_height,
                        image.page,
                        image.origin,
                        context,
                        threshold,
                    );
                }
                Err(reason) => {
                    diagnostics.images_ocr_failed += 1;
                    failures.push(format!("recognition: {reason}"));
                }
            }

            count_regions(image, diagnostics);

            match &self.describer {
                // A description is a generated claim about what a *picture*
                // shows. A typeset region is a rendering Wilkes made of the
                // document's own typesetting, and there is no picture in it to
                // describe — the formula it contains is already in the reading
                // as LaTeX. Not a failure and not a missing configuration:
                // this stage does not apply to it.
                _ if image.origin == RegionOrigin::Typeset => {}
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

            // Only a reading the recognizer actually produced is worth
            // keeping. Caching a failure would make one killed batch the
            // permanent answer for every figure it covered.
            if self.cache.is_some() && read.get(&index).is_some_and(|one| one.is_ok()) {
                if let Some(cache) = &self.cache {
                    cache.put(
                        &key,
                        &cache::Annotation {
                            ocr_regions: image.ocr_regions.clone(),
                            description: image.description.clone(),
                            status: image.status.clone(),
                        },
                    );
                }
            }
        }
    }
}

/// Count what was read and what became of it.
///
/// Two questions, counted separately because they fail separately: what kind
/// of content the recognizer found, and which of it entered the reading. A
/// table nobody recognized and a table recognized and refused as ragged are
/// both absent from the reading and are not the same fact about the document.
fn count_regions(image: &ExtractedImage, diagnostics: &mut ExtractionDiagnostics) {
    for region in &image.ocr_regions {
        match region.kind {
            RegionKind::Text => diagnostics.regions_routed_text += 1,
            RegionKind::Formula => diagnostics.regions_routed_formula += 1,
            RegionKind::Table => diagnostics.regions_routed_table += 1,
            RegionKind::Chart => diagnostics.regions_routed_chart += 1,
            RegionKind::Code => diagnostics.regions_routed_code += 1,
        }

        match region.admission {
            OcrAdmission::Accepted => diagnostics.ocr_regions_accepted += 1,
            OcrAdmission::RejectedLowConfidence => {
                diagnostics.ocr_regions_rejected_low_confidence += 1
            }
            OcrAdmission::DeduplicatedAgainstNativeText => {
                diagnostics.ocr_regions_deduplicated_against_native_text += 1
            }
            OcrAdmission::RejectedInvalidLatex => diagnostics.formulas_rejected_invalid_latex += 1,
            OcrAdmission::RejectedMalformedTable => match region.kind {
                RegionKind::Chart => diagnostics.charts_rejected_malformed += 1,
                _ => diagnostics.tables_rejected_malformed += 1,
            },
        }

        if region.admission == OcrAdmission::Accepted {
            match region.kind {
                RegionKind::Formula => diagnostics.formulas_accepted += 1,
                RegionKind::Table => diagnostics.tables_accepted += 1,
                RegionKind::Chart => diagnostics.charts_accepted += 1,
                RegionKind::Text | RegionKind::Code => {}
            }
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
    // The embedded ones only. A typeset region is an area Wilkes rendered, not
    // a block MuPDF exposed, and counting it here would report figures the
    // document does not contain.
    diagnostics.native_images_found = discovered
        .iter()
        .filter(|found| found.origin == RegionOrigin::Embedded)
        .count() as u32;

    let mut images: Vec<ExtractedImage> = discovered
        .iter()
        .map(|found| {
            let (pixel_width, pixel_height) = found.decoded.as_ref().map_or((0, 0), |decoded| {
                (decoded.pixels.width(), decoded.pixels.height())
            });
            ExtractedImage {
                id: found.id.clone(),
                page: found.page,
                origin: found.origin,
                bbox: found.bbox.clone(),
                transform: found.transform,
                pixel_width,
                pixel_height,
                image_sha256: found.digest(),
                // Both offsets are into a reading that does not exist yet.
                // `render` writes them, being the one place the text is built.
                reading_range: None,
                reading_anchor: None,
                ocr_regions: Vec::new(),
                description: None,
                analyzer_identity: String::new(),
                status: if found.withheld_by_scope {
                    ImageAnalysisStatus::NotRead {
                        reason: withheld_reason(found.origin),
                    }
                } else {
                    match &found.rejected {
                        Some(reason) => ImageAnalysisStatus::SkippedTechnicalLimit {
                            reason: reason.clone(),
                        },
                        None => ImageAnalysisStatus::Complete,
                    }
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
                if matches!(
                    image.status,
                    ImageAnalysisStatus::SkippedTechnicalLimit { .. }
                        | ImageAnalysisStatus::NotRead { .. }
                ) {
                    if image.origin == RegionOrigin::Embedded {
                        diagnostics.native_images_skipped_technical_limit += 1;
                    }
                } else {
                    image.status = ImageAnalysisStatus::Partial {
                        failures: vec!["no image analyzer configured".to_string()],
                    };
                }
            }
        }
    }

    debug!(
        "images: {} found, {} analyzed, {} skipped, {} not read, {} ocr ok, {} ocr failed, \
         {} regions accepted, {} rejected, {} deduplicated; \
         kinds: {} text, {} formula ({} invalid), {} table ({} malformed), \
         {} chart ({} malformed), {} code, {} not text, {} unroutable",
        diagnostics.native_images_found,
        diagnostics.native_images_analyzed,
        diagnostics.native_images_skipped_technical_limit,
        diagnostics.native_images_not_read,
        diagnostics.images_ocr_succeeded,
        diagnostics.images_ocr_failed,
        diagnostics.ocr_regions_accepted,
        diagnostics.ocr_regions_rejected_low_confidence,
        diagnostics.ocr_regions_deduplicated_against_native_text,
        diagnostics.regions_routed_text,
        diagnostics.regions_routed_formula,
        diagnostics.formulas_rejected_invalid_latex,
        diagnostics.regions_routed_table,
        diagnostics.tables_rejected_malformed,
        diagnostics.regions_routed_chart,
        diagnostics.charts_rejected_malformed,
        diagnostics.regions_routed_code,
        diagnostics.regions_marked_not_text,
        diagnostics.regions_unroutable,
    );
    images
}

/// Why a picture the scope withholds was not read, in the terms the setting
/// is offered in. One function so the status and the log say the same thing.
fn withheld_reason(origin: RegionOrigin) -> String {
    match origin {
        RegionOrigin::Embedded => {
            "reading is limited to the formulas and ruled tables the page typesets".to_string()
        }
        // Unreachable through the backend, which withholds embedded rasters
        // only. Stated rather than asserted: a scope that one day withholds
        // something else should read wrong here, not panic in extraction.
        RegionOrigin::Typeset => "this configuration does not read typeset regions".to_string(),
    }
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

    /// The analyzer is process-wide state that a settings edit moves, so what
    /// matters is that it moves: a feature the user turns off must stop
    /// enriching, not stop enriching after a restart.
    #[test]
    fn the_configured_analyzer_can_be_replaced_and_detached() {
        struct Nothing(&'static str);
        impl ImageAnalyzer for Nothing {
            fn layout(&self) -> Option<&dyn LayoutModel> {
                None
            }
            fn identity(&self) -> String {
                self.0.to_string()
            }
            fn reads_embedded_images(&self) -> bool {
                true
            }
            fn analyze(
                &self,
                _images: &mut [ExtractedImage],
                _discovered: &[DiscoveredImage],
                _context: &AnalysisContext,
                _diagnostics: &mut ExtractionDiagnostics,
            ) {
            }
            fn release(&self) {}
        }

        let restore = configured();
        configure(Some(Arc::new(Nothing("first"))));
        assert_eq!(configured().map(|a| a.identity()).as_deref(), Some("first"));
        configure(Some(Arc::new(Nothing("second"))));
        assert_eq!(
            configured().map(|a| a.identity()).as_deref(),
            Some("second")
        );
        configure(None);
        assert!(configured().is_none());
        configure(restore);
    }

    /// A manager for the two tests below, neither of which reaches the
    /// worker: one returns before addressing a recognizer and the other fails
    /// the installed check first.
    #[cfg(feature = "candle")]
    fn test_manager() -> crate::worker::manager::WorkerManager {
        crate::worker::manager::WorkerManager::new(
            crate::worker::manager::WorkerPaths {
                python_path: std::path::PathBuf::new(),
                python_package_dir: std::path::PathBuf::new(),
                requirements_path: std::path::PathBuf::new(),
                venv_dir: std::path::PathBuf::new(),
                worker_bin: std::path::PathBuf::new(),
                data_dir: std::path::PathBuf::new(),
            },
            crate::worker::ipc::WorkerKind::Recognize,
        )
        .0
    }

    /// Disabled is answered without touching the disk: the settings say no
    /// enrichment, and loading a recognizer to discover that would make
    /// turning the feature off cost what turning it on costs.
    #[test]
    #[cfg(feature = "candle")]
    fn settings_that_ask_for_nothing_build_nothing() {
        let built = build_analyzer(
            test_manager(),
            std::path::Path::new("/nonexistent"),
            std::path::Path::new("/nonexistent"),
            &crate::types::ImageAnalysisSettings::default(),
            "http://localhost:11434",
        )
        .expect("disabled is not a failure");
        assert!(built.is_none());
    }

    /// Enabled without the weights is an error, never a quiet disable: a
    /// reading that silently omitted the enrichment would be
    /// indistinguishable from one that found no text in the picture.
    #[test]
    #[cfg(feature = "candle")]
    fn enabled_without_the_recognizer_is_an_error_and_not_a_silent_disable() {
        let dir = tempfile::tempdir().expect("a temporary data directory");
        let Err(error) = build_analyzer(
            test_manager(),
            dir.path(),
            dir.path(),
            &crate::types::ImageAnalysisSettings {
                enabled: true,
                ..Default::default()
            },
            "http://localhost:11434",
        ) else {
            panic!("an uninstalled recognizer is a failure");
        };
        assert!(
            format!("{error:#}").contains("recognizer"),
            "the failure should name what is missing: {error:#}"
        );
        // Of the recognizers that install anything. One that ships with the
        // operating system is present in every directory including this one,
        // and is not evidence that an install leaked into a fresh cache.
        assert!(recognizer_catalogue(dir.path())
            .models
            .iter()
            .filter(|model| model.footprint_bytes > 0)
            .all(|model| !model.is_cached));
    }

    /// FIGURE.md's acceptance criterion on identity, in one place: *model,
    /// prompt, threshold, mapping, or serialization changes alter extraction
    /// identity and force reindexing.* Each of the five, named and checked.
    #[test]
    fn every_input_that_changes_the_bytes_changes_the_recipe() {
        use crate::extract::image::describe::FigureDescriber;
        use crate::extract::image::ocr::OcrEngine;

        struct Recognizer(&'static str, f32);
        impl OcrEngine for Recognizer {
            fn identity(&self) -> String {
                format!("{}+admit-{}", self.0, self.1)
            }
            fn admission_threshold(&self) -> f32 {
                self.1
            }
            fn spot_batch(
                &self,
                images: &[image::RgbImage],
            ) -> anyhow::Result<Vec<ocr::ImageRecognition>> {
                Ok(vec![ocr::ImageRecognition::default(); images.len()])
            }
        }

        struct Describer(&'static str);
        impl FigureDescriber for Describer {
            fn identity(&self) -> String {
                self.0.to_string()
            }
            fn describe(
                &self,
                _image: &image::RgbImage,
                _ocr: &[crate::types::ImageOcrRegion],
            ) -> anyhow::Result<crate::types::ImageDescription> {
                unreachable!("identity only")
            }
        }

        /// A detector that only has to be nameable: the identity is what is
        /// under test, not the detection.
        struct Detector(&'static str);
        impl LayoutModel for Detector {
            fn input_side(&self) -> u32 {
                800
            }
            fn release(&self) {}
            fn identity(&self) -> String {
                self.0.to_string()
            }
            fn detect(&self, _page: &image::RgbImage) -> anyhow::Result<Vec<LayoutRegion>> {
                unreachable!("identity only")
            }
        }

        let analyzer = |model: &'static str, threshold: f32, prompt: &'static str| {
            NativeImageAnalyzer::new(
                Box::new(Recognizer(model, threshold)),
                Some(Box::new(Describer(prompt))),
                ImageScope::TypesetAndEmbedded,
                Some(Box::new(Detector("detector-v1"))),
            )
            .identity()
        };
        let baseline = analyzer("weights-a", 0.6, "prompt-v1");

        assert_ne!(baseline, analyzer("weights-b", 0.6, "prompt-v1"), "model");
        assert_ne!(
            baseline,
            analyzer("weights-a", 0.7, "prompt-v1"),
            "threshold"
        );
        assert_ne!(baseline, analyzer("weights-a", 0.6, "prompt-v2"), "prompt");

        // The mapping and the serialization are constants rather than
        // configuration, so what a test can hold is that the identity carries
        // them: changing either then changes it by construction, and cannot
        // be changed without this failing to be true.
        assert!(
            baseline.contains(ocr::MAPPING_VERSION),
            "mapping: {baseline}"
        );
        assert!(
            baseline.contains(serialize::SERIALIZATION_VERSION),
            "serialization: {baseline}"
        );
        assert!(baseline.contains(LIMITS_VERSION), "limits: {baseline}");
        // The per-kind admission rules decide which recognized bytes reach
        // the reading, so two readings produced under different rules are
        // different readings even when one model read one picture.
        assert!(
            baseline.contains(ocr::ADMISSION_RULES_VERSION),
            "admission rules: {baseline}"
        );

        // And configuring a describer at all is a different reading from not.
        assert_ne!(
            baseline,
            NativeImageAnalyzer::new(
                Box::new(Recognizer("weights-a", 0.6)),
                None,
                ImageScope::TypesetAndEmbedded,
                Some(Box::new(Detector("detector-v1"))),
            )
            .identity(),
        );

        // So is withholding the embedded rasters. This is the whole safety of
        // the setting: a library read under one scope and a library read under
        // the other are different readings, and the recipe has to say so or
        // the two would be mixed in one index.
        let scoped = |scope| {
            NativeImageAnalyzer::new(
                Box::new(Recognizer("weights-a", 0.6)),
                Some(Box::new(Describer("prompt-v1"))),
                scope,
                Some(Box::new(Detector("detector-v1"))),
            )
            .identity()
        };
        assert_ne!(
            scoped(ImageScope::TypesetOnly),
            scoped(ImageScope::TypesetAndEmbedded),
            "scope"
        );

        // And so is the detector. It decides which areas of a page are read
        // at all, so two libraries routed by different detectors hold
        // different readings and the recipe has to say so.
        let detected = |detector: Option<&'static str>| {
            NativeImageAnalyzer::new(
                Box::new(Recognizer("weights-a", 0.6)),
                Some(Box::new(Describer("prompt-v1"))),
                ImageScope::TypesetAndEmbedded,
                detector.map(|name| Box::new(Detector(name)) as Box<dyn LayoutModel>),
            )
            .identity()
        };
        assert_ne!(detected(Some("detector-v1")), detected(Some("detector-v2")));
        assert_ne!(detected(Some("detector-v1")), detected(None), "no detector");
    }
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
        let rgb =
            decode(2, 2, 3, 6, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]).expect("rgb decodes");
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
        let padded = decode(
            2,
            2,
            3,
            8,
            &[1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0],
        )
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
