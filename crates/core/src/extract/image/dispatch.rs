//! What recognizers exist, and the one place that says so.
//!
//! Recognition is dispatched the way embedding and generation are: a role
//! carries an engine, the engine plus a model id names a recognizer, and the
//! worker subprocess loads it without knowing which one it is going to get.
//! Nothing here mentions a checkpoint by name outside the arm that owns it, so
//! a second engine — a Python recognizer behind the sidecar, say — is a
//! variant and two match arms rather than a new path through the code.
//!
//! The split between what needs the weights and what does not is the point of
//! this module, and it is the invariant of the whole image-analysis path:
//! **no model in here executes in the host process.** `load_recognizer_local`
//! is named `_local` because it loads into whatever process calls it, and the
//! only caller allowed to is the recognition worker. `identity`,
//! `inventory`, `installed` and `admission_threshold` are derived from
//! constants, are needed by the host to write the extraction recipe, and must
//! never require a model to answer — the recipe describes what the reading was
//! produced under, and the process that owns the recipe is not the process
//! holding the weights.
//!
//! The reason is the kill path, not tidiness. Reading a corpus is hours of
//! inference, and the only way to stop inference that has stopped making
//! progress is to kill the process running it. A model loaded in the host is
//! one the user can only stop by quitting the application.

use std::path::Path;

use super::ocr::OcrEngine;

/// A way of recognizing the text drawn inside an image.
///
/// One variant today. It is an enum rather than an implied constant because
/// the worker protocol carries it, and a protocol that cannot name a second
/// engine is one that has to change shape to gain one.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum RecognitionEngine {
    /// ONNX Runtime, in the recognition worker. The consumer-facing default,
    /// the way `EmbeddingEngine::Fastembed` is for embedding: the smaller
    /// model that reads a whole page in one pass.
    #[default]
    #[serde(alias = "onnx")]
    Onnx,
    /// PaddleOCR-VL under candle, in the recognition worker.
    #[serde(alias = "candle")]
    Candle,
    /// Apple's Vision framework, in the recognition worker. macOS only, and
    /// deliberately not the default: it reads lines of text two orders of
    /// magnitude faster than either model and emits nothing else — no
    /// formulas, no tables, no figure regions. Choosing it trades the
    /// structure away, which is a decision and not a speed setting.
    #[serde(alias = "vision")]
    Vision,
}

impl RecognitionEngine {
    pub fn as_str(&self) -> &'static str {
        match self {
            RecognitionEngine::Onnx => "onnx",
            RecognitionEngine::Candle => "candle",
            RecognitionEngine::Vision => "vision",
        }
    }

    /// The model this engine reads with unless told otherwise.
    pub fn default_model(&self) -> &'static str {
        match self {
            RecognitionEngine::Onnx => super::granite_docling::MODEL_ID,
            RecognitionEngine::Candle => {
                #[cfg(feature = "candle")]
                {
                    super::paddleocr_vl::SHIPPED_CHECKPOINT.name
                }
                #[cfg(not(feature = "candle"))]
                {
                    ""
                }
            }
            RecognitionEngine::Vision => {
                #[cfg(all(feature = "recognize-vision", target_os = "macos"))]
                {
                    super::vision::MODEL_ID
                }
                #[cfg(not(all(feature = "recognize-vision", target_os = "macos")))]
                {
                    ""
                }
            }
        }
    }

    /// Every engine this build can actually recognize with.
    pub fn supported_engines() -> Vec<Self> {
        let mut engines = Vec::new();
        #[cfg(feature = "recognize-onnx")]
        engines.push(RecognitionEngine::Onnx);
        #[cfg(feature = "candle")]
        engines.push(RecognitionEngine::Candle);
        #[cfg(all(feature = "recognize-vision", target_os = "macos"))]
        engines.push(RecognitionEngine::Vision);
        engines
    }
}

/// The model id the shipped recipe names for `engine`.
pub fn shipped_model_id(engine: RecognitionEngine) -> &'static str {
    engine.default_model()
}

/// What a recognizer is for.
///
/// Every role is dispatched by engine and model id, is installed the same way
/// and is named the same way, so they belong in one catalogue. They are not
/// interchangeable: a page reader handed a crop of one expression comes apart,
/// a formula reader emits one whole-crop region and could not read a page, and
/// a table reader emits no text at all. So the role travels with the
/// descriptor, and a picker that offers page readers filters on it rather than
/// keeping its own list of which ids are which.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecognizerRole {
    /// Reads a whole page or a whole picture: prose, and whatever else its
    /// task configuration covers.
    Page,
    /// Reads one cropped expression and returns its LaTeX. Spent only on the
    /// areas the layout detector marked out as formulas.
    Formula,
    /// Reads the *grid* of one cropped table and returns geometry: cells with
    /// a row, a column, their spans, and a box. It transcribes nothing — the
    /// cells' text is taken from the page's own glyphs by the host — so it is
    /// not an [`OcrEngine`] at all, and it is in this catalogue because it is
    /// installed, offered, versioned and spent exactly like one.
    ///
    /// Spent only on the areas the detector marked out as tables, and only for
    /// the areas a page *typesets*: there is no text layer under an embedded
    /// raster to fill a grid from. Charts go to the page reader as they always
    /// have.
    Table,
}

/// One recognizer this build can read with, described rather than listed.
///
/// [`list_models`] answers what to put in a picker; this answers what choosing
/// one would *mean*. The alternative is each consumer keeping its own table of
/// model facts, which is how a threshold typed into a form ends up outranking
/// the engine that produced the reading.
#[derive(Clone, Debug, serde::Serialize)]
pub struct RecognizerDescriptor {
    pub engine: RecognitionEngine,
    pub model_id: String,
    /// Which of the two reading jobs this model does. Decides where it is
    /// offered and what it is spent on; never inferred from the model id.
    pub role: RecognizerRole,
    pub display_name: String,
    pub description: String,
    /// The recognizer a fresh install reads with — one across the catalogue.
    pub is_default: bool,
    /// The recognizer this *engine* reads with unless told otherwise. What a
    /// picker selects when the engine is switched, and what an
    /// `ImageAnalysisSettings::model` of `None` resolves to. Derived here
    /// rather than restated by each consumer, because a consumer's own copy of
    /// it is how a library gets read under a recognizer nobody chose.
    pub is_engine_default: bool,
    pub is_cached: bool,
    pub footprint_bytes: u64,
    /// The confidence a region must reach to enter the reading. Per model,
    /// because it is an operating point over one decoder's token
    /// distribution and means nothing carried across to another.
    pub admission_threshold: f32,
    /// The region kinds this recognizer produces *under its shipped task
    /// configuration*. Not a claim about the weights: PaddleOCR-VL parses
    /// tables and formulas too, behind task prompts Wilkes does not drive.
    pub emits: Vec<super::ocr::RegionKind>,
}

