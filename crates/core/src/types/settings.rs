//! Application settings, the local services they configure, and display
//! preferences.

use super::*;

// ── Settings ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalMcpSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub require_token: bool,
    #[serde(default = "default_external_mcp_bind_address")]
    pub bind_address: std::net::IpAddr,
    #[serde(default = "default_external_mcp_port")]
    pub port: u16,
}

pub(super) fn default_external_mcp_bind_address() -> std::net::IpAddr {
    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

pub(super) fn default_external_mcp_port() -> u16 {
    39_217
}

impl Default for ExternalMcpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            require_token: false,
            bind_address: default_external_mcp_bind_address(),
            port: default_external_mcp_port(),
        }
    }
}

/// The Wilkes HTTP API, served by the desktop app over the workspace it
/// already has open.
///
/// It exists so that another program on this machine does not have to open the
/// workspace itself to read from it. A Wilkes workspace has one owner — the
/// process holding its databases — and a second opener races it for
/// `settings.json` and the semantic index. With this on, the app *is* the
/// owner and everything else asks it over HTTP.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpApiSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Ask every request for a bearer token, stored apart from these
    /// settings. Off by default, as the external MCP server's is: the API is
    /// unauthenticated unless this is turned on.
    #[serde(default)]
    pub require_token: bool,
    #[serde(default = "default_http_api_bind_address")]
    pub bind_address: std::net::IpAddr,
    #[serde(default = "default_http_api_port")]
    pub port: u16,
}

pub(super) fn default_http_api_bind_address() -> std::net::IpAddr {
    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

/// Not `wilkes-server`'s 2000: the two are different owners of the same
/// workspace, and a shared default would have them fight for the port on the
/// one machine where both might be started.
pub(super) fn default_http_api_port() -> u16 {
    2020
}

impl Default for HttpApiSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            require_token: false,
            bind_address: default_http_api_bind_address(),
            port: default_http_api_port(),
        }
    }
}

/// Sending files to, and receiving them from, other Wilkes instances on the
/// local network. Off by default: while on, this instance announces itself
/// over mDNS and accepts files from any instance that finds it, writing them
/// into the current root.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanSharingSettings {
    #[serde(default)]
    pub enabled: bool,
}

