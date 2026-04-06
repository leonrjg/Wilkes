use std::sync::{Arc, Mutex};

use tracing::{error, info};
use crate::extract::ExtractorRegistry;
use crate::types::{FileMatches, FileType, Match, SearchCapabilities, SearchQuery, SourceOrigin};

use super::{SearchProvider, SearchResultTx};
use crate::embed::Embedder;
use crate::embed::index::SemanticIndex;

pub struct SemanticSearchProvider {
    embedder: Arc<dyn Embedder>,
    index: Arc<Mutex<Option<SemanticIndex>>>,
    supported_extensions: Vec<String>,
}

impl SemanticSearchProvider {
    pub fn new(
        embedder: Arc<dyn Embedder>,
        index: Arc<Mutex<Option<SemanticIndex>>>,
        supported_extensions: Vec<String>,
    ) -> Self {
        Self { embedder, index, supported_extensions }
    }
}

impl SearchProvider for SemanticSearchProvider {
    fn search(
        &self,
        query: &SearchQuery,
        _extractors: &ExtractorRegistry,
        tx: SearchResultTx,
    ) -> anyhow::Result<Vec<String>> {
        // 1. Embed the query string.
        info!("[semantic] embedding query...");
        let query_vecs = self.embedder.embed_query(&[query.pattern.as_str()])
            .map_err(|e| { error!("[semantic] embed error: {e:#}"); e })?;
        info!("[semantic] query embedded, running index query");
        let query_vec = query_vecs
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("Embedder returned no vector for the query"))?;

        // 2. Lock the index and run the nearest-neighbour query.
        let guard = self.index.lock().unwrap();
        let idx = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Semantic index is not built yet"))?;

        let top_k = if query.max_results == 0 { 20 } else { query.max_results };
        let results = idx.query(&query_vec, top_k)?;
        drop(guard);

        // 3. Convert IndexedChunk results into FileMatches / Match.
        //    Group by file path, preserving score-ranked order across files.
        use std::collections::HashMap;
        let mut by_file: HashMap<std::path::PathBuf, (FileType, Vec<Match>)> = HashMap::new();
        let mut file_order: Vec<std::path::PathBuf> = Vec::new();

        for chunk in results {
            let Some(file_type) = FileType::detect(&chunk.file_path, &query.supported_extensions) else {
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
            }
            let entry = by_file.entry(chunk.file_path).or_insert_with(|| (file_type, Vec::new()));
            entry.1.push(m);
        }

        for path in file_order {
            if tx.is_closed() {
                break;
            }
            let (file_type, matches) = by_file.remove(&path).unwrap();
            let fm = FileMatches { path, file_type, matches };
            if tx.blocking_send(fm).is_err() {
                break;
            }
        }

        Ok(Vec::new())
    }

    fn capabilities(&self) -> SearchCapabilities {
        let index_built = self
            .index
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(false);

        SearchCapabilities {
            supports_regex: false,
            supports_case_sensitivity: false,
            is_indexed: true,
            supported_file_types: self.supported_extensions.clone(),
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
        fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(vec![vec![0.0; 768]])
        }
        fn model_id(&self) -> &str { "mock" }
        fn dimension(&self) -> usize { 768 }
    }

    #[test]
    fn test_capabilities_without_index() {
        let embedder = Arc::new(MockEmbedder);
        let index = Arc::new(Mutex::new(None));
        let extensions = vec!["pdf".to_string(), "txt".to_string()];
        let provider = SemanticSearchProvider::new(embedder, index, extensions);

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
        let provider = SemanticSearchProvider::new(embedder, index, vec![]);
        
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let query = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: std::path::PathBuf::from("/"),
            file_type_filters: vec![],
            max_results: 10,
            respect_gitignore: false,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Semantic,
            supported_extensions: vec![],
        };
        
        let res = provider.search(&query, &ExtractorRegistry::new(), tx);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("not built yet"));
    }

    #[tokio::test]
    async fn test_search_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let idx = SemanticIndex::create(
            dir.path(), "mock", 768,
            crate::types::EmbeddingEngine::SBERT,
            None,
        ).unwrap();
        let embedder = Arc::new(MockEmbedder);
        let index = Arc::new(Mutex::new(Some(idx)));
        let provider = SemanticSearchProvider::new(embedder, index, vec![]);
        
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let query = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: std::path::PathBuf::from("/"),
            file_type_filters: vec![],
            max_results: 10,
            respect_gitignore: false,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Semantic,
            supported_extensions: vec![],
        };
        
        let res = provider.search(&query, &ExtractorRegistry::new(), tx);
        assert!(res.is_ok());
    }
}
