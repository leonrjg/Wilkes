//! Embedder and generator models, generation and image-analysis settings,
//! and installed artifacts.

use super::*;

// ── Embedder model ────────────────────────────────────────────────────────────

/// Identifies an embedding model. For fastembed models this is the Debug representation
/// of the `EmbeddingModel` enum variant (e.g. "BGEBaseENV15"); for SBERT/Candle models
/// it is the HuggingFace model code. Serialises as a plain string.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct EmbedderModel(pub String);

impl EmbedderModel {
    pub fn model_id(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for EmbedderModel {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(EmbedderModel(s))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelectedEmbedder {
    pub engine: EmbeddingEngine,
    pub model: EmbedderModel,
    pub dimension: usize,
}

impl SelectedEmbedder {
    pub fn default_for(engine: EmbeddingEngine) -> Self {
        Self {
            engine,
            model: EmbedderModel(engine.default_model().to_string()),
            dimension: 384,
        }
    }
}

impl Default for SelectedEmbedder {
    fn default() -> Self {
        Self::default_for(EmbeddingEngine::default())
    }
}

// ── Model descriptor (returned by list_models) ────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub model_id: String,
    pub display_name: String,
    pub description: String,
    pub dimension: usize,
    pub is_cached: bool,
    pub is_default: bool,
    pub is_recommended: bool,
    /// Total bytes of all model files. Populated from disk for cached models;
    /// `None` for uncached models until explicitly fetched from HuggingFace.
    pub size_bytes: Option<u64>,
    /// How many texts to embed at once. `None` means process all texts as one batch
    /// (required for some quantized models to ensure consistent results).
    pub preferred_batch_size: Option<usize>,
}

/// Where a model's retrieval prefixes came from, and whether the answer is
/// known at all.
///
/// A prefix is not cosmetic — Underdog measured the same model putting the
/// right answer at rank 52 with its query prefix and rank 1792 without it — so
/// a consumer choosing a model needs to know not only what the prefixes are
/// but whether anything has actually established them.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrefixSource {
    /// Parsed from the model's own auxiliary config, which is authoritative.
    Discovered,
    /// Supplied by the curated table, for models that document their
    /// convention only in the model card.
    Curated,
    /// The artifacts are here, they were read, and they name no prefix: this
    /// model takes none.
    NotDocumented,
    /// The model is not local and nothing here has labelled it, so whether it
    /// needs prefixes cannot be known until it is downloaded.
    Undetermined,
}

/// Everything a caller can learn about an embedding model *without* loading
/// it, so that the choice of model is made where the models are.
///
/// This is the whole of what Wilkes says about a model: the dimension, the
/// input recipe, whether the artifacts are here, and — since the picker reads
/// this too — which entry a fresh install would choose. [`ModelDescriptor`]
/// remains the engines' internal listing shape, from which this is built.
/// Every field is either something Wilkes can establish or an explicit
/// absence; nothing here is inferred from a model's name.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmbedderCapability {
    pub engine: EmbeddingEngine,
    /// The id this engine takes back: a catalogue key for fastembed, a
    /// HuggingFace repository id for the engines that load one directly.
    pub model_id: String,
    pub display_name: String,
    pub description: String,
    /// The weights this model is, when the engine names a repository. Absent
    /// where the engine's catalogue does not expose one.
    pub repository_id: Option<String>,
    /// The width of the vectors this model produces.
    ///
    /// `None` is a real answer and not a failure: a model the user added by
    /// hand has no catalogue entry, and its dimension is a property of the
    /// weights that only the first load reveals. A caller must not fill that
    /// in from a picker — it is the one mistake a rebuilt corpus cannot
    /// recover from.
    pub dimension: Option<usize>,
    /// Widths this model may be truncated to. One entry — its own dimension —
    /// until an engine here implements a truncation contract; never a claim
    /// copied from a model card Wilkes cannot honour.
    pub supported_dimensions: Vec<usize>,
    pub query_prefix: Option<String>,
    pub passage_prefix: Option<String>,
    pub prefix_source: PrefixSource,
    /// Longest input the model accepts, when its own config says so.
    pub max_input_tokens: Option<usize>,
    /// Whether the artifacts are on this machine already.
    pub locally_available: bool,
    /// Bytes on disk once installed, where they are known — from disk for a
    /// cached model, and otherwise only after an explicit size fetch.
    pub size_bytes: Option<u64>,
    pub preferred_batch_size: Option<usize>,
    /// True for the entries the engine's own catalogue lists, false for a
    /// model the user added by hand.
    pub catalogued: bool,
    /// The model a fresh install embeds with. Exactly one entry per engine
    /// carries it, and a hand-added model never does.
    pub is_default: bool,
    /// A model this build vouches for, as a picker would mark it. Advice
    /// about a choice rather than a property of the weights — which is why it
    /// sits beside `catalogued` rather than beside `dimension`.
    pub is_recommended: bool,
}

