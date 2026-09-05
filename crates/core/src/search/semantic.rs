use std::sync::{Arc, Mutex};

use crate::extract::ExtractorRegistry;
use crate::types::{
    FileMatches, IndexingConfig, Match, RetrievalSettings, SearchCapabilities, SearchDocument,
    SearchQuery, SearchScope, SourceOrigin,
};
use tracing::{error, info, warn};

use super::{
    document_field_matches, field_matcher, prioritize_and_limit_results, SearchOutcome,
    SearchProvider, SearchResultTx,
};
use crate::embed::index::{SemanticIndex, SemanticQueryScope};
use crate::embed::Embedder;
use crate::generate::tasks::hypothetical_document;
use crate::generate::Generator;

pub struct SemanticSearchProvider {
    embedder: Arc<dyn Embedder>,
    index: Arc<Mutex<Option<SemanticIndex>>>,
    indexing: IndexingConfig,
    /// Query-vector enhancement (HyDE, PRF). Default is all-off, so a provider
    /// built with `new` alone behaves exactly as it did before these features.
    retrieval: RetrievalSettings,
    /// Only consulted for HyDE, and only when the setting is enabled. `None`
    /// means no generation model is loaded.
    generator: Option<Arc<dyn Generator>>,
}

impl SemanticSearchProvider {
    pub fn new(
        embedder: Arc<dyn Embedder>,
        index: Arc<Mutex<Option<SemanticIndex>>>,
        indexing: IndexingConfig,
    ) -> Self {
        Self {
            embedder,
            index,
            indexing,
            retrieval: RetrievalSettings::default(),
            generator: None,
        }
    }

    /// Attach query-vector enhancement. The generator is only needed for HyDE;
    /// pass `None` when generation is unavailable and HyDE will degrade to the
    /// raw query (and log a warning) rather than failing the search.
    pub fn with_retrieval(
        mut self,
        retrieval: RetrievalSettings,
        generator: Option<Arc<dyn Generator>>,
    ) -> Self {
        self.retrieval = retrieval;
        self.generator = generator;
        self
    }

