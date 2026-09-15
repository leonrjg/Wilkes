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

use serde::Serialize;

use wilkes_core::integrations::IntegrationRegistry;
use wilkes_core::types::{
    IntegrationsSettings, LiteratureSearchResult, ResolvedLiteratureDownload,
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

pub async fn search_all(
    settings: &IntegrationsSettings,
    query: String,
    limit: Option<usize>,
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
    // Spawned together and awaited in registry order: the requests run at
    // once, and the answer lists providers in the same order every time
    // regardless of which replied first.
    let pending: Vec<_> = registry
        .literature_sources()
        .filter(|entry| entry.enabled)
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

#[cfg(test)]
mod tests {
    use super::*;

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
        )
        .await
        .expect("nothing enabled is a state the pane presents, not a failure");
        assert_eq!(response.query, "graph theory");
        assert!(response.providers.is_empty());
    }

    #[tokio::test]
    async fn an_empty_query_is_refused() {
        let error = search_all(&IntegrationsSettings::default(), "   ".into(), None)
            .await
            .expect_err("empty query");
        assert!(error.0.contains("empty"), "{error}");
    }

    #[tokio::test]
    async fn an_out_of_range_limit_is_refused_rather_than_clamped() {
        for limit in [0, MAX_LIMIT + 1] {
            let error = search_all(&IntegrationsSettings::default(), "q".into(), Some(limit))
                .await
                .expect_err("out of range limit");
            assert!(error.0.contains(&limit.to_string()), "{error}");
        }
    }

    #[tokio::test]
    async fn resolving_a_builtin_result_returns_its_direct_url_without_downloading() {
        let mut settings = IntegrationsSettings::default();
        settings.openalex.enabled = true;
        let resolved = resolve_download(&settings, "openalex".to_string(), direct_result())
            .await
            .unwrap();
        assert_eq!(resolved.url, "https://example.test/paper.pdf");
        assert!(resolved.filename.is_none());
    }
}