/// What this Wilkes can embed with, as one answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmbedderCapabilityManifest {
    pub engines: Vec<EmbeddingEngine>,
    /// The input roles the managed surface can produce vectors for. A role is
    /// implied by the endpoint that embeds — `chunks/search` embeds a query,
    /// import and `embed/text` embed passages — so this names what a consumer
    /// can ask for, not a flag it may set.
    pub roles: Vec<String>,
    pub models: Vec<EmbedderCapability>,
}

// ── Generation ────────────────────────────────────────────────────────────────

/// The weights repo id of a generation model. The sibling of `EmbedderModel`.
pub const LEGACY_QUANTIZED_GEMMA_MODEL: &str = "unsloth/gemma-3-1b-it-GGUF";

pub const DENSE_GEMMA_MODEL: &str = "unsloth/gemma-3-1b-it";

pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize)]
pub struct GeneratorModel(pub String);

impl GeneratorModel {
    pub fn model_id(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for GeneratorModel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let model = String::deserialize(deserializer)?;
        Ok(Self(if model == LEGACY_QUANTIZED_GEMMA_MODEL {
            DENSE_GEMMA_MODEL.to_string()
        } else {
            model
        }))
    }
}

/// A backend-neutral catalog entry for a generation model.
///
/// Download artifact details belong to the Candle implementation. Ollama owns
/// its model store, so exposing Hugging Face filenames here would make every
/// non-Candle backend pretend to have artifacts it does not manage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeneratorDescriptor {
    pub engine: crate::generate::GenerationEngine,
    pub model_id: String,
    pub display_name: String,
    pub description: String,
    pub context_tokens: usize,
    pub is_cached: bool,
    pub is_default: bool,
    pub is_recommended: bool,
    pub size_bytes: Option<u64>,
}

/// Sibling of `SemanticSettings`. Deliberately not folded into it: the two
/// subsystems are independently toggleable, and coupling them would make
/// enabling semantic search silently enable a second resident model.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GenerationSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Missing in settings written before multiple generation backends existed.
    #[serde(default)]
    pub engine: crate::generate::GenerationEngine,
    #[serde(default)]
    pub model: Option<GeneratorModel>,
    /// "auto", "cpu", "metal". Absent falls back to the engine default.
    #[serde(default)]
    pub device: Option<String>,
    /// Base URL of the Ollama HTTP API. Kept even while Candle is selected so
    /// switching back restores the user's endpoint.
    #[serde(default = "default_ollama_url")]
    pub ollama_url: String,
    /// Explicit Ollama context window. `None` selects the model-reported
    /// maximum; a smaller value lets users trade prompt capacity for KV-cache
    /// memory without relying on Ollama's small implicit default.
    #[serde(default)]
    pub context_tokens: Option<usize>,
    /// Per-task sampling overrides. Absent entries use the task default; the
    /// settings exist for tuning, not as a step the user must take.
    #[serde(default)]
    pub sampling_overrides: HashMap<GenerationTask, crate::generate::Sampling>,
}

/// Which of a document's pictures a recognizer is spent on.
///
/// Not a quality setting — a cost one. Every raster a PDF embeds costs one
/// recognizer call, and on a document VLM that is seconds to minutes each.
/// Most of those rasters are diagrams whose only text is a handful of axis
/// labels, and a large share of them transcribe to nothing worth indexing.
/// The areas a page *typesets* as mathematics or as a ruled table are the
/// opposite case: few per document, and the recognizer is the only way to
/// read them at all, because the glyph run the page drew there does not
/// survive into the reading as text.
///
/// So the two are separable, and the cheap half is the default. Which areas
/// are typeset is decided by the page's own evidence — the font a run is set
/// in, the rules a table is drawn with — in the typeset survey under
/// [`crate::extract::document`], and not by a model. A layout detector, when there
/// is one, joins that survey rather than replacing this choice: it would
/// widen what counts as a formula or a table, which is a different question
/// from whether the embedded rasters are read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageScope {
    /// The formulas and ruled tables the page draws with fonts and paths, and
    /// nothing else. The rasters are still found, counted and reported — they
    /// are simply not decoded and not read.
    #[default]
    TypesetOnly,
    /// Those, and every raster the PDF embeds.
    TypesetAndEmbedded,
}

