//! Local generation on Candle.
//!
//! Mirrors `embed/engines/candle.rs` function-for-function so the two stay
//! recognisable, and reuses its device planning outright. What differs is
//! forced by the workload: a KV cache across decode steps instead of a
//! stateless batch, plus per-model weight formats. Qwen uses compact GGUF
//! weights; Gemma uses its dense safetensors implementation because Candle's
//! quantized Gemma decoder does not preserve Gemma's hybrid sliding KV cache.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use async_trait::async_trait;
use candle_core::quantized::gguf_file;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::generation::{LogitsProcessor, Sampling as CandleSampling};
use hf_hub::api::sync::ApiBuilder;
use tokenizers::Tokenizer;
use tracing::{debug, info, warn};

mod gemma3;
mod protocol;
mod weights;

use protocol::ModelFamily;
use weights::DecoderWeights;

use crate::embed::engines::candle::{realize_device, select_device_plan};
use crate::generate::grammar::{Grammar, GrammarState, VocabTrie};
use crate::generate::{
    Constraint, Generated, GenerationEngine, GenerationRequest, GenerationRuntime,
    GenerationTimings, Generator, Sampling, StopReason,
};
use crate::models::hf_hub::HfProgressReporter;
use crate::models::progress::ProgressTx;
use crate::types::{
    GeneratorDescriptor, GeneratorModel, DENSE_GEMMA_MODEL, LEGACY_QUANTIZED_GEMMA_MODEL,
};

// ── Static model catalog ──────────────────────────────────────────────────────

#[derive(Debug)]
struct GeneratorInfo {
    family: ModelFamily,
    weight_format: WeightFormat,
    model_id: &'static str,
    display_name: &'static str,
    description: &'static str,
    weights_file: &'static str,
    /// Immutable commit sha, never a branch name. `unsloth` is a third-party
    /// republisher; pinning is what keeps that a
    /// reviewed decision rather than a moving target. A branch such as `main`
    /// would silently re-resolve, which is exactly what the pin exists to stop.
    weights_revision: &'static str,
    config_file: Option<&'static str>,
    tokenizer_repo: &'static str,
    /// The tokenizer is a downloaded artifact like the weights, so it carries
    /// the same pin. A tokenizer that drifts from the weights it was paired
    /// with produces garbage output, not an error.
    tokenizer_revision: &'static str,
    context_tokens: usize,
    is_recommended: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WeightFormat {
    Gguf,
    Safetensors,
}

/// A commit sha is 40 lowercase hex characters. Anything else — a branch, a
/// tag — resolves to whatever the repo owner last pushed. Enforced by the
/// catalog test, which is where a new entry gets caught.
#[cfg(test)]
fn is_commit_sha(revision: &str) -> bool {
    revision.len() == 40
        && revision
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
}

/// Every model names its tokenizer repository explicitly. Dense repositories
/// can use the same repo; GGUF repositories generally need a separate one.
const GENERATOR_MODELS: &[GeneratorInfo] = &[
    GeneratorInfo {
        family: ModelFamily::Qwen3,
        weight_format: WeightFormat::Gguf,
        model_id: "unsloth/Qwen3-0.6B-GGUF",
        display_name: "Qwen3 0.6B (Q4_K_M)",
        description: "397 MB. Fast enough for interactive labelling on Apple Silicon.",
        weights_file: "Qwen3-0.6B-Q4_K_M.gguf",
        weights_revision: "50968a4468ef4233ed78cd7c3de230dd1d61a56b",
        config_file: None,
        tokenizer_repo: "Qwen/Qwen3-0.6B",
        tokenizer_revision: "c1899de289a04d12100db370d81485cdf75e47ca",
        context_tokens: 32768,
        is_recommended: true,
    },
    GeneratorInfo {
        family: ModelFamily::Qwen3,
        weight_format: WeightFormat::Gguf,
        model_id: "Qwen/Qwen3-0.6B-GGUF",
        display_name: "Qwen3 0.6B (Q8_0, official)",
        description: "~640 MB. The official build, larger but from Qwen directly.",
        weights_file: "Qwen3-0.6B-Q8_0.gguf",
        weights_revision: "23749fefcc72300e3a2ad315e1317431b06b590a",
        config_file: None,
        tokenizer_repo: "Qwen/Qwen3-0.6B",
        tokenizer_revision: "c1899de289a04d12100db370d81485cdf75e47ca",
        context_tokens: 32768,
        is_recommended: false,
    },
    GeneratorInfo {
        family: ModelFamily::Qwen3,
        weight_format: WeightFormat::Gguf,
        model_id: "Qwen/Qwen3-1.7B-GGUF",
        display_name: "Qwen3 1.7B (Q8_0, official)",
        description: "~1.8 GB. Better labels, noticeably slower and heavier.",
        weights_file: "Qwen3-1.7B-Q8_0.gguf",
        weights_revision: "90862c4b9d2787eaed51d12237eafdfe7c5f6077",
        config_file: None,
        tokenizer_repo: "Qwen/Qwen3-1.7B",
        tokenizer_revision: "70d244cc86ccca08cf5af4e1e306ecf908b1ad5e",
        context_tokens: 32768,
        is_recommended: false,
    },
    GeneratorInfo {
        family: ModelFamily::Gemma3,
        weight_format: WeightFormat::Safetensors,
        model_id: "unsloth/gemma-3-1b-it",
        display_name: "Gemma 3 1B (Dense)",
        description:
            "2.04 GB. Dense Gemma with correct hybrid sliding attention; Gemma license applies.",
        weights_file: "model.safetensors",
        weights_revision: "78d5959229dd4b9146485a64c528cf7236ffe16e",
        config_file: Some("config.json"),
        tokenizer_repo: "unsloth/gemma-3-1b-it",
        tokenizer_revision: "78d5959229dd4b9146485a64c528cf7236ffe16e",
        context_tokens: 32768,
        is_recommended: false,
    },
];

pub const DEFAULT_GENERATOR_MODEL: &str = "unsloth/Qwen3-0.6B-GGUF";
const LEGACY_GEMMA_WEIGHTS_FILE: &str = "gemma-3-1b-it-Q4_K_M.gguf";
const LEGACY_GEMMA_WEIGHTS_REVISION: &str = "74e404523bcadb954d7c4e6e6a3a84f1d007568e";

/// Bound the query dimension of non-flash attention during prompt ingestion.
///
/// A monolithic 32k prefill materializes an attention tensor proportional to
/// `heads * prompt_len * prompt_len`; that can exhaust a 16 GB unified-memory
/// Mac before the OS can recover. Both supported decoder families accept a
/// non-zero sequence offset and retain KV state, so the same logical prompt can
/// be evaluated in bounded blocks. Keep this below Gemma 3's 512-token local
/// attention window and small enough that the higher-head Qwen catalog entries
/// remain bounded at the end of a 32k context.
const PREFILL_CHUNK_TOKENS: usize = 128;

fn find_model(model_id: &str) -> anyhow::Result<&'static GeneratorInfo> {
    GENERATOR_MODELS
        .iter()
        .find(|info| info.model_id == model_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "'{model_id}' is not a known generation model. Custom generation models are \
                 rejected because their chat template cannot be inferred."
            )
        })
}