/// Everything this build can recognize with, as one answer.
///
/// The recognition counterpart of [`crate::types::EmbedderCapabilityManifest`],
/// and it carries the engine list for the same reason that one does: "this
/// build has no PaddleOCR-VL" and "PaddleOCR-VL offers no models" are
/// different answers, and only the first one may be shown as an engine the
/// user cannot choose. Deriving the engines from the models collapses them.
///
/// `models` holds every recognizer, page readers and the formula reader
/// alike, each carrying its [`RecognizerRole`]. The formula reader used to sit
/// in a field of its own here, which made a second model of the same kind a
/// second field rather than a second row.
#[derive(Clone, Debug, serde::Serialize)]
pub struct RecognizerCatalogue {
    pub engines: Vec<RecognitionEngine>,
    pub models: Vec<RecognizerDescriptor>,
    /// The layout detector, which is not a recognizer but is the other half of
    /// reading a document: it decides which areas a recognizer is spent on.
    /// In the same answer because a picker that offered the recognizer and
    /// left the detector to a second call would let a user finish configuring
    /// and still read no mathematics.
    ///
    /// `None` when this build has no detector compiled in.
    pub detector: Option<InstallableModelStatus>,
}

/// One model a picker can offer: what it is, and whether it is
/// here.
///
/// Serialize only, like the inventory it carries: this is an answer the host
/// produces, and nothing reads one back.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InstallableModelStatus {
    pub inventory: crate::types::RecognizerInventory,
    pub is_installed: bool,
}

/// Everything this build can recognize with, default first, then cached.
pub fn list_models(model_dir: &Path) -> Vec<RecognizerDescriptor> {
    use super::ocr::RegionKind;
    let mut models: Vec<RecognizerDescriptor> = Vec::new();

    #[cfg(feature = "recognize-onnx")]
    models.push(RecognizerDescriptor {
        engine: RecognitionEngine::Onnx,
        model_id: super::granite_docling::MODEL_ID.to_string(),
        role: RecognizerRole::Page,
        display_name: "Granite-Docling 258M".to_string(),
        description: "Reads a page in one pass: prose, headings, LaTeX formulas and \
                      tables. Smaller and broader than PaddleOCR-VL."
            .to_string(),
        is_default: true,
        is_engine_default: super::granite_docling::MODEL_ID
            == RecognitionEngine::Onnx.default_model(),
        is_cached: super::granite_docling::is_installed(model_dir),
        footprint_bytes: super::granite_docling::footprint_bytes(),
        admission_threshold: super::granite_docling::ADMISSION_THRESHOLD,
        emits: vec![
            RegionKind::Text,
            RegionKind::Formula,
            RegionKind::Table,
            RegionKind::Chart,
            RegionKind::Code,
        ],
    });

    // A row here rather than a slot of its own. It is an `OcrEngine` under the
    // same runtime as the page reader above, installed the same way and named
    // the same way; what makes it different is its role, and the role is a
    // field. `is_default` is false because nothing defaults to reading pages
    // with it — the picker filters to `RecognizerRole::Page`.
    #[cfg(feature = "recognize-onnx")]
    models.push(RecognizerDescriptor {
        engine: RecognitionEngine::Onnx,
        model_id: super::texify::MODEL_ID.to_string(),
        role: RecognizerRole::Formula,
        display_name: "Texify".to_string(),
        description: "Reads one cropped expression back as LaTeX. Spent only on the \
                      areas the layout detector marks out as formulas, which a page \
                      reader cannot read."
            .to_string(),
        is_default: false,
        is_engine_default: false,
        is_cached: super::texify::is_installed(model_dir),
        footprint_bytes: super::texify::footprint_bytes(),
        admission_threshold: super::texify::ADMISSION_THRESHOLD,
        emits: vec![RegionKind::Formula],
    });

    // The third role, and a row here for the same reason the formula reader is
    // one: it is installed, pinned, versioned and spent like the others, and
    // what makes it different is its role. `is_default` is false because
    // nothing defaults to reading pages with it — the picker filters to
    // `RecognizerRole::Page`.
    #[cfg(feature = "recognize-onnx")]
    models.push(RecognizerDescriptor {
        engine: RecognitionEngine::Onnx,
        model_id: super::table_structure::MODEL_ID.to_string(),
        role: RecognizerRole::Table,
        display_name: "SLANet-plus".to_string(),
        description: "Reads the grid of a ruled table the page typesets — rows, columns \
                      and merged cells — and the cells are then filled from the page's own \
                      text. Nothing transcribes glyphs the document already holds."
            .to_string(),
        is_default: false,
        is_engine_default: false,
        is_cached: super::table_structure::is_installed(model_dir),
        footprint_bytes: super::table_structure::footprint_bytes(),
        admission_threshold: super::table_structure::ADMISSION_THRESHOLD,
        emits: vec![RegionKind::Table],
    });

    #[cfg(feature = "candle")]
    for checkpoint in super::paddleocr_vl::CHECKPOINTS {
        models.push(RecognizerDescriptor {
            engine: RecognitionEngine::Candle,
            model_id: checkpoint.name.to_string(),
            role: RecognizerRole::Page,
            display_name: format!("PaddleOCR-VL {}", checkpoint.name),
            description: "Transcribes the text drawn inside a picture, with precise \
                          per-region geometry."
                .to_string(),
            is_default: false,
            is_engine_default: checkpoint.name == RecognitionEngine::Candle.default_model(),
            is_cached: super::paddleocr_vl::is_installed(model_dir, checkpoint),
            footprint_bytes: checkpoint.footprint_bytes(),
            admission_threshold: super::paddleocr_vl::ADMISSION_THRESHOLD,
            emits: vec![RegionKind::Text],
        });
    }

    #[cfg(all(feature = "recognize-vision", target_os = "macos"))]
    models.push(RecognizerDescriptor {
        engine: RecognitionEngine::Vision,
        model_id: super::vision::MODEL_ID.to_string(),
        role: RecognizerRole::Page,
        display_name: "Apple Vision".to_string(),
        description: "Reads lines of text about a hundred times faster than the \
                      models, using the recognizer built into macOS. Prose only: \
                      no formulas, no tables, nothing to download."
            .to_string(),
        is_default: false,
        is_engine_default: super::vision::MODEL_ID == RecognitionEngine::Vision.default_model(),
        // Part of the operating system, so there is nothing to install and
        // nothing an uninstall could take away.
        is_cached: super::vision::is_installed(),
        footprint_bytes: super::vision::footprint_bytes(),
        admission_threshold: super::vision::ADMISSION_THRESHOLD,
        emits: vec![RegionKind::Text],
    });

    let _ = model_dir;
    models.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then(b.is_cached.cmp(&a.is_cached))
            .then(a.model_id.cmp(&b.model_id))
    });
    models
}

