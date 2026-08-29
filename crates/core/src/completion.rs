//! One authoritative pipeline for library-grounded editor completion.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::embed::index::{SemanticIndex, SemanticQueryScope};
use crate::embed::Embedder;
use crate::generate::tasks::grounded_completion::{generate_and_verify, GroundedCompletionInput};
use crate::generate::tasks::hypothetical_continuation::hypothetical_continuation_stream;
use crate::generate::{GenerationTimings, Generator};
use crate::types::SourceOrigin;

const DENSE_CANDIDATES: usize = 60;
const MAX_PASSAGE_CHARS: usize = 7_000;
const MAX_RECORDS: usize = 64;
const MAX_SUPPRESSIONS: usize = 12;
const MAX_AVOID_SUGGESTIONS: usize = 8;
const MAX_AVOID_SUGGESTION_CHARS: usize = 1_000;
const MAX_AVOID_SUGGESTION_TOTAL_CHARS: usize = 4_000;
const MAX_DUPLICATE_RETRIES: usize = 2;
const RRF_K: f32 = 60.0;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CompletionMode {
    #[default]
    Append,
    Bridge,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptFormat {
    #[default]
    InstructContinue,
    InstructInfill,
    NativeFim,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CompletionScopeMode {
    #[default]
    Library,
    Prefer,
    Only,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletionScope {
    #[serde(default)]
    pub mode: CompletionScopeMode,
    #[serde(default)]
    pub pinned: Vec<PathBuf>,
    #[serde(default)]
    pub excluded: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletionRequest {
    pub path: PathBuf,
    pub text: String,
    /// Unicode scalar offset, never a UTF-8 byte offset.
    pub cursor: usize,
    #[serde(default)]
    pub scope: CompletionScope,
    /// Previously shown candidates that a regeneration must not repeat.
    #[serde(default, alias = "avoidSuggestions")]
    pub avoid_suggestions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionSource {
    pub path: PathBuf,
    pub title: String,
    pub page: Option<u32>,
    pub chunk_ids: Vec<String>,
    pub score: f32,
    pub pinned: bool,
    #[serde(skip)]
    passage: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextComposition {
    pub window_tokens: usize,
    pub used_tokens: usize,
    pub doc_coverage: DocumentCoverage,
    pub retrieval_tokens: usize,
    pub doc_tokens: usize,
    pub scope_mode: CompletionScopeMode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DocumentCoverage {
    Full,
    Elided {
        head_tokens: usize,
        tail_tokens: usize,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CompletionEvent {
    Retrieval {
        sources: Vec<CompletionSource>,
        hyde_query: String,
    },
    Context {
        composition: ContextComposition,
    },
    Shown {
        text: String,
        mode: CompletionMode,
    },
    Suppressed {
        reason: String,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompletionFeedback {
    Accepted,
    Partial,
    Dismissed,
    TypedThrough,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SteeringContribution {
    pub path: PathBuf,
    pub weight: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SuppressionEntry {
    pub reason: String,
    pub candidate: String,
    pub hyde_query: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSteering {
    pub documents: Vec<SteeringContribution>,
    pub suppressions: Vec<SuppressionEntry>,
}

#[derive(Clone)]
struct CompletionRecord {
    sources: Vec<(PathBuf, String)>,
    mode: CompletionMode,
    prompt_format: PromptFormat,
    scope_mode: CompletionScopeMode,
    retrieval_scores: Vec<f32>,
    model: String,
}

/// An immutable feedback operation prepared while the session is locked, then
/// embedded after releasing that lock. Applying it is the only operation that
/// consumes the corresponding completion record.
#[derive(Clone)]
pub struct CompletionFeedbackPlan {
    completion_id: String,
    verdict: CompletionFeedback,
    record: CompletionRecord,
}

impl CompletionFeedbackPlan {
    pub fn embed(&self, embedder: &dyn Embedder) -> anyhow::Result<Vec<Vec<f32>>> {
        let texts = self
            .record
            .sources
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>();
        embedder.embed_passages(&texts)
    }
}

#[derive(Clone)]
struct HydeCache {
    boundary: usize,
    query: String,
    vector: Vec<f32>,
}

#[derive(Clone)]
struct DocumentVectorCache {
    hash: u64,
    vector: Vec<f32>,
}

#[derive(Clone)]
struct RetrievalCache {
    boundary: usize,
    scope: CompletionScope,
    sources: Vec<CompletionSource>,
}

#[derive(Default)]
pub struct CompletionSession {
    hyde: HashMap<PathBuf, HydeCache>,
    documents: HashMap<PathBuf, DocumentVectorCache>,
    paragraph_vectors: HashMap<u64, Vec<f32>>,
    retrieval: HashMap<PathBuf, RetrievalCache>,
    feedback_vector: Option<Vec<f32>>,
    contributions: HashMap<PathBuf, f32>,
    records: HashMap<String, CompletionRecord>,
    record_order: VecDeque<String>,
    suppressions: VecDeque<SuppressionEntry>,
}

impl CompletionSession {
    pub fn steering(&self) -> SessionSteering {
        let mut documents = self
            .contributions
            .iter()
            .map(|(path, weight)| SteeringContribution {
                path: path.clone(),
                weight: *weight,
            })
            .collect::<Vec<_>>();
        documents.sort_by(|left, right| right.weight.abs().total_cmp(&left.weight.abs()));
        documents.truncate(8);
        SessionSteering {
            documents,
            suppressions: self.suppressions.iter().rev().cloned().collect(),
        }
    }

    pub fn reset(&mut self) {
        self.feedback_vector = None;
        self.contributions.clear();
        self.records.clear();
        self.record_order.clear();
        self.retrieval.clear();
    }

    pub fn prepare_feedback(
        &self,
        completion_id: &str,
        verdict: CompletionFeedback,
    ) -> anyhow::Result<CompletionFeedbackPlan> {
        let Some(record) = self.records.get(completion_id).cloned() else {
            anyhow::bail!("unknown completion id");
        };
        Ok(CompletionFeedbackPlan {
            completion_id: completion_id.to_string(),
            verdict,
            record,
        })
    }

    pub fn apply_feedback(
        &mut self,
        plan: CompletionFeedbackPlan,
        vectors: Vec<Vec<f32>>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.records.contains_key(&plan.completion_id),
            "unknown completion id"
        );
        let weight = match plan.verdict {
            CompletionFeedback::Accepted => 1.0,
            CompletionFeedback::Partial => 0.5,
            CompletionFeedback::Dismissed => -0.2,
            CompletionFeedback::TypedThrough => -0.1,
        };
        let centroid = mean_normalized(&vectors);
        self.records.remove(&plan.completion_id);
        self.record_order.retain(|id| id != &plan.completion_id);
        let Some(centroid) = centroid else {
            return Ok(());
        };
        let accumulator = self
            .feedback_vector
            .get_or_insert_with(|| vec![0.0; centroid.len()]);
        for value in accumulator.iter_mut() {
            *value *= 0.95;
        }
        for contribution in self.contributions.values_mut() {
            *contribution *= 0.95;
        }
        for (slot, value) in accumulator.iter_mut().zip(centroid) {
            *slot += weight * value;
        }
        for (path, _) in &plan.record.sources {
            let path = path.clone();
            *self.contributions.entry(path).or_default() += weight;
        }
        tracing::info!(
            completion_id = plan.completion_id,
            feedback = ?plan.verdict,
            mode = ?plan.record.mode,
            prompt_format = ?plan.record.prompt_format,
            scope_mode = ?plan.record.scope_mode,
            retrieval_scores = ?plan.record.retrieval_scores,
            model = plan.record.model,
            "grounded completion feedback"
        );
        Ok(())
    }

    fn remember(
        &mut self,
        id: String,
        sources: &[CompletionSource],
        mode: CompletionMode,
        prompt_format: PromptFormat,
        scope_mode: CompletionScopeMode,
        model: &str,
    ) {
        if self.records.contains_key(&id) {
            self.record_order.retain(|candidate| candidate != &id);
        }
        self.records.insert(
            id.clone(),
            CompletionRecord {
                sources: sources
                    .iter()
                    .map(|source| (source.path.clone(), source.passage.clone()))
                    .collect(),
                mode,
                prompt_format,
                scope_mode,
                retrieval_scores: sources.iter().map(|source| source.score).collect(),
                model: model.to_string(),
            },
        );
        self.record_order.push_back(id);
        while self.record_order.len() > MAX_RECORDS {
            if let Some(old) = self.record_order.pop_front() {
                self.records.remove(&old);
            }
        }
    }

    fn suppress(&mut self, reason: &str, candidate: String, hyde_query: String) {
        self.suppressions.push_back(SuppressionEntry {
            reason: reason.to_string(),
            candidate,
            hyde_query,
        });
        while self.suppressions.len() > MAX_SUPPRESSIONS {
            self.suppressions.pop_front();
        }
    }
}

#[derive(Clone)]
struct Hit {
    path: PathBuf,
    start: usize,
    end: usize,
    chunk_ids: Vec<String>,
    origin: SourceOrigin,
    dense_score: f32,
    fused_score: f32,
}

pub struct CompletionDependencies {
    pub embedder: Arc<dyn Embedder>,
    pub index: Arc<Mutex<Option<SemanticIndex>>>,
    pub generator: Arc<dyn Generator>,
    pub library_roots: Vec<PathBuf>,
}

pub fn run_completion(
    completion_id: &str,
    request: &CompletionRequest,
    dependencies: &CompletionDependencies,
    session: &mut CompletionSession,
    cancelled: &AtomicBool,
    emit: &mut dyn FnMut(CompletionEvent),
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !completion_id.trim().is_empty() && completion_id.len() <= 128,
        "invalid completion id"
    );
    anyhow::ensure!(
        request.cursor <= request.text.chars().count(),
        "cursor is outside the document"
    );
    validate_scope(&request.scope)?;
    validate_avoid_suggestions(&request.avoid_suggestions)?;
    check_cancelled(cancelled)?;

    let (prefix, suffix) = split_at_char(&request.text, request.cursor);
    anyhow::ensure!(
        !prefix.trim().is_empty(),
        "completion requires a non-empty prefix"
    );
    let mode = classify_mode(suffix);
    let prefix_tail = tail_chars(prefix, 4_000);
    let suffix_head = take_chars(suffix, 2_000);
    let boundary = last_sentence_boundary(prefix);

    let (hyde_query, hyde_vector) = match session.hyde.get(&request.path) {
        Some(cache) if cache.boundary == boundary => (cache.query.clone(), cache.vector.clone()),
        _ => {
            let query = hypothetical_continuation_stream(
                dependencies.generator.as_ref(),
                &prefix_tail,
                &mut |_| {
                    if cancelled.load(Ordering::Relaxed) {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                },
            )?;
            check_cancelled(cancelled)?;
            let vector = first_vector(dependencies.embedder.embed_query(&[&query])?)?;
            session.hyde.insert(
                request.path.clone(),
                HydeCache {
                    boundary,
                    query: query.clone(),
                    vector: vector.clone(),
                },
            );
            (query, vector)
        }
    };

    let sources = match session.retrieval.get(&request.path) {
        Some(cache) if cache.boundary == boundary && cache.scope == request.scope => {
            cache.sources.clone()
        }
        _ => {
            let sources = retrieve_sources(
                request,
                &prefix_tail,
                &hyde_vector,
                dependencies,
                session,
                cancelled,
            )?;
            session.retrieval.insert(
                request.path.clone(),
                RetrievalCache {
                    boundary,
                    scope: request.scope.clone(),
                    sources: sources.clone(),
                },
            );
            sources
        }
    };
    emit(CompletionEvent::Retrieval {
        sources: sources.clone(),
        hyde_query: hyde_query.clone(),
    });
    if sources.is_empty() {
        session.suppress("no_relevant_passages", String::new(), hyde_query);
        emit(CompletionEvent::Suppressed {
            reason: "no_relevant_passages".to_string(),
        });
        return Ok(());
    }

    let (prompt, composition, grounding_text, format) = assemble_prompt(
        request,
        mode,
        &sources,
        dependencies.generator.context_tokens(),
    );
    emit(CompletionEvent::Context { composition });
    check_cancelled(cancelled)?;
    let task_input = GroundedCompletionInput {
        prompt,
        mode,
        prompt_format: format,
        prefix_tail,
        suffix_head,
        grounding_text,
        avoid_suggestions: request.avoid_suggestions.clone(),
        seed: completion_seed(completion_id),
        at_paragraph_start: prefix.trim_end().ends_with("\n\n") || prefix.trim().is_empty(),
        at_sentence_start: prefix
            .trim_end()
            .chars()
            .last()
            .is_some_and(|character| ".!?".contains(character)),
    };
    let mut streamed = String::new();
    let mut attempt = 0;
    let result = loop {
        streamed.clear();
        let mut attempt_input = task_input.clone();
        attempt_input.seed = task_input.seed.wrapping_add(attempt as u64);
        let result = generate_and_verify(
            dependencies.generator.as_ref(),
            &attempt_input,
            &mut |token| {
                if cancelled.load(Ordering::Relaxed) {
                    return ControlFlow::Break(());
                }
                streamed.push_str(token);
                ControlFlow::Continue(())
            },
        );
        if matches!(
            result,
            Err(
                crate::generate::tasks::grounded_completion::SuppressionReason::DuplicateSuggestion
            )
        ) && attempt < MAX_DUPLICATE_RETRIES
            && !cancelled.load(Ordering::Relaxed)
        {
            attempt += 1;
            tracing::info!(
                completion_id,
                attempt,
                next_seed = attempt_input.seed.wrapping_add(1),
                "retrying duplicate grounded completion"
            );
            continue;
        }
        break result;
    };
    match result {
        Ok(generated) => {
            let text = generated.text.trim().to_string();
            session.remember(
                completion_id.to_string(),
                &sources,
                mode,
                format,
                request.scope.mode,
                dependencies.generator.model_id(),
            );
            tracing::info!(completion_id, mode = ?mode, prompt_format = ?format, scope_mode = ?request.scope.mode, retrieval_scores = ?sources.iter().map(|source| source.score).collect::<Vec<_>>(), model = dependencies.generator.model_id(), timings = ?dependencies.generator.last_timings().unwrap_or_else(GenerationTimings::default), "grounded completion shown");
            emit(CompletionEvent::Shown { text, mode });
        }
        Err(_reason) if cancelled.load(Ordering::Relaxed) => anyhow::bail!("completion cancelled"),
        Err(reason) => {
            let reason_text = reason.as_str();
            session.suppress(reason_text, streamed, hyde_query);
            tracing::info!(
                completion_id,
                reason = reason_text,
                mode = ?mode,
                prompt_format = ?format,
                scope_mode = ?request.scope.mode,
                retrieval_scores = ?sources.iter().map(|source| source.score).collect::<Vec<_>>(),
                model = dependencies.generator.model_id(),
                timings = ?dependencies.generator.last_timings().unwrap_or_else(GenerationTimings::default),
                "grounded completion suppressed"
            );
            emit(CompletionEvent::Suppressed {
                reason: reason_text.to_string(),
            });
        }
    }
    Ok(())
}

fn retrieve_sources(
    request: &CompletionRequest,
    prefix_tail: &str,
    hyde_vector: &[f32],
    dependencies: &CompletionDependencies,
    session: &mut CompletionSession,
    cancelled: &AtomicBool,
) -> anyhow::Result<Vec<CompletionSource>> {
    let document_vector =
        working_document_vector(request, dependencies.embedder.as_ref(), session)?;
    let query_vector = blend_query(
        hyde_vector,
        document_vector.as_deref(),
        session.feedback_vector.as_deref(),
    );
    check_cancelled(cancelled)?;
    let pinned = request.scope.pinned.iter().cloned().collect::<HashSet<_>>();
    let excluded = request
        .scope
        .excluded
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let eligible = (request.scope.mode == CompletionScopeMode::Only).then_some(&pinned);
    let dense = {
        let guard = dependencies
            .index
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let index = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("semantic index is not built"))?;
        index.query_scoped_filtered(
            &query_vector,
            DENSE_CANDIDATES,
            SemanticQueryScope::Corpus,
            eligible,
            Some(&excluded),
        )?
    };
    let mut hits = dense
        .into_iter()
        .filter(|chunk| {
            chunk.file_path != request.path && has_meaningful_prompt_evidence(&chunk.chunk_text)
        })
        .map(|chunk| Hit {
            path: chunk.file_path,
            start: chunk.extraction_byte_range.start,
            end: chunk.extraction_byte_range.end,
            chunk_ids: vec![format!(
                "{}:{}",
                chunk.extraction_byte_range.start, chunk.extraction_byte_range.end
            )],
            origin: chunk.origin,
            dense_score: chunk.score,
            fused_score: 0.0,
        })
        .collect::<Vec<_>>();
    fuse_lexical(
        &mut hits,
        prefix_tail,
        &query_vector,
        dependencies,
        &request.path,
        &pinned,
        &excluded,
        request.scope.mode,
        cancelled,
    )?;
    rank_and_threshold(&mut hits, &pinned, request.scope.mode);
    check_cancelled(cancelled)?;
    let sources = expand_sources(&hits, dependencies, &pinned)?;
    Ok(allocate_sources(
        sources,
        request.scope.mode,
        dependencies.generator.context_tokens(),
    ))
}

fn check_cancelled(cancelled: &AtomicBool) -> anyhow::Result<()> {
    anyhow::ensure!(!cancelled.load(Ordering::Relaxed), "completion cancelled");
    Ok(())
}

fn validate_avoid_suggestions(suggestions: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        suggestions.len() <= MAX_AVOID_SUGGESTIONS,
        "too many previous suggestions"
    );
    let mut total = 0;
    for suggestion in suggestions {
        let characters = suggestion.chars().count();
        anyhow::ensure!(
            characters <= MAX_AVOID_SUGGESTION_CHARS,
            "previous suggestion is too long"
        );
        total += characters;
    }
    anyhow::ensure!(
        total <= MAX_AVOID_SUGGESTION_TOTAL_CHARS,
        "previous suggestions are too long"
    );
    Ok(())
}

fn validate_scope(scope: &CompletionScope) -> anyhow::Result<()> {
    let pinned = scope.pinned.iter().cloned().collect::<HashSet<_>>();
    anyhow::ensure!(
        scope.excluded.iter().all(|path| !pinned.contains(path)),
        "completion scope cannot both pin and exclude a document"
    );
    Ok(())
}

fn completion_seed(completion_id: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    completion_id.hash(&mut hasher);
    hasher.finish()
}

fn split_at_char(text: &str, offset: usize) -> (&str, &str) {
    let byte = text
        .char_indices()
        .nth(offset)
        .map_or(text.len(), |(index, _)| index);
    text.split_at(byte)
}

fn take_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}
fn tail_chars(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(limit)).collect()
}

fn classify_mode(suffix: &str) -> CompletionMode {
    let paragraph_suffix = suffix.split("\n\n").next().unwrap_or_default();
    if paragraph_suffix.trim().is_empty() {
        CompletionMode::Append
    } else {
        CompletionMode::Bridge
    }
}

fn last_sentence_boundary(prefix: &str) -> usize {
    let mut boundary = 0;
    for (index, character) in prefix.chars().enumerate() {
        if ".!?\n".contains(character) {
            boundary = index + 1;
        }
    }
    boundary
}

fn first_vector(mut vectors: Vec<Vec<f32>>) -> anyhow::Result<Vec<f32>> {
    vectors
        .pop()
        .ok_or_else(|| anyhow::anyhow!("embedder returned no vector"))
}

fn normalize(vector: &[f32]) -> Vec<f32> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return vector.to_vec();
    }
    vector.iter().map(|value| value / norm).collect()
}

fn mean_normalized(vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
    let dimension = vectors.first()?.len();
    let mut result = vec![0.0; dimension];
    for vector in vectors {
        for (slot, value) in result.iter_mut().zip(normalize(vector)) {
            *slot += value;
        }
    }
    Some(normalize(&result))
}

fn blend_query(hyde: &[f32], document: Option<&[f32]>, feedback: Option<&[f32]>) -> Vec<f32> {
    let mut result = normalize(hyde)
        .into_iter()
        .map(|value| value * 0.65)
        .collect::<Vec<_>>();
    if let Some(document) = document {
        for (slot, value) in result.iter_mut().zip(normalize(document)) {
            *slot += value * 0.25;
        }
    }
    if let Some(feedback) = feedback {
        for (slot, value) in result.iter_mut().zip(normalize(feedback)) {
            *slot += value * 0.10;
        }
    }
    normalize(&result)
}

fn working_document_vector(
    request: &CompletionRequest,
    embedder: &dyn Embedder,
    session: &mut CompletionSession,
) -> anyhow::Result<Option<Vec<f32>>> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    request.text.hash(&mut hasher);
    let hash = hasher.finish();
    if let Some(cache) = session
        .documents
        .get(&request.path)
        .filter(|cache| cache.hash == hash)
    {
        return Ok(Some(cache.vector.clone()));
    }
    let paragraphs = request
        .text
        .split("\n\n")
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    if paragraphs.is_empty() {
        return Ok(None);
    }
    let paragraph_hashes = paragraphs
        .iter()
        .map(|paragraph| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            paragraph.hash(&mut hasher);
            hasher.finish()
        })
        .collect::<Vec<_>>();
    let missing = paragraphs
        .iter()
        .zip(&paragraph_hashes)
        .filter(|(_, hash)| !session.paragraph_vectors.contains_key(hash))
        .map(|(paragraph, hash)| (*paragraph, *hash))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let texts = missing
            .iter()
            .map(|(paragraph, _)| *paragraph)
            .collect::<Vec<_>>();
        let embedded = embedder.embed_passages(&texts)?;
        anyhow::ensure!(
            embedded.len() == missing.len(),
            "embedder returned the wrong number of paragraph vectors"
        );
        for ((_, hash), vector) in missing.into_iter().zip(embedded) {
            session.paragraph_vectors.insert(hash, vector);
        }
    }
    let vectors = paragraph_hashes
        .iter()
        .filter_map(|hash| session.paragraph_vectors.get(hash).cloned())
        .collect::<Vec<_>>();
    let vector =
        mean_normalized(&vectors).ok_or_else(|| anyhow::anyhow!("document embedding was empty"))?;
    session.documents.insert(
        request.path.clone(),
        DocumentVectorCache {
            hash,
            vector: vector.clone(),
        },
    );
    Ok(Some(vector))
}

fn lexical_terms(text: &str) -> Vec<String> {
    let mut terms = text
        .split(|character: char| !character.is_alphanumeric() && character != '-')
        .filter(|term| term.chars().count() >= 4)
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    terms.reverse();
    terms.truncate(8);
    terms.sort();
    terms.dedup();
    terms
}

fn hit_key(hit: &Hit) -> (PathBuf, usize, usize) {
    (hit.path.clone(), hit.start, hit.end)
}

fn cosine(left: &[f32], right: &[f32]) -> f32 {
    let left = normalize(left);
    let right = normalize(right);
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

fn fuse_lexical(
    hits: &mut Vec<Hit>,
    prefix: &str,
    query_vector: &[f32],
    dependencies: &CompletionDependencies,
    working_path: &Path,
    pinned: &HashSet<PathBuf>,
    excluded: &HashSet<PathBuf>,
    mode: CompletionScopeMode,
    cancelled: &AtomicBool,
) -> anyhow::Result<()> {
    let terms = lexical_terms(prefix);
    let dense_rank = hits
        .iter()
        .enumerate()
        .map(|(rank, hit)| (hit_key(hit), rank))
        .collect::<HashMap<_, _>>();
    let mut lexical = Vec::new();
    if !terms.is_empty() {
        let guard = dependencies
            .index
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let index = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("semantic index is not built"))?;
        let mut seen_ids = HashSet::new();
        for root in &dependencies.library_roots {
            for (row, chunk) in index.topic_chunks_for_root(root)?.into_iter().enumerate() {
                if row.is_multiple_of(256) {
                    check_cancelled(cancelled)?;
                }
                if !seen_ids.insert(chunk.chunk_id)
                    || chunk.file_path == working_path
                    || excluded.contains(&chunk.file_path)
                    || (mode == CompletionScopeMode::Only && !pinned.contains(&chunk.file_path))
                {
                    continue;
                }
                let lower = chunk.chunk_text.to_lowercase();
                let matches = terms
                    .iter()
                    .filter(|term| lower.contains(term.as_str()))
                    .count();
                if matches == 0 {
                    continue;
                }
                if !has_meaningful_prompt_evidence(&chunk.chunk_text) {
                    continue;
                }
                let score = cosine(query_vector, &chunk.embedding);
                lexical.push((
                    matches,
                    score,
                    Hit {
                        path: chunk.file_path,
                        start: chunk.extraction_byte_range.start,
                        end: chunk.extraction_byte_range.end,
                        chunk_ids: vec![chunk.chunk_id.to_string()],
                        origin: chunk.origin,
                        dense_score: score,
                        fused_score: 0.0,
                    },
                ));
            }
        }
    }
    lexical.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.total_cmp(&left.1))
    });
    lexical.truncate(DENSE_CANDIDATES);
    let lexical_rank = lexical
        .iter()
        .enumerate()
        .map(|(rank, (_, _, hit))| (hit_key(hit), rank))
        .collect::<HashMap<_, _>>();
    let mut existing = hits.iter().map(hit_key).collect::<HashSet<_>>();
    for (_, _, hit) in lexical {
        if existing.insert(hit_key(&hit)) {
            hits.push(hit);
        }
    }
    for hit in hits.iter_mut() {
        let key = hit_key(hit);
        let rank = dense_rank.get(&key).copied();
        let lexical = lexical_rank.get(&key).copied();
        let dense_rrf = rank.map_or(0.0, |rank| 1.0 / (RRF_K + rank as f32 + 1.0));
        let lexical_rrf = lexical.map_or(0.0, |rank| 1.0 / (RRF_K + rank as f32 + 1.0));
        let boost = if mode == CompletionScopeMode::Prefer && pinned.contains(&hit.path) {
            1.35
        } else {
            1.0
        };
        hit.fused_score = (dense_rrf + lexical_rrf) * boost;
    }
    Ok(())
}

fn rank_and_threshold(hits: &mut Vec<Hit>, pinned: &HashSet<PathBuf>, mode: CompletionScopeMode) {
    hits.sort_by(|left, right| {
        right
            .fused_score
            .total_cmp(&left.fused_score)
            .then_with(|| right.dense_score.total_cmp(&left.dense_score))
    });
    let top = hits.first().map_or(0.0, |hit| hit.dense_score);
    let floor = (top - 0.35).max(0.12);
    hits.retain(|hit| {
        hit.dense_score >= floor
            || (mode == CompletionScopeMode::Prefer
                && pinned.contains(&hit.path)
                && hit.dense_score >= 0.08)
    });
    let mut per_document = HashMap::<PathBuf, usize>::new();
    hits.retain(|hit| {
        let count = per_document.entry(hit.path.clone()).or_default();
        let cap = if pinned.contains(&hit.path) { 6 } else { 3 };
        *count += 1;
        *count <= cap
    });
}

fn safe_byte_floor(text: &str, mut byte: usize) -> usize {
    byte = byte.min(text.len());
    while byte > 0 && !text.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

fn expand_sources(
    hits: &[Hit],
    dependencies: &CompletionDependencies,
    pinned: &HashSet<PathBuf>,
) -> anyhow::Result<Vec<CompletionSource>> {
    let guard = dependencies
        .index
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let index = guard
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("semantic index is not built"))?;
    let mut sources = Vec::new();
    let mut merged = hits.to_vec();
    merged.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.start.cmp(&right.start))
    });
    let mut stitched = Vec::<Hit>::new();
    for hit in merged {
        if let Some(previous) = stitched.last_mut().filter(|previous| {
            previous.path == hit.path && hit.start <= previous.end.saturating_add(512)
        }) {
            previous.end = previous.end.max(hit.end);
            previous.chunk_ids.extend(hit.chunk_ids);
            previous.chunk_ids.sort();
            previous.chunk_ids.dedup();
            if hit.dense_score > previous.dense_score {
                previous.origin = hit.origin;
                previous.dense_score = hit.dense_score;
            }
            previous.fused_score = previous.fused_score.max(hit.fused_score);
        } else {
            stitched.push(hit);
        }
    }
    for hit in &stitched {
        let Some((full_text, _)) = index.indexed_document_for_path(&hit.path)? else {
            continue;
        };
        let start = safe_byte_floor(&full_text, hit.start);
        let end = safe_byte_floor(&full_text, hit.end);
        let before: String = full_text[..start]
            .chars()
            .rev()
            .take(MAX_PASSAGE_CHARS / 2)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let after: String = full_text[end..]
            .chars()
            .take(MAX_PASSAGE_CHARS / 2)
            .collect();
        let combined = format!("{before}{}{after}", &full_text[start..end]);
        let passage = paragraph_window(&combined, MAX_PASSAGE_CHARS);
        let Some(passage) = clean_prompt_passage(&passage) else {
            continue;
        };
        let page = match hit.origin {
            SourceOrigin::PdfPage { page, .. } => Some(page),
            _ => None,
        };
        let title = hit
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("Untitled")
            .to_string();
        sources.push(CompletionSource {
            path: hit.path.clone(),
            title,
            page,
            chunk_ids: hit.chunk_ids.clone(),
            score: hit.dense_score,
            pinned: pinned.contains(&hit.path),
            passage,
        });
    }
    sources.sort_by(|left, right| left.score.total_cmp(&right.score));
    Ok(sources)
}

