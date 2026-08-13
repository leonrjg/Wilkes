use crate::embed::index::SemanticIndex;
use crate::extract::ExtractorRegistry;
use crate::types::{
    ByteRange, FileMatches, FileType, Match, SearchCapabilities, SearchQuery, SearchScope,
    SourceMap, SourceOrigin,
};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{WalkBuilder, WalkState};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{SearchOutcome, SearchProvider, SearchResultTx};

/// Shared handle to the live semantic index, as held by the API layer.
type IndexHandle = Arc<Mutex<Option<SemanticIndex>>>;

pub struct GrepSearchProvider {
    all_roots: Vec<std::path::PathBuf>,
    all_root_errors: Vec<String>,
    /// When set, PDF matches are found against text the index already holds
    /// instead of re-extracting the file. Files the index does not hold (or that
    /// changed on disk) fall back to live extraction. `None` disables this and
    /// makes every PDF extract live, i.e. the setting is off.
    index: Option<IndexHandle>,
}

impl GrepSearchProvider {
    pub fn new() -> Self {
        Self {
            all_roots: Vec::new(),
            all_root_errors: Vec::new(),
            index: None,
        }
    }

    pub fn with_all_roots(
        all_roots: Vec<std::path::PathBuf>,
        all_root_errors: Vec<String>,
    ) -> Self {
        Self {
            all_roots,
            all_root_errors,
            index: None,
        }
    }

    /// Enable index-backed exact search: PDF matches are read from the semantic
    /// index when it holds the file's current text, otherwise extracted live.
    pub fn with_index(mut self, index: Option<IndexHandle>) -> Self {
        self.index = index;
        self
    }

