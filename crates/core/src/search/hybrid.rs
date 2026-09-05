//! Combined retrieval: wording and meaning in one search.
//!
//! A query like "instrumental variables weak identification" names a precise
//! piece of terminology *and* a problem that a great many papers discuss
//! without ever writing that phrase. Asking the user to pick which of the two
//! they meant, before they have seen either, is asking them to guess. This
//! provider does not ask: it runs the exact lane and the semantic lane over the
//! same catalog and fuses their rankings, and every admitted document says
//! which lane(s) admitted it.
//!
//! **The lanes are the existing providers, unmodified.** Nothing here
//! re-implements matching or nearest-neighbour retrieval; this is composition
//! and ranking only. Each lane is handed the query with its own mode set, so
//! the exact lane matches exactly (honouring `is_regex` and `case_sensitive`)
//! and the semantic lane behaves as it does under [`SearchMode::Semantic`] —
//! there is no third set of matching rules to keep in step with the other two.
//!
//! **Rank fusion, not score fusion.** A grep hit has no score and a cosine
//! similarity is not a probability, so there is no arithmetic that could
//! combine them without inventing a scale. What the two lanes agree on is an
//! *ordering*, so the fusion is over ranks: reciprocal rank fusion, which needs
//! no per-lane weight and rewards a document both lanes ranked well without
//! letting either lane's absolute numbers matter.

use std::collections::HashMap;
use std::path::PathBuf;

use tokio::sync::mpsc;
use tracing::warn;

use crate::extract::ExtractorRegistry;
use crate::types::{
    FileMatches, MatchEvidence, SearchCapabilities, SearchDocument, SearchMode, SearchQuery,
};

use super::grep::GrepSearchProvider;
use super::semantic::SemanticSearchProvider;
use super::{prioritize_and_limit_results, SearchOutcome, SearchProvider, SearchResultTx};

/// The `k` of reciprocal rank fusion. The constant's job is to stop rank 0 from
/// dwarfing every other rank, so that a document both lanes ranked reasonably
/// can outrank one lane's single best hit. 60 is the value the technique was
/// published with and there is nothing corpus-specific to tune it against here.
const RRF_K: f32 = 60.0;

/// Buffer for a lane's results on their way to the collector. The lane and the
/// collector run concurrently, so this bounds memory rather than throughput.
const LANE_CHANNEL_CAPACITY: usize = 64;

/// How often an idle collector re-checks whether the caller has gone. A lane
/// that is mid-scan sends nothing for long stretches, so noticing cancellation
/// cannot wait on the next result to arrive.
const CANCELLATION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

pub struct HybridSearchProvider {
    exact: GrepSearchProvider,
    /// `None` when the host could not supply an embedder and a built index. The
    /// combined mode then reports its own reduction (see [`Self::search`])
    /// instead of pretending a lane ran.
    semantic: Option<SemanticSearchProvider>,
}

impl HybridSearchProvider {
    pub fn new(exact: GrepSearchProvider, semantic: Option<SemanticSearchProvider>) -> Self {
        Self { exact, semantic }
    }
}

impl SearchProvider for HybridSearchProvider {
    fn search(
        &self,
        query: &SearchQuery,
        extractors: &ExtractorRegistry,
        tx: SearchResultTx,
        documents: &[SearchDocument],
    ) -> anyhow::Result<SearchOutcome> {
        // Each lane searches under its own mode. Anything that reads
        // `query.mode` — the identity-field matcher most of all — must see the
        // mode whose rules it is about to apply, not the composition's.
        let exact_query = SearchQuery {
            mode: SearchMode::Grep,
            ..query.clone()
        };
        let (exact_results, exact_outcome) =
            run_lane(&self.exact, &tx, &exact_query, extractors, documents)?;
        if tx.is_closed() {
            return Ok(exact_outcome);
        }

        let mut outcome = exact_outcome;
        let semantic_results = match &self.semantic {
            Some(semantic) => {
                let semantic_query = SearchQuery {
                    mode: SearchMode::Semantic,
                    // Nearest-neighbour retrieval has no notion of either, and
                    // leaving them set would only misdescribe the lane.
                    is_regex: false,
                    case_sensitive: false,
                    ..query.clone()
                };
                let (results, semantic_outcome) =
                    run_lane(semantic, &tx, &semantic_query, extractors, documents)?;
                outcome.errors.extend(semantic_outcome.errors);
                outcome.hyde_documents = semantic_outcome.hyde_documents;
                results
            }
            None => {
                // Not a failure to recover from here: the host decides which
                // lanes exist and has already recorded why this one does not.
                // Saying it again in the log keeps a reduced combined search
                // from looking like a complete one.
                warn!(
                    "[hybrid] no semantic lane for this search; combined results carry exact matches only"
                );
                Vec::new()
            }
        };

        let fused = fuse(exact_results, semantic_results);
        for result in prioritize_and_limit_results(fused, query.max_results) {
            if tx.is_closed() || tx.blocking_send(result).is_err() {
                break;
            }
        }

        Ok(outcome)
    }

