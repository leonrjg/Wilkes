//! What a user writes to describe a service, and what makes it valid.
//!
//! # Why a description and not a script
//!
//! Read `openalex/client.rs` and `semantic_scholar/client.rs` asking only
//! *what do they do?* Both build a URL from a template, GET it with one header
//! or query parameter for identification, walk into the JSON body, and project
//! about ten fields through a handful of normalizations. Twice, by hand, for
//! the same transformation.
//!
//! A manifest says that transformation instead of performing it. An embedded
//! scripting engine would say it too — along with everything else, at the cost
//! of a sandbox, a new runtime, and a manifest nobody can audit by reading it.
//! The three rules below are what keep this a description:
//!
//! 1. **Templates are typed substitution.** Each capability exposes a finite
//!    set of named values, the engine owns their encoding, and no request
//!    template may change the host or scheme — those come from `base_url` and
//!    are fixed when the manifest is saved.
//! 2. **The field map is a projection.** JSON paths or CSS selection only,
//!    with every transformation drawn from a closed vocabulary
//!    (see [`super::coerce`]).
//! 3. **Sequencing is finite and purpose-specific.** Search declares one GET;
//!    resolving a selected download may declare at most four origin-pinned
//!    GET steps. There are no branches, loops, arbitrary expressions, or
//!    writes.
//!
//! Anything a manifest cannot say, it says loudly by failing to load — never
//! by producing a plausible-looking wrong record.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde::{Deserialize, Serialize};
use url::Url;

use super::coerce::Coercion;
use super::selector::Selector;

/// The manifest format this build understands.
///
/// A manifest declaring anything else is refused rather than read
/// optimistically: a future version means fields whose meaning this build does
/// not know, and guessing at them is how a projection silently changes.
pub const MANIFEST_VERSION: u32 = 1;

/// Cap on a stored manifest, in bytes. Generous for a description of one
/// service, small enough that a pasted-in mistake is not persisted.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub manifest_version: u32,
    /// Slug, unique among custom integrations. Namespaced as `custom:<id>`
    /// wherever a provider is named, so a manifest can never shadow a built-in.
    pub id: String,
    pub name: String,
    pub http: HttpSpec,
    #[serde(default)]
    pub capabilities: Capabilities,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HttpSpec {
    /// The one host this manifest may ever contact. Every request is this,
    /// plus a capability's path; no template can reach anywhere else.
    pub base_url: String,
    /// Identification the service requires, sent on every request.
    #[serde(default)]
    pub params: Vec<HttpParam>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ParamLocation {
    Header,
    Query,
}

impl ParamLocation {
    const ALL: &'static [Self] = &[Self::Header, Self::Query];

    fn name(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Query => "query",
        }
    }
}

/// One identification parameter.
///
/// `value` and `secret` are separated because they are handled differently,
/// not because a service can tell them apart: a `value` is part of the
/// manifest and travels with it when exported (OpenAlex's `mailto=`, a
/// contact address, a fixed API version), while a `secret` is only a *name*
/// whose value is stored beside the manifest and is stripped on export. Exactly
/// one of the two must be set.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HttpParam {
    pub location: ParamLocation,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<SearchCapability>,
    /// Resolve a selected search result to a file URL. Resolution is kept
    /// separate from search so a result list never spends credentials or one
    /// request per row merely to show itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolve_download: Option<ResolveDownloadCapability>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    #[default]
    Json,
    Html,
}

impl ResponseFormat {
    const ALL: &'static [Self] = &[Self::Json, Self::Html];

    fn name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Html => "html",
        }
    }
}

/// A request whose only job is to prove the service answers.
///
/// Its response is not read: a status code is the whole answer, which is what
/// both built-in clients' `status` already do with a fixed probe DOI.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HealthCapability {
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SearchCapability {
    /// Path and query, relative to `base_url`. May use `{query}` and `{limit}`.
    pub path: String,
    /// How selectors in this capability are interpreted. Omitted means JSON,
    /// preserving every version-1 manifest written before HTML support.
    #[serde(default)]
    pub response_format: ResponseFormat,
    /// Where the records are in the response: a JSON array selector, or an
    /// HTML CSS selector. Omitted only when a JSON body is itself the array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<String>,
    /// Result field name → where to find it in one record.
    pub fields: BTreeMap<String, FieldSpec>,
}

