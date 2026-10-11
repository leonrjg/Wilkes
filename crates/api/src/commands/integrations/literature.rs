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

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use wilkes_core::catalogue::{mirrored_sources, CatalogueStore};
use wilkes_core::generate::tasks::browse_request::compose_browse_request;
use wilkes_core::generate::Generator;
use wilkes_core::integrations::custom::manifest::SecretSlot;
use wilkes_core::integrations::custom::manifest::MIN_SUGGEST_CHARS;
use wilkes_core::integrations::{builtin, IntegrationRegistry, ProviderCapabilities};
use wilkes_core::metadata::cache::MetadataCache;
use wilkes_core::metadata::doi::normalize_doi;
use wilkes_core::types::{
    BrowseRequest, FilterValue, IntegrationStatus, IntegrationsSettings, LiteratureSearchResult,
    MirrorStatus, ProviderWork, ResolvedLiteratureDownload,
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
    /// How many records match in all, for a listing whose provider says. A
    /// keyword search does not carry one.
    pub total: Option<u64>,
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
    /// What the catalogue mirror holds for a provider answered from it; null
    /// for one that is asked over the network. `enabled` follows from it: a
    /// mirrored provider is on once it has been fetched.
    pub mirror: Option<MirrorStatus>,
}

/// Every provider this installation knows, in registry order, each saying
/// whether the user has it switched on. Disabled ones are listed rather than
/// dropped: a caller filtering by provider needs to tell *switched off* from
/// *does not exist*, which are different mistakes with different fixes.
pub fn providers(
    settings: &IntegrationsSettings,
    mirror_dir: Option<&Path>,
) -> Vec<LiteratureProviderInfo> {
    let shipped = builtin::manifests();
    let mirror = mirror_dir.map(mirror_statuses).unwrap_or_default();
    IntegrationRegistry::from_settings(settings, mirror_dir)
        .literature_sources()
        .map(|entry| LiteratureProviderInfo {
            provider: entry.id().to_string(),
            name: entry.name().to_string(),
            enabled: entry.enabled,
            builtin: entry.builtin,
            base_url: entry.source().base_url().to_string(),
            // Only a shipped manifest has an origin the user may move.
            default_base_url: entry.manifest().filter(|_| entry.builtin).and_then(|_| {
                shipped
                    .iter()
                    .find(|manifest| manifest.id == entry.id())
                    .map(|manifest| manifest.http.base_url.clone())
            }),
            capabilities: entry.source().capabilities(),
            secrets: entry
                .manifest()
                .map(|manifest| manifest.secret_slots())
                .unwrap_or_default(),
            mirror: mirror
                .iter()
                .find(|(id, _)| id == entry.id())
                .map(|(_, status)| status.clone()),
        })
        .collect()
}

