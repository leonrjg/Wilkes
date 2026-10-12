//! The external services Wilkes can ask questions of, and the contract they
//! answer under.
//!
//! # A provider is a description of a service, not a compilation unit
//!
//! [`LiteratureSource`] is query in, [`LiteratureSearchResult`]s out — and,
//! for a service that offers them, filtered listings, DOI lookups and
//! reference lists. Nothing provider-specific — an OpenAlex work id, a
//! Semantic Scholar paper id, the shape of either service's JSON — crosses it.
//!
//! A remote service is [`custom::CustomSource`], which reads a
//! [`custom::manifest::Manifest`]. OpenAlex and Semantic Scholar are manifests
//! embedded in the binary ([`builtin`]); a manifest a user pastes in is stored
//! in settings. A learning catalogue is
//! [`crate::catalogue::MirroredSource`], whose answer comes from a copy of the
//! catalogue kept on this machine. All of them enter [`IntegrationRegistry`]
//! through the same door, and every caller looks a provider up by id and gets
//! a trait object; none of them can tell, or needs to tell, which kind it got.
//!
//! Zotero is not a literature provider — it is a local library with a write
//! surface — and stays what it was.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::catalogue::{mirrored_sources, CatalogueStore};

use crate::types::{
    BrowsePage, BrowseRequest, BuiltinProviderState, FilterSuggestion, IntegrationStatus,
    IntegrationsSettings, LiteratureSearchResult, ProviderWork, ResolvedLiteratureDownload,
};

pub mod builtin;
pub mod custom;
pub mod zotero;

use custom::manifest::{Base, Bases, BrowseControls, Manifest, SecretSlot};
use custom::{CustomSource, Standing};

/// What a provider can do beyond keyword search, for a caller that has to offer
/// it before it has asked anything.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProviderCapabilities {
    /// Whether keyword search can be read past its first page. A provider that
    /// cannot is asked for one page, and a second would repeat the first.
    pub paged_search: bool,
    /// The filter panel the provider declares, if it offers a listing.
    pub browse: Option<BrowseControls>,
    /// Whether a DOI can be looked up (metadata enrichment).
    pub lookup: bool,
    /// Whether the DOIs a work cites can be listed (citation graph).
    pub references: bool,
    pub resolve_download: bool,
    /// Whether the provider is answered from a copy held on this machine. One
    /// that is sends nothing anywhere when it is asked, and holds nothing
    /// until it has been fetched.
    pub mirrored: bool,
}

/// A searchable index of scholarly works.
#[async_trait]
pub trait LiteratureSource: Send + Sync {
    /// Stable identifier, as named by callers (`openalex`, `custom:crossref`).
    fn id(&self) -> &str;

    /// Human-readable name, for status messages and errors.
    fn name(&self) -> &str;

    /// The description this provider was built from, for one that was built
    /// from a description.
    fn manifest(&self) -> Option<&Manifest>;

    /// The values this provider asks the user for (an email address, an API
    /// key). A provider that extends another asks only for its own: the
    /// other's are set on it.
    fn secret_slots(&self) -> Vec<SecretSlot> {
        Vec::new()
    }

    /// The origin the provider's records come from.
    fn base_url(&self) -> &str;

    fn capabilities(&self) -> ProviderCapabilities;

    /// `page` is 1-based. A page past the first is refused by a provider whose
    /// [`ProviderCapabilities::paged_search`] is false.
    async fn search(
        &self,
        query: &str,
        limit: usize,
        page: usize,
    ) -> anyhow::Result<Vec<LiteratureSearchResult>>;

    /// A listing by filter and sort, with the keyword optional. What may be
    /// asked is what [`ProviderCapabilities::browse`] declares.
    async fn browse(&self, _request: &BrowseRequest) -> anyhow::Result<BrowsePage> {
        anyhow::bail!("{} does not offer browse", self.name())
    }

    /// Whether [`Self::browse`] would send `request`, without sending it: what
    /// the provider declares decides, so a request built by something other
    /// than a person at the filter panel can be checked before it is shown as
    /// one. The error names what was refused.
    fn check_browse(&self, _request: &BrowseRequest) -> anyhow::Result<()> {
        anyhow::bail!("{} does not offer browse", self.name())
    }

