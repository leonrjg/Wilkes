//! A literature provider, described rather than compiled.
//!
//! [`CustomSource`] implements [`LiteratureSource`] by reading a [`Manifest`]:
//! it builds a URL from a template, sends the identification the manifest
//! declares, and projects JSON (by JMESPath expression) or HTML (by CSS
//! selection) through [`expr`]. It is the *only* implementation: OpenAlex and
//! Semantic Scholar are manifests embedded in the binary
//! ([`super::builtin`]), loaded through the same door as a manifest a user
//! pasted in this morning, and no caller can tell them apart.
//!
//! # Every origin is pinned
//!
//! Every request is a declared literal `base_url` plus a capability's path,
//! and the assembled URL is checked to still have that base's origin before it
//! is sent. Resolver steps may name another base, but a template cannot. Global
//! headers and query parameters do not cross that origin boundary, and the HTTP
//! client refuses cross-origin redirects. A manifest therefore cannot send a
//! request or a credential anywhere the user did not see when importing it.

pub mod expr;
pub mod legacy;
pub mod manifest;

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use dom_query::{Document, Matcher, Selection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use self::expr::{Evaluator, Projected};
use self::manifest::{
    lookup_field_type, search_field_type, BrowseCapability, FieldSpec, FieldType, FilterSpec,
    LookupCapability, Manifest, ParamValue, ReferencesCapability, ResolverStep, ResponseFormat,
    SearchCapability,
};
use crate::integrations::{LiteratureSource, ProviderCapabilities};
use crate::metadata::doi::normalize_doi;
use crate::network::{ProviderHttpClient, ProviderHttpErrorKind};
use crate::types::{
    BrowseRequest, CustomIntegrationConfig, IntegrationState, IntegrationStatus,
    LiteratureAcquisition, LiteratureSearchResult, ProviderWork, ResolvedLiteratureDownload,
};

/// How much of a probe's raw response is shown back to the user. Enough to see
/// the shape of a record and the key names around it; not the whole page.
const PROBE_BODY_CHARS: usize = 8_000;

const PROBE_LIMIT: usize = 3;

/// Most any one request is asked for. The same ceiling the MCP tool clamps to.
const MAX_LIMIT: usize = 100;

/// Whether a source was shipped with Wilkes or written by the user. It decides
/// the id and nothing else: a built-in is `openalex`, a custom provider is
/// `custom:<id>`, so a manifest can never shadow a built-in and no log line is
/// ambiguous about which kind it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    BuiltIn,
    Custom,
}

