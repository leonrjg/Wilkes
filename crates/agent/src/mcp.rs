use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use agent_client_protocol::schema::v1::{HttpHeader, McpServer, McpServerHttp};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
    Router,
};
use futures_util::{stream, StreamExt};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, ServerHandler,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use wilkes_core::acquire::{download_to_root, store_file_in_root, AcquiredFiles};
use wilkes_core::integrations::{acquire_result, IntegrationRegistry};
use wilkes_core::types::{IntegrationsSettings, LiteratureAcquisition, LiteratureSearchResult};

use crate::{
    browser::{AgentBrowser, BROWSER_UNAVAILABLE},
    context::{ActiveDoc, ContextFile},
    host::ContextStateHandle,
    reader,
    search::{SearchService, WorkspaceCatalog, WorkspaceDescriptor},
};

mod params;
mod queries;
mod runtime;
mod tools;

use self::params::*;
use self::queries::*;
pub use self::runtime::*;

const DEFAULT_TEXT_CHAR_LIMIT: usize = 24_000;
const MAX_TEXT_CHAR_LIMIT: usize = 120_000;
const DEFAULT_SEARCH_MAX_RESULTS: usize = 10;
const MAX_SEARCH_MAX_RESULTS: usize = 50;
const DEFAULT_SEARCH_CONTEXT_LINES: u32 = 2;
const MAX_SEARCH_CONTEXT_LINES: u32 = 5;
const DEFAULT_RELATED_DOCUMENTS_LIMIT: usize = 8;
const MAX_RELATED_DOCUMENTS_LIMIT: usize = 25;
const DEFAULT_LIST_DOCUMENTS_LIMIT: usize = 50;
const MAX_LIST_DOCUMENTS_LIMIT: usize = 500;
const SEMANTIC_INDEX_GUIDANCE: &str = "The user can enable the semantic index in Wilkes Settings. Use exact search with mode='exact' instead in the meantime.";
/// The combined mode has already returned whatever its exact lane found, so
/// telling the caller to fall back to exact search would be telling it to
/// repeat a search it has the results of. What it does not have is the related
/// passages, and only the user can make those available.
const COMBINED_INDEX_GUIDANCE: &str = "These results carry exact matches only; related passages need the semantic index, which the user can enable in Wilkes Settings.";
const EXTERNAL_DOCUMENT_PATH_REQUIRED: &str =
    "External Wilkes MCP does not default document tools to the active document; pass path explicitly after reading list_context.";
const WORKSPACE_DOCUMENT_PATH_REQUIRED: &str =
    "The active document belongs to the workspace open in Wilkes, not the workspace this call names; pass path explicitly.";

/// Names of the read-only tools this server exposes. Shared with the permission
/// boundary in `host.rs` so calls to Wilkes's *own* MCP server are
/// auto-allowed without ever prompting the user (they are the Q&A pane's own
/// internal plumbing). Mutating tools such as `download` must not be added here,
/// so the agent's normal permission flow remains in effect.
pub(crate) const WILKES_MCP_TOOL_NAMES: &[&str] = &[
    "list_context",
    "get_document_text",
    "get_document_outline",
    "get_related_documents",
    "get_citation_graph",
    "get_file_metadata",
    "list_documents",
    "search",
    "literature_search",
    "list_smart_collections",
    "read_library",
    "read_custom_integrations",
];

/// Shared by every `WilkesMcp` the service builds. The server is stateless, so
/// each request gets a fresh instance and anything a later call needs from an
/// earlier one has to live outside it.
type LiteratureResultCache =
    Arc<std::sync::Mutex<HashMap<(String, String), LiteratureSearchResult>>>;

/// Results held at most; past this the oldest searches are of no further use,
/// and the cache is emptied rather than grown without bound.
const LITERATURE_CACHE_CAPACITY: usize = 2000;

#[derive(Clone)]
struct WilkesMcp {
    context: McpContext,
    cwd: PathBuf,
    /// Resolves the library a call reads. Held instead of a `SearchService` so
    /// no server is pinned to the workspace that was active when it started.
    workspaces: Option<Arc<dyn WorkspaceCatalog>>,
    /// Full literature results by (provider, result id), kept so `download`
    /// can resolve a chosen result server-side. The model is shown a view
    /// without file URLs and names a result by id only.
    literature_results: LiteratureResultCache,
    /// The desktop app's browser, signed in where the user signed in; none
    /// where Wilkes runs without the desktop app.
    browser: Option<Arc<dyn AgentBrowser>>,
    tool_router: ToolRouter<Self>,
}

