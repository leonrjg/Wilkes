use crate::embed::index::SemanticIndex;
use crate::extract::ExtractorRegistry;
use crate::types::{
    ByteRange, FileMatches, FileType, Match, SearchCapabilities, SearchDocument, SearchQuery,
    SourceMap, SourceOrigin,
};
use grep_matcher::Matcher;
use grep_regex::RegexMatcher;
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkMatch};
use rayon::prelude::*;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{
    document_field_matches, exact_matcher, pdf_projection, prioritize_and_limit_results,
    SearchOutcome, SearchProvider, SearchResultTx,
};

/// Shared handle to the live semantic index, as held by the API layer.
type IndexHandle = Arc<Mutex<Option<SemanticIndex>>>;

#[derive(Default)]
struct GrepDiagnostics {
    indexed_pdf_reads: AtomicUsize,
    live_pdf_fallbacks: AtomicUsize,
    index_unavailable_fallbacks: AtomicUsize,
}

pub struct GrepSearchProvider {
    /// When set, PDF matches are found against text the index already holds
    /// instead of re-extracting the file. Files the index does not hold (or that
    /// changed on disk) fall back to live extraction. `None` disables this and
    /// makes every PDF extract live, i.e. the setting is off.
    index: Option<IndexHandle>,
}

impl GrepSearchProvider {
    pub fn new() -> Self {
        Self { index: None }
    }

    /// Enable index-backed exact search: PDF matches are read from the semantic
    /// index when it holds the file's current text, otherwise extracted live.
    pub fn with_index(mut self, index: Option<IndexHandle>) -> Self {
        self.index = index;
        self
    }

    fn build_matcher(query: &SearchQuery) -> anyhow::Result<RegexMatcher> {
        exact_matcher(query)
    }
}

impl Default for GrepSearchProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for GrepSearchProvider {
    fn search(
        &self,
        query: &SearchQuery,
        extractors: &ExtractorRegistry,
        tx: SearchResultTx,
        documents: &[SearchDocument],
    ) -> anyhow::Result<SearchOutcome> {
        let matcher = Self::build_matcher(query)?;
        let has_pdf = documents
            .iter()
            .any(|document| document.file_type == FileType::Pdf);
        let pdf_literal_matcher = (!query.is_regex && has_pdf)
            .then(|| pdf_projection::literal_matcher(&query.pattern, query.case_sensitive))
            .transpose()?;
        let errors = Mutex::new(Vec::new());
        let files_scanned = AtomicUsize::new(0);
        let diagnostics = GrepDiagnostics::default();
        let index = self.index.as_ref();
        let field_matches_by_document = documents
            .iter()
            .map(|document| document_field_matches(&matcher, document))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let field_match_count = field_matches_by_document
            .iter()
            .map(Vec::len)
            .sum::<usize>();
        let content_budget = if query.max_results == 0 {
            usize::MAX
        } else {
            query.max_results.saturating_sub(field_match_count)
        };
        let remaining_content_budget = AtomicUsize::new(content_budget);
        let mut results: Vec<(usize, FileMatches)> = documents
            .par_iter()
            .enumerate()
            .filter_map(|(catalog_index, document)| {
                if tx.is_closed() {
                    return None;
                }
                let field_matches = field_matches_by_document[catalog_index].clone();
                if remaining_content_budget.load(Ordering::Relaxed) == 0 {
                    return (!field_matches.is_empty()).then(|| {
                        (
                            catalog_index,
                            FileMatches {
                                path: document.path.clone(),
                                file_type: document.file_type.clone(),
                                title: document.title.clone(),
                                field_matches,
                                matches: Vec::new(),
                                evidence: Vec::new(),
                            },
                        )
                    });
                }
                files_scanned.fetch_add(1, Ordering::Relaxed);
                let mut matches = match search_document_content(
                    document,
                    query,
                    extractors,
                    &matcher,
                    pdf_literal_matcher.as_ref(),
                    index,
                    &diagnostics,
                ) {
                    Ok(matches) => matches,
                    Err(err) => {
                        errors
                            .lock()
                            .unwrap()
                            .push(format!("{}: {err:#}", document.path.display()));
                        Vec::new()
                    }
                };
                let available_matches = matches.len();
                matches.truncate(claim_match_budget(
                    &remaining_content_budget,
                    available_matches,
                ));
                if field_matches.is_empty() && matches.is_empty() {
                    return None;
                }
                Some((
                    catalog_index,
                    FileMatches {
                        path: document.path.clone(),
                        file_type: document.file_type.clone(),
                        title: document.title.clone(),
                        field_matches,
                        matches,
                        evidence: Vec::new(),
                    },
                ))
            })
            .collect();
        results.sort_by_key(|(catalog_index, _)| *catalog_index);
        let results = prioritize_and_limit_results(
            results.into_iter().map(|(_, result)| result).collect(),
            query.max_results,
        );
        for result in results {
            if tx.is_closed() || tx.blocking_send(result).is_err() {
                break;
            }
        }

        Ok(SearchOutcome {
            errors: errors.into_inner().unwrap(),
            hyde_documents: Vec::new(),
            files_scanned: Some(files_scanned.load(Ordering::Relaxed)),
            indexed_pdf_reads: diagnostics.indexed_pdf_reads.load(Ordering::Relaxed),
            live_pdf_fallbacks: diagnostics.live_pdf_fallbacks.load(Ordering::Relaxed),
            index_unavailable_fallbacks: diagnostics
                .index_unavailable_fallbacks
                .load(Ordering::Relaxed),
        })
    }