// ── Artifact resolution ───────────────────────────────────────────────────────

/// Cache lookup is revision-aware for the same reason the download is: a file
/// cached from `main` is not the pinned artifact, and answering "already
/// cached" for it would let the pin be bypassed by whatever was fetched first.
fn cached_path(data_dir: &Path, repo: &str, revision: &str, filename: &str) -> Option<PathBuf> {
    hf_hub::Cache::new(data_dir.to_path_buf())
        .repo(hf_hub::Repo::with_revision(
            repo.to_string(),
            hf_hub::RepoType::Model,
            revision.to_string(),
        ))
        .get(filename)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratorArtifacts {
    pub weights_path: PathBuf,
    pub tokenizer_path: PathBuf,
    pub config_path: Option<PathBuf>,
}

fn resolve_artifacts(data_dir: &Path, info: &GeneratorInfo) -> anyhow::Result<GeneratorArtifacts> {
    let config_path = info
        .config_file
        .map(|filename| {
            cached_path(data_dir, info.model_id, info.weights_revision, filename).ok_or_else(|| {
                anyhow::anyhow!(
                    "'{filename}' not cached for '{}' at {}",
                    info.model_id,
                    info.weights_revision
                )
            })
        })
        .transpose()?;
    Ok(GeneratorArtifacts {
        weights_path: cached_path(
            data_dir,
            info.model_id,
            info.weights_revision,
            info.weights_file,
        )
        .ok_or_else(|| {
            anyhow::anyhow!(
                "'{}' not cached for '{}' at {}",
                info.weights_file,
                info.model_id,
                info.weights_revision
            )
        })?,
        tokenizer_path: cached_path(
            data_dir,
            info.tokenizer_repo,
            info.tokenizer_revision,
            "tokenizer.json",
        )
        .ok_or_else(|| {
            anyhow::anyhow!(
                "tokenizer.json not cached for '{}' at {}",
                info.tokenizer_repo,
                info.tokenizer_revision
            )
        })?,
        config_path,
    })
}

pub fn is_generator_available(data_dir: &Path, model_id: &str) -> bool {
    match find_model(model_id) {
        Ok(info) => resolve_artifacts(data_dir, info).is_ok(),
        Err(_) => false,
    }
}

fn cached_size_bytes(data_dir: &Path, info: &GeneratorInfo) -> Option<u64> {
    let artifacts = resolve_artifacts(data_dir, info).ok()?;
    let mut paths = vec![artifacts.weights_path, artifacts.tokenizer_path];
    paths.extend(artifacts.config_path);
    let total = paths
        .iter()
        .map(std::fs::metadata)
        .collect::<Result<Vec<_>, _>>()
        .ok()?
        .into_iter()
        .map(|metadata| metadata.len())
        .sum();
    Some(total)
}

pub fn list_supported_models(data_dir: &Path) -> Vec<GeneratorDescriptor> {
    let mut models: Vec<GeneratorDescriptor> = GENERATOR_MODELS
        .iter()
        .map(|info| {
            let is_cached = resolve_artifacts(data_dir, info).is_ok();
            GeneratorDescriptor {
                engine: GenerationEngine::Candle,
                model_id: info.model_id.to_string(),
                display_name: info.display_name.to_string(),
                description: info.description.to_string(),
                context_tokens: info.context_tokens,
                is_cached,
                is_default: info.model_id == DEFAULT_GENERATOR_MODEL,
                is_recommended: info.is_recommended,
                size_bytes: if is_cached {
                    cached_size_bytes(data_dir, info)
                } else {
                    None
                },
            }
        })
        .collect();
    models.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then(b.is_cached.cmp(&a.is_cached))
            .then(a.model_id.cmp(&b.model_id))
    });
    models
}