/// A finite, acyclic resolver from one selected result to a downloadable URL.
///
/// Every step is a GET to the manifest's pinned origin. Its named fields
/// become placeholders available only to later steps and to the final output.
/// The final URL is returned to Wilkes' existing downloader; this capability
/// never writes a byte itself.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResolveDownloadCapability {
    #[serde(default)]
    pub steps: Vec<ResolverStep>,
    /// A template using result fields and values extracted by prior steps.
    pub url: String,
    /// Optional caller-chosen filename. When absent, the downloader derives
    /// one from the URL and response content type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResolverStep {
    pub path: String,
    #[serde(default)]
    pub response_format: ResponseFormat,
    /// Parameters used by this step alone. A download credential therefore
    /// need not be sent with the provider's public search request.
    #[serde(default)]
    pub params: Vec<HttpParam>,
    /// Placeholder name -> selector in this response.
    pub fields: BTreeMap<String, FieldSpec>,
}

/// How one output field is found.
///
/// The bare-string form is the common case (`title = "display_name"`); the
/// object form adds ordered fallback selectors, a coercion, HTML attribute
/// extraction, or a regular-expression capture.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum FieldSpec {
    Path(String),
    Mapped {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Selectors tried in order until one resolves. Not a fallback in the
        /// sense the project forbids — it is one service documenting two
        /// places it puts the same fact, which OpenAlex does with `doi` and
        /// `ids.doi`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        first_of: Vec<String>,
        /// Use one of the search capability's typed inputs (`query` or
        /// `limit`) instead of selecting from the response. This is how a DOI
        /// lookup carries the requested DOI into its later download resolver
        /// when the HTML result does not repeat it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        coerce: Option<Coercion>,
        /// For HTML, read this attribute from the first matched element
        /// instead of its text. JSON selectors may not set it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attribute: Option<String>,
        /// Apply a Rust regular expression to selected text and keep one
        /// capture. This is deliberately a bounded transformation rather than
        /// an embedded program; Rust's regex engine has no catastrophic
        /// backtracking.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capture: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capture_group: Option<usize>,
    },
}

impl FieldSpec {
    /// The selector strings this field will try, in order.
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Self::Path(path) => vec![path.as_str()],
            Self::Mapped { path, first_of, .. } => path
                .iter()
                .map(String::as_str)
                .chain(first_of.iter().map(String::as_str))
                .collect(),
        }
    }

    pub fn coercion(&self) -> Option<Coercion> {
        match self {
            Self::Path(_) => None,
            Self::Mapped { coerce, .. } => *coerce,
        }
    }

    pub fn input(&self) -> Option<&str> {
        match self {
            Self::Path(_) => None,
            Self::Mapped { input, .. } => input.as_deref(),
        }
    }

    pub fn attribute(&self) -> Option<&str> {
        match self {
            Self::Path(_) => None,
            Self::Mapped { attribute, .. } => attribute.as_deref(),
        }
    }

    pub fn capture(&self) -> Option<(&str, usize)> {
        match self {
            Self::Path(_) => None,
            Self::Mapped {
                capture,
                capture_group,
                ..
            } => capture
                .as_deref()
                .map(|pattern| (pattern, capture_group.unwrap_or(1))),
        }
    }
}

/// The output fields a search capability may name, and the type each one
/// holds. Compiled in from [`crate::types::LiteratureSearchResult`] because
/// that is the contract a custom provider is being mapped *to*; a manifest
/// naming anything else is a typo, and is refused as one.
pub const SEARCH_FIELDS: &[(&str, FieldType)] = &[
    ("id", FieldType::Text),
    ("doi", FieldType::Text),
    ("title", FieldType::Text),
    ("year", FieldType::Integer),
    ("publication_date", FieldType::Text),
    ("venue", FieldType::Text),
    ("citation_count", FieldType::Integer),
    ("is_open_access", FieldType::Boolean),
    ("pdf_url", FieldType::Url),
    ("landing_page_url", FieldType::Url),
    ("open_access_status", FieldType::Text),
    ("license", FieldType::Text),
    ("authors", FieldType::Text),
    ("publisher", FieldType::Text),
    ("language", FieldType::Text),
    ("file_format", FieldType::Text),
    ("file_size", FieldType::Text),
];