fn paragraph_window(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.trim().to_string();
    }
    let clipped = text.chars().take(max_chars).collect::<String>();
    let bounded_end = clipped
        .rsplit_once("\n\n")
        .map_or(clipped.as_str(), |(head, _)| head);
    bounded_end
        .split_once("\n\n")
        .map_or(bounded_end, |(_, body)| body)
        .trim()
        .to_string()
}

#[derive(Default)]
struct EvidenceStats {
    alphabetic_chars: usize,
    digit_chars: usize,
    prose_words: usize,
    numeric_tokens: usize,
    compact_code_tokens: usize,
}

impl EvidenceStats {
    fn add(&mut self, other: &Self) {
        self.alphabetic_chars += other.alphabetic_chars;
        self.digit_chars += other.digit_chars;
        self.prose_words += other.prose_words;
        self.numeric_tokens += other.numeric_tokens;
        self.compact_code_tokens += other.compact_code_tokens;
    }
}

fn evidence_stats(text: &str) -> EvidenceStats {
    let mut stats = EvidenceStats {
        alphabetic_chars: text
            .chars()
            .filter(|character| character.is_alphabetic())
            .count(),
        digit_chars: text
            .chars()
            .filter(|character| character.is_numeric())
            .count(),
        ..EvidenceStats::default()
    };
    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '.' | '-' | '+')
        });
        if token.is_empty() {
            continue;
        }
        let numeric = token
            .strip_suffix('%')
            .unwrap_or(token)
            .parse::<f64>()
            .is_ok();
        if numeric {
            stats.numeric_tokens += 1;
            continue;
        }
        let alphabetic = token
            .chars()
            .filter(|character| character.is_alphabetic())
            .count();
        let has_digit = token.chars().any(|character| character.is_numeric());
        let compact_code = has_digit
            && token.chars().count() <= 8
            && alphabetic <= 4
            && token
                .chars()
                .filter(|character| character.is_alphabetic())
                .all(|character| character.is_uppercase());
        if compact_code {
            stats.compact_code_tokens += 1;
        } else if alphabetic >= 2 && !has_digit {
            stats.prose_words += 1;
        }
    }
    stats
}