/// Resolve `(engine, model_id)` to its catalogue entry.
///
/// The one place that says what a model id means. `paddleocr_vl` used to
/// answer this for every engine, which was invisible while it was the only
/// one and wrong the moment it was not.
fn descriptor(
    engine: RecognitionEngine,
    model_id: &str,
    model_dir: &Path,
) -> anyhow::Result<RecognizerDescriptor> {
    list_models(model_dir)
        .into_iter()
        .find(|m| m.engine == engine && m.model_id == model_id)
        .ok_or_else(|| {
            let known: Vec<String> = list_models(model_dir)
                .iter()
                .map(|m| format!("{}/{}", m.engine.as_str(), m.model_id))
                .collect();
            anyhow::anyhow!(
                "unknown recognizer '{}/{model_id}'; this build ships {}",
                engine.as_str(),
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            )
        })
}

/// The recipe string a recognizer of this engine and model reads documents
/// under, answered without loading it.
pub fn identity(engine: RecognitionEngine, model_id: &str) -> anyhow::Result<String> {
    match engine {
        #[cfg(feature = "recognize-onnx")]
        RecognitionEngine::Onnx => match model_id {
            super::granite_docling::MODEL_ID => Ok(super::granite_docling::identity()),
            super::texify::MODEL_ID => Ok(super::texify::identity()),
            super::table_structure::MODEL_ID => Ok(super::table_structure::identity()),
            other => anyhow::bail!("unknown onnx recognizer '{other}'"),
        },
        #[cfg(not(feature = "recognize-onnx"))]
        RecognitionEngine::Onnx => {
            let _ = model_id;
            anyhow::bail!("the onnx recognizer is not compiled into this build")
        }
        #[cfg(feature = "candle")]
        RecognitionEngine::Candle => Ok(super::paddleocr_vl::identity_of(&checkpoint(model_id)?)),
        #[cfg(not(feature = "candle"))]
        RecognitionEngine::Candle => {
            let _ = model_id;
            anyhow::bail!("the candle recognizer is not compiled into this build")
        }
        #[cfg(all(feature = "recognize-vision", target_os = "macos"))]
        RecognitionEngine::Vision => {
            anyhow::ensure!(
                model_id == super::vision::MODEL_ID,
                "unknown vision recognizer '{model_id}'"
            );
            Ok(super::vision::identity())
        }
        #[cfg(not(all(feature = "recognize-vision", target_os = "macos")))]
        RecognitionEngine::Vision => {
            let _ = model_id;
            anyhow::bail!("the vision recognizer is not compiled into this build")
        }
    }
}

/// The confidence a region of this engine's output must reach to enter the
/// reading. Part of the identity above, and needed by the host for the same
/// reason: admission happens where the geometry does.
pub fn admission_threshold(engine: RecognitionEngine, model_id: &str) -> anyhow::Result<f32> {
    // Per model, from the catalogue. It used to be a constant inside
    // `paddleocr_vl`, which made a second model's operating point look like a
    // property of recognition itself.
    Ok(descriptor(engine, model_id, Path::new(""))?.admission_threshold)
}

/// Whether this engine's weights for `model_id` are on disk and intact.
///
/// Asked by the host before it attaches an analyzer, not by the worker before
/// it loads one. Attaching is now cheap and no longer discovers a missing
/// checkpoint by failing to load it, so without this a library would be read
/// under a recipe that claims enrichment while every image quietly failed —
/// which is the exact outcome the recipe exists to make impossible.
pub fn installed(
    engine: RecognitionEngine,
    model_id: &str,
    model_dir: &Path,
) -> anyhow::Result<bool> {
    Ok(descriptor(engine, model_id, model_dir)?.is_cached)
}

/// What `(engine, model_id)` is for: reading pages, or reading formulas.
///
/// From the catalogue, so the answer is the descriptor's own and not a second
/// table of which ids are which. Asked before a recognizer is attached, which
/// is where a setting naming the wrong kind of model has to be caught.
pub fn role(
    engine: RecognitionEngine,
    model_id: &str,
    model_dir: &Path,
) -> anyhow::Result<RecognizerRole> {
    Ok(descriptor(engine, model_id, model_dir)?.role)
}

/// Load the layout detector in the calling process.
///
/// Must only be called from a worker subprocess, exactly like
/// [`load_recognizer_local`] and for exactly the same reason: a detector run
/// on the extraction thread is a hundred seconds of uninterruptible inference
/// for a four-hundred-page book. The host reaches it through
/// [`super::worker_layout::attach`].
///
/// The detector is not in [`list_models`] — it is a `LayoutModel` and not an
/// [`OcrEngine`], so it could never come back from `load_recognizer_local` —
/// but it is loaded through this module for the same reason everything else
/// is: nothing outside the arm that owns a checkpoint names it.
#[cfg(feature = "recognize-onnx")]
pub fn load_layout_detector_local(
    model_id: &str,
    model_dir: &Path,
) -> anyhow::Result<Box<dyn super::LayoutModel>> {
    anyhow::ensure!(
        model_id == super::doclayout::MODEL_ID,
        "unknown layout detector '{model_id}'"
    );
    // One thread short of the machine, like the page reader: the worker serves
    // one request at a time, and a box fully saturated by detection has
    // nothing left to render the next page with.
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1))
        .unwrap_or(1);
    Ok(Box::new(super::doclayout::DocLayout::load(
        model_dir, threads,
    )?))
}