    /// Embed the raw query string into a single vector (question space).
    fn embed_query_vector(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let vecs = self.embedder.embed_query(&[text]).map_err(|e| {
            error!("[semantic] embed error: {e:#}");
            e
        })?;
        vecs.into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("Embedder returned no vector for the query"))
    }

    /// HyDE: replace the question-space vector with the mean of the (optional)
    /// query vector and one or more embedded LLM-generated hypothetical
    /// passages, moving the search into document space.
    ///
    /// Best-effort by contract: the base vector is the *input* this transforms,
    /// not a competing implementation, so any failure logs a warning and
    /// returns the un-transformed base vector.
    fn apply_hyde(&self, query_text: &str, base: Vec<f32>) -> (Vec<f32>, Vec<String>) {
        let hyde = &self.retrieval.hyde;
        if !hyde.enabled {
            return (base, Vec::new());
        }

        let Some(generator) = self.generator.as_deref() else {
            warn!(
                "[semantic] HyDE is enabled but no generation model is loaded; searched with the raw query"
            );
            return (base, Vec::new());
        };

        let passages = match hypothetical_document::hypothetical_documents(
            generator,
            query_text,
            hyde.hypotheticals,
        ) {
            Ok(passages) => passages,
            Err(e) => {
                warn!("[semantic] HyDE generation failed; searched with the raw query: {e:#}");
                return (base, Vec::new());
            }
        };

        let refs: Vec<&str> = passages.iter().map(String::as_str).collect();
        let passage_vecs = match self.embedder.embed_passages(&refs) {
            Ok(vecs) => vecs,
            Err(e) => {
                warn!(
                    "[semantic] HyDE passage embedding failed; searched with the raw query: {e:#}"
                );
                return (base, Vec::new());
            }
        };

        // An embedder must return exactly one vector per input passage. If it
        // violates that contract, no generated passage can be truthfully
        // identified as part of the final query vector, so degrade atomically.
        if passage_vecs.len() != passages.len() {
            warn!(
                "[semantic] HyDE embedder returned {} vector(s) for {} passage(s); searched with the raw query",
                passage_vecs.len(),
                passages.len()
            );
            return (base, Vec::new());
        }

        // Normalise each component so the mean is a direction, not dominated by
        // whichever vector happens to have the largest magnitude.
        let mut components: Vec<Vec<f32>> = Vec::with_capacity(passage_vecs.len() + 1);
        if hyde.include_query {
            components.push(normalize(&base));
        }
        for vec in &passage_vecs {
            components.push(normalize(vec));
        }

        match mean_vector(&components) {
            Some(mean) => {
                info!(
                    "[semantic] HyDE: query vector shifted using {} hypothetical passage(s)",
                    passage_vecs.len()
                );
                (normalize(&mean), passages)
            }
            None => (base, Vec::new()),
        }
    }

    /// Pseudo-relevance feedback (Rocchio). Runs an initial retrieval with `q0`,
    /// treats the top hits as pseudo-relevant, and folds their centroid back
    /// into the vector: `q1 = α·q0 + β·centroid`.
    ///
    /// An empty initial result set is a no-op, not an error. Invalid zero
    /// weights and embedding failures log a warning and return `q0` unchanged.
    fn apply_prf(
        &self,
        q0: &[f32],
        query: &SearchQuery,
        eligible_paths: Option<&std::collections::HashSet<std::path::PathBuf>>,
    ) -> anyhow::Result<Vec<f32>> {
        let prf = &self.retrieval.pseudo_relevance_feedback;
        if !prf.enabled || prf.feedback_docs == 0 {
            return Ok(q0.to_vec());
        }
        if prf.alpha == 0.0 && prf.beta == 0.0 {
            warn!("[semantic] PRF alpha and beta are both zero; searched without feedback");
            return Ok(q0.to_vec());
        }

        let feedback = {
            let guard = self.index.lock().unwrap();
            let idx = guard
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Semantic index is not built yet"))?;
            idx.query_scoped_filtered(
                q0,
                prf.feedback_docs,
                query_scope(query),
                eligible_paths,
                None,
            )?
        };
        if feedback.is_empty() {
            return Ok(q0.to_vec());
        }

        let texts: Vec<&str> = feedback
            .iter()
            .map(|chunk| chunk.chunk_text.as_str())
            .collect();
        let feedback_vecs = match self.embedder.embed_passages(&texts) {
            Ok(vecs) => vecs,
            Err(e) => {
                warn!("[semantic] PRF feedback embedding failed; searched without feedback: {e:#}");
                return Ok(q0.to_vec());
            }
        };

        let normalized: Vec<Vec<f32>> = feedback_vecs.iter().map(|vec| normalize(vec)).collect();
        let Some(centroid) = mean_vector(&normalized) else {
            return Ok(q0.to_vec());
        };

        let q1 = rocchio(&normalize(q0), &centroid, prf.alpha, prf.beta);
        info!(
            "[semantic] PRF: query vector refined from {} feedback passage(s)",
            feedback_vecs.len()
        );
        Ok(normalize(&q1))
    }
}

/// The nearest-neighbour scope for a query. Rebuilt per call because it borrows
/// the query's paths and is consumed by each index lookup.
fn query_scope(query: &SearchQuery) -> SemanticQueryScope<'_> {
    match &query.scope {
        SearchScope::Corpus => SemanticQueryScope::Root(&query.root),
        SearchScope::All => SemanticQueryScope::Corpus,
        SearchScope::File { path } => SemanticQueryScope::File(path),
    }
}

/// L2-normalise a vector; a zero vector normalises to itself.
fn normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|value| value / norm).collect()
}

/// Element-wise mean of equal-length vectors. `None` for an empty input.
fn mean_vector(vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
    let dim = vectors.first()?.len();
    let mut acc = vec![0.0f32; dim];
    for vec in vectors {
        for (slot, value) in acc.iter_mut().zip(vec) {
            *slot += value;
        }
    }
    let count = vectors.len() as f32;
    for slot in acc.iter_mut() {
        *slot /= count;
    }
    Some(acc)
}

/// Rocchio combination `alpha*query + beta*centroid`.
fn rocchio(query: &[f32], centroid: &[f32], alpha: f32, beta: f32) -> Vec<f32> {
    query
        .iter()
        .zip(centroid)
        .map(|(q, c)| alpha * q + beta * c)
        .collect()
}

