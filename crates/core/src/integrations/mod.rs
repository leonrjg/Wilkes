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
//! There is one implementation: [`custom::CustomSource`], which reads a
//! [`custom::manifest::Manifest`]. OpenAlex and Semantic Scholar are manifests
//! embedded in the binary ([`builtin`]); a manifest a user pastes in is stored
//! in settings. Both enter [`IntegrationRegistry`] through the same door, and
//! every caller looks a provider up by id and gets a trait object; none of
//! them can tell, or needs to tell, which kind it got.
//!
//! Zotero is not a literature provider — it is a local library with a write
//! surface — and stays what it was.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::types::{
    BrowseRequest, IntegrationStatus, IntegrationsSettings, LiteratureSearchResult, ProviderWork,
    ResolvedLiteratureDownload,
};

pub mod builtin;
pub mod custom;
pub mod zotero;

use custom::manifest::{BrowseControls, Manifest};
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
}

/// A searchable index of scholarly works.
#[async_trait]
pub trait LiteratureSource: Send + Sync {
    /// Stable identifier, as named by callers (`openalex`, `custom:crossref`).
    fn id(&self) -> &str;

    /// Human-readable name, for status messages and errors.
    fn name(&self) -> &str;

    /// The description this provider was built from.
    fn manifest(&self) -> &Manifest;

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
    async fn browse(
        &self,
        _request: &BrowseRequest,
    ) -> anyhow::Result<Vec<LiteratureSearchResult>> {
        anyhow::bail!("{} does not offer browse", self.name())
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

/// One provider as the registry holds it: the source plus whether the user has
/// it switched on.
///
/// `enabled` lives here rather than inside the source because it is a fact
/// about the user's settings, not about the service. Keeping disabled
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

    pub fn manifest(&self) -> &Manifest {
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
/// A registry is built from [`IntegrationsSettings`] on demand rather than
/// held as shared mutable state and invalidated on change. It is a pure
/// function of settings, so there is no window in which it disagrees with them
/// and no invalidation to forget; building one parses a handful of small
/// manifests and allocates a handful of small clients.
pub struct IntegrationRegistry {
    literature: Vec<RegisteredLiteratureSource>,
}

impl IntegrationRegistry {
    pub fn from_settings(settings: &IntegrationsSettings) -> Self {
        let mut literature: Vec<RegisteredLiteratureSource> = Vec::new();

        for mut manifest in builtin::manifests() {
            let state = settings
                .builtin
                .get(&manifest.id)
                .cloned()
                .unwrap_or_default();
            // A user's own mirror or proxy. The origin the manifest then
            // contacts is this one, and is the one the settings form shows.
            if let Some(base_url) = state
                .base_url
                .as_deref()
                .map(str::trim)
                .filter(|base_url| !base_url.is_empty())
            {
                manifest.http.base_url = base_url.to_string();
            }
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
            match CustomSource::from_config(config) {
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
        let registry = IntegrationRegistry::from_settings(&settings(false, false));
        let mut ids = registry.literature_ids();
        ids.sort();
        assert_eq!(ids, vec!["openalex", "semantic_scholar"]);
        assert!(registry.enabled_literature_ids().is_empty());
    }

    #[test]
    fn a_default_installation_has_both_built_ins_switched_off() {
        let registry = IntegrationRegistry::from_settings(&IntegrationsSettings::default());
        assert_eq!(registry.literature_ids().len(), 2);
        assert!(registry.enabled_literature_ids().is_empty());
        assert!(registry.literature_sources().all(|entry| entry.builtin));
    }

    #[test]
    fn disabled_and_unknown_are_different_errors() {
        let registry = IntegrationRegistry::from_settings(&settings(true, false));
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
        let registry = IntegrationRegistry::from_settings(&settings);
        let entry = registry.literature("openalex").unwrap();
        assert_eq!(
            entry.manifest().origins(),
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
    fn capabilities_are_what_the_manifest_declares() {
        let registry = IntegrationRegistry::from_settings(&IntegrationsSettings::default());
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
}