/// The formula recognizer this build ships, or `None` when it ships none.
///
/// Found by role rather than by name, so the one place that knows which model
/// reads formulas is the catalogue row that declares it. A build compiled
/// without the ONNX recognizer has no such row and therefore no formula
/// reader, which is a configuration and not an error.
pub fn formula_model(model_dir: &Path) -> Option<RecognizerDescriptor> {
    list_models(model_dir)
        .into_iter()
        .find(|model| model.role == RecognizerRole::Formula)
}

/// The table structure model this build ships, or `None` when it ships none.
///
/// Found by role, exactly as [`formula_model`] is, and for the same reason: the
/// one place that knows which model reads tables is the catalogue row that
/// declares it. A build with no such row, or an installation that has not
/// downloaded it, is a configuration and not an error — the areas the detector
/// calls tables go to the page reader, which is what they did before this model
/// existed. See [`super::NativeImageAnalyzer::route`].
pub fn table_model(model_dir: &Path) -> Option<RecognizerDescriptor> {
    list_models(model_dir)
        .into_iter()
        .find(|model| model.role == RecognizerRole::Table)
}

/// Load the table structure model in the calling process.
///
/// Must only be called from a worker subprocess, exactly like
/// [`load_recognizer_local`] and [`load_layout_detector_local`], and for
/// exactly the same reason. The host reaches it through
/// [`super::table_structure::attach`].
///
/// It is not in the same loader as the recognizers because it is not an
/// [`OcrEngine`]: it answers a grid, not text. It is in this module for the
/// reason everything else is — nothing outside the arm that owns a checkpoint
/// names it.
#[cfg(feature = "recognize-onnx")]
pub fn load_table_structure_local(
    model_id: &str,
    model_dir: &Path,
) -> anyhow::Result<Box<dyn super::table_structure::TableStructure>> {
    anyhow::ensure!(
        model_id == super::table_structure::MODEL_ID,
        "unknown table reader '{model_id}'"
    );
    let (_, threads) = recognizer_layout(RecognizerRole::Table, "cpu");
    Ok(Box::new(super::table_structure::SlanetPlus::load(
        model_dir, threads,
    )?))
}

/// Load the recognizer in the calling process.
///
/// Must only be called from a worker subprocess — see this module's invariant.
/// The host reaches every recognizer through [`super::worker_ocr::attach`]
/// instead, formula readers included: a fault here takes down the process it
/// is in, and a decode that stops making progress is ended by killing that
/// process. Neither is survivable in the host.
pub fn load_recognizer_local(
    engine: RecognitionEngine,
    model_id: &str,
    model_dir: &Path,
    device: &str,
) -> anyhow::Result<Box<dyn OcrEngine>> {
    match engine {
        #[cfg(feature = "recognize-onnx")]
        RecognitionEngine::Onnx => match model_id {
            super::granite_docling::MODEL_ID => {
                // ONNX Runtime is told how the work is laid out rather than
                // which device: the CPU provider is the only one this
                // recognizer was measured on, and naming a device it will not
                // honour would be a setting that reads as a promise.
                let (readers, threads) = recognizer_layout(RecognizerRole::Page, device);
                Ok(Box::new(super::granite_docling::GraniteDocling::load(
                    model_dir, readers, threads,
                )?))
            }
            // Laid out on the same principle as the page reader and not on
            // the same numbers: a formula crop's decode waits on memory rather
            // than on arithmetic too, so the machine one reader leaves idle is
            // used by more readers and not by wider ones — but a formula
            // reader is a fifth of a page reader's footprint, so the count
            // where that stops paying is a different count. This arm used to
            // take the threads and drop the reader count on the floor, which
            // made every document's crops one serial queue; see
            // [`recognizer_layout`]'s second table for what that cost.
            super::texify::MODEL_ID => {
                let (readers, threads) = recognizer_layout(RecognizerRole::Formula, device);
                Ok(Box::new(super::texify::Texify::load(
                    model_dir, readers, threads,
                )?))
            }
            other => anyhow::bail!("unknown onnx recognizer '{other}'"),
        },
        #[cfg(not(feature = "recognize-onnx"))]
        RecognitionEngine::Onnx => {
            let (_, _, _) = (model_id, model_dir, device);
            anyhow::bail!("the onnx recognizer is not compiled into this build")
        }
        #[cfg(feature = "candle")]
        RecognitionEngine::Candle => Ok(Box::new(super::paddleocr_vl::PaddleOcrVl::load(
            model_dir,
            checkpoint(model_id)?,
            device,
        )?)),
        #[cfg(not(feature = "candle"))]
        RecognitionEngine::Candle => {
            let (_, _, _) = (model_id, model_dir, device);
            anyhow::bail!("the candle recognizer is not compiled into this build")
        }
        #[cfg(all(feature = "recognize-vision", target_os = "macos"))]
        RecognitionEngine::Vision => {
            anyhow::ensure!(
                model_id == super::vision::MODEL_ID,
                "unknown vision recognizer '{model_id}'"
            );
            // No model directory and no device: the recognizer is the operating
            // system's and picks its own hardware. Naming either would be a
            // setting Wilkes cannot honour.
            let (_, _) = (model_dir, device);
            Ok(Box::new(super::vision::AppleVision::load()?))
        }
        #[cfg(not(all(feature = "recognize-vision", target_os = "macos")))]
        RecognitionEngine::Vision => {
            let (_, _, _) = (model_id, model_dir, device);
            anyhow::bail!("the vision recognizer is not compiled into this build")
        }
    }
}

