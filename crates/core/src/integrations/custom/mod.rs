//! A literature provider the user described rather than one we compiled.
//!
//! [`CustomSource`] implements [`LiteratureSource`] by reading a [`Manifest`]:
//! it builds a URL from a template, sends the identification the manifest
//! declares, and projects the response through [`selector`] and [`coerce`].
//! Nothing about it is privileged or special-cased — the registry holds it
//! next to `OpenAlexClient`, and callers cannot tell them apart.
//!
//! # The host is pinned
//!
//! Every request is `base_url` plus a capability's path, and the assembled URL
//! is checked to still have `base_url`'s origin before it is sent. A template
//! therefore cannot redirect a request — with a `//evil.test` path, an
//! `@`-in-userinfo trick, or a scheme change — to anywhere the user did not
//! agree to when importing the manifest. That check is the reason paths are
//! concatenated and then re-parsed rather than trusted.

pub mod coerce;
pub mod manifest;
pub mod selector;

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use self::coerce::Projected;
use self::manifest::{search_field_type, FieldSpec, Manifest, SearchCapability};
use self::selector::Selector;
use crate::integrations::LiteratureSource;
use crate::network::{ProviderHttpClient, ProviderHttpErrorKind};
use crate::types::{
    CustomIntegrationConfig, IntegrationState, IntegrationStatus, LiteratureSearchResult,
};

/// How much of a probe's raw response is shown back to the user. Enough to see
/// the shape of a record and the key names around it; not the whole page.
const PROBE_BODY_CHARS: usize = 8_000;

/// The query a probe searches for. Fixed rather than user-supplied so a probe
/// is reproducible and so an empty-result probe means the projection is wrong,
/// not that the user picked an obscure term.
const PROBE_QUERY: &str = "graph neural networks";
const PROBE_LIMIT: usize = 3;

pub struct CustomSource {
    /// Namespaced `custom:<manifest id>`, so a manifest can never shadow a
    /// built-in and no log line is ambiguous about which kind it names.
    id: String,
    manifest: Manifest,
    /// The base with any trailing slash removed, for concatenating a
    /// capability's path onto. Kept separately from `base_url` because a
    /// `Url` re-adds the slash when it is printed, and `base + "/works"` would
    /// then request `//works` — a different path that a server is entitled to
    /// treat as a different resource.
    base: String,
    /// The same base as a parsed URL, for the origin check and for
    /// `absolute_url` to resolve against.
    base_url: Url,
    secrets: HashMap<String, String>,
    http: ProviderHttpClient,
}

impl CustomSource {
    pub fn from_config(config: &CustomIntegrationConfig) -> anyhow::Result<Self> {
        let manifest = Manifest::parse(&config.manifest)?;
        anyhow::ensure!(
            manifest.id == config.id,
            "stored id '{}' does not match the manifest's id '{}'",
            config.id,
            manifest.id
        );
        Self::new(manifest, config.secrets.clone())
    }

    pub fn new(manifest: Manifest, secrets: HashMap<String, String>) -> anyhow::Result<Self> {
        manifest.validate()?;
        let base = manifest.http.base_url.trim_end_matches('/').to_string();
        let base_url = Url::parse(&base)?;
        Ok(Self {
            id: format!("custom:{}", manifest.id),
            http: ProviderHttpClient::new(manifest.name.clone()),
            manifest,
            base,
            base_url,
            secrets,
        })
    }