fn stats_are_layout_noise(stats: &EvidenceStats) -> bool {
    let layout_tokens = stats.numeric_tokens + stats.compact_code_tokens;
    if stats.prose_words == 0 && layout_tokens > 0 {
        return true;
    }
    if layout_tokens >= 3 && layout_tokens > stats.prose_words.saturating_mul(2) {
        return true;
    }
    stats.digit_chars > stats.alphabetic_chars.saturating_mul(2) && stats.prose_words < 4
}

fn is_layout_noise_line(line: &str) -> bool {
    stats_are_layout_noise(&evidence_stats(line))
}

fn has_meaningful_prompt_evidence(text: &str) -> bool {
    let mut stats = EvidenceStats::default();
    let mut has_prose_line = false;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let line_stats = evidence_stats(line);
        if stats_are_layout_noise(&line_stats) {
            continue;
        }
        has_prose_line |= line_stats.prose_words >= 5
            || (line_stats.prose_words >= 3
                && line
                    .chars()
                    .last()
                    .is_some_and(|character| matches!(character, '.' | '!' | '?' | ';' | ':')));
        stats.add(&line_stats);
    }
    has_prose_line
        && stats.alphabetic_chars >= 12
        && stats.prose_words >= 3
        && stats.numeric_tokens + stats.compact_code_tokens <= stats.prose_words.saturating_mul(2)
}