    /// What the provider suggests for one of its filters as the user types.
    /// Only a filter that declares `suggest` can be asked.
    async fn suggest(&self, _filter: &str, _text: &str) -> anyhow::Result<Vec<FilterSuggestion>> {
        anyhow::bail!("{} does not offer suggestions", self.name())
    }

    /// One bibliographic record for a DOI, for the metadata cache.
    async fn lookup(&self, _doi: &str) -> anyhow::Result<ProviderWork> {
        anyhow::bail!("{} does not offer lookup", self.name())
    }

    /// The normalized DOIs the work identified by `doi` references.
    ///
    /// **DOI in, DOIs out**: callers identify a work by its DOI and receive
    /// DOIs, so no provider-specific identifier ever crosses this boundary and
    /// the citation-graph storage stays decoupled from whichever provider
    /// supplies the edges. References the provider cannot resolve to a DOI
    /// (books, datasets, works without a registered one) are omitted: they can
    /// never participate in a DOI-keyed library join, so they are not edges we
    /// can store.
    async fn references(&self, _doi: &str) -> anyhow::Result<Vec<String>> {
        anyhow::bail!("{} does not offer references", self.name())
    }

    /// Resolve a selected result to a direct file URL. Providers that already
    /// put that URL in `pdf_url` need nothing more; a manifest may declare a
    /// bounded, credentialed resolver that runs only after the user chooses a
    /// result.
    async fn resolve_download(
        &self,
        result: &LiteratureSearchResult,
    ) -> anyhow::Result<Option<ResolvedLiteratureDownload>> {
        Ok(result
            .pdf_url
            .as_ref()
            .map(|url| ResolvedLiteratureDownload {
                url: url.clone(),
                filename: None,
            }))
    }

    /// Whether the service is reachable and usable right now.
    ///
    /// Probed whether or not the integration is enabled: the settings form
    /// asks this *before* enabling, so a short-circuit on `enabled` would
    /// answer "disabled" and the integration could never be switched on.
    async fn status(&self, enabled: bool) -> anyhow::Result<IntegrationStatus>;
}

/// The shipped manifests as this installation reaches them: each beside the
/// user's part of it, with a mirror or proxy the user chose already in place
/// of the shipped origin. The origin a manifest then contacts is that one, and
/// is the one the settings form shows.
fn shipped(
    settings: &IntegrationsSettings,
) -> impl Iterator<Item = (Manifest, BuiltinProviderState)> + '_ {
    builtin::manifests().into_iter().map(|mut manifest| {
        let state = settings
            .builtin
            .get(&manifest.id)
            .cloned()
            .unwrap_or_default();
        if let Some(base_url) = state
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|base_url| !base_url.is_empty())
        {
            manifest.http.base_url = base_url.to_string();
        }
        (manifest, state)
    })
}

/// The providers a manifest in these settings may extend: the shipped ones as
/// this installation reaches them, and the user's own that stand on their own.
///
/// Whether the base is switched on is not asked. Extending reads a description
/// of a service; it does not go through the provider that description runs as.
impl Bases for IntegrationsSettings {
    fn base(&self, provider: &str) -> anyhow::Result<Base> {
        if let Some((manifest, state)) = shipped(self).find(|(manifest, _)| manifest.id == provider)
        {
            return Ok(Base {
                manifest,
                secrets: state.secrets,
            });
        }
        let config = provider
            .strip_prefix("custom:")
            .and_then(|id| self.custom.iter().find(|config| config.id == id))
            .ok_or_else(|| {
                let known: Vec<String> = builtin::BUILTIN_MANIFESTS
                    .iter()
                    .map(|shipped| shipped.id.to_string())
                    .chain(self.custom.iter().map(|config| format!("custom:{}", config.id)))
                    .collect();
                anyhow::anyhow!(
                    "manifest extends '{provider}', which is not a provider here; expected one of: {}",
                    known.join(", ")
                )
            })?;
        let manifest = Manifest::parse(&config.manifest)
            .map_err(|error| anyhow::anyhow!("'{provider}' cannot be extended: {error}"))?;
        Ok(Base {
            manifest,
            secrets: config.secrets.clone(),
        })
    }
}