    fn capabilities(&self) -> SearchCapabilities {
        SearchCapabilities {
            supports_regex: true,
            supports_case_sensitivity: true,
            is_indexed: false,
            supported_file_types: vec![
                "txt".into(),
                "md".into(),
                "rs".into(),
                "py".into(),
                "js".into(),
                "ts".into(),
                "json".into(),
                "toml".into(),
                "yaml".into(),
            ],
            requires_index: false,
            semantic_index_built: false,
            supported_engines: crate::types::EmbeddingEngine::supported_engines(),
        }
    }
}

fn claim_match_budget(remaining: &AtomicUsize, requested: usize) -> usize {
    if remaining.load(Ordering::Relaxed) == usize::MAX {
        return requested;
    }
    loop {
        let available = remaining.load(Ordering::Relaxed);
        let claimed = available.min(requested);
        if remaining
            .compare_exchange_weak(
                available,
                available - claimed,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            return claimed;
        }
    }
}

fn search_document_content(
    document: &SearchDocument,
    query: &SearchQuery,
    extractors: &ExtractorRegistry,
    matcher: &RegexMatcher,
    pdf_literal_matcher: Option<&RegexMatcher>,
    index: Option<&IndexHandle>,
    diagnostics: &GrepDiagnostics,
) -> anyhow::Result<Vec<Match>> {
    let path = document.path.as_path();
    match &document.file_type {
        // Plain text is memory-mapped and already fast; it also carries exact
        // line/column origins that the chunk-granular index cannot reproduce, so
        // it never uses the index regardless of the setting.
        FileType::PlainText => search_text_file(path, matcher, query.context_lines as u64),
        FileType::Pdf => {
            // Prefer text the index already holds. A genuine index fault is
            // logged and demoted to live extraction rather than failing the
            // file; "not indexed / stale / pre-v4" simply returns None.
            let from_index = match index {
                Some(handle) => indexed_pdf_matches(handle, path, matcher, pdf_literal_matcher)
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            "index-backed grep failed for {}, extracting live: {e:#}",
                            path.display()
                        );
                        IndexedPdfSearch::DocumentUnavailable
                    }),
                None => IndexedPdfSearch::Disabled,
            };
            match from_index {
                IndexedPdfSearch::Served(matches) => {
                    diagnostics
                        .indexed_pdf_reads
                        .fetch_add(1, Ordering::Relaxed);
                    Ok(matches)
                }
                IndexedPdfSearch::IndexUnavailable => {
                    diagnostics
                        .live_pdf_fallbacks
                        .fetch_add(1, Ordering::Relaxed);
                    diagnostics
                        .index_unavailable_fallbacks
                        .fetch_add(1, Ordering::Relaxed);
                    live_pdf_matches(path, extractors, matcher, pdf_literal_matcher)
                }
                IndexedPdfSearch::Disabled | IndexedPdfSearch::DocumentUnavailable => {
                    diagnostics
                        .live_pdf_fallbacks
                        .fetch_add(1, Ordering::Relaxed);
                    live_pdf_matches(path, extractors, matcher, pdf_literal_matcher)
                }
            }
        }
    }
}

// ── Text file search ──────────────────────────────────────────────────────────

type SinkError = Box<dyn std::error::Error>;

struct CollectSink<'m> {
    matcher: &'m RegexMatcher,
    matches: Vec<Match>,
}

impl<'m> Sink for CollectSink<'m> {
    type Error = SinkError;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        let line = mat.bytes();
        let line_num = mat.line_number().unwrap_or(0) as u32;
        let base_offset = mat.absolute_byte_offset() as usize;

        // Collect all matches within this line without holding self borrow.
        let mut line_matches: Vec<Match> = Vec::new();

        self.matcher
            .find_iter(line, |m| {
                let start = m.start();
                let end = m.end();
                let matched_text = String::from_utf8_lossy(&line[start..end]).into_owned();
                let context_before = String::from_utf8_lossy(&line[..start]).into_owned();
                let context_after = String::from_utf8_lossy(&line[end..])
                    .trim_end_matches(['\n', '\r'])
                    .to_owned();

                line_matches.push(Match {
                    text_range: Some(ByteRange {
                        start: base_offset + start,
                        end: base_offset + end,
                    }),
                    matched_text,
                    context_before,
                    context_after,
                    origin: SourceOrigin::TextFile {
                        line: line_num,
                        col: start as u32,
                    },
                    score: None,
                });
                true
            })
            .map_err(|e| -> SinkError { Box::new(e) as SinkError })?;