impl WilkesMcp {
    /// The file a literature result names, resolved here so the URL never
    /// passes through the model.
    /// The full record of a result an earlier `literature_search` returned.
    fn cached_literature_result(
        &self,
        provider: &str,
        result_id: &str,
    ) -> Result<LiteratureSearchResult, String> {
        self.literature_results
            .lock()
            .unwrap()
            .get(&(provider.to_string(), result_id.to_string()))
            .cloned()
            .ok_or_else(|| {
                format!(
                    "No result '{result_id}' from {provider} is available; run literature_search again and use an id it returns."
                )
            })
    }

    fn with_literature_results(mut self, cache: LiteratureResultCache) -> Self {
        self.literature_results = cache;
        self
    }

    fn with_browser(mut self, browser: Option<Arc<dyn AgentBrowser>>) -> Self {
        self.browser = browser;
        self
    }

    fn require_browser(&self) -> Result<&Arc<dyn AgentBrowser>, String> {
        self.browser
            .as_ref()
            .ok_or_else(|| BROWSER_UNAVAILABLE.to_string())
    }

    fn new(
        context: McpContext,
        cwd: PathBuf,
        workspaces: Option<Arc<dyn WorkspaceCatalog>>,
    ) -> Self {
        let mut tool_router = Self::tool_router();
        normalize_tool_input_schemas(&mut tool_router);
        Self {
            context,
            cwd,
            workspaces,
            literature_results: Arc::default(),
            browser: None,
            tool_router,
        }
    }
}

/// Keep Wilkes tool schemas within the broadly supported JSON Schema subset.
///
/// Schemars represents `Option<T>` as a nullable union such as
/// `"type": ["integer", "null"]`. That is valid JSON Schema 2020-12, but some
/// MCP clients discard array-valued `type` declarations before presenting a
/// tool to the model. Tool parameters are already optional through the
/// top-level `required` list, so advertising only their non-null value type is
/// both accurate for normal calls and more interoperable.
fn normalize_tool_input_schemas<S>(tool_router: &mut ToolRouter<S>) {
    for route in tool_router.map.values_mut() {
        let schema = Arc::make_mut(&mut route.attr.input_schema);
        for value in schema.values_mut() {
            normalize_optional_schema(value);
        }
    }
}

fn normalize_optional_schema(value: &mut serde_json::Value) {
    let serde_json::Value::Object(schema) = value else {
        if let serde_json::Value::Array(values) = value {
            for value in values {
                normalize_optional_schema(value);
            }
        }
        return;
    };

    let collapsed_type = match schema.get_mut("type") {
        Some(serde_json::Value::Array(types)) => {
            let non_null_count = types.iter().filter(|value| *value != "null").count();
            if non_null_count > 0 && non_null_count < types.len() {
                types.retain(|value| value != "null");
            }
            (types.len() == 1).then(|| types[0].clone())
        }
        _ => None,
    };
    if let Some(value_type) = collapsed_type {
        schema.insert("type".to_string(), value_type);
    }

    if let Some(serde_json::Value::Array(values)) = schema.get_mut("enum") {
        let has_non_null = values.iter().any(|value| !value.is_null());
        if has_non_null {
            values.retain(|value| !value.is_null());
        }
    }

    for keyword in ["anyOf", "oneOf"] {
        let single_branch = match schema.get_mut(keyword) {
            Some(serde_json::Value::Array(branches)) => {
                branches.retain(|branch| !is_null_schema(branch));
                for branch in branches.iter_mut() {
                    normalize_optional_schema(branch);
                }
                (branches.len() == 1).then(|| branches[0].clone())
            }
            _ => None,
        };

        if let Some(serde_json::Value::Object(branch)) = single_branch {
            if branch.keys().all(|key| !schema.contains_key(key)) {
                schema.remove(keyword);
                schema.extend(branch);
            }
        }
    }

    for nested in schema.values_mut() {
        normalize_optional_schema(nested);
    }
}

fn is_null_schema(value: &serde_json::Value) -> bool {
    let Some(schema) = value.as_object() else {
        return false;
    };
    if schema.get("const").is_some_and(serde_json::Value::is_null) {
        return true;
    }
    match schema.get("type") {
        Some(serde_json::Value::String(value)) => value == "null",
        Some(serde_json::Value::Array(values)) => {
            !values.is_empty() && values.iter().all(|value| value == "null")
        }
        _ => false,
    }
}

