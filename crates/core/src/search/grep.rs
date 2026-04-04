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

    fn detect_file_type(path: &Path) -> Option<FileType> {
        const TEXT_EXTENSIONS: &[&str] = &[
            "txt", "md", "markdown", "rst", "rs", "py", "js", "ts", "jsx", "tsx",
            "json", "toml", "yaml", "yml", "xml", "html", "htm", "css", "scss",
            "sass", "less", "c", "cpp", "cc", "cxx", "h", "hpp", "java", "go",
            "rb", "sh", "bash", "zsh", "fish", "lua", "php", "swift", "kt",
            "cs", "r", "sql", "graphql", "gql", "proto", "ini", "cfg", "conf",
            "env", "gitignore", "lock", "log", "csv", "tsv", "jsonl",
        ];

        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext_lc = ext.to_ascii_lowercase();
            if ext_lc == "pdf" {
                return Some(FileType::Pdf);
            }
            if TEXT_EXTENSIONS.contains(&ext_lc.as_str()) {
                return Some(FileType::PlainText);
            }
        }

        // No extension or unknown extension — check well-known filenames.
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let name_lc = name.to_ascii_lowercase();
            if ["makefile", "dockerfile", "jenkinsfile", "procfile", "gemfile",
                "rakefile", "vagrantfile", "podfile", "brewfile"]
                .contains(&name_lc.as_str())
            {
                return Some(FileType::PlainText);
            }
        }

        None
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

            let file_type = match Self::detect_file_type(path) {
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
                .resolve(start)
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