/// Fields without which a result cannot be used: `id` because nothing can be
/// deduplicated or downloaded without one, `title` because a result the user
/// cannot read is not a result.
pub const REQUIRED_SEARCH_FIELDS: &[&str] = &["id", "title"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    Text,
    Integer,
    Boolean,
    Url,
}

impl FieldType {
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::Url => "url",
        }
    }
}

pub fn search_field_type(field: &str) -> Option<FieldType> {
    SEARCH_FIELDS
        .iter()
        .find(|(name, _)| *name == field)
        .map(|(_, ty)| *ty)
}

/// Placeholders a capability may use, by capability.
const SEARCH_PLACEHOLDERS: &[&str] = &["query", "limit"];
const HEALTH_PLACEHOLDERS: &[&str] = &[];
const MAX_RESOLVER_STEPS: usize = 4;

const AUTHORING_JSON_EXAMPLE: &str = r#"manifest_version = 1
id = "example-json"
name = "Example JSON service"

[http]
base_url = "https://api.example.test"

[capabilities.search]
path = "/works?q={query}&limit={limit}"
response_format = "json"
items = "results[*]"

[capabilities.search.fields]
id = "id"
title = "title"
doi = { path = "doi", coerce = "normalize_doi" }
year = { path = "published", coerce = "year_from_date" }
pdf_url = "links.pdf"
"#;

const AUTHORING_HTML_EXAMPLE: &str = r#"manifest_version = 1
id = "example-html"
name = "Example HTML service"

[http]
base_url = "https://library.example.test"

[[http.params]]
location = "header"
name = "User-Agent"
value = "Mozilla/5.0"

[capabilities.search]
path = "/search?q={query}"
response_format = "html"
items = "article.result"

[capabilities.search.fields]
id = { path = "a.download", attribute = "data-id" }
title = "h2"
authors = { path = "a.author", coerce = "join" }
file_format = { path = ".metadata", capture = '''(?i)\b(PDF|EPUB)\b''' }
landing_page_url = { path = "a.details", attribute = "href", coerce = "absolute_url" }

[capabilities.resolve_download]
url = "{download_url}"
filename = "{title}.{file_format}"

[[capabilities.resolve_download.steps]]
path = "/api/download?id={id}"
response_format = "json"

[[capabilities.resolve_download.steps.params]]
location = "query"
name = "key"
secret = "member_key"

[capabilities.resolve_download.steps.fields]
download_url = "url"
"#;