    fn build_matcher(query: &SearchQuery) -> anyhow::Result<RegexMatcher> {
        let pattern = if query.is_regex {
            query.pattern.clone()
        } else {
            let escaped = regex::escape(&query.pattern);
            // Replace literal spaces with \s+ to handle varying whitespace/newlines
            // in all file types (especially PDFs).
            escaped.replace(" ", r"\s+")
        };
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(!query.case_sensitive)
            .build(&pattern)?;
        Ok(matcher)
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
        eligible_paths: Option<&std::collections::HashSet<std::path::PathBuf>>,
    ) -> anyhow::Result<SearchOutcome> {
        let matcher = Self::build_matcher(query)?;
        let total_matches = AtomicUsize::new(0);
        let files_scanned = AtomicUsize::new(0);
        let errors = Mutex::new(if query.scope == SearchScope::All {
            self.all_root_errors.clone()
        } else {
            Vec::new()
        });

        let index = self.index.as_ref();

        // A single-file scope has nothing to walk in parallel.
        if let SearchScope::File { path } = &query.scope {
            let _ = search_path(
                path,
                query,
                extractors,
                &matcher,
                &tx,
                &total_matches,
                &files_scanned,
                &errors,
                eligible_paths,
                index,
            )?;
            return Ok(SearchOutcome {
                errors: errors.into_inner().unwrap(),
                hyde_documents: Vec::new(),
                files_scanned: Some(files_scanned.load(Ordering::Relaxed)),
            });
        }

        // Resolve the root set: the single root for Corpus, every library root
        // for All.
        let (first, rest): (&Path, &[std::path::PathBuf]) = match &query.scope {
            SearchScope::Corpus => (query.root.as_path(), &[]),
            SearchScope::All => self
                .all_roots
                .split_first()
                .map(|(first, rest)| (first.as_path(), rest))
                .ok_or_else(|| {
                    anyhow::anyhow!("No accessible library directories are configured")
                })?,
            SearchScope::File { .. } => unreachable!("File scope handled above"),
        };

        let mut builder = WalkBuilder::new(first);
        for root in rest {
            builder.add(root);
        }
        builder.git_ignore(query.respect_gitignore).hidden(false);

        // Fan the walk across a thread pool. PDF extraction dominates grep cost
        // and is independent per file, so this is where the wall-clock win comes
        // from. Match count and errors are shared synchronized state; result
        // ordering is not preserved, which is fine because matches already stream
        // to the caller as they are found rather than being returned as a batch.
        builder.build_parallel().run(|| {
            Box::new(|entry| {
                if tx.is_closed() {
                    return WalkState::Quit;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => return WalkState::Continue,
                };
                let path = entry.path();
                if !path.is_file() {
                    return WalkState::Continue;
                }
                match search_path(
                    path,
                    query,
                    extractors,
                    &matcher,
                    &tx,
                    &total_matches,
                    &files_scanned,
                    &errors,
                    eligible_paths,
                    index,
                ) {
                    // Stop the whole walk: max_results reached or receiver closed.
                    Ok(true) => WalkState::Quit,
                    Ok(false) => WalkState::Continue,
                    // The pattern was already validated by build_matcher, so an
                    // Err here is a single-file failure that must not abort the
                    // rest of the search. Log and record it, then keep walking.
                    Err(err) => {
                        tracing::error!("grep error at {}: {err:#}", path.display());
                        errors
                            .lock()
                            .unwrap()
                            .push(format!("{}: {err:#}", path.display()));
                        WalkState::Continue
                    }
                }
            })
        });

        Ok(SearchOutcome {
            errors: errors.into_inner().unwrap(),
            hyde_documents: Vec::new(),
            files_scanned: Some(files_scanned.load(Ordering::Relaxed)),
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

#[allow(clippy::too_many_arguments)]
fn search_path(
    path: &Path,
    query: &SearchQuery,
    extractors: &ExtractorRegistry,
    matcher: &RegexMatcher,
    tx: &SearchResultTx,
    total_matches: &AtomicUsize,
    files_scanned: &AtomicUsize,
    errors: &Mutex<Vec<String>>,
    eligible_paths: Option<&std::collections::HashSet<std::path::PathBuf>>,
    index: Option<&IndexHandle>,
) -> anyhow::Result<bool> {
    if tx.is_closed() {
        return Ok(true);
    }

    if !path.is_file() {
        return Ok(false);
    }

    if eligible_paths.is_some_and(|eligible| !eligible.contains(path)) {
        return Ok(false);
    }

    if query.max_file_size > 0 {
        if let Ok(meta) = path.metadata() {
            if meta.len() > query.max_file_size {
                if matches!(query.scope, SearchScope::File { .. }) {
                    errors.lock().unwrap().push(format!(
                        "Search file exceeds the configured maximum size ({} bytes > {} bytes): {}",
                        meta.len(),
                        query.max_file_size,
                        path.display()
                    ));
                }
                return Ok(false);
            }
        }
    }

    let Some(file_type) = FileType::detect(path, &query.supported_extensions) else {
        return Ok(false);
    };

    files_scanned.fetch_add(1, Ordering::Relaxed);

    let matches = match &file_type {
        // Plain text is memory-mapped and already fast; it also carries exact
        // line/column origins that the chunk-granular index cannot reproduce, so
        // it never uses the index regardless of the setting.
        FileType::PlainText => search_text_file(path, matcher, query.context_lines as u64)?,
        FileType::Pdf => {
            // Prefer text the index already holds. A genuine index fault is
            // logged and demoted to live extraction rather than failing the
            // file; "not indexed / stale / pre-v4" simply returns None.
            let from_index = index.and_then(|handle| {
                indexed_pdf_matches(handle, path, matcher).unwrap_or_else(|e| {
                    tracing::warn!(
                        "index-backed grep failed for {}, extracting live: {e:#}",
                        path.display()
                    );
                    None
                })
            });
            match from_index {
                Some(matches) => matches,
                None => match live_pdf_matches(path, extractors, matcher) {
                    Ok(matches) => matches,
                    Err(e) => {
                        errors
                            .lock()
                            .unwrap()
                            .push(format!("{}: {e:#}", path.display()));
                        return Ok(false);
                    }
                },
            }
        }
    };

    if matches.is_empty() {
        return Ok(false);
    }

    // Atomically claim this file's matches against the global budget. Reserving
    // before sending is what keeps `max_results` a hard cap under parallelism:
    // if another worker already spent the budget, `previous` is past the limit
    // and we drop this file instead of racing an extra result past the cap.
    let previous = total_matches.fetch_add(matches.len(), Ordering::Relaxed);
    if query.max_results > 0 && previous >= query.max_results {
        return Ok(true);
    }

    let running_total = previous + matches.len();
    let file_matches = FileMatches {
        path: path.to_path_buf(),
        file_type,
        title: None,
        matches,
    };
    if tx.blocking_send(file_matches).is_err() {
        return Ok(true);
    }

    Ok(query.max_results > 0 && running_total >= query.max_results)
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
) -> anyhow::Result<Vec<Match>> {
    let extractor = extractors
        .find(path, None)
        .ok_or_else(|| anyhow::anyhow!("no extractor registered"))?;
    let content = extractor.extract(path)?;
    search_text_and_map(&content.text, &content.source_map, matcher)
}

/// Search a PDF's text as held by the semantic index. Returns `None` when the
/// index cannot serve this file (not loaded, not indexed, changed on disk, or
/// indexed before schema v4), leaving the caller to extract it live.
fn indexed_pdf_matches(
    index: &IndexHandle,
    path: &Path,
    matcher: &RegexMatcher,
) -> anyhow::Result<Option<Vec<Match>>> {
    // Hold the index lock only long enough to copy out the stored text; run the
    // regex afterwards so we do not block indexing for the scan itself.
    let document = {
        let guard = index
            .lock()
            .map_err(|_| anyhow::anyhow!("semantic index lock poisoned"))?;
        let Some(idx) = guard.as_ref() else {
            return Ok(None);
        };
        idx.indexed_document_for_path(path)?
    };
    let Some((text, source_map)) = document else {
        return Ok(None);
    };
    Ok(Some(search_text_and_map(&text, &source_map, matcher)?))
}

/// Run the exact matcher over already-extracted `text`, resolving each match's
/// source position through `source_map`. Shared by the live and index-backed
/// PDF paths so both produce identical `Match` shapes.
fn search_text_and_map(
    full: &str,
    source_map: &SourceMap,
    matcher: &RegexMatcher,
) -> anyhow::Result<Vec<Match>> {
    let text = full.as_bytes();
    let mut matches = Vec::new();

    matcher
        .find_iter(text, |m| {
            let start = m.start();
            let end = m.end();
            let matched_text = String::from_utf8_lossy(&text[start..end]).into_owned();
            let origin = source_map
                .resolve_range(ByteRange { start, end })
                .unwrap_or(SourceOrigin::PdfPage {
                    page: 1,
                    bbox: None,
                });

            // Extract ~120-char context windows around the match using char
            // boundaries so we don't split UTF-8 sequences.
            // We replace newlines with spaces in the context so the result looks
            // clean in the UI list even if it spans a line break.
            let ctx_before = extract_context_before(full, start, 120).replace(['\n', '\r'], " ");
            let ctx_after = extract_context_after(full, end, 120).replace(['\n', '\r'], " ");

            matches.push(Match {
                text_range: Some(ByteRange { start, end }),
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
    use crate::types::ExtractedContent;
    use std::fs;
    use tempfile::tempdir;

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
        let matches = search_text_and_map(&content.text, &content.source_map, &matcher).unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "fox");
        assert_eq!(matches[0].context_before, "The quick brown ");
        assert_eq!(matches[0].context_after, " jumps over the lazy dog");
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
        let outcome = provider.search(&query, &extractors, tx, None).unwrap();

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
    fn file_scoped_search_reports_oversized_file() {
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
            .search(&query, &ExtractorRegistry::new(), tx, None)
            .unwrap();

        assert!(rx.blocking_recv().is_none());
        assert_eq!(outcome.files_scanned, Some(0));
        assert_eq!(outcome.errors.len(), 1);
        assert!(outcome.errors[0].contains("exceeds the configured maximum size"));
        assert!(outcome.errors[0].contains("19 bytes > 10 bytes"));
        assert!(outcome.errors[0].contains(&path.display().to_string()));
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
            .search(&query, &ExtractorRegistry::new(), tx, None)
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

        let provider = GrepSearchProvider::with_all_roots(vec![first, second], Vec::new());
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let extractors = ExtractorRegistry::new();
        std::thread::spawn(move || provider.search(&query, &extractors, tx, None).unwrap());

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
            provider.search(&query, &extractors, tx, None).unwrap();
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
            path: pdf.clone(),
            full_text: "alpha beta gamma".to_string(),
            chunks: vec![(
                Chunk {
                    file_path: pdf.clone(),
                    text: "alpha beta gamma".to_string(),
                    byte_range: ByteRange { start: 0, end: 16 },
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
            pattern: "beta".to_string(),
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
        std::thread::spawn(move || {
            provider.search(&query, &extractors, tx, None).unwrap();
        });

        let mut results = Vec::new();
        while let Some(m) = rx.blocking_recv() {
            results.push(m);
        }

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].matches.len(), 1);
        assert_eq!(results[0].matches[0].matched_text, "beta");
        match &results[0].matches[0].origin {
            SourceOrigin::PdfPage { page, .. } => assert_eq!(*page, 1),
            other => panic!("expected pdf page origin, got {other:?}"),
        }
    }

    struct FailingPdfExtractor;

    impl ContentExtractor for FailingPdfExtractor {
        fn can_handle(&self, path: &Path, _mime: Option<&str>) -> bool {
            path.extension().and_then(|e| e.to_str()) == Some("pdf")
        }

        fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent> {
            anyhow::bail!("failed to extract {}", path.display());
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
            provider
                .search(&query_clone, &extractors, tx, None)
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
        // Use a single file with 3 matches to test that it returns all of them
        // before breaking, as it checks the limit only after processing each file.
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
            provider.search(&query, &extractors, tx, None).unwrap();
        });

        let mut all_matches = Vec::new();
        while let Some(m) = rx.blocking_recv() {
            all_matches.extend(m.matches);
        }

        // It should return all matches from the first file.
        assert_eq!(all_matches.len(), 3);
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
            provider.search(&query, &extractors, tx, None).unwrap();
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
        let res = provider.search(&query, &extractors, tx, None);
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
                })
            }
        }

        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(MockPdfExtractor));

        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        provider.search(&query, &registry, tx, None).unwrap();

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
        provider.search(&query, &extractors, tx, None).unwrap();

        let mut results = Vec::new();
        while let Ok(m) = rx.try_recv() {
            results.push(m);
        }
        assert!(results.is_empty());
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
        let eligible = std::collections::HashSet::from([included.clone()]);
        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        std::thread::spawn(move || {
            provider
                .search(&query, &ExtractorRegistry::new(), tx, Some(&eligible))
                .unwrap();
        });
        let result = rx.recv().await.expect("eligible result");
        assert_eq!(result.path, included);
        assert!(rx.recv().await.is_none());
    }
}
