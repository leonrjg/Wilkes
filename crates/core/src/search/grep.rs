use crate::extract::ExtractorRegistry;
use crate::types::{ByteRange, FileMatches, FileType, Match, SearchCapabilities, SearchQuery, SourceOrigin};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkBuilder;
use std::path::Path;

use super::{SearchProvider, SearchResultTx};

pub struct GrepSearchProvider;

impl GrepSearchProvider {
    pub fn new() -> Self {
        Self
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
    ) -> anyhow::Result<Vec<String>> {
        let matcher = Self::build_matcher(query)?;

        let walk = WalkBuilder::new(&query.root)
            .git_ignore(query.respect_gitignore)
            .hidden(false)
            .build();

        let mut total_matches: usize = 0;
        let mut errors: Vec<String> = Vec::new();

        for entry in walk {
            if tx.is_closed() {
                break;
            }

            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            // File size filter
            if query.max_file_size > 0 {
                if let Ok(meta) = path.metadata() {
                    if meta.len() > query.max_file_size {
                        continue;
                    }
                }
            }

            // File type filter
            if !query.file_type_filters.is_empty() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !query.file_type_filters
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(ext))
                {
                    continue;
                }
            }

            let file_type = match FileType::detect(path, &query.supported_extensions) {
                Some(ft) => ft,
                None => {
                    // Use infer for unknown types; skip if recognised binary.
                    match infer::get_from_path(path) {
                        Ok(Some(_)) => continue, // known binary format — skip
                        _ => FileType::PlainText, // assume text
                    }
                }
            };

            let matches = match &file_type {
                FileType::PlainText => search_text_file(path, &matcher, query.context_lines as u64)?,
                FileType::Pdf => {
                    match extractors.find(path, None) {
                        Some(extractor) => match extractor.extract(path) {
                            Ok(content) => search_extracted_content(&content, &matcher)?,
                            Err(e) => {
                                errors.push(format!("{}: {e:#}", path.display()));
                                continue;
                            }
                        },
                        None => {
                            errors.push(format!("{}: no extractor registered", path.display()));
                            continue;
                        }
                    }
                }
            };

            if !matches.is_empty() {
                total_matches += matches.len();
                let file_matches = FileMatches {
                    path: path.to_path_buf(),
                    file_type,
                    matches,
                };
                if tx.blocking_send(file_matches).is_err() {
                    break;
                }
                if query.max_results > 0 && total_matches >= query.max_results {
                    break;
                }
            }
        }

        Ok(errors)
    }

    fn capabilities(&self) -> SearchCapabilities {
        SearchCapabilities {
            supports_regex: true,
            supports_case_sensitivity: true,
            is_indexed: false,
            supported_file_types: vec![
                "txt".into(), "md".into(), "rs".into(), "py".into(), "js".into(),
                "ts".into(), "json".into(), "toml".into(), "yaml".into(),
            ],
            requires_index: false,
            semantic_index_built: false,
            supported_engines: crate::types::EmbeddingEngine::supported_engines(),
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

    fn matched(
        &mut self,
        _searcher: &Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        let line = mat.bytes();
        let line_num = mat.line_number().unwrap_or(0) as u32;
        let base_offset = mat.absolute_byte_offset() as usize;

        // Collect all matches within this line without holding self borrow.
        let mut line_matches: Vec<Match> = Vec::new();

        self.matcher
            .find_iter(line, |m| {
                let start = m.start();
                let end = m.end();
                let matched_text =
                    String::from_utf8_lossy(&line[start..end]).into_owned();
                let context_before =
                    String::from_utf8_lossy(&line[..start]).into_owned();
                let context_after =
                    String::from_utf8_lossy(&line[end..])
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

fn search_text_file(path: &Path, matcher: &RegexMatcher, context_lines: u64) -> anyhow::Result<Vec<Match>> {
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

fn search_extracted_content(
    content: &crate::types::ExtractedContent,
    matcher: &RegexMatcher,
) -> anyhow::Result<Vec<Match>> {
    let text = content.text.as_bytes();
    let full = &content.text;
    let mut matches = Vec::new();

    matcher
        .find_iter(text, |m| {
            let start = m.start();
            let end = m.end();
            let matched_text = String::from_utf8_lossy(&text[start..end]).into_owned();
            let origin = content
                .source_map
                .resolve_range(ByteRange { start, end })
                .unwrap_or(SourceOrigin::PdfPage { page: 1, bbox: None });

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
    chars[start..].iter().collect::<String>().trim_start().to_string()
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
    chars[..end].iter().collect::<String>().trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs;

    #[test]
    fn test_build_matcher() {
        let mut query = SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: Path::new(".").to_path_buf(),
            file_type_filters: vec![],
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            supported_extensions: vec![],
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
            file_type_filters: vec![],
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            supported_extensions: vec!["txt".to_string()],
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
            file_type_filters: vec![],
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            supported_extensions: vec!["txt".to_string()],
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
            file_type_filters: vec![],
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 1, // One line of context
            mode: crate::types::SearchMode::Grep,
            supported_extensions: vec!["txt".to_string()],
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
    fn test_search_provider_filtering() {
        let dir = tempdir().unwrap();
        let path_txt = dir.path().join("test.txt");
        let path_rs = dir.path().join("test.rs");
        fs::write(&path_txt, "hello world").unwrap();
        fs::write(&path_rs, "hello world").unwrap();

        let query = SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: dir.path().to_path_buf(),
            file_type_filters: vec!["rs".to_string()], // Only .rs files
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: crate::types::SearchMode::Grep,
            supported_extensions: vec!["txt".to_string(), "rs".to_string()],
        };

        let provider = GrepSearchProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        
        // Use a separate thread or async block to avoid blocking
        let query_clone = query.clone();
        let extractors = ExtractorRegistry::new();
        std::thread::spawn(move || {
            provider.search(&query_clone, &extractors, tx).unwrap();
        });

        let mut results = Vec::new();
        while let Some(m) = rx.blocking_recv() {
            results.push(m);
        }

        assert_eq!(results.len(), 1);
        assert!(results[0].path.ends_with("test.rs"));
        assert!(!results[0].path.ends_with("test.txt"));
    }
}
