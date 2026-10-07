//! One query against every literature provider the user has switched on.
//!
//! The MCP tool asks one named provider, because an agent can choose which to
//! ask and pay for one request at a time. A person typing into the search pane
//! has no reason to know which index holds the paper, so this asks all of them
//! at once and says, per provider, what each answered — a provider that is
//! down or rate-limited is reported beside the ones that did answer rather
//! than failing the search or vanishing from it.
//!
//! Resolution is the registry's, exactly as the MCP tool's is: the id a
//! provider is reported under here is the id that tool accepts.

use std::sync::{Arc, Mutex};

use serde::Serialize;

use wilkes_core::integrations::custom::manifest::SecretSlot;
use wilkes_core::integrations::{builtin, IntegrationRegistry, ProviderCapabilities};
use wilkes_core::metadata::cache::MetadataCache;
use wilkes_core::metadata::doi::normalize_doi;
use wilkes_core::types::{
    BrowseRequest, IntegrationStatus, IntegrationsSettings, LiteratureSearchResult, ProviderWork,
    ResolvedLiteratureDownload,
};

/// Works per provider when the caller does not say. The pane lists every
/// provider's answer in turn, so this is per section, not a total.
pub const DEFAULT_LIMIT: usize = 10;

/// Most any one provider is asked for. The same ceiling the MCP tool clamps to.
pub const MAX_LIMIT: usize = 100;

