//! The tools' parameters and responses, as the MCP schema describes them.

use super::*;

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum IntegerOrString<T> {
    Integer(T),
    String(String),
}

pub(super) fn deserialize_optional_integer<'de, D, T>(
    deserializer: D,
) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + std::str::FromStr,
{
    match Option::<IntegerOrString<T>>::deserialize(deserializer)? {
        None => Ok(None),
        Some(IntegerOrString::Integer(value)) => Ok(Some(value)),
        Some(IntegerOrString::String(value)) => value
            .parse()
            .map(Some)
            .map_err(|_| serde::de::Error::custom(format!("invalid integer string {value:?}"))),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum BoolOrString {
    Bool(bool),
    String(String),
}

pub(super) fn deserialize_optional_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<BoolOrString>::deserialize(deserializer)? {
        None => Ok(None),
        Some(BoolOrString::Bool(value)) => Ok(Some(value)),
        Some(BoolOrString::String(value)) => match value.as_str() {
            "true" => Ok(Some(true)),
            "false" => Ok(Some(false)),
            _ => Err(serde::de::Error::custom(format!(
                "invalid boolean string {value:?}"
            ))),
        },
    }
}

/// Parameters for the tools whose only input is which workspace to read.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct WorkspaceParams {
    /// Workspace id to read, from list_context. Omit for the workspace that is
    /// active in Wilkes. Naming one does not switch the app to it.
    pub(super) workspace: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct GetDocumentTextParams {
    /// Document to read. Required for external MCP clients; an in-app chat may
    /// omit it to use that chat session's active document.
    pub(super) path: Option<String>,
    /// One 1-based PDF page to read. Pass either page or page_range, not both.
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<u32>")]
    pub(super) page: Option<u32>,
    /// Inclusive 1-based PDF page range in "N-M" format, for example "7-9".
    pub(super) page_range: Option<String>,
    /// Maximum characters to return (default 24000, capped at 120000).
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
    pub(super) max_chars: Option<usize>,
    /// Workspace id to read, from list_context. Omit for the workspace that is
    /// active in Wilkes. Naming one does not switch the app to it.
    pub(super) workspace: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct GetDocumentOutlineParams {
    /// Document whose declared outline to read. Required for external MCP
    /// clients; an in-app chat may omit it to use that chat's active document.
    pub(super) path: Option<String>,
    /// Workspace id to read, from list_context. Omit for the workspace that is
    /// active in Wilkes. Naming one does not switch the app to it.
    pub(super) workspace: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct SearchParams {
    /// Text to search for.
    pub(super) query: String,
    /// Optional. Defaults to combined, which searches wording and meaning at
    /// once. Use exact for literal/regex matching, semantic for meaning only.
    #[serde(default)]
    pub(super) mode: SearchModeParam,
    /// Search location. Use all for a library-wide search; omit for the current root.
    pub(super) scope: Option<SearchScopeParam>,
    /// Corpus/index root. Omit unless searching a different root is intentional.
    pub(super) root: Option<String>,
    /// Restrict search to one file in any configured library root or chat
    /// context. Use this for questions about a concrete document.
    pub(super) file: Option<String>,
    /// Maximum matches to return.
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
    pub(super) max_results: Option<usize>,
    /// Exact search only.
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    #[schemars(with = "Option<bool>")]
    pub(super) case_sensitive: Option<bool>,
    /// Exact search only.
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    #[schemars(with = "Option<bool>")]
    pub(super) is_regex: Option<bool>,
    /// Exact search context lines.
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<u32>")]
    pub(super) context_lines: Option<u32>,
    /// Optional saved smart collection ID to intersect with the chosen scope.
    pub(super) collection_id: Option<String>,
    /// Workspace id to read, from list_context. Omit for the workspace that is
    /// active in Wilkes. Naming one does not switch the app to it.
    pub(super) workspace: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct GetRelatedDocumentsParams {
    /// Document to find related documents for. Required for external MCP
    /// clients; an in-app chat may omit it to use its active document.
    pub(super) path: Option<String>,
    /// Search location. Use all for the whole library; omit for the current root.
    pub(super) scope: Option<SearchScopeParam>,
    /// Corpus/index root. Omit unless using a different root is intentional.
    pub(super) root: Option<String>,
    /// Maximum related documents to return (1-25, default 8).
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
    pub(super) limit: Option<usize>,
    /// Optional saved smart collection ID to constrain returned documents.
    pub(super) collection_id: Option<String>,
    /// Workspace id to read, from list_context. Omit for the workspace that is
    /// active in Wilkes. Naming one does not switch the app to it.
    pub(super) workspace: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct LiteratureSearchParams {
    /// Scholarly works search query.
    pub(super) query: String,
    /// Id of an enabled literature provider, as reported by list_context in
    /// literature_providers. Providers shipped with Wilkes are named by their
    /// plain id (openalex, semantic_scholar); one the user defined is named
    /// custom:<id>.
    pub(super) provider: String,
    /// Maximum works to return (1-100, default 10).
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
    pub(super) limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct GetFileMetadataParams {
    /// Document to read metadata for. Required for external MCP clients; an
    /// in-app chat may omit it to use its active document.
    pub(super) path: Option<String>,
    /// Workspace id to read, from list_context. Omit for the workspace that is
    /// active in Wilkes. Naming one does not switch the app to it.
    pub(super) workspace: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct ListDocumentsParams {
    /// Corpus/library root to list. Omit for the current root.
    pub(super) root: Option<String>,
    /// List location. Use all for the whole library; omit for the current root only.
    pub(super) scope: Option<SearchScopeParam>,
    /// Maximum documents to return (1-500, default 50).
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
    pub(super) limit: Option<usize>,
    /// Workspace id to read, from list_context. Omit for the workspace that is
    /// active in Wilkes. Naming one does not switch the app to it.
    pub(super) workspace: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct GetCitationGraphParams {
    /// Graph location. Use all for the whole library; omit for the current root only.
    pub(super) scope: Option<SearchScopeParam>,
    /// Corpus/library root to read. Omit for the current root. Ignored with scope all.
    pub(super) root: Option<String>,
    /// List an outside work only when at least this many documents cite it (default 2).
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
    pub(super) min_external_citations: Option<usize>,
    /// Maximum outside works to return, most cited first (1-200, default 25).
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
    pub(super) limit: Option<usize>,
    /// Workspace id to read, from list_context. Omit for the workspace that is
    /// active in Wilkes. Naming one does not switch the app to it.
    pub(super) workspace: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct GetCitationGraphResponse {
    pub(super) scope: SearchScopeParamResponse,
    pub(super) root: Option<String>,
    pub(super) coverage: wilkes_core::types::CitationCoverage,
    /// Documents in scope with a DOI, most cited within the scope first.
    pub(super) documents: Vec<CitationGraphDocumentSummary>,
    /// `[citing, cited]` pairs of indices into `documents`.
    pub(super) edges: Vec<[usize; 2]>,
    pub(super) external: Vec<ExternalCitedWorkSummary>,
    pub(super) external_total: usize,
    pub(super) external_returned: usize,
    pub(super) without_doi: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct CitationGraphDocumentSummary {
    pub(super) doi: String,
    pub(super) title: Option<String>,
    pub(super) publication_date: Option<String>,
    pub(super) paths: Vec<String>,
    pub(super) cited_by_in_scope: usize,
    pub(super) cites_in_scope: usize,
    pub(super) references_fetched: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct ExternalCitedWorkSummary {
    #[serde(flatten)]
    pub(super) reference: wilkes_core::types::CitationReference,
    pub(super) cited_by_in_scope: usize,
    /// Set when the work is in the library, under another root.
    pub(super) library_path: Option<String>,
}

/// The most downloads one `download` call takes.
pub(super) const MAX_DOWNLOADS_PER_CALL: usize = 20;

/// How many of one call's downloads are fetched at the same time. Enough to
/// overlap the waits on several slow sites; few enough not to look like a
/// flood to one site named several times.
pub(super) const CONCURRENT_DOWNLOADS: usize = 6;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct DownloadParams {
    /// Every file to download (1-20), fetched at the same time. Put all the
    /// files the user asked for in one call rather than one call per file.
    pub(super) downloads: Vec<DownloadItem>,
    /// Workspace id to read, from list_context. Omit for the workspace that is
    /// active in Wilkes. Naming one does not switch the app to it.
    pub(super) workspace: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct DownloadItem {
    /// Direct HTTP(S) URL of the file to download, when the user supplied one.
    /// Give either url or provider with result_id, not both.
    pub(super) url: Option<String>,
    /// Literature provider id the result came from. Used with result_id.
    pub(super) provider: Option<String>,
    /// The id of a result returned by literature_search for provider. Wilkes
    /// locates the file itself; no URL is needed.
    pub(super) result_id: Option<String>,
    /// File name inside the current Wilkes root, or the folder's name when a
    /// result comes back as several files. Must not contain directories.
    pub(super) filename: Option<String>,
    /// Fetch url through the agent browser, which is signed in wherever the
    /// user signed in in Wilkes' browser, instead of trying directly first. A
    /// direct download the site refuses with HTTP 401 or 403 is retried there
    /// anyway. Only with url.
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    #[schemars(with = "Option<bool>")]
    pub(super) through_browser: Option<bool>,
}

/// What became of one download a call asked for, in the order asked.
#[derive(Debug, Serialize)]
pub(super) struct DownloadOutcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) result_id: Option<String>,
    #[serde(flatten)]
    pub(super) result: DownloadResult,
}

impl DownloadOutcome {
    /// What the caller named the file by, to report it under.
    pub(super) fn asked_for(&self) -> String {
        match (&self.url, &self.provider, &self.result_id) {
            (Some(url), _, _) => url.clone(),
            (None, Some(provider), Some(result_id)) => format!("{provider} {result_id}"),
            _ => "A download naming neither url nor a result".to_string(),
        }
    }

    pub(super) fn error(&self) -> Option<&str> {
        match &self.result {
            DownloadResult::Failed { error } => Some(error),
            DownloadResult::Acquired(_) => None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(super) enum DownloadResult {
    Acquired(AcquiredFiles),
    Failed { error: String },
}

#[derive(Debug, Serialize)]
pub(super) struct DownloadResponse {
    pub(super) downloads: Vec<DownloadOutcome>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct ReadWebPageParams {
    /// HTTP(S) address of the page.
    pub(super) url: String,
    /// Maximum characters to return (default 24000, capped at 120000).
    #[serde(default, deserialize_with = "deserialize_optional_integer")]
    #[schemars(with = "Option<usize>")]
    pub(super) max_chars: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(super) struct ReadWebPageResponse {
    pub(super) url: String,
    pub(super) title: String,
    pub(super) text: String,
    pub(super) truncated: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SaveLiteratureResultParams {
    /// Literature provider id the result came from.
    pub(super) provider: String,
    /// The id of a result returned by literature_search for provider.
    pub(super) result_id: String,
    /// Why the work is worth coming back to, in the user's words.
    pub(super) note: Option<String>,
    /// Workspace id to save into, from list_context. Omit for the workspace
    /// that is active in Wilkes. Naming one does not switch the app to it.
    pub(super) workspace: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadCustomIntegrationsParams {
    /// Also return the guide to the manifest format: what a manifest may
    /// declare and how. Read it before writing or editing one.
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    #[schemars(with = "Option<bool>")]
    pub(super) authoring_guide: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ProbeCustomIntegrationParams {
    /// The whole manifest, TOML or JSON, as it would be saved.
    pub(super) manifest: String,
    /// What to search for.
    pub(super) query: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SaveCustomIntegrationParams {
    /// The whole manifest, TOML or JSON. Its id says which integration it
    /// adds or replaces.
    pub(super) manifest: String,
    /// Whether the integration is switched on. Omit to keep what is stored;
    /// a new integration is off unless this is true.
    #[serde(default, deserialize_with = "deserialize_optional_bool")]
    #[schemars(with = "Option<bool>")]
    pub(super) enabled: Option<bool>,
}

/// What a probe found, with results shown as `literature_search` shows them.
#[derive(Debug, Serialize)]
pub(super) struct ProbeCustomIntegrationResponse {
    pub(super) ok: bool,
    pub(super) error: Option<String>,
    /// Whether it ran with the secrets the user stored for this integration.
    pub(super) stored_secrets: bool,
    pub(super) request_url: String,
    pub(super) raw_response: String,
    pub(super) issues: Vec<wilkes_core::integrations::custom::ProjectionIssue>,
    pub(super) results: Vec<LiteratureResultView>,
}

#[derive(Debug, Serialize)]
pub(super) struct LiteratureSearchResponse {
    pub(super) query: String,
    pub(super) provider: String,
    pub(super) results: Vec<LiteratureResultView>,
}

/// What the model sees of a literature result: everything but the file URL,
/// which stays on the server and is resolved when `download` names the result.
#[derive(Debug, Serialize)]
pub(super) struct LiteratureResultView {
    pub(super) id: String,
    pub(super) doi: Option<String>,
    pub(super) title: Option<String>,
    pub(super) year: Option<i64>,
    pub(super) publication_date: Option<String>,
    pub(super) venue: Option<String>,
    pub(super) citation_count: Option<i64>,
    pub(super) is_open_access: bool,
    /// Whether `download` can fetch a file for this result.
    pub(super) downloadable: bool,
    pub(super) landing_page_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) image_url: Option<String>,
    pub(super) open_access_status: Option<String>,
    pub(super) license: Option<String>,
    pub(super) authors: Option<String>,
    pub(super) publisher: Option<String>,
    pub(super) language: Option<String>,
    pub(super) file_format: Option<String>,
    pub(super) file_size: Option<String>,
    /// What the work says it is about, where the provider carries a blurb.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) subject: Option<String>,
    /// `textbook`, `course` or `reference`, for a learning catalogue's record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) kind: Option<String>,
}

impl From<&LiteratureSearchResult> for LiteratureResultView {
    fn from(result: &LiteratureSearchResult) -> Self {
        Self {
            id: result.id.clone(),
            doi: result.doi.clone(),
            title: result.title.clone(),
            year: result.year,
            publication_date: result.publication_date.clone(),
            venue: result.venue.clone(),
            citation_count: result.citation_count,
            is_open_access: result.is_open_access,
            // A course is many files assembled from the publisher's pages;
            // `download` fetches one file and cannot fetch that.
            downloadable: result.pdf_url.is_some()
                || result.acquisition == LiteratureAcquisition::Provider,
            landing_page_url: result.landing_page_url.clone(),
            image_url: result.image_url.clone(),
            open_access_status: result.open_access_status.clone(),
            license: result.license.clone(),
            authors: result.authors.clone(),
            publisher: result.publisher.clone(),
            language: result.language.clone(),
            file_format: result.file_format.clone(),
            file_size: result.file_size.clone(),
            summary: result.summary.clone(),
            subject: result.subject.clone(),
            kind: result.kind.clone(),
        }
    }
}

#[derive(
    Debug, Clone, Copy, Default, Deserialize, Serialize, schemars::JsonSchema, PartialEq, Eq,
)]
#[serde(rename_all = "snake_case")]
pub(super) enum SearchModeParam {
    #[default]
    Combined,
    Exact,
    Semantic,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum SearchScopeParam {
    All,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub(super) struct PageRange {
    pub(super) start: u32,
    pub(super) end: u32,
}

#[derive(Debug, Serialize)]
pub(super) struct ListContextResponse {
    /// The workspace the reported roots and documents belong to: the one named
    /// by the call, otherwise the workspace active in Wilkes.
    pub(super) workspace: Option<WorkspaceInfo>,
    /// Every workspace this Wilkes instance holds. Pass one of these ids as
    /// `workspace` to read that library without switching the app to it.
    pub(super) workspaces: Vec<WorkspaceInfo>,
    pub(super) current_root: Option<String>,
    pub(super) roots: Vec<String>,
    pub(super) first_files: Vec<String>,
    pub(super) active_doc: Option<ActiveDocInfo>,
    pub(super) context_files: Vec<ContextFileInfo>,
    /// Ids accepted by literature_search right now. Reported here rather than
    /// baked into that tool's description because which providers exist is a
    /// fact about the user's settings, and a user-defined one has no id until
    /// they write it.
    pub(super) literature_providers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct WorkspaceInfo {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) roots: Vec<String>,
    pub(super) current_root: Option<String>,
    /// Whether tool calls that name no workspace reach this one.
    pub(super) active: bool,
}

impl From<WorkspaceDescriptor> for WorkspaceInfo {
    fn from(workspace: WorkspaceDescriptor) -> Self {
        Self {
            id: workspace.id,
            name: workspace.name,
            roots: workspace
                .roots
                .iter()
                .map(|root| display_path(root))
                .collect(),
            current_root: workspace.active_root.as_deref().map(display_path),
            active: workspace.active,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct ActiveDocInfo {
    pub(super) path: String,
    pub(super) page: Option<u32>,
}

#[derive(Debug, Serialize)]
pub(super) struct ContextFileInfo {
    pub(super) path: String,
    pub(super) pages: Option<u32>,
}

#[derive(Debug, Serialize)]
pub(super) struct GetDocumentTextResponse {
    pub(super) path: String,
    pub(super) page: Option<u32>,
    pub(super) page_range: Option<PageRange>,
    pub(super) text: String,
    pub(super) truncated: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct GetDocumentOutlineResponse {
    pub(super) path: String,
    /// The document's declared table of contents. Empty means the document
    /// declares no outline; it is not an extraction failure.
    pub(super) outline: Vec<wilkes_core::types::OutlineEntry>,
    /// Per-document extraction decisions made while resolving the outline —
    /// the outline read's own, which runs no recognizer. Its recognition
    /// counters are zero for every document; see
    /// [`wilkes_core::types::DeclaredOutline`].
    pub(super) extraction: wilkes_core::types::ExtractionDiagnostics,
}

#[derive(Debug, Serialize)]
pub(super) struct SearchResponse {
    pub(super) query: String,
    pub(super) mode: SearchModeParam,
    pub(super) root: String,
    pub(super) file: Option<String>,
    pub(super) matches: Vec<SearchFileResponse>,
    pub(super) stats: wilkes_core::types::SearchStats,
    pub(super) truncated: bool,
    /// Whether the scan reached the end of the corpus. `false` means the
    /// search answered with what it had found so far.
    pub(super) complete: bool,
    /// What this search could not do, as distinct from what went wrong with
    /// it. A combined search whose semantic lane was unavailable found real
    /// exact matches and is not a failed call, so it says so here rather than
    /// in `stats.errors`.
    pub(super) notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct GetRelatedDocumentsResponse {
    pub(super) path: String,
    pub(super) root: String,
    pub(super) scope: SearchScopeParamResponse,
    pub(super) documents: Vec<wilkes_core::types::RelatedDocument>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum SearchScopeParamResponse {
    CurrentRoot,
    All,
}

#[derive(Debug, Serialize)]
pub(super) struct GetFileMetadataResponse {
    pub(super) path: String,
    #[serde(flatten)]
    pub(super) metadata: wilkes_core::types::DocumentMetadata,
}

#[derive(Debug, Serialize)]
pub(super) struct ListDocumentsResponse {
    pub(super) root: String,
    pub(super) scope: SearchScopeParamResponse,
    pub(super) documents: Vec<DocumentSummaryResponse>,
    pub(super) truncated: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct DocumentSummaryResponse {
    pub(super) path: String,
    pub(super) title: Option<String>,
    pub(super) author: Option<String>,
    pub(super) doi: Option<String>,
    pub(super) publication_date: Option<String>,
    pub(super) citation_count: Option<i64>,
}

impl From<wilkes_core::types::FileEntry> for DocumentSummaryResponse {
    fn from(entry: wilkes_core::types::FileEntry) -> Self {
        Self {
            path: display_path(&entry.path),
            title: entry.title,
            author: entry.author,
            doi: entry.doi,
            publication_date: entry.publication_date,
            citation_count: entry.citation_count,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct SearchFileResponse {
    pub(super) path: String,
    pub(super) file_type: wilkes_core::types::FileType,
    /// Why this document is in the result set: `exact_phrase`,
    /// `related_passage`, or both. Only the combined mode establishes more than
    /// one of them, so a single-mode search omits the field entirely — its
    /// `mode` already answers the question.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) evidence: Vec<wilkes_core::types::MatchEvidence>,
    /// Document title from cached metadata; null until the file is processed.
    pub(super) title: Option<String>,
    /// Document author from cached metadata; null until the file is processed.
    pub(super) author: Option<String>,
    /// Document DOI from cached metadata; null when absent or not yet processed.
    pub(super) doi: Option<String>,
    pub(super) matches: Vec<SearchMatchResponse>,
}

#[derive(Debug, Serialize)]
pub(super) struct SearchMatchResponse {
    pub(super) kind: SearchMatchKindResponse,
    pub(super) text: String,
    pub(super) line: Option<u32>,
    pub(super) page: Option<u32>,
    pub(super) score: Option<f32>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum SearchMatchKindResponse {
    Content,
    Filename,
    Title,
    Author,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadLibraryParams {
    pub(super) kind: crate::library::LibraryKind,
    pub(super) workspace: Option<String>,
    #[serde(default)]
    pub(super) offset: usize,
    pub(super) limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct EditLibraryParams {
    pub(super) workspace: Option<String>,
    pub(super) edit: crate::library::LibraryEdit,
}

impl ListContextResponse {
    pub(super) fn from_snapshot(
        root: crate::context::RootContext,
        active_doc: Option<ActiveDoc>,
        context_files: Vec<ContextFile>,
        roots: Vec<PathBuf>,
        workspaces: Vec<WorkspaceDescriptor>,
        requested_workspace: Option<&str>,
        literature_providers: Vec<String>,
    ) -> Self {
        let workspaces: Vec<WorkspaceInfo> =
            workspaces.into_iter().map(WorkspaceInfo::from).collect();
        // Which workspace the rest of this response describes: the named one,
        // otherwise whichever the catalog reports as active.
        let workspace = workspaces
            .iter()
            .find(|workspace| match requested_workspace {
                Some(requested) => workspace.id == requested,
                None => workspace.active,
            })
            .cloned();
        Self {
            workspace,
            workspaces,
            current_root: root.path.map(|path| path.to_string_lossy().into_owned()),
            roots: roots.into_iter().map(|path| display_path(&path)).collect(),
            first_files: root
                .first_files
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            literature_providers,
            active_doc: active_doc.map(|doc| ActiveDocInfo {
                path: doc.path,
                page: doc.page,
            }),
            context_files: context_files
                .into_iter()
                .map(|file| ContextFileInfo {
                    path: file.path,
                    pages: file.pages,
                })
                .collect(),
        }
    }
}

impl From<wilkes_core::types::FileMatches> for SearchFileResponse {
    fn from(file: wilkes_core::types::FileMatches) -> Self {
        let mut matches = file
            .field_matches
            .into_iter()
            .map(SearchMatchResponse::from)
            .collect::<Vec<_>>();
        matches.extend(file.matches.into_iter().map(SearchMatchResponse::from));
        Self {
            path: display_path(&file.path),
            file_type: file.file_type,
            evidence: file.evidence,
            title: file.title,
            author: None,
            doi: None,
            matches,
        }
    }
}

impl From<wilkes_core::types::SearchFieldMatch> for SearchMatchResponse {
    fn from(matched: wilkes_core::types::SearchFieldMatch) -> Self {
        let kind = match matched.field {
            wilkes_core::types::SearchField::Filename => SearchMatchKindResponse::Filename,
            wilkes_core::types::SearchField::Title => SearchMatchKindResponse::Title,
            wilkes_core::types::SearchField::Author => SearchMatchKindResponse::Author,
        };
        Self {
            kind,
            text: format!(
                "{}{}{}",
                matched.context_before, matched.matched_text, matched.context_after
            ),
            line: None,
            page: None,
            score: None,
        }
    }
}

impl From<wilkes_core::types::Match> for SearchMatchResponse {
    fn from(matched: wilkes_core::types::Match) -> Self {
        let (line, page) = match matched.origin {
            wilkes_core::types::SourceOrigin::TextFile { line, .. } => (Some(line), None),
            wilkes_core::types::SourceOrigin::PdfPage { page, .. } => (None, Some(page)),
        };
        let mut text = String::with_capacity(
            matched.context_before.len() + matched.matched_text.len() + matched.context_after.len(),
        );
        text.push_str(&matched.context_before);
        text.push_str(&matched.matched_text);
        text.push_str(&matched.context_after);
        Self {
            kind: SearchMatchKindResponse::Content,
            text,
            line,
            page,
            score: matched.score,
        }
    }
}
