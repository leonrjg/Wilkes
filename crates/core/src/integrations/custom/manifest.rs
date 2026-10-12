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

use std::collections::{BTreeMap, HashMap};
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

/// A manifest as it is written, which is a [`Manifest`] or the part of one a
/// derived manifest adds to its base. Read only by [`Manifest::resolve`],
/// which is the one place the two become the same thing.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Written {
    manifest_version: u32,
    id: String,
    name: String,
    #[serde(default)]
    extends: Option<Extends>,
    #[serde(default)]
    http: Option<HttpSpec>,
    #[serde(default)]
    capabilities: Capabilities,
}

/// What a derived manifest takes from another provider.
///
/// Whole capabilities, named one by one: a capability has exactly one author,
/// so it is either taken from the base or declared, never merged from both,
/// and a base that later gains one does not hand it to a manifest that never
/// asked for it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Extends {
    /// The base as the registry names it: `openalex`, `custom:crossref`.
    pub provider: String,
    pub capabilities: Vec<String>,
}

/// A provider a manifest may extend, as this installation reaches it.
pub struct Base {
    /// With any origin the user moved it to already in place.
    pub manifest: Manifest,
    /// The values this installation holds for the secrets the base names.
    pub secrets: HashMap<String, String>,
}

/// Where a derived manifest's base is found.
pub trait Bases {
    /// The provider named `provider`, or why it cannot be extended. A base
    /// stands on its own: one that itself extends is refused, so inheritance
    /// is one level deep and cannot form a cycle.
    fn base(&self, provider: &str) -> anyhow::Result<Base>;
}

/// A manifest ready for the engine, and what it took from a base to get there.
pub struct Resolved {
    pub manifest: Manifest,
    pub inherited: Option<Inherited>,
}

/// The part of a resolved manifest that is its base's.
pub struct Inherited {
    pub provider: String,
    /// What the base calls itself.
    pub name: String,
    pub capabilities: Vec<String>,
    /// The base's secrets, which stay the base's: a derived provider sends
    /// them where the base would and stores none of them.
    pub secrets: HashMap<String, String>,
    /// Every secret the base's manifest names, set or not.
    pub secrets_named: Vec<String>,
}

impl Resolved {
    /// The secrets this manifest names itself, which are the ones its own
    /// settings hold. A base's are set on the base.
    pub fn own_secret_slots(&self) -> Vec<SecretSlot> {
        let mut slots = self.manifest.secret_slots();
        if let Some(inherited) = &self.inherited {
            slots.retain(|slot| !inherited.secrets_named.contains(&slot.name));
        }
        slots
    }
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

/// Every capability a manifest may declare, in the order a summary names them.
pub const CAPABILITY_NAMES: &[&str] = &[
    "search",
    "browse",
    "health",
    "lookup",
    "references",
    "resolve_download",
];

impl Capabilities {
    pub fn declares(&self, name: &str) -> bool {
        match name {
            "search" => self.search.is_some(),
            "browse" => self.browse.is_some(),
            "health" => self.health.is_some(),
            "lookup" => self.lookup.is_some(),
            "references" => self.references.is_some(),
            "resolve_download" => self.resolve_download.is_some(),
            _ => false,
        }
    }

    /// The capabilities declared, in [`CAPABILITY_NAMES`] order.
    pub fn declared(&self) -> Vec<&'static str> {
        CAPABILITY_NAMES
            .iter()
            .copied()
            .filter(|name| self.declares(name))
            .collect()
    }

    /// Take one capability, whole, from `base`.
    fn inherit(&mut self, name: &str, base: &Capabilities) {
        match name {
            "search" => self.search = base.search.clone(),
            "browse" => self.browse = base.browse.clone(),
            "health" => self.health = base.health.clone(),
            "lookup" => self.lookup = base.lookup.clone(),
            "references" => self.references = base.references.clone(),
            "resolve_download" => self.resolve_download = base.resolve_download.clone(),
            _ => {}
        }
    }

    /// Names of the secrets these capabilities declare themselves, which is
    /// only ever in a resolver step.
    fn secret_names(&self) -> Vec<&str> {
        self.resolve_download
            .iter()
            .flat_map(|resolve| &resolve.steps)
            .flat_map(|step| step.headers.values().chain(step.query.values()))
            .filter_map(|value| match value {
                ParamValue::Secret(secret) => Some(secret.secret.as_str()),
                ParamValue::Literal(_) => None,
            })
            .collect()
    }
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
/// What the user may vary is declared here rather than as a template the user
/// fills: the engine composes the request from the declared pieces, so a
/// filter can only produce a clause the manifest author wrote, with a value
/// whose shape the manifest declared, and the interface renders its filter
/// panel from this declaration without knowing which service it is talking to.
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
    /// JMESPath over the response body yielding how many records match in
    /// all, so a page of ten can say it is ten of twelve thousand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<String>,
    /// How this service's suggestion responses are read. Declared once: it is
    /// a property of the service, not of each filter that asks for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggest: Option<SuggestSpec>,
    /// The ways several values of one filter may be combined, in the order
    /// the interface offers them. Declared once, because how "any of" and
    /// "none of" are written is this service's filter syntax, not a filter's.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub modes: IndexMap<String, ModeSpec>,
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

/// Where a suggestion response holds its entries and what each one says.
/// JMESPath, like every other JSON selection in a manifest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SuggestSpec {
    /// Where the entries are. Omitted when the body is itself the array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<String>,
    /// The value a filter is set to when the entry is chosen.
    pub value: String,
    /// What the user sees.
    pub label: String,
    /// A second line that tells two entries of one name apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// One way of combining several values of a filter.
///
/// Every mode is the same three steps — rewrite each value, then either put
/// them all in one clause or write the clause once per value — so a mode is a
/// row of data and a service with a different syntax declares different rows.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModeSpec {
    /// What the interface calls it: "Any of", "None of".
    pub label: String,
    /// The values go in one clause, joined by this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join: Option<String>,
    /// The clause is written once per value; the filter separator joins them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repeat: bool,
    /// How each value is written before it is combined. Carries `{value}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

impl ModeSpec {
    /// One value as this mode writes it.
    pub fn write(&self, value: &str) -> String {
        match &self.value {
            Some(template) => template.replace("{value}", value),
            None => value.to_string(),
        }
    }
}

/// A value the user types rather than picks.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Integer,
    Number,
    /// `YYYY-MM-DD`.
    Date,
    Text,
}