impl SearchProvider for SemanticSearchProvider {
    fn search(
        &self,
        query: &SearchQuery,
        _extractors: &ExtractorRegistry,
        tx: SearchResultTx,
        documents: &[SearchDocument],
    ) -> anyhow::Result<SearchOutcome> {
        // 1. Reconcile the index with the current root before returning semantic
        // results. This blocks the first stale search so callers never see known-
        // stale paths after offline creates/renames/deletes.
        let reconcile_errors = if query.scope == SearchScope::All {
            Vec::new()
        } else {
            let mut guard = self.index.lock().unwrap();
            let idx = guard
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("Semantic index is not built yet"))?;
            idx.reconcile_root(
                &query.root,
                _extractors,
                self.embedder.as_ref(),
                &self.indexing,
            )?
        };

        use std::collections::HashMap;
        let documents_by_path = documents
            .iter()
            .map(|document| (document.path.clone(), document))
            .collect::<HashMap<_, _>>();
        let mut file_order: Vec<std::path::PathBuf> = Vec::new();
        let mut by_file: HashMap<std::path::PathBuf, FileMatches> = HashMap::new();
        let field_matcher = field_matcher(query)?;
        let mut field_match_count = 0usize;
        for document in documents {
            let field_matches = document_field_matches(&field_matcher, document)?;
            if field_matches.is_empty() {
                continue;
            }
            field_match_count += field_matches.len();
            file_order.push(document.path.clone());
            by_file.insert(
                document.path.clone(),
                FileMatches {
                    path: document.path.clone(),
                    file_type: document.file_type.clone(),
                    title: document.title.clone(),
                    field_matches,
                    matches: Vec::new(),
                    evidence: Vec::new(),
                },
            );
        }

        // Direct identity hits own the first part of the global budget. When
        // they exhaust it, avoid query embedding and vector retrieval entirely.
        if query.max_results > 0 && field_match_count >= query.max_results {
            let ordered = file_order
                .into_iter()
                .filter_map(|path| by_file.remove(&path))
                .collect();
            for result in prioritize_and_limit_results(ordered, query.max_results) {
                if tx.is_closed() || tx.blocking_send(result).is_err() {
                    break;
                }
            }
            return Ok(SearchOutcome {
                errors: reconcile_errors,
                hyde_documents: Vec::new(),
                files_scanned: None,
                indexed_pdf_reads: 0,
                live_pdf_fallbacks: 0,
                index_unavailable_fallbacks: 0,
            });
        }

        // 2. Form the query vector. The raw query embedding is the base; HyDE
        // then PRF (each optional) reshape it before the authoritative lookup.
        // Neither adds a ranking stage after retrieval — the index still owns
        // relevance.
        info!("[semantic] embedding query...");
        let base_vec = self.embed_query_vector(query.pattern.as_str())?;
        let (hyde_vec, hyde_documents) = self.apply_hyde(query.pattern.as_str(), base_vec);
        let eligible_paths = documents
            .iter()
            .map(|document| document.path.clone())
            .collect::<std::collections::HashSet<_>>();
        let query_vec = self.apply_prf(&hyde_vec, query, Some(&eligible_paths))?;
        info!("[semantic] query vector ready, running index query");

        // 3. Lock the index and run the nearest-neighbour query.
        let guard = self.index.lock().unwrap();
        let idx = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Semantic index is not built yet"))?;

        let top_k = if query.max_results == 0 {
            0
        } else {
            query.max_results - field_match_count
        };
        let results = idx.query_scoped_filtered(
            &query_vec,
            top_k,
            query_scope(query),
            Some(&eligible_paths),
            None,
        )?;
        drop(guard);