impl std::fmt::Debug for WilkesMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WilkesMcp")
            .field("context", &self.context)
            .field("cwd", &self.cwd)
            .field(
                "workspaces",
                &self.workspaces.as_ref().map(|_| "WorkspaceCatalog"),
            )
            .finish_non_exhaustive()
    }
}

/// One tool call's resolved workspace: which library it reads and the service
/// that reads it.
///
/// Resolved once at the tool boundary and passed down, so the workspace a call
/// names governs every step of it — roots, access boundary and index — and an
/// unknown id fails once, up front, instead of degrading into an empty answer
/// somewhere further in.
struct WorkspaceScope {
    named: Option<String>,
    search: Option<Arc<dyn SearchService>>,
}

impl WorkspaceScope {
    fn named(&self) -> Option<&str> {
        self.named.as_deref()
    }

    fn search(&self) -> Option<Arc<dyn SearchService>> {
        self.search.clone()
    }

    fn require_search(&self, unavailable: &str) -> Result<Arc<dyn SearchService>, String> {
        self.search.clone().ok_or_else(|| unavailable.to_string())
    }

    /// Whether the resolved workspace refuses writes. A scope with no service
    /// reads as writable; the tool that asks fails on the missing service
    /// first, and answering "read-only" for an absent workspace would report
    /// the wrong reason.
    fn is_read_only(&self) -> bool {
        self.search
            .as_ref()
            .is_some_and(|search| search.is_read_only())
    }
}

impl WilkesMcp {
    /// Resolve the workspace a call names, or the active one when it names
    /// none. Naming a workspace opens its context alongside the active one; it
    /// never activates it.
    async fn scope(&self, workspace: Option<&str>) -> Result<WorkspaceScope, String> {
        let named = workspace
            .map(str::trim)
            .filter(|workspace| !workspace.is_empty())
            .map(str::to_string);
        let Some(catalog) = self.workspaces.as_ref() else {
            // A server without a catalog can still answer for whatever context
            // it was given, but it cannot honour a named workspace.
            if let Some(named) = named {
                return Err(format!(
                    "Workspace {named} cannot be resolved: this session has no Wilkes workspace access."
                ));
            }
            return Ok(WorkspaceScope {
                named: None,
                search: None,
            });
        };
        let search = catalog.search_for(named.as_deref()).await?;
        Ok(WorkspaceScope {
            named,
            search: Some(search),
        })
    }

    async fn workspace_descriptors(&self) -> Result<Vec<WorkspaceDescriptor>, String> {
        match &self.workspaces {
            Some(catalog) => catalog.workspaces().await,
            None => Ok(Vec::new()),
        }
    }

    async fn context_snapshot(&self, scope: &WorkspaceScope) -> crate::host::ContextSnapshot {
        match &self.context {
            // A named workspace overrides the chat's own root: the caller asked
            // about that library, not the one this conversation opened in.
            McpContext::Session(context) if scope.named().is_none() => context.snapshot(),
            McpContext::Session(context) => {
                let snapshot = context.snapshot();
                let root = match scope.search() {
                    Some(search) => search.default_root().await,
                    None => None,
                };
                crate::host::ContextSnapshot {
                    root: crate::context::root_context(root.as_deref()),
                    ..snapshot
                }
            }
            McpContext::Library(context) => {
                let root = match scope.search() {
                    Some(search) => search.default_root().await,
                    None => None,
                };
                crate::host::ContextSnapshot {
                    active_doc: context.active_document(),
                    context_files: Vec::new(),
                    root: crate::context::root_context(root.as_deref()),
                }
            }
        }
    }

    async fn library_roots(&self, scope: &WorkspaceScope) -> Vec<PathBuf> {
        match scope.search() {
            Some(search) => search.library_roots().await,
            None => Vec::new(),
        }
    }

    async fn is_path_allowed(&self, path: &Path, scope: &WorkspaceScope) -> bool {
        let within_roots = is_within_roots(path, &self.library_roots(scope).await);
        match &self.context {
            // A chat's own context files belong to the workspace it opened in,
            // so a call that names another workspace is admitted only by that
            // workspace's roots.
            McpContext::Session(context) if scope.named().is_none() => {
                context.is_allowed(path) || within_roots
            }
            _ => within_roots,
        }
    }