    pub fn declares_search(&self) -> bool {
        self.manifest.capabilities.search.is_some()
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Run the search capability once against a fixed query and report what
    /// happened at every stage.
    ///
    /// The probe exists because a manifest cannot be checked by reading it: a
    /// selector is only right about a response that has arrived. Enablement is
    /// gated on this (see `custom-integrations.md` §6), and it reports every
    /// unresolved field by name — a mapping tool that silently nulled them
    /// would be a guessing tool.
    pub async fn probe(&self) -> ProbeReport {
        let Some(search) = &self.manifest.capabilities.search else {
            return ProbeReport::failed(
                &self.id,
                "search",
                String::new(),
                "manifest declares no search capability".to_string(),
            );
        };

        let request = match self.request(&substitute(
            &search.path,
            &[("query", PROBE_QUERY), ("limit", &PROBE_LIMIT.to_string())],
        )) {
            Ok(request) => request,
            Err(error) => {
                return ProbeReport::failed(&self.id, "search", String::new(), error.to_string())
            }
        };
        let redacted = request.redacted_url.clone();

        let body = match self.http.get_bytes(request.url, &request.headers).await {
            Ok(body) => body,
            Err(error) => {
                return ProbeReport::failed(&self.id, "search", redacted, error.to_string())
            }
        };
        let raw = String::from_utf8_lossy(&body);
        let raw_preview: String = raw.chars().take(PROBE_BODY_CHARS).collect();

        let value: Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(error) => {
                let mut report = ProbeReport::failed(
                    &self.id,
                    "search",
                    redacted,
                    format!("response is not JSON: {error}"),
                );
                report.raw_response = raw_preview;
                return report;
            }
        };

        match self.project(search, &value) {
            Ok(projection) => {
                let records_seen = projection.records_seen;
                let issues_empty = projection.issues.is_empty();
                ProbeReport {
                    id: self.id.clone(),
                    capability: "search".to_string(),
                    request_url: redacted,
                    raw_response: raw_preview,
                    results: projection.results,
                    issues: projection.issues,
                    // Clean means: records arrived, and every value that was
                    // present was usable. A probe that maps nothing, or that maps
                    // some fields and reports the rest, is a failed probe even
                    // though every request succeeded — the manifest is not yet
                    // usable, and calling that "ok" is what would let a broken
                    // provider be enabled.
                    ok: records_seen > 0 && issues_empty,
                    error: None,
                }
            }
            Err(error) => ProbeReport::failed(&self.id, "search", redacted, error.to_string()),
        }
    }

    fn project(&self, search: &SearchCapability, body: &Value) -> anyhow::Result<Projection> {
        let items = match &search.items {
            Some(path) => {
                let selector = Selector::parse(path).map_err(|e| anyhow::anyhow!("items: {e}"))?;
                selector.resolve(body).ok_or_else(|| {
                    anyhow::anyhow!("items selector '{path}' matched nothing in the response")
                })?
            }
            None => body,
        };
        let items = items.as_array().ok_or_else(|| {
            anyhow::anyhow!("items must select an array; found {}", describe_kind(items))
        })?;

        let mut results = Vec::with_capacity(items.len());
        let mut issues = Vec::new();
        for (index, item) in items.iter().enumerate() {
            match self.project_one(search, item, index, &mut issues) {
                Some(result) => results.push(result),
                None => issues.push(ProjectionIssue {
                    record: index,
                    field: "id".to_string(),
                    selector: String::new(),
                    problem: "record skipped: without an id it cannot be identified or downloaded"
                        .to_string(),
                }),
            }
        }

        Ok(Projection {
            records_seen: items.len(),
            results,
            issues,
        })
    }