/// What the mirror holds for each catalogue, by provider id.
///
/// A mirror that cannot be read reports every catalogue as never fetched,
/// which is what the registry will have concluded about them too.
fn mirror_statuses(mirror_dir: &Path) -> Vec<(String, MirrorStatus)> {
    let held = match CatalogueStore::open(mirror_dir).and_then(|store| store.holdings()) {
        Ok(held) => held,
        Err(error) => {
            tracing::error!("catalogue mirror is unreadable: {error:#}");
            Vec::new()
        }
    };
    mirrored_sources(mirror_dir)
        .iter()
        .map(|source| {
            let id = wilkes_core::integrations::LiteratureSource::id(source);
            let holding = held.iter().find(|held| held.provider == id);
            (
                id.to_string(),
                MirrorStatus {
                    grain: source.grain(),
                    records: holding.map_or(0, |held| held.records),
                    synced_at_ms: holding.and_then(|held| held.synced_at_ms),
                },
            )
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
    mirror_dir: Option<&Path>,
    query: String,
    limit: Option<usize>,
    providers: Option<Vec<String>>,
    page: Option<usize>,
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

    let page = match page {
        None => 1,
        Some(0) => {
            return Err(LiteratureRequestError(
                "Literature search page is 1-based; 0 is not a page".into(),
            ))
        }
        Some(page) => page,
    };

    let registry = IntegrationRegistry::from_settings(settings, mirror_dir);
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
                tokio::spawn(async move { source.search(&query, limit, page).await }),
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
                total: None,
            }),
            Err(error) => {
                tracing::warn!(provider = %provider, "literature search failed: {error}");
                providers.push(LiteratureProviderAnswer {
                    provider,
                    name,
                    results: None,
                    error: Some(error),
                    total: None,
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
    mirror_dir: Option<&Path>,
    provider: String,
    result: LiteratureSearchResult,
) -> Result<ResolvedLiteratureDownload, LiteratureRequestError> {
    let registry = IntegrationRegistry::from_settings(settings, mirror_dir);
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
    mirror_dir: Option<&Path>,
    provider: String,
    request: BrowseRequest,
) -> Result<LiteratureProviderAnswer, LiteratureRequestError> {
    let registry = IntegrationRegistry::from_settings(settings, mirror_dir);
    let source = registry
        .literature_for_search(&provider)
        .map_err(|error| LiteratureRequestError(error.to_string()))?;
    let name = source.name().to_string();
    Ok(match source.browse(&request).await {
        Ok(page) => LiteratureProviderAnswer {
            provider,
            name,
            results: Some(page.results),
            error: None,
            total: page.total,
        },
        Err(error) => {
            tracing::warn!(provider = %provider, "literature browse failed: {error:#}");
            LiteratureProviderAnswer {
                provider,
                name,
                results: None,
                error: Some(format!("{error:#}")),
                total: None,
            }
        }
    })
}

/// What a provider suggests for one of its filters as the user types.
///
/// Unlike a listing, a failure here is the request's own: there is one
/// provider and one filter, and nothing else in the answer to keep.
pub async fn suggest(
    settings: &IntegrationsSettings,
    mirror_dir: Option<&Path>,
    provider: String,
    filter: String,
    text: String,
) -> Result<Vec<wilkes_core::types::FilterSuggestion>, LiteratureRequestError> {
    let registry = IntegrationRegistry::from_settings(settings, mirror_dir);
    let source = registry
        .literature_for_search(&provider)
        .map_err(|error| LiteratureRequestError(error.to_string()))?;
    source.suggest(&filter, &text).await.map_err(|error| {
        tracing::warn!(provider = %provider, filter = %filter, "filter suggestion failed: {error:#}");
        LiteratureRequestError(format!("{error:#}"))
    })
}

/// What a request in words came to for one provider: the filters and the sort
/// to set on it, in the shape the filter panel holds them.
///
/// Exactly one of `filters`-and-`sort` and `error` says something. A provider
/// that could not be composed for is reported beside those that could, like a
/// provider that fails a search.
#[derive(Clone, Debug, Serialize)]
pub struct ComposedListing {
    pub provider: String,
    pub name: String,
    /// Filter name → its values. Every one has passed the provider's own
    /// check, so setting them is setting something the listing accepts.
    pub filters: BTreeMap<String, Vec<String>>,
    /// Filter name → value → what the value is called, for a value a person
    /// cannot read: the author an identifier names.
    pub labels: BTreeMap<String, BTreeMap<String, String>>,
    pub sort: Option<String>,
    /// What was asked for and left out, each in a sentence: a name the
    /// provider does not know, a value its declaration refuses.
    pub notes: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LiteratureComposition {
    /// The request as it was composed from.
    pub text: String,
    /// The words to search for beside the filters: the first provider's, since
    /// the pane has one search box and every listing shares it.
    pub keyword: Option<String>,
    pub providers: Vec<ComposedListing>,
}

/// Turns a request written in ordinary words into filters on each named
/// provider.
///
/// The model writes filter names and values in a grammar built from the
/// provider's own declaration, never a request; what it wrote is then settled
/// here — a name the provider must be asked about is looked up through
/// [`LiteratureSource::suggest`], and every filter is put to
/// [`LiteratureSource::check_browse`] before it is returned, so nothing comes
/// back that the listing would refuse. A filter that fails either is dropped
/// with a note saying so; the rest stand.
///
/// One provider at a time, because each has its own filters and there is one
/// model. `cancel` is read at every decoded token and between providers.
///
/// [`LiteratureSource::suggest`]: wilkes_core::integrations::LiteratureSource::suggest
/// [`LiteratureSource::check_browse`]: wilkes_core::integrations::LiteratureSource::check_browse
pub async fn compose(
    settings: &IntegrationsSettings,
    mirror_dir: Option<&Path>,
    generator: Arc<dyn Generator>,
    text: String,
    providers: Vec<String>,
    today: &str,
    cancel: Arc<AtomicBool>,
) -> Result<LiteratureComposition, LiteratureRequestError> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(LiteratureRequestError(
            "There is no request to compose a search from.".into(),
        ));
    }
    if providers.is_empty() {
        return Err(LiteratureRequestError(
            "Composing a search names no providers.".into(),
        ));
    }
    let registry = IntegrationRegistry::from_settings(settings, mirror_dir);
    // Checked whole before the model is asked anything, as a search's filter is.
    let mut sources = Vec::with_capacity(providers.len());
    for provider in providers {
        let source = registry
            .literature_for_search(&provider)
            .map_err(|error| LiteratureRequestError(error.to_string()))?;
        sources.push((provider, Arc::clone(source)));
    }

    let cancelled = || LiteratureRequestError("Composing the search was cancelled.".into());
    let mut keyword = None;
    let mut listings = Vec::with_capacity(sources.len());
    for (provider, source) in sources {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let name = source.name().to_string();
        let composed = compose_for(&source, &generator, &text, today, &cancel).await;
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        listings.push(match composed {
            Ok((words, mut listing)) => {
                if keyword.is_none() {
                    keyword = words;
                }
                listing.provider = provider;
                listing
            }
            Err(error) => {
                tracing::warn!(provider = %provider, "composing a search failed: {error:#}");
                ComposedListing {
                    provider,
                    name,
                    filters: BTreeMap::new(),
                    labels: BTreeMap::new(),
                    sort: None,
                    notes: Vec::new(),
                    error: Some(format!("{error:#}")),
                }
            }
        });
    }
    Ok(LiteratureComposition {
        text,
        keyword,
        providers: listings,
    })
}

/// One provider's composition: its keyword, and the filters that stood.
async fn compose_for(
    source: &Arc<dyn wilkes_core::integrations::LiteratureSource>,
    generator: &Arc<dyn Generator>,
    text: &str,
    today: &str,
    cancel: &Arc<AtomicBool>,
) -> anyhow::Result<(Option<String>, ComposedListing)> {
    let name = source.name().to_string();
    let controls = source
        .capabilities()
        .browse
        .ok_or_else(|| anyhow::anyhow!("{name} declares nothing to filter or sort by"))?;

    let composition = {
        let generator = Arc::clone(generator);
        let controls = controls.clone();
        let text = text.to_string();
        let today = today.to_string();
        let cancel = Arc::clone(cancel);
        tokio::task::spawn_blocking(move || {
            compose_browse_request(generator.as_ref(), &controls, &text, &today, &mut |_| {
                if cancel.load(Ordering::Relaxed) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })
        })
        .await
        .map_err(|error| anyhow::anyhow!("composing task failed: {error}"))??
    };

    let mut listing = ComposedListing {
        provider: String::new(),
        name: name.clone(),
        filters: BTreeMap::new(),
        labels: BTreeMap::new(),
        sort: None,
        notes: composition.notes,
        error: None,
    };
    // What has stood so far, as the request the listing would be asked with.
    // Each addition is checked against it, so one refused filter costs that
    // filter and says why, rather than refusing the whole composition.
    let mut standing = BrowseRequest {
        query: composition.keyword.clone().filter(|_| controls.keyword),
        ..BrowseRequest::default()
    };
    for (filter, raw) in composition.filters {
        let control = controls
            .filters
            .iter()
            .find(|control| control.name == filter);
        let label = control.map_or(filter.as_str(), |control| control.label.as_str());
        let mut values = Vec::with_capacity(raw.len());
        if control.is_some_and(|control| control.input == "suggest") {
            // The model wrote a name; the filter takes the provider's
            // identifier for it. Asking is the provider's own typeahead.
            for words in raw {
                if cancel.load(Ordering::Relaxed) {
                    anyhow::bail!("composing the search was cancelled");
                }
                if words.chars().count() < MIN_SUGGEST_CHARS {
                    listing
                        .notes
                        .push(format!("“{words}” is too short to look up as {label}"));
                    continue;
                }
                match source.suggest(&filter, &words).await {
                    Ok(found) => match found.into_iter().next() {
                        Some(suggestion) => {
                            listing
                                .labels
                                .entry(filter.clone())
                                .or_default()
                                .insert(suggestion.value.clone(), suggestion.label);
                            values.push(suggestion.value);
                        }
                        None => listing.notes.push(format!(
                            "{name} knows no {label} called “{words}”, so it was left out"
                        )),
                    },
                    Err(error) => {
                        tracing::warn!(filter = %filter, "looking up a composed filter value failed: {error:#}");
                        listing.notes.push(format!(
                            "{label} “{words}” could not be looked up: {error:#}"
                        ));
                    }
                }
            }
        } else {
            values = raw;
        }
        if values.is_empty() {
            continue;
        }
        let mut candidate = standing.clone();
        candidate
            .filters
            .insert(filter.clone(), FilterValue::Many(values.clone()));
        match source.check_browse(&candidate) {
            Ok(()) => {
                standing = candidate;
                listing.filters.insert(filter, values);
            }
            Err(error) => {
                listing.labels.remove(&filter);
                listing
                    .notes
                    .push(format!("{label} was left out: {error:#}"));
            }
        }
    }
    if let Some(sort) = composition.sort {
        let mut candidate = standing.clone();
        candidate.sort = Some(sort.clone());
        match source.check_browse(&candidate) {
            Ok(()) => listing.sort = Some(sort),
            Err(error) => listing
                .notes
                .push(format!("The order was left out: {error:#}")),
        }
    }
    Ok((standing.query, listing))
}

/// Whether a provider is reachable and usable now.
///
/// Answers for a provider whether or not it is enabled: the settings form asks
/// before switching it on. One question for every provider, shipped or
/// pasted in, so there is one place that says how it is asked.
pub async fn status(
    settings: &IntegrationsSettings,
    mirror_dir: Option<&Path>,
    provider: &str,
) -> anyhow::Result<IntegrationStatus> {
    let registry = IntegrationRegistry::from_settings(settings, mirror_dir);
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
    mirror_dir: Option<&Path>,
    cache: Option<Arc<Mutex<MetadataCache>>>,
    provider: &str,
    doi: &str,
) -> anyhow::Result<ProviderWork> {
    let registry = IntegrationRegistry::from_settings(settings, mirror_dir);
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
            citation_count: None,
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
            summary: None,
            subject: None,
            kind: None,
            pages: None,
            recall_score: None,
            acquisition: wilkes_core::types::LiteratureAcquisition::Direct,
        }
    }

    #[tokio::test]
    async fn no_enabled_provider_is_an_empty_answer_not_an_error() {
        let response = search_all(
            &IntegrationsSettings::default(),
            None,
            "graph theory".into(),
            None,
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
        let error = search_all(
            &IntegrationsSettings::default(),
            None,
            "   ".into(),
            None,
            None,
            None,
        )
        .await
        .expect_err("empty query");
        assert!(error.0.contains("empty"), "{error}");
    }

    #[tokio::test]
    async fn an_out_of_range_limit_is_refused_rather_than_clamped() {
        for limit in [0, MAX_LIMIT + 1] {
            let error = search_all(
                &IntegrationsSettings::default(),
                None,
                "q".into(),
                Some(limit),
                None,
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
            None,
            "q".into(),
            None,
            Some(vec!["openalex".into(), "custom:gone".into()]),
            None,
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
            None,
            "q".into(),
            None,
            Some(vec![]),
            None,
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
            None,
            "graph theory".into(),
            None,
            Some(vec!["openalex".into()]),
            None,
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
    fn textbook(id: &str, title: &str) -> wilkes_core::types::CatalogueRecord {
        wilkes_core::types::CatalogueRecord {
            provider: "libretexts".into(),
            external_id: id.into(),
            title: title.into(),
            summary: String::new(),
            subject: String::new(),
            authors: String::new(),
            license: String::new(),
            landing_url: None,
            pdf_url: None,
            outline_url: None,
            grain: wilkes_core::types::CatalogueGrain::Textbook,
            pages: None,
        }
    }

    #[tokio::test]
    async fn a_fetched_catalogue_answers_beside_the_other_providers_and_an_unfetched_one_is_not_asked(
    ) {
        let dir = tempfile::tempdir().unwrap();
        CatalogueStore::open(dir.path())
            .unwrap()
            .replace_provider("libretexts", &[textbook("a", "Graph Theory")])
            .unwrap();

        let response = search_all(
            &IntegrationsSettings::default(),
            Some(dir.path()),
            "graph theory".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            response
                .providers
                .iter()
                .map(|entry| entry.provider.as_str())
                .collect::<Vec<_>>(),
            vec!["libretexts"]
        );
        let results = response.providers[0].results.as_ref().unwrap();
        assert_eq!(results[0].title.as_deref(), Some("Graph Theory"));
        assert_eq!(results[0].kind.as_deref(), Some("textbook"));
    }

    #[test]
    fn the_provider_list_says_what_the_mirror_holds_for_each_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        CatalogueStore::open(dir.path())
            .unwrap()
            .replace_provider("libretexts", &[textbook("a", "Graph Theory")])
            .unwrap();

        let listed = providers(&IntegrationsSettings::default(), Some(dir.path()));
        let held = listed
            .iter()
            .find(|info| info.provider == "libretexts")
            .unwrap();
        assert!(held.enabled && held.capabilities.mirrored);
        let mirror = held.mirror.as_ref().unwrap();
        assert_eq!(mirror.records, 1);
        assert!(mirror.synced_at_ms.is_some());
        // No manifest, so nothing to configure: no secrets, no origin to move.
        assert!(held.secrets.is_empty() && held.default_base_url.is_none());

        let unfetched = listed
            .iter()
            .find(|info| info.provider == "devdocs")
            .unwrap();
        assert!(!unfetched.enabled);
        assert_eq!(
            unfetched.mirror,
            Some(MirrorStatus {
                grain: wilkes_core::types::CatalogueGrain::Reference,
                records: 0,
                synced_at_ms: None,
            })
        );
        // A provider asked over the network has no mirror to report.
        assert!(listed
            .iter()
            .filter(|info| !info.capabilities.mirrored)
            .all(|info| info.mirror.is_none()));
    }

    #[test]
    fn the_provider_list_names_disabled_providers_too() {
        let settings = enabled(&["openalex"]);
        let listed = providers(&settings, None);
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
        let resolved = resolve_download(&settings, None, "openalex".to_string(), direct_result())
            .await
            .unwrap();
        assert_eq!(resolved.url, "https://example.test/paper.pdf");
        assert!(resolved.filename.is_none());
    }

    #[test]
    fn the_provider_list_carries_what_the_interface_renders() {
        let listed = providers(&IntegrationsSettings::default(), None);
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
        let listed = providers(&settings, None);
        let openalex = listed.iter().find(|p| p.provider == "openalex").unwrap();
        assert_eq!(openalex.base_url, "https://mirror.example.test");
        assert_eq!(
            openalex.default_base_url.as_deref(),
            Some("https://api.openalex.org")
        );
    }

    #[tokio::test]
    async fn status_names_an_unknown_provider() {
        let error = status(&IntegrationsSettings::default(), None, "crossref")
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
        let status = status(&settings, None, "openalex").await.unwrap();
        assert_eq!(status.state, wilkes_core::types::IntegrationState::Ready);
        assert!(!status.enabled);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn lookup_rejects_a_disabled_provider() {
        let error = lookup(
            &IntegrationsSettings::default(),
            None,
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
            None,
            "openalex".into(),
            BrowseRequest {
                filters: [(
                    "nope".to_string(),
                    wilkes_core::types::FilterValue::One("1".to_string()),
                )]
                .into(),
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
            None,
            "openalex".into(),
            BrowseRequest::default(),
        )
        .await
        .unwrap_err();
        assert!(error.0.contains("disabled"), "{error}");
        let error = browse(
            &IntegrationsSettings::default(),
            None,
            "nowhere".into(),
            BrowseRequest::default(),
        )
        .await
        .unwrap_err();
        assert!(error.0.contains("Unknown literature provider"), "{error}");
    }

    // ── Composing a search from a request in words ───────────────────────────

    use wilkes_core::generate::mock::MockGenerator;

    /// OpenAlex switched on and pointed at `origin`, so a suggestion lookup
    /// has somewhere to go and a test can see whether it went.
    fn openalex_at(origin: &str) -> IntegrationsSettings {
        let mut settings = enabled(&["openalex", "semantic_scholar"]);
        settings.builtin.get_mut("openalex").unwrap().base_url = Some(origin.to_string());
        settings
    }

    async fn composed(
        settings: &IntegrationsSettings,
        generator: &Arc<MockGenerator>,
        providers: &[&str],
    ) -> Result<LiteratureComposition, LiteratureRequestError> {
        compose(
            settings,
            None,
            Arc::clone(generator) as Arc<dyn Generator>,
            "open reviews on CRISPR by Jennifer Doudna since 2020, most cited first".into(),
            providers.iter().map(|id| id.to_string()).collect(),
            "2026-10-11",
            Arc::new(AtomicBool::new(false)),
        )
        .await
    }

    #[tokio::test]
    async fn a_request_in_words_becomes_filters_the_provider_accepts() {
        let mut server = mockito::Server::new_async().await;
        let lookup = server
            .mock("GET", "/autocomplete/authors")
            .match_query(mockito::Matcher::UrlEncoded(
                "q".into(),
                "Jennifer Doudna".into(),
            ))
            .with_status(200)
            .with_body(
                r#"{"results":[{"id":"https://openalex.org/A5001","display_name":"Jennifer A. Doudna","hint":"UC Berkeley"},{"id":"https://openalex.org/A5002","display_name":"J. Doudna","hint":null}]}"#,
            )
            .expect(1)
            .create_async()
            .await;
        let generator = Arc::new(MockGenerator::scripted([
            "keyword: crispr\nfilter: type = review\nfilter: open_access\nfilter: year_from = 2020\nfilter: author = Jennifer Doudna\nsort: cited_by_count:desc\n",
        ]));

        let composition = composed(&openalex_at(&server.url()), &generator, &["openalex"])
            .await
            .unwrap();
        lookup.assert_async().await;

        assert_eq!(composition.keyword.as_deref(), Some("crispr"));
        let listing = &composition.providers[0];
        assert_eq!(listing.provider, "openalex");
        assert_eq!(listing.error, None);
        assert_eq!(listing.filters["type"], vec!["review"]);
        assert_eq!(listing.filters["open_access"], vec!["true"]);
        assert_eq!(listing.filters["year_from"], vec!["2020"]);
        // The name became the provider's identifier, and is still called what
        // the provider calls it.
        assert_eq!(
            listing.filters["author"],
            vec!["https://openalex.org/A5001"]
        );
        assert_eq!(
            listing.labels["author"]["https://openalex.org/A5001"],
            "Jennifer A. Doudna"
        );
        assert_eq!(listing.sort.as_deref(), Some("cited_by_count:desc"));
        assert!(listing.notes.is_empty(), "{:?}", listing.notes);
        // The model was shown the request and the provider's own filters.
        let prompt = &generator.requests()[0].prompt;
        assert!(prompt.contains("Request: open reviews on CRISPR by Jennifer Doudna"));
        assert!(prompt.contains("- year_from (Year, From): a whole number"));
    }

    /// One filter the provider would refuse costs that filter and says why.
    /// The others are what the user asked for and stand.
    #[tokio::test]
    async fn a_value_the_provider_refuses_is_left_out_with_a_note_and_the_rest_stand() {
        let mut server = mockito::Server::new_async().await;
        let lookup = server
            .mock("GET", "/autocomplete/authors")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"{"results":[]}"#)
            .create_async()
            .await;
        let generator = Arc::new(MockGenerator::scripted([
            "filter: published_from = 2024-13-01\nfilter: type = review\nfilter: author = Nobody Atall\n",
        ]));

        let composition = composed(&openalex_at(&server.url()), &generator, &["openalex"])
            .await
            .unwrap();
        lookup.assert_async().await;

        let listing = &composition.providers[0];
        assert_eq!(
            listing.filters.keys().collect::<Vec<_>>(),
            vec!["type"],
            "{:?}",
            listing.notes
        );
        assert!(listing.labels.is_empty());
        assert_eq!(listing.notes.len(), 2, "{:?}", listing.notes);
        assert!(listing.notes.iter().any(|note| note.contains("YYYY-MM-DD")));
        assert!(listing
            .notes
            .iter()
            .any(|note| note.contains("knows no Author called “Nobody Atall”")));
    }

    /// A provider with nothing to filter by is reported in its own entry, and
    /// the model is not asked about it.
    #[tokio::test]
    async fn a_provider_that_declares_no_filters_is_reported_and_not_composed_for() {
        let generator = Arc::new(MockGenerator::scripted(["filter: type = review\n"]));
        let composition = composed(
            &openalex_at("http://127.0.0.1:9"),
            &generator,
            &["semantic_scholar", "openalex"],
        )
        .await
        .unwrap();
        assert_eq!(generator.request_count(), 1);
        let unfilterable = &composition.providers[0];
        assert_eq!(unfilterable.provider, "semantic_scholar");
        assert!(unfilterable
            .error
            .as_deref()
            .unwrap()
            .contains("declares nothing to filter"));
        assert_eq!(composition.providers[1].filters["type"], vec!["review"]);
    }

    #[tokio::test]
    async fn composing_for_an_unknown_provider_is_refused_before_the_model_is_asked() {
        let generator = Arc::new(MockGenerator::scripted(["filter: type = review\n"]));
        let error = composed(
            &openalex_at("http://127.0.0.1:9"),
            &generator,
            &["openalex", "crossref"],
        )
        .await
        .expect_err("unknown provider");
        assert!(error.0.contains("crossref"), "{error}");
        assert_eq!(generator.request_count(), 0);
    }

    #[tokio::test]
    async fn a_cancelled_composition_is_an_error_and_asks_nothing() {
        let generator = Arc::new(MockGenerator::scripted(["filter: type = review\n"]));
        let error = compose(
            &openalex_at("http://127.0.0.1:9"),
            None,
            Arc::clone(&generator) as Arc<dyn Generator>,
            "reviews".into(),
            vec!["openalex".into()],
            "2026-10-11",
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .expect_err("cancelled");
        assert!(error.0.contains("cancelled"), "{error}");
        assert_eq!(generator.request_count(), 0);
    }
}
