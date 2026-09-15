//! A literature provider the user described rather than one we compiled.
//!
//! [`CustomSource`] implements [`LiteratureSource`] by reading a [`Manifest`]:
//! it builds a URL from a template, sends the identification the manifest
//! declares, and projects JSON paths or HTML CSS selections through
//! [`coerce`].
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
use dom_query::{Document, Matcher, Selection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use self::coerce::Projected;
use self::manifest::{
    search_field_type, FieldSpec, HttpParam, Manifest, ResolverStep, ResponseFormat,
    SearchCapability,
};
use self::selector::Selector;
use crate::integrations::LiteratureSource;
use crate::network::{ProviderHttpClient, ProviderHttpErrorKind};
use crate::types::{
    CustomIntegrationConfig, IntegrationState, IntegrationStatus, LiteratureAcquisition,
    LiteratureSearchResult, ResolvedLiteratureDownload,
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

        let request = match self.request(
            &substitute(
                &search.path,
                &[("query", PROBE_QUERY), ("limit", &PROBE_LIMIT.to_string())],
            ),
            &[],
        ) {
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

        match self.project_response(search, &body, PROBE_QUERY, PROBE_LIMIT) {
            Ok(projection) => {
                let records_seen = projection.records_seen;
                let issues_empty = projection.issues.is_empty();
                let resolver_error = if self.manifest.capabilities.resolve_download.is_some() {
                    match projection.results.first() {
                        Some(result) => self
                            .resolve_manifest_download(result)
                            .await
                            .err()
                            .map(|error| format!("resolve_download probe failed: {error:#}")),
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
            Err(error) => ProbeReport::failed(&self.id, "search", redacted, error.to_string()),
        }
    }

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

        let records_seen = items.len().min(limit);
        let mut results = Vec::with_capacity(records_seen);
        let mut issues = Vec::new();
        for (index, item) in items.iter().take(limit).enumerate() {
            match self.project_one(
                search,
                query,
                limit,
                index,
                &mut issues,
                |spec, field_type| self.select_json(spec, item, field_type),
            ) {
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
        let records_seen = items.length().min(limit);
        let mut results = Vec::with_capacity(records_seen);
        let mut issues = Vec::new();
        for (index, item) in items.iter().take(limit).enumerate() {
            match self.project_one(
                search,
                query,
                limit,
                index,
                &mut issues,
                |spec, field_type| self.select_html(spec, &item, field_type),
            ) {
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

    fn project_one<F>(
        &self,
        search: &SearchCapability,
        query: &str,
        limit: usize,
        index: usize,
        issues: &mut Vec<ProjectionIssue>,
        mut select: F,
    ) -> Option<LiteratureSearchResult>
    where
        F: FnMut(&FieldSpec, manifest::FieldType) -> Result<Option<Projected>, String>,
    {
        let mut text: HashMap<&str, String> = HashMap::new();
        let mut integer: HashMap<&str, i64> = HashMap::new();
        let mut boolean: HashMap<&str, bool> = HashMap::new();

        for (field, spec) in &search.fields {
            // Unknown field names are refused when the manifest is saved, so
            // one here means a manifest that reached storage another way.
            let Some(field_type) = search_field_type(field) else {
                continue;
            };
            let selected = match spec.input() {
                Some(input) => self.select_input(spec, input, query, limit, field_type),
                None => select(spec, field_type),
            };
            match selected {
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
        if !text.contains_key("title") {
            issues.push(ProjectionIssue {
                record: index,
                field: "title".to_string(),
                selector: search
                    .fields
                    .get("title")
                    .map(|spec| spec.paths().join(" | "))
                    .unwrap_or_default(),
                problem: "required field matched nothing".to_string(),
            });
        }
        let pdf_url = text.remove("pdf_url");
        let acquisition = if self.manifest.capabilities.resolve_download.is_some() {
            LiteratureAcquisition::Provider
        } else if pdf_url.is_some() {
            LiteratureAcquisition::Direct
        } else {
            LiteratureAcquisition::None
        };
        Some(LiteratureSearchResult {
            id,
            doi: text.remove("doi"),
            title: text.remove("title"),
            year: integer.remove("year"),
            publication_date: text.remove("publication_date"),
            venue: text.remove("venue"),
            citation_count: integer.remove("citation_count").unwrap_or(0),
            is_open_access: boolean.remove("is_open_access").unwrap_or(false),
            pdf_url,
            landing_page_url: text.remove("landing_page_url"),
            open_access_status: text.remove("open_access_status"),
            license: text.remove("license"),
            authors: text.remove("authors"),
            publisher: text.remove("publisher"),
            language: text.remove("language"),
            file_format: text.remove("file_format"),
            file_size: text.remove("file_size"),
            acquisition,
        })
    }

    fn select_json(
        &self,
        spec: &FieldSpec,
        item: &Value,
        field_type: manifest::FieldType,
    ) -> Result<Option<Projected>, String> {
        self.select_projected(spec, field_type, |path| {
            let selector = match Selector::parse(path) {
                Ok(selector) => selector,
                Err(error) => return Err(error),
            };
            Ok(selector.resolve(item).cloned())
        })
    }

    fn select_input(
        &self,
        spec: &FieldSpec,
        input: &str,
        query: &str,
        limit: usize,
        field_type: manifest::FieldType,
    ) -> Result<Option<Projected>, String> {
        let mut raw = match input {
            "query" => Value::String(query.to_string()),
            "limit" => Value::Number(limit.into()),
            // Refused during manifest validation.
            _ => return Err(format!("unknown search input '{input}'")),
        };
        if let Some((pattern, group)) = spec.capture() {
            raw = capture_value(pattern, group, &raw)?;
        }
        coerce::project(spec.coercion(), field_type, &raw, &self.base_url)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn select_html(
        &self,
        spec: &FieldSpec,
        item: &Selection<'_>,
        field_type: manifest::FieldType,
    ) -> Result<Option<Projected>, String> {
        self.select_projected(spec, field_type, |path| {
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
            if matches!(spec.coercion(), Some(coerce::Coercion::Join)) {
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

    fn select_projected<F>(
        &self,
        spec: &FieldSpec,
        field_type: manifest::FieldType,
        mut raw_for_path: F,
    ) -> Result<Option<Projected>, String>
    where
        F: FnMut(&str) -> Result<Option<Value>, String>,
    {
        let mut last_error = None;
        for path in spec.paths() {
            let Some(mut raw) = raw_for_path(path)? else {
                continue;
            };
            if let Some((pattern, group)) = spec.capture() {
                match capture_value(pattern, group, &raw) {
                    Ok(captured) => raw = captured,
                    Err(problem) => {
                        last_error = Some(format!("{path}: {problem}"));
                        continue;
                    }
                }
            }
            match coerce::project(spec.coercion(), field_type, &raw, &self.base_url) {
                Ok(value) => return Ok(Some(value)),
                Err(mismatch) => last_error = Some(format!("{path}: {mismatch}")),
            }
        }
        last_error.map_or(Ok(None), Err)
    }

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
            let request = self.request(&path, &step.params)?;
            let body = self.http.get_bytes(request.url, &request.headers).await?;
            let extracted = self.extract_step(step, &body).map_err(|error| {
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
            .map(|name| sanitize_filename(&name))
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
    ) -> anyhow::Result<HashMap<String, String>> {
        let mut out = HashMap::new();
        match step.response_format {
            ResponseFormat::Json => {
                let value: Value = serde_json::from_slice(body)
                    .map_err(|error| anyhow::anyhow!("response is not JSON: {error}"))?;
                for (name, spec) in &step.fields {
                    let projected = self
                        .select_json(spec, &value, resolver_field_type(spec))
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
                        .select_html(spec, &root, resolver_field_type(spec))
                        .map_err(|error| anyhow::anyhow!("field '{name}': {error}"))?
                        .ok_or_else(|| anyhow::anyhow!("field '{name}' matched nothing"))?;
                    out.insert(name.clone(), projected_string(projected));
                }
            }
        }
        Ok(out)
    }

    /// Assemble one request: the URL with its declared query parameters, the
    /// headers, and a copy of the URL with secrets removed for display.
    fn request<'a>(
        &'a self,
        path: &str,
        capability_params: &'a [HttpParam],
    ) -> anyhow::Result<PreparedRequest<'a>> {
        let mut url = Url::parse(&format!("{}{path}", self.base))?;
        anyhow::ensure!(
            url.origin() == self.base_url.origin(),
            "path '{path}' would send the request to {} instead of {}",
            url.origin().ascii_serialization(),
            self.base_url.origin().ascii_serialization()
        );

        let mut headers = Vec::new();
        let mut redacted_pairs: Vec<(String, String)> = Vec::new();
        for param in self.manifest.http.params.iter().chain(capability_params) {
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

fn resolver_field_type(spec: &FieldSpec) -> manifest::FieldType {
    match spec.coercion() {
        Some(coerce::Coercion::Int | coerce::Coercion::YearFromDate) => {
            manifest::FieldType::Integer
        }
        Some(coerce::Coercion::Bool) => manifest::FieldType::Boolean,
        Some(coerce::Coercion::AbsoluteUrl) => manifest::FieldType::Url,
        _ => manifest::FieldType::Text,
    }
}

fn projected_string(value: Projected) -> String {
    match value {
        Projected::Text(value) => value,
        Projected::Integer(value) => value.to_string(),
        Projected::Boolean(value) => value.to_string(),
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

fn sanitize_filename(value: &str) -> anyhow::Result<String> {
    let mut safe = String::new();
    for character in value.trim().chars() {
        if character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        {
            safe.push('_');
        } else {
            safe.push(character);
        }
    }
    while safe.contains("..") {
        safe = safe.replace("..", "_");
    }
    let safe: String = safe.chars().take(200).collect();
    anyhow::ensure!(
        !safe.trim().is_empty(),
        "resolved filename is empty after sanitizing"
    );
    Ok(safe)
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

        let request = self.request(
            &substitute(
                &search.path,
                &[("query", query), ("limit", &limit.to_string())],
            ),
            &[],
        )?;
        let body = match self.http.get_bytes(request.url, &request.headers).await {
            Ok(body) => body,
            Err(error) if error.kind == ProviderHttpErrorKind::RateLimited => {
                anyhow::bail!("{} rate limit reached", self.manifest.name)
            }
            Err(error) => return Err(error.into()),
        };
        let projection = self.project_response(search, &body, query, limit)?;
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

    async fn resolve_download(
        &self,
        result: &LiteratureSearchResult,
    ) -> anyhow::Result<Option<ResolvedLiteratureDownload>> {
        self.resolve_manifest_download(result).await
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

        let request = self.request(&health.path, &[])?;
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

    const ANNA_MANIFEST: &str = r#"
manifest_version = 1
id = "anna"
name = "Anna"

[http]
base_url = "BASE_URL"

[[http.params]]
location = "header"
name = "User-Agent"
value = "Mozilla/5.0 Wilkes"

[capabilities.search]
path = "/search?q={query}&content=journal"
response_format = "html"
items = '''div:has(> a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"])'''

[capabilities.search.fields]
id = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", capture = '''^/md5/([0-9a-f]+)$''' }
title = '''div.max-w-full a[href^="/md5/"]'''
authors = { path = '''div.max-w-full a[href^="/search"]:has(span[class="icon-[mdi--user-edit]"])''', coerce = "join" }
publisher = '''div.max-w-full a[href^="/search"]:has(span[class="icon-[mdi--company]"])'''
venue = '''div.max-w-full a[href^="/search"]:has(span[class="icon-[mdi--company]"])'''
language = { path = "div.text-gray-800", capture = '''^\s*✅?\s*([^\[]+?)\s*\[''' }
file_format = { path = "div.text-gray-800", capture = '''(?i)\b(EPUB|PDF|MOBI|AZW3|AZW|DJVU|CBZ|CBR|FB2|DOCX?|TXT)\b''' }
file_size = { path = "div.text-gray-800", capture = '''(?i)(\d+(?:\.\d+)?\s*(?:MB|KB|GB|TB))''' }
landing_page_url = { path = '''a[href^="/md5/"][class="custom-a block mr-2 sm:mr-4 hover:opacity-80"]''', attribute = "href", coerce = "absolute_url" }

[capabilities.resolve_download]
url = "{download_url}"
filename = "{title}.{file_format}"

[[capabilities.resolve_download.steps]]
path = "/dyn/api/fast_download.json?md5={id}"
response_format = "json"

[[capabilities.resolve_download.steps.params]]
location = "query"
name = "key"
secret = "anna_key"

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
manifest_version = 1
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
doi = { input = "query", coerce = "normalize_doi" }
title = '''div.max-w-full a[href^="/md5/"]'''

[capabilities.resolve_download]
url = "/scidb?doi={doi}"
filename = "{title}"
"#;

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

    #[tokio::test]
    async fn anna_html_search_and_credentialed_download_resolution_are_manifest_expressible() {
        let mut server = mockito::Server::new_async().await;
        let search = server
            .mock("GET", "/search")
            .match_query(mockito::Matcher::Any)
            .match_header("user-agent", "Mozilla/5.0 Wilkes")
            .with_status(200)
            .with_body(ANNA_BODY)
            .create_async()
            .await;
        let resolve = server
            .mock("GET", "/dyn/api/fast_download.json")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("md5".into(), "deadbeef".into()),
                mockito::Matcher::UrlEncoded("key".into(), "secret".into()),
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
        let request = pinned.request("//evil.test/works", &[]).unwrap();
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