    fn project_one(
        &self,
        search: &SearchCapability,
        item: &Value,
        index: usize,
        issues: &mut Vec<ProjectionIssue>,
    ) -> Option<LiteratureSearchResult> {
        let mut text: HashMap<&str, String> = HashMap::new();
        let mut integer: HashMap<&str, i64> = HashMap::new();
        let mut boolean: HashMap<&str, bool> = HashMap::new();

        for (field, spec) in &search.fields {
            // Unknown field names are refused when the manifest is saved, so
            // one here means a manifest that reached storage another way.
            let Some(field_type) = search_field_type(field) else {
                continue;
            };
            match self.select(spec, item, field_type) {
                Ok(Some(Projected::Text(value))) => {
                    text.insert(field_as_static(field), value);
                }
                Ok(Some(Projected::Integer(value))) => {
                    integer.insert(field_as_static(field), value);
                }
                Ok(Some(Projected::Boolean(value))) => {
                    boolean.insert(field_as_static(field), value);
                }
                Ok(None) => {}
                Err(problem) => issues.push(ProjectionIssue {
                    record: index,
                    field: field.clone(),
                    selector: spec.paths().join(" | "),
                    problem,
                }),
            }
        }

        let id = text.remove("id").filter(|value| !value.trim().is_empty())?;
        Some(LiteratureSearchResult {
            id,
            doi: text.remove("doi"),
            title: text.remove("title"),
            year: integer.remove("year"),
            publication_date: text.remove("publication_date"),
            venue: text.remove("venue"),
            citation_count: integer.remove("citation_count").unwrap_or(0),
            is_open_access: boolean.remove("is_open_access").unwrap_or(false),
            pdf_url: text.remove("pdf_url"),
            landing_page_url: text.remove("landing_page_url"),
            open_access_status: text.remove("open_access_status"),
            license: text.remove("license"),
        })
    }

    /// Try a field's selectors in order. `Ok(None)` is an absent value, which
    /// is ordinary; `Err` is a value that was there and could not be used,
    /// which is not.
    fn select(
        &self,
        spec: &FieldSpec,
        item: &Value,
        field_type: manifest::FieldType,
    ) -> Result<Option<Projected>, String> {
        let mut last_error = None;
        for path in spec.paths() {
            let selector = match Selector::parse(path) {
                Ok(selector) => selector,
                Err(error) => return Err(error),
            };
            let Some(raw) = selector.resolve(item) else {
                continue;
            };
            match coerce::project(spec.coercion(), field_type, raw, &self.base_url) {
                Ok(value) => return Ok(Some(value)),
                Err(mismatch) => last_error = Some(format!("{path}: {mismatch}")),
            }
        }
        match last_error {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    /// Assemble one request: the URL with its declared query parameters, the
    /// headers, and a copy of the URL with secrets removed for display.
    fn request(&self, path: &str) -> anyhow::Result<PreparedRequest<'_>> {
        let mut url = Url::parse(&format!("{}{path}", self.base))?;
        anyhow::ensure!(
            url.origin() == self.base_url.origin(),
            "path '{path}' would send the request to {} instead of {}",
            url.origin().ascii_serialization(),
            self.base_url.origin().ascii_serialization()
        );

        let mut headers = Vec::new();
        let mut redacted_pairs: Vec<(String, String)> = Vec::new();
        for param in &self.manifest.http.params {
            let (value, secret) = match (&param.value, &param.secret) {
                (Some(value), None) => (value.clone(), false),
                (None, Some(name)) => (
                    self.secrets
                        .get(name)
                        .filter(|value| !value.trim().is_empty())
                        .cloned()
                        .ok_or_else(|| {
                            anyhow::anyhow!("secret '{name}' has no value; set it in Integrations")
                        })?,
                    true,
                ),
                // Refused when the manifest is validated.
                _ => anyhow::bail!(
                    "param '{}' must set exactly one of value or secret",
                    param.name
                ),
            };
            match param.location {
                manifest::ParamLocation::Header => headers.push((param.name.as_str(), value)),
                manifest::ParamLocation::Query => {
                    url.query_pairs_mut().append_pair(&param.name, &value);
                    redacted_pairs.push((
                        param.name.clone(),
                        if secret { "***".to_string() } else { value },
                    ));
                }
            }
        }

        let mut redacted = url.clone();
        if !redacted_pairs.is_empty() {
            let kept: Vec<(String, String)> = url
                .query_pairs()
                .filter(|(name, _)| !redacted_pairs.iter().any(|(secret, _)| secret == name))
                .map(|(name, value)| (name.into_owned(), value.into_owned()))
                .collect();
            redacted.query_pairs_mut().clear();
            for (name, value) in kept.into_iter().chain(redacted_pairs) {
                redacted.query_pairs_mut().append_pair(&name, &value);
            }
        }

        Ok(PreparedRequest {
            url: url.to_string(),
            redacted_url: redacted.to_string(),
            headers,
        })
    }
}

/// Header names borrow from the manifest rather than being cloned: the
/// manifest outlives every request built from it, and `ProviderHttpClient`
/// takes its header names by reference.
struct PreparedRequest<'a> {
    url: String,
    redacted_url: String,
    headers: Vec<(&'a str, String)>,
}

struct Projection {
    records_seen: usize,
    results: Vec<LiteratureSearchResult>,
    issues: Vec<ProjectionIssue>,
}

/// One field of one record that could not be mapped.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectionIssue {
    /// Index of the record in the response, so a user can find it in the raw
    /// body shown beside the report.
    pub record: usize,
    pub field: String,
    pub selector: String,
    pub problem: String,
}