/// One filter: a clause, and what its value may be.
///
/// There are no kinds. A clause with no `{value}` is on or off; otherwise
/// exactly one of `options`, `value`, `suggest` and `from_result` says where
/// the value comes from (`suggest` and `from_result` may stand together: the
/// same filter can be typed into or set from a result). A range is two filters
/// sharing a `group`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FilterSpec {
    pub label: String,
    /// Added to the request when the filter is set. `{value}` is the value,
    /// or several combined as the chosen mode says.
    pub clause: String,
    /// Filters sharing a group are drawn together under its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The value is one of these `[value, label]` pairs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<(String, String)>>,
    /// The value is typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<ValueType>,
    /// The value is chosen from the service's suggestions for what was typed.
    /// A path relative to `base_url`, using `{query}` and optionally `{limit}`;
    /// the response is read as `browse.suggest` says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggest: Option<String>,
    /// The value is taken from a search result: the named result field of a
    /// row the user is looking at ("works citing this one").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_result: Option<String>,
    /// The `browse.modes` this filter may combine several values by, the
    /// first being the default. Empty means it takes one value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<String>,
}

/// What a filter's value may be: the one thing that differs between filters,
/// and so the one place the engine tells them apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ValueRule<'a> {
    /// No value: the filter is on or off.
    Toggle,
    Options(&'a [(String, String)]),
    Typed(ValueType),
    /// Free data the service supplied: a suggestion's value, a result's field.
    Free,
}

/// The longest free value a filter accepts.
pub const MAX_FILTER_VALUE_CHARS: usize = 200;
/// The most values one filter accepts.
pub const MAX_FILTER_VALUES: usize = 10;
/// The most suggestions asked for and returned.
pub const MAX_SUGGESTIONS: usize = 10;
/// The fewest characters a suggestion is asked for.
pub const MIN_SUGGEST_CHARS: usize = 2;

impl FilterSpec {
    pub fn rule(&self) -> ValueRule<'_> {
        if let Some(options) = &self.options {
            ValueRule::Options(options)
        } else if let Some(value) = self.value {
            ValueRule::Typed(value)
        } else if self.suggest.is_some() || self.from_result.is_some() {
            ValueRule::Free
        } else {
            ValueRule::Toggle
        }
    }

    /// The control the interface draws, by name. `result` has none of its
    /// own: its value only ever comes from a row.
    pub fn input(&self) -> &'static str {
        match self.rule() {
            ValueRule::Toggle => "toggle",
            ValueRule::Options(_) => "options",
            ValueRule::Typed(ValueType::Integer) => "integer",
            ValueRule::Typed(ValueType::Number) => "number",
            ValueRule::Typed(ValueType::Date) => "date",
            ValueRule::Typed(ValueType::Text) => "text",
            ValueRule::Free if self.suggest.is_some() => "suggest",
            ValueRule::Free => "result",
        }
    }
}

/// Strings a free value may not carry, because they are this service's filter
/// syntax: a value holding one would write a clause, or a combination, that no
/// manifest author declared.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reserved {
    /// May not appear anywhere in a value.
    pub within: Vec<String>,
    /// May not start a value.
    pub leading: Vec<String>,
}

impl ValueRule<'_> {
    /// The value as it goes into a clause, or why it may not.
    ///
    /// The only place filters differ. Everything after it — combining values,
    /// writing the clause — is the same for every filter.
    pub fn admit(&self, raw: &str, reserved: &Reserved) -> Result<String, String> {
        let value = raw.trim();
        match self {
            Self::Toggle => match value {
                "true" => Ok(String::new()),
                other => Err(format!("is on or off; got '{other}'")),
            },
            Self::Options(options) => options
                .iter()
                .any(|(option, _)| option == value)
                .then(|| value.to_string())
                .ok_or_else(|| format!("'{value}' is not one of its options")),
            Self::Typed(ValueType::Integer) => value
                .parse::<i64>()
                .map(|number| number.to_string())
                .map_err(|_| format!("must be a whole number; got '{value}'")),
            Self::Typed(ValueType::Number) => match value.parse::<f64>() {
                Ok(number) if number.is_finite() => Ok(number.to_string()),
                _ => Err(format!("must be a number; got '{value}'")),
            },
            Self::Typed(ValueType::Date) => is_iso_date(value)
                .then(|| value.to_string())
                .ok_or_else(|| format!("must be a date written YYYY-MM-DD; got '{value}'")),
            Self::Typed(ValueType::Text) | Self::Free => free_value(value, reserved),
        }
    }
}

fn is_iso_date(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    let [year, month, day] = parts.as_slice() else {
        return false;
    };
    let digits = |part: &str, len: usize| {
        part.chars().count() == len && part.chars().all(|c| c.is_ascii_digit())
    };
    digits(year, 4)
        && digits(month, 2)
        && digits(day, 2)
        && (1..=12).contains(&month.parse::<u32>().unwrap_or(0))
        && (1..=31).contains(&day.parse::<u32>().unwrap_or(0))
}

