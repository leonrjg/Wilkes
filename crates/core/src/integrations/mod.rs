//! The external services Wilkes can ask questions of, and the contracts they
//! answer under.
//!
//! # A provider is a description of a service, not a compilation unit
//!
//! Each contract here is provider-neutral: [`CitationSource`] is DOI in, DOIs
//! out; [`LiteratureSource`] is query in, [`LiteratureSearchResult`]s out.
//! Nothing provider-specific — an OpenAlex work id, a Semantic Scholar paper
//! id, the shape of either service's JSON — crosses one of them.
//!
//! That is what lets [`custom`] exist. A user-authored manifest implements
//! [`LiteratureSource`] by describing a service rather than by being compiled
//! against it, and enters [`IntegrationRegistry`] through the same door
//! `OpenAlexClient` does. Every caller looks a provider up by id and gets a
//! trait object; none of them can tell, or needs to tell, which kind it got.

use std::sync::Arc;

use async_trait::async_trait;

use crate::types::{IntegrationStatus, IntegrationsSettings, LiteratureSearchResult};

pub mod citations;
pub mod custom;
pub mod openalex;
pub mod semantic_scholar;
pub mod zotero;

pub use citations::CitationSource;

/// A searchable index of scholarly works.
///
/// Extracted from what was a two-armed `match` on an enum inside the MCP tool:
/// each arm re-checked `enabled`, re-built a client, and re-wrapped the same
/// result, so a third provider meant a third copy and a user-defined provider
/// meant no arm could exist at all. The signature is unchanged from the
/// inherent `search` both built-in clients already had.
#[async_trait]
pub trait LiteratureSource: Send + Sync {
    /// Stable identifier, as named by callers (`openalex`, `custom:crossref`).
    fn id(&self) -> &str;

    /// Human-readable name, for status messages and errors.
    fn name(&self) -> &str;

    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<LiteratureSearchResult>>;

    /// Whether the service is reachable and usable right now.
    ///
    /// Takes `enabled` because a disabled integration reports *disabled*
    /// rather than being probed: the answer is known without a request, and
    /// making one would be a request the user switched off.
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
    source: Arc<dyn LiteratureSource>,
}

impl RegisteredLiteratureSource {
    pub fn id(&self) -> &str {
        self.source.id()
    }

    pub fn name(&self) -> &str {
        self.source.name()
    }

    /// The source itself, only for a caller that has checked `enabled`.
    pub fn source(&self) -> &Arc<dyn LiteratureSource> {
        &self.source
    }
}

/// Every provider this installation knows: the ones compiled in, and the ones
/// the user described.
///
/// # Derived, not stored
///
/// A registry is built from [`IntegrationsSettings`] on demand rather than
/// held as shared mutable state and invalidated on change. It is a pure
/// function of settings, so there is no window in which it disagrees with them
/// and no invalidation to forget; building one allocates a handful of small
/// clients, which is what the previous code did per request anyway.
pub struct IntegrationRegistry {
    literature: Vec<RegisteredLiteratureSource>,
}

impl IntegrationRegistry {
    pub fn from_settings(settings: &IntegrationsSettings) -> Self {
        let mut literature: Vec<RegisteredLiteratureSource> = vec![
            RegisteredLiteratureSource {
                enabled: settings.semantic_scholar.enabled,
                source: Arc::new(semantic_scholar::SemanticScholarClient::from_settings(
                    &settings.semantic_scholar,
                )),
            },
            RegisteredLiteratureSource {
                enabled: settings.openalex.enabled,
                source: Arc::new(openalex::OpenAlexClient::from_settings(&settings.openalex)),
            },
        ];

        for config in &settings.custom {
            match custom::CustomSource::from_config(config) {
                // Only a manifest that declares the capability becomes a
                // literature entry, so "unknown provider" and "provider cannot
                // search" stay one already-handled case rather than two.
                Ok(source) if source.declares_search() => {
                    literature.push(RegisteredLiteratureSource {
                        enabled: config.enabled,
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
    use crate::types::{OpenAlexSettings, SemanticScholarSettings};

    fn settings(openalex: bool, semantic_scholar: bool) -> IntegrationsSettings {
        IntegrationsSettings {
            openalex: OpenAlexSettings {
                enabled: openalex,
                ..Default::default()
            },
            semantic_scholar: SemanticScholarSettings {
                enabled: semantic_scholar,
                ..Default::default()
            },
            ..Default::default()
        }
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
}