/// Where this instance's embeddings are computed.
///
/// With this on, every embedding this installation needs — an index build, a
/// search query, a managed corpus — is computed by the Wilkes instance at
/// `address`, over its HTTP API, with the model this instance selected. The
/// library, the index and extraction stay here; only text and vectors cross.
///
/// The host's bearer token is stored apart from these settings, for the
/// reason [`crate::token_file`] gives. There is no fallback to this machine:
/// with the host unreachable, embedding is unavailable until the setting is
/// turned off.
///
/// Installation-wide rather than per workspace, because it describes the
/// machine, not a library.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteEmbeddingSettings {
    #[serde(default)]
    pub enabled: bool,
    /// The host's HTTP API: a base URL such as `http://mini.local:2020`.
    #[serde(default)]
    pub address: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, alias = "bookmarked_dirs")]
    pub favorites: Vec<PathBuf>,
    #[serde(default)]
    pub recent_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub last_directory: Option<PathBuf>,
    pub respect_gitignore: bool,
    pub max_file_size: u64,
    /// Maximum bytes accepted for a single library download.
    #[serde(
        default = "default_max_download_bytes",
        deserialize_with = "deserialize_max_download_bytes"
    )]
    pub max_download_bytes: u64,
    pub context_lines: usize,
    pub theme: Theme,
    /// Inversion percentage applied to document canvases in dark mode.
    #[serde(
        default = "default_dark_mode_inversion",
        deserialize_with = "deserialize_dark_mode_inversion"
    )]
    pub dark_mode_inversion: u8,
    /// How document pages are darkened in dark mode: inverted, or dimmed.
    #[serde(default)]
    pub dark_mode_reading: DarkModeReading,
    /// How much of a page's brightness dimming takes away, as a percentage,
    /// when pages are dimmed rather than inverted.
    #[serde(
        default = "default_dark_mode_dimming",
        deserialize_with = "deserialize_dark_mode_dimming"
    )]
    pub dark_mode_dimming: u8,
    #[serde(default)]
    pub search_prefer_semantic: bool,
    /// When enabled, Wilkes keeps the text of every paged document in the
    /// library in the document text index (`wilkes_core::text_index`), whether
    /// or not it is embedded, and exact (grep) search reads it from there
    /// instead of re-extracting the file, falling back to live extraction only
    /// for files the store does not yet hold. On by default: re-extracting a
    /// document on every search is the same text at a much higher price.
    #[serde(default = "default_grep_use_index")]
    pub grep_use_index: bool,
    #[serde(default)]
    pub semantic: SemanticSettings,
    #[serde(default)]
    pub integrations: IntegrationsSettings,
    #[serde(default)]
    pub primary_metadata_source: MetadataSourcePreference,
    /// The *text* files a corpus contains. Documents are not in here and
    /// cannot be: [`FileType::detect`] admits every format the MuPDF backend
    /// reads whatever this list says, so a document extension stored in it
    /// would be a value that decides nothing while reading as though it did.
    /// `deserialize_supported_extensions` drops them on the way in, including
    /// from the settings files of installs written before this was true.
    #[serde(
        default = "default_supported_extensions",
        deserialize_with = "deserialize_supported_extensions"
    )]
    pub supported_extensions: Vec<String>,
    #[serde(default)]
    pub max_results: usize,
    /// Hours between feed refreshes, for every feed. Feeds also refresh when
    /// Wilkes starts, if the last refresh is older than this.
    #[serde(default = "default_feeds_refresh_hours")]
    pub feeds_refresh_hours: u64,
    #[serde(default)]
    pub bookmarks_dock: BookmarkDock,
    #[serde(default)]
    pub file_sort_key: FileSortKey,
    #[serde(default)]
    pub file_sort_direction: FileSortDirection,
    #[serde(default = "default_file_display_fields")]
    pub file_display_fields: Vec<FileDisplayField>,
    /// Present the recursive library listing as collapsible folders. This is
    /// deliberately a view preference: the catalogue and search boundary
    /// continue to identify documents by their canonical filesystem paths.
    #[serde(default)]
    pub file_tree_enabled: bool,
    /// Desired CSS-pixel height for body text when a PDF is auto-zoomed.
    #[serde(default = "default_pdf_auto_zoom_target_px")]
    pub pdf_auto_zoom_target_px: f64,
    /// Open web links in the desktop app's docked browser pane rather than the
    /// system browser. The web build has no pane and ignores it.
    #[serde(default = "default_open_links_in_app")]
    pub open_links_in_app: bool,
    /// Show the line-number gutter in the document editor.
    #[serde(default = "default_editor_line_numbers")]
    pub editor_line_numbers: bool,
    /// Preferred agent backend for the "Ask the documents" chat pane. The
    /// in-pane selector and header dropdown may switch a session to a
    /// different backend transiently; this field is the single persisted
    /// default, and Settings is its only writer.
    #[serde(default, deserialize_with = "deserialize_chat_backend_setting")]
    pub chat_backend: AgentBackend,
    /// Per-backend chat config defaults applied to newly started sessions.
    /// Written whenever the user changes a config option in the chat pane, so
    /// each backend restores its last model/thought level/mode on a new chat.
    /// Distinct from a conversation's own `config_values` snapshot, which
    /// restores *that* conversation on reopen.
    #[serde(default)]
    pub chat_config: Vec<ChatBackendConfig>,
    /// User-authored instructions prepended to every chat turn. Kept in the
    /// global settings (rather than a conversation record) so an edit applies
    /// consistently to new and existing conversations.
    #[serde(default)]
    pub chat_custom_instructions: String,
    /// Optional MCP endpoint for regular Claude Code and Codex clients.
    /// Authentication material, when enabled, is stored separately in app data
    /// and is intentionally never serialized as part of Settings.
    #[serde(default)]
    pub external_mcp: ExternalMcpSettings,
    /// The read/write HTTP API, off by default. Unauthenticated like
    /// `wilkes-server` itself, which is why it binds loopback by default,
    /// unless `require_token` asks for a bearer token.
    #[serde(default)]
    pub http_api: HttpApiSettings,
    /// Sending files to other Wilkes instances on the local network.
    #[serde(default)]
    pub lan_sharing: LanSharingSettings,
    /// Running embeddings on another Wilkes instance instead of this one.
    #[serde(default)]
    pub remote_embedding: RemoteEmbeddingSettings,
    /// Local text generation. Off by default; every affordance that depends on
    /// it is invisible until it is both enabled and ready.
    #[serde(default)]
    pub generation: GenerationSettings,
    /// Query-vector enhancement for semantic search (HyDE, pseudo-relevance
    /// feedback). Off by default.
    #[serde(default)]
    pub retrieval: RetrievalSettings,
    /// Transcription and description of the pictures inside documents. Off by
    /// default.
    #[serde(default)]
    pub image_analysis: ImageAnalysisSettings,
}