/// What one run of a capability produced, for the user to check before
/// enabling the integration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProbeReport {
    pub id: String,
    pub capability: String,
    /// The URL that was requested, with secret parameter values replaced.
    pub request_url: String,
    /// The first [`PROBE_BODY_CHARS`] characters of the response.
    pub raw_response: String,
    pub results: Vec<LiteratureSearchResult>,
    pub issues: Vec<ProjectionIssue>,
    pub ok: bool,
    pub error: Option<String>,
}

impl ProbeReport {
    fn failed(id: &str, capability: &str, request_url: String, error: String) -> Self {
        Self {
            id: id.to_string(),
            capability: capability.to_string(),
            request_url,
            raw_response: String::new(),
            results: Vec::new(),
            issues: Vec::new(),
            ok: false,
            error: Some(error),
        }
    }
}

#[async_trait]
impl LiteratureSource for CustomSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.manifest.name
    }

    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<LiteratureSearchResult>> {
        let query = query.trim();
        anyhow::ensure!(
            !query.is_empty(),
            "{} search query cannot be empty",
            self.manifest.name
        );
        let search = self
            .manifest
            .capabilities
            .search
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("{} cannot search", self.manifest.name))?;
        let limit = limit.clamp(1, 100);

        let request = self.request(&substitute(
            &search.path,
            &[("query", query), ("limit", &limit.to_string())],
        ))?;
        let body = match self.http.get_bytes(request.url, &request.headers).await {
            Ok(body) => body,
            Err(error) if error.kind == ProviderHttpErrorKind::RateLimited => {
                anyhow::bail!("{} rate limit reached", self.manifest.name)
            }
            Err(error) => return Err(error.into()),
        };
        let value: Value = serde_json::from_slice(&body).map_err(|e| {
            anyhow::anyhow!("{} returned a non-JSON response: {e}", self.manifest.name)
        })?;

        let projection = self.project(search, &value)?;
        // A selector that stopped matching is the service having changed shape
        // under a manifest that was probed against the old one. It is not
        // fatal — the other fields are still right — but it is never silent.
        if !projection.issues.is_empty() {
            let mut fields: Vec<&str> = projection
                .issues
                .iter()
                .map(|issue| issue.field.as_str())
                .collect();
            fields.sort_unstable();
            fields.dedup();
            tracing::warn!(
                integration = %self.id,
                records = projection.records_seen,
                unmapped = projection.issues.len(),
                "custom integration could not map fields: {}",
                fields.join(", ")
            );
        }
        Ok(projection.results)
    }

    async fn status(&self, enabled: bool) -> anyhow::Result<IntegrationStatus> {
        if !enabled {
            return Ok(IntegrationStatus {
                id: self.id.clone(),
                enabled,
                state: IntegrationState::Disabled,
                message: format!("{} integration is disabled.", self.manifest.name),
                version: None,
            });
        }

        let Some(health) = &self.manifest.capabilities.health else {
            return Ok(IntegrationStatus {
                id: self.id.clone(),
                enabled,
                state: IntegrationState::Ready,
                message: format!(
                    "{} declares no health check, so it is used without probing.",
                    self.manifest.name
                ),
                version: None,
            });
        };

        let request = self.request(&health.path)?;
        match self.http.get_status(request.url, &request.headers).await {
            Ok(_) => Ok(IntegrationStatus {
                id: self.id.clone(),
                enabled,
                state: IntegrationState::Ready,
                message: format!("{} is reachable.", self.manifest.name),
                version: None,
            }),
            Err(error) if error.kind == ProviderHttpErrorKind::RateLimited => {
                Ok(IntegrationStatus {
                    id: self.id.clone(),
                    enabled,
                    state: IntegrationState::RateLimited,
                    message: format!(
                        "{} is reachable, but the rate limit is currently reached.",
                        self.manifest.name
                    ),
                    version: None,
                })
            }
            Err(error) => Ok(IntegrationStatus {
                id: self.id.clone(),
                enabled,
                state: IntegrationState::RemoteApiDown,
                message: error.to_string(),
                version: None,
            }),
        }
    }
}