/// Remove PDF layout artifacts that have no usable linear reading while preserving
/// prose, including prose that contains ordinary statistical values. This is a
/// completion-only evidence boundary: the semantic index and exact search retain
/// their original extracted text.
fn clean_prompt_passage(text: &str) -> Option<String> {
    let mut cleaned = String::new();
    let mut pending_break = false;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            pending_break = !cleaned.is_empty();
            continue;
        }
        if is_layout_noise_line(line) {
            continue;
        }
        if !cleaned.is_empty() {
            cleaned.push(if pending_break { '\n' } else { ' ' });
        }
        cleaned.push_str(line);
        pending_break = false;
    }
    has_meaningful_prompt_evidence(&cleaned).then_some(cleaned)
}

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

fn allocate_sources(
    mut sources: Vec<CompletionSource>,
    mode: CompletionScopeMode,
    window: usize,
) -> Vec<CompletionSource> {
    let budget = window.saturating_sub(700) / 2;
    let reserved = if mode == CompletionScopeMode::Prefer {
        budget / 2
    } else {
        0
    };
    let mut chosen = Vec::new();
    let mut used = 0;
    if reserved > 0 {
        let mut represented = HashSet::new();
        // Give every relevant pinned document one seat before allowing a
        // prolific pinned document to consume the reserved half by itself.
        for source in sources.iter().filter(|source| source.pinned).rev() {
            if !represented.insert(source.path.clone()) {
                continue;
            }
            let tokens = estimate_tokens(&source.passage);
            if used + tokens <= reserved {
                chosen.push(source.clone());
                used += tokens;
            }
        }
        for source in sources.iter().filter(|source| source.pinned).rev() {
            if chosen
                .iter()
                .any(|chosen| chosen.path == source.path && chosen.chunk_ids == source.chunk_ids)
            {
                continue;
            }
            let tokens = estimate_tokens(&source.passage);
            if used + tokens <= reserved {
                chosen.push(source.clone());
                used += tokens;
            }
        }
    }
    for source in sources.drain(..).rev() {
        if chosen
            .iter()
            .any(|chosen| chosen.chunk_ids == source.chunk_ids && chosen.path == source.path)
        {
            continue;
        }
        let tokens = estimate_tokens(&source.passage);
        if used + tokens <= budget {
            chosen.push(source);
            used += tokens;
        }
    }
    chosen.sort_by(|left, right| left.score.total_cmp(&right.score));
    chosen
}