pub fn fetch_model_size(model_id: &str) -> anyhow::Result<u64> {
    let info = find_model(model_id)?;
    let weights_siblings =
        crate::models::hf_hub::fetch_hf_siblings_at(info.model_id, Some(info.weights_revision))?;
    let artifact_size = |siblings: &[crate::models::hf_hub::HfSibling],
                         repo: &str,
                         revision: &str,
                         filename: &str|
     -> anyhow::Result<u64> {
        siblings
            .iter()
            .find(|sibling| sibling.rfilename == filename)
            .and_then(|sibling| sibling.size)
            .ok_or_else(|| {
                anyhow::anyhow!("HF repo '{repo}' does not list '{filename}' at {revision}")
            })
    };

    let mut total = artifact_size(
        &weights_siblings,
        info.model_id,
        info.weights_revision,
        info.weights_file,
    )?;
    if let Some(config_file) = info.config_file {
        total += artifact_size(
            &weights_siblings,
            info.model_id,
            info.weights_revision,
            config_file,
        )?;
    }
    let tokenizer_siblings = if info.tokenizer_repo == info.model_id
        && info.tokenizer_revision == info.weights_revision
    {
        weights_siblings
    } else {
        crate::models::hf_hub::fetch_hf_siblings_at(
            info.tokenizer_repo,
            Some(info.tokenizer_revision),
        )?
    };
    total += artifact_size(
        &tokenizer_siblings,
        info.tokenizer_repo,
        info.tokenizer_revision,
        "tokenizer.json",
    )?;
    Ok(total)
}

// ── Install ───────────────────────────────────────────────────────────────────

pub fn install_local(
    data_dir: &Path,
    model: &GeneratorModel,
    progress: Option<ProgressTx>,
) -> anyhow::Result<()> {
    let info = find_model(&model.0)?;
    let reporter = progress.map(HfProgressReporter::new);

    download(
        data_dir,
        info.model_id,
        info.weights_revision,
        info.weights_file,
        reporter.clone(),
    )?;
    if let Some(config_file) = info.config_file {
        download(
            data_dir,
            info.model_id,
            info.weights_revision,
            config_file,
            reporter.clone(),
        )?;
    }
    download(
        data_dir,
        info.tokenizer_repo,
        info.tokenizer_revision,
        "tokenizer.json",
        reporter,
    )?;
    if info.model_id == DENSE_GEMMA_MODEL {
        // Only retire the old bytes after every dense artifact is present. A
        // failed 2 GB download must not destroy the previously cached model.
        remove_cached_artifact(
            data_dir,
            LEGACY_QUANTIZED_GEMMA_MODEL,
            LEGACY_GEMMA_WEIGHTS_REVISION,
            LEGACY_GEMMA_WEIGHTS_FILE,
        )?;
    }
    Ok(())
}

fn download(
    data_dir: &Path,
    repo_id: &str,
    revision: &str,
    filename: &str,
    progress: Option<HfProgressReporter>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = cached_path(data_dir, repo_id, revision, filename) {
        debug!("'{filename}' already cached for '{repo_id}' at {revision}");
        return Ok(path);
    }

    let api = ApiBuilder::new()
        .with_cache_dir(data_dir.to_path_buf())
        .build()
        .context("Failed to initialise HF hub API")?;
    let repo = api.repo(hf_hub::Repo::with_revision(
        repo_id.to_string(),
        hf_hub::RepoType::Model,
        revision.to_string(),
    ));
    let outcome = match progress {
        Some(progress) => repo.download_with_progress(filename, progress),
        None => repo.download(filename),
    };
    outcome.map_err(|e| anyhow::anyhow!("Failed to download '{filename}' from '{repo_id}': {e:#}"))
}

fn remove_cached_artifact(
    data_dir: &Path,
    repo: &str,
    revision: &str,
    filename: &str,
) -> anyhow::Result<()> {
    let Some(path) = cached_path(data_dir, repo, revision, filename) else {
        return Ok(());
    };
    let linked = std::fs::read_link(&path).ok().map(|target| {
        if target.is_absolute() {
            target
        } else {
            path.parent().unwrap_or(data_dir).join(target)
        }
    });
    std::fs::remove_file(&path).with_context(|| format!("Failed to remove {}", path.display()))?;
    if let Some(blob) = linked {
        if blob != path && blob.exists() {
            std::fs::remove_file(&blob)
                .with_context(|| format!("Failed to remove {}", blob.display()))?;
        }
    }
    Ok(())
}

// ── Generator ─────────────────────────────────────────────────────────────────

struct LoadedModel {
    weights: DecoderWeights,
    device: Device,
}

fn dense_gemma_dtype(device: &Device) -> DType {
    if matches!(device, Device::Cpu) {
        DType::F32
    } else {
        DType::F16
    }
}

struct CandidateSet {
    token_ids: Vec<u32>,
    has_text_continuation: bool,
}

struct CachedGrammar {
    grammar: Grammar,
    candidates: Mutex<HashMap<GrammarState, Arc<CandidateSet>>>,
}

impl CachedGrammar {
    fn new(grammar: Grammar) -> Self {
        Self {
            grammar,
            candidates: Mutex::new(HashMap::new()),
        }
    }

    fn candidates(
        &self,
        state: &GrammarState,
        trie: &VocabTrie,
        eos_ids: &[u32],
    ) -> Arc<CandidateSet> {
        if let Some(cached) = self
            .candidates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(state)
            .cloned()
        {
            return cached;
        }

        let mut token_ids = trie.allowed_token_ids(&self.grammar, state);
        let has_text_continuation = !token_ids.is_empty();
        if self.grammar.is_complete(state) {
            for eos in eos_ids {
                if !token_ids.contains(eos) {
                    token_ids.push(*eos);
                }
            }
        }
        let computed = Arc::new(CandidateSet {
            token_ids,
            has_text_continuation,
        });
        self.candidates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(state.clone(), Arc::clone(&computed));
        computed
    }
}

