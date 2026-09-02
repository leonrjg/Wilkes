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
//! 1. **Templates are typed substitution.** `{query}`, `{limit}` and `{doi}`
//!    are the only placeholders, the engine owns their encoding, and no
//!    template may change the host or scheme — those come from `base_url` and
//!    are fixed when the manifest is saved.
//! 2. **The field map is a projection.** Selection only (see [`super::selector`]),
//!    with every transformation drawn from a closed vocabulary
//!    (see [`super::coerce`]).
//! 3. **Each capability declares one request.** Sequencing a second request
//!    from the first's output is where a manifest becomes a program; see the
//!    exclusions in `docs/internal/specs/custom-integrations.md` §9.
//!
//! Anything a manifest cannot say, it says loudly by failing to load — never
//! by producing a plausible-looking wrong record.

use std::collections::BTreeMap;

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
    /// Where the array of records is in the response body. Omitted when the
    /// body *is* the array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<String>,
    /// Result field name → where to find it in one record.
    pub fields: BTreeMap<String, FieldSpec>,
}

/// How one output field is found.
///
/// The bare-string form is the common case (`title = "display_name"`); the
/// object form adds an ordered fallback or a coercion.
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        coerce: Option<Coercion>,
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

        for param in &self.http.params {
            match (&param.value, &param.secret) {
                (Some(_), Some(_)) => problems.push(format!(
                    "param '{}' sets both value and secret; it must set exactly one",
                    param.name
                )),
                (None, None) => problems.push(format!(
                    "param '{}' sets neither value nor secret",
                    param.name
                )),
                _ => {}
            }
            if param.name.trim().is_empty() {
                problems.push("a param has an empty name".to_string());
            }
            if param.location == ParamLocation::Header && !is_header_name(&param.name) {
                problems.push(format!("param '{}' is not a valid header name", param.name));
            }
        }

        if self.capabilities.health.is_none() && self.capabilities.search.is_none() {
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
            if let Some(items) = &search.items {
                if let Err(error) = Selector::parse(items) {
                    problems.push(format!("search.items: {error}"));
                }
            }
            problems.extend(self.search_field_problems(search));
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
            if paths.is_empty() {
                problems.push(format!(
                    "search.fields.{field} names neither a path nor first_of"
                ));
            }
            for path in paths {
                if let Err(error) = Selector::parse(path) {
                    problems.push(format!("search.fields.{field}: {error}"));
                }
            }

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

    /// The one host this manifest may contact, for the import dialog to show
    /// before anything is saved.
    pub fn host(&self) -> Option<String> {
        Url::parse(&self.http.base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
    }

    /// Names of the secrets this manifest needs supplied.
    pub fn required_secrets(&self) -> Vec<&str> {
        self.http
            .params
            .iter()
            .filter_map(|param| param.secret.as_deref())
            .collect()
    }
}

/// Placeholders present in a template, and whether they are ones this
/// capability supplies.
fn template_problems(field: &str, template: &str, allowed: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut rest = template;

    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            problems.push(format!("{field}: unclosed '{{' in template"));
            return problems;
        };
        let placeholder = &after[..close];
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
        rest = &after[close + 1..];
    }

    if !template.starts_with('/') {
        problems.push(format!("{field}: must start with '/'"));
    }

    problems
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
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
}