/// What a derived provider takes from its base, for [`IntegrationRegistry::fingerprints`]:
/// the manifest it resolves to and the base's secrets. A provider that extends
/// nothing, or cannot be read, has none.
fn inherited_fingerprint(
    config: &crate::types::CustomIntegrationConfig,
    settings: &IntegrationsSettings,
) -> serde_json::Value {
    match Manifest::resolve(&config.manifest, settings) {
        Ok(resolved) => match resolved.inherited {
            Some(inherited) => serde_json::json!({
                "manifest": resolved.manifest,
                "secrets": inherited.secrets.iter().collect::<std::collections::BTreeMap<_, _>>(),
            }),
            None => serde_json::Value::Null,
        },
        // The registry names an unloadable manifest when it drops it.
        Err(error) => {
            tracing::debug!(
                integration = %config.id,
                "custom integration has no resolved manifest to compare: {error:#}"
            );
            serde_json::Value::Null
        }
    }
}

/// One provider as the registry holds it: the source plus whether the user has
/// it switched on.
///
/// `enabled` lives here rather than inside the source because it is a fact
/// about this installation, not about the service: the user's setting for a
/// remote provider, and for a mirrored one whether it has been fetched — a
/// catalogue holding nothing cannot answer, and one holding something is one
/// the user asked for. Keeping disabled
/// providers in the registry is deliberate — it is what lets a caller
/// distinguish *you named a provider that is switched off* from *you named a
/// provider that does not exist*, which are different mistakes with different
/// fixes.
pub struct RegisteredLiteratureSource {
    pub enabled: bool,
    /// Shipped with Wilkes rather than written by the user. Decides whether the
    /// interface offers to edit or delete its manifest (it does not).
    pub builtin: bool,
    source: Arc<dyn LiteratureSource>,
}

impl RegisteredLiteratureSource {
    pub fn id(&self) -> &str {
        self.source.id()
    }

    pub fn name(&self) -> &str {
        self.source.name()
    }

    pub fn manifest(&self) -> Option<&Manifest> {
        self.source.manifest()
    }

    /// The source itself, only for a caller that has checked `enabled`.
    pub fn source(&self) -> &Arc<dyn LiteratureSource> {
        &self.source
    }
}

/// Every provider this installation knows: the ones shipped, and the ones the
/// user described.
///
/// # Derived, not stored
///
/// A registry is built from [`IntegrationsSettings`] and the catalogue mirror
/// on demand rather than held as shared mutable state and invalidated on
/// change. It is a function of those two, so there is no window in which it
/// disagrees with them and no invalidation to forget; building one parses a
/// handful of small manifests, allocates a handful of small clients and reads
/// which catalogues the mirror holds.
pub struct IntegrationRegistry {
    literature: Vec<RegisteredLiteratureSource>,
}

impl IntegrationRegistry {
    /// `mirror_dir` is where the catalogue mirror lives. `None` is a caller
    /// with no mirror at all — a test, a tool run outside an installation —
    /// and registers no catalogue, rather than catalogues that cannot answer.
    pub fn from_settings(settings: &IntegrationsSettings, mirror_dir: Option<&Path>) -> Self {
        let mut literature: Vec<RegisteredLiteratureSource> = Vec::new();

        // Catalogues first: they are answered locally, so in a list ordered by
        // who is asked they are the ones that have already replied.
        if let Some(mirror_dir) = mirror_dir {
            let held = match CatalogueStore::open(mirror_dir).and_then(|store| store.holdings()) {
                Ok(held) => held,
                // The catalogues are still registered: unreadable is not
                // unknown, and a caller naming one should be told it is off.
                Err(error) => {
                    tracing::error!(
                        "catalogue mirror is unreadable, so no catalogue is searchable: {error:#}"
                    );
                    Vec::new()
                }
            };
            for source in mirrored_sources(mirror_dir) {
                literature.push(RegisteredLiteratureSource {
                    enabled: held.iter().any(|held| held.provider == source.id()),
                    builtin: true,
                    source: Arc::new(source),
                });
            }
        }

        for (manifest, state) in shipped(settings) {
            let id = manifest.id.clone();
            match CustomSource::build(manifest, state.secrets, None, Standing::BuiltIn) {
                Ok(source) => literature.push(RegisteredLiteratureSource {
                    enabled: state.enabled,
                    builtin: true,
                    source: Arc::new(source),
                }),
                // Reachable only through a base URL the user typed that is not
                // a URL. Dropping the provider silently would leave one that
                // is configured, enabled and absent.
                Err(error) => tracing::error!(
                    integration = %id,
                    "built-in integration is not loadable and was skipped: {error:#}"
                ),
            }
        }

        for config in &settings.custom {
            match CustomSource::from_config(config, settings) {
                // Only a manifest that declares the capability becomes a
                // literature entry, so "unknown provider" and "provider cannot
                // search" stay one already-handled case rather than two.
                Ok(source) if source.declares_search() => {
                    literature.push(RegisteredLiteratureSource {
                        enabled: config.enabled,
                        builtin: false,
                        source: Arc::new(source),
                    })
                }
                Ok(_) => {}
                // A stored manifest can only become invalid by being edited
                // outside the app or by outliving a manifest version, since
                // nothing is saved unvalidated. Dropping the provider is the
                // only coherent response — but silently dropping it would
                // leave the user with a provider that is configured, enabled
                // and absent, with nothing anywhere saying why.
                Err(error) => tracing::error!(
                    integration = %config.id,
                    "custom integration is not loadable and was skipped: {error}"
                ),
            }
        }

        Self { literature }
    }

