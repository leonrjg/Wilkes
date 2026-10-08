//! What a user writes to describe a service, and what makes it valid.
//!
//! # Why a description and not a script
//!
//! Every provider Wilkes talks to — the ones shipped with it and the ones a
//! user pastes in — does the same thing: build a URL from a template, GET it
//! with a header or query parameter for identification, walk into the response,
//! and project about ten fields through a handful of normalizations. Written by
//! hand that is the same transformation once per provider; said in a manifest
//! it is a document anyone can read. The three rules below are what keep this
//! a description:
//!
//! 1. **Templates are typed substitution.** Each capability exposes a finite
//!    set of named values, the engine owns their encoding, and no request
//!    template may change the host or scheme — those come from a literal
//!    `base_url` and are visible before the manifest is saved.
//! 2. **The field map is a projection.** A JMESPath expression (or, for HTML,
//!    a CSS selector) per field, over a function set drawn from a closed list
//!    (see [`super::expr`]). No assignment, no recursion, no I/O.
//! 3. **Sequencing is finite and purpose-specific.** Search declares one GET.
//!    Looking a work up may declare up to three alternative GETs, tried in
//!    order until one yields a record. Resolving a download may declare up to
//!    four origin-pinned GET steps. Resolving references is one GET for the
//!    work and one batched GET per fixed-size chunk of its references. There
//!    are no branches, loops, arbitrary expressions, or writes.
//!
//! Anything a manifest cannot say, it says loudly by failing to load — never
//! by producing a plausible-looking wrong record.

use std::collections::BTreeMap;
use std::fmt::Write;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use url::Url;

use super::expr;

/// The manifest format this build understands.
///
/// A manifest declaring anything else is refused rather than read
/// optimistically: a future version means fields whose meaning this build does
/// not know, and guessing at them is how a projection silently changes. A
/// *past* version is not read either — it is converted once, when settings
/// load, by [`super::legacy`].
pub const MANIFEST_VERSION: u32 = 2;

/// Cap on a stored manifest, in bytes. Generous for a description of one
/// service, small enough that a pasted-in mistake is not persisted.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub manifest_version: u32,
    /// Slug, unique among custom integrations. Namespaced as `custom:<id>`
    /// wherever a provider is named, so a manifest can never shadow a
    /// built-in.
    pub id: String,
    pub name: String,
    pub http: HttpSpec,
    #[serde(default)]
    pub capabilities: Capabilities,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HttpSpec {
    /// The origin used by health, search, and resolver steps that do not
    /// declare their own base. Every path is pinned to its effective origin.
    pub base_url: String,
    /// Headers the primary service requires, sent to the primary origin and to
    /// resolver steps on that same origin. Never to another origin.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, ParamValue>,
    /// Query parameters, with the same reach as `headers`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, ParamValue>,
}

/// One identification value: a literal that travels with the manifest, or the
/// *name* of a secret whose value is stored beside it and stripped on export.
///
/// A plain string is the common case (`User-Agent = "Mozilla/5.0"`); the table
/// form names a secret (`mailto = { secret = "email", optional = true }`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ParamValue {
    Literal(String),
    Secret(SecretRef),
}

/// A reference to a per-installation value.
///
/// `optional` is what makes OpenAlex's contact address and Semantic Scholar's
/// API key expressible: a service that works without them, better with them,
/// must not refuse to run while they are unset. A required secret that has no
/// value stops the request before it is sent; an optional one is left off it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    pub secret: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub optional: bool,
    /// What the settings form calls it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// How the settings form masks and validates it. Defaults to a password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<SecretKind>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    Text,
    Email,
    Password,
}

impl SecretKind {
    const ALL: &'static [Self] = &[Self::Text, Self::Email, Self::Password];

    fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Email => "email",
            Self::Password => "password",
        }
    }
}

/// A secret a manifest names, as the settings form needs to render it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretSlot {
    pub name: String,
    pub label: String,
    pub kind: SecretKind,
    pub optional: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<SearchCapability>,
    /// Filtered, sorted, paged listing of the same records `search` returns,
    /// with the keyword optional. It reuses `search`'s projection: a service
    /// that lists answers in the shape it searches in, and a second field map
    /// would be a second place for the two to disagree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browse: Option<BrowseCapability>,
    /// Resolve a selected search result to a file URL. Resolution is kept
    /// separate from search so a result list never spends credentials or one
    /// request per row merely to show itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolve_download: Option<ResolveDownloadCapability>,
    /// DOI in, one bibliographic record out: what the metadata cache stores
    /// about a file once it has a DOI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lookup: Option<LookupCapability>,
    /// DOI in, the DOIs the work cites out: the edges of the citation graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<ReferencesCapability>,
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
/// Its response is not read: a status code is the whole answer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HealthCapability {
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SearchCapability {
    /// Path and query, relative to `base_url`. May use `{query}` and `{limit}`,
    /// and `{page}` (1-based) or `{offset}` (`(page - 1) * limit`) when the
    /// service can be read past its first page. A path using neither is a
    /// search that has one page.
    pub path: String,
    /// How selectors in this capability are interpreted. Omitted means JSON.
    #[serde(default)]
    pub response_format: ResponseFormat,
    /// Where the records are in the response: a JMESPath expression yielding
    /// an array, or an HTML CSS selector. Omitted only when a JSON body is
    /// itself the array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<String>,
    /// Result field name → where to find it in one record.
    pub fields: BTreeMap<String, FieldSpec>,
}

impl SearchCapability {
    /// Whether the search can be read past its first page: its path says where
    /// to start. A caller must not ask for a second page of one that cannot,
    /// because the service would answer the first again.
    pub fn paged(&self) -> bool {
        self.path.contains("{page}") || self.path.contains("{offset}")
    }
}

/// A filtered, sorted, paged listing.
///
/// What the user may vary is declared here, as typed controls, rather than as
/// a template the user fills: the engine composes the request from the
/// declared pieces, so a control can only produce a clause the manifest author
/// wrote, and the interface renders its filter panel from this declaration
/// without knowing which service it is talking to.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BrowseCapability {
    /// Path and query, relative to `base_url`. May use `{limit}` and `{page}`
    /// (1-based). The engine appends the declared controls to it.
    pub path: String,
    /// The query parameter that carries a keyword, appended only when the
    /// caller gave one. Omitted means this service's listing takes none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_param: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<SortSpec>,
    /// The query parameter that carries every active filter clause, joined by
    /// `filter_separator`. Required when `filters` is non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_param: Option<String>,
    #[serde(default = "default_filter_separator")]
    pub filter_separator: String,
    /// Filters in the order the interface shows them.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub filters: IndexMap<String, FilterSpec>,
}