/// How an ONNX recognizer's work is laid out: how many images it reads at
/// once, and how many threads each of those readers gets.
///
/// Still one less than the machine has in total, so an indexing pass that runs
/// for an hour does not take the interface down with it. What changed is how
/// that budget is spent. Giving it all to one reader spends most of it on a
/// decode that cannot use it: each step is a pass over the whole decoder's
/// weights, so it waits on memory rather than on arithmetic, and a page read
/// with nine threads produces a token in 13.8ms against 15.3ms with one.
/// Nine-tenths of the machine sits idle whatever the thread count is.
///
/// Reading several images at once uses some of that idle capacity. Not all of
/// it: the readers contend for the same memory bandwidth the single reader was
/// already waiting on, so the gain is real but bounded.
///
/// ## The page reader
///
/// Six pages of a text page through the whole engine, on a 10-core M4:
///
/// | layout | elapsed | peak |
/// |--------|---------|------|
/// | 1 x 9  | 47.3s   | 2.9 GB |
/// | 2 x 4  | 41.7s   | 5.6 GB |
/// | 3 x 2  | 40.5s   | 8.7 GB |
/// | 4 x 2  | 41.3s   | 11.0 GB |
///
/// Two readers is the knee. The second buys 13% for 2.7 GB; the third buys a
/// further 3% for 2.8 GB, and the fourth is slower than the third while
/// costing more again. Two also keeps the pool's peak near the 4.9 GB a single
/// reader needed before its tiles were grouped, which is the footprint this
/// recognizer has already been shown to live in.
///
/// Four threads because a reader keeps improving up to it — two readers of two
/// threads take 46.1s, of three 42.2s, of four 41.7s — the page's vision
/// encoding and prefill being the parts that still scale where the decode does
/// not.
///
/// ## The formula reader
///
/// Not the same numbers, and this function used to give it the page reader's
/// and then throw the reader count away: `load_recognizer_local` took the
/// threads, ignored the readers, and every document's crops went through one
/// reader in a single queue. What that cost, over the 124 crops the five
/// heaviest pages of the `formula_recall` fixture produce, handed to
/// [`super::texify::Texify`] in one call on the same 10-core M4:
///
/// | layout | wall | ms/crop | ms/page | cores | peak |
/// |--------|------|---------|---------|-------|------|
/// | 1 x 4  | 31.5s | 254 | 6296 | 3.14 | 1.3 GB |
/// | 2 x 4  | 21.0s | 170 | 4204 | 5.94 | 2.3 GB |
/// | 3 x 4  | 17.8s | 144 | 3568 | 8.61 | 3.3 GB |
/// | 3 x 3  | 18.6s | 150 | 3727 | 7.37 | 3.3 GB |
///
/// A crop is a 543 MB reader and a page is a 2.9 GB one, so the memory knee
/// that stopped the page reader at two is nowhere near here: three formula
/// readers cost 3.3 GB, which is less than *one* page reader and two. That is
/// the whole of why the two roles are laid out differently, and it is a fact
/// about the checkpoints rather than about recognition, so it is stated here
/// and not discovered again at each call site.
///
/// Three readers of three threads and not three of four, though the latter is
/// 4.5% quicker: four apiece is twelve threads against a budget of nine, and
/// at 8.61 effective cores of ten it spends the margin this function exists to
/// keep. Three of three is exactly the budget, is 41% quicker than the single
/// reader production was actually running, and leaves 2.6 cores.
///
/// `pub` so a probe that claims to read a document under the production
/// configuration runs the numbers production runs rather than a pair typed
/// beside them. Nothing under `src/` outside this module calls it.
#[cfg(feature = "recognize-onnx")]
pub fn recognizer_layout(role: RecognizerRole, _device: &str) -> (usize, usize) {
    // Per role, because the two checkpoints have their knees in different
    // places — see the two tables above. Not per model id: what decides this
    // is what the model is *for*, and a second formula reader would want the
    // formula reader's shape rather than a row of its own.
    let (threads_per_reader, max_readers) = match role {
        RecognizerRole::Page => (4, 2),
        RecognizerRole::Formula => (3, 3),
        // One reader, at the thread count the 56 crops of the measured
        // textbook were timed at. There is no second reader here because there
        // is nothing for it to save: a crop is 23 ms, a whole book's tables are
        // 1.3 s, and the graph is 7.4 MB against the formula reader's 543 —
        // the knee both tables above are looking for is nowhere near a model
        // this size. Four threads and not fewer because four is what was
        // measured; eight was measured too and answered in the same time, so
        // the graph does not use them either way.
        RecognizerRole::Table => (4, 1),
    };

    let budget = std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1))
        .unwrap_or(1);
    let readers = (budget / threads_per_reader).clamp(1, max_readers);
    (readers, threads_per_reader)
}

/// Keep an existing library on the recognizer that produced it.
///
/// `ImageAnalysisSettings::engine` is `#[serde(default)]`, so a settings file
/// written before the field existed deserializes as the ONNX engine. For
/// someone with analysis already on and 1.9 GB of PaddleOCR-VL on disk, that
/// is a silent recognizer swap: the recipe changes, and every document with a
/// picture in it is re-read and re-embedded without anybody asking for it.
/// `ImageAnalysisSettings`' own doc comment says that is the outcome the
/// design exists to prevent.
///
/// So a configuration that predates the field and has analysis enabled is
/// pinned to the recognizer it was actually using. A configuration with
/// analysis off has no reading to protect and takes the new default, as does
/// a fresh install.
///
/// `stored` is the raw settings object as read from disk, before defaults are
/// applied — the field's *absence* is the signal, and a value that came from
/// the default is indistinguishable from one the user chose once it is typed.
pub fn migrate_recognizer_choice(
    stored: &serde_json::Value,
    settings: &mut crate::types::ImageAnalysisSettings,
) -> bool {
    let names_engine = stored
        .get("image_analysis")
        .and_then(|v| v.get("engine"))
        .is_some();
    if names_engine || !settings.enabled {
        return false;
    }
    settings.engine = RecognitionEngine::Candle;
    settings.model = Some(RecognitionEngine::Candle.default_model().to_string());
    tracing::info!(
        "image analysis predates the recognizer setting and is enabled; pinning it to \
         {} so the library is not re-read under a recognizer nobody chose",
        settings.model.as_deref().unwrap_or("")
    );
    true
}