/// Enrichment of the pictures inside a document.
///
/// Off by default, and deliberately not a quiet default-on: turning it on
/// installs a recognizer, and it changes the extraction recipe, which re-reads
/// and re-embeds every document that has a picture in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAnalysisSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Which recognizer reads the pictures.
    ///
    /// Defaulted, and the default is the ONNX engine — which means a settings
    /// file written before this field existed would silently change
    /// recognizer, change the extraction recipe, and re-read every document
    /// with a picture in it. `migrate_recognizer_choice` exists to stop that;
    /// see its note.
    #[serde(default)]
    pub engine: crate::extract::image::dispatch::RecognitionEngine,
    /// The recognizer's model id. Absent takes the engine's default.
    #[serde(default)]
    pub model: Option<String>,
    /// Formula recognizer id. Absent uses the catalogue default (PP-FormulaNet_plus-S).
    #[serde(default)]
    pub formula_model: Option<String>,
    /// "auto", "cpu", "metal". Absent takes the recognizer's default.
    #[serde(default)]
    pub device: Option<String>,
    /// The Ollama tag figures are described with, or empty for transcription
    /// only. The server is [`GenerationSettings::ollama_url`]: there is one
    /// Ollama endpoint per app, and a second field for the same server would
    /// be a second answer to where it is.
    ///
    /// A description is a separate fact from a transcription, produced by a
    /// separate model, and it is optional in a way the transcription is not.
    #[serde(default)]
    pub describer_model: String,
    /// Which of the document's pictures are read. See [`ImageScope`].
    ///
    /// Defaulted, and the default withholds the embedded rasters — for a
    /// settings file written before this field existed as well, which
    /// therefore moves the extraction recipe and re-reads every document with
    /// a picture in it. Deliberate, not an oversight: the alternative default
    /// spends hours of a library's indexing on pictures most of which
    /// transcribe to nothing, and a setting that only applies to installations
    /// made after it shipped is a setting with two meanings.
    #[serde(default)]
    pub scope: ImageScope,
    /// The role readers switched off: installed, and deliberately not spent.
    ///
    /// Installing a reader used to be the whole of choosing it, so the only
    /// way to stop reading formulas with one was to make its weights
    /// disappear. That conflates two questions — "is this on disk" and "is
    /// this how I want my library read" — and only the second is a setting.
    /// A role in here is a reader that stays downloaded and is not attached,
    /// so the areas the detector marks out for it go to the page reader
    /// instead, exactly as they do when it was never installed.
    ///
    /// By role rather than by model id, because that is how the analyzer
    /// finds these readers in the first place: the selected formula reader
    /// can change while the role remains switched off.
    ///
    /// [`RecognizerRole::Page`] is a member like the other two, and it is the
    /// one whose absence has nowhere to fall through to: what only the page
    /// reader read — a chart, an embedded raster, a formula whose own reader
    /// is missing — is then not read at all rather than read worse. That is a
    /// different question from `enabled`, which says whether the pictures are
    /// looked at in the first place. The one configuration `build_analyzer`
    /// refuses is the empty one: every role in here and nothing left attached
    /// reads as a library whose pictures hold no text, so it is an error and
    /// not a build.
    ///
    /// The general reader is off by default. An explicitly saved list keeps
    /// the user's reader choices, including an empty list that enables all roles.
    #[serde(default = "default_disabled_image_roles")]
    pub disabled_roles: Vec<crate::extract::image::dispatch::RecognizerRole>,
}

pub(super) fn default_disabled_image_roles() -> Vec<crate::extract::image::dispatch::RecognizerRole>
{
    vec![crate::extract::image::dispatch::RecognizerRole::Page]
}

impl Default for ImageAnalysisSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: Default::default(),
            model: None,
            formula_model: None,
            device: None,
            describer_model: String::new(),
            scope: Default::default(),
            disabled_roles: default_disabled_image_roles(),
        }
    }
}