        // 4. Seed direct filename/title hits in catalog order, then merge
        // score-ranked content chunks into those files or append content-only
        // files. This guarantees one result object per document.
        for chunk in results {
            let Some(document) = documents_by_path.get(&chunk.file_path) else {
                continue;
            };

            let text_range = match &chunk.origin {
                SourceOrigin::TextFile { .. } => Some(chunk.extraction_byte_range.clone()),
                SourceOrigin::PdfPage { .. } => None,
            };

            let m = Match {
                text_range,
                matched_text: chunk.chunk_text.clone(),
                context_before: String::new(),
                context_after: String::new(),
                origin: chunk.origin,
                score: Some(chunk.score),
            };

            if !by_file.contains_key(&chunk.file_path) {
                file_order.push(chunk.file_path.clone());
                by_file.insert(
                    chunk.file_path.clone(),
                    FileMatches {
                        path: chunk.file_path.clone(),
                        file_type: document.file_type.clone(),
                        title: document.title.clone(),
                        field_matches: Vec::new(),
                        matches: Vec::new(),
                        evidence: Vec::new(),
                    },
                );
            }
            by_file.get_mut(&chunk.file_path).unwrap().matches.push(m);
        }

        let ordered = file_order
            .into_iter()
            .filter_map(|path| by_file.remove(&path))
            .collect();
        for fm in prioritize_and_limit_results(ordered, query.max_results) {
            if tx.is_closed() {
                break;
            }
            if tx.blocking_send(fm).is_err() {
                break;
            }
        }