/// What one provider answered.
///
/// Exactly one of `results` and `error` is present. An empty `results` is an
/// answer — the provider holds nothing on the query — and is not the same as
/// an error that prevented it from saying.
#[derive(Clone, Debug, Serialize)]
pub struct LiteratureProviderAnswer {
    pub provider: String,
    pub name: String,
    pub results: Option<Vec<LiteratureSearchResult>>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LiteratureSearchResponse {
    pub query: String,
    /// One entry per enabled provider, in registry order. Empty means no
    /// provider is enabled, which the caller presents as a setting to change
    /// rather than as "nothing found".
    pub providers: Vec<LiteratureProviderAnswer>,
}

/// One literature provider as the registry holds it, for a caller that has to
/// offer them before it has asked anything.
///
/// Named from the registry rather than rebuilt from settings, because whether
/// a user-described manifest can search at all is the registry's answer: a
/// caller that recomputed it would be a second place that could disagree
/// about which providers exist.
///
/// Everything the interface needs to render a provider — what it is called,
/// what it can do, which secrets it asks for — is read off its manifest here,
/// so the interface holds nothing about any particular provider.
#[derive(Clone, Debug, Serialize)]
pub struct LiteratureProviderInfo {
    pub provider: String,
    pub name: String,
    pub enabled: bool,
    /// Shipped with Wilkes rather than written by the user.
    pub builtin: bool,
    /// The origin requests go to now.
    pub base_url: String,
    /// The origin the shipped manifest declares, for a built-in. A base URL
    /// equal to it is no override.
    pub default_base_url: Option<String>,
    pub capabilities: ProviderCapabilities,
    /// The values this provider asks the user for (an email address, an API
    /// key), each optional or required.
    pub secrets: Vec<SecretSlot>,
}

/// Every provider this installation knows, in registry order, each saying
/// whether the user has it switched on. Disabled ones are listed rather than
/// dropped: a caller filtering by provider needs to tell *switched off* from
/// *does not exist*, which are different mistakes with different fixes.
pub fn providers(settings: &IntegrationsSettings) -> Vec<LiteratureProviderInfo> {
    let shipped = builtin::manifests();
    IntegrationRegistry::from_settings(settings)
        .literature_sources()
        .map(|entry| LiteratureProviderInfo {
            provider: entry.id().to_string(),
            name: entry.name().to_string(),
            enabled: entry.enabled,
            builtin: entry.builtin,
            base_url: entry.manifest().http.base_url.clone(),
            default_base_url: entry
                .builtin
                .then(|| {
                    shipped
                        .iter()
                        .find(|manifest| manifest.id == entry.id())
                        .map(|manifest| manifest.http.base_url.clone())
                })
                .flatten(),
            capabilities: entry.source().capabilities(),
            secrets: entry.manifest().secret_slots(),
        })
        .collect()
}

/// Why the search could not be run at all. A provider that fails is not this:
/// it is reported inside the response.
#[derive(Debug)]
pub struct LiteratureRequestError(pub String);

impl std::fmt::Display for LiteratureRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for LiteratureRequestError {}

/// One query against the enabled providers, or against the named subset of
/// them.
///
/// `providers` is a filter the caller asked for, not a hint: a provider left
/// out of it is not asked, which is the point — these are rate-limited
/// services on the other side of the network, and a pane that narrowed its
/// results after the fact would have spent the request anyway. `None` asks
/// every enabled provider.
pub async fn search_all(
    settings: &IntegrationsSettings,
    query: String,
    limit: Option<usize>,
    providers: Option<Vec<String>>,
) -> Result<LiteratureSearchResponse, LiteratureRequestError> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err(LiteratureRequestError(
            "Literature search query cannot be empty.".into(),
        ));
    }
    let limit = match limit {
        None => DEFAULT_LIMIT,
        Some(limit) if (1..=MAX_LIMIT).contains(&limit) => limit,
        Some(limit) => {
            return Err(LiteratureRequestError(format!(
                "Literature search limit {limit} is outside 1-{MAX_LIMIT}"
            )))
        }
    };

    let registry = IntegrationRegistry::from_settings(settings);
    // A named filter is checked whole before anything is asked, so a caller
    // does not get half an answer before being told it named a provider that
    // does not exist. An empty list is refused rather than read as "all":
    // "ask nobody" is the caller's own decision not to search, and a request
    // that meant it would not have been made.
    if let Some(selected) = providers.as_deref() {
        if selected.is_empty() {
            return Err(LiteratureRequestError(
                "Literature search names no providers.".into(),
            ));
        }
        if let Some(unknown) = selected
            .iter()
            .find(|id| registry.literature(id.as_str()).is_none())
        {
            return Err(LiteratureRequestError(format!(
                "Unknown literature provider {unknown:?}; known providers are {}",
                registry.literature_ids().join(", ")
            )));
        }
    }
    // Spawned together and awaited in registry order: the requests run at
    // once, and the answer lists providers in the same order every time
    // regardless of which replied first.
    let pending: Vec<_> = registry
        .literature_sources()
        .filter(|entry| entry.enabled)
        .filter(|entry| match providers.as_deref() {
            None => true,
            Some(selected) => selected.iter().any(|id| id == entry.id()),
        })
        .map(|entry| {
            let source = std::sync::Arc::clone(entry.source());
            let query = query.clone();
            (
                entry.id().to_string(),
                entry.name().to_string(),
                tokio::spawn(async move { source.search(&query, limit).await }),
            )
        })
        .collect();

    let mut providers = Vec::with_capacity(pending.len());
    for (provider, name, handle) in pending {
        let outcome = match handle.await {
            Ok(Ok(results)) => Ok(results),
            Ok(Err(error)) => Err(format!("{error:#}")),
            Err(error) => Err(format!("{name} search task failed: {error}")),
        };
        match outcome {
            Ok(results) => providers.push(LiteratureProviderAnswer {
                provider,
                name,
                results: Some(results),
                error: None,
            }),
            Err(error) => {
                tracing::warn!(provider = %provider, "literature search failed: {error}");
                providers.push(LiteratureProviderAnswer {
                    provider,
                    name,
                    results: None,
                    error: Some(error),
                });
            }
        }
    }
    Ok(LiteratureSearchResponse { query, providers })
}

