use std::sync::{Arc, Mutex};

use crate::extract::ExtractorRegistry;
use crate::types::{
    FileMatches, FileType, IndexingConfig, Match, SearchCapabilities, SearchQuery, SearchScope,
    SourceOrigin,
};
use tracing::{error, info};

use super::{SearchProvider, SearchResultTx};
use crate::embed::index::{SemanticIndex, SemanticQueryScope};
use crate::embed::Embedder;

pub struct SemanticSearchProvider {
    embedder: Arc<dyn Embedder>,
    index: Arc<Mutex<Option<SemanticIndex>>>,
    indexing: IndexingConfig,
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
        }
    }
}

impl SearchProvider for SemanticSearchProvider {
    fn search(
        &self,
        query: &SearchQuery,
        _extractors: &ExtractorRegistry,
        tx: SearchResultTx,
        eligible_paths: Option<&std::collections::HashSet<std::path::PathBuf>>,
    ) -> anyhow::Result<Vec<String>> {
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

        // 2. Embed the query string.
        info!("[semantic] embedding query...");
        let query_vecs = self
            .embedder
            .embed_query(&[query.pattern.as_str()])
            .map_err(|e| {
                error!("[semantic] embed error: {e:#}");
                e
            })?;
        info!("[semantic] query embedded, running index query");
        let query_vec = query_vecs
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("Embedder returned no vector for the query"))?;

        // 3. Lock the index and run the nearest-neighbour query.
        let guard = self.index.lock().unwrap();
        let idx = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Semantic index is not built yet"))?;

        let top_k = query.max_results;
        let scope = match &query.scope {
            SearchScope::Corpus => SemanticQueryScope::Root(&query.root),
            SearchScope::All => SemanticQueryScope::Corpus,
            SearchScope::File { path } => SemanticQueryScope::File(path),
        };
        let results = idx.query_scoped_filtered(&query_vec, top_k, scope, eligible_paths)?;
        drop(guard);

        // 4. Convert IndexedChunk results into FileMatches / Match.
        //    Group by file path, preserving score-ranked order across files.
        use std::collections::HashMap;
        let mut by_file: HashMap<std::path::PathBuf, (FileType, Vec<Match>)> = HashMap::new();
        let mut file_order: Vec<std::path::PathBuf> = Vec::new();

        for chunk in results {
            let Some(file_type) = FileType::detect(&chunk.file_path, &query.supported_extensions)
            else {
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
            let entry = by_file
                .entry(chunk.file_path)
                .or_insert_with(|| (file_type, Vec::new()));
            entry.1.push(m);
        }

        for path in file_order {
            if tx.is_closed() {
                break;
            }
            let (file_type, matches) = by_file.remove(&path).unwrap();
            let fm = FileMatches {
                path,
                file_type,
                matches,
            };
            if tx.blocking_send(fm).is_err() {
                break;
            }
        }

        Ok(reconcile_errors)
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
        fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(vec![vec![1.0; 768]])
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

        let res = provider.search(&query, &ExtractorRegistry::new(), tx, None);
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
            provider
                .search(&query, &ExtractorRegistry::new(), tx, None)
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

    struct TinyMockEmbedder;

    impl Embedder for TinyMockEmbedder {
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
            provider
                .search(&query, &ExtractorRegistry::new(), tx, None)
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
