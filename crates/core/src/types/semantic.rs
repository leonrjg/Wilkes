//! Semantic search settings, retrieval tuning and index status.

use super::*;

// ── Semantic settings ─────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize)]
pub struct SemanticSettings {
    pub enabled: bool,
    #[serde(default = "SemanticSettings::default_selected")]
    pub selected: SelectedEmbedder,
    /// Per-engine device overrides ("auto", "cpu", "mps", "cuda").
    /// Missing entries fall back to each engine's own default_device().
    #[serde(default)]
    pub engine_devices: HashMap<EmbeddingEngine, String>,
    pub index_path: Option<PathBuf>,
    /// List of arbitrary HuggingFace IDs manually added by the user, scoped by engine.
    #[serde(default, deserialize_with = "deserialize_custom_models")]
    pub custom_models: Vec<CustomModel>,
    #[serde(default = "SemanticSettings::default_chunk_size")]
    pub chunk_size: usize,
    #[serde(default = "SemanticSettings::default_chunk_overlap")]
    pub chunk_overlap: usize,
    /// Maximum number of indexed chunks admitted to the flat Ward topic pass.
    /// This is the resource control for the O(n²) clustering work; topic
    /// granularity only changes the requested cut.
    #[serde(default = "SemanticSettings::default_topic_cloud_input_cap")]
    pub topic_cloud_input_cap: usize,
    /// Idle timeout for worker processes in seconds.
    #[serde(default = "SemanticSettings::default_worker_timeout")]
    pub worker_timeout_secs: u64,
    /// Texts the embedding worker puts through the model in one forward pass.
    ///
    /// The resource control for embedding, and the only one: a request carries
    /// however many chunks a document has, and the worker's peak memory is set
    /// by its largest single batch rather than by the size of the request.
    /// See [`SemanticSettings::default_embed_batch_size`] for what the default
    /// is measured against.
    #[serde(default = "SemanticSettings::default_embed_batch_size")]
    pub embed_batch_size: usize,
}

#[derive(Deserialize)]
pub(super) struct SemanticSettingsSerde {
    pub(super) enabled: bool,
    #[serde(default = "SemanticSettings::default_selected")]
    pub(super) selected: SelectedEmbedder,
    #[serde(default)]
    pub(super) engine_devices: HashMap<EmbeddingEngine, String>,
    pub(super) index_path: Option<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_custom_models")]
    pub(super) custom_models: Vec<CustomModel>,
    #[serde(default = "SemanticSettings::default_chunk_size")]
    pub(super) chunk_size: usize,
    #[serde(default = "SemanticSettings::default_chunk_overlap")]
    pub(super) chunk_overlap: usize,
    #[serde(default = "SemanticSettings::default_topic_cloud_input_cap")]
    pub(super) topic_cloud_input_cap: usize,
    #[serde(default = "SemanticSettings::default_worker_timeout")]
    pub(super) worker_timeout_secs: u64,
    #[serde(default = "SemanticSettings::default_embed_batch_size")]
    pub(super) embed_batch_size: usize,
}

#[derive(Deserialize)]
pub(super) struct LegacySemanticSettingsSerde {
    pub(super) enabled: bool,
    #[serde(default)]
    pub(super) engine: EmbeddingEngine,
    #[serde(default = "SemanticSettings::default_model")]
    pub(super) model: EmbedderModel,
    #[serde(default = "SemanticSettings::default_dimension")]
    pub(super) dimension: usize,
    #[serde(default)]
    pub(super) engine_devices: HashMap<EmbeddingEngine, String>,
    pub(super) index_path: Option<PathBuf>,
    #[serde(default, deserialize_with = "deserialize_custom_models")]
    pub(super) custom_models: Vec<CustomModel>,
    #[serde(default = "SemanticSettings::default_chunk_size")]
    pub(super) chunk_size: usize,
    #[serde(default = "SemanticSettings::default_chunk_overlap")]
    pub(super) chunk_overlap: usize,
    #[serde(default = "SemanticSettings::default_topic_cloud_input_cap")]
    pub(super) topic_cloud_input_cap: usize,
    #[serde(default = "SemanticSettings::default_worker_timeout")]
    pub(super) worker_timeout_secs: u64,
    #[serde(default = "SemanticSettings::default_embed_batch_size")]
    pub(super) embed_batch_size: usize,
}