/// What the recognizer is, where it came from, and under what terms.
pub fn inventory(
    engine: RecognitionEngine,
    model_id: &str,
) -> anyhow::Result<crate::types::RecognizerInventory> {
    match engine {
        #[cfg(feature = "recognize-onnx")]
        RecognitionEngine::Onnx => match model_id {
            super::granite_docling::MODEL_ID => Ok(super::granite_docling::inventory()),
            super::texify::MODEL_ID => Ok(super::texify::inventory()),
            super::table_structure::MODEL_ID => Ok(super::table_structure::inventory()),
            other => anyhow::bail!("unknown onnx recognizer '{other}'"),
        },
        #[cfg(not(feature = "recognize-onnx"))]
        RecognitionEngine::Onnx => {
            let _ = model_id;
            anyhow::bail!("the onnx recognizer is not compiled into this build")
        }
        #[cfg(feature = "candle")]
        RecognitionEngine::Candle => Ok(super::paddleocr_vl::inventory(&checkpoint(model_id)?)),
        #[cfg(not(feature = "candle"))]
        RecognitionEngine::Candle => {
            let _ = model_id;
            anyhow::bail!("the candle recognizer is not compiled into this build")
        }
        #[cfg(all(feature = "recognize-vision", target_os = "macos"))]
        RecognitionEngine::Vision => {
            anyhow::ensure!(
                model_id == super::vision::MODEL_ID,
                "unknown vision recognizer '{model_id}'"
            );
            Ok(super::vision::inventory())
        }
        #[cfg(not(all(feature = "recognize-vision", target_os = "macos")))]
        RecognitionEngine::Vision => {
            let _ = model_id;
            anyhow::bail!("the vision recognizer is not compiled into this build")
        }
    }
}

/// Download and verify a recognizer's artifacts.
pub fn install(
    engine: RecognitionEngine,
    model_id: &str,
    model_dir: &Path,
    progress: Option<crate::models::progress::ProgressTx>,
) -> anyhow::Result<()> {
    match engine {
        #[cfg(feature = "recognize-onnx")]
        RecognitionEngine::Onnx => match model_id {
            super::granite_docling::MODEL_ID => {
                super::granite_docling::install(model_dir, progress)
            }
            super::texify::MODEL_ID => super::texify::install(model_dir, progress),
            super::table_structure::MODEL_ID => {
                super::table_structure::install(model_dir, progress)
            }
            other => anyhow::bail!("unknown onnx recognizer '{other}'"),
        },
        #[cfg(not(feature = "recognize-onnx"))]
        RecognitionEngine::Onnx => {
            let (_, _, _) = (model_id, model_dir, progress);
            anyhow::bail!("the onnx recognizer is not compiled into this build")
        }
        #[cfg(feature = "candle")]
        RecognitionEngine::Candle => {
            super::paddleocr_vl::install(model_dir, &checkpoint(model_id)?, progress)
        }
        #[cfg(not(feature = "candle"))]
        RecognitionEngine::Candle => {
            let (_, _, _) = (model_id, model_dir, progress);
            anyhow::bail!("the candle recognizer is not compiled into this build")
        }
        // Installable is a property of the artifacts, and this recognizer has
        // none. An error rather than a silent success: nothing should ever ask,
        // because the catalogue reports it cached, and a caller that asks
        // anyway has a bug that a no-op would hide.
        RecognitionEngine::Vision => {
            let (_, _, _) = (model_id, model_dir, progress);
            anyhow::bail!("the vision recognizer is part of macOS and cannot be installed")
        }
    }
}