    /// What a settings change would have to differ in for a provider to behave
    /// differently: whether it is on, the values of its secrets, its origin and,
    /// for a provider the user wrote, its manifest. Keyed by provider id, one
    /// entry per provider that exists under `settings`.
    ///
    /// Not a hash and not stable across builds — only ever compared with
    /// another computed by the same build, to find which providers a settings
    /// edit touched. The name a user gave a provider is deliberately absent: a
    /// label does not change what the service says.
    pub fn fingerprints(
        settings: &IntegrationsSettings,
    ) -> std::collections::BTreeMap<String, String> {
        let mut out = std::collections::BTreeMap::new();
        for shipped in builtin::BUILTIN_MANIFESTS {
            let state = settings
                .builtin
                .get(shipped.id)
                .cloned()
                .unwrap_or_default();
            out.insert(
                shipped.id.to_string(),
                serde_json::json!({
                    "enabled": state.enabled,
                    "base_url": state.base_url,
                    "secrets": state.secrets.iter().collect::<std::collections::BTreeMap<_, _>>(),
                })
                .to_string(),
            );
        }
        for config in &settings.custom {
            out.insert(
                format!("custom:{}", config.id),
                serde_json::json!({
                    "enabled": config.enabled,
                    "manifest": config.manifest,
                    "base": inherited_fingerprint(config, settings),
                    "secrets": config.secrets.iter().collect::<std::collections::BTreeMap<_, _>>(),
                })
                .to_string(),
            );
        }
        out
    }

    pub fn literature(&self, id: &str) -> Option<&RegisteredLiteratureSource> {
        self.literature.iter().find(|entry| entry.id() == id)
    }

    /// Ids of the providers a caller may name, enabled ones first.
    ///
    /// Used to build the error a caller sees when it names something else, and
    /// to tell an agent what it may actually ask for.
    pub fn literature_ids(&self) -> Vec<&str> {
        self.literature.iter().map(|entry| entry.id()).collect()
    }