    async fn current_root(&self, scope: &WorkspaceScope) -> Result<PathBuf, String> {
        match &self.context {
            McpContext::Session(context) if scope.named().is_none() => {
                if let Some(root) = context.search_root() {
                    return Ok(root);
                }
                if let Some(search) = scope.search() {
                    if let Some(root) = search.default_root().await {
                        return Ok(root);
                    }
                }
                Ok(self.cwd.clone())
            }
            _ => {
                let search = scope.require_search("Wilkes search is not available.")?;
                search.default_root().await.ok_or_else(|| match scope.named() {
                    Some(workspace) => format!(
                        "Workspace {workspace} has no current library root. Open a directory in it, or pass root explicitly."
                    ),
                    None => "No current Wilkes library root is configured. Open a directory in Wilkes first."
                        .to_string(),
                })
            }
        }
    }

    /// The provider ids a caller may name right now.
    ///
    /// Takes a scope the caller has already resolved rather than resolving its
    /// own: `list_context` holds one, and resolving a second time would send a
    /// redundant lookup through the workspace catalog on every call.
    async fn literature_provider_ids(&self, scope: &WorkspaceScope) -> Vec<String> {
        let settings = match scope.search() {
            Some(search) => search.integrations().await,
            None => IntegrationsSettings::default(),
        };
        let mirror = scope.search().and_then(|search| search.catalogue_mirror());
        IntegrationRegistry::from_settings(&settings, mirror.as_deref())
            .enabled_literature_ids()
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// Every provider a call may name: the remote ones as settings have them,
    /// and the catalogues the installation's mirror holds. The mirror is found
    /// through the active workspace's service because it is the installation's
    /// and every workspace names the same one.
    async fn literature_registry(&self) -> IntegrationRegistry {
        IntegrationRegistry::from_settings(
            &self.integrations().await,
            self.literature_mirror().await.as_deref(),
        )
    }

    /// The installation's catalogue mirror, found through the active
    /// workspace's service because every workspace names the same one.
    async fn literature_mirror(&self) -> Option<PathBuf> {
        match self.scope(None).await {
            Ok(scope) => scope.search().and_then(|search| search.catalogue_mirror()),
            Err(error) => {
                info!(%error, "mcp: catalogue mirror unavailable");
                None
            }
        }
    }

    /// Literature and provider settings are global rather than per-workspace,
    /// so this deliberately resolves through the active workspace only.
    async fn integrations(&self) -> IntegrationsSettings {
        let search = match self.scope(None).await {
            Ok(scope) => scope.search(),
            Err(error) => {
                info!(%error, "mcp: integrations settings unavailable");
                None
            }
        };
        match search {
            Some(search) => search.integrations().await,
            None => IntegrationsSettings::default(),
        }
    }
}

fn is_within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    let Ok(canonical) = path.canonicalize() else {
        return false;
    };
    roots.iter().any(|root| {
        root.canonicalize()
            .map(|root| canonical.starts_with(root))
            .unwrap_or(false)
    })
}

/// Name the library the path was actually checked against. A caller that named
/// a workspace has to be able to tell "wrong workspace" from "outside every
/// root", and the two produce the same refusal otherwise.
fn mcp_access_error(path: &Path, scope: &WorkspaceScope) -> String {
    match scope.named() {
        Some(workspace) => format!(
            "{} is not in a configured library root of workspace {workspace}. Pass a different workspace, or open its containing directory as a root of that workspace.",
            path.display()
        ),
        None => format!(
            "{} is not in a configured Wilkes library root or this chat's context. Open its containing directory as a Wilkes root or add the file to the chat context.",
            path.display()
        ),
    }
}

fn no_roots_error(scope: &WorkspaceScope) -> String {
    match scope.named() {
        Some(workspace) => format!("Workspace {workspace} has no configured library roots."),
        None => "No Wilkes library roots are configured.".to_string(),
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WilkesMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("wilkes", env!("CARGO_PKG_VERSION"))
                    .with_title("Wilkes")
                    .with_description("Document search, literature and research-library tools"),
            )
            .with_instructions("Wilkes document context and literature tools. Context and search tools are read-only. The download, save_literature_result and edit_library tools write real library data, and save_custom_integration writes the user's integration settings; all must only be used for user-requested changes. Managed read_only workspaces refuse writes.")
    }
}

#[cfg(test)]
mod tests;
