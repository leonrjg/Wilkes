//! Integration settings: providers, custom integrations and Zotero.

use super::*;

/// Which integrations exist and how this installation has them configured.
///
/// Deserialization reads two older shapes: the `semantic_scholar` and
/// `openalex` objects written before those providers became manifests, and
/// custom manifests written before manifest version 2. Both are rewritten into
/// today's shape as they load, so nothing past this boundary ever meets them,
/// and the next save writes only the new form.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(from = "IntegrationsSettingsWire")]
pub struct IntegrationsSettings {
    pub zotero: ZoteroSettings,
    /// This installation's part of each provider shipped with Wilkes, by
    /// provider id: whether it is switched on, the values for the secrets its
    /// manifest names, an optional base URL. The manifest itself ships with
    /// the build and is not stored.
    pub builtin: std::collections::BTreeMap<String, BuiltinProviderState>,
    /// Providers the user described rather than ones Wilkes shipped.
    ///
    /// Here, and not in a table of their own, because a provider must reach
    /// every consumer that already receives settings — the MCP server is
    /// handed an `IntegrationsSettings` by value and can reach no database —
    /// and a second configuration channel for the same question is exactly the
    /// duplication this feature exists to remove.
    pub custom: Vec<CustomIntegrationConfig>,
    /// Programs a manifest may run to fetch a file, by the exact name a
    /// manifest gives them. The user's to grant, and never a manifest's:
    /// importing one that names a program does not put it here. A host that
    /// fixes the list for itself (the server, from its environment) answers
    /// with its own and refuses edits to this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_programs: Vec<String>,
}

/// The user's part of a shipped provider.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuiltinProviderState {
    #[serde(default)]
    pub enabled: bool,
    /// A mirror or proxy to use instead of the manifest's own origin. Absent
    /// means the manifest's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Values for the secrets the manifest names (`email`, `api_key`). Held
    /// here and never in the manifest, which ships with the build.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub secrets: HashMap<String, String>,
}

#[derive(Deserialize)]
pub(super) struct IntegrationsSettingsWire {
    #[serde(default)]
    pub(super) zotero: ZoteroSettings,
    #[serde(default)]
    pub(super) builtin: std::collections::BTreeMap<String, BuiltinProviderState>,
    #[serde(default)]
    pub(super) custom: Vec<CustomIntegrationConfig>,
    #[serde(default)]
    pub(super) allowed_programs: Vec<String>,
    #[serde(default)]
    pub(super) semantic_scholar: Option<LegacyRemoteSettings>,
    #[serde(default)]
    pub(super) openalex: Option<LegacyRemoteSettings>,
}

/// What `semantic_scholar` and `openalex` held before they were manifests: an
/// enable flag, a base URL, and one optional credential. Semantic Scholar's
/// was an API key; OpenAlex's was a contact address for its `mailto` pool,
/// which OpenAlex retired in February 2026, so it is not carried over.
#[derive(Deserialize)]
pub(super) struct LegacyRemoteSettings {
    #[serde(default)]
    pub(super) enabled: bool,
    #[serde(default)]
    pub(super) base_url: Option<String>,
    #[serde(default)]
    pub(super) api_key: Option<String>,
}

impl LegacyRemoteSettings {
    /// The same installation's choices, in today's shape. A base URL equal to
    /// the one the manifest ships with is no override.
    pub(super) fn into_state(
        self,
        shipped_base_url: &str,
        secret_name: &str,
    ) -> BuiltinProviderState {
        let credential = self
            .api_key
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        BuiltinProviderState {
            enabled: self.enabled,
            base_url: self
                .base_url
                .map(|url| url.trim().trim_end_matches('/').to_string())
                .filter(|url| !url.is_empty() && url != shipped_base_url),
            secrets: credential
                .map(|value| HashMap::from([(secret_name.to_string(), value)]))
                .unwrap_or_default(),
        }
    }
}