/// A value nobody declared in advance. It is refused, by what is wrong with
/// it, rather than escaped: a search silently rewritten is a different search.
fn free_value(value: &str, reserved: &Reserved) -> Result<String, String> {
    if value.is_empty() {
        return Err("cannot be empty".to_string());
    }
    if value.chars().count() > MAX_FILTER_VALUE_CHARS {
        return Err(format!(
            "is longer than {MAX_FILTER_VALUE_CHARS} characters"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err("contains a control character".to_string());
    }
    if let Some(found) = reserved
        .within
        .iter()
        .find(|part| value.contains(part.as_str()))
    {
        return Err(format!(
            "cannot contain '{found}', which this service reads as filter syntax"
        ));
    }
    if let Some(found) = reserved
        .leading
        .iter()
        .find(|part| value.starts_with(part.as_str()))
    {
        return Err(format!(
            "cannot start with '{found}', which this service reads as filter syntax"
        ));
    }
    Ok(value.to_string())
}

/// What a manifest's browse capability offers, for the interface to render.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BrowseControls {
    pub keyword: bool,
    /// Whether a listing says how many records match in all.
    pub total: bool,
    pub sort: Option<SortControl>,
    /// The combination modes, in the order they are offered.
    pub modes: Vec<ControlOption>,
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
    pub group: Option<String>,
    /// `toggle`, `options`, `integer`, `number`, `date`, `text`, `suggest` or
    /// `result`: which control sets the value.
    pub input: String,
    pub options: Vec<ControlOption>,
    /// The modes several values may be combined by; empty means one value.
    pub modes: Vec<String>,
    /// The result field a row offers this filter with, if any.
    pub from_result: Option<String>,
}

impl BrowseCapability {
    pub fn controls(&self) -> BrowseControls {
        BrowseControls {
            keyword: self.query_param.is_some(),
            total: self.total.is_some(),
            sort: self.sort.as_ref().map(|sort| SortControl {
                default: sort.default.clone(),
                keyword_default: sort.keyword_default.clone(),
                options: options_view(&sort.options),
            }),
            modes: self
                .modes
                .iter()
                .map(|(id, mode)| ControlOption {
                    value: id.clone(),
                    label: mode.label.clone(),
                })
                .collect(),
            filters: self
                .filters
                .iter()
                .map(|(name, spec)| FilterControl {
                    name: name.clone(),
                    label: spec.label.clone(),
                    group: spec.group.clone(),
                    input: spec.input().to_string(),
                    options: options_view(spec.options.as_deref().unwrap_or(&[])),
                    modes: spec.modes.clone(),
                    from_result: spec.from_result.clone(),
                })
                .collect(),
        }
    }

    /// The strings this service's filter syntax is made of.
    pub fn reserved(&self) -> Reserved {
        let mut reserved = Reserved {
            within: vec![self.filter_separator.clone()],
            leading: Vec::new(),
        };
        for mode in self.modes.values() {
            if let Some(join) = &mode.join {
                reserved.within.push(join.clone());
            }
            // What a mode writes before a value is how it marks one: a value
            // that began with it would be read as marked.
            if let Some(before) = mode
                .value
                .as_deref()
                .and_then(|template| template.split("{value}").next())
                .filter(|before| !before.is_empty())
            {
                reserved.leading.push(before.to_string());
            }
        }
        reserved
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
    /// A template using result fields and values extracted by prior steps, or
    /// several in order of preference. See [`UrlTemplate`].
    pub url: UrlTemplate,
    /// Optional caller-chosen filename. When absent, the downloader derives
    /// one from the URL and response content type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

/// Where the file is: one template, or the first of several that can be
/// written.
///
/// A template can be written once every value it names is known. A list is
/// how a manifest says a result may already hold what a step would fetch
/// (`["{pdf_url}", "{download_url}"]`): the steps run only while none of its
/// templates can be written, so a result that needs no request costs none.
/// Nothing is tried and retried — which template is used is decided by what
/// is known, never by whether a download worked.
///
/// A single template is no choice, and every step runs for it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum UrlTemplate {
    One(String),
    FirstOf(Vec<String>),
}

impl UrlTemplate {
    /// The templates, in order of preference.
    pub fn alternatives(&self) -> &[String] {
        match self {
            Self::One(template) => std::slice::from_ref(template),
            Self::FirstOf(templates) => templates,
        }
    }

    /// Whether the steps stop as soon as a template can be written.
    pub fn chooses(&self) -> bool {
        matches!(self, Self::FirstOf(_))
    }
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
        /// Resolver steps only: what it means when the selector matches
        /// nothing. See [`Missing`].
        #[serde(default, skip_serializing_if = "Missing::is_error")]
        missing: Missing,
    },
}

/// What a resolver step's field matching nothing says about the result.
///
/// The author knows the page; the engine does not. A link that is absent
/// because the service holds no file for this work and a link that is absent
/// because the page was redesigned look the same from here, so the manifest
/// says which absence is an answer. Only the selector finding nothing is
/// covered: a value that is found and does not fit — a capture that does not
/// match, the wrong type — is the manifest being wrong about the page, and
/// stays an error whatever this says.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Missing {
    /// The manifest no longer describes the response.
    #[default]
    Error,
    /// The service has no file for this result.
    Unavailable,
}

impl Missing {
    const ALL: &'static [Self] = &[Self::Error, Self::Unavailable];

    fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Unavailable => "unavailable",
        }
    }

    fn is_error(&self) -> bool {
        *self == Self::Error
    }
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

    pub fn missing(&self) -> Missing {
        match self {
            Self::Path(_) => Missing::Error,
            Self::Mapped { missing, .. } => *missing,
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
const SUGGEST_PLACEHOLDERS: &[&str] = &["query", "limit"];
const HEALTH_PLACEHOLDERS: &[&str] = &[];
const LOOKUP_PLACEHOLDERS: &[&str] = &["doi"];
const REFERENCES_PLACEHOLDERS: &[&str] = &["doi"];
const REFERENCES_RESOLVE_PLACEHOLDERS: &[&str] = &["ids", "count"];
const MAX_RESOLVER_STEPS: usize = 4;
/// A list of URL templates is a choice, so it has at least two to choose from.
const MIN_URL_ALTERNATIVES: usize = 2;
const MAX_URL_ALTERNATIVES: usize = 4;
pub const MAX_LOOKUP_PATHS: usize = 3;
pub const MAX_REFERENCE_CHUNK: usize = 100;

/// The deepest table header a manifest we ship or teach may use:
/// `[capabilities.browse.filters]`, and nothing below it.
pub const MAX_HEADER_DEPTH: usize = 3;

/// Where a manifest's text departs from the layout Wilkes ships and teaches:
/// every entry of a map (a filter, a field, a header) is one line under one
/// header, and a list of steps is one inline table per step.
///
/// The verbose alternative is a header per entry, then a header per part of
/// an entry (`[capabilities.browse.filters.year]`, or
/// `[[capabilities.resolve_download.steps]]` followed by
/// `[capabilities.resolve_download.steps.query]`), which restates the whole
/// path for every entry and was removed once already. Both layouts are valid
/// TOML and parse to the same manifest, so the parser cannot tell them apart
/// and does not refuse a user's own manifest over it; this is a rule for what
/// Wilkes writes, and the tests hold the shipped manifests and the authoring
/// prompt to it.
pub fn layout_problems(source: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (index, line) in source.lines().enumerate() {
        // A header starts in the first column with a bare key; an array value
        // on a line of its own starts with a quote, a bracket or a space.
        let Some(rest) = line.strip_prefix('[') else {
            continue;
        };
        let array = rest.starts_with('[');
        let rest = rest.trim_start_matches('[');
        let Some(end) = rest.find(']') else {
            continue;
        };
        let path = &rest[..end];
        if path.is_empty()
            || !path
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            continue;
        }
        let depth = path.split('.').count();
        if array {
            problems.push(format!(
                "line {}: [[{path}]] is a header per list item; write the list as one inline table per item",
                index + 1
            ));
        } else if depth > MAX_HEADER_DEPTH {
            problems.push(format!(
                "line {}: [{path}] is a header per entry; write the entry as one line under its parent header",
                index + 1
            ));
        }
    }
    problems
}

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

steps = [
  { base_url = "https://downloads.example.test", path = "/api/download?id={id}", response_format = "json", query = { key = { secret = "member_key" } }, fields = { download_url = "absolute_url(url)" } },
]
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
total = "meta.count"

[capabilities.browse.sort]
param = "sort"
default = "cited_by_count:desc"
keyword_default = "relevance_score:desc"
options = [["cited_by_count:desc", "Most cited"], ["publication_date:desc", "Newest"], ["relevance_score:desc", "Most relevant"]]

[capabilities.browse.suggest]
items = "results"
value = "id"
label = "name"
hint = "affiliation"

[capabilities.browse.modes]
any  = { label = "Any of", join = "|" }
none = { label = "None of", repeat = true, value = "!{value}" }

[capabilities.browse.filters]
year_from   = { label = "From", group = "Year", clause = "from_publication_date:{value}-01-01", value = "integer" }
year_to     = { label = "To", group = "Year", clause = "to_publication_date:{value}-12-31", value = "integer" }
type        = { label = "Type", clause = "type:{value}", modes = ["any", "none"], options = [["article", "Article"], ["book", "Book"]] }
open_access = { label = "Open access only", clause = "is_oa:true" }
title       = { label = "Title mentions", clause = "title.search:{value}", value = "text" }
author      = { label = "Author", clause = "author.id:{value}", modes = ["any", "none"], suggest = "/suggest/authors?q={query}" }
cites       = { label = "Works citing", clause = "cites:{value}", from_result = "id" }

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

const AUTHORING_EXTENDS_EXAMPLE: &str = r#"manifest_version = 2
id = "openalex-files"
name = "OpenAlex with downloads"
extends = { provider = "openalex", capabilities = ["health", "search", "browse"] }

[capabilities.resolve_download]
url = "{download_url}"

steps = [
  { base_url = "https://files.example.test", path = "/by-doi?doi={doi}", query = { key = { secret = "files_key" } }, fields = { download_url = "absolute_url(url)" } },
]
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
    let missing = Missing::ALL
        .iter()
        .map(|missing| missing.name())
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
extends = { provider = "openalex", capabilities = ["search", "browse"] }   # optional; see "Extending a provider" below

[http]                                # required, unless the manifest extends a provider: then it is that provider's and is left out
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
total = "expression"                  # optional; JMESPath over the response yielding how many records match in all

[capabilities.browse.sort]            # optional
param = "sort"
default = "value-of-the-default-option"
keyword_default = "value-used-instead-when-a-keyword-is-given"   # optional
options = [["value", "Label"], ["value2", "Label 2"]]

[capabilities.browse.suggest]         # optional; how every suggestion response of this service is read (JMESPath)
items = "expression"                  # optional; where the entries are
value = "expression"                  # what a filter is set to when the entry is chosen
label = "expression"                  # what the user sees
hint = "expression"                   # optional; a second line that tells two entries apart

[capabilities.browse.modes]           # optional; the ways several values of one filter combine, in the order offered
any = { label = "Any of", join = "|" }                         # join: the values go in one clause, joined by this
all = { label = "All of", repeat = true }                      # repeat: the clause is written once per value
none = { label = "None of", repeat = true, value = "!{value}" } # value: how each value is written first

[capabilities.browse.filters]         # one line per filter, shown in this order; a name is a-z, 0-9, _
# A filter is a label, a clause, and where its value comes from. Write each as one inline table under this one
# header; never a header per filter. A long options array may span lines. There are no kinds:
on_off = { label = "Shown to the user", clause = "field:true" }                                                  # no {value}: on or off
choice = { label = "Shown to the user", clause = "field:{value}", options = [["value", "Label"]] }             # one of a closed list
typed = { label = "Shown to the user", clause = "field:>{value}", value = "integer" }                          # integer | number | date (YYYY-MM-DD) | text
picked = { label = "Shown to the user", clause = "field.id:{value}", suggest = "/suggest/things?q={query}" }  # chosen from the service's suggestions; also {limit}
from_row = { label = "Works citing", clause = "cites:{value}", from_result = "id" }                             # set from a search result field of a row; may stand with suggest
# Exactly one of options, value and suggest, or from_result, when the clause has {value}. Optional on any filter:
#   group = "Year"            filters sharing a group are drawn together, which is how a range is written (two filters)
#   modes = ["any", "none"]   which browse.modes may combine several values, the first being the default; omitted means one value
# Text and suggested values are refused if they contain the filter separator or a mode's join, so they cannot add a clause.

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
url = "{result_or_step_value}"        # required; HTTP(S) or relative to http.base_url. Or a list in order of preference,
                                      # url = ["{pdf_url}", "{step_value}"]: the first whose values are all known is used, and
                                      # steps run only while none is, so a result that already holds the URL costs no request
filename = "{title}.{file_format}"   # optional; sanitized before download

steps = [                             # optional; one inline table per step, in order; never a header per step
  # base_url: optional literal HTTP(S) base, defaults to http.base_url. path: result values plus outputs from earlier steps.
  # response_format: optional, "json" or "html". query and/or headers: this step only. fields: each a required scalar string
  # (the full field-spec object form is also allowed). A step field that matches nothing is an error, unless its object form
  # says missing = "unavailable": then the service has no file for that result, which is an answer and not a failure. Set it
  # only on the field whose absence means exactly that. A value that is found and does not fit is always an error.
  { base_url = "https://other-origin.test", path = "/resolve?id={id}", response_format = "json", query = { param = { secret = "symbolic_secret_name" } }, fields = { new_value = "expression" } },
]

Extending a provider:
`extends` takes whole capabilities from a provider that already exists, named as Wilkes names it: one it ships with (`openalex`, `semantic_scholar`) or one the user wrote (`custom:<id>`). Use it when the service is already described and only something is to be added, such as a download step for its results.
- `capabilities` lists every capability taken; nothing is taken that is not listed. A listed capability is not also declared, and there is no partial merge.
- The manifest has no `[http]`: requests go to the provider's origin with its headers, query parameters and secrets, which stay set on that provider.
- Capabilities declared here run against that same origin. A resolver step reaches another origin through its own `base_url`, `headers` and `query`, and its secret names must differ from the provider's.
- The provider extended must not itself extend another.

JMESPath selects keys (`a.b`), indices (`a[0]`, `a[-1]`), projections (`a[*].b`), filters (`a[?k == 'v']`), alternatives (`not_null(a, b)`) and disjunctions (`a || b`). Quote keys that are not plain identifiers: `published."date-parts"[0][0]`. Standard JMESPath functions are available, plus these:
"#,
    );
    writeln!(prompt, "{functions}").unwrap();
    prompt.push_str("\nSearch result fields:\n");
    writeln!(prompt, "{fields}").unwrap();
    writeln!(prompt, "\nLookup record fields:\n{lookup_fields}").unwrap();
    writeln!(
        prompt,
        "\nClosed enum values: response_format = {response_formats}; secret kind = {secret_kinds}; step field missing = {missing}."
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
    prompt.push_str("\nValid example extending a provider to add a download step:\n\n");
    prompt.push_str(AUTHORING_EXTENDS_EXAMPLE);
    prompt.push_str(
        "\nSOURCE MATERIAL TO TRANSLATE\nPaste the service documentation, HTTP examples, or existing implementation below this line before sending the prompt to the model.\n",
    );
    prompt
}