/// The checkpoint a model id names, or an error naming what is on offer.
///
/// An unknown id is an error rather than a fall back to the shipped
/// checkpoint: reading a library under a recognizer nobody asked for is
/// indistinguishable, afterwards, from reading it under the one they did.
#[cfg(feature = "candle")]
fn checkpoint(model_id: &str) -> anyhow::Result<super::paddleocr_vl::Checkpoint> {
    super::paddleocr_vl::CHECKPOINTS
        .iter()
        .find(|checkpoint| checkpoint.name == model_id)
        .copied()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown recognizer '{model_id}'; this build ships {}",
                super::paddleocr_vl::CHECKPOINTS
                    .iter()
                    .map(|checkpoint| checkpoint.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ImageAnalysisSettings;

    /// The default is the ONNX engine, and its default model is catalogued.
    /// A default nobody ships is a build that cannot recognize anything.
    #[cfg(feature = "recognize-onnx")]
    #[test]
    fn the_default_engine_and_its_model_are_in_the_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        let engine = RecognitionEngine::default();
        assert_eq!(engine, RecognitionEngine::Onnx);

        let models = list_models(dir.path());
        let default = models
            .iter()
            .find(|m| m.is_default)
            .expect("something is the default");
        assert_eq!(default.engine, engine);
        assert_eq!(default.model_id, engine.default_model());
        // Default first is what a picker relies on.
        assert_eq!(models[0].model_id, default.model_id);
    }

    /// Every catalogued recognizer states what it costs, what it admits at,
    /// and what it produces — the recognition counterpart of the embedder
    /// manifest's rule that every catalogued model states its dimension.
    #[test]
    fn every_catalogued_recognizer_states_its_cost_threshold_and_kinds() {
        let dir = tempfile::tempdir().unwrap();
        for model in list_models(dir.path()) {
            // Per role, because admission means two different things. A page
            // reader admits a region on the confidence of its own decode, so
            // its threshold is an operating point and must be one — a page
            // reader admitting at zero would put every hallucinated region
            // into the reading. A formula reader admits on whether the LaTeX
            // parses, which is a property of the answer and not a score, so it
            // declares zero and the assertion here is that it declares
            // *exactly* zero rather than a number nothing consults.
            match model.role {
                RecognizerRole::Page => assert!(
                    model.admission_threshold > 0.0 && model.admission_threshold < 1.0,
                    "the page reader {} admits at {}",
                    model.model_id,
                    model.admission_threshold
                ),
                // A formula is admitted on whether its LaTeX parses and a table
                // on whether its grid holds the page's glyphs. Both are
                // properties of the answer rather than scores, so both declare
                // zero, and the assertion is that they declare *exactly* zero
                // rather than a number nothing consults.
                RecognizerRole::Formula | RecognizerRole::Table => assert_eq!(
                    model.admission_threshold, 0.0,
                    "{} declares a score threshold, but its kind is not admitted on one",
                    model.model_id
                ),
            }
            assert!(!model.emits.is_empty(), "{} emits nothing", model.model_id);
            // A recognizer either downloads artifacts — so it states their size
            // and is absent from a directory nothing has been installed into —
            // or it ships with the operating system, so it has no footprint and
            // is always present. Those are the only two shapes, and a
            // recognizer that claimed a footprint while needing no download
            // would put a number in front of the user that nothing will ever
            // fetch.
            if model.footprint_bytes > 0 {
                assert!(
                    !model.is_cached,
                    "{} has artifacts, so a fresh directory cannot hold them",
                    model.model_id
                );
            } else {
                assert!(
                    model.is_cached,
                    "{} states no footprint, so it must need no installing",
                    model.model_id
                );
            }
        }
    }

    /// Every engine the build compiled in offers exactly one default model,
    /// and it is the one `default_model` names. A picker resolves an absent
    /// `ImageAnalysisSettings::model` through this, and switching engine
    /// selects through it: an engine with none would leave the picker with
    /// nothing to select, and one with two would let the choice depend on the
    /// order the catalogue happened to be built in.
    #[test]
    fn every_supported_engine_offers_exactly_one_default_model() {
        let dir = tempfile::tempdir().unwrap();
        let catalogue = super::super::recognizer_catalogue(dir.path());
        for engine in &catalogue.engines {
            let defaults: Vec<&RecognizerDescriptor> = catalogue
                .models
                .iter()
                .filter(|model| model.engine == *engine && model.is_engine_default)
                .collect();
            assert_eq!(
                defaults.len(),
                1,
                "{} offers {} default models",
                engine.as_str(),
                defaults.len()
            );
            assert_eq!(defaults[0].model_id, engine.default_model());
        }
        assert_eq!(
            catalogue.engines,
            RecognitionEngine::supported_engines(),
            "the catalogue reports the build's engines, not the ones its models happen to name"
        );
    }

    /// The two recognizers do not read documents under one recipe.
    #[cfg(all(feature = "recognize-onnx", feature = "candle"))]
    #[test]
    fn the_two_engines_read_documents_under_two_recipes() {
        let onnx = identity(
            RecognitionEngine::Onnx,
            super::super::granite_docling::MODEL_ID,
        )
        .unwrap();
        let candle = identity(
            RecognitionEngine::Candle,
            RecognitionEngine::Candle.default_model(),
        )
        .unwrap();
        assert_ne!(onnx, candle);
        assert_ne!(
            admission_threshold(
                RecognitionEngine::Onnx,
                super::super::granite_docling::MODEL_ID
            )
            .unwrap(),
            admission_threshold(
                RecognitionEngine::Candle,
                RecognitionEngine::Candle.default_model()
            )
            .unwrap(),
            "two decoders' operating points are not the same number by coincidence"
        );
    }

    /// A settings file written before the engine field existed, with analysis
    /// on, keeps the recognizer that produced its library. Without this the
    /// serde default silently swaps the recognizer, changes the recipe, and
    /// re-reads every document with a picture in it.
    #[cfg(feature = "candle")]
    #[test]
    fn an_older_enabled_configuration_keeps_the_recognizer_that_read_it() {
        let stored = serde_json::json!({
            "image_analysis": { "enabled": true, "describer_model": "" }
        });
        let mut settings = ImageAnalysisSettings {
            enabled: true,
            ..Default::default()
        };
        assert_eq!(
            settings.engine,
            RecognitionEngine::Onnx,
            "the fixture starts where serde would leave it"
        );

        assert!(migrate_recognizer_choice(&stored, &mut settings));
        assert_eq!(settings.engine, RecognitionEngine::Candle);
        assert_eq!(
            settings.model.as_deref(),
            Some(RecognitionEngine::Candle.default_model())
        );
    }

    /// A configuration with analysis off has no reading to protect, and a
    /// configuration that names an engine already said what it wants.
    #[test]
    fn a_disabled_or_explicit_configuration_is_left_alone() {
        let mut off = ImageAnalysisSettings {
            enabled: false,
            ..Default::default()
        };
        assert!(!migrate_recognizer_choice(
            &serde_json::json!({ "image_analysis": { "enabled": false } }),
            &mut off
        ));
        assert_eq!(off.engine, RecognitionEngine::Onnx);

        let mut explicit = ImageAnalysisSettings {
            enabled: true,
            engine: RecognitionEngine::Onnx,
            ..Default::default()
        };
        assert!(!migrate_recognizer_choice(
            &serde_json::json!({
                "image_analysis": { "enabled": true, "engine": "onnx" }
            }),
            &mut explicit
        ));
        assert_eq!(explicit.engine, RecognitionEngine::Onnx);
    }

    /// Both roles get more than one reader on a machine with the cores for
    /// them, and each gets its own shape. The formula reader's count is the
    /// one this asserts hardest: it was computed and then discarded at the
    /// call site, so every document's crops went through one reader, and a
    /// layout function that answers correctly while nobody uses the answer is
    /// exactly what that looked like.
    #[cfg(feature = "recognize-onnx")]
    #[test]
    fn each_role_is_laid_out_for_its_own_footprint() {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let (page_readers, page_threads) = recognizer_layout(RecognizerRole::Page, "cpu");
        let (formula_readers, formula_threads) = recognizer_layout(RecognizerRole::Formula, "cpu");

        // Whatever the machine, a reader is a reader and the budget is never
        // spent below one.
        assert!(page_readers >= 1 && formula_readers >= 1);
        assert!(page_threads >= 1 && formula_threads >= 1);
        // And it is never spent above it: the interface keeps a thread.
        let budget = cores.saturating_sub(1).max(1);
        assert!(
            page_readers * page_threads <= budget.max(page_threads),
            "the page reader wants {page_readers}x{page_threads} of a {budget}-thread budget"
        );
        assert!(
            formula_readers * formula_threads <= budget.max(formula_threads),
            "the formula reader wants {formula_readers}x{formula_threads} of a \
             {budget}-thread budget"
        );

        // The roles are laid out differently, which is the whole reason this
        // function takes a role. On a machine too small to run more than one
        // reader of either they collapse to the same answer, and that is not a
        // failure — so the claim is made only where there is room.
        if budget >= 9 {
            assert_eq!((page_readers, page_threads), (2, 4));
            assert_eq!(
                (formula_readers, formula_threads),
                (3, 3),
                "the formula reader reads three crops at once; one is what the bug was"
            );
            // One reader on purpose, and asserted so a copy of the formula
            // reader's shape cannot drift in here: a 7.4 MB graph at 23 ms a
            // crop has nothing for a second copy to save.
            assert_eq!(
                recognizer_layout(RecognizerRole::Table, "cpu"),
                (1, 4),
                "the table reader is one reader; its whole cost is a second a book"
            );
        }
    }

    /// The table reader is a row of this catalogue like the others: it answers
    /// identity, inventory and installedness from constants, with no graph
    /// loaded and nothing on disk, and it is found by role rather than by name.
    #[cfg(feature = "recognize-onnx")]
    #[test]
    fn the_table_reader_is_a_catalogue_row_answerable_without_its_graph() {
        let dir = tempfile::tempdir().unwrap();
        let model = table_model(dir.path()).expect("this build ships a table structure model");
        assert_eq!(model.role, RecognizerRole::Table);
        assert_eq!(model.model_id, super::super::table_structure::MODEL_ID);
        assert_eq!(model.emits, vec![super::super::ocr::RegionKind::Table]);
        // Nothing defaults to reading pages with it, and the picker filters to
        // `Page`: a build where this were true would offer a model that emits
        // no prose as the page recognizer.
        assert!(!model.is_default && !model.is_engine_default);
        assert!(!model.is_cached, "a fresh directory holds no graph");

        let id = identity(model.engine, &model.model_id).unwrap();
        assert_eq!(id, super::super::table_structure::identity());
        assert!(id.contains(super::super::table_structure::MODEL_ID), "{id}");
        assert_eq!(
            inventory(model.engine, &model.model_id).unwrap().name,
            super::super::table_structure::MODEL_ID
        );
        assert!(!installed(model.engine, &model.model_id, dir.path()).unwrap());

        // And once the file the recipe names is where the recipe names it.
        let install_dir = super::super::table_structure::install_dir(dir.path());
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::write(
            install_dir.join(super::super::table_structure::GRAPH),
            b"a graph",
        )
        .unwrap();
        assert!(installed(model.engine, &model.model_id, dir.path()).unwrap());
        assert!(table_model(dir.path()).unwrap().is_cached);
    }

    /// Each role is offered exactly once and none of them is offered as
    /// another. The picker filters on this, so a table reader that answered to
    /// `Page` would be selectable as the page recognizer and would read every
    /// page of the library as an empty grid.
    #[cfg(feature = "recognize-onnx")]
    #[test]
    fn the_three_roles_are_offered_apart() {
        let dir = tempfile::tempdir().unwrap();
        let models = list_models(dir.path());
        for role in [RecognizerRole::Formula, RecognizerRole::Table] {
            assert_eq!(
                models.iter().filter(|model| model.role == role).count(),
                1,
                "{role:?} is not offered exactly once"
            );
        }
        assert!(
            models
                .iter()
                .any(|model| model.role == RecognizerRole::Page && model.is_default),
            "the default recognizer reads pages"
        );
        assert_ne!(
            formula_model(dir.path()).unwrap().model_id,
            table_model(dir.path()).unwrap().model_id
        );
    }

    /// An unknown model id is refused rather than answered with the default,
    /// which would put a reading into the index under a recipe that never
    /// produced it.
    #[test]
    fn an_unknown_recognizer_is_refused_by_every_question() {
        let dir = tempfile::tempdir().unwrap();
        for engine in RecognitionEngine::supported_engines() {
            let error = descriptor(engine, "no-such-recognizer", dir.path()).unwrap_err();
            assert!(
                error.to_string().contains("no-such-recognizer"),
                "the error should name what was asked for: {error}"
            );
            assert!(installed(engine, "no-such-recognizer", dir.path()).is_err());
            assert!(admission_threshold(engine, "no-such-recognizer").is_err());
        }
    }

    #[cfg(feature = "candle")]
    #[test]
    fn the_recipe_strings_are_answerable_without_the_weights() {
        let shipped = shipped_model_id(RecognitionEngine::Candle);
        assert!(!shipped.is_empty());
        assert!(identity(RecognitionEngine::Candle, shipped)
            .unwrap()
            .contains(shipped));
        assert_eq!(
            admission_threshold(RecognitionEngine::Candle, shipped).unwrap(),
            super::super::paddleocr_vl::ADMISSION_THRESHOLD
        );
    }

    /// A model id nobody ships is refused rather than quietly answered with
    /// the shipped one, which would put a reading into the index under a
    /// recipe that never produced it.
    #[cfg(feature = "candle")]
    #[test]
    fn an_unknown_recognizer_is_an_error_not_a_fallback() {
        let error = identity(RecognitionEngine::Candle, "no-such-recognizer").unwrap_err();
        assert!(
            error.to_string().contains("no-such-recognizer"),
            "the error should name what was asked for: {error}"
        );
        assert!(load_recognizer_local(
            RecognitionEngine::Candle,
            "no-such-recognizer",
            Path::new("/nonexistent"),
            "cpu"
        )
        .is_err());
    }

    #[cfg(feature = "candle")]
    #[test]
    fn two_checkpoints_read_documents_under_two_recipes() {
        let names: Vec<&str> = super::super::paddleocr_vl::CHECKPOINTS
            .iter()
            .map(|checkpoint| checkpoint.name)
            .collect();
        assert!(names.len() >= 2, "the fixture assumes more than one");
        assert_ne!(
            identity(RecognitionEngine::Candle, names[0]).unwrap(),
            identity(RecognitionEngine::Candle, names[1]).unwrap()
        );
    }
}