pub(super) fn deserialize_custom_models<'de, D>(
    deserializer: D,
) -> Result<Vec<CustomModel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;

    if let Some(arr) = value.as_array() {
        let mut result = Vec::new();
        for item in arr {
            if let Some(s) = item.as_str() {
                // Migration: old Vec<String> format. Default to SBERT.
                result.push(CustomModel {
                    engine: EmbeddingEngine::SBERT,
                    model_id: s.to_string(),
                });
            } else if let Ok(custom) = serde_json::from_value::<CustomModel>(item.clone()) {
                result.push(custom);
            } else {
                return Err(D::Error::custom("Invalid custom_model format"));
            }
        }
        Ok(result)
    } else {
        Ok(Vec::new())
    }
}

impl SemanticSettings {
    pub(super) fn default_selected() -> SelectedEmbedder {
        SelectedEmbedder::default_for(EmbeddingEngine::default())
    }

    pub(super) fn default_model() -> EmbedderModel {
        EmbedderModel(EmbeddingEngine::default().default_model().to_string())
    }

    pub(super) fn default_chunk_size() -> usize {
        600
    }

    pub(super) fn default_chunk_overlap() -> usize {
        128
    }

    pub const fn default_topic_cloud_input_cap() -> usize {
        1_500
    }

    pub(super) fn default_worker_timeout() -> u64 {
        crate::worker::DEFAULT_IDLE_TIMEOUT_SECS
    }

    /// Sixteen.
    ///
    /// Measured, on the 24 GB machine this was found on, against the worst
    /// batch a real corpus produces. It is the worst batch that matters and
    /// not the average one: sentence-transformers sorts a request longest
    /// first, so the first batch is always the largest chunks in it, and a
    /// default that survives the average would fail on every document's first
    /// batch. Against chunks of 4454 characters — the largest the index
    /// actually holds — peak footprint went 4.3 GB at a batch of 4, 4.4 at 8,
    /// 5.6 at 16, 7.7 at 20, 8.5 at 24, and past 9.5 at 32. Sixteen is the
    /// last point before the curve turns sharp, and four times the throughput
    /// of the timid end of it.
    ///
    /// Peak does not grow with the number of batches — it was flat across four
    /// of them — so this bounds the whole run and not just one request.
    ///
    /// Machine-specific by nature: a machine with more memory can afford more,
    /// and that is why it is a setting rather than a constant.
    pub const fn default_embed_batch_size() -> usize {
        16
    }

    pub(super) fn default_dimension() -> usize {
        384 // Default for AllMiniLML6V2
    }

    /// Returns the effective device string for the given engine,
    /// falling back to that engine's built-in default when no override is set.
    pub fn device_for(&self, engine: EmbeddingEngine) -> &str {
        self.engine_devices
            .get(&engine)
            .map(String::as_str)
            .unwrap_or_else(|| engine.default_device())
    }
}

impl<'de> Deserialize<'de> for SemanticSettings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("selected").is_some() {
            let parsed = serde_json::from_value::<SemanticSettingsSerde>(value)
                .map_err(serde::de::Error::custom)?;
            Ok(Self {
                enabled: parsed.enabled,
                selected: parsed.selected,
                engine_devices: parsed.engine_devices,
                index_path: parsed.index_path,
                custom_models: parsed.custom_models,
                chunk_size: parsed.chunk_size,
                chunk_overlap: parsed.chunk_overlap,
                topic_cloud_input_cap: parsed.topic_cloud_input_cap,
                worker_timeout_secs: parsed.worker_timeout_secs,
                embed_batch_size: parsed.embed_batch_size,
            })
        } else {
            let parsed = serde_json::from_value::<LegacySemanticSettingsSerde>(value)
                .map_err(serde::de::Error::custom)?;
            Ok(Self {
                enabled: parsed.enabled,
                selected: SelectedEmbedder {
                    engine: parsed.engine,
                    model: parsed.model,
                    dimension: parsed.dimension,
                },
                engine_devices: parsed.engine_devices,
                index_path: parsed.index_path,
                custom_models: parsed.custom_models,
                chunk_size: parsed.chunk_size,
                chunk_overlap: parsed.chunk_overlap,
                topic_cloud_input_cap: parsed.topic_cloud_input_cap,
                worker_timeout_secs: parsed.worker_timeout_secs,
                embed_batch_size: parsed.embed_batch_size,
            })
        }
    }
}