/// Resolve one selected result on demand. This never downloads the file: the
/// returned plan goes through the catalogue downloader, keeping one owner for
/// filesystem writes and their size/duplicate checks.
pub async fn resolve_download(
    settings: &IntegrationsSettings,
    provider: String,
    result: LiteratureSearchResult,
) -> Result<ResolvedLiteratureDownload, LiteratureRequestError> {
    let registry = IntegrationRegistry::from_settings(settings);
    let source = registry
        .literature_for_search(&provider)
        .map_err(|error| LiteratureRequestError(error.to_string()))?;
    source
        .resolve_download(&result)
        .await
        .map_err(|error| LiteratureRequestError(error.to_string()))?
        .ok_or_else(|| {
            LiteratureRequestError(format!(
                "{} did not provide a downloadable file for result '{}'",
                source.name(),
                result.id
            ))
        })
}

/// A provider's listing by filter and sort, answered in the same shape as one
/// provider's part of a search.
///
/// A request the provider's manifest does not allow (an undeclared filter, a
/// value outside its options) is an error *in the answer*, beside the
/// provider's name, like a provider that is down: the caller has one place to
/// look.
pub async fn browse(
    settings: &IntegrationsSettings,
    provider: String,
    request: BrowseRequest,
) -> Result<LiteratureProviderAnswer, LiteratureRequestError> {
    let registry = IntegrationRegistry::from_settings(settings);
    let source = registry
        .literature_for_search(&provider)
        .map_err(|error| LiteratureRequestError(error.to_string()))?;
    let name = source.name().to_string();
    Ok(match source.browse(&request).await {
        Ok(results) => LiteratureProviderAnswer {
            provider,
            name,
            results: Some(results),
            error: None,
        },
        Err(error) => {
            tracing::warn!(provider = %provider, "literature browse failed: {error:#}");
            LiteratureProviderAnswer {
                provider,
                name,
                results: None,
                error: Some(format!("{error:#}")),
            }
        }
    })
}

/// Whether a provider is reachable and usable now.
///
/// Answers for a provider whether or not it is enabled: the settings form asks
/// before switching it on. One question for every provider, shipped or
/// pasted in, so there is one place that says how it is asked.
pub async fn status(
    settings: &IntegrationsSettings,
    provider: &str,
) -> anyhow::Result<IntegrationStatus> {
    let registry = IntegrationRegistry::from_settings(settings);
    let entry = registry.literature(provider).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown literature provider '{provider}'; known providers are {}",
            registry.literature_ids().join(", ")
        )
    })?;
    entry.source().status(entry.enabled).await
}

