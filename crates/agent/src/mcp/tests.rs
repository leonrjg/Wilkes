use super::*;
use crate::search::{CollectedSearch, SearchService};
use async_trait::async_trait;
use std::sync::Mutex;
use tempfile::tempdir;
use wilkes_core::types::{
    DocumentMetadata, FileEntry, FileListResponse, FileMatches, FileType, Match, RelatedDocument,
    RelatedDocumentsQuery, SearchField, SearchFieldMatch, SearchMode, SearchQuery, SearchScope,
    SearchStats, SourceOrigin,
};

mod documents;
mod downloads;
mod library;
mod runtime;
mod schema;
mod search;
mod workspaces;

struct FakeSearch {
    read_only: bool,
    last_query: Mutex<Option<SearchQuery>>,
    last_related_query: Mutex<Option<RelatedDocumentsQuery>>,
    default_root: Option<PathBuf>,
    library_roots: Vec<PathBuf>,
    response: Mutex<Option<CollectedSearch>>,
    related_response: Mutex<Option<Vec<RelatedDocument>>>,
    documents: Mutex<Option<FileListResponse>>,
    metadata: Mutex<Option<DocumentMetadata>>,
    /// (provider, result id, note) of every result saved.
    saved: Mutex<Vec<(String, String, Option<String>)>>,
    /// (manifest, enabled) of every custom integration saved.
    saved_manifests: Mutex<Vec<(String, Option<bool>)>>,
    /// What the service reports as the integration settings right now.
    integrations: Mutex<IntegrationsSettings>,
}

fn assert_schema_has_no_null_union(value: &serde_json::Value, path: &str) {
    match value {
        serde_json::Value::Object(schema) => {
            if let Some(serde_json::Value::Array(types)) = schema.get("type") {
                assert!(
                    !types.iter().any(|value| value == "null"),
                    "nullable type remained in {path}: {value}"
                );
            }
            if let Some(serde_json::Value::Array(values)) = schema.get("enum") {
                assert!(
                    !values.iter().any(serde_json::Value::is_null),
                    "nullable enum remained in {path}: {value}"
                );
            }
            for keyword in ["anyOf", "oneOf"] {
                if let Some(serde_json::Value::Array(branches)) = schema.get(keyword) {
                    assert!(
                        !branches.iter().any(is_null_schema),
                        "nullable {keyword} branch remained in {path}: {value}"
                    );
                }
            }
            for (key, nested) in schema {
                assert_schema_has_no_null_union(nested, &format!("{path}.{key}"));
            }
        }
        serde_json::Value::Array(values) => {
            for (index, nested) in values.iter().enumerate() {
                assert_schema_has_no_null_union(nested, &format!("{path}[{index}]"));
            }
        }
        _ => {}
    }
}