impl Default for SemanticSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            selected: Self::default_selected(),
            engine_devices: HashMap::new(),
            index_path: None,
            custom_models: Vec::new(),
            chunk_size: Self::default_chunk_size(),
            chunk_overlap: Self::default_chunk_overlap(),
            topic_cloud_input_cap: Self::default_topic_cloud_input_cap(),
            worker_timeout_secs: Self::default_worker_timeout(),
            embed_batch_size: Self::default_embed_batch_size(),
        }
    }
}

// ── Retrieval query enhancement ───────────────────────────────────────────────

/// Techniques that reshape the *query vector* before the nearest-neighbour
/// lookup. Both are optional and off by default. Neither adds a ranking stage
/// after retrieval: search relevance stays owned by the vector index. They only
/// change where in the latent space the query lands before that lookup happens.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct RetrievalSettings {
    /// HyDE: search with the embedding of an LLM-generated hypothetical answer,
    /// which sits in document space rather than terse-question space.
    #[serde(default)]
    pub hyde: HydeSettings,
    /// Pseudo-relevance feedback (Rocchio): fold the centroid of the top initial
    /// hits back into the query vector and retrieve a second time.
    #[serde(default)]
    pub pseudo_relevance_feedback: PrfSettings,
}

/// Hypothetical Document Embeddings (Gao et al., 2022).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HydeSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Number of hypothetical documents generated and averaged together. More
    /// broadens topical coverage at a linear generation-latency cost.
    #[serde(default = "HydeSettings::default_hypotheticals")]
    pub hypotheticals: usize,
    /// Keep the original query vector in the average. When false, retrieval
    /// relies solely on the generated hypotheticals.
    #[serde(default = "default_true")]
    pub include_query: bool,
}

impl HydeSettings {
    pub const fn default_hypotheticals() -> usize {
        1
    }
}

impl Default for HydeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            hypotheticals: Self::default_hypotheticals(),
            include_query: true,
        }
    }
}

/// Pseudo-relevance feedback via the Rocchio update `q' = α·q + β·centroid`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PrfSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Number of top initial hits treated as pseudo-relevant feedback.
    #[serde(default = "PrfSettings::default_feedback_docs")]
    pub feedback_docs: usize,
    /// Weight on the original query vector.
    #[serde(default = "PrfSettings::default_alpha")]
    pub alpha: f32,
    /// Weight on the feedback centroid.
    #[serde(default = "PrfSettings::default_beta")]
    pub beta: f32,
}

impl PrfSettings {
    pub const fn default_feedback_docs() -> usize {
        5
    }
    pub fn default_alpha() -> f32 {
        1.0
    }
    pub fn default_beta() -> f32 {
        0.5
    }
}

impl Default for PrfSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            feedback_docs: Self::default_feedback_docs(),
            alpha: Self::default_alpha(),
            beta: Self::default_beta(),
        }
    }
}

// ── Index status ──────────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexStatus {
    pub indexed_files: usize,
    pub total_chunks: usize,
    pub built_at: Option<u64>,
    pub build_duration_ms: Option<u64>,
    pub engine: EmbeddingEngine,
    pub model_id: String,
    pub dimension: usize,
    pub root_path: Option<std::path::PathBuf>,
    pub db_size_bytes: Option<u64>,
}

/// How much of one root the semantic index covers.
///
/// [`IndexStatus`] counts what the index *holds*; this counts what it is
/// *missing*, which is a different question and the only one that answers
/// "is this root worth indexing". It is derived by listing the root's
/// indexable files and asking the index and the job journal about each — a
/// directory walk and two table reads, never a document read.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RootCoverage {
    pub root: std::path::PathBuf,
    /// Indexable files found under `root` now.
    pub indexable: usize,
    /// Of those, the ones the index holds plus the ones a job read and found
    /// no text in. A document with nothing to index is accounted for, not
    /// missing: the index has no row for it and never will.
    pub covered: usize,
    /// Whether every indexable file under `root` is accounted for. False when
    /// no index exists, and false for a root no job has ever reached.
    pub complete: bool,
}