impl Manifest {
    /// Read a manifest that stands on its own. One that extends another
    /// provider is read by [`Self::resolve`], which knows where to find it.
    pub fn parse(source: &str) -> anyhow::Result<Self> {
        let written = Written::read(source)?;
        if let Some(extends) = &written.extends {
            anyhow::bail!(
                "manifest extends '{}', so it does not stand on its own",
                extends.provider
            );
        }
        Ok(Self::flatten(written, None)?.manifest)
    }

    /// Read a manifest, taking from its base what it says it takes.
    ///
    /// This is the only place a derived manifest is treated as one. What comes
    /// out is an ordinary manifest — the base's origin and identification, the
    /// capabilities taken from it, the ones declared here — so the engine,
    /// validation and everything drawn from a manifest have one thing to read.
    pub fn resolve(source: &str, bases: &dyn Bases) -> anyhow::Result<Resolved> {
        let written = Written::read(source)?;
        let base = match &written.extends {
            Some(extends) => {
                anyhow::ensure!(
                    extends.provider != format!("custom:{}", written.id),
                    "manifest extends '{}', which is itself",
                    extends.provider
                );
                Some(bases.base(&extends.provider)?)
            }
            None => None,
        };
        Self::flatten(written, base)
    }

    fn flatten(written: Written, base: Option<Base>) -> anyhow::Result<Resolved> {
        let Written {
            manifest_version,
            id,
            name,
            extends,
            http,
            mut capabilities,
        } = written;

        let (Some(extends), Some(base)) = (extends, base) else {
            let http = http.ok_or_else(|| {
                anyhow::anyhow!(
                    "manifest declares no [http]; one that extends no provider names its own origin"
                )
            })?;
            let manifest = Manifest {
                manifest_version,
                id,
                name,
                http,
                capabilities,
            };
            manifest.validate()?;
            return Ok(Resolved {
                manifest,
                inherited: None,
            });
        };

        let provider = &extends.provider;
        let mut problems = Vec::new();
        // Every inherited path is relative to the base's origin, so the origin
        // and what identifies a request to it are the base's to declare.
        if http.is_some() {
            problems.push(format!(
                "[http] is taken from '{provider}'; a manifest that extends declares none"
            ));
        }
        if extends.capabilities.is_empty() {
            problems.push(format!(
                "extends.capabilities names nothing to take from '{provider}'"
            ));
        }
        let declared = capabilities.declared();
        // Read before anything is taken from the base: these are the names
        // this manifest chose.
        let own_secrets: Vec<String> = capabilities
            .secret_names()
            .into_iter()
            .map(str::to_string)
            .collect();
        for (index, capability) in extends.capabilities.iter().enumerate() {
            let capability = capability.as_str();
            if !CAPABILITY_NAMES.contains(&capability) {
                problems.push(format!(
                    "extends.capabilities: '{capability}' is not a capability; expected one of: {}",
                    CAPABILITY_NAMES.join(", ")
                ));
            } else if extends.capabilities[..index]
                .iter()
                .any(|seen| seen == capability)
            {
                problems.push(format!("extends.capabilities names '{capability}' twice"));
            } else if declared.contains(&capability) {
                problems.push(format!(
                    "{capability} is both taken from '{provider}' and declared here; a capability has one author"
                ));
            } else if !base.manifest.capabilities.declares(capability) {
                problems.push(format!("'{provider}' declares no {capability} to take"));
            } else {
                capabilities.inherit(capability, &base.manifest.capabilities);
            }
        }
        // A name the base uses is the base's: its value is set there, once.
        let secrets_named: Vec<String> = base
            .manifest
            .secret_slots()
            .into_iter()
            .map(|slot| slot.name)
            .collect();
        let mut clashes: Vec<&String> = own_secrets
            .iter()
            .filter(|name| secrets_named.contains(name))
            .collect();
        clashes.sort_unstable();
        clashes.dedup();
        for name in clashes {
            problems.push(format!(
                "secret '{name}' is one '{provider}' names; give this manifest's own a different name"
            ));
        }

        let manifest = Manifest {
            manifest_version,
            id,
            name,
            http: base.manifest.http.clone(),
            capabilities,
        };
        problems.extend(manifest.problems());
        anyhow::ensure!(
            problems.is_empty(),
            "manifest is invalid:\n  - {}",
            problems.join("\n  - ")
        );
        Ok(Resolved {
            manifest,
            inherited: Some(Inherited {
                provider: extends.provider,
                name: base.manifest.name,
                capabilities: extends.capabilities,
                secrets: base.secrets,
                secrets_named,
            }),
        })
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
                FieldOf::Search,
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
                        FieldOf::ResolverStep,
                    ));
                }
                available.push(name.clone());
            }
        }

        let alternatives = resolve.url.alternatives();
        if resolve.url.chooses()
            && !(MIN_URL_ALTERNATIVES..=MAX_URL_ALTERNATIVES).contains(&alternatives.len())
        {
            problems.push(format!(
                "resolve_download.url lists {} templates; a list is {MIN_URL_ALTERNATIVES} to {MAX_URL_ALTERNATIVES}, and one template is written as a string",
                alternatives.len()
            ));
        }
        for template in alternatives {
            problems.extend(template_problems_owned(
                "resolve_download.url",
                template,
                &available,
                false,
            ));
            if template.trim().is_empty() {
                problems.push("resolve_download.url cannot be empty".to_string());
            }
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

impl Written {
    fn read(source: &str) -> anyhow::Result<Self> {
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
        Ok(if is_json {
            serde_json::from_str(source)?
        } else {
            toml::from_str(source)?
        })
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

/// Which capability a field specification belongs to, for the two things that
/// depend on it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldOf {
    /// The only place a typed input exists to be carried.
    Search,
    Lookup,
    /// The only place a field must match, and so the only place that can say
    /// what it means when it does not.
    ResolverStep,
}

/// Problems with one field's specification, in a response of `format`.
fn field_spec_problems(
    label: &str,
    format: ResponseFormat,
    spec: &FieldSpec,
    field_of: FieldOf,
) -> Vec<String> {
    let allow_input = field_of == FieldOf::Search;
    let mut problems = Vec::new();
    if spec.missing() != Missing::Error && field_of != FieldOf::ResolverStep {
        problems.push(format!(
            "{label}.missing is only for resolve_download step fields; a field that matches nothing here is already left empty"
        ));
    }
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

    if let Some(total) = &browse.total {
        if let Err(error) = expr::check(total) {
            problems.push(format!("browse.total is not a valid expression: {error}"));
        }
    }

    if let Some(suggest) = &browse.suggest {
        for (field, expression) in [
            ("items", suggest.items.as_deref()),
            ("value", Some(suggest.value.as_str())),
            ("label", Some(suggest.label.as_str())),
            ("hint", suggest.hint.as_deref()),
        ] {
            if let Some(Err(error)) = expression.map(expr::check) {
                problems.push(format!(
                    "browse.suggest.{field} is not a valid expression: {error}"
                ));
            }
        }
    }

    for (name, mode) in &browse.modes {
        let label = format!("browse.modes.{name}");
        if !is_variable(name) {
            problems.push(format!(
                "{label}: the mode name must be a-z, 0-9 or '_', starting with a letter"
            ));
        }
        if mode.label.trim().is_empty() {
            problems.push(format!("{label}.label cannot be empty"));
        }
        match (&mode.join, mode.repeat) {
            (Some(join), false) if join.is_empty() => {
                problems.push(format!("{label}.join cannot be empty"))
            }
            (Some(_), false) | (None, true) => {}
            (Some(_), true) => problems.push(format!("{label} sets both join and repeat")),
            (None, false) => problems.push(format!(
                "{label} says neither how its values are joined nor to repeat the clause"
            )),
        }
        if let Some(template) = &mode.value {
            problems.extend(clause_problems(&label, "value", template, true));
        }
    }

    for (name, spec) in &browse.filters {
        let label = format!("browse.filters.{name}");
        if !is_variable(name) {
            problems.push(format!(
                "{label}: the filter name must be a-z, 0-9 or '_', starting with a letter"
            ));
        }
        if spec.label.trim().is_empty() {
            problems.push(format!("{label}.label cannot be empty"));
        }
        if spec
            .group
            .as_deref()
            .is_some_and(|group| group.trim().is_empty())
        {
            problems.push(format!("{label}.group cannot be empty"));
        }

        let sources = [
            spec.options.is_some(),
            spec.value.is_some(),
            spec.suggest.is_some(),
        ]
        .iter()
        .filter(|set| **set)
        .count();
        if sources > 1 {
            problems.push(format!(
                "{label} sets more than one of options, value and suggest"
            ));
        }
        if spec.from_result.is_some() && (spec.options.is_some() || spec.value.is_some()) {
            problems.push(format!(
                "{label}.from_result stands alone or with suggest, not with options or value"
            ));
        }
        let takes_value = sources > 0 || spec.from_result.is_some();
        if !takes_value && spec.clause.contains("{value}") {
            problems.push(format!(
                "{label}.clause takes a {{value}} but the filter declares none of options, value, suggest and from_result"
            ));
        } else {
            problems.extend(clause_problems(&label, "clause", &spec.clause, takes_value));
        }

        if let Some(options) = &spec.options {
            if options.is_empty() {
                problems.push(format!("{label}.options cannot be empty"));
            }
            problems.extend(option_problems(&label, options));
        }
        if let Some(path) = &spec.suggest {
            problems.extend(template_problems(
                &format!("{label}.suggest"),
                path,
                SUGGEST_PLACEHOLDERS,
            ));
            if !path.contains("{query}") {
                problems.push(format!("{label}.suggest must contain {{query}}"));
            }
            if browse.suggest.is_none() {
                problems.push(format!(
                    "{label}.suggest needs browse.suggest to say how the response is read"
                ));
            }
        }
        if let Some(field) = &spec.from_result {
            if !SEARCH_FIELDS.iter().any(|(known, _)| known == field) {
                problems.push(format!(
                    "{label}.from_result '{field}' is not a search result field"
                ));
            }
        }
        if !spec.modes.is_empty() && !takes_value {
            problems.push(format!("{label}.modes needs a filter that takes a value"));
        }
        for (index, mode) in spec.modes.iter().enumerate() {
            if !browse.modes.contains_key(mode) {
                problems.push(format!(
                    "{label}.modes names '{mode}', which browse.modes does not declare"
                ));
            }
            if spec.modes[..index].contains(mode) {
                problems.push(format!("{label}.modes repeats '{mode}'"));
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
            FieldOf::Lookup,
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
    fn browse_declares_controls_the_interface_can_render() {
        let manifest = Manifest::parse(AUTHORING_FULL_EXAMPLE).unwrap();
        let controls = manifest.capabilities.browse.unwrap().controls();
        assert!(controls.keyword);
        assert!(controls.total);
        assert_eq!(controls.sort.as_ref().unwrap().options.len(), 3);
        assert_eq!(
            controls.modes,
            vec![
                ControlOption {
                    value: "any".into(),
                    label: "Any of".into()
                },
                ControlOption {
                    value: "none".into(),
                    label: "None of".into()
                },
            ]
        );
        // Declaration order, not alphabetical, and each filter's control is
        // what its value source calls for.
        let inputs: Vec<_> = controls
            .filters
            .iter()
            .map(|f| (f.name.as_str(), f.input.as_str()))
            .collect();
        assert_eq!(
            inputs,
            vec![
                ("year_from", "integer"),
                ("year_to", "integer"),
                ("type", "options"),
                ("open_access", "toggle"),
                ("title", "text"),
                ("author", "suggest"),
                ("cites", "result"),
            ]
        );
        assert_eq!(controls.filters[0].group.as_deref(), Some("Year"));
        assert_eq!(controls.filters[2].options.len(), 2);
        assert_eq!(controls.filters[2].modes, vec!["any", "none"]);
        assert_eq!(controls.filters[6].from_result.as_deref(), Some("id"));
    }

    #[test]
    fn browse_is_refused_when_it_cannot_say_where_its_filters_go() {
        let source = AUTHORING_FULL_EXAMPLE.replace("filter_param = \"filter\"\n", "");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("browse.filter_param is required"), "{error}");
    }

    #[test]
    fn a_browse_clause_carries_a_value_exactly_when_the_filter_has_one() {
        let source = AUTHORING_FULL_EXAMPLE.replace("type:{value}", "type:fixed");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("must contain {value}"), "{error}");
        let source = AUTHORING_FULL_EXAMPLE.replace("is_oa:true", "is_oa:{value}");
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(
            error.contains("declares none of options, value, suggest"),
            "{error}"
        );
    }

    /// The filter kinds are gone, and a manifest still written with them is
    /// told so by name rather than read as something else.
    #[test]
    fn a_filter_written_with_a_kind_is_refused_by_name() {
        let source = AUTHORING_FULL_EXAMPLE.replace(
            r#"open_access = { label"#,
            r#"open_access = { kind = "flag", label"#,
        );
        let error = format!("{:#}", Manifest::parse(&source).unwrap_err());
        assert!(error.contains("kind"), "{error}");
    }

    #[test]
    fn a_filter_says_where_its_value_comes_from_once() {
        for (from, to, expected) in [
            (
                r#"value = "text" }"#,
                r#"value = "text", options = [["a", "A"]] }"#,
                "more than one of options, value and suggest",
            ),
            (
                r#"from_result = "id" }"#,
                r#"from_result = "id", value = "text" }"#,
                "from_result stands alone or with suggest",
            ),
            (
                r#"from_result = "id" }"#,
                r#"from_result = "nope" }"#,
                "is not a search result field",
            ),
            (
                r#"suggest = "/suggest/authors?q={query}" }"#,
                r#"suggest = "/suggest/authors" }"#,
                "suggest must contain {query}",
            ),
            (
                r#"open_access = { label = "Open access only""#,
                r#"open_access = { modes = ["any"], label = "Open access only""#,
                "modes needs a filter that takes a value",
            ),
            (
                r#"modes = ["any", "none"], options"#,
                r#"modes = ["any", "every"], options"#,
                "names 'every', which browse.modes does not declare",
            ),
        ] {
            assert!(AUTHORING_FULL_EXAMPLE.contains(from), "{from}");
            let source = AUTHORING_FULL_EXAMPLE.replace(from, to);
            let error = Manifest::parse(&source).unwrap_err().to_string();
            assert!(error.contains(expected), "{expected}: {error}");
        }
    }

    #[test]
    fn a_suggesting_filter_needs_the_response_described() {
        let start = AUTHORING_FULL_EXAMPLE
            .find("[capabilities.browse.suggest]")
            .unwrap();
        let end = AUTHORING_FULL_EXAMPLE
            .find("[capabilities.browse.modes]")
            .unwrap();
        let source = format!(
            "{}{}",
            &AUTHORING_FULL_EXAMPLE[..start],
            &AUTHORING_FULL_EXAMPLE[end..]
        );
        let error = Manifest::parse(&source).unwrap_err().to_string();
        assert!(error.contains("needs browse.suggest"), "{error}");
    }

    #[test]
    fn a_mode_says_one_way_of_combining_values() {
        for (from, to, expected) in [
            (
                r#"any  = { label = "Any of", join = "|" }"#,
                r#"any  = { label = "Any of" }"#,
                "neither how its values are joined nor to repeat",
            ),
            (
                r#"any  = { label = "Any of", join = "|" }"#,
                r#"any  = { label = "Any of", join = "|", repeat = true }"#,
                "sets both join and repeat",
            ),
            (
                r#"value = "!{value}" }"#,
                r#"value = "!" }"#,
                "must contain {value}",
            ),
        ] {
            assert!(AUTHORING_FULL_EXAMPLE.contains(from), "{from}");
            let source = AUTHORING_FULL_EXAMPLE.replace(from, to);
            let error = Manifest::parse(&source).unwrap_err().to_string();
            assert!(error.contains(expected), "{expected}: {error}");
        }
    }

    /// What a free value may not carry is the service's own filter syntax,
    /// read off its declaration.
    #[test]
    fn the_reserved_strings_are_the_declared_syntax() {
        let manifest = Manifest::parse(AUTHORING_FULL_EXAMPLE).unwrap();
        let reserved = manifest.capabilities.browse.unwrap().reserved();
        assert_eq!(reserved.within, vec![",", "|"]);
        assert_eq!(reserved.leading, vec!["!"]);
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
    fn a_resolver_url_is_one_template_or_a_short_list_of_them() {
        let with_url = |url: &str| {
            let from = r#"url = "{download_url}""#;
            assert!(AUTHORING_HTML_EXAMPLE.contains(from));
            Manifest::parse(&AUTHORING_HTML_EXAMPLE.replace(from, &format!("url = {url}")))
        };
        let listed = with_url(r#"["{pdf_url}", "{download_url}"]"#).unwrap();
        let url = listed.capabilities.resolve_download.unwrap().url;
        assert!(url.chooses());
        assert_eq!(url.alternatives(), ["{pdf_url}", "{download_url}"]);

        for (url, expected) in [
            (r#"[]"#, "lists 0 templates"),
            (
                r#"["{download_url}"]"#,
                "one template is written as a string",
            ),
            (
                r#"["{id}", "{doi}", "{title}", "{pdf_url}", "{download_url}"]"#,
                "lists 5 templates",
            ),
            (
                r#"["{pdf_url}", "{nowhere}"]"#,
                "resolve_download.url: '{nowhere}' is not available here",
            ),
            (
                r#"["{pdf_url}", ""]"#,
                "resolve_download.url cannot be empty",
            ),
        ] {
            let error = with_url(url).unwrap_err().to_string();
            assert!(error.contains(expected), "{url}: {error}");
        }
    }

    #[test]
    fn only_a_resolver_step_field_may_say_what_its_absence_means() {
        let step = r#"fields = { download_url = { path = "absolute_url(url)", missing = "unavailable" } }"#;
        let source = AUTHORING_HTML_EXAMPLE
            .replace(r#"fields = { download_url = "absolute_url(url)" }"#, step);
        assert!(source.contains(step), "the step was not rewritten");
        let manifest = Manifest::parse(&source).unwrap();
        let resolve = manifest.capabilities.resolve_download.unwrap();
        assert_eq!(
            resolve.steps[0].fields["download_url"].missing(),
            Missing::Unavailable
        );

        // A search or lookup field that matches nothing is already left empty.
        for (from, to, label) in [
            (
                "title = \"name\"\ndoi",
                "title = { path = \"name\", missing = \"unavailable\" }\ndoi",
                "search.fields.title.missing is only for resolve_download step fields",
            ),
            (
                "year = \"year\"",
                "year = { path = \"year\", missing = \"unavailable\" }",
                "lookup.fields.year.missing is only for resolve_download step fields",
            ),
        ] {
            assert!(AUTHORING_FULL_EXAMPLE.contains(from), "{from}");
            let error = Manifest::parse(&AUTHORING_FULL_EXAMPLE.replace(from, to))
                .unwrap_err()
                .to_string();
            assert!(error.contains(label), "{error}");
        }

        // There is no third thing an absence can mean.
        assert!(Manifest::parse(&source.replace("\"unavailable\"", "\"skip\"")).is_err());
    }

    /// What the prompt teaches is what gets written. It shows one line per
    /// entry, and says so, because a model copies the layout it is shown.
    #[test]
    fn the_authoring_prompt_teaches_one_line_per_entry() {
        assert_eq!(layout_problems(&authoring_prompt()), Vec::<String>::new());
    }

    #[test]
    fn a_header_per_entry_and_a_header_per_list_item_are_both_named() {
        let verbose = "[capabilities.browse.filters]\nyear = { kind = \"flag\" }\n\n\
                       [capabilities.browse.filters.year]\nkind = \"range\"\n\n\
                       [[capabilities.resolve_download.steps]]\npath = \"/a\"\n\
                       options = [\n  [\"a\", \"A\"],\n[\"b\", \"B\"],\n]\n";
        let problems = layout_problems(verbose);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("line 4"), "{problems:?}");
        assert!(problems[0].contains("capabilities.browse.filters.year"));
        assert!(problems[1].contains("[[capabilities.resolve_download.steps]]"));
    }

    #[test]
    fn authoring_prompt_examples_are_manifests_the_parser_accepts() {
        Manifest::parse(AUTHORING_JSON_EXAMPLE).unwrap();
        Manifest::parse(AUTHORING_HTML_EXAMPLE).unwrap();
        Manifest::parse(AUTHORING_FULL_EXAMPLE).unwrap();
        // The one that extends is read where there is something to extend.
        let resolved = Manifest::resolve(
            AUTHORING_EXTENDS_EXAMPLE,
            &crate::types::IntegrationsSettings::default(),
        )
        .unwrap();
        assert_eq!(
            resolved.manifest.capabilities.declared(),
            vec!["search", "browse", "health", "resolve_download"]
        );
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
        for missing in Missing::ALL {
            assert!(prompt.contains(missing.name()));
        }
        for required in REQUIRED_SEARCH_FIELDS {
            let field_type = search_field_type(required).unwrap();
            assert!(prompt.contains(&format!("- {required}: {} (required)", field_type.name())));
        }
        assert!(prompt.contains(AUTHORING_JSON_EXAMPLE));
        assert!(prompt.contains(AUTHORING_HTML_EXAMPLE));
        assert!(prompt.contains(AUTHORING_FULL_EXAMPLE));
        assert!(prompt.contains(AUTHORING_EXTENDS_EXAMPLE));
        assert!(prompt.contains("base_url = \"https://other-origin.test\""));
        assert!(prompt.contains("never copied to a cross-origin step"));
        assert!(prompt.contains(&format!("limited to {MAX_RESOLVER_STEPS}")));
        assert!(prompt.ends_with("before sending the prompt to the model.\n"));
    }
}