pub struct CustomSource {
    id: String,
    /// What to call this provider: the name the user gave it, otherwise the
    /// one the manifest declares. Resolved once here so that every caller —
    /// the pane's heading, a status line, the text of a failed request — says
    /// the same thing without each deciding for itself.
    display_name: String,
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
        Self::build(
            manifest,
            config.secrets.clone(),
            config.name.clone(),
            Standing::Custom,
        )
    }

    pub fn new(manifest: Manifest, secrets: HashMap<String, String>) -> anyhow::Result<Self> {
        Self::build(manifest, secrets, None, Standing::Custom)
    }

    /// The same source under a name the user chose. A blank override is no
    /// override: a cleared rename box restores the manifest's own name rather
    /// than leaving a provider with nothing to be called.
    pub fn named(
        manifest: Manifest,
        secrets: HashMap<String, String>,
        name: Option<String>,
    ) -> anyhow::Result<Self> {
        Self::build(manifest, secrets, name, Standing::Custom)
    }

    pub fn build(
        manifest: Manifest,
        secrets: HashMap<String, String>,
        name: Option<String>,
        standing: Standing,
    ) -> anyhow::Result<Self> {
        manifest.validate()?;
        let base = manifest.http.base_url.trim_end_matches('/').to_string();
        let base_url = Url::parse(&base)?;
        let display_name = name
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| manifest.name.clone());
        Ok(Self {
            id: match standing {
                Standing::BuiltIn => manifest.id.clone(),
                Standing::Custom => format!("custom:{}", manifest.id),
            },
            http: ProviderHttpClient::new_origin_pinned(display_name.clone()),
            display_name,
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

    /// Nothing derived from an external failure reaches the interface before
    /// every configured credential has been removed. The network layer hides
    /// request URLs, but response bodies and nested transport errors are not
    /// required to be as disciplined.
    fn redact_error(&self, mut message: String) -> String {
        let mut secrets: Vec<&str> = self
            .secrets
            .values()
            .map(String::as_str)
            .filter(|value| !value.is_empty())
            .collect();
        secrets.sort_unstable_by(|left, right| {
            right.len().cmp(&left.len()).then_with(|| left.cmp(right))
        });
        secrets.dedup();
        for secret in secrets {
            message = message.replace(secret, "***");
        }
        message
    }

    /// Run the search capability once against the caller's example query and
    /// report what happened at every stage.
    ///
    /// The probe exists because a manifest cannot be checked by reading it: a
    /// selector is only right about a response that has arrived. It reports
    /// every unresolved field by name — a mapping tool that silently nulled
    /// them would be a guessing tool.
    ///
    /// It is evidence and not a gate (see `custom-integrations.md` §6): a
    /// service that is down, rate-limiting or holding nothing for the chosen
    /// example query fails a probe without saying anything about whether the
    /// manifest is right, so what this finds is shown to the user rather than
    /// standing between them and saving their work.
    pub async fn probe(&self, query: &str) -> ProbeReport {
        let query = query.trim();
        if query.is_empty() {
            return ProbeReport::failed(
                &self.id,
                "search",
                String::new(),
                "probe query cannot be empty".to_string(),
            );
        }
        let Some(search) = &self.manifest.capabilities.search else {
            return ProbeReport::failed(
                &self.id,
                "search",
                String::new(),
                "manifest declares no search capability".to_string(),
            );
        };

        let request = match self.request(
            &substitute(
                &search.path,
                &[("query", query), ("limit", &PROBE_LIMIT.to_string())],
            ),
            &[],
        ) {
            Ok(request) => request,
            Err(error) => {
                return ProbeReport::failed(
                    &self.id,
                    "search",
                    String::new(),
                    self.redact_error(error.to_string()),
                )
            }
        };
        let redacted = request.redacted_url.clone();

        let body = match self.http.get_bytes(request.url, &request.headers).await {
            Ok(body) => body,
            Err(error) => {
                return ProbeReport::failed(
                    &self.id,
                    "search",
                    redacted,
                    self.redact_error(error.to_string()),
                )
            }
        };
        let raw = String::from_utf8_lossy(&body);
        let raw_preview: String = raw.chars().take(PROBE_BODY_CHARS).collect();

        match self.project_response(search, &body, query, PROBE_LIMIT) {
            Ok(projection) => {
                let records_seen = projection.records_seen;
                let issues_empty = projection.issues.is_empty();
                let resolver_error = if self.manifest.capabilities.resolve_download.is_some() {
                    match projection.results.first() {
                        Some(result) => {
                            self.resolve_manifest_download(result)
                                .await
                                .err()
                                .map(|error| {
                                    self.redact_error(format!(
                                        "resolve_download probe failed: {error:#}"
                                    ))
                                })
                        }
                        None => Some(
                            "resolve_download probe needs one successfully projected result"
                                .to_string(),
                        ),
                    }
                } else {
                    None
                };
                ProbeReport {
                    id: self.id.clone(),
                    capability: match self.manifest.capabilities.resolve_download.is_some() {
                        true => "search + resolve_download".to_string(),
                        false => "search".to_string(),
                    },
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
                    ok: records_seen > 0 && issues_empty && resolver_error.is_none(),
                    error: resolver_error,
                }
            }
            Err(error) => ProbeReport::failed(
                &self.id,
                "search",
                redacted,
                self.redact_error(error.to_string()),
            ),
        }
    }

    // ── Projection ───────────────────────────────────────────────────────────

    fn project_response(
        &self,
        search: &SearchCapability,
        body: &[u8],
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Projection> {
        match search.response_format {
            ResponseFormat::Json => {
                let value: Value = serde_json::from_slice(body)
                    .map_err(|error| anyhow::anyhow!("response is not JSON: {error}"))?;
                self.project_json(search, &value, query, limit)
            }
            ResponseFormat::Html => {
                let html = std::str::from_utf8(body)
                    .map_err(|error| anyhow::anyhow!("response is not UTF-8 HTML: {error}"))?;
                self.project_html(search, html, query, limit)
            }
        }
    }

    fn project_json(
        &self,
        search: &SearchCapability,
        body: &Value,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Projection> {
        let evaluator = Evaluator::new(&self.base_url);
        let items = records_in(&evaluator, search.items.as_deref(), body, "items")?;

        let records_seen = items.len().min(limit);
        let mut results = Vec::with_capacity(records_seen);
        let mut issues = Vec::new();
        for (index, item) in items.iter().take(limit).enumerate() {
            let fields = self.project_fields(
                &search.fields,
                search_field_type,
                Some((query, limit)),
                index,
                &mut issues,
                &evaluator,
                |spec, field_type| self.select_json(spec, item, field_type, &evaluator),
            );
            match self.search_result(fields, &search.fields, index, &mut issues) {
                Some(result) => results.push(result),
                None => note_missing_id(index, &mut issues),
            }
        }

        Ok(Projection {
            records_seen,
            results,
            issues,
        })
    }

    fn project_html(
        &self,
        search: &SearchCapability,
        html: &str,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Projection> {
        let items_path = search
            .items
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("items is required for an HTML response"))?;
        let matcher = Matcher::new(items_path)
            .map_err(|error| anyhow::anyhow!("items CSS selector is invalid: {error:?}"))?;
        let document = Document::from(html);
        let items = document.select_matcher(&matcher);
        if items.is_empty() {
            anyhow::bail!("items selector '{items_path}' matched nothing in the response");
        }
        let evaluator = Evaluator::new(&self.base_url);
        let records_seen = items.length().min(limit);
        let mut results = Vec::with_capacity(records_seen);
        let mut issues = Vec::new();
        for (index, item) in items.iter().take(limit).enumerate() {
            let fields = self.project_fields(
                &search.fields,
                search_field_type,
                Some((query, limit)),
                index,
                &mut issues,
                &evaluator,
                |spec, field_type| self.select_html(spec, &item, field_type, &evaluator),
            );
            match self.search_result(fields, &search.fields, index, &mut issues) {
                Some(result) => results.push(result),
                None => note_missing_id(index, &mut issues),
            }
        }
        Ok(Projection {
            records_seen,
            results,
            issues,
        })
    }

    /// Project every declared field of one record, recording what could not be
    /// mapped. Typed inputs (`query`, `limit`) are answered here, whatever the
    /// response format; everything else is the format's own `select`.
    #[allow(clippy::too_many_arguments)]
    fn project_fields<F>(
        &self,
        fields: &BTreeMap<String, FieldSpec>,
        type_of: fn(&str) -> Option<FieldType>,
        inputs: Option<(&str, usize)>,
        index: usize,
        issues: &mut Vec<ProjectionIssue>,
        evaluator: &Evaluator,
        mut select: F,
    ) -> Fields
    where
        F: FnMut(&FieldSpec, FieldType) -> Result<Option<Projected>, String>,
    {
        let mut out = HashMap::new();
        for (field, spec) in fields {
            // Unknown field names are refused when the manifest is saved, so
            // one here means a manifest that reached storage another way.
            let Some(field_type) = type_of(field) else {
                continue;
            };
            let selected = match (spec.input(), inputs) {
                (Some(input), Some((query, limit))) => {
                    self.select_input(spec, input, query, limit, field_type, evaluator)
                }
                (Some(input), None) => Err(format!("input '{input}' is not available here")),
                (None, _) => select(spec, field_type),
            };
            match selected {
                Ok(Some(value)) => {
                    out.insert(field.clone(), value);
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
        Fields(out)
    }

    fn search_result(
        &self,
        mut fields: Fields,
        specs: &BTreeMap<String, FieldSpec>,
        index: usize,
        issues: &mut Vec<ProjectionIssue>,
    ) -> Option<LiteratureSearchResult> {
        let id = fields.text("id").filter(|value| !value.trim().is_empty())?;
        let title = fields.text("title");
        if title.is_none() {
            issues.push(ProjectionIssue {
                record: index,
                field: "title".to_string(),
                selector: specs
                    .get("title")
                    .map(|spec| spec.paths().join(" | "))
                    .unwrap_or_default(),
                problem: "required field matched nothing".to_string(),
            });
        }
        let pdf_url = fields.text("pdf_url");
        let acquisition = if self.manifest.capabilities.resolve_download.is_some() {
            LiteratureAcquisition::Provider
        } else if pdf_url.is_some() {
            LiteratureAcquisition::Direct
        } else {
            LiteratureAcquisition::None
        };
        Some(LiteratureSearchResult {
            id,
            doi: fields.text("doi"),
            title,
            year: fields.int("year"),
            publication_date: fields.text("publication_date"),
            venue: fields.text("venue"),
            citation_count: fields.int("citation_count").unwrap_or(0),
            is_open_access: fields.boolean("is_open_access").unwrap_or(false),
            pdf_url,
            landing_page_url: fields.text("landing_page_url"),
            open_access_status: fields.text("open_access_status"),
            license: fields.text("license"),
            authors: fields.text("authors"),
            publisher: fields.text("publisher"),
            language: fields.text("language"),
            file_format: fields.text("file_format"),
            file_size: fields.text("file_size"),
            acquisition,
        })
    }

    fn select_json(
        &self,
        spec: &FieldSpec,
        item: &Value,
        field_type: FieldType,
        evaluator: &Evaluator,
    ) -> Result<Option<Projected>, String> {
        self.select_projected(spec, field_type, evaluator, |path| {
            evaluator.eval(path, item)
        })
    }

    fn select_input(
        &self,
        spec: &FieldSpec,
        input: &str,
        query: &str,
        limit: usize,
        field_type: FieldType,
        evaluator: &Evaluator,
    ) -> Result<Option<Projected>, String> {
        let raw = match input {
            "query" => Value::String(query.to_string()),
            "limit" => Value::Number(limit.into()),
            // Refused during manifest validation.
            _ => return Err(format!("unknown search input '{input}'")),
        };
        self.finish(spec, raw, field_type, evaluator)
    }

    fn select_html(
        &self,
        spec: &FieldSpec,
        item: &Selection<'_>,
        field_type: FieldType,
        evaluator: &Evaluator,
    ) -> Result<Option<Projected>, String> {
        self.select_projected(spec, field_type, evaluator, |path| {
            let selected = if path == ":scope" {
                item.clone()
            } else {
                let matcher = Matcher::new(path)
                    .map_err(|error| format!("invalid CSS selector: {error:?}"))?;
                item.select_matcher(&matcher)
            };
            if selected.is_empty() {
                return Ok(None);
            }
            if spec.all() {
                let values: Vec<Value> = selected
                    .iter()
                    .filter_map(|element| match spec.attribute() {
                        Some(attribute) => element
                            .attr(attribute)
                            .map(|value| Value::String(value.to_string())),
                        None => {
                            let value = normalize_html_text(&element.text());
                            (!value.is_empty()).then_some(Value::String(value))
                        }
                    })
                    .collect();
                return Ok((!values.is_empty()).then_some(Value::Array(values)));
            }
            let value = match spec.attribute() {
                Some(attribute) => selected.attr(attribute).map(|value| value.to_string()),
                None => {
                    let value = normalize_html_text(&selected.text());
                    (!value.is_empty()).then_some(value)
                }
            };
            Ok(value.map(Value::String))
        })
    }

    /// Try each of the field's selectors in order until one yields a value that
    /// survives the field's own post-processing.
    fn select_projected<F>(
        &self,
        spec: &FieldSpec,
        field_type: FieldType,
        evaluator: &Evaluator,
        mut raw_for_path: F,
    ) -> Result<Option<Projected>, String>
    where
        F: FnMut(&str) -> Result<Option<Value>, String>,
    {
        let mut last_error = None;
        for path in spec.paths() {
            let raw = match raw_for_path(path) {
                Ok(Some(raw)) => raw,
                Ok(None) => continue,
                // A selector that cannot be evaluated is this selector's
                // failure; a later alternative may still resolve.
                Err(problem) => {
                    last_error = Some(format!("{path}: {problem}"));
                    continue;
                }
            };
            match self.finish(spec, raw, field_type, evaluator) {
                Ok(Some(value)) => return Ok(Some(value)),
                Ok(None) => {}
                Err(problem) => last_error = Some(format!("{path}: {problem}")),
            }
        }
        last_error.map_or(Ok(None), Err)
    }

    /// What happens to a value once it has been found: a regular-expression
    /// capture, the field's `then` expression, and the strict type check.
    fn finish(
        &self,
        spec: &FieldSpec,
        mut raw: Value,
        field_type: FieldType,
        evaluator: &Evaluator,
    ) -> Result<Option<Projected>, String> {
        if let Some((pattern, group)) = spec.capture() {
            raw = capture_value(pattern, group, &raw)?;
        }
        if let Some(then) = spec.then() {
            raw = match evaluator.eval(then, &raw)? {
                Some(value) => value,
                None => return Ok(None),
            };
        }
        expr::project(field_type, &raw, &self.base_url)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    // ── Search, browse ───────────────────────────────────────────────────────

    async fn fetch(&self, request: PreparedRequest<'_>) -> anyhow::Result<Vec<u8>> {
        match self.http.get_bytes(request.url, &request.headers).await {
            Ok(body) => Ok(body),
            Err(error) if error.kind == ProviderHttpErrorKind::RateLimited => {
                anyhow::bail!("{} rate limit reached", self.manifest.name)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// A selector that stopped matching is the service having changed shape
    /// under a manifest that was probed against the old one. It is not fatal —
    /// the other fields are still right — but it is never silent.
    fn warn_unmapped(&self, projection: &Projection) {
        if projection.issues.is_empty() {
            return;
        }
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

    async fn run_search(
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
        let limit = limit.clamp(1, MAX_LIMIT);

        let request = self.request(
            &substitute(
                &search.path,
                &[("query", query), ("limit", &limit.to_string())],
            ),
            &[],
        )?;
        let body = self.fetch(request).await?;
        let projection = self.project_response(search, &body, query, limit)?;
        self.warn_unmapped(&projection);
        Ok(projection.results)
    }

    async fn run_browse(
        &self,
        request: &BrowseRequest,
    ) -> anyhow::Result<Vec<LiteratureSearchResult>> {
        let name = &self.manifest.name;
        let browse = self
            .manifest
            .capabilities
            .browse
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("{name} does not offer browse"))?;
        let search = self
            .manifest
            .capabilities
            .search
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("{name} cannot search"))?;
        let limit = request.limit.unwrap_or(10).clamp(1, MAX_LIMIT);
        let page = request.page.unwrap_or(1).max(1);
        let query = request
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty());

        let extra = browse_parameters(browse, request, query)
            .map_err(|problem| anyhow::anyhow!("{name} browse: {problem}"))?;
        let prepared = self.request(
            &substitute(
                &browse.path,
                &[("limit", &limit.to_string()), ("page", &page.to_string())],
            ),
            &extra,
        )?;
        let body = self.fetch(prepared).await?;
        let projection = self.project_response(search, &body, query.unwrap_or(""), limit)?;
        self.warn_unmapped(&projection);
        Ok(projection.results)
    }

    // ── Lookup, references ───────────────────────────────────────────────────

    async fn run_lookup(&self, doi: &str) -> anyhow::Result<ProviderWork> {
        let doi = normalize_doi(doi).ok_or_else(|| anyhow::anyhow!("Invalid DOI: {doi}"))?;
        let lookup = self.lookup_capability()?;
        let evaluator = Evaluator::new(&self.base_url);

        for path in &lookup.paths {
            let request = self.request(&substitute(path, &[("doi", &doi)]), &[])?;
            let body = match self.http.get_bytes(request.url, &request.headers).await {
                Ok(body) => body,
                // "No record under this key" — the next request may know the
                // work under another.
                Err(error) if error.kind == ProviderHttpErrorKind::NotFound => continue,
                Err(error) if error.kind == ProviderHttpErrorKind::RateLimited => {
                    anyhow::bail!("{} rate limit reached", self.manifest.name)
                }
                Err(error) => return Err(error.into()),
            };
            let value: Value = serde_json::from_slice(&body)
                .map_err(|error| anyhow::anyhow!("response is not JSON: {error}"))?;
            let record = match &lookup.record {
                Some(expression) => evaluator
                    .eval(expression, &value)
                    .map_err(|error| anyhow::anyhow!("lookup.record: {error}"))?,
                None => Some(value),
            };
            let Some(record) = record else { continue };
            return self.work_from_record(lookup, &record, &evaluator, doi);
        }
        anyhow::bail!("{} has no record for DOI {doi}", self.manifest.name)
    }

    fn work_from_record(
        &self,
        lookup: &LookupCapability,
        record: &Value,
        evaluator: &Evaluator,
        doi: String,
    ) -> anyhow::Result<ProviderWork> {
        let mut issues = Vec::new();
        let mut fields = self.project_fields(
            &lookup.fields,
            lookup_field_type,
            None,
            0,
            &mut issues,
            evaluator,
            |spec, field_type| self.select_json(spec, record, field_type, evaluator),
        );
        // A lookup is one record, and a field that is present but unusable is
        // a record the cache must not store half of.
        if let Some(issue) = issues.first() {
            anyhow::bail!(
                "{} lookup could not read '{}': {}",
                self.manifest.name,
                issue.field,
                issue.problem
            );
        }
        let work_id = fields
            .text("id")
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("{} lookup record has no id", self.manifest.name))?;
        Ok(ProviderWork {
            doi,
            work_id,
            title: fields.text("title"),
            year: fields.int("year"),
            publication_date: fields.text("publication_date"),
            venue: fields.text("venue"),
            citation_count: fields.int("citation_count").unwrap_or(0),
            external_ids: fields
                .object("external_ids")
                .unwrap_or_default()
                .into_iter()
                .collect(),
            cached_at_ms: now_ms(),
        })
    }

    fn lookup_capability(&self) -> anyhow::Result<&LookupCapability> {
        self.manifest
            .capabilities
            .lookup
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("{} does not offer lookup", self.manifest.name))
    }

    fn references_capability(&self) -> anyhow::Result<&ReferencesCapability> {
        self.manifest
            .capabilities
            .references
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("{} does not offer references", self.manifest.name))
    }

    async fn run_references(&self, doi: &str) -> anyhow::Result<Vec<String>> {
        let doi = normalize_doi(doi).ok_or_else(|| anyhow::anyhow!("Invalid DOI: {doi}"))?;
        let references = self.references_capability()?;
        let evaluator = Evaluator::new(&self.base_url);

        let work = self
            .request(&substitute(&references.path, &[("doi", &doi)]), &[])
            .map(|request| (request.url, request.headers))?;
        let body = self.fetch_raw(work.0, &work.1).await?;
        let value: Value = serde_json::from_slice(&body)
            .map_err(|error| anyhow::anyhow!("response is not JSON: {error}"))?;
        let identifiers = match evaluator
            .eval(&references.ids, &value)
            .map_err(|error| anyhow::anyhow!("references.ids: {error}"))?
        {
            None => return Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .into_iter()
                .map(|item| match item {
                    Value::String(id) => Ok(id),
                    other => Err(anyhow::anyhow!(
                        "references.ids must yield strings; found {other}"
                    )),
                })
                .collect::<anyhow::Result<Vec<String>>>()?,
            Some(other) => anyhow::bail!("references.ids must yield an array; found {other}"),
        };
        if identifiers.is_empty() {
            return Ok(Vec::new());
        }

        let resolve = &references.resolve;
        let mut dois = Vec::new();
        let mut without_doi = 0usize;
        for chunk in identifiers.chunks(resolve.chunk) {
            // Each identifier is encoded; the separator is the service's OR
            // operator and stays literal.
            let joined = chunk
                .iter()
                .map(|id| urlencoding::encode(id).into_owned())
                .collect::<Vec<_>>()
                .join(&resolve.join);
            let path = resolve
                .path
                .replace("{ids}", &joined)
                .replace("{count}", &chunk.len().to_string());
            let request = self.request(&path, &[]).map(|r| (r.url, r.headers))?;
            let body = self.fetch_raw(request.0, &request.1).await?;
            let value: Value = serde_json::from_slice(&body)
                .map_err(|error| anyhow::anyhow!("response is not JSON: {error}"))?;
            let records = records_in(
                &evaluator,
                resolve.items.as_deref(),
                &value,
                "references.resolve.items",
            )?;
            for record in &records {
                let found = evaluator
                    .eval(&resolve.doi, record)
                    .map_err(|error| anyhow::anyhow!("references.resolve.doi: {error}"))?;
                match found
                    .as_ref()
                    .and_then(Value::as_str)
                    .and_then(normalize_doi)
                {
                    Some(doi) => dois.push(doi),
                    None => without_doi += 1,
                }
            }
        }
        if without_doi > 0 {
            // References the provider cannot resolve to a DOI (books, datasets,
            // works without a registered one) can never join a DOI-keyed
            // library, so they are not edges. Counted, not silent.
            tracing::debug!(
                integration = %self.id,
                without_doi,
                "references without a usable DOI left out of the citation graph"
            );
        }
        dois.sort();
        dois.dedup();
        Ok(dois)
    }

    async fn fetch_raw(&self, url: String, headers: &[(&str, String)]) -> anyhow::Result<Vec<u8>> {
        match self.http.get_bytes(url, headers).await {
            Ok(body) => Ok(body),
            Err(error) if error.kind == ProviderHttpErrorKind::RateLimited => {
                anyhow::bail!("{} rate limit reached", self.manifest.name)
            }
            Err(error) => Err(error.into()),
        }
    }

    // ── Download resolution ──────────────────────────────────────────────────

    async fn resolve_manifest_download(
        &self,
        result: &LiteratureSearchResult,
    ) -> anyhow::Result<Option<ResolvedLiteratureDownload>> {
        let Some(resolve) = &self.manifest.capabilities.resolve_download else {
            return Ok(result
                .pdf_url
                .as_ref()
                .map(|url| ResolvedLiteratureDownload {
                    url: url.clone(),
                    filename: None,
                }));
        };

        let mut values = result_values(result);
        for (index, step) in resolve.steps.iter().enumerate() {
            let path = substitute_values(&step.path, &values, Substitution::UrlComponent)
                .map_err(|error| anyhow::anyhow!("resolver step {}: {error}", index + 1))?;
            let step_base_url = self.resolver_step_base_url(step).map_err(|error| {
                anyhow::anyhow!("resolver step {} base_url: {error}", index + 1)
            })?;
            let step_base = step_base_url.as_str().trim_end_matches('/');
            let inherit_global = step_base_url.origin() == self.base_url.origin();
            let request = self.request_against(
                step_base,
                &step_base_url,
                inherit_global,
                &path,
                &step.headers,
                &step.query,
                &[],
            )?;
            let body = self.http.get_bytes(request.url, &request.headers).await?;
            let extracted = self
                .extract_step(step, &body, &step_base_url)
                .map_err(|error| {
                    anyhow::anyhow!("resolver step {} response: {error}", index + 1)
                })?;
            for (name, value) in extracted {
                values.insert(name, value);
            }
        }

        let rendered = substitute_values(&resolve.url, &values, Substitution::ExactRawOtherwiseUrl)
            .map_err(|error| anyhow::anyhow!("resolve_download.url: {error}"))?;
        let url = match Url::parse(&rendered) {
            Ok(url) => url,
            Err(_) => self.base_url.join(&rendered)?,
        };
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https"),
            "resolved download URL uses unsupported scheme '{}'",
            url.scheme()
        );
        let filename = resolve
            .filename
            .as_ref()
            .map(|template| substitute_values(template, &values, Substitution::Raw))
            .transpose()
            .map_err(|error| anyhow::anyhow!("resolve_download.filename: {error}"))?
            // The same answer the downloader gives: a name a manifest
            // rendered is written by the same filesystem as any other, and two
            // ideas of what one may contain would differ exactly where a
            // template is long.
            .map(|name| {
                crate::path::sanitize_file_name(&name)
                    .map_err(|error| anyhow::anyhow!("resolve_download.filename: {error}"))
            })
            .transpose()?;
        Ok(Some(ResolvedLiteratureDownload {
            url: url.to_string(),
            filename,
        }))
    }

    fn extract_step(
        &self,
        step: &ResolverStep,
        body: &[u8],
        base_url: &Url,
    ) -> anyhow::Result<HashMap<String, String>> {
        // `absolute_url` resolves against the step's own base: a relative URL
        // in a response from another origin means *that* origin.
        let evaluator = Evaluator::new(base_url);
        let mut out = HashMap::new();
        match step.response_format {
            ResponseFormat::Json => {
                let value: Value = serde_json::from_slice(body)
                    .map_err(|error| anyhow::anyhow!("response is not JSON: {error}"))?;
                for (name, spec) in &step.fields {
                    let projected = self
                        .select_json(spec, &value, FieldType::Text, &evaluator)
                        .map_err(|error| anyhow::anyhow!("field '{name}': {error}"))?
                        .ok_or_else(|| anyhow::anyhow!("field '{name}' matched nothing"))?;
                    out.insert(name.clone(), projected_string(projected));
                }
            }
            ResponseFormat::Html => {
                let html = std::str::from_utf8(body)
                    .map_err(|error| anyhow::anyhow!("response is not UTF-8 HTML: {error}"))?;
                let document = Document::from(html);
                let root = Selection::from(document.root());
                for (name, spec) in &step.fields {
                    let projected = self
                        .select_html(spec, &root, FieldType::Text, &evaluator)
                        .map_err(|error| anyhow::anyhow!("field '{name}': {error}"))?
                        .ok_or_else(|| anyhow::anyhow!("field '{name}' matched nothing"))?;
                    out.insert(name.clone(), projected_string(projected));
                }
            }
        }
        Ok(out)
    }

    // ── Requests ─────────────────────────────────────────────────────────────

    /// Assemble one request against the primary origin: the URL with its
    /// declared query parameters, the headers, and a copy of the URL with
    /// secrets removed for display. `extra` are query pairs the caller
    /// composed (a browse's filters), encoded here.
    fn request<'a>(
        &'a self,
        path: &str,
        extra: &[(String, String)],
    ) -> anyhow::Result<PreparedRequest<'a>> {
        self.request_against(
            &self.base,
            &self.base_url,
            true,
            path,
            &EMPTY_PARAMS,
            &EMPTY_PARAMS,
            extra,
        )
    }

    fn resolver_step_base_url(&self, step: &ResolverStep) -> anyhow::Result<Url> {
        Url::parse(
            step.base_url
                .as_deref()
                .unwrap_or(&self.manifest.http.base_url),
        )
        .map_err(Into::into)
    }

    /// The value of one declared parameter, and whether it is a secret.
    /// `None` is an optional secret nobody has set: the parameter is left off
    /// the request rather than sent empty.
    fn param_value(&self, value: &ParamValue) -> anyhow::Result<Option<(String, bool)>> {
        match value {
            ParamValue::Literal(value) => Ok(Some((value.clone(), false))),
            ParamValue::Secret(secret) => {
                match self
                    .secrets
                    .get(&secret.secret)
                    .filter(|value| !value.trim().is_empty())
                {
                    Some(value) => Ok(Some((value.trim().to_string(), true))),
                    None if secret.optional => Ok(None),
                    None => anyhow::bail!(
                        "secret '{}' has no value; set it in Integrations",
                        secret.secret
                    ),
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn request_against<'a>(
        &'a self,
        base: &str,
        base_url: &Url,
        inherit_global: bool,
        path: &str,
        step_headers: &'a BTreeMap<String, ParamValue>,
        step_query: &'a BTreeMap<String, ParamValue>,
        extra: &[(String, String)],
    ) -> anyhow::Result<PreparedRequest<'a>> {
        let mut url = Url::parse(&format!("{base}{path}"))?;
        anyhow::ensure!(
            url.origin() == base_url.origin(),
            "path '{path}' would send the request to {} instead of {}",
            url.origin().ascii_serialization(),
            base_url.origin().ascii_serialization()
        );

        for (name, value) in extra {
            url.query_pairs_mut().append_pair(name, value);
        }

        let (global_headers, global_query) = match inherit_global {
            true => (&self.manifest.http.headers, &self.manifest.http.query),
            false => (&EMPTY_PARAMS, &EMPTY_PARAMS),
        };

        // A step's header or parameter of the same name replaces the primary
        // one rather than being sent beside it.
        let mut header_params: Vec<(&'a str, &'a ParamValue)> = Vec::new();
        for (name, value) in global_headers.iter().chain(step_headers) {
            match header_params
                .iter_mut()
                .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            {
                Some(slot) => *slot = (name.as_str(), value),
                None => header_params.push((name.as_str(), value)),
            }
        }
        let mut headers: Vec<(&'a str, String)> = Vec::new();
        for (name, value) in header_params {
            if let Some((value, _)) = self.param_value(value)? {
                headers.push((name, value));
            }
        }

        let mut query_params: Vec<(&'a str, &'a ParamValue)> = Vec::new();
        for (name, value) in global_query.iter().chain(step_query) {
            match query_params
                .iter_mut()
                .find(|(existing, _)| *existing == name.as_str())
            {
                Some(slot) => slot.1 = value,
                None => query_params.push((name.as_str(), value)),
            }
        }
        let mut secret_names: Vec<&str> = Vec::new();
        for (name, value) in query_params {
            if let Some((value, secret)) = self.param_value(value)? {
                url.query_pairs_mut().append_pair(name, &value);
                if secret {
                    secret_names.push(name);
                }
            }
        }

        let mut redacted = url.clone();
        if !secret_names.is_empty() {
            let pairs: Vec<(String, String)> = url
                .query_pairs()
                .map(|(name, value)| {
                    let value = match secret_names.contains(&name.as_ref()) {
                        true => "***".to_string(),
                        false => value.into_owned(),
                    };
                    (name.into_owned(), value)
                })
                .collect();
            redacted.query_pairs_mut().clear().extend_pairs(pairs);
        }

        Ok(PreparedRequest {
            url: url.to_string(),
            redacted_url: redacted.to_string(),
            headers,
        })
    }
}

static EMPTY_PARAMS: BTreeMap<String, ParamValue> = BTreeMap::new();

/// The records a response holds: the array `expression` selects, or the body
/// itself when it is the array.
fn records_in(
    evaluator: &Evaluator,
    expression: Option<&str>,
    body: &Value,
    label: &str,
) -> anyhow::Result<Vec<Value>> {
    let found = match expression {
        Some(expression) => evaluator
            .eval(expression, body)
            .map_err(|error| anyhow::anyhow!("{label}: {error}"))?
            .ok_or_else(|| {
                anyhow::anyhow!("{label} expression '{expression}' matched nothing in the response")
            })?,
        None => body.clone(),
    };
    match found {
        Value::Array(items) => Ok(items),
        other => anyhow::bail!(
            "{label} must select an array; found {}",
            describe_kind(&other)
        ),
    }
}

/// The query pairs a browse request adds to its path: filters, sort, keyword.
///
/// Everything a caller may vary was declared by the manifest, so everything
/// else is refused by name — a filter that is not declared, an option that is
/// not one of the declared ones, a bound the filter does not offer.
fn browse_parameters(
    browse: &BrowseCapability,
    request: &BrowseRequest,
    query: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let mut extra = Vec::new();
    let mut consumed: Vec<String> = Vec::new();

    let mut clauses = Vec::new();
    for (name, spec) in &browse.filters {
        match spec {
            FilterSpec::Choice {
                clause, options, ..
            } => {
                consumed.push(name.clone());
                if let Some(value) = request.filters.get(name).filter(|v| !v.is_empty()) {
                    if !options.iter().any(|(option, _)| option == value) {
                        return Err(format!("'{value}' is not an option of filter '{name}'"));
                    }
                    clauses.push(clause.replace("{value}", value));
                }
            }
            FilterSpec::Flag { clause, .. } => {
                consumed.push(name.clone());
                match request.filters.get(name).map(String::as_str) {
                    None | Some("") | Some("false") => {}
                    Some("true") => clauses.push(clause.clone()),
                    Some(other) => {
                        return Err(format!("filter '{name}' is true or false; got '{other}'"))
                    }
                }
            }
            FilterSpec::Range {
                min_clause,
                max_clause,
                ..
            } => {
                for (suffix, clause) in [("min", min_clause), ("max", max_clause)] {
                    let key = format!("{name}.{suffix}");
                    consumed.push(key.clone());
                    let Some(value) = request.filters.get(&key).filter(|v| !v.is_empty()) else {
                        continue;
                    };
                    let Some(clause) = clause else {
                        return Err(format!("filter '{name}' has no {suffix} bound"));
                    };
                    let number: i64 = value.trim().parse().map_err(|_| {
                        format!("filter '{key}' must be a whole number; got '{value}'")
                    })?;
                    clauses.push(clause.replace("{value}", &number.to_string()));
                }
            }
        }
    }
    if let Some(unknown) = request.filters.keys().find(|key| !consumed.contains(key)) {
        return Err(format!("'{unknown}' is not a filter this provider offers"));
    }
    if !clauses.is_empty() {
        let param = browse
            .filter_param
            .as_deref()
            .ok_or_else(|| "no filter_param declared".to_string())?;
        extra.push((param.to_string(), clauses.join(&browse.filter_separator)));
    }

    match (
        &browse.sort,
        request.sort.as_deref().filter(|s| !s.is_empty()),
    ) {
        (Some(sort), chosen) => {
            let fallback = match (query, &sort.keyword_default) {
                (Some(_), Some(keyword_default)) => keyword_default,
                _ => &sort.default,
            };
            let chosen = chosen.unwrap_or(fallback);
            if !sort.options.iter().any(|(value, _)| value == chosen) {
                return Err(format!("'{chosen}' is not a sort option"));
            }
            extra.push((sort.param.clone(), chosen.to_string()));
        }
        (None, Some(_)) => return Err("this provider does not offer a sort".to_string()),
        (None, None) => {}
    }

    if let Some(query) = query {
        let param = browse
            .query_param
            .as_deref()
            .ok_or_else(|| "this provider's listing takes no keyword".to_string())?;
        extra.push((param.to_string(), query.to_string()));
    }
    Ok(extra)
}

fn note_missing_id(index: usize, issues: &mut Vec<ProjectionIssue>) {
    issues.push(ProjectionIssue {
        record: index,
        field: "id".to_string(),
        selector: String::new(),
        problem: "record skipped: without an id it cannot be identified or downloaded".to_string(),
    });
}

fn normalize_html_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn capture_value(pattern: &str, group: usize, raw: &Value) -> Result<Value, String> {
    let text = raw
        .as_str()
        .ok_or_else(|| "regular-expression capture requires text".to_string())?;
    let regex = regex::Regex::new(pattern)
        .map_err(|error| format!("invalid regular expression: {error}"))?;
    let captured = regex
        .captures(text)
        .and_then(|captures| captures.get(group))
        .map(|capture| capture.as_str().trim().to_string())
        .filter(|capture| !capture.is_empty())
        .ok_or_else(|| format!("capture group {group} matched nothing"))?;
    Ok(Value::String(captured))
}

fn projected_string(value: Projected) -> String {
    match value {
        Projected::Text(value) => value,
        Projected::Integer(value) => value.to_string(),
        Projected::Boolean(value) => value.to_string(),
        Projected::Object(value) => Value::Object(value).to_string(),
    }
}

fn result_values(result: &LiteratureSearchResult) -> HashMap<String, String> {
    let mut values = HashMap::from([
        ("id".to_string(), result.id.clone()),
        (
            "citation_count".to_string(),
            result.citation_count.to_string(),
        ),
        (
            "is_open_access".to_string(),
            result.is_open_access.to_string(),
        ),
    ]);
    for (name, value) in [
        ("doi", &result.doi),
        ("title", &result.title),
        ("publication_date", &result.publication_date),
        ("venue", &result.venue),
        ("pdf_url", &result.pdf_url),
        ("landing_page_url", &result.landing_page_url),
        ("open_access_status", &result.open_access_status),
        ("license", &result.license),
        ("authors", &result.authors),
        ("publisher", &result.publisher),
        ("language", &result.language),
        ("file_format", &result.file_format),
        ("file_size", &result.file_size),
    ] {
        if let Some(value) = value {
            values.insert(name.to_string(), value.clone());
        }
    }
    if let Some(year) = result.year {
        values.insert("year".to_string(), year.to_string());
    }
    values
}

#[derive(Clone, Copy)]
enum Substitution {
    UrlComponent,
    Raw,
    ExactRawOtherwiseUrl,
}

fn substitute_values(
    template: &str,
    values: &HashMap<String, String>,
    mode: Substitution,
) -> Result<String, String> {
    if let Some(name) = matches!(mode, Substitution::ExactRawOtherwiseUrl)
        .then(|| {
            template
                .strip_prefix('{')
                .and_then(|value| value.strip_suffix('}'))
        })
        .flatten()
        .filter(|_| template.chars().filter(|c| *c == '{').count() == 1)
        .filter(|_| template.chars().filter(|c| *c == '}').count() == 1)
    {
        return values
            .get(name)
            .cloned()
            .ok_or_else(|| format!("required value '{name}' is missing"));
    }

    let encode = matches!(
        mode,
        Substitution::UrlComponent | Substitution::ExactRawOtherwiseUrl
    );
    let mut out = String::new();
    let mut rest = template;
    while let Some((prefix, after)) = rest.split_once('{') {
        out.push_str(prefix);
        let (name, tail) = after
            .split_once('}')
            .ok_or_else(|| "unclosed '{' in template".to_string())?;
        let value = values
            .get(name)
            .ok_or_else(|| format!("required value '{name}' is missing"))?;
        if encode {
            out.push_str(&urlencoding::encode(value));
        } else {
            out.push_str(value);
        }
        rest = tail;
    }
    out.push_str(rest);
    Ok(out)
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

/// The projected values of one record, by field name, taken out as they are
/// used.
struct Fields(HashMap<String, Projected>);

impl Fields {
    fn text(&mut self, field: &str) -> Option<String> {
        match self.0.remove(field) {
            Some(Projected::Text(value)) => Some(value),
            _ => None,
        }
    }

    fn int(&mut self, field: &str) -> Option<i64> {
        match self.0.remove(field) {
            Some(Projected::Integer(value)) => Some(value),
            _ => None,
        }
    }

    fn boolean(&mut self, field: &str) -> Option<bool> {
        match self.0.remove(field) {
            Some(Projected::Boolean(value)) => Some(value),
            _ => None,
        }
    }

    fn object(&mut self, field: &str) -> Option<serde_json::Map<String, Value>> {
        match self.0.remove(field) {
            Some(Projected::Object(value)) => Some(value),
            _ => None,
        }
    }
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
        &self.display_name
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn capabilities(&self) -> ProviderCapabilities {
        let capabilities = &self.manifest.capabilities;
        ProviderCapabilities {
            browse: capabilities.browse.as_ref().map(BrowseCapability::controls),
            lookup: capabilities.lookup.is_some(),
            references: capabilities.references.is_some(),
            resolve_download: capabilities.resolve_download.is_some(),
        }
    }

    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<LiteratureSearchResult>> {
        self.run_search(query, limit).await
    }

    async fn browse(&self, request: &BrowseRequest) -> anyhow::Result<Vec<LiteratureSearchResult>> {
        self.run_browse(request).await
    }

    async fn lookup(&self, doi: &str) -> anyhow::Result<ProviderWork> {
        self.run_lookup(doi).await
    }

    /// The DOIs the work identified by `doi` references. **DOI in, DOIs out**:
    /// callers identify a work by its DOI and receive normalized DOIs, so no
    /// provider-specific identifier ever crosses this boundary and the
    /// citation-graph storage stays decoupled from whichever provider supplies
    /// the edges. References the provider cannot resolve to a DOI are omitted:
    /// they can never participate in a DOI-keyed library join.
    async fn references(&self, doi: &str) -> anyhow::Result<Vec<String>> {
        self.run_references(doi).await
    }

    async fn resolve_download(
        &self,
        result: &LiteratureSearchResult,
    ) -> anyhow::Result<Option<ResolvedLiteratureDownload>> {
        self.resolve_manifest_download(result).await
    }

    async fn status(&self, enabled: bool) -> anyhow::Result<IntegrationStatus> {
        let status_of = |state: IntegrationState, message: String| IntegrationStatus {
            id: self.id.clone(),
            enabled,
            state,
            message,
            version: None,
        };

        let Some(health) = &self.manifest.capabilities.health else {
            return Ok(status_of(
                IntegrationState::Ready,
                format!(
                    "{} declares no health check, so it is used without probing.",
                    self.manifest.name
                ),
            ));
        };

        // Probed whether or not the integration is enabled: the settings form
        // asks this *before* enabling, so a short-circuit on `enabled` would
        // answer "disabled" and the integration could never be switched on.
        let request = self.request(&health.path, &[])?;
        match self.http.get_status(request.url, &request.headers).await {
            Ok(_) => Ok(status_of(
                IntegrationState::Ready,
                format!("{} is reachable.", self.manifest.name),
            )),
            Err(error) if error.kind == ProviderHttpErrorKind::RateLimited => Ok(status_of(
                IntegrationState::RateLimited,
                format!(
                    "{} is reachable, but the rate limit is currently reached.",
                    self.manifest.name
                ),
            )),
            Err(error) => Ok(status_of(
                IntegrationState::RemoteApiDown,
                self.redact_error(error.to_string()),
            )),
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
            &urlencoding::encode(value),
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

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::builtin;
    use mockito::Matcher;

    // ── Fixtures ─────────────────────────────────────────────────────────────

    /// The bodies the two native clients' own tests used, so the shipped
    /// manifests are being asked about the same bytes the Rust clients were.
    const OPENALEX_BODY: &str = r#"{"results":[{"id":"https://openalex.org/W1","display_name":"T","ids":{"doi":"https://doi.org/10.1/example"},"open_access":{"is_oa":true,"oa_status":"gold","oa_url":"https://example.test/article"},"best_oa_location":{"is_oa":true,"pdf_url":"https://example.test/paper.pdf","landing_page_url":"https://example.test/article","license":"cc-by"}}]}"#;
    const SEMANTIC_SCHOLAR_BODY: &str = r#"{"data":[{"paperId":"p1","externalIds":{"DOI":"10.1/example"},"title":"T","citationCount":3,"isOpenAccess":true,"openAccessPdf":{"url":"https://example.test/paper.pdf","status":"GOLD","license":"CCBY"}}]}"#;

    /// A JSON service that needs a credential, shaped like Semantic Scholar's
    /// search. The probe and error tests use it because a *required* secret is
    /// what they are about; the shipped providers' are optional.
    const KEYED_MANIFEST: &str = r#"
manifest_version = 2
id = "keyed"
name = "Keyed"

[http]
base_url = "BASE_URL"

[http.headers]
x-api-key = { secret = "api_key" }

[capabilities.search]
path = "/graph/v1/paper/search?query={query}&limit={limit}&fields=title,citationCount"
items = "data"

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

    const ANNA_MANIFEST: &str = r#"
manifest_version = 2
id = "anna"
name = "Anna"

[http]
base_url = "BASE_URL"

[http.headers]
User-Agent = "Mozilla/5.0 Wilkes"

[capabilities.search]
path = "/search?q={query}&content=journal"
response_format = "html"
items = '''div:has(> a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"])'''

[capabilities.search.fields]
id = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", capture = '''^/md5/([0-9a-f]+)$''' }
title = '''div.max-w-full a[href^="/md5/"]'''
authors = { path = '''div.max-w-full a[href^="/search"]:has(span[class="icon-[mdi--user-edit]"])''', all = true, then = "join(', ', @)" }
publisher = '''div.max-w-full a[href^="/search"]:has(span[class="icon-[mdi--company]"])'''
venue = '''div.max-w-full a[href^="/search"]:has(span[class="icon-[mdi--company]"])'''
language = { path = "div.text-gray-800", capture = '''^\s*✅?\s*([^\[]+?)\s*\[''' }
file_format = { path = "div.text-gray-800", capture = '''(?i)\b(EPUB|PDF|MOBI|AZW3|AZW|DJVU|CBZ|CBR|FB2|DOCX?|TXT)\b''' }
file_size = { path = "div.text-gray-800", capture = '''(?i)(\d+(?:\.\d+)?\s*(?:MB|KB|GB|TB))''' }
landing_page_url = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", then = "absolute_url(@)" }

[capabilities.resolve_download]
url = "{download_url}"
filename = "{title}.{file_format}"

[[capabilities.resolve_download.steps]]
path = "/dyn/api/fast_download.json?md5={id}"
response_format = "json"

[capabilities.resolve_download.steps.query]
key = { secret = "anna_key" }

[capabilities.resolve_download.steps.fields]
download_url = "download_url"
"#;

    const ANNA_BODY: &str = r#"<!doctype html><html><body>
<div class="result flex">
  <a href="/md5/deadbeef" class="custom-a block mr-2 sm:mr-4 hover:opacity-80">cover</a>
  <div class="max-w-full">
    <a href="/md5/deadbeef">Graph: Networks</a>
    <a href="/search?q=Ada"><span class="icon-[mdi--user-edit]"></span> Ada Lovelace </a>
    <a href="/search?q=Grace"><span class="icon-[mdi--user-edit]"></span> Grace Hopper </a>
    <a href="/search?q=Journal"><span class="icon-[mdi--company]"></span> Example Journal </a>
    <div class="text-gray-800">✅ English [en] · PDF · 2.4MB · 2024</div>
  </div>
</div>
</body></html>"#;

    const ANNA_DOI_MANIFEST: &str = r#"
manifest_version = 2
id = "anna-doi"
name = "Anna DOI"

[http]
base_url = "BASE_URL"

[capabilities.search]
path = "/scidb/{query}"
response_format = "html"
items = '''div:has(> a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"])'''

[capabilities.search.fields]
id = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", capture = '''^/md5/([0-9a-f]+)$''' }
doi = { input = "query", then = "normalize_doi(@)" }
title = '''div.max-w-full a[href^="/md5/"]'''

[capabilities.resolve_download]
url = "/scidb?doi={doi}"
filename = "{title}"
"#;

    fn source(manifest: &str, base_url: &str, secrets: &[(&str, &str)]) -> CustomSource {
        let manifest = Manifest::parse(&manifest.replace("BASE_URL", base_url)).unwrap();
        CustomSource::new(manifest, secret_map(secrets)).unwrap()
    }

    fn secret_map(secrets: &[(&str, &str)]) -> HashMap<String, String> {
        secrets
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// A shipped provider, pointed at a mock server in place of its own origin.
    fn shipped(id: &str, base_url: &str, secrets: &[(&str, &str)]) -> CustomSource {
        let mut manifest = builtin::manifests()
            .into_iter()
            .find(|manifest| manifest.id == id)
            .unwrap_or_else(|| panic!("no shipped manifest '{id}'"));
        manifest.http.base_url = base_url.to_string();
        CustomSource::build(manifest, secret_map(secrets), None, Standing::BuiltIn).unwrap()
    }

    // ── Identity ─────────────────────────────────────────────────────────────

    /// Renaming is a label on this installation's copy, so it must not need
    /// the manifest touched — the manifest describes the service, and editing
    /// it to change a row's name would mean re-reading and re-probing it.
    #[test]
    fn a_config_name_overrides_the_manifests_own() {
        let config = CustomIntegrationConfig {
            id: "keyed".into(),
            name: Some("My Keyed".into()),
            enabled: false,
            manifest: KEYED_MANIFEST.replace("BASE_URL", "https://example.test"),
            secrets: HashMap::new(),
        };
        let source = CustomSource::from_config(&config).expect("loads");
        assert_eq!(source.name(), "My Keyed");
        // The id is the manifest's and is untouched by a rename: it is what
        // every stored selection and every log line names.
        assert_eq!(source.id(), "custom:keyed");
    }

    /// A cleared rename box is no override, not a nameless provider.
    #[test]
    fn a_blank_config_name_leaves_the_manifests_own() {
        for name in [None, Some(String::new()), Some("   ".into())] {
            let config = CustomIntegrationConfig {
                id: "keyed".into(),
                name,
                enabled: false,
                manifest: KEYED_MANIFEST.replace("BASE_URL", "https://example.test"),
                secrets: HashMap::new(),
            };
            let source = CustomSource::from_config(&config).expect("loads");
            assert_eq!(source.name(), "Keyed");
        }
    }

    #[test]
    fn a_built_in_is_not_namespaced_and_a_custom_provider_is() {
        assert_eq!(
            shipped("openalex", "https://example.test", &[]).id(),
            "openalex"
        );
        assert_eq!(
            source(KEYED_MANIFEST, "https://example.test", &[]).id(),
            "custom:keyed"
        );
    }

    // ── The shipped providers: search ────────────────────────────────────────

    /// What the native OpenAlex client made of its own fixture, said as the
    /// record it produced. The shipped manifest must produce the same one.
    fn expected_openalex_record() -> LiteratureSearchResult {
        LiteratureSearchResult {
            id: "https://openalex.org/W1".into(),
            doi: Some("10.1/example".into()),
            title: Some("T".into()),
            year: None,
            publication_date: None,
            venue: None,
            citation_count: 0,
            is_open_access: true,
            pdf_url: Some("https://example.test/paper.pdf".into()),
            landing_page_url: Some("https://example.test/article".into()),
            open_access_status: Some("gold".into()),
            license: Some("cc-by".into()),
            authors: None,
            publisher: None,
            language: None,
            file_format: None,
            file_size: None,
            acquisition: LiteratureAcquisition::Direct,
        }
    }

    #[tokio::test]
    async fn the_openalex_manifest_sends_query_limit_and_mailto_and_reads_the_record() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("search".into(), "graph neural networks".into()),
                Matcher::UrlEncoded("per-page".into(), "2".into()),
                Matcher::UrlEncoded("mailto".into(), "team@example.com".into()),
                Matcher::Regex("select=id".into()),
            ]))
            .with_status(200)
            .with_body(OPENALEX_BODY)
            .create_async()
            .await;

        let results = shipped(
            "openalex",
            &server.url(),
            &[("email", " team@example.com ")],
        )
        .search(" graph neural networks ", 2)
        .await
        .unwrap();

        assert_eq!(results, vec![expected_openalex_record()]);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn an_unset_optional_secret_is_left_off_the_request() {
        let mut server = mockito::Server::new_async().await;
        // Mocks match in creation order, so a request that carries `mailto`
        // lands here and fails the `expect(0)`.
        let with_mailto = server
            .mock("GET", "/works")
            .match_query(Matcher::Regex("mailto".into()))
            .expect(0)
            .create_async()
            .await;
        let without = server
            .mock("GET", "/works")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(OPENALEX_BODY)
            .expect(2)
            .create_async()
            .await;

        // No `email` secret at all, and a blank one: both mean "not set".
        for secrets in [vec![], vec![("email", "   ")]] {
            let results = shipped("openalex", &server.url(), &secrets)
                .search("graph", 1)
                .await
                .unwrap();
            assert_eq!(results.len(), 1);
        }
        with_mailto.assert_async().await;
        without.assert_async().await;
    }

    #[tokio::test]
    async fn the_semantic_scholar_manifest_sends_its_key_and_reads_the_record() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/search")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("query".into(), "graph neural networks".into()),
                Matcher::UrlEncoded("limit".into(), "2".into()),
                Matcher::Regex("fields=title".into()),
            ]))
            .match_header("x-api-key", "secret")
            .with_status(200)
            .with_body(SEMANTIC_SCHOLAR_BODY)
            .create_async()
            .await;

        let results = shipped(
            "semantic_scholar",
            &server.url(),
            &[("api_key", " secret ")],
        )
        .search(" graph neural networks ", 2)
        .await
        .unwrap();

        assert_eq!(results.len(), 1);
        let paper = &results[0];
        assert_eq!(paper.id, "p1");
        assert_eq!(paper.doi.as_deref(), Some("10.1/example"));
        assert_eq!(paper.citation_count, 3);
        assert!(paper.is_open_access);
        assert_eq!(
            paper.pdf_url.as_deref(),
            Some("https://example.test/paper.pdf")
        );
        assert_eq!(paper.open_access_status.as_deref(), Some("GOLD"));
        assert_eq!(paper.license.as_deref(), Some("CCBY"));
        assert_eq!(paper.acquisition, LiteratureAcquisition::Direct);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn semantic_scholar_works_without_a_key_and_sends_no_header() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/search")
            .match_query(Matcher::Any)
            .match_header("x-api-key", Matcher::Missing)
            .with_status(200)
            .with_body(SEMANTIC_SCHOLAR_BODY)
            .create_async()
            .await;
        let results = shipped("semantic_scholar", &server.url(), &[])
            .search("graph", 1)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        mock.assert_async().await;
    }

    /// The one case the dotted-path selector could not say: `first_of` is
    /// *first present*, not *or*, and OpenAlex's `is_open_access` is an `or`
    /// over two fields that are both always present. It used to be the pinned
    /// divergence between the manifest and the Rust client; the Rust client is
    /// gone and the manifest says it.
    #[tokio::test]
    async fn openalex_open_access_is_a_disjunction_of_the_work_and_its_best_location() {
        let body = r#"{"results":[{"id":"W1","display_name":"T","ids":{},"open_access":{"is_oa":false},"best_oa_location":{"is_oa":true,"pdf_url":"https://example.test/p.pdf"}}]}"#;
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
        let results = shipped("openalex", &server.url(), &[])
            .search("q", 1)
            .await
            .unwrap();
        assert!(results[0].is_open_access, "false || true is true");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn openalex_reads_authors_and_publisher_and_treats_no_authors_as_none() {
        let body = r#"{"results":[
            {"id":"W1","display_name":"A","ids":{},"authorships":[{"author":{"display_name":"Ada Lovelace"}},{"author":{"display_name":"Grace Hopper"}}],"primary_location":{"source":{"display_name":"J","host_organization_name":"Pub"}}},
            {"id":"W2","display_name":"B","ids":{},"authorships":[]}
        ]}"#;
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
        let results = shipped("openalex", &server.url(), &[])
            .search("q", 2)
            .await
            .unwrap();
        assert_eq!(
            results[0].authors.as_deref(),
            Some("Ada Lovelace, Grace Hopper")
        );
        assert_eq!(results[0].publisher.as_deref(), Some("Pub"));
        assert_eq!(results[1].authors, None, "an empty list is no authors");
        mock.assert_async().await;
    }

    // ── Status ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn status_probes_whether_or_not_the_integration_is_enabled() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
            .match_query(Matcher::UrlEncoded("fields".into(), "paperId".into()))
            .with_status(200)
            .with_body(r#"{"paperId":"p1"}"#)
            .expect(2)
            .create_async()
            .await;

        let source = shipped("semantic_scholar", &server.url(), &[]);
        let disabled = source.status(false).await.unwrap();
        assert_eq!(disabled.state, IntegrationState::Ready);
        assert!(!disabled.enabled);
        let enabled = source.status(true).await.unwrap();
        assert_eq!(enabled.state, IntegrationState::Ready);
        assert!(enabled.enabled);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn status_reports_a_down_service() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(Matcher::Any)
            .with_status(400)
            .create_async()
            .await;
        let status = shipped("openalex", &server.url(), &[])
            .status(true)
            .await
            .unwrap();
        assert_eq!(status.state, IntegrationState::RemoteApiDown);
        mock.assert_async().await;
    }

    // ── Lookup ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn semantic_scholar_lookup_normalizes_the_doi_and_sends_the_key() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
            .match_query(Matcher::Regex("fields=title".into()))
            .match_header("x-api-key", "secret")
            .with_status(200)
            .with_body(
                r#"{"paperId":"p1","externalIds":{"DOI":"10.1145/3801158","CorpusId":7},"title":"T","year":2026,"citationCount":3}"#,
            )
            .create_async()
            .await;

        let work = shipped("semantic_scholar", &server.url(), &[("api_key", "secret")])
            .lookup("https://doi.org/10.1145/3801158")
            .await
            .unwrap();

        assert_eq!(work.doi, "10.1145/3801158");
        assert_eq!(work.work_id, "p1");
        assert_eq!(work.title.as_deref(), Some("T"));
        assert_eq!(work.year, Some(2026));
        assert_eq!(work.citation_count, 3);
        assert_eq!(work.external_ids["CorpusId"], serde_json::json!(7));
        assert!(work.cached_at_ms > 0);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn a_rate_limited_lookup_is_retried_once() {
        let mut server = mockito::Server::new_async().await;
        let first = server
            .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
            .match_query(Matcher::Any)
            .with_status(429)
            .with_header("Retry-After", "0")
            .expect(1)
            .create_async()
            .await;
        let second = server
            .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(
                r#"{"paperId":"p1","externalIds":{"DOI":"10.1145/3801158"},"citationCount":9}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let work = shipped("semantic_scholar", &server.url(), &[])
            .lookup("10.1145/3801158")
            .await
            .unwrap();

        assert_eq!(work.citation_count, 9);
        first.assert_async().await;
        second.assert_async().await;
    }

    #[tokio::test]
    async fn a_missing_paper_is_no_record_not_a_failure_to_read() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/DOI:10.1145%2F3801158")
            .match_query(Matcher::Any)
            .with_status(404)
            .create_async()
            .await;
        let error = shipped("semantic_scholar", &server.url(), &[])
            .lookup("10.1145/3801158")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("no record for DOI 10.1145/3801158"),
            "{error}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn an_invalid_doi_is_refused_before_any_request() {
        let error = shipped("openalex", "http://127.0.0.1:1", &[])
            .lookup("not-a-doi")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Invalid DOI"), "{error}");
    }

    const OPENALEX_RECORD: &str = r#"{"results":[{"id":"https://openalex.org/W3145166639","doi":"https://doi.org/10.1109/msr52588.2021.00074","display_name":"Sampling Projects in GitHub","publication_year":2021,"publication_date":"2021-05-01","cited_by_count":12,"ids":{"openalex":"https://openalex.org/W3145166639","doi":"https://doi.org/10.1109/msr52588.2021.00074"},"primary_location":{"source":{"display_name":"MSR"}}}]}"#;

    /// The DOI index first; when it holds nothing, the landing-page index,
    /// which knows works the first does not. This is the bounded alternative
    /// the lookup capability exists to say.
    #[tokio::test]
    async fn lookup_moves_to_the_next_request_only_when_a_response_holds_no_record() {
        let mut server = mockito::Server::new_async().await;
        let by_doi = server
            .mock("GET", "/works")
            .match_query(Matcher::UrlEncoded(
                "filter".into(),
                "doi:https://doi.org/10.1109/msr52588.2021.00074".into(),
            ))
            .with_status(200)
            .with_body(r#"{"results":[]}"#)
            .expect(1)
            .create_async()
            .await;
        let by_landing_page = server
            .mock("GET", "/works")
            .match_query(Matcher::UrlEncoded(
                "filter".into(),
                "locations.landing_page_url:https://doi.org/10.1109/msr52588.2021.00074".into(),
            ))
            .with_status(200)
            .with_body(OPENALEX_RECORD)
            .expect(1)
            .create_async()
            .await;

        let work = shipped("openalex", &server.url(), &[])
            .lookup("https://doi.org/10.1109/MSR52588.2021.00074")
            .await
            .unwrap();

        assert_eq!(work.doi, "10.1109/msr52588.2021.00074");
        assert_eq!(work.work_id, "https://openalex.org/W3145166639");
        assert_eq!(work.title.as_deref(), Some("Sampling Projects in GitHub"));
        assert_eq!(work.year, Some(2021));
        assert_eq!(work.venue.as_deref(), Some("MSR"));
        assert_eq!(work.citation_count, 12);
        assert_eq!(
            work.external_ids["openalex"],
            serde_json::json!("https://openalex.org/W3145166639")
        );
        by_doi.assert_async().await;
        by_landing_page.assert_async().await;
    }

    #[tokio::test]
    async fn lookup_that_finds_a_record_first_does_not_ask_again() {
        let mut server = mockito::Server::new_async().await;
        let first = server
            .mock("GET", "/works")
            .match_query(Matcher::Regex("filter=doi".into()))
            .with_status(200)
            .with_body(OPENALEX_RECORD)
            .expect(1)
            .create_async()
            .await;
        let second = server
            .mock("GET", "/works")
            .match_query(Matcher::Regex("landing_page_url".into()))
            .expect(0)
            .create_async()
            .await;
        shipped("openalex", &server.url(), &[])
            .lookup("10.1109/msr52588.2021.00074")
            .await
            .unwrap();
        first.assert_async().await;
        second.assert_async().await;
    }

    /// Only "no record here" moves on. A request that *failed* says nothing
    /// about whether the next would find one, and asking anyway would paper
    /// over the failure.
    #[tokio::test]
    async fn lookup_stops_at_a_failure_instead_of_trying_the_next_request() {
        let mut server = mockito::Server::new_async().await;
        let failing = server
            .mock("GET", "/works")
            .match_query(Matcher::Regex("filter=doi".into()))
            .with_status(400)
            .expect(1)
            .create_async()
            .await;
        let never = server
            .mock("GET", "/works")
            .match_query(Matcher::Regex("landing_page_url".into()))
            .expect(0)
            .create_async()
            .await;
        let error = shipped("openalex", &server.url(), &[])
            .lookup("10.1109/msr52588.2021.00074")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("400"), "{error}");
        failing.assert_async().await;
        never.assert_async().await;
    }

    #[tokio::test]
    async fn lookup_with_no_record_anywhere_says_so() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(r#"{"results":[]}"#)
            .expect(2)
            .create_async()
            .await;
        let error = shipped("openalex", &server.url(), &[])
            .lookup("10.1109/msr52588.2021.00074")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("has no record for DOI"), "{error}");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn a_provider_without_lookup_says_so_by_name() {
        let error = source(KEYED_MANIFEST, "https://example.test", &[("api_key", "k")])
            .lookup("10.1/x")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not offer lookup"), "{error}");
    }

    // ── References ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn references_resolve_a_works_identifiers_to_dois_in_one_batch() {
        let mut server = mockito::Server::new_async().await;
        let work = server
            .mock("GET", "/works")
            .match_query(Matcher::UrlEncoded(
                "filter".into(),
                "doi:https://doi.org/10.1/cites".into(),
            ))
            .with_status(200)
            .with_body(
                r#"{"results":[{"id":"https://openalex.org/W1","referenced_works":["https://openalex.org/W10","https://openalex.org/W11","https://openalex.org/W12"]}]}"#,
            )
            .expect(1)
            .create_async()
            .await;
        // The separator is the service's OR operator and stays literal; the
        // mock sees the decoded query either way, so this checks the value.
        let resolve = server
            .mock("GET", "/works")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("filter".into(), "openalex_id:W10|W11|W12".into()),
                Matcher::UrlEncoded("per-page".into(), "3".into()),
            ]))
            .with_status(200)
            .with_body(
                r#"{"results":[{"id":"https://openalex.org/W12","doi":"https://doi.org/10.5/B"},{"id":"https://openalex.org/W10","doi":"https://doi.org/10.5/ref"},{"id":"https://openalex.org/W11"},{"id":"W13","doi":"https://doi.org/10.5/ref"}]}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let dois = shipped("openalex", &server.url(), &[])
            .references("https://doi.org/10.1/CITES")
            .await
            .unwrap();

        // Sorted, deduplicated, normalized; the record without a DOI is not an
        // edge a DOI-keyed library can join.
        assert_eq!(dois, vec!["10.5/b", "10.5/ref"]);
        work.assert_async().await;
        resolve.assert_async().await;
    }

    #[tokio::test]
    async fn references_are_resolved_in_declared_chunks() {
        const CHUNKED: &str = r#"
manifest_version = 2
id = "chunked"
name = "Chunked"
[http]
base_url = "BASE_URL"
[capabilities.search]
path = "/s?q={query}"
items = "r"
[capabilities.search.fields]
id = "id"
title = "t"
[capabilities.references]
path = "/w/{doi}"
ids = "refs"
[capabilities.references.resolve]
path = "/batch?ids={ids}&n={count}"
join = ","
chunk = 2
items = "r"
doi = "doi"
"#;
        let mut server = mockito::Server::new_async().await;
        let _work = server
            .mock("GET", "/w/10.1%2Fx")
            .with_status(200)
            .with_body(r#"{"refs":["a","b","c"]}"#)
            .create_async()
            .await;
        let first = server
            .mock("GET", "/batch")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("ids".into(), "a,b".into()),
                Matcher::UrlEncoded("n".into(), "2".into()),
            ]))
            .with_status(200)
            .with_body(r#"{"r":[{"doi":"10.9/a"}]}"#)
            .expect(1)
            .create_async()
            .await;
        let second = server
            .mock("GET", "/batch")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("ids".into(), "c".into()),
                Matcher::UrlEncoded("n".into(), "1".into()),
            ]))
            .with_status(200)
            .with_body(r#"{"r":[{"doi":"10.9/c"}]}"#)
            .expect(1)
            .create_async()
            .await;

        let dois = source(CHUNKED, &server.url(), &[])
            .references("10.1/x")
            .await
            .unwrap();
        assert_eq!(dois, vec!["10.9/a", "10.9/c"]);
        first.assert_async().await;
        second.assert_async().await;
    }

    #[tokio::test]
    async fn a_work_that_references_nothing_makes_no_second_request() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(r#"{"results":[{"id":"W1","referenced_works":[]}]}"#)
            .expect(1)
            .create_async()
            .await;
        let dois = shipped("openalex", &server.url(), &[])
            .references("10.1/x")
            .await
            .unwrap();
        assert!(dois.is_empty());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn an_unknown_work_has_no_references_rather_than_an_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(r#"{"results":[]}"#)
            .expect(1)
            .create_async()
            .await;
        let dois = shipped("openalex", &server.url(), &[])
            .references("10.1/x")
            .await
            .unwrap();
        assert!(dois.is_empty());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn a_provider_without_references_says_so_by_name() {
        let error = shipped("semantic_scholar", "https://example.test", &[])
            .references("10.1/x")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not offer references"), "{error}");
    }

    // ── Browse ───────────────────────────────────────────────────────────────

    fn openalex_browse() -> BrowseCapability {
        builtin::manifests()
            .into_iter()
            .find(|manifest| manifest.id == "openalex")
            .unwrap()
            .capabilities
            .browse
            .unwrap()
    }

    fn browse_request(filters: &[(&str, &str)]) -> BrowseRequest {
        BrowseRequest {
            filters: filters
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..BrowseRequest::default()
        }
    }

    #[test]
    fn browse_composes_the_declared_clauses_in_declaration_order() {
        let browse = openalex_browse();
        let request = browse_request(&[
            // Deliberately out of order: the manifest decides the order.
            ("open_access", "true"),
            ("year.max", "2024"),
            ("field", "17"),
            ("year.min", "2020"),
            ("cited_by.min", "100"),
            ("not_retracted", "false"),
        ]);
        let extra = browse_parameters(&browse, &request, None).unwrap();
        assert_eq!(
            extra,
            vec![
                (
                    "filter".to_string(),
                    "from_publication_date:2020-01-01,to_publication_date:2024-12-31,\
                     cited_by_count:>100,primary_topic.field.id:17,open_access.is_oa:true"
                        .to_string()
                ),
                ("sort".to_string(), "cited_by_count:desc".to_string()),
            ]
        );
    }

    #[test]
    fn browse_with_a_keyword_defaults_to_the_keyword_sort_and_sends_the_keyword() {
        let browse = openalex_browse();
        let extra = browse_parameters(&browse, &browse_request(&[]), Some("graph")).unwrap();
        assert_eq!(
            extra,
            vec![
                ("sort".to_string(), "relevance_score:desc".to_string()),
                ("search".to_string(), "graph".to_string()),
            ]
        );
        // An explicit sort beats the default of either kind.
        let mut request = browse_request(&[]);
        request.sort = Some("publication_date:desc".into());
        let extra = browse_parameters(&browse, &request, Some("graph")).unwrap();
        assert_eq!(extra[0].1, "publication_date:desc");
    }

    #[test]
    fn browse_refuses_what_the_manifest_did_not_declare() {
        let browse = openalex_browse();
        for (filters, expected) in [
            (vec![("nope", "1")], "'nope' is not a filter"),
            (
                vec![("type", "novel")],
                "'novel' is not an option of filter 'type'",
            ),
            (vec![("open_access", "maybe")], "true or false"),
            (vec![("year.min", "soon")], "must be a whole number"),
            (vec![("year", "2020")], "'year' is not a filter"),
            (
                vec![("open_access.min", "1")],
                "'open_access.min' is not a filter",
            ),
            (vec![("cited_by.max", "10")], "has no max bound"),
        ] {
            let error = browse_parameters(&browse, &browse_request(&filters), None).unwrap_err();
            assert!(error.contains(expected), "{filters:?}: {error}");
        }
        let mut request = browse_request(&[]);
        request.sort = Some("whatever".into());
        let error = browse_parameters(&browse, &request, None).unwrap_err();
        assert!(error.contains("is not a sort option"), "{error}");
    }

    #[test]
    fn a_blank_filter_value_is_no_filter() {
        let browse = openalex_browse();
        let extra = browse_parameters(
            &browse,
            &browse_request(&[("type", ""), ("year.min", ""), ("open_access", "")]),
            None,
        )
        .unwrap();
        assert_eq!(
            extra,
            vec![("sort".to_string(), "cited_by_count:desc".to_string())]
        );
    }

    #[tokio::test]
    async fn browse_requests_the_page_the_limit_the_filters_and_the_sort() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/works")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("per-page".into(), "5".into()),
                Matcher::UrlEncoded("page".into(), "3".into()),
                Matcher::UrlEncoded(
                    "filter".into(),
                    "from_publication_date:2020-01-01,open_access.is_oa:true".into(),
                ),
                Matcher::UrlEncoded("sort".into(), "publication_date:desc".into()),
                Matcher::UrlEncoded("mailto".into(), "team@example.com".into()),
            ]))
            .with_status(200)
            .with_body(OPENALEX_BODY)
            .create_async()
            .await;

        let mut request = browse_request(&[("year.min", "2020"), ("open_access", "true")]);
        request.sort = Some("publication_date:desc".into());
        request.page = Some(3);
        request.limit = Some(5);
        let results = shipped("openalex", &server.url(), &[("email", "team@example.com")])
            .browse(&request)
            .await
            .unwrap();

        assert_eq!(results, vec![expected_openalex_record()]);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn browse_without_a_keyword_sends_no_search_parameter() {
        let mut server = mockito::Server::new_async().await;
        let with_search = server
            .mock("GET", "/works")
            .match_query(Matcher::Regex("search=".into()))
            .expect(0)
            .create_async()
            .await;
        let without = server
            .mock("GET", "/works")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(OPENALEX_BODY)
            .expect(1)
            .create_async()
            .await;
        shipped("openalex", &server.url(), &[])
            .browse(&browse_request(&[]))
            .await
            .unwrap();
        with_search.assert_async().await;
        without.assert_async().await;
    }

    #[tokio::test]
    async fn a_provider_that_declares_no_browse_refuses_by_name() {
        let error = shipped("semantic_scholar", "https://example.test", &[])
            .browse(&BrowseRequest::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not offer browse"), "{error}");
    }

    // ── Search: HTML and download resolution ─────────────────────────────────

    #[tokio::test]
    async fn anna_html_search_and_credentialed_download_resolution_are_manifest_expressible() {
        let mut server = mockito::Server::new_async().await;
        let search = server
            .mock("GET", "/search")
            .match_query(Matcher::Any)
            .match_header("user-agent", "Mozilla/5.0 Wilkes")
            .with_status(200)
            .with_body(ANNA_BODY)
            .create_async()
            .await;
        let resolve = server
            .mock("GET", "/dyn/api/fast_download.json")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("md5".into(), "deadbeef".into()),
                Matcher::UrlEncoded("key".into(), "secret".into()),
            ]))
            .with_status(200)
            .with_body(r#"{"download_url":"https://files.example.test/book"}"#)
            .create_async()
            .await;

        let anna = source(ANNA_MANIFEST, &server.url(), &[("anna_key", "secret")]);
        let results = anna.search("graph networks", 1).await.unwrap();
        assert_eq!(results.len(), 1);
        let book = &results[0];
        assert_eq!(book.id, "deadbeef");
        assert_eq!(book.title.as_deref(), Some("Graph: Networks"));
        assert_eq!(book.authors.as_deref(), Some("Ada Lovelace, Grace Hopper"));
        assert_eq!(book.publisher.as_deref(), Some("Example Journal"));
        assert_eq!(book.language.as_deref(), Some("English"));
        assert_eq!(book.file_format.as_deref(), Some("PDF"));
        assert_eq!(book.file_size.as_deref(), Some("2.4MB"));
        assert_eq!(book.acquisition, LiteratureAcquisition::Provider);

        let download = anna.resolve_download(book).await.unwrap().unwrap();
        assert_eq!(download.url, "https://files.example.test/book");
        assert_eq!(download.filename.as_deref(), Some("Graph_ Networks.PDF"));
        search.assert_async().await;
        resolve.assert_async().await;
    }

    #[tokio::test]
    async fn cross_origin_resolver_uses_its_own_base_and_does_not_receive_global_headers() {
        let mut search_server = mockito::Server::new_async().await;
        let mut resolver_server = mockito::Server::new_async().await;
        let search = search_server
            .mock("GET", "/search")
            .match_query(Matcher::Any)
            .match_header("user-agent", "Mozilla/5.0 Wilkes")
            .with_status(200)
            .with_body(ANNA_BODY)
            .create_async()
            .await;
        let resolve = resolver_server
            .mock("GET", "/dyn/api/fast_download.json")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("md5".into(), "deadbeef".into()),
                Matcher::UrlEncoded("key".into(), "secret".into()),
            ]))
            .match_header("user-agent", Matcher::Missing)
            .with_status(200)
            .with_body(r#"{"download_url":"/files/book.pdf"}"#)
            .create_async()
            .await;

        let manifest = ANNA_MANIFEST
            .replace(
                "[[capabilities.resolve_download.steps]]",
                &format!(
                    "[[capabilities.resolve_download.steps]]\nbase_url = {:?}",
                    resolver_server.url()
                ),
            )
            .replace(
                "download_url = \"download_url\"",
                "download_url = \"absolute_url(download_url)\"",
            );
        let anna = source(&manifest, &search_server.url(), &[("anna_key", "secret")]);
        let book = anna.search("graph networks", 1).await.unwrap().remove(0);
        let download = anna.resolve_download(&book).await.unwrap().unwrap();

        assert_eq!(
            download.url,
            format!("{}/files/book.pdf", resolver_server.url())
        );
        search.assert_async().await;
        resolve.assert_async().await;
    }

    #[tokio::test]
    async fn a_same_origin_step_inherits_global_headers_and_may_replace_one() {
        let manifest = ANNA_MANIFEST.replace(
            "[capabilities.resolve_download.steps.query]",
            "[capabilities.resolve_download.steps.headers]\nuser-agent = \"Step/1\"\n\n[capabilities.resolve_download.steps.query]",
        );
        let mut server = mockito::Server::new_async().await;
        let _search = server
            .mock("GET", "/search")
            .match_query(Matcher::Any)
            .match_header("user-agent", "Mozilla/5.0 Wilkes")
            .with_status(200)
            .with_body(ANNA_BODY)
            .create_async()
            .await;
        // The step's header of the same name replaces the primary one rather
        // than being sent beside it.
        let resolve = server
            .mock("GET", "/dyn/api/fast_download.json")
            .match_query(Matcher::Any)
            .match_header("user-agent", "Step/1")
            .with_status(200)
            .with_body(r#"{"download_url":"https://files.example.test/book"}"#)
            .expect(1)
            .create_async()
            .await;
        let anna = source(&manifest, &server.url(), &[("anna_key", "secret")]);
        let book = anna.search("graph networks", 1).await.unwrap().remove(0);
        anna.resolve_download(&book).await.unwrap().unwrap();
        resolve.assert_async().await;
    }

    #[tokio::test]
    async fn anna_doi_input_can_flow_to_the_scidb_paper_download() {
        let mut server = mockito::Server::new_async().await;
        let search = server
            .mock("GET", "/scidb/10.1234%2Fexample")
            .with_status(200)
            .with_body(ANNA_BODY)
            .create_async()
            .await;

        let anna = source(ANNA_DOI_MANIFEST, &server.url(), &[]);
        let result = anna.search("10.1234/example", 1).await.unwrap().remove(0);
        assert_eq!(result.doi.as_deref(), Some("10.1234/example"));

        let download = anna.resolve_download(&result).await.unwrap().unwrap();
        assert_eq!(
            download.url,
            format!("{}/scidb?doi=10.1234%2Fexample", server.url())
        );
        assert_eq!(download.filename.as_deref(), Some("Graph_ Networks"));
        search.assert_async().await;
    }

    // ── Secrets and the pinned origin ────────────────────────────────────────

    #[tokio::test]
    async fn a_required_secret_is_needed_before_a_request_is_made() {
        let error = source(KEYED_MANIFEST, "https://example.test", &[])
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
        let escaping = KEYED_MANIFEST.replace(
            "path = \"/graph/v1/paper/search?query={query}",
            "path = \"@evil.test/graph/v1/paper/search?query={query}",
        );
        let error = Manifest::parse(&escaping.replace("BASE_URL", "https://api.example.test"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("must start with '/'"), "{error}");

        let doubled = KEYED_MANIFEST.replace(
            "path = \"/graph/v1/paper/search?query={query}",
            "path = \"//evil.test/graph/v1/paper/search?query={query}",
        );
        let pinned = source(&doubled, "https://api.example.test", &[("api_key", "k")]);
        let request = pinned.request("//evil.test/works", &[]).unwrap();
        assert!(
            request
                .url
                .starts_with("https://api.example.test//evil.test/works"),
            "{}",
            request.url
        );
    }

    // ── Probe ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn probe_reports_an_unmapped_field_by_name_and_refuses_to_pass() {
        // `citationCount` arrives as a string where the manifest promised an
        // integer: present, unusable, and reported rather than nulled.
        let body = r#"{"data":[{"paperId":"p1","title":"T","citationCount":"many"}]}"#;
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/search")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let report = source(KEYED_MANIFEST, &server.url(), &[("api_key", "secret")])
            .probe("graph neural networks")
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
        let manifest = KEYED_MANIFEST.replace("[http.headers]", "[http.query]");
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/search")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(SEMANTIC_SCHOLAR_BODY)
            .create_async()
            .await;

        let report = source(&manifest, &server.url(), &[("api_key", "hunter2")])
            .probe("custom probe terms")
            .await;

        assert!(report.ok, "{:?}", report.error);
        assert!(
            !report.request_url.contains("hunter2"),
            "{}",
            report.request_url
        );
        assert!(report.request_url.contains("***"), "{}", report.request_url);
        let request_url = Url::parse(&report.request_url).unwrap();
        assert!(request_url
            .query_pairs()
            .any(|(name, value)| name == "query" && value == "custom probe terms"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn a_blank_probe_query_is_refused_before_any_request() {
        let report = source(KEYED_MANIFEST, "https://example.test", &[("api_key", "k")])
            .probe("   ")
            .await;

        assert!(!report.ok);
        assert_eq!(report.error.as_deref(), Some("probe query cannot be empty"));
        assert!(report.request_url.is_empty());
    }

    #[test]
    fn surfaced_errors_redact_every_configured_secret() {
        let source = source(
            KEYED_MANIFEST,
            "https://example.test",
            &[("api_key", "hunter2"), ("shorter", "hunter")],
        );

        let error = source.redact_error(
            "request failed with hunter2; nested cause repeated hunter2 and hunter".to_string(),
        );

        assert_eq!(
            error,
            "request failed with ***; nested cause repeated *** and ***"
        );
    }

    #[tokio::test]
    async fn a_record_without_an_id_is_skipped_and_said_so() {
        let body = r#"{"data":[{"title":"no id here"},{"paperId":"p2","title":"T"}]}"#;
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/search")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let report = source(KEYED_MANIFEST, &server.url(), &[("api_key", "secret")])
            .probe("graph neural networks")
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
    async fn an_items_expression_that_matches_nothing_names_itself() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/graph/v1/paper/search")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_body(r#"{"papers":[]}"#)
            .create_async()
            .await;

        let report = source(KEYED_MANIFEST, &server.url(), &[("api_key", "secret")])
            .probe("graph neural networks")
            .await;

        assert!(!report.ok);
        let error = report.error.unwrap();
        assert!(error.contains("expression 'data'"), "{error}");
        mock.assert_async().await;
    }

    // ── Capabilities ─────────────────────────────────────────────────────────

    #[test]
    fn capabilities_report_what_the_manifest_declares() {
        let capabilities = shipped("openalex", "https://example.test", &[]).capabilities();
        assert!(capabilities.lookup && capabilities.references);
        assert!(!capabilities.resolve_download);
        let controls = capabilities.browse.expect("openalex lists works");
        assert!(controls.keyword);
        assert!(controls.filters.iter().any(|filter| filter.name == "year"));

        let capabilities = source(KEYED_MANIFEST, "https://example.test", &[]).capabilities();
        assert!(capabilities.browse.is_none() && !capabilities.lookup && !capabilities.references);
    }
}
