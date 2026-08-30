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
//! this module. `load_recognizer_local` needs 1.9 GB and a device and must run
//! in the worker. `identity` and `admission_threshold` are derived from
//! constants, are needed by the host to write the extraction recipe, and must
//! never require a model to answer — the recipe describes what the reading was
//! produced under, and the process that owns the recipe is not the process
//! holding the weights.

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
}

impl RecognitionEngine {
    pub fn as_str(&self) -> &'static str {
        match self {
            RecognitionEngine::Onnx => "onnx",
            RecognitionEngine::Candle => "candle",
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
        }
    }

    /// Every engine this build can actually recognize with.
    pub fn supported_engines() -> Vec<Self> {
        let mut engines = Vec::new();
        #[cfg(feature = "recognize-onnx")]
        engines.push(RecognitionEngine::Onnx);
        #[cfg(feature = "candle")]
        engines.push(RecognitionEngine::Candle);
        engines
    }
}

/// The model id the shipped recipe names for `engine`.
pub fn shipped_model_id(engine: RecognitionEngine) -> &'static str {
    engine.default_model()
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
#[derive(Clone, Debug, serde::Serialize)]
pub struct RecognizerCatalogue {
    pub engines: Vec<RecognitionEngine>,
    pub models: Vec<RecognizerDescriptor>,
}

/// Everything this build can recognize with, default first, then cached.
pub fn list_models(model_dir: &Path) -> Vec<RecognizerDescriptor> {
    use super::ocr::RegionKind;
    let mut models: Vec<RecognizerDescriptor> = Vec::new();

    #[cfg(feature = "recognize-onnx")]
    models.push(RecognizerDescriptor {
        engine: RecognitionEngine::Onnx,
        model_id: super::granite_docling::MODEL_ID.to_string(),
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

    #[cfg(feature = "candle")]
    for checkpoint in super::paddleocr_vl::CHECKPOINTS {
        models.push(RecognizerDescriptor {
            engine: RecognitionEngine::Candle,
            model_id: checkpoint.name.to_string(),
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
        RecognitionEngine::Onnx => {
            anyhow::ensure!(
                model_id == super::granite_docling::MODEL_ID,
                "unknown onnx recognizer '{model_id}'"
            );
            Ok(super::granite_docling::identity())
        }
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

/// Load the recognizer in the calling process.
///
/// Must only be called from a worker subprocess. The recognizer is candle
/// inference against 1.9 GB of weights on whatever accelerator the device
/// string resolves to, and a fault in it takes down the process it is in —
/// which is the whole reason recognition is addressed over a worker protocol
/// rather than called directly.
pub fn load_recognizer_local(
    engine: RecognitionEngine,
    model_id: &str,
    model_dir: &Path,
    device: &str,
) -> anyhow::Result<Box<dyn OcrEngine>> {
    match engine {
        #[cfg(feature = "recognize-onnx")]
        RecognitionEngine::Onnx => {
            anyhow::ensure!(
                model_id == super::granite_docling::MODEL_ID,
                "unknown onnx recognizer '{model_id}'"
            );
            // ONNX Runtime is told how many threads to use rather than which
            // device: the CPU provider is the only one this recognizer was
            // measured on, and naming a device it will not honour would be a
            // setting that reads as a promise.
            let threads = recognizer_threads(device);
            Ok(Box::new(super::granite_docling::GraniteDocling::load(
                model_dir, threads,
            )?))
        }
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
    }
}

/// How many threads the ONNX recognizer gets.
///
/// One less than the machine has, so an indexing pass that runs for an hour
/// does not take the interface down with it, and at least one on a machine
/// that reports nothing.
#[cfg(feature = "recognize-onnx")]
fn recognizer_threads(_device: &str) -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1))
        .unwrap_or(1)
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
        RecognitionEngine::Onnx => {
            anyhow::ensure!(
                model_id == super::granite_docling::MODEL_ID,
                "unknown onnx recognizer '{model_id}'"
            );
            Ok(super::granite_docling::inventory())
        }
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
        RecognitionEngine::Onnx => {
            anyhow::ensure!(
                model_id == super::granite_docling::MODEL_ID,
                "unknown onnx recognizer '{model_id}'"
            );
            super::granite_docling::install(model_dir, progress)
        }
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
            assert!(
                model.footprint_bytes > 0,
                "{} states no footprint",
                model.model_id
            );
            assert!(
                model.admission_threshold > 0.0 && model.admission_threshold < 1.0,
                "{} admits at {}",
                model.model_id,
                model.admission_threshold
            );
            assert!(!model.emits.is_empty(), "{} emits nothing", model.model_id);
            assert!(
                !model.is_cached,
                "nothing is installed in a fresh directory"
            );
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
        let onnx = identity(RecognitionEngine::Onnx, super::super::granite_docling::MODEL_ID)
            .unwrap();
        let candle = identity(
            RecognitionEngine::Candle,
            RecognitionEngine::Candle.default_model(),
        )
        .unwrap();
        assert_ne!(onnx, candle);
        assert_ne!(
            admission_threshold(RecognitionEngine::Onnx, super::super::granite_docling::MODEL_ID)
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