    pub fn enabled_literature_ids(&self) -> Vec<&str> {
        self.literature
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.id())
            .collect()
    }

    pub fn literature_sources(&self) -> impl Iterator<Item = &RegisteredLiteratureSource> {
        self.literature.iter()
    }

    /// Resolve a provider for a search, or say precisely why not.
    ///
    /// One place answers *may I search with this?*, so the disabled message
    /// and the unknown-provider message cannot drift apart between the MCP
    /// tool, a command and a route.
    pub fn literature_for_search(
        &self,
        id: &str,
    ) -> Result<&Arc<dyn LiteratureSource>, LiteratureLookupError> {
        match self.literature(id) {
            Some(entry) if entry.enabled => Ok(entry.source()),
            Some(entry) => Err(LiteratureLookupError::Disabled {
                name: entry.name().to_string(),
            }),
            None => Err(LiteratureLookupError::Unknown {
                requested: id.to_string(),
                available: self
                    .enabled_literature_ids()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiteratureLookupError {
    Disabled {
        name: String,
    },
    Unknown {
        requested: String,
        available: Vec<String>,
    },
}

impl std::fmt::Display for LiteratureLookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled { name } => write!(f, "{name} integration is disabled."),
            Self::Unknown {
                requested,
                available,
            } if available.is_empty() => write!(
                f,
                "Unknown literature provider '{requested}'. No literature provider is enabled."
            ),
            Self::Unknown {
                requested,
                available,
            } => write!(
                f,
                "Unknown literature provider '{requested}'. Enabled providers: {}.",
                available.join(", ")
            ),
        }
    }
}

impl std::error::Error for LiteratureLookupError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BuiltinProviderState;

    fn settings(openalex: bool, semantic_scholar: bool) -> IntegrationsSettings {
        let mut settings = IntegrationsSettings::default();
        settings.builtin.insert(
            "openalex".into(),
            BuiltinProviderState {
                enabled: openalex,
                ..Default::default()
            },
        );
        settings.builtin.insert(
            "semantic_scholar".into(),
            BuiltinProviderState {
                enabled: semantic_scholar,
                ..Default::default()
            },
        );
        settings
    }

    #[test]
    fn built_ins_are_registered_whether_enabled_or_not() {
        let registry = IntegrationRegistry::from_settings(&settings(false, false), None);
        let mut ids = registry.literature_ids();
        ids.sort();
        assert_eq!(ids, vec!["openalex", "semantic_scholar"]);
        assert!(registry.enabled_literature_ids().is_empty());
    }

    #[test]
    fn a_default_installation_has_both_built_ins_switched_off() {
        let registry = IntegrationRegistry::from_settings(&IntegrationsSettings::default(), None);
        assert_eq!(registry.literature_ids().len(), 2);
        assert!(registry.enabled_literature_ids().is_empty());
        assert!(registry.literature_sources().all(|entry| entry.builtin));
    }

    #[test]
    fn disabled_and_unknown_are_different_errors() {
        let registry = IntegrationRegistry::from_settings(&settings(true, false), None);
        assert!(registry.literature_for_search("openalex").is_ok());

        let disabled = registry
            .literature_for_search("semantic_scholar")
            .err()
            .expect("a disabled provider is not searchable");
        assert!(disabled.to_string().contains("disabled"));

        let unknown = registry
            .literature_for_search("crossref")
            .err()
            .expect("an unregistered provider is not searchable");
        assert!(unknown.to_string().contains("Unknown literature provider"));
        // The message names what the caller could have said instead.
        assert!(unknown.to_string().contains("openalex"));
    }

    #[test]
    fn a_base_url_override_moves_the_origin_the_provider_contacts() {
        let mut settings = settings(true, false);
        settings.builtin.get_mut("openalex").unwrap().base_url =
            Some("https://mirror.example.test".into());
        let registry = IntegrationRegistry::from_settings(&settings, None);
        let entry = registry.literature("openalex").unwrap();
        assert_eq!(
            entry.manifest().unwrap().origins(),
            vec!["https://mirror.example.test"]
        );
    }

    #[test]
    fn fingerprints_differ_exactly_where_a_providers_behaviour_would() {
        let base = settings(true, false);
        let same = IntegrationRegistry::fingerprints(&base);
        assert_eq!(same, IntegrationRegistry::fingerprints(&base.clone()));

        let mut toggled = base.clone();
        toggled.builtin.get_mut("openalex").unwrap().enabled = false;
        let mut with_secret = base.clone();
        with_secret
            .builtin
            .get_mut("openalex")
            .unwrap()
            .secrets
            .insert("email".into(), "me@example.org".into());
        for changed in [toggled, with_secret] {
            let after = IntegrationRegistry::fingerprints(&changed);
            assert_ne!(same["openalex"], after["openalex"]);
            // The other provider was not touched.
            assert_eq!(same["semantic_scholar"], after["semantic_scholar"]);
        }
    }

    #[test]
    fn a_custom_providers_label_is_not_part_of_its_fingerprint_but_its_manifest_is() {
        let manifest = r#"
manifest_version = 2
id = "x"
name = "X"
[http]
base_url = "https://x.test"
[capabilities.search]
path = "/s?q={query}"
items = "r"
[capabilities.search.fields]
id = "id"
title = "t"
"#;
        let config = |name: Option<&str>, manifest: &str| {
            let mut settings = IntegrationsSettings::default();
            settings.custom.push(crate::types::CustomIntegrationConfig {
                id: "x".into(),
                name: name.map(str::to_string),
                enabled: true,
                manifest: manifest.to_string(),
                secrets: Default::default(),
            });
            IntegrationRegistry::fingerprints(&settings)
        };
        let plain = config(None, manifest);
        assert_eq!(
            plain["custom:x"],
            config(Some("Renamed"), manifest)["custom:x"]
        );
        assert_ne!(
            plain["custom:x"],
            config(None, &manifest.replace("https://x.test", "https://y.test"))["custom:x"]
        );
    }

    #[test]
    fn a_catalogue_is_one_more_source_and_is_on_once_the_mirror_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let registry =
            IntegrationRegistry::from_settings(&IntegrationsSettings::default(), Some(dir.path()));
        let catalogues: Vec<&str> = registry
            .literature_sources()
            .filter(|entry| entry.source().capabilities().mirrored)
            .map(|entry| entry.id())
            .collect();
        assert_eq!(
            catalogues,
            crate::catalogue::fetchers()
                .iter()
                .map(|fetch| fetch.id())
                .collect::<Vec<_>>()
        );
        // Nothing fetched: registered, and off — a different answer from
        // "unknown", as it is for a remote provider the user has not enabled.
        assert!(registry.enabled_literature_ids().is_empty());
        let off = registry
            .literature_for_search("devdocs")
            .err()
            .expect("an unfetched catalogue is not searchable");
        assert!(matches!(off, LiteratureLookupError::Disabled { .. }));

        CatalogueStore::open(dir.path())
            .unwrap()
            .replace_provider("devdocs", &[])
            .unwrap();
        let registry =
            IntegrationRegistry::from_settings(&IntegrationsSettings::default(), Some(dir.path()));
        assert_eq!(registry.enabled_literature_ids(), vec!["devdocs"]);
        assert!(registry.literature_for_search("devdocs").is_ok());
    }

    #[test]
    fn a_caller_with_no_mirror_registers_no_catalogue() {
        let registry = IntegrationRegistry::from_settings(&IntegrationsSettings::default(), None);
        assert!(registry
            .literature_sources()
            .all(|entry| !entry.source().capabilities().mirrored));
    }

    #[test]
    fn capabilities_are_what_the_manifest_declares() {
        let registry = IntegrationRegistry::from_settings(&IntegrationsSettings::default(), None);
        let openalex = registry
            .literature("openalex")
            .unwrap()
            .source()
            .capabilities();
        assert!(openalex.lookup && openalex.references);
        assert!(openalex.browse.is_some());
        let s2 = registry
            .literature("semantic_scholar")
            .unwrap()
            .source()
            .capabilities();
        assert!(s2.lookup);
        assert!(!s2.references);
        assert!(s2.browse.is_none());
    }

    /// What a user writes to add a download step to OpenAlex and nothing else.
    const OPENALEX_WITH_DOWNLOADS: &str = r#"