        self.matches.extend(line_matches);
        Ok(true)
    }
}

fn search_text_file(
    path: &Path,
    matcher: &RegexMatcher,
    context_lines: u64,
) -> anyhow::Result<Vec<Match>> {
    let mut sink = CollectSink {
        matcher,
        matches: Vec::new(),
    };

    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .before_context(context_lines as usize)
        .after_context(context_lines as usize)
        .build();

    // Ignore per-file errors (permission denied, binary content, etc.)
    let _ = searcher.search_path(matcher, path, &mut sink);

    Ok(sink.matches)
}

// ── Extracted content search (PDF / future formats) ──────────────────────────

/// Extract a PDF live and search it. Used when the index is disabled, or does
/// not yet hold this file's current text.
fn live_pdf_matches(
    path: &Path,
    extractors: &ExtractorRegistry,
    matcher: &RegexMatcher,
    pdf_literal_matcher: Option<&RegexMatcher>,
) -> anyhow::Result<Vec<Match>> {
    let extractor = extractors
        .find(path, None)
        .ok_or_else(|| anyhow::anyhow!("no extractor registered"))?;
    let content = extractor.extract(path)?;
    search_text_and_map(
        &content.text,
        &content.source_map,
        matcher,
        pdf_literal_matcher,
    )
}

/// Search a PDF's text as held by the semantic index. Returns `None` when the
/// index cannot serve this file (not loaded, not indexed, changed on disk, or
/// indexed before schema v4), leaving the caller to extract it live.
enum IndexedPdfSearch {
    Served(Vec<Match>),
    Disabled,
    IndexUnavailable,
    DocumentUnavailable,
}

fn indexed_pdf_matches(
    index: &IndexHandle,
    path: &Path,
    matcher: &RegexMatcher,
    pdf_literal_matcher: Option<&RegexMatcher>,
) -> anyhow::Result<IndexedPdfSearch> {
    // Hold the index lock only long enough to copy out the stored text; run the
    // regex afterwards so we do not block indexing for the scan itself.
    let document = {
        let guard = index
            .lock()
            .map_err(|_| anyhow::anyhow!("semantic index lock poisoned"))?;
        let Some(idx) = guard.as_ref() else {
            return Ok(IndexedPdfSearch::IndexUnavailable);
        };
        idx.indexed_document_for_path(path)?
    };
    let Some((text, source_map)) = document else {
        return Ok(IndexedPdfSearch::DocumentUnavailable);
    };
    Ok(IndexedPdfSearch::Served(search_text_and_map(
        &text,
        &source_map,
        matcher,
        pdf_literal_matcher,
    )?))
}

/// Search already-extracted PDF text and resolve every result against the raw
/// extraction. Literal queries use an artifact-normalized projection; regex
/// queries retain their historical raw-text semantics. Shared by the live and
/// index-backed paths so both produce identical `Match` shapes.
fn search_text_and_map(
    full: &str,
    source_map: &SourceMap,
    matcher: &RegexMatcher,
    pdf_literal_matcher: Option<&RegexMatcher>,
) -> anyhow::Result<Vec<Match>> {
    if let Some(literal_matcher) = pdf_literal_matcher {
        let projection = pdf_projection::PdfSearchProjection::new(full);
        return collect_pdf_matches(
            full,
            source_map,
            projection.as_bytes(),
            literal_matcher,
            |range| projection.raw_range(range),
        );
    }

    collect_pdf_matches(full, source_map, full.as_bytes(), matcher, Some)
}

fn collect_pdf_matches(
    full: &str,
    source_map: &SourceMap,
    searchable: &[u8],
    matcher: &RegexMatcher,
    map_range: impl Fn(ByteRange) -> Option<ByteRange>,
) -> anyhow::Result<Vec<Match>> {
    let raw = full.as_bytes();
    let mut matches = Vec::new();

    matcher
        .find_iter(searchable, |m| {
            let Some(raw_range) = map_range(ByteRange {
                start: m.start(),
                end: m.end(),
            }) else {
                return true;
            };
            let matched_text =
                String::from_utf8_lossy(&raw[raw_range.start..raw_range.end]).into_owned();
            let origin =
                source_map
                    .resolve_range(raw_range.clone())
                    .unwrap_or(SourceOrigin::PdfPage {
                        page: 1,
                        bbox: None,
                    });

            // Extract ~120-char context windows around the match using char
            // boundaries so we don't split UTF-8 sequences.
            // We replace newlines with spaces in the context so the result looks
            // clean in the UI list even if it spans a line break.
            let ctx_before =
                extract_context_before(full, raw_range.start, 120).replace(['\n', '\r'], " ");
            let ctx_after =
                extract_context_after(full, raw_range.end, 120).replace(['\n', '\r'], " ");

            matches.push(Match {
                text_range: Some(raw_range),
                matched_text,
                context_before: ctx_before,
                context_after: ctx_after,
                origin,
                score: None,
            });
            true
        })
        .map_err(anyhow::Error::from)?;

    Ok(matches)
}