pub const DEFAULT_MAX_DOWNLOAD_BYTES: u64 = 1_000_000_000;

pub(super) fn default_max_download_bytes() -> u64 {
    DEFAULT_MAX_DOWNLOAD_BYTES
}

pub(super) fn deserialize_max_download_bytes<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u64::deserialize(deserializer)?;
    if value == 0 {
        return Err(de::Error::custom(
            "maximum download size must be greater than zero",
        ));
    }
    Ok(value)
}

pub(super) fn default_feeds_refresh_hours() -> u64 {
    6
}

pub(super) fn default_max_file_size() -> u64 {
    50 * 1024 * 1024
}

pub(super) fn default_grep_use_index() -> bool {
    true
}

pub(super) fn default_file_display_fields() -> Vec<FileDisplayField> {
    vec![FileDisplayField::Size]
}

pub(super) fn default_dark_mode_inversion() -> u8 {
    90
}

pub(super) fn deserialize_dark_mode_inversion<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u8::deserialize(deserializer)?;
    if value > 100 {
        return Err(de::Error::custom(
            "dark-mode inversion must be between 0 and 100",
        ));
    }
    Ok(value)
}

pub(super) fn default_dark_mode_dimming() -> u8 {
    50
}

pub(super) fn deserialize_dark_mode_dimming<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u8::deserialize(deserializer)?;
    if value > 100 {
        return Err(de::Error::custom(
            "dark-mode dimming must be between 0 and 100",
        ));
    }
    Ok(value)
}

pub(super) fn default_pdf_auto_zoom_target_px() -> f64 {
    15.5
}

pub(super) fn default_open_links_in_app() -> bool {
    true
}

pub(super) fn default_editor_line_numbers() -> bool {
    true
}

/// The text files a fresh install reads. The documents — PDF, EPUB, MOBI,
/// KF8, FB2, comic archives — are deliberately absent: they are admitted by
/// [`FileType::detect`] itself and are never a setting.
pub(super) fn default_supported_extensions() -> Vec<String> {
    [
        "txt", "md", "json", "xml", "html", "htm", "log", "csv", "jsonl",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Read the extension list, less any document extension a stored settings file
/// still names.
///
/// Every install predating this carries the whole document list in its
/// `supported_extensions`, and so would every settings file the interface
/// wrote back afterwards. Dropping them here rather than only in the interface
/// means the list one consumer reads is the list every consumer reads, and the
/// panel that edits it has nothing to hide.
pub(super) fn deserialize_supported_extensions<'de, D>(
    deserializer: D,
) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let stored = Vec::<String>::deserialize(deserializer)?;
    Ok(without_document_extensions(stored))
}