impl From<IntegrationsSettingsWire> for IntegrationsSettings {
    fn from(wire: IntegrationsSettingsWire) -> Self {
        let mut builtin = wire.builtin;
        // A value already in today's shape wins: it was written after the
        // older one, by a build that no longer wrote the older key.
        if let Some(legacy) = wire.semantic_scholar {
            builtin
                .entry("semantic_scholar".to_string())
                .or_insert_with(|| legacy.into_state("https://api.semanticscholar.org", "api_key"));
        }
        if let Some(legacy) = wire.openalex {
            builtin
                .entry("openalex".to_string())
                .or_insert_with(|| legacy.into_state("https://api.openalex.org", "api_key"));
        }
        // OpenAlex's manifest named an `email` secret for its `mailto` pool
        // until OpenAlex retired that pool. A stored address is sent nowhere
        // now, so it is not kept to be written back.
        if let Some(openalex) = builtin.get_mut("openalex") {
            openalex.secrets.remove("email");
        }

        let custom = wire
            .custom
            .into_iter()
            .map(|mut config| {
                match crate::integrations::custom::legacy::upgrade(&config.manifest) {
                    Ok(Some(upgraded)) => {
                        tracing::info!(
                            integration = %config.id,
                            "custom integration was rewritten as a version 2 manifest; \
                             comments in the original text are not kept"
                        );
                        config.manifest = upgraded;
                    }
                    Ok(None) => {}
                    // Left as stored: the registry reports it unloadable, by
                    // name, instead of this guessing at what it meant.
                    Err(error) => tracing::error!(
                        integration = %config.id,
                        "custom integration could not be converted to manifest version 2: {error:#}"
                    ),
                }
                config
            })
            .collect();

        Self {
            zotero: wire.zotero,
            builtin,
            custom,
            allowed_programs: wire.allowed_programs,
        }
    }
}

/// One stored custom integration.
///
/// The manifest is kept as the user wrote it, comments and formatting intact,
/// because it is the document they edit and export; everything else about the
/// provider is derived from parsing it. `id` is stored alongside so a manifest
/// that fails to parse can still be named in an error message, and the two are
/// required to agree when the manifest is saved.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CustomIntegrationConfig {
    pub id: String,
    /// What this installation calls the provider, when the user has renamed
    /// it. Absent means the manifest's own `name` stands.
    ///
    /// Held here rather than edited into the manifest because the manifest
    /// describes the *service* — it is the thing a user is handed, copies and
    /// shares — while this names their copy of it. Rewriting the manifest to
    /// rename a row would also mean re-reading and re-probing it, which is a
    /// lot of ceremony for a label.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    /// TOML or JSON manifest source. See
    /// `wilkes_core::integrations::custom::manifest`.
    pub manifest: String,
    /// Values for the secrets the manifest names. Held beside the manifest and
    /// never inside it, so exporting a manifest cannot leak a credential.
    #[serde(default)]
    pub secrets: HashMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZoteroSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_zotero_base_url")]
    pub base_url: String,
    #[serde(default = "default_zotero_citation_style")]
    pub citation_style: String,
}

impl Default for ZoteroSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_zotero_base_url(),
            citation_style: default_zotero_citation_style(),
        }
    }
}

pub(super) fn default_zotero_base_url() -> String {
    "http://127.0.0.1:23119".to_string()
}

pub(super) fn default_zotero_citation_style() -> String {
    "chicago-note-bibliography".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationState {
    Disabled,
    ZoteroDown,
    LocalApiDisabled,
    RemoteApiDown,
    RateLimited,
    Ready,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntegrationStatus {
    pub id: String,
    pub enabled: bool,
    pub state: IntegrationState,
    pub message: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum AddOutcome {
    Added { item_key: Option<String> },
    AlreadyPresent { item_key: String },
    PossibleDuplicate { item_key: String, message: String },
}

/// CSL citation strings for a resolved Zotero item. `citation` is the in-text
/// form and `bibliography` the full reference; both are HTML produced by Zotero.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CitationResult {
    pub citation: Option<String>,
    pub bibliography: Option<String>,
    /// True when the item was resolved by a weak signal (filename or title),
    /// so the citation may belong to the wrong work.
    pub low_confidence: bool,
}