manifest_version = 2
id = "openalex-files"
name = "OpenAlex with downloads"
extends = { provider = "openalex", capabilities = ["search", "browse"] }

[capabilities.resolve_download]
url = "{download_url}"
steps = [
  { path = "/files?doi={doi}", query = { key = { secret = "files_key" } }, fields = { download_url = "url" } },
]
"#;

    const STANDALONE: &str = r#"
manifest_version = 2
id = "plain"
name = "Plain"
[http]
base_url = "https://plain.test"
[http.headers]
x-key = { secret = "plain_key" }
[capabilities.search]
path = "/s?q={query}"
items = "r"
[capabilities.search.fields]
id = "id"
title = "t"
"#;

    fn custom(
        id: &str,
        manifest: &str,
        secrets: &[(&str, &str)],
    ) -> crate::types::CustomIntegrationConfig {
        crate::types::CustomIntegrationConfig {
            id: id.into(),
            name: None,
            enabled: true,
            manifest: manifest.to_string(),
            secrets: secrets
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        }
    }

    fn with_custom(configs: Vec<crate::types::CustomIntegrationConfig>) -> IntegrationsSettings {
        IntegrationsSettings {
            custom: configs,
            ..Default::default()
        }
    }

    fn resolve_error(settings: &IntegrationsSettings, manifest: &str) -> String {
        match Manifest::resolve(manifest, settings) {
            Ok(_) => panic!("the manifest was accepted"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn a_derived_provider_has_what_it_took_and_what_it_declared_and_nothing_else() {
        let settings = with_custom(vec![custom("openalex-files", OPENALEX_WITH_DOWNLOADS, &[])]);
        let registry = IntegrationRegistry::from_settings(&settings, None);
        let derived = registry.literature("custom:openalex-files").unwrap();
        let shipped = registry.literature("openalex").unwrap();

        let capabilities = derived.source().capabilities();
        assert_eq!(
            capabilities.browse,
            shipped.source().capabilities().browse,
            "the filters are the base's, not a copy of them"
        );
        assert!(capabilities.resolve_download);
        // It asks for its own secret; OpenAlex's email is asked for on OpenAlex.
        let asked: Vec<String> = derived
            .source()
            .secret_slots()
            .into_iter()
            .map(|slot| slot.name)
            .collect();
        assert_eq!(asked, vec!["files_key"]);
        assert_eq!(shipped.source().secret_slots()[0].name, "email");
        // Not asked for, so not taken: the base goes on being the one provider
        // that enriches metadata and feeds the citation graph.
        assert!(!capabilities.lookup && !capabilities.references);
        assert_eq!(derived.name(), "OpenAlex with downloads");
        assert_eq!(
            derived.manifest().unwrap().origins(),
            vec!["https://api.openalex.org"]
        );
    }

    #[tokio::test]
    async fn a_derived_provider_reaches_its_base_as_this_installation_does() {
        let mut server = mockito::Server::new_async().await;
        let search = server
            .mock("GET", "/works")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("search".into(), "graphs".into()),
                mockito::Matcher::UrlEncoded("mailto".into(), "me@example.org".into()),
            ]))
            .with_status(200)
            .with_body(r#"{"results":[{"id":"https://openalex.org/W1","display_name":"A work","ids":{"doi":"https://doi.org/10.1/x"}}]}"#)
            .create_async()
            .await;
        let files = server
            .mock("GET", "/files")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("doi".into(), "10.1/x".into()),
                mockito::Matcher::UrlEncoded("key".into(), "k".into()),
                mockito::Matcher::UrlEncoded("mailto".into(), "me@example.org".into()),
            ]))
            .with_status(200)
            .with_body(r#"{"url":"https://files.test/x.pdf"}"#)
            .create_async()
            .await;

        // The base is switched off: it is read as a description, not asked.
        // A stale value under the base's secret name is not the base's value.
        let mut settings = with_custom(vec![custom(
            "openalex-files",
            OPENALEX_WITH_DOWNLOADS,
            &[("files_key", "k"), ("email", "stale@example.org")],
        )]);
        settings.builtin.insert(
            "openalex".into(),
            BuiltinProviderState {
                enabled: false,
                base_url: Some(server.url()),
                secrets: [("email".to_string(), "me@example.org".to_string())].into(),
            },
        );

        let registry = IntegrationRegistry::from_settings(&settings, None);
        let source = registry
            .literature_for_search("custom:openalex-files")
            .unwrap();
        let work = source.search("graphs", 1, 1).await.unwrap().remove(0);
        let download = source.resolve_download(&work).await.unwrap().unwrap();

        assert_eq!(download.url, "https://files.test/x.pdf");
        search.assert_async().await;
        files.assert_async().await;
    }

    #[test]
    fn a_users_own_provider_can_be_extended() {
        let derived = r#"
manifest_version = 2
id = "plain-more"
name = "Plain, with health"
extends = { provider = "custom:plain", capabilities = ["search"] }
[capabilities.health]
path = "/ping"
"#;
        let settings = with_custom(vec![
            custom("plain", STANDALONE, &[("plain_key", "k")]),
            custom("plain-more", derived, &[]),
        ]);
        let resolved = Manifest::resolve(derived, &settings).unwrap();
        assert_eq!(resolved.manifest.http.base_url, "https://plain.test");
        assert_eq!(
            resolved.manifest.capabilities.declared(),
            vec!["search", "health"]
        );
        let inherited = resolved.inherited.as_ref().unwrap();
        assert_eq!(inherited.secrets["plain_key"], "k");
        // The base's secret is set on the base, so this one has none to ask for.
        assert!(resolved.own_secret_slots().is_empty());
        assert!(IntegrationRegistry::from_settings(&settings, None)
            .literature("custom:plain-more")
            .is_some());
    }

    #[test]
    fn a_base_stands_on_its_own_and_exists() {
        let chained = r#"
manifest_version = 2
id = "second"
name = "Second"
extends = { provider = "custom:openalex-files", capabilities = ["search"] }
[capabilities.health]
path = "/ping"
"#;
        let settings = with_custom(vec![custom("openalex-files", OPENALEX_WITH_DOWNLOADS, &[])]);
        let error = resolve_error(&settings, chained);
        assert!(
            error.contains("'custom:openalex-files' cannot be extended"),
            "{error}"
        );
        assert!(error.contains("does not stand on its own"), "{error}");

        let missing = chained.replace("custom:openalex-files", "custom:gone");
        let error = resolve_error(&settings, &missing);
        assert!(
            error.contains("'custom:gone', which is not a provider here"),
            "{error}"
        );
        // The message names what could have been written instead.
        assert!(error.contains("openalex, custom:openalex-files"), "{error}");

        let itself = chained.replace("custom:openalex-files", "custom:second");
        assert!(resolve_error(&settings, &itself).contains("which is itself"));
    }

    #[test]
    fn a_capability_has_one_author() {
        let settings = IntegrationsSettings::default();
        for (from, to, expected) in [
            (
                "[capabilities.resolve_download]",
                "[http]\nbase_url = \"https://elsewhere.test\"\n[capabilities.resolve_download]",
                "[http] is taken from 'openalex'",
            ),
            (
                "\"search\", \"browse\"",
                "\"search\", \"resolve_download\"",
                "resolve_download is both taken from 'openalex' and declared here",
            ),
            (
                "\"search\", \"browse\"",
                "\"search\", \"search\"",
                "names 'search' twice",
            ),
            (
                "\"search\", \"browse\"",
                "\"search\", \"filters\"",
                "'filters' is not a capability",
            ),
            ("\"search\", \"browse\"", "", "names nothing to take"),
            (
                "\"search\", \"browse\"",
                "\"browse\"",
                "resolve_download requires search",
            ),
            (
                "secret = \"files_key\"",
                "secret = \"email\"",
                "secret 'email' is one 'openalex' names",
            ),
        ] {
            assert!(OPENALEX_WITH_DOWNLOADS.contains(from), "{from}");
            let error = resolve_error(&settings, &OPENALEX_WITH_DOWNLOADS.replace(from, to));
            assert!(error.contains(expected), "{expected}: {error}");
        }

        let semantic = OPENALEX_WITH_DOWNLOADS.replace("\"openalex\"", "\"semantic_scholar\"");
        let error = resolve_error(&settings, &semantic);
        assert!(
            error.contains("'semantic_scholar' declares no browse to take"),
            "{error}"
        );
    }

    #[test]
    fn a_manifest_that_extends_is_not_read_on_its_own() {
        let error = Manifest::parse(OPENALEX_WITH_DOWNLOADS)
            .unwrap_err()
            .to_string();
        assert!(error.contains("extends 'openalex'"), "{error}");
        // And one that extends nothing still names its own origin.
        let error = Manifest::parse("manifest_version = 2\nid = \"x\"\nname = \"X\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("declares no [http]"), "{error}");
    }

    #[test]
    fn a_derived_providers_fingerprint_follows_its_base() {
        let settings = with_custom(vec![custom("openalex-files", OPENALEX_WITH_DOWNLOADS, &[])]);
        let before = IntegrationRegistry::fingerprints(&settings);

        let mut moved = settings.clone();
        moved.builtin.entry("openalex".into()).or_default().base_url =
            Some("https://mirror.test".into());
        let mut keyed = settings.clone();
        keyed
            .builtin
            .entry("openalex".into())
            .or_default()
            .secrets
            .insert("email".into(), "me@example.org".into());
        for changed in [moved, keyed] {
            let after = IntegrationRegistry::fingerprints(&changed);
            assert_ne!(
                before["custom:openalex-files"],
                after["custom:openalex-files"]
            );
        }

        // Switching the base on or off changes nothing about what the derived
        // provider would send.
        let mut toggled = settings.clone();
        toggled
            .builtin
            .entry("openalex".into())
            .or_default()
            .enabled = true;
        assert_eq!(
            before["custom:openalex-files"],
            IntegrationRegistry::fingerprints(&toggled)["custom:openalex-files"]
        );
    }
}