pub struct CandleGenerator {
    family: ModelFamily,
    model_id: String,
    context_tokens: usize,
    /// Serialised: the KV cache is per-model mutable state, so two concurrent
    /// decodes on one instance would corrupt each other.
    model: Mutex<LoadedModel>,
    tokenizer: Tokenizer,
    vocab_trie: VocabTrie,
    eos_ids: Vec<u32>,
    constraints: Mutex<HashMap<String, Arc<CachedGrammar>>>,
    runtime: GenerationRuntime,
    last_timings: Mutex<Option<GenerationTimings>>,
}

impl CandleGenerator {
    fn eos_ids(tokenizer: &Tokenizer, family: ModelFamily) -> Vec<u32> {
        family
            .eos_tokens()
            .iter()
            .filter_map(|token| tokenizer.token_to_id(token))
            .collect()
    }

    fn build_vocab_text(tokenizer: &Tokenizer) -> Vec<String> {
        let vocab = tokenizer.get_vocab(true);
        let size = vocab
            .values()
            .copied()
            .max()
            .map(|m| m as usize + 1)
            .unwrap_or(0);
        let mut text = vec![String::new(); size];
        for (token, id) in vocab {
            // Decode through the tokenizer so byte-level BPE markers (Ġ, Ċ)
            // become the characters the grammar actually matches against.
            let decoded = tokenizer.decode(&[id], false).unwrap_or(token);
            text[id as usize] = decoded;
        }
        text
    }

    fn logits_processor(sampling: &Sampling) -> LogitsProcessor {
        let temperature = sampling.temperature as f64;
        let candle_sampling = if temperature <= 0.0 {
            CandleSampling::ArgMax
        } else {
            match (sampling.top_k, sampling.top_p) {
                (Some(k), Some(p)) => CandleSampling::TopKThenTopP {
                    k,
                    p: p as f64,
                    temperature,
                },
                (Some(k), None) => CandleSampling::TopK { k, temperature },
                (None, Some(p)) => CandleSampling::TopP {
                    p: p as f64,
                    temperature,
                },
                (None, None) => CandleSampling::All { temperature },
            }
        };
        LogitsProcessor::from_sampling(sampling.seed, candle_sampling)
    }

    fn compile_constraint(
        &self,
        constraint: &Constraint,
    ) -> anyhow::Result<Option<Arc<CachedGrammar>>> {
        let (key, compile) = match constraint {
            Constraint::Text { .. } => None,
            Constraint::OneOf(options) => Some((
                format!("one-of:{}", serde_json::to_string(options)?),
                Grammar::one_of(options),
            )),
            Constraint::Grammar(src) => Some((format!("grammar:{src}"), Grammar::parse(src))),
        }
        .unzip();
        let Some(key) = key else {
            return Ok(None);
        };
        if let Some(cached) = self
            .constraints
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
            .cloned()
        {
            return Ok(Some(cached));
        }
        let grammar = compile.expect("a constraint key always has a compiler")?;
        let cached = Arc::new(CachedGrammar::new(grammar));
        self.constraints
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, Arc::clone(&cached));
        Ok(Some(cached))
    }
}

/// Sample only the compact candidate set accepted by the grammar.
///
/// The candidate indices are gathered on the model device, so Metal only sends
/// the permitted logits to the host sampler. Constructing a full host mask and
/// uploading it again would force a synchronization and two full-vocabulary
/// transfers on every token.
fn sample_constrained(
    logits: &Tensor,
    candidates: &CandidateSet,
    processor: &mut LogitsProcessor,
) -> anyhow::Result<u32> {
    anyhow::ensure!(
        !candidates.token_ids.is_empty(),
        "the grammar permits no continuation and is not complete; the constraint is unsatisfiable"
    );
    let indices = Tensor::new(candidates.token_ids.as_slice(), logits.device())?;
    let compact = logits.index_select(&indices, 0)?;
    let selected = processor.sample(&compact)? as usize;
    candidates
        .token_ids
        .get(selected)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("constrained sampler returned an invalid candidate index"))
}

fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}

/// Return only the text before the earliest configured stop. A decoded token
/// may contain both the delimiter and text after it, so this must happen before
/// streaming the token rather than after the sink has already seen too much.
fn before_first_stop<'a>(text: &'a str, stop_strings: &[String]) -> (&'a str, bool) {
    let cutoff = stop_strings
        .iter()
        .filter(|needle| !needle.is_empty())
        .filter_map(|needle| text.find(needle))
        .min();
    match cutoff {
        Some(index) => (&text[..index], true),
        None => (text, false),
    }
}

fn prefill_chunks(token_ids: &[u32]) -> impl Iterator<Item = (usize, &[u32])> {
    token_ids
        .chunks(PREFILL_CHUNK_TOKENS)
        .scan(0, |offset, chunk| {
            let chunk_offset = *offset;
            *offset += chunk.len();
            Some((chunk_offset, chunk))
        })
}

fn output_token_budget(
    prompt_tokens: usize,
    context_tokens: usize,
    requested_limit: Option<usize>,
) -> anyhow::Result<usize> {
    anyhow::ensure!(
        prompt_tokens < context_tokens,
        "prompt of {prompt_tokens} tokens leaves no room in the {context_tokens} token context"
    );
    // The decoder forwards each emitted token to prepare the next step, so
    // preserve one context position beyond the prompt and output budget.
    let available = context_tokens - prompt_tokens - 1;
    let budget = requested_limit.unwrap_or(available);
    anyhow::ensure!(
        budget <= available,
        "prompt of {prompt_tokens} tokens plus {budget} new tokens exceeds the {context_tokens} token context"
    );
    Ok(budget)
}