/// Return up to `max_chars` characters immediately before `byte_pos`,
/// trimming leading whitespace.
fn extract_context_before(text: &str, byte_pos: usize, max_chars: usize) -> String {
    // Walk back to a valid char boundary.
    let end = (0..=byte_pos.min(text.len()))
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    let prefix = &text[..end];
    let chars: Vec<char> = prefix.chars().collect();
    let start = chars.len().saturating_sub(max_chars);
    chars[start..]
        .iter()
        .collect::<String>()
        .trim_start()
        .to_string()
}

/// Return up to `max_chars` characters immediately after `byte_pos`,
/// trimming trailing whitespace.
fn extract_context_after(text: &str, byte_pos: usize, max_chars: usize) -> String {
    // Walk forward to a valid char boundary.
    let clamped = byte_pos.min(text.len());
    let start = (clamped..=text.len())
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(text.len());
    let chars: Vec<char> = text[start..].chars().collect();
    let end = chars.len().min(max_chars);
    chars[..end]
        .iter()
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{ContentExtractor, ExtractorRegistry};
    use crate::types::{ExtractedContent, SearchScope};
    use std::fs;
    use tempfile::tempdir;

    fn test_document(path: std::path::PathBuf, query: &SearchQuery) -> SearchDocument {
        SearchDocument {
            file_type: FileType::detect(&path, &query.supported_extensions).unwrap(),
            path,
            title: None,
            author: None,
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
            .filter(|path| {
                query.max_file_size == 0
                    || path
                        .metadata()
                        .is_ok_and(|metadata| metadata.len() <= query.max_file_size)
            })
            .filter(|path| FileType::detect(path, &query.supported_extensions).is_some())
            .map(|path| test_document(path, query))
            .collect()
    }

    #[test]
    fn test_build_matcher() {
        let mut query = SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: Path::new(".").to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let matcher = GrepSearchProvider::build_matcher(&query).unwrap();
        assert!(matcher.is_match("Hello".as_bytes()).unwrap());

        query.case_sensitive = true;
        let matcher = GrepSearchProvider::build_matcher(&query).unwrap();
        assert!(!matcher.is_match("Hello".as_bytes()).unwrap());
    }

    #[test]
    fn test_context_extraction() {
        let text = "The quick brown fox jumps over the lazy dog";
        // fox starts at index 16
        // "brown " is before "fox" (from index 10 to 16)
        assert_eq!(extract_context_before(text, 16, 6), "brown ");
        assert_eq!(extract_context_after(text, 19, 6), " jumps");
    }

    #[test]
    fn test_search_text_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "line 1\nmatch this\nline 3").unwrap();

        let query = SearchQuery {
            pattern: "match".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let matcher = GrepSearchProvider::build_matcher(&query).unwrap();
        let matches = search_text_file(&path, &matcher, 0).unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "match");
        match matches[0].origin {
            SourceOrigin::TextFile { line, .. } => assert_eq!(line, 2),
            _ => panic!("Expected TextFile origin"),
        }
    }

    #[test]
    fn test_search_regex() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "user_123\nadmin_456\nguest").unwrap();

        let query = SearchQuery {
            pattern: r"\w+_\d+".to_string(),
            is_regex: true,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let matcher = GrepSearchProvider::build_matcher(&query).unwrap();
        let matches = search_text_file(&path, &matcher, 0).unwrap();

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].matched_text, "user_123");
        assert_eq!(matches[1].matched_text, "admin_456");
    }

    #[test]
    fn test_search_with_context() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "line 1\nline 2 (target)\nline 3").unwrap();

        let query = SearchQuery {
            pattern: "target".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 1, // One line of context
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let matcher = GrepSearchProvider::build_matcher(&query).unwrap();
        let matches = search_text_file(&path, &matcher, 1).unwrap();

        assert_eq!(matches.len(), 1);
        // Note: CollectSink currently only captures the matched line,
        // but it could be extended to capture context if needed.
        // Currently context_before/after in Match struct are from the SAME line.
        assert_eq!(matches[0].matched_text, "target");
        assert!(matches[0].context_before.contains("line 2 ("));
        assert!(matches[0].context_after.contains(")"));
    }

    #[test]
    fn test_search_extracted_content() {
        use crate::types::ExtractedContent;
        use crate::types::FileMetadata;
        use crate::types::SourceMap;
        use std::path::PathBuf;

        let content = ExtractedContent {
            text: "The quick brown fox jumps over the lazy dog".to_string(),
            source_map: SourceMap { segments: vec![] }, // Empty source map
            metadata: FileMetadata {
                path: PathBuf::from("test.pdf"),
                size_bytes: 0,
                mime: Some("application/pdf".to_string()),
                title: None,
                page_count: Some(1),
            },
            images: Vec::new(),
        };
        let query = SearchQuery {
            pattern: "fox".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: Path::new(".").to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };
        let matcher = GrepSearchProvider::build_matcher(&query).unwrap();
        let literal_matcher =
            pdf_projection::literal_matcher(&query.pattern, query.case_sensitive).unwrap();
        let matches = search_text_and_map(
            &content.text,
            &content.source_map,
            &matcher,
            Some(&literal_matcher),
        )
        .unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "fox");
        assert_eq!(matches[0].context_before, "The quick brown ");
        assert_eq!(matches[0].context_after, " jumps over the lazy dog");
    }

    /// The reading keeps the lines a page broke a sentence into, so a passage
    /// query still crosses them, and the match still resolves to every segment
    /// it covered — with their boxes merged, because a highlight is drawn over
    /// both lines.
    #[test]
    fn literal_pdf_match_maps_artifacts_back_to_raw_range_and_page() {
        use crate::types::{BoundingBox, SourceSegment};

        let raw = "prefix The topic should also be something\nthat interests you. suffix";
        let some_start = raw.find("something").unwrap();
        let some_end = some_start + "something".len();
        let thing_start = raw.find("that").unwrap();
        let thing_end = thing_start + "that".len();
        let source_map = SourceMap {
            segments: vec![
                SourceSegment {
                    text_range: ByteRange {
                        start: some_start,
                        end: some_end,
                    },
                    origin: SourceOrigin::PdfPage {
                        page: 4,
                        bbox: Some(BoundingBox {
                            x: 100.0,
                            y: 200.0,
                            width: 40.0,
                            height: 10.0,
                        }),
                    },
                    provenance: Default::default(),
                },
                SourceSegment {
                    text_range: ByteRange {
                        start: thing_start,
                        end: thing_end,
                    },
                    origin: SourceOrigin::PdfPage {
                        page: 4,
                        bbox: Some(BoundingBox {
                            x: 100.0,
                            y: 215.0,
                            width: 35.0,
                            height: 10.0,
                        }),
                    },
                    provenance: Default::default(),
                },
            ],
        };
        let query = "The topic should also be something that interests you.";
        let raw_matcher = RegexMatcher::new(&regex::escape(query)).unwrap();
        let literal_matcher = pdf_projection::literal_matcher(query, true).unwrap();

        let matches =
            search_text_and_map(raw, &source_map, &raw_matcher, Some(&literal_matcher)).unwrap();

        assert_eq!(matches.len(), 1);
        let found = &matches[0];
        let range = found.text_range.as_ref().unwrap();
        assert_eq!(
            &raw[range.start..range.end],
            "The topic should also be something\nthat interests you."
        );
        assert_eq!(found.matched_text, &raw[range.start..range.end]);
        match &found.origin {
            SourceOrigin::PdfPage { page, bbox } => {
                assert_eq!(*page, 4);
                let bbox = bbox.as_ref().unwrap();
                assert_eq!(bbox.x, 100.0);
                assert_eq!(bbox.y, 200.0);
                assert_eq!(bbox.width, 40.0);
                assert_eq!(bbox.height, 25.0);
            }
            other => panic!("expected PDF origin, got {other:?}"),
        }
    }

    #[test]
    fn regex_pdf_search_keeps_raw_extraction_semantics() {
        let raw = "some-\nthing";
        let source_map = SourceMap { segments: vec![] };
        let unhyphenated = RegexMatcher::new("something").unwrap();
        let raw_artifact = RegexMatcher::new("some-\\nthing").unwrap();

        assert!(search_text_and_map(raw, &source_map, &unhyphenated, None)
            .unwrap()
            .is_empty());
        let matches = search_text_and_map(raw, &source_map, &raw_artifact, None).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, raw);
    }

    #[test]
    fn test_search_provider_file_size_filter() {
        let dir = tempdir().unwrap();
        let path_small = dir.path().join("small.txt");
        let path_large = dir.path().join("large.txt");
        fs::write(&path_small, "match").unwrap();
        fs::write(&path_large, "match but too large").unwrap();

        let query = SearchQuery {
            pattern: "match".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 10, // Max 10 bytes
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let extractors = ExtractorRegistry::new();
        let outcome = provider
            .search(&query, &extractors, tx, &test_documents(&query))
            .unwrap();

        let mut results = Vec::new();
        while let Some(m) = rx.blocking_recv() {
            results.push(m);
        }

        assert_eq!(results.len(), 1);
        assert!(results[0].path.ends_with("small.txt"));
        assert_eq!(outcome.files_scanned, Some(1));
        assert!(outcome.errors.is_empty());
    }

    #[test]
    fn file_scoped_search_uses_catalog_size_exclusion() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large.txt");
        fs::write(&path, "match but too large").unwrap();

        let query = SearchQuery {
            pattern: "match".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 10,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: SearchScope::File { path: path.clone() },
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let outcome = provider
            .search(
                &query,
                &ExtractorRegistry::new(),
                tx,
                &test_documents(&query),
            )
            .unwrap();

        assert!(rx.blocking_recv().is_none());
        assert_eq!(outcome.files_scanned, Some(0));
        assert!(outcome.errors.is_empty());
    }

    #[test]
    fn search_counts_supported_files_without_matches_as_scanned() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("note.txt"), "haystack").unwrap();

        let query = SearchQuery {
            pattern: "needle".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: SearchScope::Corpus,
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let outcome = provider
            .search(
                &query,
                &ExtractorRegistry::new(),
                tx,
                &test_documents(&query),
            )
            .unwrap();

        assert!(rx.blocking_recv().is_none());
        assert_eq!(outcome.files_scanned, Some(1));
        assert!(outcome.errors.is_empty());
    }

    #[test]
    fn test_search_all_uses_multiple_roots_and_global_limit() {
        let dir = tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join("a.txt"), "needle").unwrap();
        fs::write(second.join("b.txt"), "needle").unwrap();

        let query = SearchQuery {
            pattern: "needle".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: Path::new("unused-for-all").to_path_buf(),
            max_results: 1,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: SearchScope::All,
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let documents = vec![
            test_document(first.join("a.txt"), &query),
            test_document(second.join("b.txt"), &query),
        ];
        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let extractors = ExtractorRegistry::new();
        std::thread::spawn(move || {
            provider
                .search(&query, &extractors, tx, &documents)
                .unwrap()
        });

        let mut results = Vec::new();
        while let Some(result) = rx.blocking_recv() {
            results.push(result);
        }
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].matches.len(), 1);
    }

    #[test]
    fn test_search_file_scope_ignores_siblings() {
        let dir = tempdir().unwrap();
        let scoped = dir.path().join("scoped.txt");
        let sibling = dir.path().join("sibling.txt");
        fs::write(&scoped, "match here").unwrap();
        fs::write(&sibling, "match elsewhere").unwrap();

        let query = SearchQuery {
            pattern: "match".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: SearchScope::File {
                path: scoped.clone(),
            },
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let extractors = ExtractorRegistry::new();
        std::thread::spawn(move || {
            let documents = test_documents(&query);
            provider
                .search(&query, &extractors, tx, &documents)
                .unwrap();
        });

        let mut results = Vec::new();
        while let Some(m) = rx.blocking_recv() {
            results.push(m);
        }
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, scoped);
        assert_eq!(results[0].matches.len(), 1);
    }

    #[test]
    fn index_backed_grep_serves_matches_without_extraction() {
        use crate::embed::index::chunk::Chunk;
        use crate::embed::index::db::{PreparedFile, SemanticIndex};
        use crate::types::{EmbeddingEngine, SourceOrigin};
        use std::sync::Arc;

        let dir = tempdir().unwrap();
        let root = dir.path();
        let pdf = root.join("paper.pdf");
        // Real bytes so the file's on-disk identity matches what we index; the
        // content is irrelevant because matches are served from the index.
        fs::write(&pdf, "%PDF-1.7 opaque bytes").unwrap();

        let mut idx =
            SemanticIndex::create(root, "m", 3, EmbeddingEngine::Candle, Some(root)).unwrap();
        idx.write_file(PreparedFile {
            retained: Default::default(),
            path: pdf.clone(),
            full_text: "alpha something gamma".to_string(),
            chunks: vec![(
                Chunk {
                    file_path: pdf.clone(),
                    text: "alpha something gamma".to_string(),
                    byte_range: ByteRange { start: 0, end: 21 },
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: None,
                    },
                },
                vec![1.0, 0.0, 0.0],
            )],
        })
        .unwrap();
        let handle = Arc::new(Mutex::new(Some(idx)));

        let query = SearchQuery {
            pattern: "something".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: root.to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: SearchScope::File { path: pdf.clone() },
            supported_extensions: vec!["pdf".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        // A failing extractor is the tripwire: if grep fell back to live
        // extraction instead of using the index, the search would error and
        // return no matches.
        let mut extractors = ExtractorRegistry::new();
        extractors.register(Box::new(FailingPdfExtractor));

        let provider = GrepSearchProvider::new().with_index(Some(handle));
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let search = std::thread::spawn(move || {
            let documents = test_documents(&query);
            provider
                .search(&query, &extractors, tx, &documents)
                .unwrap()
        });

        let mut results = Vec::new();
        while let Some(m) = rx.blocking_recv() {
            results.push(m);
        }
        let outcome = search.join().unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].matches.len(), 1);
        assert_eq!(results[0].matches[0].matched_text, "something");
        let range = results[0].matches[0].text_range.as_ref().unwrap();
        assert_eq!((range.start, range.end), (6, 15));
        match &results[0].matches[0].origin {
            SourceOrigin::PdfPage { page, .. } => assert_eq!(*page, 1),
            other => panic!("expected pdf page origin, got {other:?}"),
        }
        assert_eq!(outcome.indexed_pdf_reads, 1);
        assert_eq!(outcome.live_pdf_fallbacks, 0);
        assert_eq!(outcome.index_unavailable_fallbacks, 0);
    }

    struct FailingPdfExtractor;

    impl ContentExtractor for FailingPdfExtractor {
        fn can_handle(&self, path: &Path, _mime: Option<&str>) -> bool {
            path.extension().and_then(|e| e.to_str()) == Some("pdf")
        }

        fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent> {
            anyhow::bail!("failed to extract {}", path.display());
        }

        fn outline(&self, path: &Path) -> anyhow::Result<crate::types::DeclaredOutline> {
            anyhow::bail!("failed to read the outline of {}", path.display());
        }
    }

    #[test]
    fn test_search_provider_collects_pdf_extraction_errors() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("broken.pdf");
        fs::write(&path, "%PDF-1.7 fake").unwrap();

        let query = SearchQuery {
            pattern: "missing".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["pdf".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let provider = GrepSearchProvider::new();
        let mut extractors = ExtractorRegistry::new();
        extractors.register(Box::new(FailingPdfExtractor));
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let query_clone = query.clone();

        let handle = std::thread::spawn(move || {
            let documents = test_documents(&query_clone);
            provider
                .search(&query_clone, &extractors, tx, &documents)
                .unwrap()
        });

        assert!(rx.blocking_recv().is_none());
        let errors = handle.join().unwrap().errors;

        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("broken.pdf"));
        assert!(errors[0].contains("failed to extract"));
    }

    #[test]
    fn test_build_matcher_with_spaces() {
        let query = SearchQuery {
            pattern: "hello world".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: Path::new(".").to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let matcher = GrepSearchProvider::build_matcher(&query).unwrap();
        // Should match with varying whitespace
        assert!(matcher.is_match("hello   world".as_bytes()).unwrap());
        assert!(matcher.is_match("hello\nworld".as_bytes()).unwrap());
    }

    #[test]
    fn test_search_max_results() {
        let dir = tempdir().unwrap();
        // Use a single file with 3 matches to verify the global hit cap also
        // truncates within one file.
        fs::write(dir.path().join("test.txt"), "match 1\nmatch 2\nmatch 3").unwrap();

        let query = SearchQuery {
            pattern: "match".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 1, // Limit to 1 match
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let extractors = ExtractorRegistry::new();
        std::thread::spawn(move || {
            let documents = test_documents(&query);
            provider
                .search(&query, &extractors, tx, &documents)
                .unwrap();
        });

        let mut all_matches = Vec::new();
        while let Some(m) = rx.blocking_recv() {
            all_matches.extend(m.matches);
        }

        assert_eq!(all_matches.len(), 1);
    }

    #[test]
    fn test_search_supported_extensions_allow_list() {
        let dir = tempdir().unwrap();
        let path_rs = dir.path().join("main.rs");
        let path_txt = dir.path().join("notes.txt");
        fs::write(&path_rs, "fn main() {}").unwrap();
        fs::write(&path_txt, "fn main() {}").unwrap();

        let query = SearchQuery {
            pattern: "main".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["rs".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let extractors = ExtractorRegistry::new();
        std::thread::spawn(move || {
            let documents = test_documents(&query);
            provider
                .search(&query, &extractors, tx, &documents)
                .unwrap();
        });

        let mut results = Vec::new();
        while let Some(m) = rx.blocking_recv() {
            results.push(m);
        }

        assert_eq!(results.len(), 1);
        assert!(results[0].path.ends_with("main.rs"));
    }

    #[test]
    fn test_context_extraction_edge_cases() {
        let text = "short";
        assert_eq!(extract_context_before(text, 0, 10), "");
        assert_eq!(extract_context_after(text, 5, 10), "");
        assert_eq!(extract_context_before(text, 5, 10), "short");
        assert_eq!(extract_context_after(text, 0, 10), "short");

        // Invalid byte positions (between char boundaries)
        let emoji = "🦀🦀";
        // 🦀 is 4 bytes. byte 1 is not a char boundary.
        // extract_context_before will find the boundary at 0.
        assert_eq!(extract_context_before(emoji, 1, 10), "");
        // extract_context_after will find the boundary at 4.
        assert_eq!(extract_context_after(emoji, 1, 10), "🦀");
    }

    #[test]
    fn test_grep_capabilities() {
        let provider = GrepSearchProvider::new();
        let caps = provider.capabilities();
        assert!(caps.supports_regex);
        assert!(!caps.is_indexed);
    }

    #[test]
    fn test_search_cancellation() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("test.txt"), "content").unwrap();

        let query = SearchQuery {
            pattern: "content".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let provider = GrepSearchProvider::new();
        let (tx, rx) = tokio::sync::mpsc::channel(10);
        drop(rx); // Close receiver immediately

        let extractors = ExtractorRegistry::new();
        let res = provider.search(&query, &extractors, tx, &test_documents(&query));
        assert!(res.is_ok());
    }

    #[test]
    fn test_search_pdf_mock() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.pdf");
        fs::write(&path, "pdf binary data").unwrap();

        let query = SearchQuery {
            pattern: "pdf".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["pdf".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        use crate::extract::ContentExtractor;
        use crate::types::ExtractedContent;
        use crate::types::FileMetadata;
        use crate::types::SourceMap;

        struct MockPdfExtractor;
        impl ContentExtractor for MockPdfExtractor {
            fn can_handle(&self, _: &Path, _: Option<&str>) -> bool {
                true
            }
            fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent> {
                Ok(ExtractedContent {
                    text: "this is extracted pdf content".to_string(),
                    source_map: SourceMap { segments: vec![] },
                    metadata: FileMetadata {
                        path: path.to_path_buf(),
                        size_bytes: 0,
                        mime: None,
                        title: None,
                        page_count: None,
                    },
                    images: Vec::new(),
                })
            }
            fn outline(&self, _: &Path) -> anyhow::Result<crate::types::DeclaredOutline> {
                Ok(crate::types::DeclaredOutline::default())
            }
        }

        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(MockPdfExtractor));

        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        provider
            .search(&query, &registry, tx, &test_documents(&query))
            .unwrap();

        let mut results = Vec::new();
        while let Ok(m) = rx.try_recv() {
            results.push(m);
        }

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_type, FileType::Pdf);
    }

    #[test]
    fn test_search_skips_unsupported_extensions() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.png");
        fs::write(&path, "match").unwrap();

        let query = SearchQuery {
            pattern: "match".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let extractors = ExtractorRegistry::new();
        provider
            .search(&query, &extractors, tx, &test_documents(&query))
            .unwrap();

        let mut results = Vec::new();
        while let Ok(m) = rx.try_recv() {
            results.push(m);
        }
        assert!(results.is_empty());
    }

    #[test]
    fn exact_search_returns_filename_and_title_only_hits() {
        let dir = tempdir().unwrap();
        let filename_path = dir.path().join("Quarterly Report.txt");
        let title_path = dir.path().join("notes.txt");
        fs::write(&filename_path, "unrelated body").unwrap();
        fs::write(&title_path, "another unrelated body").unwrap();
        let query = SearchQuery {
            pattern: "report".into(),
            is_regex: false,
            case_sensitive: false,
            root: dir.path().to_path_buf(),
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: SearchScope::Corpus,
            supported_extensions: vec!["txt".into()],
            collection_id: None,
            tag_ids: Vec::new(),
        };
        let documents = vec![
            test_document(filename_path.clone(), &query),
            SearchDocument {
                path: title_path.clone(),
                file_type: FileType::PlainText,
                title: Some("Annual Research Report".into()),
                author: None,
            },
        ];
        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);

        provider
            .search(&query, &ExtractorRegistry::new(), tx, &documents)
            .unwrap();

        let mut results = Vec::new();
        while let Ok(result) = rx.try_recv() {
            results.push(result);
        }
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.matches.is_empty()));
        assert_eq!(
            results[0].field_matches[0].field,
            crate::types::SearchField::Filename
        );
        assert_eq!(
            results[1].field_matches[0].field,
            crate::types::SearchField::Title
        );
    }

    #[test]
    fn direct_field_hits_take_the_global_budget_before_content_hits() {
        let dir = tempdir().unwrap();
        let content_path = dir.path().join("a.txt");
        let filename_path = dir.path().join("needle-name.txt");
        fs::write(&content_path, "needle in content").unwrap();
        fs::write(&filename_path, "unrelated body").unwrap();
        let query = SearchQuery {
            pattern: "needle".into(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 1,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: SearchScope::Corpus,
            supported_extensions: vec!["txt".into()],
            collection_id: None,
            tag_ids: Vec::new(),
        };
        let documents = vec![
            test_document(content_path, &query),
            test_document(filename_path.clone(), &query),
        ];
        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);

        provider
            .search(&query, &ExtractorRegistry::new(), tx, &documents)
            .unwrap();

        let result = rx.blocking_recv().unwrap();
        assert_eq!(result.path, filename_path);
        assert_eq!(result.field_matches.len(), 1);
        assert!(result.matches.is_empty());
        assert!(rx.blocking_recv().is_none());
    }
}

#[cfg(test)]
mod eligibility_tests {
    use super::*;
    use crate::extract::ExtractorRegistry;
    use crate::types::{SearchQuery, SearchScope};
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn collection_eligibility_is_applied_before_global_limit() {
        let dir = tempdir().unwrap();
        let excluded = dir.path().join("a.txt");
        let included = dir.path().join("b.txt");
        fs::write(&excluded, "needle").unwrap();
        fs::write(&included, "needle").unwrap();
        let query = SearchQuery {
            pattern: "needle".into(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            max_results: 1,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            scope: SearchScope::Corpus,
            supported_extensions: vec!["txt".into()],
            collection_id: Some("test".into()),
            tag_ids: Vec::new(),
        };
        let documents = vec![SearchDocument {
            path: included.clone(),
            file_type: FileType::PlainText,
            title: None,
            author: None,
        }];
        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        std::thread::spawn(move || {
            provider
                .search(&query, &ExtractorRegistry::new(), tx, &documents)
                .unwrap();
        });
        let result = rx.recv().await.expect("eligible result");
        assert_eq!(result.path, included);
        assert!(rx.recv().await.is_none());
    }
}