/// Replace `{name}` placeholders with percent-encoded values.
///
/// The engine encodes, never the manifest author: a template says *where* a
/// value goes and the substitution decides *how*, so no manifest can produce a
/// malformed or injected query by forgetting to escape one. Placeholders the
/// capability does not supply are refused when the manifest is validated, so
/// anything left unreplaced here is a literal brace the service asked for.
fn substitute(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in values {
        out = out.replace(
            &format!("{{{name}}}"),
            &urlencoding::encode(value).into_owned(),
        );
    }
    out
}

fn describe_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// The result-field names are a compile-time list, so a name that matched one
/// can be re-borrowed as `'static` for the projection maps.
fn field_as_static(field: &str) -> &'static str {
    manifest::SEARCH_FIELDS
        .iter()
        .find(|(name, _)| *name == field)
        .map(|(name, _)| *name)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::openalex::OpenAlexClient;
    use crate::integrations::semantic_scholar::SemanticScholarClient;

    /// OpenAlex's search projection, said instead of written.
    ///
    /// Every field maps except one: `is_open_access` is `open_access.is_oa ||
    /// best_oa_location.is_oa` in the Rust client, and `first_of` is *first
    /// present*, not *or*. See `divergence_on_a_disjunction` — the projection
    /// deliberately has no boolean combinator, and this is the one place the
    /// built-in needs one.
    const OPENALEX_MANIFEST: &str = r#"
manifest_version = 1
id = "openalex-as-manifest"
name = "OpenAlex (manifest)"

[http]
base_url = "BASE_URL"

[capabilities.search]
path = "/works?search={query}&per-page={limit}&select=id,doi,display_name,publication_year,publication_date,cited_by_count,ids,primary_location,best_oa_location,open_access"
items = "results[*]"

[capabilities.search.fields]
id = "id"
title = "display_name"
doi = { path = "ids.doi", coerce = "normalize_doi" }
year = "publication_year"
publication_date = "publication_date"
venue = "primary_location.source.display_name"
citation_count = "cited_by_count"
is_open_access = { first_of = ["open_access.is_oa", "best_oa_location.is_oa"] }
pdf_url = "best_oa_location.pdf_url"
landing_page_url = { first_of = ["best_oa_location.landing_page_url", "open_access.oa_url"] }
open_access_status = "open_access.oa_status"
license = "best_oa_location.license"
"#;

    const SEMANTIC_SCHOLAR_MANIFEST: &str = r#"
manifest_version = 1
id = "s2-as-manifest"
name = "Semantic Scholar (manifest)"

[http]
base_url = "BASE_URL"

[[http.params]]
location = "header"
name = "x-api-key"
secret = "api_key"

[capabilities.search]
path = "/graph/v1/paper/search?query={query}&limit={limit}&fields=title,citationCount,externalIds,year,venue,publicationDate,isOpenAccess,openAccessPdf"
items = "data[*]"

