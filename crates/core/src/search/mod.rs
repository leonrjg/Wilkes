pub mod grep;
pub mod semantic;

use crate::extract::ExtractorRegistry;
use crate::types::{
    FileMatches, SearchCapabilities, SearchDocument, SearchField, SearchFieldMatch, SearchMode,
    SearchQuery,
};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use tokio::sync::mpsc;

pub type SearchResultTx = mpsc::Sender<FileMatches>;

/// Non-streaming information produced by a search provider. File matches still
/// travel over `SearchResultTx`; this outcome carries terminal diagnostics and
/// the exact query-expansion text that affected ranking.
#[derive(Debug, Default)]
pub struct SearchOutcome {
    pub errors: Vec<String>,
    pub hyde_documents: Vec<String>,
    /// Number of supported files whose contents were actually searched. `None`
    /// lets providers without file-scan semantics retain the stream-derived
    /// fallback used before this field existed.
    pub files_scanned: Option<usize>,
}

impl From<Vec<String>> for SearchOutcome {
    fn from(errors: Vec<String>) -> Self {
        Self {
            errors,
            hyde_documents: Vec::new(),
            files_scanned: None,
        }
    }
}

pub trait SearchProvider: Send + Sync {
    /// Begin searching. Results are sent to `tx` as they are discovered.
    /// Returns when the search is complete or cancelled (`tx.is_closed()`).
    /// The returned outcome contains non-fatal per-file errors (e.g. failed PDF
    /// extraction) and any query-expansion text that actually affected ranking.
    fn search(
        &self,
        query: &SearchQuery,
        extractors: &ExtractorRegistry,
        tx: SearchResultTx,
        documents: &[SearchDocument],
    ) -> anyhow::Result<SearchOutcome>;

    fn capabilities(&self) -> SearchCapabilities;
}

pub(crate) fn exact_matcher(query: &SearchQuery) -> anyhow::Result<RegexMatcher> {
    let pattern = if query.is_regex {
        query.pattern.clone()
    } else {
        let escaped = regex::escape(&query.pattern);
        escaped.replace(' ', r"\s+")
    };
    Ok(RegexMatcherBuilder::new()
        .case_insensitive(!query.case_sensitive)
        .build(&pattern)?)
}

pub(crate) fn field_matcher(query: &SearchQuery) -> anyhow::Result<RegexMatcher> {
    if query.mode == SearchMode::Grep {
        return exact_matcher(query);
    }
    Ok(RegexMatcherBuilder::new()
        .case_insensitive(true)
        .build(&regex::escape(&query.pattern))?)
}

/// Match filename and cached title without inventing a document-content
/// position. One hit per field is enough to admit the file and avoids a short
/// query consuming the global result budget repeatedly within one title.
pub(crate) fn document_field_matches(
    matcher: &RegexMatcher,
    document: &SearchDocument,
) -> anyhow::Result<Vec<SearchFieldMatch>> {
    let mut matches = Vec::with_capacity(2);
    if let Some(file_name) = document.path.file_name() {
        let filename = file_name.to_string_lossy();
        if let Some(found) = matcher.find(filename.as_bytes())? {
            matches.push(field_match(
                SearchField::Filename,
                &filename,
                found.start(),
                found.end(),
            ));
        }
    }
    if let Some(title) = document
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
    {
        if let Some(found) = matcher.find(title.as_bytes())? {
            matches.push(field_match(
                SearchField::Title,
                title,
                found.start(),
                found.end(),
            ));
        }
    }
    Ok(matches)
}

fn field_match(field: SearchField, value: &str, start: usize, end: usize) -> SearchFieldMatch {
    let bytes = value.as_bytes();
    SearchFieldMatch {
        field,
        matched_text: String::from_utf8_lossy(&bytes[start..end]).into_owned(),
        context_before: String::from_utf8_lossy(&bytes[..start]).into_owned(),
        context_after: String::from_utf8_lossy(&bytes[end..]).into_owned(),
    }
}

/// Enforce the global hit budget with direct identity hits first, then content
/// hits. Files remain unique and preserve their relative catalog/relevance
/// order within those two groups.
pub(crate) fn prioritize_and_limit_results(
    mut results: Vec<FileMatches>,
    max_results: usize,
) -> Vec<FileMatches> {
    let mut remaining = if max_results == 0 {
        usize::MAX
    } else {
        max_results
    };

    for result in &mut results {
        let keep = remaining.min(result.field_matches.len());
        result.field_matches.truncate(keep);
        remaining = remaining.saturating_sub(keep);
    }
    for result in &mut results {
        let keep = remaining.min(result.matches.len());
        result.matches.truncate(keep);
        remaining = remaining.saturating_sub(keep);
    }

    let mut field_results = Vec::new();
    let mut content_only_results = Vec::new();
    for result in results {
        if !result.field_matches.is_empty() {
            field_results.push(result);
        } else if !result.matches.is_empty() {
            content_only_results.push(result);
        }
    }
    field_results.extend(content_only_results);
    field_results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FileType;

    fn query(pattern: &str, case_sensitive: bool) -> SearchQuery {
        SearchQuery {
            pattern: pattern.into(),
            is_regex: true,
            case_sensitive,
            root: std::path::PathBuf::from("/library"),
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".into()],
            collection_id: None,
            tag_ids: Vec::new(),
        }
    }

    #[test]
    fn field_matching_honors_regex_case_and_unicode_boundaries() {
        let document = SearchDocument {
            path: std::path::PathBuf::from("/library/Résumé Paper.txt"),
            file_type: FileType::PlainText,
            title: None,
        };
        let insensitive = field_matcher(&query(r"^résumé\s", false)).unwrap();
        let matched = document_field_matches(&insensitive, &document).unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].matched_text, "Résumé ");

        let sensitive = field_matcher(&query(r"^résumé\s", true)).unwrap();
        assert!(document_field_matches(&sensitive, &document)
            .unwrap()
            .is_empty());
    }
}