fn assemble_prompt(
    request: &CompletionRequest,
    mode: CompletionMode,
    sources: &[CompletionSource],
    window: usize,
) -> (String, ContextComposition, String, PromptFormat) {
    let avoidance = if request.avoid_suggestions.is_empty() {
        String::new()
    } else {
        format!(
            "[Previous suggestions to avoid]\n{}\n\n",
            request
                .avoid_suggestions
                .iter()
                .enumerate()
                .map(|(index, suggestion)| format!("{}. {}", index + 1, suggestion.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let avoidance_tokens = estimate_tokens(&avoidance);
    let available = window.saturating_sub(700 + avoidance_tokens);
    let retrieval_tokens = sources
        .iter()
        .map(|source| estimate_tokens(&source.passage) + 16)
        .sum::<usize>();
    let doc_budget = available
        .saturating_sub(retrieval_tokens)
        .max(available / 2)
        .min(available);
    let (document, coverage) = fit_document(&request.text, request.cursor, doc_budget, mode);
    let format = if mode == CompletionMode::Bridge {
        PromptFormat::InstructInfill
    } else {
        PromptFormat::InstructContinue
    };
    let mut prompt = String::from("Complete the working document at <CURSOR>. Use only facts supported by the labeled library passages and the working document. Match its register. Advance the document with new information or reasoning; do not restate or paraphrase claims already made in the working document. Output only the missing continuation, at most two sentences. Do not cite or mention source labels.\n\n");
    if !avoidance.is_empty() {
        prompt.push_str("Produce a different continuation from every previous suggestion below; do not repeat or paraphrase them.\n\n");
        prompt.push_str(&avoidance);
    }
    for source in sources {
        prompt.push_str(&format!(
            "[Source: {}, {}]\n{}\n\n",
            source.title,
            source
                .page
                .map_or_else(|| "text".to_string(), |page| format!("p.{page}")),
            source.passage
        ));
    }
    prompt.push_str("[Working document]\n");
    prompt.push_str(&document);
    prompt.push_str("\n\nCompletion:");
    let doc_tokens = estimate_tokens(&document);
    let composition = ContextComposition {
        window_tokens: window,
        used_tokens: estimate_tokens(&prompt).min(window),
        doc_coverage: coverage,
        retrieval_tokens,
        doc_tokens,
        scope_mode: request.scope.mode,
    };
    let grounding_text = format!(
        "{}\n{}",
        request.text,
        sources
            .iter()
            .map(|source| source.passage.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    (prompt, composition, grounding_text, format)
}

fn fit_document(
    text: &str,
    cursor: usize,
    budget_tokens: usize,
    mode: CompletionMode,
) -> (String, DocumentCoverage) {
    let max_chars = budget_tokens.saturating_mul(4);
    let (prefix, suffix) = split_at_char(text, cursor);
    let marker = "\n\n[...]\n\n";
    let mode_suffix = if mode == CompletionMode::Bridge {
        format!("<CURSOR>{}", suffix)
    } else {
        "<CURSOR>".to_string()
    };
    if prefix.chars().count() + mode_suffix.chars().count() <= max_chars {
        return (format!("{prefix}{mode_suffix}"), DocumentCoverage::Full);
    }
    let remaining = max_chars.saturating_sub(marker.chars().count() + mode_suffix.chars().count());
    let head_chars = remaining / 3;
    let tail_chars_count = remaining.saturating_sub(head_chars);
    let head = prefix.chars().take(head_chars).collect::<String>();
    let tail = tail_chars(prefix, tail_chars_count);
    (
        format!(
            "{}{marker}{}{mode_suffix}",
            trim_to_paragraph_end(&head),
            trim_to_paragraph_start(&tail)
        ),
        DocumentCoverage::Elided {
            head_tokens: estimate_tokens(&head),
            tail_tokens: estimate_tokens(&tail),
        },
    )
}

fn trim_to_paragraph_end(text: &str) -> &str {
    text.rsplit_once("\n\n").map_or(text, |(head, _)| head)
}
fn trim_to_paragraph_start(text: &str) -> &str {
    text.split_once("\n\n").map_or(text, |(_, tail)| tail)
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::embed::index::chunk::Chunk;
    use crate::embed::index::db::PreparedFile;
    use crate::embed::Embedder;
    use crate::generate::mock::MockGenerator;
    use crate::types::{ByteRange, EmbeddingEngine};

    struct TestEmbedder;

    impl Embedder for TestEmbedder {
        fn embedding_space_identity(&self) -> crate::embed::EmbeddingSpaceIdentity {
            crate::embed::EmbeddingSpaceIdentity::for_test(
                self.engine(),
                self.model_id(),
                self.dimension(),
            )
        }

        fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }

        fn model_id(&self) -> &str {
            "test-embedder"
        }

        fn dimension(&self) -> usize {
            2
        }

        fn engine(&self) -> EmbeddingEngine {
            EmbeddingEngine::Candle
        }
    }

    struct FailingEmbedder;

    impl Embedder for FailingEmbedder {
        fn embedding_space_identity(&self) -> crate::embed::EmbeddingSpaceIdentity {
            crate::embed::EmbeddingSpaceIdentity::for_test(
                self.engine(),
                self.model_id(),
                self.dimension(),
            )
        }

        fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("scripted embedding failure")
        }

        fn model_id(&self) -> &str {
            "failing-embedder"
        }

        fn dimension(&self) -> usize {
            2
        }

        fn engine(&self) -> EmbeddingEngine {
            EmbeddingEngine::Candle
        }
    }

    #[test]
    fn cursor_split_and_mode_are_unicode_safe() {
        let (prefix, suffix) = split_at_char("á🙂中", 2);
        assert_eq!(prefix, "á🙂");
        assert_eq!(suffix, "中");
        assert_eq!(classify_mode("\n\nnext"), CompletionMode::Append);
        assert_eq!(classify_mode(" remaining"), CompletionMode::Bridge);
    }

    #[test]
    fn legacy_completion_scope_defaults_to_no_exclusions() {
        let scope: CompletionScope = serde_json::from_value(serde_json::json!({
            "mode": "prefer",
            "pinned": ["/library/source.pdf"]
        }))
        .unwrap();

        assert_eq!(scope.mode, CompletionScopeMode::Prefer);
        assert_eq!(scope.pinned, vec![PathBuf::from("/library/source.pdf")]);
        assert!(scope.excluded.is_empty());
    }

    #[test]
    fn completion_scope_rejects_a_document_that_is_both_pinned_and_excluded() {
        let source = PathBuf::from("/library/source.pdf");
        let scope = CompletionScope {
            mode: CompletionScopeMode::Prefer,
            pinned: vec![source.clone()],
            excluded: vec![source],
        };

        assert!(validate_scope(&scope).is_err());
    }

    #[test]
    fn document_elision_keeps_head_tail_and_cursor() {
        let text = format!("opening\n\n{}\n\nending", "middle ".repeat(100));
        let (fitted, coverage) =
            fit_document(&text, text.chars().count(), 30, CompletionMode::Append);
        assert!(fitted.contains("opening"));
        assert!(fitted.contains("ending"));
        assert!(fitted.ends_with("<CURSOR>"));
        assert!(matches!(coverage, DocumentCoverage::Elided { .. }));
    }

    #[test]
    fn query_blending_stays_normalized() {
        let blended = blend_query(&[1.0, 0.0], Some(&[0.0, 1.0]), Some(&[-1.0, 0.0]));
        let norm = blended
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn synthesis_prompt_requires_information_gain_and_lists_prior_suggestions() {
        let request = CompletionRequest {
            path: PathBuf::from("draft.txt"),
            text: "The existing claim is complete.".into(),
            cursor: "The existing claim is complete.".chars().count(),
            scope: CompletionScope::default(),
            avoid_suggestions: vec!["A previously rejected continuation.".into()],
        };
        let sources = vec![CompletionSource {
            path: PathBuf::from("source.txt"),
            title: "Source".into(),
            page: None,
            chunk_ids: vec!["1".into()],
            score: 0.8,
            pinned: false,
            passage: "A source contains a distinct supporting detail.".into(),
        }];
        let (prompt, _, _, _) = assemble_prompt(&request, CompletionMode::Append, &sources, 4_096);

        assert!(prompt.contains("Advance the document with new information or reasoning"));
        assert!(prompt.contains("do not restate or paraphrase claims already made"));
        assert!(prompt.contains("[Previous suggestions to avoid]"));
        assert!(prompt.contains("A previously rejected continuation."));
    }

    #[test]
    fn numeric_figure_matrix_is_not_meaningful_prompt_evidence() {
        let matrix = "0.47 0.45 0.21 0.06 0.07 0.09 0.11 0.04 0.01 0.01\n\
0.06 0.05 0.02 0.01\n\
0\n\
0 .94 0.93\n\
0.4\n\
0.24 0.13\n\
0.4\n\
0.41 0.13 0.06 0.02\n\
0.01 0.01\n\
0\n\
0\n\
0\n\
0.49 0.45 0.22 0.08\n\
0.1\n\
0 .1\n\
0.11 0.04 0.01 0.02\n\
0.1\n\
0.11 0.02 0.02\n\
0\n\
0.87 0.91 0.32";

        assert!(!has_meaningful_prompt_evidence(matrix));
        assert_eq!(clean_prompt_passage(matrix), None);
    }

    #[test]
    fn passage_cleaning_removes_layout_runs_but_keeps_caption_and_statistical_prose() {
        let passage = "R1 R2 R3\n\
F1 F2 F3 F4 F5 F1 F2 F3 F4 F5\n\
0.47 0.45 0.21 0.06 0.07 0.09 0.11 0.04 0.01 0.01\n\
Fig. 2. Visualization of the multiverse of p values across processing choices.\n\
The estimated effect was 0.47, with a 95% confidence interval from 0.21 to 0.73.";

        let cleaned = clean_prompt_passage(passage).unwrap();

        assert!(!cleaned.contains("R1 R2 R3"));
        assert!(!cleaned.contains("0.47 0.45 0.21"));
        assert!(cleaned.contains("Fig. 2. Visualization"));
        assert!(cleaned.contains("The estimated effect was 0.47"));
    }

    #[test]
    fn figure_heading_without_explanatory_prose_is_rejected() {
        let passage = "Religiosity (Study 2)\n\
NMO1 NMO2 NMO3 ECL1 ECL2 ECL3\n\
0.01 0.04 0.93 0.76 0.99 0.52 0.78";

        assert!(!has_meaningful_prompt_evidence(passage));
        assert_eq!(clean_prompt_passage(passage), None);
    }

    #[test]
    fn end_to_end_pipeline_never_shows_without_provenance_and_records_feedback() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let source_path = root.join("source.txt");
        let source_text = "0.47 0.45 0.21 0.06 0.07 0.09 0.11 0.04 0.01 0.01\n\
Cache entries expire when the underlying record changes.\n\
R1 R2 R3 F1 F2 F3 F4 F5";
        std::fs::write(&source_path, source_text).unwrap();
        let source_path = std::fs::canonicalize(source_path).unwrap();
        let excluded_path = root.join("excluded.txt");
        let excluded_text = "Excluded evidence must never reach the completion prompt.";
        std::fs::write(&excluded_path, excluded_text).unwrap();
        let excluded_path = std::fs::canonicalize(excluded_path).unwrap();
        let mut index = SemanticIndex::create(
            root,
            "test-embedder",
            2,
            EmbeddingEngine::Candle,
            Some(root),
        )
        .unwrap();
        index
            .write_file(PreparedFile {
                regions: Vec::new(),
                path: source_path.clone(),
                full_text: source_text.to_string(),
                chunks: vec![(
                    Chunk {
                        text: source_text.to_string(),
                        byte_range: ByteRange {
                            start: 0,
                            end: source_text.len(),
                        },
                        origin: SourceOrigin::TextFile { line: 1, col: 1 },
                        file_path: source_path.clone(),
                    },
                    vec![1.0, 0.0],
                )],
            })
            .unwrap();
        index
            .write_file(PreparedFile {
                regions: Vec::new(),
                path: excluded_path.clone(),
                full_text: excluded_text.to_string(),
                chunks: vec![(
                    Chunk {
                        text: excluded_text.to_string(),
                        byte_range: ByteRange {
                            start: 0,
                            end: excluded_text.len(),
                        },
                        origin: SourceOrigin::TextFile { line: 1, col: 1 },
                        file_path: excluded_path.clone(),
                    },
                    vec![1.0, 0.0],
                )],
            })
            .unwrap();

        let embedder: Arc<dyn Embedder> = Arc::new(TestEmbedder);
        let generator = Arc::new(MockGenerator::scripted([
            "Cache entries expire after a source change.",
            "The cache entry therefore expires.",
            "A changed record invalidates the cached value.",
        ]));
        let dependencies = CompletionDependencies {
            embedder: Arc::clone(&embedder),
            index: Arc::new(Mutex::new(Some(index))),
            generator: Arc::clone(&generator) as Arc<dyn Generator>,
            library_roots: vec![root.to_path_buf()],
        };
        let request = CompletionRequest {
            path: root.join("draft.txt"),
            text: "Cache behavior matters.".to_string(),
            cursor: "Cache behavior matters.".chars().count(),
            scope: CompletionScope {
                excluded: vec![excluded_path.clone()],
                ..CompletionScope::default()
            },
            avoid_suggestions: vec!["The cache entry therefore expires.".into()],
        };
        let mut session = CompletionSession::default();
        let mut events = Vec::new();
        run_completion(
            "completion-1",
            &request,
            &dependencies,
            &mut session,
            &AtomicBool::new(false),
            &mut |event| events.push(event),
        )
        .unwrap();

        let sources = events
            .iter()
            .find_map(|event| match event {
                CompletionEvent::Retrieval { sources, .. } => Some(sources),
                _ => None,
            })
            .unwrap();
        assert!(!sources.is_empty());
        assert!(sources.iter().all(|source| source.path != excluded_path));
        assert!(events.iter().any(|event| matches!(
            event,
            CompletionEvent::Shown { text, .. }
                if text == "A changed record invalidates the cached value."
        )));
        let requests = generator.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests[1]
            .prompt
            .contains("Cache entries expire when the underlying record changes."));
        assert!(!requests[1].prompt.contains("0.47 0.45 0.21"));
        assert!(!requests[1].prompt.contains("R1 R2 R3"));
        assert!(!requests[1].prompt.contains(excluded_text));
        assert_ne!(requests[1].sampling.seed, requests[2].sampling.seed);
        let failed_plan = session
            .prepare_feedback("completion-1", CompletionFeedback::Accepted)
            .unwrap();
        assert!(failed_plan.embed(&FailingEmbedder).is_err());
        // Preparing does not consume the record, so transient embedding
        // failures remain retryable.
        let plan = session
            .prepare_feedback("completion-1", CompletionFeedback::Accepted)
            .unwrap();
        let vectors = plan.embed(embedder.as_ref()).unwrap();
        session.apply_feedback(plan, vectors).unwrap();
        assert_eq!(session.steering().documents[0].path, source_path);
        assert!(session.steering().documents[0].weight > 0.0);
    }
}