#[async_trait]
impl SearchService for FakeSearch {
    fn is_read_only(&self) -> bool {
        self.read_only
    }
    async fn edit_library(
        self: Arc<Self>,
        edit: crate::library::LibraryEdit,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::to_value(edit).unwrap())
    }
    async fn read_library(
        self: Arc<Self>,
        _kind: crate::library::LibraryKind,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([{"id":"one"},{"id":"two"}]))
    }
    async fn save_literature_result(
        self: Arc<Self>,
        provider: String,
        work: LiteratureSearchResult,
        note: Option<String>,
    ) -> Result<serde_json::Value, String> {
        self.saved
            .lock()
            .unwrap()
            .push((provider.clone(), work.id.clone(), note));
        Ok(serde_json::json!({"provider": provider, "work": work}))
    }
    async fn integrations(self: Arc<Self>) -> IntegrationsSettings {
        self.integrations.lock().unwrap().clone()
    }
    async fn custom_integrations(self: Arc<Self>) -> Result<serde_json::Value, String> {
        let saved = self.saved_manifests.lock().unwrap();
        Ok(serde_json::json!(saved
            .iter()
            .map(|_| serde_json::json!({ "provider": "custom:fake" }))
            .collect::<Vec<_>>()))
    }
    async fn save_custom_integration(
        self: Arc<Self>,
        manifest: String,
        enabled: Option<bool>,
    ) -> Result<serde_json::Value, String> {
        self.saved_manifests
            .lock()
            .unwrap()
            .push((manifest, enabled));
        Ok(serde_json::json!({"provider": "custom:fake", "added": true}))
    }

    async fn default_root(self: Arc<Self>) -> Option<PathBuf> {
        self.default_root.clone()
    }

    async fn library_roots(self: Arc<Self>) -> Vec<PathBuf> {
        if self.library_roots.is_empty() {
            self.default_root.clone().into_iter().collect()
        } else {
            self.library_roots.clone()
        }
    }

    async fn max_download_bytes(self: Arc<Self>) -> u64 {
        wilkes_core::types::DEFAULT_MAX_DOWNLOAD_BYTES
    }

    async fn max_search_file_size(self: Arc<Self>) -> u64 {
        23 * 1024 * 1024
    }

    async fn search(
        self: Arc<Self>,
        query: SearchQuery,
        _max_files: usize,
    ) -> Result<CollectedSearch, String> {
        *self.last_query.lock().unwrap() = Some(query);
        Ok(self.response.lock().unwrap().take().unwrap())
    }

    async fn related_documents(
        self: Arc<Self>,
        query: RelatedDocumentsQuery,
    ) -> Result<Vec<RelatedDocument>, String> {
        *self.last_related_query.lock().unwrap() = Some(query);
        Ok(self.related_response.lock().unwrap().take().unwrap())
    }

    async fn list_documents(self: Arc<Self>, _root: PathBuf) -> Result<FileListResponse, String> {
        Ok(self
            .documents
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(FileListResponse {
                files: Vec::new(),
                omitted: Vec::new(),
                directories: Vec::new(),
            }))
    }

    async fn document_metadata(
        self: Arc<Self>,
        _path: PathBuf,
    ) -> Result<DocumentMetadata, String> {
        self.metadata
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "no metadata".to_string())
    }
}

/// Test catalog: several named workspaces, each with its own service, and a
/// record of which id every call resolved through.
struct FakeWorkspaces {
    active: String,
    workspaces: Vec<(String, Arc<FakeSearch>)>,
    resolved: Mutex<Vec<Option<String>>>,
}

impl FakeWorkspaces {
    fn new(active: &str, workspaces: Vec<(&str, Arc<FakeSearch>)>) -> Self {
        Self {
            active: active.to_string(),
            workspaces: workspaces
                .into_iter()
                .map(|(id, search)| (id.to_string(), search))
                .collect(),
            resolved: Mutex::new(Vec::new()),
        }
    }

    fn resolutions(&self) -> Vec<Option<String>> {
        self.resolved.lock().unwrap().clone()
    }
}

#[async_trait]
impl WorkspaceCatalog for FakeWorkspaces {
    async fn workspaces(&self) -> Result<Vec<WorkspaceDescriptor>, String> {
        Ok(self
            .workspaces
            .iter()
            .map(|(id, search)| WorkspaceDescriptor {
                id: id.clone(),
                name: format!("Workspace {id}"),
                roots: search.library_roots.clone(),
                active_root: search.default_root.clone(),
                active: *id == self.active,
                read_only: false,
            })
            .collect())
    }

    async fn search_for(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Arc<dyn SearchService>, String> {
        self.resolved
            .lock()
            .unwrap()
            .push(workspace_id.map(str::to_string));
        let id = workspace_id.unwrap_or(&self.active);
        self.workspaces
            .iter()
            .find(|(candidate, _)| candidate == id)
            .map(|(_, search)| Arc::clone(search) as Arc<dyn SearchService>)
            .ok_or_else(|| format!("Workspace {id} is not available: Unknown workspace"))
    }
}

/// A browser that serves one file and one page, as the desktop app's
/// would for a site that let it in.
struct FakeBrowser {
    staging: PathBuf,
}

#[async_trait::async_trait]
impl AgentBrowser for FakeBrowser {
    async fn download(&self, url: &str) -> Result<crate::browser::StagedDownload, String> {
        if !url.ends_with("/paper.pdf") {
            return Err("The page asks you to sign in".to_string());
        }
        let dir = self.staging.join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("download");
        std::fs::write(&file, b"%PDF-1.7 signed in").unwrap();
        Ok(crate::browser::StagedDownload::new(
            dir,
            file,
            "Signed In - Anna's Archive.pdf".to_string(),
        ))
    }