        Ok(SearchOutcome {
            errors: reconcile_errors,
            hyde_documents,
            files_scanned: None,
            indexed_pdf_reads: 0,
            live_pdf_fallbacks: 0,
            index_unavailable_fallbacks: 0,
        })
    }

    fn capabilities(&self) -> SearchCapabilities {
        let index_built = self.index.lock().map(|g| g.is_some()).unwrap_or(false);

        SearchCapabilities {
            supports_regex: false,
            supports_case_sensitivity: false,
            is_indexed: true,
            supported_file_types: self.indexing.supported_extensions.clone(),
            requires_index: true,
            semantic_index_built: index_built,
            supported_engines: crate::types::EmbeddingEngine::supported_engines(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct MockEmbedder;
    impl Embedder for MockEmbedder {
        fn embedding_space_identity(&self) -> crate::embed::EmbeddingSpaceIdentity {
            crate::embed::EmbeddingSpaceIdentity::for_test(
                self.engine(),
                self.model_id(),
                self.dimension(),
            )
        }

        fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(vec![vec![1.0; 768]; texts.len()])
        }
        fn model_id(&self) -> &str {
            "mock"
        }
        fn dimension(&self) -> usize {
            768
        }
        fn engine(&self) -> crate::types::EmbeddingEngine {
            crate::types::EmbeddingEngine::Candle
        }
    }

    fn indexing_config(extensions: Vec<String>) -> IndexingConfig {
        IndexingConfig {
            chunk_size: 100,
            chunk_overlap: 0,
            supported_extensions: extensions,
        }
    }

    /// Records how many times it was asked to generate, so HyDE wiring can be
    /// asserted without a real model.
    struct ScriptedGenerator {
        reply: String,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ScriptedGenerator {
        fn new(reply: &str) -> Self {
            Self {
                reply: reply.to_string(),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl crate::generate::Generator for ScriptedGenerator {
        fn generate_stream(
            &self,
            _req: crate::generate::GenerationRequest,
            sink: &mut dyn FnMut(&str) -> std::ops::ControlFlow<()>,
        ) -> anyhow::Result<crate::generate::Generated> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = sink(&self.reply);
            Ok(crate::generate::Generated {
                text: self.reply.clone(),
                tokens: self.reply.split_whitespace().count(),
                stop: crate::generate::StopReason::Eos,
            })
        }
        fn model_id(&self) -> &str {
            "scripted"
        }
        fn context_tokens(&self) -> usize {
            4096
        }
    }

    /// A single-chunk text index, mirroring the setup shared by several tests.
    fn index_with_one_text_chunk(dir: &tempfile::TempDir) -> (SemanticIndex, std::path::PathBuf) {
        let mut idx = SemanticIndex::create(
            dir.path(),
            "mock",
            768,
            crate::types::EmbeddingEngine::SBERT,
            Some(dir.path()),
        )
        .unwrap();

        let path = dir.path().join("doc.txt");
        std::fs::write(&path, "hello world").unwrap();

        use crate::embed::index::chunk::Chunk;
        use crate::embed::index::db::PreparedFile;
        use crate::types::{ByteRange, SourceOrigin};

        idx.write_file(PreparedFile {
            retained: Default::default(),
            full_text: String::new(),
            path: path.clone(),
            chunks: vec![(
                Chunk {
                    file_path: path.clone(),
                    text: "hello world".to_string(),
                    byte_range: ByteRange { start: 0, end: 11 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0; 768],
            )],
        })
        .unwrap();
        (idx, path)
    }

    fn text_query(root: &std::path::Path) -> SearchQuery {
        SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: root.to_path_buf(),
            max_results: 10,
            respect_gitignore: false,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Semantic,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        }
    }

    fn test_documents(query: &SearchQuery) -> Vec<SearchDocument> {
        let paths: Vec<std::path::PathBuf> = match &query.scope {
            SearchScope::File { path } => vec![path.clone()],
            SearchScope::Corpus => ignore::WalkBuilder::new(&query.root)
                .build()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_file())
                .map(|entry| entry.into_path())
                .collect(),
            SearchScope::All => Vec::new(),
        };
        paths
            .into_iter()
            .filter_map(|path| {
                crate::types::FileType::detect(&path, &query.supported_extensions).map(
                    |file_type| SearchDocument {
                        path: std::fs::canonicalize(&path).unwrap_or(path),
                        file_type,
                        title: None,
                        author: None,
                    },
                )
            })
            .collect()
    }

    async fn collect(
        handle: tokio::task::JoinHandle<SearchOutcome>,
        mut rx: tokio::sync::mpsc::Receiver<FileMatches>,
    ) -> (Vec<FileMatches>, SearchOutcome) {
        let mut results = Vec::new();
        while let Some(fm) = rx.recv().await {
            results.push(fm);
        }
        let outcome = handle.await.unwrap();
        (results, outcome)
    }

    // ── Query-vector math ─────────────────────────────────────────────────────

    #[test]
    fn normalize_produces_unit_vector() {
        let n = normalize(&[3.0, 4.0]);
        let mag = (n[0] * n[0] + n[1] * n[1]).sqrt();
        assert!((mag - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_leaves_zero_vector_untouched() {
        assert_eq!(normalize(&[0.0, 0.0]), vec![0.0, 0.0]);
    }

    #[test]
    fn mean_vector_averages_elementwise() {
        let m = mean_vector(&[vec![0.0, 2.0], vec![2.0, 4.0]]).unwrap();
        assert_eq!(m, vec![1.0, 3.0]);
    }

    #[test]
    fn mean_vector_is_none_for_empty_input() {
        assert!(mean_vector(&[]).is_none());
    }

    #[test]
    fn rocchio_weights_query_and_centroid() {
        // alpha=1, beta=0 keeps the query; beta contribution is additive.
        assert_eq!(rocchio(&[1.0, 0.0], &[0.0, 1.0], 1.0, 0.5), vec![1.0, 0.5]);
    }

    // ── HyDE wiring / degradation ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_hyde_enabled_without_generator_degrades_without_search_error() {
        let dir = tempfile::tempdir().unwrap();
        let (idx, _path) = index_with_one_text_chunk(&dir);
        let provider = SemanticSearchProvider::new(
            Arc::new(MockEmbedder),
            Arc::new(Mutex::new(Some(idx))),
            indexing_config(vec!["txt".to_string()]),
        )
        .with_retrieval(
            crate::types::RetrievalSettings {
                hyde: crate::types::HydeSettings {
                    enabled: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            None, // no generator loaded
        );

        let query = text_query(dir.path());
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let handle = tokio::task::spawn_blocking(move || {
            let documents = test_documents(&query);
            provider
                .search(&query, &ExtractorRegistry::new(), tx, &documents)
                .unwrap()
        });
        let (results, outcome) = collect(handle, rx).await;

        // Search succeeds with the raw query, and the optional enhancement's
        // degradation does not masquerade as a file/search failure.
        assert_eq!(results.len(), 1);
        assert!(outcome.errors.is_empty());
        assert!(outcome.hyde_documents.is_empty());
    }

    #[tokio::test]
    async fn test_hyde_enabled_invokes_generator() {
        let dir = tempfile::tempdir().unwrap();
        let (idx, _path) = index_with_one_text_chunk(&dir);
        let generator = Arc::new(ScriptedGenerator::new("A hypothetical answer passage."));
        let provider = SemanticSearchProvider::new(
            Arc::new(MockEmbedder),
            Arc::new(Mutex::new(Some(idx))),
            indexing_config(vec!["txt".to_string()]),
        )
        .with_retrieval(
            crate::types::RetrievalSettings {
                hyde: crate::types::HydeSettings {
                    enabled: true,
                    hypotheticals: 1,
                    include_query: true,
                },
                ..Default::default()
            },
            Some(generator.clone()),
        );

        let query = text_query(dir.path());
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let handle = tokio::task::spawn_blocking(move || {
            let documents = test_documents(&query);
            provider
                .search(&query, &ExtractorRegistry::new(), tx, &documents)
                .unwrap()
        });
        let (results, outcome) = collect(handle, rx).await;

        assert_eq!(
            generator.calls(),
            1,
            "HyDE should invoke the generator once"
        );
        assert_eq!(results.len(), 1);
        assert!(
            outcome.errors.is_empty(),
            "no degradation notes expected: {:?}",
            outcome.errors
        );
        assert_eq!(
            outcome.hyde_documents,
            vec!["A hypothetical answer passage."]
        );
    }

    #[tokio::test]
    async fn test_hyde_records_every_passage_used_for_search() {
        let dir = tempfile::tempdir().unwrap();
        let (idx, _path) = index_with_one_text_chunk(&dir);
        let generator = Arc::new(ScriptedGenerator::new("A generated search passage."));
        let provider = SemanticSearchProvider::new(
            Arc::new(MockEmbedder),
            Arc::new(Mutex::new(Some(idx))),
            indexing_config(vec!["txt".to_string()]),
        )
        .with_retrieval(
            crate::types::RetrievalSettings {
                hyde: crate::types::HydeSettings {
                    enabled: true,
                    hypotheticals: 2,
                    include_query: true,
                },
                ..Default::default()
            },
            Some(generator.clone()),
        );

        let query = text_query(dir.path());
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let handle = tokio::task::spawn_blocking(move || {
            let documents = test_documents(&query);
            provider
                .search(&query, &ExtractorRegistry::new(), tx, &documents)
                .unwrap()
        });
        let (results, outcome) = collect(handle, rx).await;

        assert_eq!(generator.calls(), 2);
        assert_eq!(results.len(), 1);
        assert!(outcome.errors.is_empty());
        assert_eq!(
            outcome.hyde_documents,
            vec!["A generated search passage.", "A generated search passage."]
        );
    }

    #[tokio::test]
    async fn test_prf_enabled_runs_and_returns_results() {
        let dir = tempfile::tempdir().unwrap();
        let (idx, _path) = index_with_one_text_chunk(&dir);
        let provider = SemanticSearchProvider::new(
            Arc::new(MockEmbedder),
            Arc::new(Mutex::new(Some(idx))),
            indexing_config(vec!["txt".to_string()]),
        )
        .with_retrieval(
            crate::types::RetrievalSettings {
                pseudo_relevance_feedback: crate::types::PrfSettings {
                    enabled: true,
                    feedback_docs: 3,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );

        let query = text_query(dir.path());
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        let handle = tokio::task::spawn_blocking(move || {
            let documents = test_documents(&query);
            provider
                .search(&query, &ExtractorRegistry::new(), tx, &documents)
                .unwrap()
        });
        let (results, outcome) = collect(handle, rx).await;

        assert_eq!(results.len(), 1);
        assert!(
            outcome.errors.is_empty(),
            "PRF should not surface errors here: {:?}",
            outcome.errors
        );
        assert!(outcome.hyde_documents.is_empty());
    }

    #[test]
    fn test_prf_with_zero_weights_keeps_original_query() {
        let provider = SemanticSearchProvider::new(
            Arc::new(MockEmbedder),
            Arc::new(Mutex::new(None)),
            indexing_config(vec!["txt".to_string()]),
        )
        .with_retrieval(
            crate::types::RetrievalSettings {
                pseudo_relevance_feedback: crate::types::PrfSettings {
                    enabled: true,
                    feedback_docs: 3,
                    alpha: 0.0,
                    beta: 0.0,
                },
                ..Default::default()
            },
            None,
        );

        let q0 = vec![0.25, 0.75];
        let query = text_query(std::path::Path::new("/unused"));
        let refined = provider.apply_prf(&q0, &query, None).unwrap();

        assert_eq!(refined, q0);
    }

    #[test]
    fn test_capabilities_without_index() {
        let embedder = Arc::new(MockEmbedder);
        let index = Arc::new(Mutex::new(None));
        let extensions = vec!["pdf".to_string(), "txt".to_string()];
        let provider = SemanticSearchProvider::new(embedder, index, indexing_config(extensions));

        let caps = provider.capabilities();
        assert!(!caps.supports_regex);
        assert!(!caps.supports_case_sensitivity);
        assert!(caps.is_indexed);
        assert!(caps.requires_index);
        assert!(!caps.semantic_index_built);
        assert!(caps.supported_file_types.contains(&"pdf".to_string()));
    }

    #[tokio::test]
    async fn test_search_unbuilt_index() {
        let embedder = Arc::new(MockEmbedder);
        let index = Arc::new(Mutex::new(None));
        let provider = SemanticSearchProvider::new(embedder, index, indexing_config(vec![]));

        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let query = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: std::path::PathBuf::from("/"),
            max_results: 10,
            respect_gitignore: false,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Semantic,
            scope: Default::default(),
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let res = provider.search(&query, &ExtractorRegistry::new(), tx, &[]);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("not built yet"));
    }

    #[tokio::test]
    async fn test_search_with_results() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let mut idx = SemanticIndex::create(
            &data_dir,
            "mock",
            768,
            crate::types::EmbeddingEngine::SBERT,
            Some(dir.path()),
        )
        .unwrap();

        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello world").unwrap();

        use crate::embed::index::chunk::Chunk;
        use crate::embed::index::db::PreparedFile;
        use crate::types::{ByteRange, SourceOrigin};

        let chunk = Chunk {
            file_path: path.clone(),
            text: "hello world".to_string(),
            byte_range: ByteRange { start: 0, end: 11 },
            origin: SourceOrigin::TextFile { line: 1, col: 1 },
        };
        let prepared = PreparedFile {
            retained: Default::default(),
            full_text: String::new(),
            path: path.clone(),
            chunks: vec![(chunk, vec![1.0; 768])],
        };
        idx.write_file(prepared).unwrap();

        let embedder = Arc::new(MockEmbedder);
        let index = Arc::new(Mutex::new(Some(idx)));
        let provider = SemanticSearchProvider::new(
            embedder,
            index.clone(),
            indexing_config(vec!["txt".to_string()]),
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let query = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: dir.path().to_path_buf(),
            max_results: 10,
            respect_gitignore: false,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Semantic,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let provider_handle = tokio::task::spawn_blocking(move || {
            let documents = test_documents(&query);
            provider
                .search(&query, &ExtractorRegistry::new(), tx, &documents)
                .unwrap();
        });

        let mut results = Vec::new();
        while let Some(fm) = rx.recv().await {
            results.push(fm);
        }
        provider_handle.await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, std::fs::canonicalize(path).unwrap());
        assert_eq!(results[0].matches.len(), 1);
        assert_eq!(results[0].matches[0].matched_text, "hello world");
    }

    #[tokio::test]
    async fn semantic_search_prioritizes_case_insensitive_filename_hits() {
        let dir = tempfile::tempdir().unwrap();
        let (idx, path) = index_with_one_text_chunk(&dir);
        let provider = SemanticSearchProvider::new(
            Arc::new(MockEmbedder),
            Arc::new(Mutex::new(Some(idx))),
            indexing_config(vec!["txt".to_string()]),
        );
        let mut query = text_query(dir.path());
        query.pattern = "DOC".into();
        query.max_results = 1;
        let canonical_path = std::fs::canonicalize(&path).unwrap();
        let documents = vec![SearchDocument {
            path: canonical_path.clone(),
            file_type: crate::types::FileType::PlainText,
            title: Some("Unrelated title".into()),
            author: None,
        }];
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);

        let handle = tokio::task::spawn_blocking(move || {
            provider
                .search(&query, &ExtractorRegistry::new(), tx, &documents)
                .unwrap()
        });
        let result = rx.recv().await.unwrap();
        assert_eq!(result.path, canonical_path);
        assert_eq!(result.field_matches.len(), 1);
        assert_eq!(
            result.field_matches[0].field,
            crate::types::SearchField::Filename
        );
        assert!(result.matches.is_empty());
        assert!(rx.recv().await.is_none());
        handle.await.unwrap();
    }

    struct TinyMockEmbedder;

    impl Embedder for TinyMockEmbedder {
        fn embedding_space_identity(&self) -> crate::embed::EmbeddingSpaceIdentity {
            crate::embed::EmbeddingSpaceIdentity::for_test(
                self.engine(),
                self.model_id(),
                self.dimension(),
            )
        }

        fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(vec![vec![1.0, 0.0]])
        }

        fn model_id(&self) -> &str {
            "tiny-mock"
        }

        fn dimension(&self) -> usize {
            2
        }

        fn engine(&self) -> crate::types::EmbeddingEngine {
            crate::types::EmbeddingEngine::Candle
        }
    }

    #[tokio::test]
    async fn test_search_groups_results_and_skips_unsupported_files() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let mut idx = SemanticIndex::create(
            &data_dir,
            "tiny-mock",
            2,
            crate::types::EmbeddingEngine::Candle,
            Some(dir.path()),
        )
        .unwrap();

        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.bin");
        let file_c = dir.path().join("c.txt");
        std::fs::write(&file_a, "alpha").unwrap();
        std::fs::write(&file_b, "beta").unwrap();
        std::fs::write(&file_c, "gamma").unwrap();

        use crate::embed::index::chunk::Chunk;
        use crate::embed::index::db::PreparedFile;
        use crate::types::{ByteRange, SourceOrigin};

        idx.write_file(PreparedFile {
            retained: Default::default(),
            full_text: String::new(),
            path: file_a.clone(),
            chunks: vec![(
                Chunk {
                    file_path: file_a.clone(),
                    text: "alpha".to_string(),
                    byte_range: ByteRange { start: 0, end: 5 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0, 0.0],
            )],
        })
        .unwrap();
        idx.write_file(PreparedFile {
            retained: Default::default(),
            full_text: String::new(),
            path: file_b.clone(),
            chunks: vec![(
                Chunk {
                    file_path: file_b.clone(),
                    text: "beta".to_string(),
                    byte_range: ByteRange { start: 0, end: 4 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![0.8, 0.2],
            )],
        })
        .unwrap();
        idx.write_file(PreparedFile {
            retained: Default::default(),
            full_text: String::new(),
            path: file_c.clone(),
            chunks: vec![(
                Chunk {
                    file_path: file_c.clone(),
                    text: "gamma".to_string(),
                    byte_range: ByteRange { start: 0, end: 5 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![0.0, 1.0],
            )],
        })
        .unwrap();

        let embedder = Arc::new(TinyMockEmbedder);
        let index = Arc::new(Mutex::new(Some(idx)));
        let provider =
            SemanticSearchProvider::new(embedder, index, indexing_config(vec!["txt".to_string()]));

        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let query = SearchQuery {
            pattern: "alpha".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: dir.path().to_path_buf(),
            max_results: 10,
            respect_gitignore: false,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Semantic,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let provider_handle = tokio::task::spawn_blocking(move || {
            let documents = test_documents(&query);
            provider
                .search(&query, &ExtractorRegistry::new(), tx, &documents)
                .unwrap();
        });

        let mut results = Vec::new();
        while let Some(fm) = rx.recv().await {
            results.push(fm);
        }
        provider_handle.await.unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].path, std::fs::canonicalize(file_a).unwrap());
        assert_eq!(results[1].path, std::fs::canonicalize(file_c).unwrap());
        assert_eq!(results[0].matches[0].matched_text, "alpha");
        assert_eq!(results[1].matches[0].matched_text, "gamma");
    }
}