/// A self-contained prompt for translating service documentation or an
/// existing client into a manifest. The vocabulary is assembled from the same
/// registries validation uses; examples live beside the parser and are parsed
/// by its tests. The UI only copies this answer and owns no schema copy.
pub fn authoring_prompt() -> String {
    let fields = SEARCH_FIELDS
        .iter()
        .map(|(name, field_type)| {
            let required = if REQUIRED_SEARCH_FIELDS.contains(name) {
                " (required)"
            } else {
                ""
            };
            format!("- {name}: {}{required}", field_type.name())
        })
        .collect::<Vec<_>>()
        .join("\n");
    let coercions = Coercion::ALL
        .iter()
        .map(|coercion| format!("- {}: {}", coercion.name(), coercion.authoring_hint()))
        .collect::<Vec<_>>()
        .join("\n");
    let response_formats = ResponseFormat::ALL
        .iter()
        .map(|format| format.name())
        .collect::<Vec<_>>()
        .join(", ");
    let param_locations = ParamLocation::ALL
        .iter()
        .map(|location| location.name())
        .collect::<Vec<_>>()
        .join(", ");

    let mut prompt = String::new();
    writeln!(
        prompt,
        "You are translating an HTTP service, its documentation, or an existing client script into a Wilkes custom-integration manifest."
    )
    .unwrap();
    prompt.push_str(
        r#"
Read all supplied source material before writing the manifest. Preserve the service's selectors, request paths, metadata parsing, headers, and authentication semantics. Do not invent endpoints or silently omit required behavior.

Return exactly one TOML manifest and no Markdown fence or commentary. If the service cannot be represented by the bounded schema below, return `UNSUPPORTED: ` followed by the exact missing capability instead of producing a partial manifest.

Safety and fidelity rules:
- Use only HTTP(S) GET requests. Search is one request. Download resolution is on demand after selection and may use only the finite steps declared below.
- Never put a credential value in the manifest. Declare `secret = "symbolic_name"`; use `value` only for public fixed values.
- Every request step remains on `http.base_url`. A resolved final download URL may be external because Wilkes passes it to its existing bounded downloader.
- Missing or unparseable required values are errors. Do not invent defaults.
- Prefer exact selectors and explicit coercions. JSON is the default response format; state HTML explicitly.

TOML schema (comments name optional fields and alternatives):

manifest_version = 1
id = "lowercase-slug"                 # required; a-z, 0-9, - or _; at most 64 characters
name = "Human-readable name"          # required

[http]
base_url = "https://one-origin.test"  # required; http or https

[[http.params]]                       # optional, repeatable; sent on every request
location = "header"                     # or "query"
name = "parameter-name"
value = "public fixed value"          # choose exactly one of value or secret
# secret = "symbolic_secret_name"

[capabilities.health]                 # optional
path = "/health"                      # no placeholders

[capabilities.search]                 # required when resolve_download exists
path = "/search?q={query}&limit={limit}"
response_format = "json"              # optional; "json" or "html"; defaults to json
items = "selector"                    # optional JSON array selector; required CSS selector for HTML

[capabilities.search.fields]
# Each key is one result field listed below. A field specification is either:
field = "selector"
# or an object with a primary path and/or ordered fallback paths:
field = { path = "selector", first_of = ["fallback.selector"], coerce = "coercion", attribute = "href", capture = "regex with a capture", capture_group = 1 }
# or a search input instead of a response selector:
field = { input = "query", coerce = "coercion" } # input is "query" or "limit"
# `attribute` is HTML-only. `join` collects all matching HTML elements or joins a JSON string array. Regex capture defaults to group 1.

[capabilities.resolve_download]       # optional; runs only after the user selects a result
url = "{result_or_step_value}"        # required; HTTP(S) or relative to base_url
filename = "{title}.{file_format}"   # optional; sanitized before download

[[capabilities.resolve_download.steps]]
path = "/resolve?id={id}"             # result values plus outputs from earlier steps
response_format = "json"              # optional; "json" or "html"; defaults to json

[[capabilities.resolve_download.steps.params]]
location = "query"                    # or "header"
name = "parameter-name"
value = "public fixed value"          # choose exactly one of value or secret
# secret = "symbolic_secret_name"

[capabilities.resolve_download.steps.fields]
new_value = "selector"                # required scalar; the full field-spec object form is also allowed

Search JSON selectors use dotted keys, `[n]`, and a trailing `[*]`. HTML selectors are CSS selectors. `first_of` is ordered fallback, not boolean OR.

Search result fields:
"#,
    );
    writeln!(prompt, "{fields}").unwrap();
    writeln!(prompt, "\nSupported coercions:\n{coercions}").unwrap();
    writeln!(
        prompt,
        "Closed enum values: response_format = {response_formats}; parameter location = {param_locations}."
    )
    .unwrap();
    writeln!(
        prompt,
        "Resolver steps are acyclic, origin-pinned, and limited to {MAX_RESOLVER_STEPS}. They have no loops, branches, arbitrary expressions, POST requests, browser JavaScript, pagination, or writes."
    )
    .unwrap();
    prompt.push_str("\nValid JSON example:\n\n");
    prompt.push_str(AUTHORING_JSON_EXAMPLE);
    prompt.push_str("\nValid HTML plus download-resolution example:\n\n");
    prompt.push_str(AUTHORING_HTML_EXAMPLE);
    prompt.push_str(
        "\nSOURCE MATERIAL TO TRANSLATE\nPaste the service documentation, HTTP examples, or existing implementation below this line before sending the prompt to the model.\n",
    );
    prompt
}