/// One provider's record of a work, from the metadata cache when it holds one
/// and from the provider when it does not.
pub async fn lookup(
    settings: &IntegrationsSettings,
    cache: Option<Arc<Mutex<MetadataCache>>>,
    provider: &str,
    doi: &str,
) -> anyhow::Result<ProviderWork> {
    let registry = IntegrationRegistry::from_settings(settings);
    let source = registry.literature_for_search(provider)?;
    let doi = normalize_doi(doi).ok_or_else(|| anyhow::anyhow!("Invalid DOI: {doi}"))?;

    let cache = cache.ok_or_else(|| anyhow::anyhow!("The metadata cache is unavailable"))?;
    {
        let guard = cache
            .lock()
            .map_err(|_| anyhow::anyhow!("The metadata cache lock failed"))?;
        if let Some(cached) = guard.get_provider_by_doi(provider, &doi)? {
            return Ok(cached);
        }
    }

    let work = source.lookup(&doi).await?;

    {
        let guard = cache
            .lock()
            .map_err(|_| anyhow::anyhow!("The metadata cache lock failed"))?;
        guard.upsert_provider_by_doi(provider, &work)?;
    }
    Ok(work)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wilkes_core::types::BuiltinProviderState;

    fn enabled(ids: &[&str]) -> IntegrationsSettings {
        let mut settings = IntegrationsSettings::default();
        for id in ids {
            settings.builtin.insert(
                id.to_string(),
                BuiltinProviderState {
                    enabled: true,
                    ..Default::default()
                },
            );
        }
        settings
    }

    fn direct_result() -> LiteratureSearchResult {
        LiteratureSearchResult {
            id: "W1".to_string(),
            doi: None,
            title: Some("A paper".to_string()),
            year: None,
            publication_date: None,
            venue: None,
            citation_count: 0,
            is_open_access: true,
            pdf_url: Some("https://example.test/paper.pdf".to_string()),
            landing_page_url: None,
            open_access_status: None,
            license: None,
            authors: None,
            publisher: None,
            language: None,
            file_format: None,
            file_size: None,
            acquisition: wilkes_core::types::LiteratureAcquisition::Direct,
        }
    }

    #[tokio::test]
    async fn no_enabled_provider_is_an_empty_answer_not_an_error() {
        let response = search_all(
            &IntegrationsSettings::default(),
            "graph theory".into(),
            None,
            None,
        )
        .await
        .expect("nothing enabled is a state the pane presents, not a failure");
        assert_eq!(response.query, "graph theory");
        assert!(response.providers.is_empty());
    }

    #[tokio::test]
    async fn an_empty_query_is_refused() {
        let error = search_all(&IntegrationsSettings::default(), "   ".into(), None, None)
            .await
            .expect_err("empty query");
        assert!(error.0.contains("empty"), "{error}");
    }

    #[tokio::test]
    async fn an_out_of_range_limit_is_refused_rather_than_clamped() {
        for limit in [0, MAX_LIMIT + 1] {
            let error = search_all(
                &IntegrationsSettings::default(),
                "q".into(),
                Some(limit),
                None,
            )
            .await
            .expect_err("out of range limit");
            assert!(error.0.contains(&limit.to_string()), "{error}");
        }
    }

    #[tokio::test]
    async fn a_filter_naming_an_unknown_provider_is_refused_by_name() {
        let error = search_all(
            &IntegrationsSettings::default(),
            "q".into(),
            None,
            Some(vec!["openalex".into(), "custom:gone".into()]),
        )
        .await
        .expect_err("unknown provider");
        assert!(error.0.contains("custom:gone"), "{error}");
        assert!(error.0.contains("openalex"), "{error}");
    }

    /// Asking nobody is the caller deciding not to search, not a search.
    #[tokio::test]
    async fn an_empty_filter_is_refused_rather_than_read_as_every_provider() {
        let error = search_all(
            &IntegrationsSettings::default(),
            "q".into(),
            None,
            Some(vec![]),
        )
        .await
        .expect_err("empty filter");
        assert!(error.0.contains("no providers"), "{error}");
    }

    /// A provider left out of the filter is not asked at all, which is the
    /// only reason the filter is worth passing down rather than applying to
    /// the answer.
    #[tokio::test]
    async fn an_unselected_provider_is_not_asked() {
        let mut settings = enabled(&["openalex", "semantic_scholar"]);
        // Both point nowhere: what is asserted is which providers were asked,
        // and a test that reached the real services to find that out would be
        // egress this crate's tests do not perform. Whichever is asked fails
        // to connect and is reported as an entry, which is what is counted.
        settings.builtin.get_mut("openalex").unwrap().base_url =
            Some("http://127.0.0.1:1/openalex".into());
        settings
            .builtin
            .get_mut("semantic_scholar")
            .unwrap()
            .base_url = Some("http://127.0.0.1:1/s2".into());
        let response = search_all(
            &settings,
            "graph theory".into(),
            None,
            Some(vec!["openalex".into()]),
        )
        .await
        .expect("a filtered search still runs");
        let asked: Vec<&str> = response
            .providers
            .iter()
            .map(|entry| entry.provider.as_str())
            .collect();
        assert_eq!(asked, vec!["openalex"]);
    }

    /// Disabled providers are listed, not dropped: a stale selection naming
    /// one must read as "switched off", not as "does not exist".
    #[test]
    fn the_provider_list_names_disabled_providers_too() {
        let settings = enabled(&["openalex"]);
        let listed = providers(&settings);
        let openalex = listed
            .iter()
            .find(|entry| entry.provider == "openalex")
            .expect("openalex is shipped");
        assert!(openalex.enabled);
        let semantic = listed
            .iter()
            .find(|entry| entry.provider == "semantic_scholar")
            .expect("a disabled provider is still listed");
        assert!(!semantic.enabled);
        assert!(!semantic.name.is_empty());
    }

    #[tokio::test]
    async fn resolving_a_builtin_result_returns_its_direct_url_without_downloading() {
        let settings = enabled(&["openalex"]);
        let resolved = resolve_download(&settings, "openalex".to_string(), direct_result())
            .await
            .unwrap();
        assert_eq!(resolved.url, "https://example.test/paper.pdf");
        assert!(resolved.filename.is_none());
    }

    #[test]
    fn the_provider_list_carries_what_the_interface_renders() {
        let listed = providers(&IntegrationsSettings::default());
        let openalex = listed.iter().find(|p| p.provider == "openalex").unwrap();
        assert!(openalex.builtin);
        assert_eq!(openalex.base_url, "https://api.openalex.org");
        assert_eq!(
            openalex.default_base_url.as_deref(),
            Some("https://api.openalex.org")
        );
        assert!(openalex.capabilities.browse.is_some());
        assert_eq!(openalex.secrets.len(), 1);
        assert_eq!(openalex.secrets[0].name, "email");
        assert!(openalex.secrets[0].optional);

        let semantic = listed
            .iter()
            .find(|p| p.provider == "semantic_scholar")
            .unwrap();
        assert_eq!(semantic.secrets[0].name, "api_key");
        assert!(semantic.capabilities.browse.is_none());
    }

    #[test]
    fn a_base_url_override_is_what_the_list_reports_and_the_default_stays_the_shipped_one() {
        let mut settings = enabled(&["openalex"]);
        settings.builtin.get_mut("openalex").unwrap().base_url =
            Some("https://mirror.example.test".into());
        let listed = providers(&settings);
        let openalex = listed.iter().find(|p| p.provider == "openalex").unwrap();
        assert_eq!(openalex.base_url, "https://mirror.example.test");
        assert_eq!(
            openalex.default_base_url.as_deref(),
            Some("https://api.openalex.org")
        );
    }

    #[tokio::test]
    async fn status_names_an_unknown_provider() {
        let error = status(&IntegrationsSettings::default(), "crossref")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Unknown literature provider 'crossref'"),
            "{error}"
        );
        assert!(error.contains("openalex"), "{error}");
    }

    #[tokio::test]
    async fn status_probes_a_disabled_provider_so_it_can_be_checked_before_enabling() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body("{}")
            .create_async()
            .await;
        let mut settings = IntegrationsSettings::default();
        settings.builtin.insert(
            "openalex".into(),
            BuiltinProviderState {
                enabled: false,
                base_url: Some(server.url()),
                ..Default::default()
            },
        );
        let status = status(&settings, "openalex").await.unwrap();
        assert_eq!(status.state, wilkes_core::types::IntegrationState::Ready);
        assert!(!status.enabled);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn lookup_rejects_a_disabled_provider() {
        let error = lookup(
            &IntegrationsSettings::default(),
            None,
            "openalex",
            "10.1145/3801158",
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("disabled"), "{error}");
    }

    #[tokio::test]
    async fn browse_reports_a_refused_request_in_the_answer_beside_the_providers_name() {
        let settings = enabled(&["openalex"]);
        let answer = browse(
            &settings,
            "openalex".into(),
            BrowseRequest {
                filters: [("nope".to_string(), "1".to_string())].into(),
                ..BrowseRequest::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(answer.provider, "openalex");
        assert_eq!(answer.name, "OpenAlex");
        assert!(answer.results.is_none());
        assert!(answer.error.unwrap().contains("'nope' is not a filter"));
    }

    #[tokio::test]
    async fn browse_names_a_provider_that_is_off_or_unknown() {
        let error = browse(
            &IntegrationsSettings::default(),
            "openalex".into(),
            BrowseRequest::default(),
        )
        .await
        .unwrap_err();
        assert!(error.0.contains("disabled"), "{error}");
        let error = browse(
            &IntegrationsSettings::default(),
            "nowhere".into(),
            BrowseRequest::default(),
        )
        .await
        .unwrap_err();
        assert!(error.0.contains("Unknown literature provider"), "{error}");
    }
}