/// Tasks whose sampling the user may override.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationTask {
    ClusterLabel,
    RelationExplanation,
    DocumentSummary,
    SearchResultsSummary,
    HypotheticalContinuation,
    GroundedCompletion,
}

/// One event protocol for every user-facing token stream. Task inputs and
/// validation stay task-specific; correlation and lifecycle do not.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum GenerationStreamEvent {
    Delta {
        request_id: String,
        task: GenerationTask,
        delta: String,
    },
    Completed {
        request_id: String,
        task: GenerationTask,
        text: String,
    },
    Failed {
        request_id: String,
        task: GenerationTask,
        error: String,
    },
}

impl Default for GenerationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: crate::generate::GenerationEngine::default(),
            model: None,
            device: None,
            ollama_url: default_ollama_url(),
            context_tokens: None,
            sampling_overrides: HashMap::new(),
        }
    }
}

pub(super) fn default_ollama_url() -> String {
    DEFAULT_OLLAMA_URL.to_string()
}

// ── Embedding engine ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum EmbeddingEngine {
    #[serde(alias = "sbert")]
    SBERT,
    #[serde(alias = "candle")]
    Candle,
    #[default]
    #[serde(alias = "fastembed")]
    Fastembed,
}

impl EmbeddingEngine {
    pub fn as_str(&self) -> &'static str {
        match self {
            EmbeddingEngine::SBERT => "sbert",
            EmbeddingEngine::Candle => "candle",
            EmbeddingEngine::Fastembed => "fastembed",
        }
    }

    /// Default device string for this engine. Used when no explicit override is set.
    pub fn default_device(&self) -> &'static str {
        match self {
            EmbeddingEngine::SBERT => "auto",
            EmbeddingEngine::Candle => "auto",
            EmbeddingEngine::Fastembed => "cpu",
        }
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            EmbeddingEngine::SBERT => "intfloat/e5-small-v2",
            EmbeddingEngine::Candle => "sentence-transformers/all-MiniLM-L6-v2",
            EmbeddingEngine::Fastembed => "AllMiniLML6V2",
        }
    }

    pub fn supports_custom_models(&self) -> bool {
        match self {
            EmbeddingEngine::SBERT => true,
            EmbeddingEngine::Candle => true,
            EmbeddingEngine::Fastembed => false,
        }
    }

    pub fn supported_engines() -> Vec<Self> {
        let mut engines = vec![EmbeddingEngine::SBERT];
        #[cfg(feature = "candle")]
        engines.push(EmbeddingEngine::Candle);
        #[cfg(feature = "fastembed")]
        engines.push(EmbeddingEngine::Fastembed);
        engines
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CustomModel {
    pub engine: EmbeddingEngine,
    pub model_id: String,
}

/// A Hugging Face repository a search found that could be added as a
/// [`CustomModel`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelSearchHit {
    pub model_id: String,
    /// The architecture its `config.json` names, when the hub reports one.
    pub model_type: Option<String>,
    /// Downloads over the last 30 days, the hub's own figure.
    pub downloads: u64,
}

// ── Recognizer inventory ──────────────────────────────────────────────────────

/// One file of a recognizer, as an inventory names it.
#[derive(Debug, Clone, Serialize)]
pub struct InventoriedArtifact {
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: String,
}

/// What a recognizer is, where it came from, and under what terms.
///
/// FIGURE.md requires this of a redistributed checkpoint before it is
/// packaged, and it is data rather than prose for the reason the pins
/// themselves are: an inventory kept in a comment is one nobody can check.
/// Every file the install writes appears here with the digest it is verified
/// against, so the inventory describes the bytes on disk and not a
/// recollection of them.
///
/// Wilkes fetches these artifacts at the user's request rather than shipping
/// them inside the application, which is why the inventory is shown where the
/// download is offered: the terms are disclosed before the bytes arrive, not
/// after.
///
/// Lives here rather than in a model module because the desktop, server and
/// MCP surfaces all name it. A type three API boundaries carry should not be
/// owned by whichever recognizer happened to need it first.
#[derive(Debug, Clone, Serialize)]
pub struct RecognizerInventory {
    pub name: String,
    pub repo: String,
    pub revision: String,
    pub license: String,
    pub license_url: String,
    pub derived_from: Vec<String>,
    pub artifacts: Vec<InventoriedArtifact>,
    pub footprint_bytes: u64,
}