fn default_filter_separator() -> String {
    ",".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SortSpec {
    pub param: String,
    /// The option used when the caller names none.
    pub default: String,
    /// The option used instead when the caller names none *and* gave a
    /// keyword: a service that ranks keyword matches by relevance wants that
    /// order for a search, and a popularity order for a bare listing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword_default: Option<String>,
    /// `[value, label]` pairs, in the order the interface shows them.
    pub options: Vec<(String, String)>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FilterSpec {
    /// One of a fixed set. `clause` carries `{value}`.
    Choice {
        label: String,
        clause: String,
        /// `[value, label]` pairs.
        options: Vec<(String, String)>,
    },
    /// On or off. `clause` is added when on.
    Flag { label: String, clause: String },
    /// An integer lower and/or upper bound, each with its own clause carrying
    /// `{value}`. A bound with no clause is not offered.
    Range {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_clause: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_clause: Option<String>,
    },
}

impl FilterSpec {
    pub fn label(&self) -> &str {
        match self {
            Self::Choice { label, .. } | Self::Flag { label, .. } | Self::Range { label, .. } => {
                label
            }
        }
    }
}

/// What a manifest's browse capability offers, for the interface to render.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BrowseControls {
    pub keyword: bool,
    pub sort: Option<SortControl>,
    pub filters: Vec<FilterControl>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SortControl {
    pub default: String,
    pub keyword_default: Option<String>,
    pub options: Vec<ControlOption>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ControlOption {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FilterControl {
    pub name: String,
    pub label: String,
    /// `choice`, `flag` or `range`.
    pub kind: String,
    pub options: Vec<ControlOption>,
    pub has_min: bool,
    pub has_max: bool,
}

impl BrowseCapability {
    pub fn controls(&self) -> BrowseControls {
        BrowseControls {
            keyword: self.query_param.is_some(),
            sort: self.sort.as_ref().map(|sort| SortControl {
                default: sort.default.clone(),
                keyword_default: sort.keyword_default.clone(),
                options: options_view(&sort.options),
            }),
            filters: self
                .filters
                .iter()
                .map(|(name, spec)| match spec {
                    FilterSpec::Choice { label, options, .. } => FilterControl {
                        name: name.clone(),
                        label: label.clone(),
                        kind: "choice".to_string(),
                        options: options_view(options),
                        has_min: false,
                        has_max: false,
                    },
                    FilterSpec::Flag { label, .. } => FilterControl {
                        name: name.clone(),
                        label: label.clone(),
                        kind: "flag".to_string(),
                        options: Vec::new(),
                        has_min: false,
                        has_max: false,
                    },
                    FilterSpec::Range {
                        label,
                        min_clause,
                        max_clause,
                    } => FilterControl {
                        name: name.clone(),
                        label: label.clone(),
                        kind: "range".to_string(),
                        options: Vec::new(),
                        has_min: min_clause.is_some(),
                        has_max: max_clause.is_some(),
                    },
                })
                .collect(),
        }
    }
}

fn options_view(options: &[(String, String)]) -> Vec<ControlOption> {
    options
        .iter()
        .map(|(value, label)| ControlOption {
            value: value.clone(),
            label: label.clone(),
        })
        .collect()
}

/// One work, as the metadata cache stores it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LookupCapability {
    /// Requests tried in order, each a path relative to `base_url` that may use
    /// `{doi}`. The first whose response holds a record wins; a response that
    /// holds none (an empty result, a 404) moves on to the next, and any other
    /// failure stops the lookup. At most [`MAX_LOOKUP_PATHS`]. More than one
    /// exists because a service may know a work under a different field when
    /// its DOI index does not.
    pub paths: Vec<String>,
    /// Where the record is in a response: a JMESPath expression. A `null`
    /// result means "no record here". Omitted means the body is the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    /// Lookup field name → where to find it in the record.
    pub fields: BTreeMap<String, FieldSpec>,
}

/// The citation edges out of one work.
///
/// Two stages, because that is how the services that offer references offer
/// them: the work lists identifiers (`ids`), and the identifiers are turned
/// back into DOIs in fixed-size batches. The chunk size is declared and
/// bounded; there is no loop a manifest controls.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReferencesCapability {
    /// The work, by `{doi}`.
    pub path: String,
    /// A JMESPath expression over that response yielding an array of the
    /// identifiers the work references. `null` or empty means none.
    pub ids: String,
    pub resolve: ReferencesResolve,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReferencesResolve {
    /// Path for one chunk. `{ids}` is the chunk's identifiers, each
    /// percent-encoded and joined by `join` (which stays literal: for most
    /// services it is the OR operator), and `{count}` is how many.
    pub path: String,
    pub join: String,
    /// Identifiers per request, 1 to [`MAX_REFERENCE_CHUNK`].
    pub chunk: usize,
    /// Where the records are in the response. Omitted means the body is the
    /// array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<String>,
    /// A JMESPath expression over one record yielding its DOI. A record whose
    /// expression yields `null` or a non-DOI has no edge and is left out.
    pub doi: String,
}

/// A finite, acyclic resolver from one selected result to a downloadable URL.
///
/// Every step is a GET to a literal, manifest-declared origin: the primary
/// base by default, or the step's override. Its named fields become
/// placeholders available only to later steps and to the final output. The
/// final URL is returned to Wilkes' existing downloader; this capability never
/// writes a byte itself.
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
    /// Optional literal origin override for this step. It is deliberately not
    /// a template: every origin a manifest can contact must be visible before
    /// the manifest is saved. When absent, this step uses `http.base_url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub path: String,
    #[serde(default)]
    pub response_format: ResponseFormat,
    /// Headers used by this step alone. A download credential therefore need
    /// not be sent with the provider's public search request.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, ParamValue>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, ParamValue>,
    /// Placeholder name -> selector in this response.
    pub fields: BTreeMap<String, FieldSpec>,
}

/// How one output field is found.
///
/// The bare-string form is the common case (`title = "display_name"`); the
/// object form adds the HTML extraction options, a regular-expression capture,
/// or a typed search input.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum FieldSpec {
    Path(String),
    Mapped {
        /// A JMESPath expression (JSON) or CSS selector (HTML).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// HTML only: CSS selectors tried in order until one resolves. CSS has
        /// no `not_null`; for JSON, say it as `not_null(a, b)`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        first_of: Vec<String>,
        /// Use one of the search capability's typed inputs (`query` or
        /// `limit`) instead of selecting from the response. This is how a DOI
        /// lookup carries the requested DOI into its later download resolver
        /// when the HTML result does not repeat it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        /// For HTML, read this attribute from the first matched element
        /// instead of its text. JSON may not set it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attribute: Option<String>,
        /// HTML only: collect every matched element into an array rather than
        /// reading the first, for a `then` that joins them.
        #[serde(default, skip_serializing_if = "is_false")]
        all: bool,
        /// A JMESPath expression applied to the extracted value, which is `@`.
        /// For HTML and typed inputs, where extraction is not itself an
        /// expression; JSON fields compose in `path`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        then: Option<String>,
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

    pub fn first_of(&self) -> &[String] {
        match self {
            Self::Path(_) => &[],
            Self::Mapped { first_of, .. } => first_of,
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

    pub fn all(&self) -> bool {
        match self {
            Self::Path(_) => false,
            Self::Mapped { all, .. } => *all,
        }
    }

    pub fn then(&self) -> Option<&str> {
        match self {
            Self::Path(_) => None,
            Self::Mapped { then, .. } => then.as_deref(),
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

/// The fields a lookup capability may name: what [`crate::types::ProviderWork`]
/// holds beyond the DOI, which the request itself supplies.
pub const LOOKUP_FIELDS: &[(&str, FieldType)] = &[
    ("id", FieldType::Text),
    ("title", FieldType::Text),
    ("year", FieldType::Integer),
    ("publication_date", FieldType::Text),
    ("venue", FieldType::Text),
    ("citation_count", FieldType::Integer),
    ("external_ids", FieldType::Object),
];

pub const REQUIRED_LOOKUP_FIELDS: &[&str] = &["id"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    Text,
    Integer,
    Boolean,
    Url,
    Object,
}

impl FieldType {
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::Url => "url",
            Self::Object => "object",
        }
    }
}

pub fn search_field_type(field: &str) -> Option<FieldType> {
    SEARCH_FIELDS
        .iter()
        .find(|(name, _)| *name == field)
        .map(|(_, ty)| *ty)
}

pub fn lookup_field_type(field: &str) -> Option<FieldType> {
    LOOKUP_FIELDS
        .iter()
        .find(|(name, _)| *name == field)
        .map(|(_, ty)| *ty)
}

/// Placeholders a capability may use, by capability.
/// What a search field may take as its input.
const SEARCH_PLACEHOLDERS: &[&str] = &["query", "limit"];
/// What a search path may name: the inputs, and where in the results to start.
const SEARCH_PATH_PLACEHOLDERS: &[&str] = &["query", "limit", "page", "offset"];
const BROWSE_PLACEHOLDERS: &[&str] = &["limit", "page"];
const HEALTH_PLACEHOLDERS: &[&str] = &[];
const LOOKUP_PLACEHOLDERS: &[&str] = &["doi"];
const REFERENCES_PLACEHOLDERS: &[&str] = &["doi"];
const REFERENCES_RESOLVE_PLACEHOLDERS: &[&str] = &["ids", "count"];
const MAX_RESOLVER_STEPS: usize = 4;
pub const MAX_LOOKUP_PATHS: usize = 3;
pub const MAX_REFERENCE_CHUNK: usize = 100;

const AUTHORING_JSON_EXAMPLE: &str = r#"manifest_version = 2
id = "example-json"
name = "Example JSON service"

[http]
base_url = "https://api.example.test"

[http.query]
mailto = { secret = "email", optional = true, label = "Contact email", kind = "email" }

[capabilities.search]
path = "/works?q={query}&limit={limit}"
response_format = "json"
items = "results"

[capabilities.search.fields]
id = "id"
title = "title"
doi = "normalize_doi(doi)"
year = "year_from_date(published)"
pdf_url = "links.pdf"
"#;

const AUTHORING_HTML_EXAMPLE: &str = r#"manifest_version = 2
id = "example-html"
name = "Example HTML service"

[http]
base_url = "https://library.example.test"

[http.headers]
User-Agent = "Mozilla/5.0"
Accept-Language = "en-US,en;q=0.9"

[capabilities.search]
path = "/search?q={query}"
response_format = "html"
items = "article.result"

[capabilities.search.fields]
id = { path = "a.download", attribute = "data-id" }
title = "h2"
authors = { path = "a.author", all = true, then = "join(', ', @)" }
file_format = { path = ".metadata", capture = '''(?i)\b(PDF|EPUB)\b''' }
landing_page_url = { path = "a.details", attribute = "href", then = "absolute_url(@)" }

[capabilities.resolve_download]
url = "{download_url}"
filename = "{title}.{file_format}"

[[capabilities.resolve_download.steps]]
base_url = "https://downloads.example.test"
path = "/api/download?id={id}"
response_format = "json"

[capabilities.resolve_download.steps.query]
key = { secret = "member_key" }

[capabilities.resolve_download.steps.fields]
download_url = "absolute_url(url)"
"#;

const AUTHORING_FULL_EXAMPLE: &str = r#"manifest_version = 2
id = "example-index"
name = "Example scholarly index"

[http]
base_url = "https://api.index.test"

[http.headers]
x-api-key = { secret = "api_key", optional = true, label = "API key" }

[capabilities.health]
path = "/ping"

[capabilities.search]
path = "/works?search={query}&per-page={limit}&page={page}"
items = "results"

[capabilities.search.fields]
id = "id"
title = "name"
doi = "normalize_doi(ids.doi)"
is_open_access = "oa.is_oa || best.is_oa"

# Browse: the same records, listed by filter and sort, keyword optional.
[capabilities.browse]
path = "/works?per-page={limit}&page={page}"
query_param = "search"
filter_param = "filter"
filter_separator = ","

[capabilities.browse.sort]
param = "sort"
default = "cited_by_count:desc"
keyword_default = "relevance_score:desc"
options = [["cited_by_count:desc", "Most cited"], ["publication_date:desc", "Newest"], ["relevance_score:desc", "Most relevant"]]

[capabilities.browse.filters.year]
kind = "range"
label = "Year"
min_clause = "from_publication_date:{value}-01-01"
max_clause = "to_publication_date:{value}-12-31"

[capabilities.browse.filters.type]
kind = "choice"
label = "Type"
clause = "type:{value}"
options = [["article", "Article"], ["book", "Book"]]

[capabilities.browse.filters.open_access]
kind = "flag"
label = "Open access only"
clause = "is_oa:true"

# Lookup: tried in order; the first request whose response holds a record wins.
[capabilities.lookup]
paths = [
  "/works?filter=doi:https://doi.org/{doi}",
  "/works?filter=landing_page:https://doi.org/{doi}",
]
record = "results[0]"

[capabilities.lookup.fields]
id = "id"
title = "name"
year = "year"
citation_count = "cited_by_count"
external_ids = "ids"

# References: the work's identifiers, then DOIs for them in batches.
[capabilities.references]
path = "/works?filter=doi:https://doi.org/{doi}&select=id,referenced_works"
ids = "results[0].referenced_works[*] | map(&split(@, '/')[-1], @)"

[capabilities.references.resolve]
path = "/works?filter=openalex_id:{ids}&select=id,doi&per-page={count}"
join = "|"
chunk = 50
items = "results"
doi = "doi"
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
    let lookup_fields = LOOKUP_FIELDS
        .iter()
        .map(|(name, field_type)| {
            let required = if REQUIRED_LOOKUP_FIELDS.contains(name) {
                " (required)"
            } else {
                ""
            };
            format!("- {name}: {}{required}", field_type.name())
        })
        .collect::<Vec<_>>()
        .join("\n");
    let functions = expr::registered_function_hints()
        .iter()
        .map(|(name, hint)| format!("- {name}: {hint}"))
        .collect::<Vec<_>>()
        .join("\n");
    let response_formats = ResponseFormat::ALL
        .iter()
        .map(|format| format.name())
        .collect::<Vec<_>>()
        .join(", ");
    let secret_kinds = SecretKind::ALL
        .iter()
        .map(|kind| kind.name())
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
- Never put a credential value in the manifest. Declare `{ secret = "symbolic_name" }`; write a plain string only for public fixed values. `optional = true` leaves the header or parameter off the request when the user has set no value.
- A resolver step may set a literal `base_url` to use another HTTP(S) origin. It cannot contain placeholders. Every contacted origin is declared and shown before save.
- `http.headers` and `http.query` are sent to the primary origin and same-origin resolver steps. They are never copied to a cross-origin step; repeat any header or parameter that origin needs under that step's `headers` or `query`.
- Redirects may stay on the request's declared origin only. A resolved final download URL may be external because Wilkes passes it to its existing bounded downloader.
- Missing or unparseable required values are errors. Do not invent defaults.
- JSON values are selected with JMESPath (https://jmespath.org); HTML values with CSS selectors. State HTML explicitly.

TOML schema (comments name optional fields and alternatives):

manifest_version = 2
id = "lowercase-slug"                 # required; a-z, 0-9, - or _; at most 64 characters
name = "Human-readable name"          # required

[http]
base_url = "https://one-origin.test"  # required; http or https

[http.headers]                        # optional; primary-origin requests, including same-origin resolver steps
Header-Name = "public fixed value"
Other-Header = { secret = "symbolic_secret_name" }   # or add optional = true, label = "...", kind = "text" | "email" | "password"

[http.query]                          # optional; same reach as headers
param = "public fixed value"
other_param = { secret = "symbolic_secret_name", optional = true }

[capabilities.health]                 # optional
path = "/health"                      # no placeholders

[capabilities.search]                 # required for every capability but health
path = "/search?q={query}&limit={limit}"   # also {page} (1-based) or {offset} where the service pages, so the pane can read on
response_format = "json"              # optional; "json" or "html"; defaults to json
items = "expression"                  # JMESPath yielding an array, or a CSS selector for HTML; optional for JSON when the body is the array

[capabilities.search.fields]
# Each key is one result field listed below. A field specification is either:
field = "expression"                  # JMESPath (JSON) or CSS selector (HTML)
# or an object:
field = { path = "css selector", attribute = "href", all = true, then = "join(', ', @)", capture = "regex with a capture", capture_group = 1 }
# or a search input instead of a response selector:
field = { input = "query", then = "expression over @" }   # input is "query" or "limit"
# `attribute`, `all` and `first_of = ["css", ...]` (CSS selectors tried in order) are HTML-only. `then` is a JMESPath expression whose `@` is the extracted value; it is for HTML and typed inputs, since JSON fields compose in `path`. Regex capture defaults to group 1.

[capabilities.browse]                 # optional; the same records as search, listed by filter and sort
path = "/list?limit={limit}&page={page}"   # {limit}, {page} (1-based)
query_param = "q"                     # optional; appended only when the caller gave a keyword
filter_param = "filter"               # required when there are filters; one parameter carries every active clause
filter_separator = ","                # optional, defaults to ","

[capabilities.browse.sort]            # optional
param = "sort"
default = "value-of-the-default-option"
keyword_default = "value-used-instead-when-a-keyword-is-given"   # optional
options = [["value", "Label"], ["value2", "Label 2"]]

[capabilities.browse.filters.name]    # one table per filter, shown in this order; name is a-z, 0-9, _
kind = "choice"                       # choice | flag | range
label = "Shown to the user"
clause = "field:{value}"              # choice: {value} is one option value. flag: no {value}, added when on
options = [["value", "Label"]]        # choice only
# kind = "range": min_clause = "field:>{value}" and/or max_clause = "field:<{value}" (integers)

[capabilities.lookup]                 # optional; DOI to one bibliographic record, for metadata enrichment
paths = ["/work?doi={doi}"]           # 1 to 3 requests tried in order; the first whose response holds a record wins. A response with no record (empty, 404) moves on; any other failure stops
record = "expression"                 # optional JMESPath to the record; null means none; omitted means the body is the record

[capabilities.lookup.fields]
id = "expression"                     # lookup fields are listed below

[capabilities.references]             # optional; DOI to the DOIs a work cites
path = "/work?doi={doi}"
ids = "expression"                    # JMESPath yielding an array of identifiers (null or empty means none)

[capabilities.references.resolve]
path = "/works?ids={ids}&per-page={count}"   # {ids}: one chunk's identifiers joined by `join`; {count}
join = "|"
chunk = 50                            # identifiers per request, 1-100
items = "expression"                  # optional; where the records are
doi = "expression"                    # JMESPath over one record yielding its DOI (null or a non-DOI leaves the record out)

[capabilities.resolve_download]       # optional; runs only after the user selects a result
url = "{result_or_step_value}"        # required; HTTP(S) or relative to http.base_url
filename = "{title}.{file_format}"   # optional; sanitized before download

[[capabilities.resolve_download.steps]]
base_url = "https://other-origin.test" # optional literal HTTP(S) base; defaults to http.base_url
path = "/resolve?id={id}"             # result values plus outputs from earlier steps
response_format = "json"              # optional; "json" or "html"; defaults to json

[capabilities.resolve_download.steps.query]   # and/or .headers; this step only
param = { secret = "symbolic_secret_name" }

[capabilities.resolve_download.steps.fields]
new_value = "expression"              # required scalar string; the full field-spec object form is also allowed

JMESPath selects keys (`a.b`), indices (`a[0]`, `a[-1]`), projections (`a[*].b`), filters (`a[?k == 'v']`), alternatives (`not_null(a, b)`) and disjunctions (`a || b`). Quote keys that are not plain identifiers: `published."date-parts"[0][0]`. Standard JMESPath functions are available, plus these:
"#,
    );
    writeln!(prompt, "{functions}").unwrap();
    prompt.push_str("\nSearch result fields:\n");
    writeln!(prompt, "{fields}").unwrap();
    writeln!(prompt, "\nLookup record fields:\n{lookup_fields}").unwrap();
    writeln!(
        prompt,
        "\nClosed enum values: response_format = {response_formats}; secret kind = {secret_kinds}."
    )
    .unwrap();
    writeln!(
        prompt,
        "Resolver steps are acyclic, pinned to their declared origins, and limited to {MAX_RESOLVER_STEPS}. A lookup has at most {MAX_LOOKUP_PATHS} requests. References resolve at most {MAX_REFERENCE_CHUNK} identifiers per request. There are no loops, branches, arbitrary expressions, POST requests, browser JavaScript, or writes."
    )
    .unwrap();
    prompt.push_str("\nValid JSON example:\n\n");
    prompt.push_str(AUTHORING_JSON_EXAMPLE);
    prompt.push_str("\nValid HTML plus download-resolution example:\n\n");
    prompt.push_str(AUTHORING_HTML_EXAMPLE);
    prompt.push_str("\nValid example using browse, lookup and references:\n\n");
    prompt.push_str(AUTHORING_FULL_EXAMPLE);
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
        let is_json = source.trim_start().starts_with('{');
        // The version is read before anything else, so a manifest written for
        // another version is told so by number. Read as this version it would
        // fail on the first field that moved, with an error about that field
        // and not about the thing the author needs to know.
        if let Some(version) = declared_version(source, is_json) {
            anyhow::ensure!(
                version == u64::from(MANIFEST_VERSION),
                "manifest_version {version} is not supported (this build reads version {MANIFEST_VERSION})"
            );
        }
        let manifest: Manifest = if is_json {
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

        problems.extend(base_url_problems("base_url", &self.http.base_url));
        problems.extend(param_problems("http", &self.http.headers, &self.http.query));

        let capabilities = &self.capabilities;
        if capabilities.health.is_none()
            && capabilities.search.is_none()
            && capabilities.resolve_download.is_none()
            && capabilities.browse.is_none()
            && capabilities.lookup.is_none()
            && capabilities.references.is_none()
        {
            problems.push("manifest declares no capabilities".to_string());
        }

        if let Some(health) = &capabilities.health {
            problems.extend(template_problems(
                "health.path",
                &health.path,
                HEALTH_PLACEHOLDERS,
            ));
        }

        if let Some(search) = &capabilities.search {
            problems.extend(template_problems(
                "search.path",
                &search.path,
                SEARCH_PATH_PLACEHOLDERS,
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

        // Every capability but `health` works on the records `search` returns
        // or on a work a search result names, so each needs `search` for the
        // same reason `resolve_download` always did.
        for (name, present) in [
            ("browse", capabilities.browse.is_some()),
            ("resolve_download", capabilities.resolve_download.is_some()),
            ("lookup", capabilities.lookup.is_some()),
            ("references", capabilities.references.is_some()),
        ] {
            if present && capabilities.search.is_none() {
                problems.push(format!(
                    "{name} requires search because it works on what search returns"
                ));
            }
        }

        if let Some(browse) = &capabilities.browse {
            problems.extend(browse_problems(browse));
        }
        if let Some(lookup) = &capabilities.lookup {
            problems.extend(lookup_problems(lookup));
        }
        if let Some(references) = &capabilities.references {
            problems.extend(references_problems(references));
        }
        if let Some(resolve) = &capabilities.resolve_download {
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
            let Some(_) = search_field_type(field) else {
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
            problems.extend(field_spec_problems(
                &format!("search.fields.{field}"),
                search.response_format,
                spec,
                true,
            ));
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
            if let Some(base_url) = &step.base_url {
                problems.extend(base_url_problems(&format!("{label}.base_url"), base_url));
            }
            problems.extend(template_problems_owned(
                &format!("{label}.path"),
                &step.path,
                &available,
                true,
            ));
            problems.extend(param_problems(&label, &step.headers, &step.query));
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
                if spec.input().is_some() {
                    problems.push(format!(
                        "{label}.fields.{name}.input is only available to search fields"
                    ));
                } else {
                    problems.extend(field_spec_problems(
                        &format!("{label}.fields.{name}"),
                        step.response_format,
                        spec,
                        false,
                    ));
                }
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

    /// Every request origin this manifest may contact, in declaration order,
    /// for the import dialog to show before anything is saved.
    pub fn origins(&self) -> Vec<String> {
        let mut origins = Vec::new();
        let candidates = std::iter::once(self.http.base_url.as_str()).chain(
            self.capabilities
                .resolve_download
                .iter()
                .flat_map(|resolve| &resolve.steps)
                .filter_map(|step| step.base_url.as_deref()),
        );
        for candidate in candidates {
            if let Ok(url) = Url::parse(candidate) {
                let origin = url.origin().ascii_serialization();
                if !origins.contains(&origin) {
                    origins.push(origin);
                }
            }
        }
        origins
    }

    /// Every secret this manifest names, once each, in declaration order, as
    /// a settings form needs to render them.
    ///
    /// A name used both as required and as optional is required: the stricter
    /// use decides whether a request can be sent.
    pub fn secret_slots(&self) -> Vec<SecretSlot> {
        let mut slots: Vec<SecretSlot> = Vec::new();
        let steps = self
            .capabilities
            .resolve_download
            .iter()
            .flat_map(|resolve| &resolve.steps);
        let values = self
            .http
            .headers
            .values()
            .chain(self.http.query.values())
            .chain(steps.flat_map(|step| step.headers.values().chain(step.query.values())));
        for value in values {
            let ParamValue::Secret(secret) = value else {
                continue;
            };
            match slots.iter_mut().find(|slot| slot.name == secret.secret) {
                Some(existing) => existing.optional &= secret.optional,
                None => slots.push(SecretSlot {
                    name: secret.secret.clone(),
                    label: secret
                        .label
                        .clone()
                        .unwrap_or_else(|| secret.secret.clone()),
                    kind: secret.kind.unwrap_or(SecretKind::Password),
                    optional: secret.optional,
                }),
            }
        }
        slots
    }

    /// Names of the secrets this manifest needs supplied before it can send a
    /// request.
    pub fn required_secrets(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .secret_slots()
            .into_iter()
            .filter(|slot| !slot.optional)
            .map(|slot| slot.name)
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Names of the secrets that improve a request but are not needed to send
    /// it.
    pub fn optional_secrets(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .secret_slots()
            .into_iter()
            .filter(|slot| slot.optional)
            .map(|slot| slot.name)
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

/// The `manifest_version` a document declares, if it is a document at all and
/// declares one as an integer.
fn declared_version(source: &str, is_json: bool) -> Option<u64> {
    if is_json {
        serde_json::from_str::<serde_json::Value>(source)
            .ok()?
            .get("manifest_version")?
            .as_u64()
    } else {
        toml::from_str::<toml::Table>(source)
            .ok()?
            .get("manifest_version")?
            .as_integer()
            .and_then(|version| u64::try_from(version).ok())
    }
}

fn selector_problem(format: ResponseFormat, selector: &str) -> Result<(), String> {
    match format {
        ResponseFormat::Json => expr::check(selector),
        ResponseFormat::Html => dom_query::Matcher::new(selector)
            .map(|_| ())
            .map_err(|error| format!("invalid CSS selector: {error:?}")),
    }
}

/// Problems with one field's specification, in a response of `format`.
///
/// `allow_input` is true only for search fields: a typed input has nothing to
/// carry in a lookup or resolver step.
fn field_spec_problems(
    label: &str,
    format: ResponseFormat,
    spec: &FieldSpec,
    allow_input: bool,
) -> Vec<String> {
    let mut problems = Vec::new();
    let paths = spec.paths();
    let input = spec.input();

    match (paths.is_empty(), input) {
        (true, None) => {
            problems.push(format!("{label} names neither a selector nor an input"));
        }
        (false, Some(_)) => problems.push(format!("{label} sets both a selector and input")),
        _ => {}
    }
    if let Some(input) = input {
        if !allow_input {
            problems.push(format!("{label}.input is only available to search fields"));
        } else if !SEARCH_PLACEHOLDERS.contains(&input) {
            problems.push(format!(
                "{label}.input '{input}' is not available; expected query or limit"
            ));
        }
        if spec.attribute().is_some() {
            problems.push(format!("{label}.attribute cannot be used with input"));
        }
        if spec.all() {
            problems.push(format!("{label}.all cannot be used with input"));
        }
    } else {
        for path in &paths {
            if let Err(error) = selector_problem(format, path) {
                problems.push(format!("{label}: {error}"));
            }
        }
        match format {
            ResponseFormat::Json => {
                if !spec.first_of().is_empty() {
                    problems.push(format!(
                        "{label}.first_of is only for HTML; for JSON write not_null(a, b)"
                    ));
                }
                if spec.attribute().is_some() {
                    problems.push(format!(
                        "{label}.attribute is only available for HTML responses"
                    ));
                }
                if spec.all() {
                    problems.push(format!("{label}.all is only available for HTML responses"));
                }
                if spec.then().is_some() {
                    problems.push(format!(
                        "{label}.then is only for HTML and inputs; a JSON field composes in its path"
                    ));
                }
            }
            ResponseFormat::Html => {}
        }
    }
    if spec.attribute().is_some_and(|name| name.trim().is_empty()) {
        problems.push(format!("{label}.attribute cannot be empty"));
    }
    if let Some(then) = spec.then() {
        if let Err(error) = expr::check(then) {
            problems.push(format!("{label}.then: {error}"));
        }
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

fn browse_problems(browse: &BrowseCapability) -> Vec<String> {
    let mut problems = template_problems("browse.path", &browse.path, BROWSE_PLACEHOLDERS);

    for (label, param) in [
        ("query_param", browse.query_param.as_deref()),
        ("filter_param", browse.filter_param.as_deref()),
        (
            "sort.param",
            browse.sort.as_ref().map(|sort| sort.param.as_str()),
        ),
    ] {
        if param.is_some_and(|name| name.trim().is_empty() || name.contains(['&', '=', '#'])) {
            problems.push(format!("browse.{label} must be a plain parameter name"));
        }
    }

    if let Some(sort) = &browse.sort {
        if sort.options.is_empty() {
            problems.push("browse.sort.options cannot be empty".to_string());
        }
        if !sort.options.iter().any(|(value, _)| *value == sort.default) {
            problems.push(format!(
                "browse.sort.default '{}' is not one of its options",
                sort.default
            ));
        }
        if let Some(keyword_default) = &sort.keyword_default {
            if !sort
                .options
                .iter()
                .any(|(value, _)| value == keyword_default)
            {
                problems.push(format!(
                    "browse.sort.keyword_default '{keyword_default}' is not one of its options"
                ));
            }
        }
        problems.extend(option_problems("browse.sort", &sort.options));
    }

    if !browse.filters.is_empty() && browse.filter_param.is_none() {
        problems.push("browse.filter_param is required when there are filters".to_string());
    }
    if browse.filter_separator.is_empty() {
        problems.push("browse.filter_separator cannot be empty".to_string());
    }

    for (name, spec) in &browse.filters {
        let label = format!("browse.filters.{name}");
        if !is_variable(name) {
            problems.push(format!(
                "{label}: the filter name must be a-z, 0-9 or '_', starting with a letter"
            ));
        }
        if spec.label().trim().is_empty() {
            problems.push(format!("{label}.label cannot be empty"));
        }
        match spec {
            FilterSpec::Choice {
                clause, options, ..
            } => {
                problems.extend(clause_problems(&label, "clause", clause, true));
                if options.is_empty() {
                    problems.push(format!("{label}.options cannot be empty"));
                }
                problems.extend(option_problems(&label, options));
            }
            FilterSpec::Flag { clause, .. } => {
                problems.extend(clause_problems(&label, "clause", clause, false));
            }
            FilterSpec::Range {
                min_clause,
                max_clause,
                ..
            } => {
                if min_clause.is_none() && max_clause.is_none() {
                    problems.push(format!(
                        "{label} declares neither min_clause nor max_clause"
                    ));
                }
                for (field, clause) in [("min_clause", min_clause), ("max_clause", max_clause)] {
                    if let Some(clause) = clause {
                        problems.extend(clause_problems(&label, field, clause, true));
                    }
                }
            }
        }
    }
    problems
}

fn option_problems(label: &str, options: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (index, (value, option_label)) in options.iter().enumerate() {
        if value.trim().is_empty() || option_label.trim().is_empty() {
            problems.push(format!(
                "{label}.options[{index}] needs a value and a label"
            ));
        }
        if options[..index].iter().any(|(earlier, _)| earlier == value) {
            problems.push(format!("{label}.options repeats '{value}'"));
        }
    }
    problems
}

/// A filter clause holds `{value}` exactly when it takes one.
fn clause_problems(label: &str, field: &str, clause: &str, takes_value: bool) -> Vec<String> {
    let mut problems = placeholder_problems(
        &format!("{label}.{field}"),
        clause,
        if takes_value { &["value"] } else { &[] },
    );
    if clause.trim().is_empty() {
        problems.push(format!("{label}.{field} cannot be empty"));
    } else if takes_value && !clause.contains("{value}") {
        problems.push(format!("{label}.{field} must contain {{value}}"));
    }
    problems
}

fn lookup_problems(lookup: &LookupCapability) -> Vec<String> {
    let mut problems = Vec::new();
    if lookup.paths.is_empty() {
        problems.push("lookup.paths needs at least one request".to_string());
    }
    if lookup.paths.len() > MAX_LOOKUP_PATHS {
        problems.push(format!(
            "lookup.paths has {} requests; the limit is {MAX_LOOKUP_PATHS}",
            lookup.paths.len()
        ));
    }
    for (index, path) in lookup.paths.iter().enumerate() {
        let label = format!("lookup.paths[{index}]");
        problems.extend(template_problems(&label, path, LOOKUP_PLACEHOLDERS));
        if !path.contains("{doi}") {
            problems.push(format!("{label} must contain {{doi}}"));
        }
    }
    if let Some(record) = &lookup.record {
        if let Err(error) = expr::check(record) {
            problems.push(format!("lookup.record: {error}"));
        }
    }
    for required in REQUIRED_LOOKUP_FIELDS {
        if !lookup.fields.contains_key(*required) {
            problems.push(format!(
                "lookup.fields is missing '{required}', which every record needs"
            ));
        }
    }
    for (field, spec) in &lookup.fields {
        if lookup_field_type(field).is_none() {
            problems.push(format!(
                "lookup.fields.{field} is not a lookup field; expected one of: {}",
                LOOKUP_FIELDS
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            continue;
        }
        problems.extend(field_spec_problems(
            &format!("lookup.fields.{field}"),
            ResponseFormat::Json,
            spec,
            false,
        ));
    }
    problems
}

fn references_problems(references: &ReferencesCapability) -> Vec<String> {
    let mut problems =
        template_problems("references.path", &references.path, REFERENCES_PLACEHOLDERS);
    if !references.path.contains("{doi}") {
        problems.push("references.path must contain {doi}".to_string());
    }
    if let Err(error) = expr::check(&references.ids) {
        problems.push(format!("references.ids: {error}"));
    }
    let resolve = &references.resolve;
    problems.extend(template_problems(
        "references.resolve.path",
        &resolve.path,
        REFERENCES_RESOLVE_PLACEHOLDERS,
    ));
    if !resolve.path.contains("{ids}") {
        problems.push("references.resolve.path must contain {ids}".to_string());
    }
    if resolve.join.is_empty() {
        problems.push("references.resolve.join cannot be empty".to_string());
    }
    if !(1..=MAX_REFERENCE_CHUNK).contains(&resolve.chunk) {
        problems.push(format!(
            "references.resolve.chunk {} is outside 1-{MAX_REFERENCE_CHUNK}",
            resolve.chunk
        ));
    }
    if let Some(items) = &resolve.items {
        if let Err(error) = expr::check(items) {
            problems.push(format!("references.resolve.items: {error}"));
        }
    }
    if let Err(error) = expr::check(&resolve.doi) {
        problems.push(format!("references.resolve.doi: {error}"));
    }
    problems
}

fn base_url_problems(label: &str, value: &str) -> Vec<String> {
    if value.contains(['{', '}']) {
        return vec![format!(
            "{label} must be a literal URL without placeholders"
        )];
    }
    match Url::parse(value) {
        Ok(url) if !matches!(url.scheme(), "http" | "https") => vec![format!(
            "{label} scheme '{}' is not http or https",
            url.scheme()
        )],
        Ok(url) if url.host_str().is_none() => vec![format!("{label} has no host")],
        Ok(_) => Vec::new(),
        Err(error) => vec![format!("{label} is not a URL: {error}")],
    }
}

fn param_problems(
    label: &str,
    headers: &BTreeMap<String, ParamValue>,
    query: &BTreeMap<String, ParamValue>,
) -> Vec<String> {
    let mut problems = Vec::new();
    for (kind, params) in [("headers", headers), ("query", query)] {
        for (name, value) in params {
            if name.trim().is_empty() {
                problems.push(format!("{label}.{kind} has an empty name"));
            }
            if kind == "headers" && !is_header_name(name) {
                problems.push(format!(
                    "{label}.headers '{name}' is not a valid header name"
                ));
            }
            if let ParamValue::Secret(secret) = value {
                if secret.secret.trim().is_empty() {
                    problems.push(format!("{label}.{kind} '{name}' names an empty secret"));
                }
            }
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
manifest_version = 2
id = "crossref"
name = "Crossref"

[http]
base_url = "https://api.crossref.org"

[http.query]
mailto = "team@example.com"

[capabilities.health]
path = "/works/10.1145/3801158"

[capabilities.search]
path = "/works?query.bibliographic={query}&rows={limit}"
items = "message.items"

[capabilities.search.fields]
id = "DOI"
title = "title[0]"
doi = "normalize_doi(DOI)"
year = "published.\"date-parts\"[0][0]"
citation_count = "\"is-referenced-by-count\""
pdf_url = "not_null(link[0].URL, resource.primary.URL)"
"#;

    const HTML: &str = r#"
manifest_version = 2
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
        assert_eq!(manifest.origins(), vec!["https://api.crossref.org"]);
        assert!(manifest.required_secrets().is_empty());
        assert_eq!(
            manifest.http.query.get("mailto"),
            Some(&ParamValue::Literal("team@example.com".to_string()))
        );
    }

    #[test]
    fn a_version_one_manifest_is_refused_by_number() {
        let error = Manifest::parse(&CROSSREF.replace("version = 2", "version = 1"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("manifest_version 1 is not supported"),
            "{error}"
        );
    }

    #[test]
    fn resolver_step_origins_are_literal_visible_and_deduplicated() {
        let source = format!(
            "{HTML}\n[capabilities.resolve_download]\nurl = \"{{download_url}}\"\n\
             [[capabilities.resolve_download.steps]]\n\
             base_url = \"https://downloads.example.test/api\"\n\
             path = \"/resolve?id={{id}}\"\n\
             [capabilities.resolve_download.steps.fields]\n\
             download_url = \"url\"\n\
             [[capabilities.resolve_download.steps]]\n\
             base_url = \"https://downloads.example.test/other\"\n\
             path = \"/confirm?url={{download_url}}\"\n\
             [capabilities.resolve_download.steps.fields]\n\
             confirmed_url = \"url\"\n"
        );
        let manifest = Manifest::parse(&source).unwrap();
        assert_eq!(
            manifest.origins(),
            vec!["https://example.test", "https://downloads.example.test"]
        );
    }

    #[test]
    fn rejects_a_templated_or_non_http_resolver_base_url() {
        let template = format!(
            "{HTML}\n[capabilities.resolve_download]\nurl = \"{{download_url}}\"\n\
             [[capabilities.resolve_download.steps]]\n\
             base_url = \"https://{{mirror}}.example.test\"\n\
             path = \"/resolve?id={{id}}\"\n\
             [capabilities.resolve_download.steps.fields]\n\
             download_url = \"url\"\n"
        );
        let error = Manifest::parse(&template).unwrap_err().to_string();
        assert!(
            error.contains("literal URL without placeholders"),
            "{error}"
        );

        let non_http = template.replace(
            "https://{mirror}.example.test",
            "file:///private/download.json",
        );
        let error = Manifest::parse(&non_http).unwrap_err().to_string();
        assert!(error.contains("not http or https"), "{error}");
    }

    #[test]
    fn round_trips_through_json_and_toml() {
        let manifest = Manifest::parse(CROSSREF).unwrap();
        let json = serde_json::to_string(&manifest).unwrap();
        assert_eq!(Manifest::parse(&json).unwrap(), manifest);
        let toml = toml::to_string(&manifest).unwrap();
        assert_eq!(Manifest::parse(&toml).unwrap(), manifest);
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
    fn rejects_an_expression_that_does_not_parse_or_calls_an_unknown_function() {
        let error =
            Manifest::parse(&CROSSREF.replace(r#"title = "title[0]""#, r#"title = "title[""#))
                .unwrap_err()
                .to_string();
        assert!(error.contains("search.fields.title"), "{error}");
        let error = Manifest::parse(
            &CROSSREF.replace(r#"title = "title[0]""#, r#"title = "shell_out(title)""#),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("shell_out"), "{error}");
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
    fn a_secret_may_be_required_or_optional_and_is_listed_once() {
        let source = CROSSREF.replace(
            r#"mailto = "team@example.com""#,
            r#"mailto = { secret = "email", optional = true, label = "Email", kind = "email" }
token = { secret = "token" }
other = { secret = "email" }"#,
        );
        let manifest = Manifest::parse(&source).unwrap();
        // `email` is used both as optional and as required: required wins.
        assert_eq!(manifest.required_secrets(), vec!["email", "token"]);
        assert!(manifest.optional_secrets().is_empty());
        let slots = manifest.secret_slots();
        assert_eq!(slots.iter().filter(|slot| slot.name == "email").count(), 1);
        assert_eq!(slots[0].kind, SecretKind::Email);
        assert_eq!(slots[0].label, "Email");
    }

    #[test]
    fn an_optional_secret_is_not_required() {
        let source = CROSSREF.replace(
            r#"mailto = "team@example.com""#,
            r#"mailto = { secret = "email", optional = true }"#,
        );
        let manifest = Manifest::parse(&source).unwrap();
        assert!(manifest.required_secrets().is_empty());
        assert_eq!(manifest.optional_secrets(), vec!["email"]);
    }

    #[test]
    fn a_header_map_is_validated_for_names() {
        let source = CROSSREF.replace(
            "[http.query]",
            "[http.headers]\n\"bad name\" = \"x\"\n[http.query]",
        );
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("not a valid header name"), "{error}");
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
    fn rejects_html_only_options_on_json_fields() {
        for replacement in [
            r#"title = { path = "title[0]", attribute = "href" }"#,
            r#"title = { path = "title[0]", all = true }"#,
            r#"title = { path = "title[0]", then = "@" }"#,
            r#"title = { path = "title[0]", first_of = ["a"] }"#,
        ] {
            let source = CROSSREF.replace(r#"title = "title[0]""#, replacement);
            let error = Manifest::parse(&source).unwrap_err().to_string();
            assert!(
                error.contains("search.fields.title"),
                "{replacement}: {error}"
            );
        }
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
    fn every_capability_but_health_needs_search() {
        let source = r#"
manifest_version = 2
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
    fn browse_declares_typed_controls_the_interface_can_render() {
        let manifest = Manifest::parse(AUTHORING_FULL_EXAMPLE).unwrap();
        let controls = manifest.capabilities.browse.unwrap().controls();
        assert!(controls.keyword);
        assert_eq!(controls.sort.as_ref().unwrap().options.len(), 3);
        // Declaration order, not alphabetical: `year` precedes `type`.
        let names: Vec<_> = controls.filters.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["year", "type", "open_access"]);
        assert_eq!(controls.filters[0].kind, "range");
        assert!(controls.filters[0].has_min && controls.filters[0].has_max);
        assert_eq!(controls.filters[1].options.len(), 2);
        assert_eq!(controls.filters[2].kind, "flag");
    }

    #[test]
    fn browse_is_refused_when_it_cannot_say_where_its_filters_go() {
        let source = AUTHORING_FULL_EXAMPLE.replace("filter_param = \"filter\"\n", "");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("browse.filter_param is required"), "{error}");
    }

    #[test]
    fn a_browse_clause_must_carry_its_value() {
        let source = AUTHORING_FULL_EXAMPLE.replace("type:{value}", "type:fixed");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("must contain {value}"), "{error}");
        let source = AUTHORING_FULL_EXAMPLE.replace("is_oa:true", "is_oa:{value}");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("not available here"), "{error}");
    }

    #[test]
    fn a_browse_default_sort_must_be_one_of_its_options() {
        let source = AUTHORING_FULL_EXAMPLE.replace(
            r#"default = "cited_by_count:desc""#,
            r#"default = "nonsense""#,
        );
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("not one of its options"), "{error}");
    }

    #[test]
    fn lookup_is_bounded_and_must_use_the_doi() {
        let too_many = AUTHORING_FULL_EXAMPLE.replace(
            r#"  "/works?filter=landing_page:https://doi.org/{doi}","#,
            r#"  "/b?d={doi}",
  "/c?d={doi}",
  "/d?d={doi}","#,
        );
        let error = Manifest::parse(&too_many).unwrap_err().to_string();
        assert!(error.contains("the limit is 3"), "{error}");

        let no_doi = AUTHORING_FULL_EXAMPLE
            .replace("/works?filter=doi:https://doi.org/{doi}\",", "/works\",");
        let error = Manifest::parse(&no_doi).unwrap_err().to_string();
        assert!(error.contains("must contain {doi}"), "{error}");
    }

    #[test]
    fn references_chunk_is_bounded() {
        let source = AUTHORING_FULL_EXAMPLE.replace("chunk = 50", "chunk = 500");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("outside 1-100"), "{error}");
        let source = AUTHORING_FULL_EXAMPLE.replace("chunk = 50", "chunk = 0");
        assert!(Manifest::parse(&source).is_err());
    }

    #[test]
    fn authoring_prompt_examples_are_manifests_the_parser_accepts() {
        Manifest::parse(AUTHORING_JSON_EXAMPLE).unwrap();
        Manifest::parse(AUTHORING_HTML_EXAMPLE).unwrap();
        Manifest::parse(AUTHORING_FULL_EXAMPLE).unwrap();
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
        for (field, field_type) in LOOKUP_FIELDS {
            assert!(
                prompt.contains(&format!("- {field}: {}", field_type.name())),
                "prompt omitted lookup field {field}"
            );
        }
        for (name, _) in expr::registered_function_hints() {
            assert!(prompt.contains(name), "prompt omitted function {name}");
        }
        for format in ResponseFormat::ALL {
            assert!(prompt.contains(format.name()));
        }
        for kind in SecretKind::ALL {
            assert!(prompt.contains(kind.name()));
        }
        for required in REQUIRED_SEARCH_FIELDS {
            let field_type = search_field_type(required).unwrap();
            assert!(prompt.contains(&format!("- {required}: {} (required)", field_type.name())));
        }
        assert!(prompt.contains(AUTHORING_JSON_EXAMPLE));
        assert!(prompt.contains(AUTHORING_HTML_EXAMPLE));
        assert!(prompt.contains(AUTHORING_FULL_EXAMPLE));
        assert!(prompt.contains("base_url = \"https://other-origin.test\""));
        assert!(prompt.contains("never copied to a cross-origin step"));
        assert!(prompt.contains(&format!("limited to {MAX_RESOLVER_STEPS}")));
        assert!(prompt.ends_with("before sending the prompt to the model.\n"));
    }
}