impl Generator for CandleGenerator {
    fn generate_stream(
        &self,
        req: GenerationRequest,
        sink: &mut dyn FnMut(&str) -> std::ops::ControlFlow<()>,
    ) -> anyhow::Result<Generated> {
        let constraint_started = Instant::now();
        let grammar = self.compile_constraint(&req.constraint)?;
        let mut constraint_elapsed = constraint_started.elapsed();
        let stop_strings: Vec<String> = match &req.constraint {
            Constraint::Text { stop } => stop.clone(),
            _ => Vec::new(),
        };

        let prompt_started = Instant::now();
        let framed = self.family.frame_prompt(&req);
        let prompt_ids = self
            .tokenizer
            .encode(framed.as_str(), true)
            .map_err(anyhow::Error::msg)?
            .get_ids()
            .to_vec();
        let max_tokens =
            output_token_budget(prompt_ids.len(), self.context_tokens, req.max_tokens)?;

        let mut model = self
            .model
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        model.weights.reset_cache();

        let mut processor = Self::logits_processor(&req.sampling);
        let mut grammar_state = grammar.as_ref().map(|g| g.grammar.initial_state());
        let mut emitted_ids: Vec<u32> = Vec::new();
        let mut raw = String::new();
        let mut streamed = String::new();
        let mut stop = StopReason::MaxTokens;

        tracing::info!(
            model = self.model_id,
            prompt_tokens = prompt_ids.len(),
            prefill_chunk_tokens = PREFILL_CHUNK_TOKENS,
            prefill_chunks = prompt_ids.len().div_ceil(PREFILL_CHUNK_TOKENS),
            synchronize_each_chunk = true,
            "starting bounded generation prefill"
        );
        let mut logits = None;
        let mut offset = 0;
        for (chunk_offset, chunk) in prefill_chunks(&prompt_ids) {
            debug_assert_eq!(chunk_offset, offset);
            let input = Tensor::new(chunk, &model.device)?.unsqueeze(0)?;
            logits = Some(model.weights.forward(&input, chunk_offset)?.squeeze(0)?);
            // Metal retains resources referenced by an uncommitted command
            // buffer. Waiting here is what makes the tensor-shape bound a
            // real peak-memory bound rather than merely a smaller series of
            // allocations that remain live together until sampling.
            model.device.synchronize()?;
            offset += chunk.len();
        }
        let mut logits = logits.ok_or_else(|| {
            candle_core::Error::Msg("generation prompt tokenized to an empty sequence".to_string())
        })?;
        let prompt_elapsed = prompt_started.elapsed();
        tracing::info!(
            model = self.model_id,
            prompt_tokens = prompt_ids.len(),
            prompt_micros = micros(prompt_elapsed),
            "bounded generation prefill completed"
        );
        let decode_started = Instant::now();

        for _ in 0..max_tokens {
            if let Some((penalty, window)) = req.sampling.repeat_penalty {
                if penalty != 1.0 && !emitted_ids.is_empty() {
                    let start = emitted_ids.len().saturating_sub(window);
                    logits = candle_transformers::utils::apply_repeat_penalty(
                        &logits,
                        penalty,
                        &emitted_ids[start..],
                    )?;
                }
            }

            let next = match (grammar.as_ref(), grammar_state.as_ref()) {
                (Some(grammar), Some(state)) => {
                    let started = Instant::now();
                    let candidates = grammar.candidates(state, &self.vocab_trie, &self.eos_ids);
                    let selected = sample_constrained(&logits, &candidates, &mut processor)?;
                    constraint_elapsed += started.elapsed();
                    selected
                }
                _ => processor.sample(&logits)?,
            };

            if self.eos_ids.contains(&next) {
                stop = StopReason::Eos;
                break;
            }

            let piece = self
                .tokenizer
                .decode(&[next], false)
                .map_err(anyhow::Error::msg)?;

            if let (Some(grammar), Some(state)) = (grammar.as_ref(), grammar_state.as_ref()) {
                grammar_state = Some(grammar.grammar.advance(state, &piece).ok_or_else(|| {
                    anyhow::anyhow!("masked sampling produced a token outside the grammar")
                })?);
            }

            emitted_ids.push(next);
            raw.push_str(&piece);

            // Protocol-owned extraction keeps model scaffolding out of task
            // streams without teaching each task about model families.
            let visible = self.family.visible_text(&raw);
            let (streamable, hit_stop) = before_first_stop(visible, &stop_strings);
            if streamable.len() > streamed.len() {
                let delta = streamable[streamed.len()..].to_string();
                streamed = streamable.to_string();
                if sink(&delta).is_break() {
                    stop = StopReason::Cancelled;
                    break;
                }
            }

            if hit_stop {
                stop = StopReason::StopString;
                break;
            }

            if let (Some(grammar), Some(state)) = (grammar.as_ref(), grammar_state.as_ref()) {
                let started = Instant::now();
                let candidates = grammar.candidates(state, &self.vocab_trie, &self.eos_ids);
                let terminal =
                    grammar.grammar.is_complete(state) && !candidates.has_text_continuation;
                constraint_elapsed += started.elapsed();
                if terminal {
                    stop = StopReason::GrammarComplete;
                    break;
                }
            }

            let input = Tensor::new(&[next], &model.device)?.unsqueeze(0)?;
            logits = model.weights.forward(&input, offset)?.squeeze(0)?;
            offset += 1;
        }

        let visible = self.family.visible_text(&raw);
        let (text, _) = before_first_stop(visible, &stop_strings);
        let timings = GenerationTimings {
            prompt_micros: micros(prompt_elapsed),
            decode_micros: micros(decode_started.elapsed()),
            constraint_micros: micros(constraint_elapsed),
        };
        *self
            .last_timings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(timings.clone());

        tracing::info!(
            model = self.model_id,
            prompt_tokens = prompt_ids.len(),
            generated_tokens = emitted_ids.len(),
            stop = ?stop,
            timings = ?timings,
            "bounded generation completed"
        );

        Ok(Generated {
            text: text.trim().to_string(),
            tokens: emitted_ids.len(),
            stop,
        })
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn context_tokens(&self) -> usize {
        self.context_tokens
    }

    fn runtime(&self) -> Option<GenerationRuntime> {
        Some(self.runtime.clone())
    }

    fn last_timings(&self) -> Option<GenerationTimings> {
        self.last_timings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

// ── Loading ───────────────────────────────────────────────────────────────────

/// Load a generator directly in the calling process. Only ever called from the
/// worker subprocess — a Metal fault here would otherwise take down the app.
pub fn load_generator(
    model: &GeneratorModel,
    data_dir: &Path,
    device: &str,
) -> anyhow::Result<Arc<dyn Generator>> {
    let load_started = Instant::now();
    let info = find_model(&model.0)?;
    let artifacts = resolve_artifacts(data_dir, info)?;

    let realized = realize_device(select_device_plan(device))?;
    let tokenizer = Tokenizer::from_file(&artifacts.tokenizer_path).map_err(anyhow::Error::msg)?;

    let weights = match info.weight_format {
        WeightFormat::Gguf => {
            let mut file = std::fs::File::open(&artifacts.weights_path)
                .with_context(|| format!("Failed to open {}", artifacts.weights_path.display()))?;
            let content =
                gguf_file::Content::read(&mut file).context("Failed to read the GGUF header")?;
            DecoderWeights::from_gguf(info.family, content, &mut file, &realized.device)
                .with_context(|| format!("Failed to load {:?} GGUF weights", info.family))?
        }
        WeightFormat::Safetensors => {
            anyhow::ensure!(
                info.family == ModelFamily::Gemma3,
                "dense safetensors are only configured for Gemma 3"
            );
            let config_path = artifacts
                .config_path
                .as_ref()
                .context("dense Gemma is missing config.json")?;
            let config_text = std::fs::read_to_string(config_path)
                .with_context(|| format!("Failed to read {}", config_path.display()))?;
            let config: gemma3::Config = serde_json::from_str(&config_text)
                .with_context(|| format!("Failed to parse {}", config_path.display()))?;
            // The pinned checkpoint is BF16, but Candle 0.11's Metal BF16
            // execution becomes numerically unstable once Gemma's rotating
            // cache is full. FP16 preserves the dense 2-byte footprint and
            // coherent long-context decoding. CPU matmul supports neither
            // half dtype, so CPU realizes the checkpoint as F32.
            let dtype = dense_gemma_dtype(&realized.device);
            let vb = unsafe {
                VarBuilder::from_mmaped_safetensors(
                    &[artifacts.weights_path.as_path()],
                    dtype,
                    &realized.device,
                )
            }
            .with_context(|| {
                format!(
                    "Failed to mmap dense weights from {}",
                    artifacts.weights_path.display()
                )
            })?;
            let dense = gemma3::Model::new(false, &config, vb).map_err(|error| {
                anyhow::anyhow!("Failed to load dense Gemma 3 weights: {error:#}")
            })?;
            DecoderWeights::from_dense_gemma(dense)
        }
    };
    let eos_ids = CandleGenerator::eos_ids(&tokenizer, info.family);
    anyhow::ensure!(
        !eos_ids.is_empty(),
        "tokenizer for '{}' has none of the {:?} EOS tokens",
        info.model_id,
        info.family.eos_tokens()
    );
    let vocab_text = CandleGenerator::build_vocab_text(&tokenizer);
    let vocab_trie = VocabTrie::new(&vocab_text);

    let mut generator = CandleGenerator {
        family: info.family,
        model_id: model.0.clone(),
        context_tokens: info.context_tokens,
        vocab_trie,
        eos_ids,
        tokenizer,
        model: Mutex::new(LoadedModel {
            weights,
            device: realized.device,
        }),
        constraints: Mutex::new(HashMap::new()),
        runtime: GenerationRuntime {
            requested_device: device.to_string(),
            device: realized.name,
            fallback_reason: realized.fallback_reason,
            model_load_micros: 0,
        },
        last_timings: Mutex::new(None),
    };

    warm_up(&generator);
    generator.runtime.model_load_micros = micros(load_started.elapsed());
    Ok(Arc::new(generator))
}

/// One throwaway forward pass at load time. The first Metal forward compiles
/// kernels: measured at 7.5s before warmup and 76ms after. Paying it here keeps
/// it out of the first user-visible request.
fn warm_up(generator: &CandleGenerator) {
    let mut model = generator
        .model
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let result = Tensor::new(&[0u32], &model.device)
        .and_then(|t| t.unsqueeze(0))
        .and_then(|t| model.weights.forward(&t, 0));
    match result {
        Ok(_) => info!("generation model warmed up"),
        // Not fatal: the first real request pays the cost instead.
        Err(e) => warn!("generation warmup forward pass failed: {e:#}"),
    }
    model.weights.reset_cache();
}

// ── Installer ─────────────────────────────────────────────────────────────────

/// Mirrors `EmbedderInstaller`. Separate trait because it yields a `Generator`.
#[async_trait]
pub trait GeneratorInstaller: Send + Sync {
    fn is_available(&self, data_dir: &Path) -> bool;
    async fn install(&self, data_dir: &Path, tx: ProgressTx) -> anyhow::Result<()>;
    fn uninstall(&self, data_dir: &Path) -> anyhow::Result<()>;
    /// Build the live generator. Dispatches through the worker, never in-process.
    fn build(&self, data_dir: &Path) -> anyhow::Result<Arc<dyn Generator>>;
}

pub struct CandleGeneratorInstaller {
    pub model: GeneratorModel,
    pub manager: crate::worker::manager::WorkerManager,
    pub device: String,
}

impl CandleGeneratorInstaller {
    pub fn new(
        model: GeneratorModel,
        manager: crate::worker::manager::WorkerManager,
        device: String,
    ) -> Self {
        Self {
            model,
            manager,
            device,
        }
    }
}

#[async_trait]
impl GeneratorInstaller for CandleGeneratorInstaller {
    fn is_available(&self, data_dir: &Path) -> bool {
        is_generator_available(data_dir, &self.model.0)
    }

    async fn install(&self, data_dir: &Path, tx: ProgressTx) -> anyhow::Result<()> {
        let model = self.model.clone();
        let data_dir = data_dir.to_path_buf();
        tokio::task::spawn_blocking(move || install_local(&data_dir, &model, Some(tx))).await?
    }

    fn uninstall(&self, data_dir: &Path) -> anyhow::Result<()> {
        let info = find_model(&self.model.0)?;
        remove_cached_artifact(
            data_dir,
            info.model_id,
            info.weights_revision,
            info.weights_file,
        )
    }

    fn build(&self, data_dir: &Path) -> anyhow::Result<Arc<dyn Generator>> {
        let info = find_model(&self.model.0)?;
        Ok(Arc::new(crate::generate::worker::WorkerGenerator::new(
            self.manager.clone(),
            crate::generate::worker::WorkerGeneratorConfig {
                model_id: self.model.0.clone(),
                device: self.device.clone(),
                engine: GenerationEngine::Candle,
                context_tokens: info.context_tokens,
                data_dir: data_dir.to_path_buf(),
            },
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn catalog_leads_with_the_recommended_default() {
        let dir = tempdir().unwrap();
        let models = list_supported_models(dir.path());
        assert!(!models.is_empty());
        assert_eq!(models[0].model_id, DEFAULT_GENERATOR_MODEL);
        assert!(models[0].is_default);
        assert!(models[0].is_recommended);
    }

    #[test]
    fn catalog_contains_only_the_dense_gemma3_model() {
        let info = find_model(DENSE_GEMMA_MODEL).unwrap();
        assert_eq!(info.family, ModelFamily::Gemma3);
        assert_eq!(info.weight_format, WeightFormat::Safetensors);
        assert_eq!(info.weights_file, "model.safetensors");
        assert_eq!(info.config_file, Some("config.json"));
        assert!(find_model(LEGACY_QUANTIZED_GEMMA_MODEL).is_err());
    }

    #[test]
    fn every_catalog_entry_pins_all_artifact_revisions() {
        for info in GENERATOR_MODELS {
            assert!(!info.weights_file.is_empty(), "{}", info.model_id);
            assert!(!info.tokenizer_repo.is_empty(), "{}", info.model_id);
            assert!(
                is_commit_sha(info.weights_revision),
                "{} pins its weights to '{}', which is not a commit sha",
                info.model_id,
                info.weights_revision
            );
            assert!(
                is_commit_sha(info.tokenizer_revision),
                "{} pins its tokenizer to '{}', which is not a commit sha",
                info.model_id,
                info.tokenizer_revision
            );
            if info.weight_format == WeightFormat::Gguf {
                assert_ne!(
                    info.tokenizer_repo, info.model_id,
                    "the GGUF repos carry no tokenizer.json"
                );
                assert!(info.config_file.is_none());
            } else {
                assert_eq!(info.tokenizer_repo, info.model_id);
                assert!(info.config_file.is_some());
            }
        }
    }

    #[test]
    fn unknown_models_are_rejected_rather_than_guessed_at() {
        let err = find_model("some/random-model").unwrap_err().to_string();
        assert!(err.contains("not a known generation model"), "{err}");
        assert!(!is_generator_available(
            tempdir().unwrap().path(),
            "some/random-model"
        ));
    }

    #[test]
    fn uncached_models_report_unavailable() {
        let dir = tempdir().unwrap();
        assert!(!is_generator_available(dir.path(), DEFAULT_GENERATOR_MODEL));
        let models = list_supported_models(dir.path());
        assert!(models.iter().all(|m| !m.is_cached));
        assert!(models.iter().all(|m| m.size_bytes.is_none()));
    }

    #[test]
    fn sampling_maps_onto_candle_variants() {
        // Greedy stays greedy no matter what else is set.
        let greedy = Sampling {
            temperature: 0.0,
            top_k: Some(40),
            top_p: Some(0.9),
            ..Sampling::default()
        };
        let _ = CandleGenerator::logits_processor(&greedy);
        let stochastic = Sampling {
            temperature: 0.7,
            top_p: Some(0.9),
            ..Sampling::default()
        };
        let _ = CandleGenerator::logits_processor(&stochastic);
    }

    #[test]
    fn dense_gemma_uses_a_cpu_supported_dtype() {
        assert_eq!(dense_gemma_dtype(&Device::Cpu), DType::F32);
    }

    #[test]
    fn long_prefills_are_contiguous_and_never_exceed_the_memory_bound() {
        let token_ids = (0..32_768_u32).collect::<Vec<_>>();
        let chunks = prefill_chunks(&token_ids).collect::<Vec<_>>();

        assert_eq!(chunks.len(), token_ids.len().div_ceil(PREFILL_CHUNK_TOKENS));
        assert!(PREFILL_CHUNK_TOKENS <= 512);
        assert!(chunks
            .iter()
            .all(|(_, chunk)| !chunk.is_empty() && chunk.len() <= PREFILL_CHUNK_TOKENS));

        let mut next_offset = 0;
        let mut rebuilt = Vec::with_capacity(token_ids.len());
        for (offset, chunk) in chunks {
            assert_eq!(offset, next_offset);
            rebuilt.extend_from_slice(chunk);
            next_offset += chunk.len();
        }
        assert_eq!(next_offset, token_ids.len());
        assert_eq!(rebuilt, token_ids);
    }

    #[test]
    fn empty_prefill_produces_no_decoder_blocks() {
        assert_eq!(prefill_chunks(&[]).count(), 0);
    }

    #[test]
    fn unlimited_output_uses_the_remaining_context_capacity() {
        assert_eq!(output_token_budget(300, 1_000, None).unwrap(), 699);
    }

    #[test]
    fn explicit_output_limits_remain_bounded_by_context_capacity() {
        assert_eq!(output_token_budget(300, 1_000, Some(120)).unwrap(), 120);
        assert!(output_token_budget(900, 1_000, Some(100)).is_err());
        assert!(output_token_budget(1_000, 1_000, None).is_err());
    }

    #[test]
    fn constrained_sampling_only_considers_grammar_candidates() {
        let grammar =
            CachedGrammar::new(Grammar::one_of(&["yes".to_string(), "no".to_string()]).unwrap());
        let state = grammar.grammar.initial_state();
        let vocab: Vec<String> = ["y", "n", "z", "<|im_end|>"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let trie = VocabTrie::new(&vocab);
        let candidates = grammar.candidates(&state, &trie, &[3]);
        assert_eq!(candidates.token_ids, vec![0, 1]);
        let logits = Tensor::new(&[1.0f32, 2.0, 100.0, 100.0], &Device::Cpu).unwrap();
        let mut processor = CandleGenerator::logits_processor(&Sampling::default());
        assert_eq!(
            sample_constrained(&logits, &candidates, &mut processor).unwrap(),
            1
        );
    }

    #[test]
    fn constrained_candidates_admit_eos_once_the_grammar_is_satisfied() {
        let grammar = CachedGrammar::new(Grammar::one_of(&["yes".to_string()]).unwrap());
        let state = grammar
            .grammar
            .advance(&grammar.grammar.initial_state(), "yes")
            .unwrap();
        let vocab: Vec<String> = ["y", "<|im_end|>"].iter().map(|s| s.to_string()).collect();
        let candidates = grammar.candidates(&state, &VocabTrie::new(&vocab), &[1]);
        assert_eq!(candidates.token_ids, vec![1]);
        assert!(!candidates.has_text_continuation);
    }

    #[test]
    fn stop_delimiters_cut_a_token_before_it_is_streamed() {
        let stops = vec!["\n".to_string(), "END".to_string()];
        assert_eq!(
            before_first_stop("Monitoring\nMetrics", &stops),
            ("Monitoring", true)
        );
        assert_eq!(
            before_first_stop("alpha END beta\n", &stops),
            ("alpha ", true)
        );
        assert_eq!(before_first_stop("complete", &stops), ("complete", false));
    }

    #[test]
    fn an_unsatisfiable_constraint_errors_rather_than_sampling_garbage() {
        let grammar = CachedGrammar::new(Grammar::one_of(&["yes".to_string()]).unwrap());
        let state = grammar.grammar.initial_state();
        let vocab: Vec<String> = vec!["q".to_string()];
        let candidates = grammar.candidates(&state, &VocabTrie::new(&vocab), &[]);
        let logits = Tensor::new(&[1.0f32], &Device::Cpu).unwrap();
        let mut processor = CandleGenerator::logits_processor(&Sampling::default());
        assert!(sample_constrained(&logits, &candidates, &mut processor).is_err());
    }

    #[test]
    fn installer_reports_unavailable_for_an_empty_data_dir() {
        let dir = tempdir().unwrap();
        let (manager, _rx, _fut) = crate::worker::manager::WorkerManager::new(
            crate::worker::manager::WorkerPaths::resolve(dir.path()),
        );
        let installer = CandleGeneratorInstaller::new(
            GeneratorModel(DEFAULT_GENERATOR_MODEL.to_string()),
            manager,
            "cpu".to_string(),
        );
        assert!(!installer.is_available(dir.path()));
        // Uninstalling something absent is a no-op, not an error.
        assert!(installer.uninstall(dir.path()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn obsolete_weight_cleanup_removes_only_the_pinned_snapshot_and_blob() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let repo_dir = dir.path().join("models--unsloth--gemma-3-1b-it-GGUF");
        let snapshot = repo_dir
            .join("snapshots")
            .join(LEGACY_GEMMA_WEIGHTS_REVISION);
        let blobs = repo_dir.join("blobs");
        let refs = repo_dir.join("refs");
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::create_dir_all(&refs).unwrap();
        std::fs::write(
            refs.join(LEGACY_GEMMA_WEIGHTS_REVISION),
            LEGACY_GEMMA_WEIGHTS_REVISION,
        )
        .unwrap();
        let blob = blobs.join("old-q4");
        std::fs::write(&blob, b"old weights").unwrap();
        let entry = snapshot.join(LEGACY_GEMMA_WEIGHTS_FILE);
        symlink("../../blobs/old-q4", &entry).unwrap();

        remove_cached_artifact(
            dir.path(),
            LEGACY_QUANTIZED_GEMMA_MODEL,
            LEGACY_GEMMA_WEIGHTS_REVISION,
            LEGACY_GEMMA_WEIGHTS_FILE,
        )
        .unwrap();

        assert!(!entry.exists());
        assert!(!blob.exists());
        assert!(snapshot.exists());
    }
}