    fn capabilities(&self) -> SearchCapabilities {
        // The exact lane is always present and owns the literal-matching
        // capabilities; the semantic lane owns whatever the index reports.
        let mut capabilities = self.exact.capabilities();
        if let Some(semantic) = &self.semantic {
            let semantic_capabilities = semantic.capabilities();
            capabilities.is_indexed = true;
            capabilities.semantic_index_built = semantic_capabilities.semantic_index_built;
            capabilities.supported_file_types = semantic_capabilities.supported_file_types;
        }
        capabilities
    }
}

/// Run one lane to completion and collect what it streamed.
///
/// The lane's channel is bounded, so something must drain it while the lane
/// fills it, and the collector thread is that drain.
///
/// The collector is also how a cancelled search reaches the lane. A provider
/// learns it has been cancelled from the sender it was handed, and here that
/// sender is this function's, not the caller's — so a lane given only a private
/// channel would keep scanning a corpus nobody is waiting for. The collector
/// therefore watches `caller` and, when it goes, drops the receiver: the lane's
/// next `is_closed` or send fails, which is exactly the signal it already
/// stops on.
fn run_lane(
    provider: &dyn SearchProvider,
    caller: &SearchResultTx,
    query: &SearchQuery,
    extractors: &ExtractorRegistry,
    documents: &[SearchDocument],
) -> anyhow::Result<(Vec<FileMatches>, SearchOutcome)> {
    let (tx, mut rx) = mpsc::channel::<FileMatches>(LANE_CHANNEL_CAPACITY);
    std::thread::scope(|scope| {
        let collector = scope.spawn(move || {
            let mut collected = Vec::new();
            loop {
                match rx.try_recv() {
                    Ok(result) => collected.push(result),
                    Err(mpsc::error::TryRecvError::Disconnected) => break,
                    Err(mpsc::error::TryRecvError::Empty) => {
                        if caller.is_closed() {
                            break;
                        }
                        std::thread::sleep(CANCELLATION_POLL_INTERVAL);
                    }
                }
            }
            collected
        });
        let outcome = provider.search(query, extractors, tx, documents);
        let collected = collector
            .join()
            .map_err(|_| anyhow::anyhow!("hybrid lane collector thread panicked"))?;
        outcome.map(|outcome| (collected, outcome))
    })
}

/// Fuse two ranked lists into one, by reciprocal rank fusion over documents.
///
/// A document's fused score is the sum of `1 / (k + rank)` over the lanes that
/// returned it, so appearing in both lanes beats appearing high in one. Within
/// a document, the exact lane's matches come first: they carry a position the
/// reader can be taken to, and a chunk is context around a subject rather than
/// a place in the text.
fn fuse(exact: Vec<FileMatches>, semantic: Vec<FileMatches>) -> Vec<FileMatches> {
    let mut order: Vec<PathBuf> = Vec::with_capacity(exact.len() + semantic.len());
    let mut fused: HashMap<PathBuf, FileMatches> = HashMap::new();
    let mut scores: HashMap<PathBuf, f32> = HashMap::new();

    for (rank, mut result) in exact.into_iter().enumerate() {
        *scores.entry(result.path.clone()).or_insert(0.0) += reciprocal_rank(rank);
        result.evidence = vec![MatchEvidence::ExactPhrase];
        order.push(result.path.clone());
        fused.insert(result.path.clone(), result);
    }

    for (rank, result) in semantic.into_iter().enumerate() {
        *scores.entry(result.path.clone()).or_insert(0.0) += reciprocal_rank(rank);
        // The semantic lane also admits documents on an identity match, and
        // such a document has no passage of its own. What it has is the query
        // text in its filename, title or author — which is the other
        // explanation, not this one. So the label follows what the document
        // carries rather than which lane handed it over.
        let carries_passage = !result.matches.is_empty();
        match fused.get_mut(&result.path) {
            Some(existing) => {
                merge_into(existing, result, carries_passage);
            }
            None => {
                let mut result = result;
                result.evidence = if carries_passage {
                    vec![MatchEvidence::RelatedPassage]
                } else {
                    vec![MatchEvidence::ExactPhrase]
                };
                order.push(result.path.clone());
                fused.insert(result.path.clone(), result);
            }
        }
    }

    // Sort by fused score, keeping first-seen order as the tie-break so equal
    // scores stay in the exact lane's catalog order rather than a hash order.
    let mut ranked: Vec<(usize, PathBuf)> = order.into_iter().enumerate().collect();
    ranked.sort_by(|(left_seen, left_path), (right_seen, right_path)| {
        let left = scores.get(left_path).copied().unwrap_or(0.0);
        let right = scores.get(right_path).copied().unwrap_or(0.0);
        right
            .partial_cmp(&left)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left_seen.cmp(right_seen))
    });

    ranked
        .into_iter()
        .filter_map(|(_, path)| fused.remove(&path))
        .collect()
}