/// The text extensions of a list that may also name documents.
pub fn without_document_extensions(extensions: Vec<String>) -> Vec<String> {
    use crate::extract::document::format::PagedFormat;
    extensions
        .into_iter()
        .filter(|extension| {
            !PagedFormat::EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
        .collect()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            favorites: Vec::new(),
            recent_dirs: Vec::new(),
            last_directory: None,
            respect_gitignore: true,
            max_file_size: default_max_file_size(),
            max_download_bytes: default_max_download_bytes(),
            context_lines: 2,
            theme: Theme::default(),
            dark_mode_inversion: default_dark_mode_inversion(),
            dark_mode_reading: DarkModeReading::default(),
            dark_mode_dimming: default_dark_mode_dimming(),
            search_prefer_semantic: false,
            grep_use_index: default_grep_use_index(),
            semantic: SemanticSettings::default(),
            integrations: IntegrationsSettings::default(),
            primary_metadata_source: MetadataSourcePreference::default(),
            supported_extensions: default_supported_extensions(),
            max_results: 50,
            feeds_refresh_hours: default_feeds_refresh_hours(),
            bookmarks_dock: BookmarkDock::default(),
            file_sort_key: FileSortKey::default(),
            file_sort_direction: FileSortDirection::default(),
            file_display_fields: default_file_display_fields(),
            file_tree_enabled: false,
            pdf_auto_zoom_target_px: default_pdf_auto_zoom_target_px(),
            open_links_in_app: default_open_links_in_app(),
            editor_line_numbers: default_editor_line_numbers(),
            chat_backend: AgentBackend::default(),
            chat_config: Vec::new(),
            chat_custom_instructions: String::new(),
            external_mcp: ExternalMcpSettings::default(),
            http_api: HttpApiSettings::default(),
            lan_sharing: LanSharingSettings::default(),
            remote_embedding: RemoteEmbeddingSettings::default(),
            generation: GenerationSettings::default(),
            retrieval: RetrievalSettings::default(),
            image_analysis: ImageAnalysisSettings::default(),
        }
    }
}

pub(super) fn deserialize_chat_backend_setting<'de, D>(
    deserializer: D,
) -> Result<AgentBackend, D::Error>
where
    D: Deserializer<'de>,
{
    match String::deserialize(deserializer)?.as_str() {
        "ClaudeCode" => Ok(AgentBackend::ClaudeCode),
        "Codex" => Ok(AgentBackend::Codex),
        "Nanocoder" => Ok(AgentBackend::Nanocoder),
        // Migration-only: Gemini support was removed, but older settings files
        // may still contain this persisted preference.
        "Gemini" => Ok(AgentBackend::default()),
        value => Err(de::Error::unknown_variant(
            value,
            &["ClaudeCode", "Codex", "Nanocoder"],
        )),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileSortKey {
    #[default]
    Filename,
    Title,
    Author,
    Created,
    Modified,
    Size,
    Publication,
    Citations,
    /// The order [`crate::reading_order`] gives the listed folder. Not a
    /// field of a file: the list asks for it.
    ReadingOrder,
}

/// Optional document-metadata field that can be shown as a column in the file
/// list. `Settings::file_display_fields` holds the set currently visible.
/// Extend with new variants as more metadata is projected onto `FileEntry`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileDisplayField {
    Title,
    Author,
    Created,
    Modified,
    Publication,
    Citations,
    Size,
}

/// Which source wins when several know a document's title, author, DOI or
/// date. `File` and `Zotero` are the two that are not literature providers; any
/// other value names a provider by id (`openalex`, `semantic_scholar`,
/// `custom:crossref`), so a provider added tomorrow is selectable with no
/// change here.
///
/// Serialized as the bare string the settings file has always held, so a value
/// stored when only two providers existed still reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MetadataSourcePreference {
    File,
    #[default]
    Zotero,
    Provider(String),
}

impl MetadataSourcePreference {
    pub fn as_str(&self) -> &str {
        match self {
            Self::File => "file",
            Self::Zotero => "zotero",
            Self::Provider(id) => id,
        }
    }
}

impl Serialize for MetadataSourcePreference {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for MetadataSourcePreference {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "file" => Self::File,
            "zotero" => Self::Zotero,
            "" => return Err(serde::de::Error::custom("metadata source cannot be empty")),
            _ => Self::Provider(value),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileSortDirection {
    #[default]
    Asc,
    Desc,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum BookmarkDock {
    Left,
    #[default]
    Right,
}

/// How document pages are darkened when the app is in dark mode. Inverting
/// turns a white page dark; dimming keeps its colours and lowers its brightness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DarkModeReading {
    #[default]
    Invert,
    Dim,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}