impl Manifest {
    pub fn parse(source: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            source.len() <= MAX_MANIFEST_BYTES,
            "manifest is too long ({} bytes, limit {MAX_MANIFEST_BYTES})",
            source.len()
        );
        let manifest: Manifest = if source.trim_start().starts_with('{') {
            serde_json::from_str(source)?
        } else {
            toml::from_str(source)?
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Everything checkable without touching the network.
    ///
    /// Deliberately exhaustive rather than first-error: a user fixing a pasted
    /// manifest wants the whole list, and finding one more typo per save is
    /// the worst version of this feature.
    pub fn validate(&self) -> anyhow::Result<()> {
        let problems = self.problems();
        anyhow::ensure!(
            problems.is_empty(),
            "manifest is invalid:\n  - {}",
            problems.join("\n  - ")
        );
        Ok(())
    }

    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();

        if self.manifest_version != MANIFEST_VERSION {
            problems.push(format!(
                "manifest_version {} is not supported (this build reads version {MANIFEST_VERSION})",
                self.manifest_version
            ));
        }
        if !is_slug(&self.id) {
            problems.push(format!(
                "id '{}' must be 1-64 characters of a-z, 0-9, '-' or '_'",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            problems.push("name cannot be empty".to_string());
        }

        match Url::parse(&self.http.base_url) {
            Ok(url) if !matches!(url.scheme(), "http" | "https") => problems.push(format!(
                "base_url scheme '{}' is not http or https",
                url.scheme()
            )),
            Ok(url) if url.host_str().is_none() => {
                problems.push("base_url has no host".to_string())
            }
            Ok(_) => {}
            Err(error) => problems.push(format!("base_url is not a URL: {error}")),
        }

        problems.extend(param_problems("http.params", &self.http.params));

        if self.capabilities.health.is_none()
            && self.capabilities.search.is_none()
            && self.capabilities.resolve_download.is_none()
        {
            problems.push("manifest declares no capabilities".to_string());
        }

        if let Some(health) = &self.capabilities.health {
            problems.extend(template_problems(
                "health.path",
                &health.path,
                HEALTH_PLACEHOLDERS,
            ));
        }

        if let Some(search) = &self.capabilities.search {
            problems.extend(template_problems(
                "search.path",
                &search.path,
                SEARCH_PLACEHOLDERS,
            ));
            match (&search.response_format, &search.items) {
                (ResponseFormat::Html, None) => {
                    problems.push("search.items is required for an HTML response".to_string())
                }
                (format, Some(items)) => {
                    if let Err(error) = selector_problem(*format, items) {
                        problems.push(format!("search.items: {error}"));
                    }
                }
                (ResponseFormat::Json, None) => {}
            }
            problems.extend(self.search_field_problems(search));
        }

        if let Some(resolve) = &self.capabilities.resolve_download {
            if self.capabilities.search.is_none() {
                problems.push(
                    "resolve_download requires search because it resolves a selected result"
                        .to_string(),
                );
            }
            problems.extend(self.resolve_download_problems(resolve));
        }

        problems
    }

    fn search_field_problems(&self, search: &SearchCapability) -> Vec<String> {
        let mut problems = Vec::new();

        for required in REQUIRED_SEARCH_FIELDS {
            if !search.fields.contains_key(*required) {
                problems.push(format!(
                    "search.fields is missing '{required}', which every result needs"
                ));
            }
        }

        for (field, spec) in &search.fields {
            let Some(field_type) = search_field_type(field) else {
                problems.push(format!(
                    "search.fields.{field} is not a result field; expected one of: {}",
                    SEARCH_FIELDS
                        .iter()
                        .map(|(name, _)| *name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                continue;
            };

            let paths = spec.paths();
            match (paths.is_empty(), spec.input()) {
                (true, None) => problems.push(format!(
                    "search.fields.{field} names neither a selector nor an input"
                )),
                (false, Some(_)) => problems.push(format!(
                    "search.fields.{field} sets both response selectors and input"
                )),
                _ => {}
            }
            if let Some(input) = spec.input() {
                if !SEARCH_PLACEHOLDERS.contains(&input) {
                    problems.push(format!(
                        "search.fields.{field}.input '{input}' is not available; expected query or limit"
                    ));
                }
            }
            for path in paths {
                if let Err(error) = selector_problem(search.response_format, path) {
                    problems.push(format!("search.fields.{field}: {error}"));
                }
            }

            problems.extend(field_spec_problems(
                &format!("search.fields.{field}"),
                search.response_format,
                spec,
            ));

            if let Some(coercion) = spec.coercion() {
                if !coercion.produces(field_type) {
                    problems.push(format!(
                        "search.fields.{field} is {}, which coercion '{}' cannot produce",
                        field_type.name(),
                        coercion.name()
                    ));
                }
            }
        }

        problems
    }

    fn resolve_download_problems(&self, resolve: &ResolveDownloadCapability) -> Vec<String> {
        let mut problems = Vec::new();
        if resolve.steps.len() > MAX_RESOLVER_STEPS {
            problems.push(format!(
                "resolve_download has {} steps; the limit is {MAX_RESOLVER_STEPS}",
                resolve.steps.len()
            ));
        }

        let mut available: Vec<String> = SEARCH_FIELDS
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect();
        for (index, step) in resolve.steps.iter().enumerate() {
            let label = format!("resolve_download.steps[{index}]");
            problems.extend(template_problems_owned(
                &format!("{label}.path"),
                &step.path,
                &available,
                true,
            ));
            problems.extend(param_problems(&format!("{label}.params"), &step.params));
            if step.fields.is_empty() {
                problems.push(format!("{label}.fields cannot be empty"));
            }
            for (name, spec) in &step.fields {
                if !is_variable(name) {
                    problems.push(format!(
                        "{label}.fields.{name} is not a valid placeholder name"
                    ));
                }
                if available.iter().any(|existing| existing == name) {
                    problems.push(format!(
                        "{label}.fields.{name} shadows an existing resolver value"
                    ));
                }
                let paths = spec.paths();
                if paths.is_empty() {
                    problems.push(format!("{label}.fields.{name} names no selector"));
                }
                if spec.input().is_some() {
                    problems.push(format!(
                        "{label}.fields.{name}.input is only available to search fields"
                    ));
                }
                for path in paths {
                    if let Err(error) = selector_problem(step.response_format, path) {
                        problems.push(format!("{label}.fields.{name}: {error}"));
                    }
                }
                problems.extend(field_spec_problems(
                    &format!("{label}.fields.{name}"),
                    step.response_format,
                    spec,
                ));
                available.push(name.clone());
            }
        }

        problems.extend(template_problems_owned(
            "resolve_download.url",
            &resolve.url,
            &available,
            false,
        ));
        if resolve.url.trim().is_empty() {
            problems.push("resolve_download.url cannot be empty".to_string());
        }
        if let Some(filename) = &resolve.filename {
            problems.extend(template_problems_owned(
                "resolve_download.filename",
                filename,
                &available,
                false,
            ));
            if filename.trim().is_empty() {
                problems.push("resolve_download.filename cannot be empty".to_string());
            }
        }
        problems
    }

    /// The one host this manifest may contact, for the import dialog to show
    /// before anything is saved.
    pub fn host(&self) -> Option<String> {
        Url::parse(&self.http.base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
    }

    /// Names of the secrets this manifest needs supplied.
    pub fn required_secrets(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .http
            .params
            .iter()
            .filter_map(|param| param.secret.as_deref())
            .collect();
        if let Some(resolve) = &self.capabilities.resolve_download {
            names.extend(
                resolve
                    .steps
                    .iter()
                    .flat_map(|step| &step.params)
                    .filter_map(|param| param.secret.as_deref()),
            );
        }
        names.sort_unstable();
        names.dedup();
        names
    }
}

fn selector_problem(format: ResponseFormat, selector: &str) -> Result<(), String> {
    match format {
        ResponseFormat::Json => Selector::parse(selector).map(|_| ()),
        ResponseFormat::Html => dom_query::Matcher::new(selector)
            .map(|_| ())
            .map_err(|error| format!("invalid CSS selector: {error:?}")),
    }
}

fn field_spec_problems(label: &str, format: ResponseFormat, spec: &FieldSpec) -> Vec<String> {
    let mut problems = Vec::new();
    if spec.input().is_some() && spec.attribute().is_some() {
        problems.push(format!("{label}.attribute cannot be used with input"));
    } else if format == ResponseFormat::Json && spec.attribute().is_some() {
        problems.push(format!(
            "{label}.attribute is only available for HTML responses"
        ));
    }
    if spec.attribute().is_some_and(|name| name.trim().is_empty()) {
        problems.push(format!("{label}.attribute cannot be empty"));
    }
    if matches!(
        spec,
        FieldSpec::Mapped {
            capture: None,
            capture_group: Some(_),
            ..
        }
    ) {
        problems.push(format!("{label}.capture_group requires capture"));
    }
    if let Some((pattern, group)) = spec.capture() {
        match regex::Regex::new(pattern) {
            Ok(regex) if group >= regex.captures_len() => problems.push(format!(
                "{label}.capture_group {group} does not exist in the regular expression"
            )),
            Ok(_) => {}
            Err(error) => problems.push(format!("{label}.capture is invalid: {error}")),
        }
    }
    problems
}

fn param_problems(label: &str, params: &[HttpParam]) -> Vec<String> {
    let mut problems = Vec::new();
    for param in params {
        match (&param.value, &param.secret) {
            (Some(_), Some(_)) => problems.push(format!(
                "{label} param '{}' sets both value and secret; it must set exactly one",
                param.name
            )),
            (None, None) => problems.push(format!(
                "{label} param '{}' sets neither value nor secret",
                param.name
            )),
            _ => {}
        }
        if param.name.trim().is_empty() {
            problems.push(format!("{label} has an empty parameter name"));
        }
        if param.location == ParamLocation::Header && !is_header_name(&param.name) {
            problems.push(format!(
                "{label} param '{}' is not a valid header name",
                param.name
            ));
        }
    }
    problems
}

/// Placeholders present in a template, and whether they are ones this
/// capability supplies.
fn placeholder_problems(field: &str, template: &str, allowed: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut rest = template;

    while let Some((_, after)) = rest.split_once('{') {
        let Some((placeholder, tail)) = after.split_once('}') else {
            problems.push(format!("{field}: unclosed '{{' in template"));
            return problems;
        };
        if !allowed.contains(&placeholder) {
            problems.push(match allowed.is_empty() {
                true => format!("{field}: '{{{placeholder}}}' is not available here"),
                false => format!(
                    "{field}: '{{{placeholder}}}' is not available here; this capability supplies {}",
                    allowed
                        .iter()
                        .map(|p| format!("{{{p}}}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        rest = tail;
    }

    problems
}

fn template_problems(field: &str, template: &str, allowed: &[&str]) -> Vec<String> {
    let mut problems = placeholder_problems(field, template, allowed);
    if !template.starts_with('/') {
        problems.push(format!("{field}: must start with '/'"));
    }
    problems
}

fn template_problems_owned(
    field: &str,
    template: &str,
    allowed: &[String],
    require_path: bool,
) -> Vec<String> {
    let borrowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
    match require_path {
        true => template_problems(field, template, &borrowed),
        false => placeholder_problems(field, template, &borrowed),
    }
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn is_variable(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_lowercase() || first == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_header_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const CROSSREF: &str = r#"
manifest_version = 1
id = "crossref"
name = "Crossref"

[http]
base_url = "https://api.crossref.org"

[[http.params]]
location = "query"
name = "mailto"
value = "team@example.com"

[capabilities.health]
path = "/works/10.1145/3801158"

[capabilities.search]
path = "/works?query.bibliographic={query}&rows={limit}"
items = "message.items[*]"

[capabilities.search.fields]
id = "DOI"
title = "title[0]"
doi = { path = "DOI", coerce = "normalize_doi" }
year = { path = "published.date-parts[0][0]", coerce = "int" }
citation_count = { path = "is-referenced-by-count", coerce = "int" }
pdf_url = { first_of = ["link[0].URL", "resource.primary.URL"] }
"#;

    const HTML: &str = r#"
manifest_version = 1
id = "html"
name = "HTML"
[http]
base_url = "https://example.test"
[capabilities.search]
path = "/search?q={query}"
response_format = "html"
items = '''article:has(a[href^="/work/"])'''
[capabilities.search.fields]
id = { path = '''a[href^="/work/"]''', attribute = "href", capture = '''^/work/(.+)$''' }
title = "h2"
"#;

    #[test]
    fn parses_a_toml_manifest() {
        let manifest = Manifest::parse(CROSSREF).unwrap();
        assert_eq!(manifest.id, "crossref");
        assert_eq!(manifest.host().as_deref(), Some("api.crossref.org"));
        assert!(manifest.required_secrets().is_empty());
    }

    #[test]
    fn round_trips_through_json() {
        let manifest = Manifest::parse(CROSSREF).unwrap();
        let json = serde_json::to_string(&manifest).unwrap();
        assert_eq!(Manifest::parse(&json).unwrap(), manifest);
    }

    #[test]
    fn rejects_an_unknown_result_field() {
        let source = CROSSREF.replace("citation_count = ", "citations = ");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("citations"), "{error}");
        assert!(error.contains("is not a result field"), "{error}");
    }

    #[test]
    fn rejects_an_unavailable_placeholder() {
        let source = CROSSREF.replace("{query}", "{doi}");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("{doi}"), "{error}");
        assert!(error.contains("not available here"), "{error}");
    }

    #[test]
    fn rejects_a_coercion_the_field_type_cannot_hold() {
        let source = CROSSREF.replace(
            r#"title = "title[0]""#,
            r#"title = { path = "title[0]", coerce = "int" }"#,
        );
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("search.fields.title"), "{error}");
        assert!(error.contains("int"), "{error}");
    }

    #[test]
    fn rejects_a_missing_required_field() {
        let source = CROSSREF.replace(r#"title = "title[0]""#, "");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("missing 'title'"), "{error}");
    }

    #[test]
    fn rejects_a_non_http_base_url() {
        let source = CROSSREF.replace("https://api.crossref.org", "file:///etc/passwd");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("not http or https"), "{error}");
    }

    #[test]
    fn rejects_a_param_that_is_both_value_and_secret() {
        let source = CROSSREF.replace(
            r#"value = "team@example.com""#,
            r#"value = "team@example.com"
secret = "token""#,
        );
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("exactly one"), "{error}");
    }

    #[test]
    fn reports_every_problem_at_once() {
        let source = CROSSREF
            .replace("citation_count = ", "citations = ")
            .replace("{query}", "{doi}");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("citations"), "{error}");
        assert!(error.contains("{doi}"), "{error}");
    }

    #[test]
    fn validates_html_css_attributes_and_captures() {
        let manifest = Manifest::parse(HTML).unwrap();
        assert_eq!(
            manifest.capabilities.search.unwrap().response_format,
            ResponseFormat::Html
        );
    }

    #[test]
    fn rejects_an_invalid_css_selector_before_network_use() {
        let error = Manifest::parse(&HTML.replace("title = \"h2\"", "title = \"h2[\""))
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid CSS selector"), "{error}");
    }

    #[test]
    fn rejects_html_attribute_extraction_on_json() {
        let source = CROSSREF.replace(
            r#"title = "title[0]""#,
            r#"title = { path = "title[0]", attribute = "href" }"#,
        );
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(
            error.contains("attribute is only available for HTML"),
            "{error}"
        );
    }

    #[test]
    fn resolver_templates_cannot_read_an_undeclared_value() {
        let source =
            format!("{HTML}\n[capabilities.resolve_download]\nurl = \"{{not_from_any_step}}\"\n");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("not_from_any_step"), "{error}");
        assert!(error.contains("not available"), "{error}");
    }

    #[test]
    fn a_download_resolver_without_search_is_refused_as_unreachable() {
        let source = r#"
manifest_version = 1
id = "orphan"
name = "Orphan"
[http]
base_url = "https://example.test"
[capabilities.resolve_download]
url = "{id}"
"#;
        let error = Manifest::parse(source).unwrap_err().to_string();
        assert!(
            error.contains("resolve_download requires search"),
            "{error}"
        );
    }

    #[test]
    fn authoring_prompt_examples_are_manifests_the_parser_accepts() {
        Manifest::parse(AUTHORING_JSON_EXAMPLE).unwrap();
        Manifest::parse(AUTHORING_HTML_EXAMPLE).unwrap();
    }

    #[test]
    fn authoring_prompt_is_derived_from_the_complete_validation_vocabulary() {
        let prompt = authoring_prompt();
        for (field, field_type) in SEARCH_FIELDS {
            assert!(
                prompt.contains(&format!("- {field}: {}", field_type.name())),
                "prompt omitted search field {field}"
            );
        }
        for coercion in Coercion::ALL {
            assert!(
                prompt.contains(coercion.name()),
                "prompt omitted coercion {}",
                coercion.name()
            );
        }
        for format in ResponseFormat::ALL {
            assert!(prompt.contains(format.name()));
        }
        for location in ParamLocation::ALL {
            assert!(prompt.contains(location.name()));
        }
        for required in REQUIRED_SEARCH_FIELDS {
            let field_type = search_field_type(required).unwrap();
            assert!(prompt.contains(&format!("- {required}: {} (required)", field_type.name())));
        }
        assert!(prompt.contains(AUTHORING_JSON_EXAMPLE));
        assert!(prompt.contains(AUTHORING_HTML_EXAMPLE));
        assert!(prompt.contains(&format!("limited to {MAX_RESOLVER_STEPS}")));
        assert!(prompt.ends_with("before sending the prompt to the model.\n"));
    }
}