fn reciprocal_rank(rank: usize) -> f32 {
    1.0 / (RRF_K + rank as f32)
}

/// Fold a semantic-lane result into the exact-lane result for the same
/// document. Both lanes run the same identity-field matcher over the same
/// catalog entry, so field matches are already duplicates and are not merged;
/// the content chunks are what the semantic lane adds.
fn merge_into(existing: &mut FileMatches, semantic: FileMatches, carries_passage: bool) {
    existing.matches.extend(semantic.matches);
    if carries_passage && !existing.evidence.contains(&MatchEvidence::RelatedPassage) {
        existing.evidence.push(MatchEvidence::RelatedPassage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ByteRange, FileType, Match, SearchField, SearchFieldMatch, SourceOrigin};

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("/library/{name}"))
    }

    fn content_match(text: &str, score: Option<f32>) -> Match {
        Match {
            text_range: Some(ByteRange { start: 0, end: 1 }),
            matched_text: text.into(),
            context_before: String::new(),
            context_after: String::new(),
            origin: SourceOrigin::TextFile { line: 1, col: 0 },
            score,
        }
    }

    fn file(name: &str, matches: Vec<Match>) -> FileMatches {
        FileMatches {
            path: path(name),
            file_type: FileType::PlainText,
            title: None,
            field_matches: Vec::new(),
            matches,
            evidence: Vec::new(),
        }
    }

    fn hybrid_query(root: PathBuf) -> SearchQuery {
        SearchQuery {
            pattern: "weak identification".into(),
            is_regex: false,
            case_sensitive: false,
            root,
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: SearchMode::Hybrid,
            scope: Default::default(),
            supported_extensions: vec!["txt".into()],
            collection_id: None,
            tag_ids: Vec::new(),
        }
    }

    /// A lane that behaves like the real ones: it works until the sender it was
    /// handed says nobody is listening. Reports whether it was ever released.
    struct LaneAwaitingCancellation {
        released: std::sync::atomic::AtomicBool,
    }

    impl SearchProvider for LaneAwaitingCancellation {
        fn search(
            &self,
            _query: &SearchQuery,
            _extractors: &ExtractorRegistry,
            tx: SearchResultTx,
            _documents: &[SearchDocument],
        ) -> anyhow::Result<SearchOutcome> {
            // Bounded so a regression fails the test instead of hanging it.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !tx.is_closed() {
                if std::time::Instant::now() > deadline {
                    return Ok(SearchOutcome::default());
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            self.released
                .store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(SearchOutcome::default())
        }

        fn capabilities(&self) -> SearchCapabilities {
            GrepSearchProvider::new().capabilities()
        }
    }

    #[test]
    fn a_cancelled_search_releases_the_lane_it_is_running() {
        // Each lane is handed this function's sender rather than the caller's,
        // so without the collector watching the caller a cancelled combined
        // search would keep scanning a corpus nobody is waiting for.
        let (caller_tx, caller_rx) = mpsc::channel::<FileMatches>(1);
        drop(caller_rx);

        let lane = LaneAwaitingCancellation {
            released: std::sync::atomic::AtomicBool::new(false),
        };
        let dir = tempfile::tempdir().unwrap();
        let (results, _) = run_lane(
            &lane,
            &caller_tx,
            &hybrid_query(dir.path().to_path_buf()),
            &crate::extract::exact_search_registry(),
            &[],
        )
        .unwrap();

        assert!(
            lane.released.load(std::sync::atomic::Ordering::Relaxed),
            "the lane was never told the caller had gone"
        );
        assert!(results.is_empty());
    }

    #[test]
    fn a_document_both_lanes_returned_carries_both_explanations_and_all_matches() {
        let exact = vec![file("a.txt", vec![content_match("weak identification", None)])];
        let semantic = vec![file(
            "a.txt",
            vec![content_match("the instruments are only weakly correlated", Some(0.8))],
        )];

        let fused = fuse(exact, semantic);

        assert_eq!(fused.len(), 1);
        assert_eq!(
            fused[0].evidence,
            vec![MatchEvidence::ExactPhrase, MatchEvidence::RelatedPassage]
        );
        assert_eq!(fused[0].matches.len(), 2);
        // The exact lane's positioned match stays first.
        assert_eq!(fused[0].matches[0].matched_text, "weak identification");
        assert!(fused[0].matches[0].score.is_none());
        assert_eq!(fused[0].matches[1].score, Some(0.8));
    }

    #[test]
    fn single_lane_documents_carry_only_that_lane_explanation() {
        let exact = vec![file("exact.txt", vec![content_match("phrase", None)])];
        let semantic = vec![file("related.txt", vec![content_match("passage", Some(0.5))])];

        let fused = fuse(exact, semantic);

        let by_path = |name: &str| {
            fused
                .iter()
                .find(|result| result.path == path(name))
                .expect("document present")
        };
        assert_eq!(by_path("exact.txt").evidence, vec![MatchEvidence::ExactPhrase]);
        assert_eq!(
            by_path("related.txt").evidence,
            vec![MatchEvidence::RelatedPassage]
        );
    }

    #[test]
    fn a_semantic_lane_identity_hit_is_an_exact_occurrence_not_a_passage() {
        // The semantic lane runs the identity-field matcher too, so it admits
        // documents that have no passage of their own. Calling that a related
        // passage would name something the result does not contain.
        let mut title_only = file("iv-handbook.pdf", Vec::new());
        title_only.field_matches = vec![SearchFieldMatch {
            field: SearchField::Title,
            matched_text: "Instrumental Variables".into(),
            context_before: String::new(),
            context_after: String::new(),
        }];

        let fused = fuse(Vec::new(), vec![title_only]);

        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].evidence, vec![MatchEvidence::ExactPhrase]);
    }

    #[test]
    fn agreement_between_lanes_outranks_a_single_lane_top_hit() {
        // "both" is second in each lane; "exact_only" is first in one of them.
        let exact = vec![
            file("exact_only.txt", vec![content_match("phrase", None)]),
            file("both.txt", vec![content_match("phrase", None)]),
        ];
        let semantic = vec![
            file("semantic_only.txt", vec![content_match("passage", Some(0.9))]),
            file("both.txt", vec![content_match("passage", Some(0.7))]),
        ];

        let fused = fuse(exact, semantic);

        assert_eq!(fused[0].path, path("both.txt"));
        assert_eq!(fused.len(), 3);
    }

    #[test]
    fn equal_scores_keep_the_exact_lane_catalog_order() {
        let exact = vec![
            file("first.txt", vec![content_match("phrase", None)]),
            file("second.txt", vec![content_match("phrase", None)]),
            file("third.txt", vec![content_match("phrase", None)]),
        ];

        let fused = fuse(exact, Vec::new());

        let order: Vec<_> = fused.iter().map(|result| result.path.clone()).collect();
        assert_eq!(
            order,
            vec![path("first.txt"), path("second.txt"), path("third.txt")]
        );
    }

    #[test]
    fn identity_hits_are_not_duplicated_when_both_lanes_report_them() {
        // Both lanes run the same field matcher over the same catalog entry.
        let field_match = SearchFieldMatch {
            field: SearchField::Title,
            matched_text: "Instrumental Variables".into(),
            context_before: String::new(),
            context_after: String::new(),
        };
        let mut exact = file("paper.pdf", Vec::new());
        exact.field_matches = vec![field_match.clone()];
        let mut semantic = file("paper.pdf", vec![content_match("passage", Some(0.6))]);
        semantic.field_matches = vec![field_match];

        let fused = fuse(vec![exact], vec![semantic]);

        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].field_matches.len(), 1);
        assert_eq!(fused[0].matches.len(), 1);
    }

    #[test]
    fn a_search_with_no_semantic_lane_still_returns_the_exact_lane() {
        use crate::extract::exact_search_registry;
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("note.txt");
        fs::write(&file_path, "weak identification is the problem here").unwrap();

        let provider = HybridSearchProvider::new(GrepSearchProvider::new(), None);
        let query = hybrid_query(dir.path().to_path_buf());
        let documents = vec![SearchDocument {
            path: file_path.clone(),
            file_type: FileType::PlainText,
            title: None,
            author: None,
        }];

        let (tx, mut rx) = mpsc::channel::<FileMatches>(8);
        let outcome = std::thread::scope(|scope| {
            let collector = scope.spawn(move || {
                let mut collected = Vec::new();
                while let Some(result) = rx.blocking_recv() {
                    collected.push(result);
                }
                collected
            });
            let outcome = provider
                .search(&query, &exact_search_registry(), tx, &documents)
                .unwrap();
            (collector.join().unwrap(), outcome)
        });

        let (results, outcome) = outcome;
        assert!(outcome.errors.is_empty());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, file_path);
        assert_eq!(results[0].evidence, vec![MatchEvidence::ExactPhrase]);
    }
}