    async fn read(&self, url: &str) -> Result<crate::browser::BrowserPage, String> {
        Ok(crate::browser::BrowserPage {
            url: url.to_string(),
            title: "Full text".to_string(),
            text: "é".repeat(10),
        })
    }
}

fn mcp_with_browser(root: &Path, staging: &Path) -> WilkesMcp {
    WilkesMcp::new(
        McpContext::Library(ExternalMcpContext::default()),
        PathBuf::new(),
        Some(single_workspace(fake_search_with_root(root.to_path_buf()))),
    )
    .with_browser(Some(Arc::new(FakeBrowser {
        staging: staging.to_path_buf(),
    })))
}

fn browser_item(url: &str) -> DownloadItem {
    DownloadItem {
        url: Some(url.to_string()),
        provider: None,
        result_id: None,
        filename: None,
        through_browser: Some(true),
    }
}

fn batch(downloads: Vec<DownloadItem>) -> DownloadParams {
    DownloadParams {
        downloads,
        workspace: None,
    }
}

fn browser_download(url: &str) -> DownloadParams {
    batch(vec![browser_item(url)])
}

fn direct_item(url: String) -> DownloadItem {
    DownloadItem {
        url: Some(url),
        provider: None,
        result_id: None,
        filename: None,
        through_browser: None,
    }
}

fn direct_download(url: String) -> DownloadParams {
    batch(vec![direct_item(url)])
}

fn single_workspace(search: Arc<FakeSearch>) -> Arc<FakeWorkspaces> {
    Arc::new(FakeWorkspaces::new("active", vec![("active", search)]))
}

fn fake_search_with_root(root: PathBuf) -> Arc<FakeSearch> {
    Arc::new(FakeSearch {
        saved: Mutex::default(),
        saved_manifests: Mutex::default(),
        integrations: Mutex::default(),
        read_only: false,
        last_query: Mutex::new(None),
        last_related_query: Mutex::new(None),
        default_root: Some(root.clone()),
        library_roots: vec![root],
        response: Mutex::new(None),
        related_response: Mutex::new(None),
        documents: Mutex::new(None),
        metadata: Mutex::new(None),
    })
}

fn two_workspaces(
    active_root: PathBuf,
    other_root: PathBuf,
) -> (Arc<FakeWorkspaces>, Arc<FakeSearch>) {
    let other = fake_search_with_root(other_root);
    let catalog = Arc::new(FakeWorkspaces::new(
        "active-id",
        vec![
            ("active-id", fake_search_with_root(active_root)),
            ("other-id", Arc::clone(&other)),
        ],
    ));
    (catalog, other)
}

fn collected(stats: SearchStats, complete: bool) -> CollectedSearch {
    CollectedSearch {
        files: Vec::new(),
        stats,
        truncated: false,
        complete,
    }
}

async fn combined_search_response(collected: CollectedSearch) -> SearchResponse {
    let dir = tempdir().unwrap();
    let service = Arc::new(FakeSearch {
        saved: Mutex::default(),
        saved_manifests: Mutex::default(),
        integrations: Mutex::default(),
        read_only: false,
        last_query: Mutex::new(None),
        last_related_query: Mutex::new(None),
        default_root: Some(dir.path().to_path_buf()),
        library_roots: Vec::new(),
        response: Mutex::new(Some(collected)),
        related_response: Mutex::new(None),
        documents: Mutex::new(None),
        metadata: Mutex::new(None),
    });
    search_documents(
        &ContextStateHandle::default(),
        Some(service),
        dir.path(),
        SearchParams {
            query: "photocatalysis".to_string(),
            mode: SearchModeParam::Combined,
            scope: None,
            root: None,
            file: None,
            max_results: None,
            case_sensitive: None,
            is_regex: None,
            context_lines: None,
            collection_id: None,
            workspace: None,
        },
    )
    .await
    .unwrap()
}

fn doc_entry(path: PathBuf, title: &str, doi: Option<&str>) -> FileEntry {
    FileEntry {
        path,
        size_bytes: 1,
        file_type: FileType::Paged,
        extension: "pdf".into(),
        created_at_ms: None,
        modified_at_ms: None,
        title: Some(title.into()),
        author: Some("Author".into()),
        doi: doi.map(Into::into),
        publication_date: None,
        citation_count: None,
        metadata_conflicts: Default::default(),
        tags: Vec::new(),
    }
}