[capabilities.search.fields]
id = "paperId"
title = "title"
doi = "externalIds.DOI"
year = "year"
publication_date = "publicationDate"
venue = "venue"
citation_count = "citationCount"
is_open_access = "isOpenAccess"
pdf_url = "openAccessPdf.url"
open_access_status = "openAccessPdf.status"
license = "openAccessPdf.license"
"#;

    /// The fixture bodies are the ones the built-in clients' own tests use, so
    /// the two implementations are being asked about identical bytes.
    const OPENALEX_BODY: &str = r#"{"results":[{"id":"https://openalex.org/W1","display_name":"T","ids":{"doi":"https://doi.org/10.1/example"},"open_access":{"is_oa":true,"oa_status":"gold","oa_url":"https://example.test/article"},"best_oa_location":{"is_oa":true,"pdf_url":"https://example.test/paper.pdf","landing_page_url":"https://example.test/article","license":"cc-by"}}]}"#;
    const SEMANTIC_SCHOLAR_BODY: &str = r#"{"data":[{"paperId":"p1","externalIds":{"DOI":"10.1/example"},"title":"T","citationCount":3,"isOpenAccess":true,"openAccessPdf":{"url":"https://example.test/paper.pdf","status":"GOLD","license":"CCBY"}}]}"#;

    fn source(manifest: &str, base_url: &str, secrets: &[(&str, &str)]) -> CustomSource {
        let manifest = Manifest::parse(&manifest.replace("BASE_URL", base_url)).unwrap();
        CustomSource::new(
            manifest,
            secrets
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
        .unwrap()
    }

    /// The proof the design asked for: if a manifest cannot reproduce what the
    /// Rust client makes of the same bytes, the projection vocabulary is
    /// wrong, and it is cheaper to learn that here than after a UI exists.
    #[tokio::test]
    async fn manifest_reproduces_the_openalex_client_on_its_own_fixture() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(mockito::Matcher::Any)
            .expect(2)
            .with_status(200)
            .with_body(OPENALEX_BODY)
            .create_async()
            .await;

        let built_in = OpenAlexClient::new(server.url(), None)
            .search("graph neural networks", 2)
            .await
            .unwrap();
        let projected = source(OPENALEX_MANIFEST, &server.url(), &[])
            .search("graph neural networks", 2)
            .await
            .unwrap();

        assert_eq!(projected, built_in);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_reproduces_the_semantic_scholar_client_on_its_own_fixture() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/search")
            .match_query(mockito::Matcher::Any)
            .match_header("x-api-key", "secret")
            .expect(2)
            .with_status(200)
            .with_body(SEMANTIC_SCHOLAR_BODY)
            .create_async()
            .await;

        let built_in = SemanticScholarClient::new(server.url(), Some("secret".into()))
            .search("graph neural networks", 2)
            .await
            .unwrap();
        let projected = source(
            SEMANTIC_SCHOLAR_MANIFEST,
            &server.url(),
            &[("api_key", "secret")],
        )
        .search("graph neural networks", 2)
        .await
        .unwrap();

        assert_eq!(projected, built_in);
        mock.assert_async().await;
    }

    /// The one thing the projection cannot say, pinned so it is not
    /// rediscovered as a bug.
    ///
    /// `first_of` returns the first selector that *resolves*; OpenAlex's
    /// `is_open_access` is a disjunction over two fields that are both always
    /// present. Where they disagree, the manifest reports the first and the
    /// Rust client reports their `||`. Fixing it means a boolean combinator in
    /// the vocabulary, which is a deliberate decision and not a quiet one.
    #[tokio::test]
    async fn divergence_on_a_disjunction() {
        let body = r#"{"results":[{"id":"W1","display_name":"T","ids":{},"open_access":{"is_oa":false},"best_oa_location":{"is_oa":true,"pdf_url":"https://example.test/p.pdf"}}]}"#;
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(mockito::Matcher::Any)
            .expect(2)
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let built_in = OpenAlexClient::new(server.url(), None)
            .search("q", 1)
            .await
            .unwrap();
        let projected = source(OPENALEX_MANIFEST, &server.url(), &[])
            .search("q", 1)
            .await
            .unwrap();

        assert!(built_in[0].is_open_access, "|| of false and true is true");
        assert!(
            !projected[0].is_open_access,
            "first_of takes the first present value, which is false"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn a_secret_is_required_before_a_request_is_made() {
        let error = source(SEMANTIC_SCHOLAR_MANIFEST, "https://example.test", &[])
            .search("q", 1)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("secret 'api_key'"), "{error}");
    }

    /// The host is pinned by two independent things, and this checks both.
    ///
    /// Validation refuses a path that does not start with `/`, which is what
    /// stops `@evil.test/` from being read as userinfo and moving the host.
    /// Concatenation then keeps a `//evil.test` path a *path* — the origin
    /// check in `request` is the backstop that would catch it if either of
    /// those ever stopped holding.
    #[tokio::test]
    async fn the_host_is_pinned_against_a_path_that_tries_to_move_it() {
        let escaping = OPENALEX_MANIFEST.replace(
            "path = \"/works?search={query}",
            "path = \"@evil.test/works?search={query}",
        );
        let error = Manifest::parse(&escaping.replace("BASE_URL", "https://api.openalex.org"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("must start with '/'"), "{error}");

        let doubled = OPENALEX_MANIFEST.replace(
            "path = \"/works?search={query}",
            "path = \"//evil.test/works?search={query}",
        );
        let pinned = source(&doubled, "https://api.openalex.org", &[]);
        let request = pinned.request("//evil.test/works").unwrap();
        assert!(
            request
                .url
                .starts_with("https://api.openalex.org//evil.test/works"),
            "{}",
            request.url
        );
    }

    #[tokio::test]
    async fn probe_reports_an_unmapped_field_by_name_and_refuses_to_pass() {
        // `citationCount` arrives as a string where the manifest promised an
        // integer: present, unusable, and reported rather than nulled.
        let body = r#"{"data":[{"paperId":"p1","title":"T","citationCount":"many"}]}"#;
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/search")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let report = source(
            SEMANTIC_SCHOLAR_MANIFEST,
            &server.url(),
            &[("api_key", "secret")],
        )
        .probe()
        .await;

        assert!(!report.ok, "a probe with unmapped fields is not clean");
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].field, "citation_count");
        assert!(report.issues[0].problem.contains("integer"));
        // The record still projected: the failure is per field, not per record.
        assert_eq!(report.results.len(), 1);
        assert!(report.raw_response.contains("many"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn probe_redacts_a_secret_sent_as_a_query_parameter() {
        let manifest =
            SEMANTIC_SCHOLAR_MANIFEST.replace(r#"location = "header""#, r#"location = "query""#);
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/search")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(SEMANTIC_SCHOLAR_BODY)
            .create_async()
            .await;

        let report = source(&manifest, &server.url(), &[("api_key", "hunter2")])
            .probe()
            .await;

        assert!(report.ok, "{:?}", report.error);
        assert!(
            !report.request_url.contains("hunter2"),
            "{}",
            report.request_url
        );
        assert!(report.request_url.contains("***"), "{}", report.request_url);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn a_record_without_an_id_is_skipped_and_said_so() {
        let body = r#"{"data":[{"title":"no id here"},{"paperId":"p2","title":"T"}]}"#;
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/search")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let report = source(
            SEMANTIC_SCHOLAR_MANIFEST,
            &server.url(),
            &[("api_key", "secret")],
        )
        .probe()
        .await;

        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].id, "p2");
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.record == 0 && issue.problem.contains("skipped")));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn an_items_selector_that_matches_nothing_names_itself() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/search")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"{"papers":[]}"#)
            .create_async()
            .await;

        let report = source(
            SEMANTIC_SCHOLAR_MANIFEST,
            &server.url(),
            &[("api_key", "secret")],
        )
        .probe()
        .await;

        assert!(!report.ok);
        let error = report.error.unwrap();
        assert!(error.contains("data[*]"), "{error}");
        mock.assert_async().await;
    }
}
