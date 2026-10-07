//! Manifests written before version 2, converted once.
//!
//! Version 1 spelled identification as a list of `{location, name, value |
//! secret}` records, selected JSON with a dotted-path grammar, and
//! post-processed with a closed list of `coerce` names. Version 2 spells
//! identification as `headers` and `query` maps, selects JSON with JMESPath,
//! and post-processes with functions in the expression ([`super::expr`]).
//!
//! This module is *not* a second way to read a manifest. [`Manifest::parse`]
//! accepts version 2 and refuses everything else; this exists so that a
//! manifest a user stored under version 1 is rewritten as version 2 the first
//! time settings are loaded, after which nothing in the application ever
//! meets version 1 again. Its output is the v2 [`Manifest`] type, serialized,
//! and the rewrite is lossy in exactly one way — comments in the stored text —
//! which the caller logs.
//!
//! Every conversion is mechanical and total or it fails loudly: a selector
//! this module cannot express in JMESPath, or a v1 shape it does not
//! recognise, is an error naming the manifest, never a guess.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::manifest::{
    Capabilities, FieldSpec, HealthCapability, HttpSpec, Manifest, ParamValue,
    ResolveDownloadCapability, ResolverStep, ResponseFormat, SearchCapability, SecretRef,
    MANIFEST_VERSION,
};

const LEGACY_VERSION: u32 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1Manifest {
    manifest_version: u32,
    id: String,
    name: String,
    http: V1Http,
    #[serde(default)]
    capabilities: V1Capabilities,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1Http {
    base_url: String,
    #[serde(default)]
    params: Vec<V1Param>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1Param {
    location: V1Location,
    name: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    secret: Option<String>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum V1Location {
    Header,
    Query,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct V1Capabilities {
    #[serde(default)]
    health: Option<V1Health>,
    #[serde(default)]
    search: Option<V1Search>,
    #[serde(default)]
    resolve_download: Option<V1Resolve>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1Health {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1Search {
    path: String,
    #[serde(default)]
    response_format: ResponseFormat,
    #[serde(default)]
    items: Option<String>,
    fields: BTreeMap<String, V1Field>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1Resolve {
    #[serde(default)]
    steps: Vec<V1Step>,
    url: String,
    #[serde(default)]
    filename: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1Step {
    #[serde(default)]
    base_url: Option<String>,
    path: String,
    #[serde(default)]
    response_format: ResponseFormat,
    #[serde(default)]
    params: Vec<V1Param>,
    fields: BTreeMap<String, V1Field>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum V1Field {
    Path(String),
    Mapped(V1Mapped),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1Mapped {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    first_of: Vec<String>,
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    coerce: Option<V1Coercion>,
    #[serde(default)]
    attribute: Option<String>,
    #[serde(default)]
    capture: Option<String>,
    #[serde(default)]
    capture_group: Option<usize>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum V1Coercion {
    Int,
    Bool,
    NormalizeDoi,
    YearFromDate,
    StripHtml,
    Join,
    AbsoluteUrl,
}

impl V1Coercion {
    /// The function that does what this coercion did.
    fn function(self) -> &'static str {
        match self {
            Self::Int => "to_int",
            Self::Bool => "to_bool",
            Self::NormalizeDoi => "normalize_doi",
            Self::YearFromDate => "year_from_date",
            Self::StripHtml => "strip_html",
            // Wrapped specially: `join` takes the separator first.
            Self::Join => "join",
            Self::AbsoluteUrl => "absolute_url",
        }
    }

    fn apply(self, inner: &str) -> String {
        match self {
            Self::Join => format!("join(', ', {inner})"),
            other => format!("{}({inner})", other.function()),
        }
    }
}

/// Rewrite `source` as a version 2 manifest if it is a version 1 one.
///
/// `Ok(None)` means it is already current and nothing needs doing. An error
/// means it could not be converted and should be left as stored: the registry
/// will then report it unloadable, loudly, rather than this module guessing.
pub fn upgrade(source: &str) -> anyhow::Result<Option<String>> {
    let is_json = source.trim_start().starts_with('{');
    let version = declared_version(source, is_json)?;
    if version != LEGACY_VERSION {
        return Ok(None);
    }

    let legacy: V1Manifest = if is_json {
        serde_json::from_str(source)?
    } else {
        toml::from_str(source)?
    };
    debug_assert_eq!(legacy.manifest_version, LEGACY_VERSION);
    let manifest = convert(legacy)?;
    manifest.validate()?;
    Ok(Some(if is_json {
        serde_json::to_string_pretty(&manifest)?
    } else {
        toml::to_string_pretty(&manifest)?
    }))
}

/// Whether `source` declares itself a version 1 manifest, which is the only
/// kind [`upgrade`] converts. False for anything that is not a manifest at all.
pub fn declares_legacy_version(source: &str) -> bool {
    let is_json = source.trim_start().starts_with('{');
    declared_version(source, is_json).is_ok_and(|version| version == LEGACY_VERSION)
}

fn declared_version(source: &str, is_json: bool) -> anyhow::Result<u32> {
    let version = if is_json {
        serde_json::from_str::<serde_json::Value>(source)?
            .get("manifest_version")
            .and_then(serde_json::Value::as_u64)
    } else {
        toml::from_str::<toml::Table>(source)?
            .get("manifest_version")
            .and_then(toml::Value::as_integer)
            .and_then(|version| u64::try_from(version).ok())
    };
    version
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| anyhow::anyhow!("manifest declares no manifest_version"))
}

fn convert(legacy: V1Manifest) -> anyhow::Result<Manifest> {
    let (headers, query) = convert_params(&legacy.http.params)?;
    let capabilities = legacy.capabilities;

    Ok(Manifest {
        manifest_version: MANIFEST_VERSION,
        id: legacy.id,
        name: legacy.name,
        http: HttpSpec {
            base_url: legacy.http.base_url,
            headers,
            query,
        },
        capabilities: Capabilities {
            health: capabilities
                .health
                .map(|health| HealthCapability { path: health.path }),
            search: capabilities
                .search
                .map(|search| -> anyhow::Result<SearchCapability> {
                    let format = search.response_format;
                    Ok(SearchCapability {
                        path: search.path,
                        response_format: format,
                        items: search
                            .items
                            .map(|items| convert_items(&items, format))
                            .transpose()?,
                        fields: convert_fields(search.fields, format)?,
                    })
                })
                .transpose()?,
            resolve_download: capabilities
                .resolve_download
                .map(|resolve| -> anyhow::Result<ResolveDownloadCapability> {
                    Ok(ResolveDownloadCapability {
                        steps: resolve
                            .steps
                            .into_iter()
                            .map(|step| {
                                let (headers, query) = convert_params(&step.params)?;
                                Ok(ResolverStep {
                                    base_url: step.base_url,
                                    path: step.path,
                                    response_format: step.response_format,
                                    headers,
                                    query,
                                    fields: convert_fields(step.fields, step.response_format)?,
                                })
                            })
                            .collect::<anyhow::Result<Vec<_>>>()?,
                        url: resolve.url,
                        filename: resolve.filename,
                    })
                })
                .transpose()?,
            ..Capabilities::default()
        },
    })
}

type ParamMaps = (BTreeMap<String, ParamValue>, BTreeMap<String, ParamValue>);

fn convert_params(params: &[V1Param]) -> anyhow::Result<ParamMaps> {
    let mut headers = BTreeMap::new();
    let mut query = BTreeMap::new();
    for param in params {
        let value = match (&param.value, &param.secret) {
            (Some(value), None) => ParamValue::Literal(value.clone()),
            (None, Some(secret)) => ParamValue::Secret(SecretRef {
                secret: secret.clone(),
                optional: false,
                label: None,
                kind: None,
            }),
            _ => anyhow::bail!(
                "param '{}' must set exactly one of value or secret",
                param.name
            ),
        };
        let target = match param.location {
            V1Location::Header => &mut headers,
            V1Location::Query => &mut query,
        };
        if target.insert(param.name.clone(), value).is_some() {
            anyhow::bail!(
                "param '{}' is declared twice; version 2 keeps one value per name",
                param.name
            );
        }
    }
    Ok((headers, query))
}

fn convert_fields(
    fields: BTreeMap<String, V1Field>,
    format: ResponseFormat,
) -> anyhow::Result<BTreeMap<String, FieldSpec>> {
    fields
        .into_iter()
        .map(|(name, field)| {
            let spec = convert_field(field, format)
                .map_err(|error| anyhow::anyhow!("field '{name}': {error}"))?;
            Ok((name, spec))
        })
        .collect()
}

fn convert_field(field: V1Field, format: ResponseFormat) -> anyhow::Result<FieldSpec> {
    let mapped = match field {
        V1Field::Path(path) => {
            return Ok(FieldSpec::Path(match format {
                ResponseFormat::Json => jmespath_from_dotted(&path)?,
                ResponseFormat::Html => path,
            }))
        }
        V1Field::Mapped(mapped) => mapped,
    };

    // A typed input is not a response selector, whatever the format.
    if let Some(input) = mapped.input {
        return Ok(FieldSpec::Mapped {
            path: None,
            first_of: Vec::new(),
            input: Some(input),
            attribute: None,
            all: false,
            then: mapped.coerce.map(|coerce| coerce.apply("@")),
            capture: mapped.capture,
            capture_group: mapped.capture_group,
        });
    }

    match format {
        ResponseFormat::Json => {
            let mut alternatives = Vec::new();
            for path in mapped.path.iter().chain(&mapped.first_of) {
                alternatives.push(jmespath_from_dotted(path)?);
            }
            let selected = match alternatives.len() {
                0 => anyhow::bail!("names neither a selector nor an input"),
                1 => alternatives.remove(0),
                _ => format!("not_null({})", alternatives.join(", ")),
            };
            let expression = match mapped.coerce {
                Some(coerce) => coerce.apply(&selected),
                None => selected,
            };
            if mapped.attribute.is_some() {
                anyhow::bail!("attribute is only meaningful for HTML");
            }
            Ok(FieldSpec::Mapped {
                path: Some(expression),
                first_of: Vec::new(),
                input: None,
                attribute: None,
                all: false,
                then: None,
                capture: mapped.capture,
                capture_group: mapped.capture_group,
            })
        }
        ResponseFormat::Html => {
            let joins = matches!(mapped.coerce, Some(V1Coercion::Join));
            Ok(FieldSpec::Mapped {
                path: mapped.path,
                first_of: mapped.first_of,
                input: None,
                attribute: mapped.attribute,
                all: joins,
                then: mapped.coerce.map(|coerce| coerce.apply("@")),
                capture: mapped.capture,
                capture_group: mapped.capture_group,
            })
        }
    }
}

/// `items` selected the array to iterate with a trailing `[*]`; JMESPath needs
/// no marker, so it is dropped.
fn convert_items(items: &str, format: ResponseFormat) -> anyhow::Result<String> {
    match format {
        ResponseFormat::Html => Ok(items.to_string()),
        ResponseFormat::Json => {
            let expression = jmespath_from_dotted(items)?;
            Ok(expression
                .strip_suffix("[*]")
                .map(str::to_string)
                .unwrap_or(expression))
        }
    }
}

/// The version 1 selector grammar — dotted keys, `[n]`, `['quoted keys']` and a
/// trailing `[*]` — as the JMESPath expression that selects the same value.
///
/// The only real difference is quoting: JMESPath identifiers are
/// `[A-Za-z_][A-Za-z0-9_]*`, and version 1 keys were any run of characters
/// other than `.` and `[`, so `date-parts` becomes `"date-parts"`.
pub(crate) fn jmespath_from_dotted(source: &str) -> anyhow::Result<String> {
    let chars: Vec<char> = source.chars().collect();
    if chars.is_empty() {
        anyhow::bail!("selector is empty");
    }
    let mut out = String::new();
    let mut i = 0usize;
    let mut expect_key = true;
    while i < chars.len() {
        match chars[i] {
            '[' => {
                let close = chars[i + 1..]
                    .iter()
                    .position(|c| *c == ']')
                    .map(|offset| i + 1 + offset)
                    .ok_or_else(|| anyhow::anyhow!("unclosed '[' in selector '{source}'"))?;
                let inner: String = chars[i + 1..close].iter().collect();
                if inner == "*" || inner.parse::<usize>().is_ok() {
                    out.push('[');
                    out.push_str(&inner);
                    out.push(']');
                } else if let Some(key) = unquote(&inner) {
                    if !out.is_empty() {
                        out.push('.');
                    }
                    out.push_str(&quote_key(&key));
                } else {
                    anyhow::bail!("'[{inner}]' is not an index, '*', or a quoted key");
                }
                i = close + 1;
                expect_key = false;
            }
            '.' => {
                if expect_key {
                    anyhow::bail!("selector '{source}' has an empty step");
                }
                expect_key = true;
                i += 1;
            }
            _ => {
                let end = chars[i..]
                    .iter()
                    .position(|c| *c == '.' || *c == '[')
                    .map(|offset| i + offset)
                    .unwrap_or(chars.len());
                let key: String = chars[i..end].iter().collect();
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(&quote_key(&key));
                i = end;
                expect_key = false;
            }
        }
    }
    if expect_key {
        anyhow::bail!("selector '{source}' ends with '.'");
    }
    Ok(out)
}

fn unquote(value: &str) -> Option<String> {
    let mut chars = value.chars();
    let open = chars.next()?;
    if open != '\'' && open != '"' {
        return None;
    }
    let rest: String = chars.collect();
    let inner = rest.strip_suffix(open)?;
    (!inner.is_empty()).then(|| inner.to_string())
}

fn quote_key(key: &str) -> String {
    let mut chars = key.chars();
    let plain = matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    match plain {
        true => key.to_string(),
        false => format!("\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\"")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::custom::expr::Evaluator;
    use serde_json::json;
    use url::Url;

    fn eval(expression: &str, value: serde_json::Value) -> Option<serde_json::Value> {
        Evaluator::new(&Url::parse("https://example.test").unwrap())
            .eval(expression, &value)
            .unwrap()
    }

    #[test]
    fn dotted_paths_become_the_jmespath_that_selects_the_same_value() {
        let body = json!({
            "published": {"date-parts": [[2021, 4]]},
            "query.bibliographic": {"n": 1},
            "ids": {"doi": "10.1/x"},
            "results": [{"a": 1}],
        });
        for (dotted, expected) in [
            ("published.date-parts[0][0]", json!(2021)),
            ("['query.bibliographic'].n", json!(1)),
            ("ids.doi", json!("10.1/x")),
            ("results[0].a", json!(1)),
        ] {
            let converted = jmespath_from_dotted(dotted).unwrap();
            assert_eq!(
                eval(&converted, body.clone()),
                Some(expected),
                "{dotted} -> {converted}"
            );
        }
    }

    #[test]
    fn malformed_selectors_are_errors_not_guesses() {
        for source in ["", ".", "a.", "a..b", "a[", "a[x]", "a[1"] {
            assert!(jmespath_from_dotted(source).is_err(), "accepted {source:?}");
        }
    }

    const CROSSREF_V1: &str = r#"
# a comment that the rewrite will not keep
manifest_version = 1
id = "crossref"
name = "Crossref"

[http]
base_url = "https://api.crossref.org"

[[http.params]]
location = "query"
name = "mailto"
value = "team@example.com"

[[http.params]]
location = "header"
name = "Crossref-Plus-API-Token"
secret = "crossref_token"

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
authors = { path = "author", coerce = "join" }
"#;

    #[test]
    fn a_version_one_manifest_becomes_a_valid_version_two_manifest() {
        let upgraded = upgrade(CROSSREF_V1).unwrap().expect("v1 is converted");
        let manifest = Manifest::parse(&upgraded).expect("the result is valid v2");

        assert_eq!(manifest.manifest_version, 2);
        assert_eq!(
            manifest.http.query.get("mailto"),
            Some(&ParamValue::Literal("team@example.com".into()))
        );
        assert!(matches!(
            manifest.http.headers.get("Crossref-Plus-API-Token"),
            Some(ParamValue::Secret(secret)) if secret.secret == "crossref_token" && !secret.optional
        ));
        let search = manifest.capabilities.search.unwrap();
        assert_eq!(search.items.as_deref(), Some("message.items"));
        assert_eq!(
            search.fields["doi"],
            FieldSpec::Mapped {
                path: Some("normalize_doi(DOI)".into()),
                first_of: vec![],
                input: None,
                attribute: None,
                all: false,
                then: None,
                capture: None,
                capture_group: None,
            }
        );
        // `first_of` was "first present"; `not_null` says exactly that.
        match &search.fields["pdf_url"] {
            FieldSpec::Mapped {
                path: Some(path), ..
            } => {
                assert_eq!(path, "not_null(link[0].URL, resource.primary.URL)")
            }
            other => panic!("unexpected pdf_url spec {other:?}"),
        }
    }

    #[test]
    fn the_converted_selectors_select_what_the_old_ones_did() {
        let manifest = Manifest::parse(&upgrade(CROSSREF_V1).unwrap().unwrap()).unwrap();
        let search = manifest.capabilities.search.unwrap();
        let evaluator = Evaluator::new(&Url::parse("https://api.crossref.org").unwrap());
        let record = json!({
            "DOI": "10.1145/ABC",
            "published": {"date-parts": [[2021, 4]]},
            "is-referenced-by-count": "12",
            "resource": {"primary": {"URL": "https://x.test/p.pdf"}},
            "author": ["Ada", "Grace"],
        });
        let path = |field: &str| match &search.fields[field] {
            FieldSpec::Mapped {
                path: Some(path), ..
            }
            | FieldSpec::Path(path) => path.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            evaluator.eval(&path("doi"), &record).unwrap(),
            Some(json!("10.1145/abc"))
        );
        assert_eq!(
            evaluator.eval(&path("year"), &record).unwrap(),
            Some(json!(2021))
        );
        assert_eq!(
            evaluator.eval(&path("citation_count"), &record).unwrap(),
            Some(json!(12))
        );
        assert_eq!(
            evaluator.eval(&path("pdf_url"), &record).unwrap(),
            Some(json!("https://x.test/p.pdf"))
        );
        assert_eq!(
            evaluator.eval(&path("authors"), &record).unwrap(),
            Some(json!("Ada, Grace"))
        );
    }

    const HTML_V1: &str = r#"
manifest_version = 1
id = "anna"
name = "Anna"

[http]
base_url = "https://anna.example.test"

[[http.params]]
location = "header"
name = "User-Agent"
value = "Mozilla/5.0"

[capabilities.search]
path = "/search?q={query}"
response_format = "html"
items = "div.result"

[capabilities.search.fields]
id = { path = "a.md5", attribute = "href", capture = '''^/md5/([0-9a-f]+)$''' }
title = "h2"
authors = { path = "a.author", coerce = "join" }
doi = { input = "query", coerce = "normalize_doi" }
landing_page_url = { path = "a.md5", attribute = "href", coerce = "absolute_url" }
language = { first_of = ["span.lang", "div.lang"] }

[capabilities.resolve_download]
url = "{download_url}"
filename = "{title}.{file_format}"

[[capabilities.resolve_download.steps]]
path = "/api/fast_download.json?md5={id}"
response_format = "json"

[[capabilities.resolve_download.steps.params]]
location = "query"
name = "key"
secret = "anna_key"

[capabilities.resolve_download.steps.fields]
download_url = { path = "download_url", coerce = "absolute_url" }
"#;

    #[test]
    fn html_coercions_become_typed_extraction_options() {
        let manifest = Manifest::parse(&upgrade(HTML_V1).unwrap().unwrap()).unwrap();
        let search = manifest.capabilities.search.unwrap();

        // `join` over repeated elements: collect them all, then join.
        match &search.fields["authors"] {
            FieldSpec::Mapped {
                all, then, path, ..
            } => {
                assert!(*all);
                assert_eq!(then.as_deref(), Some("join(', ', @)"));
                assert_eq!(path.as_deref(), Some("a.author"));
            }
            other => panic!("{other:?}"),
        }
        match &search.fields["doi"] {
            FieldSpec::Mapped { input, then, .. } => {
                assert_eq!(input.as_deref(), Some("query"));
                assert_eq!(then.as_deref(), Some("normalize_doi(@)"));
            }
            other => panic!("{other:?}"),
        }
        match &search.fields["landing_page_url"] {
            FieldSpec::Mapped {
                then, attribute, ..
            } => {
                assert_eq!(then.as_deref(), Some("absolute_url(@)"));
                assert_eq!(attribute.as_deref(), Some("href"));
            }
            other => panic!("{other:?}"),
        }
        match &search.fields["language"] {
            FieldSpec::Mapped { first_of, .. } => assert_eq!(first_of.len(), 2),
            other => panic!("{other:?}"),
        }

        let resolve = manifest.capabilities.resolve_download.unwrap();
        let step = &resolve.steps[0];
        assert!(matches!(
            step.query.get("key"),
            Some(ParamValue::Secret(secret)) if secret.secret == "anna_key"
        ));
        assert!(step.headers.is_empty());
        match &step.fields["download_url"] {
            FieldSpec::Mapped { path, .. } => {
                assert_eq!(path.as_deref(), Some("absolute_url(download_url)"))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn only_a_version_one_manifest_declares_the_legacy_version() {
        assert!(declares_legacy_version(CROSSREF_V1));
        assert!(!declares_legacy_version(
            &upgrade(CROSSREF_V1).unwrap().unwrap()
        ));
        assert!(!declares_legacy_version("id = 'x'"));
        assert!(!declares_legacy_version("this is not a manifest ["));
    }

    #[test]
    fn a_current_manifest_is_left_alone() {
        let current = upgrade(CROSSREF_V1).unwrap().unwrap();
        assert_eq!(upgrade(&current).unwrap(), None);
    }

    #[test]
    fn a_json_manifest_stays_json() {
        let as_json =
            serde_json::to_string(&toml::from_str::<toml::Value>(CROSSREF_V1).unwrap()).unwrap();
        let upgraded = upgrade(&as_json).unwrap().unwrap();
        assert!(upgraded.trim_start().starts_with('{'));
        Manifest::parse(&upgraded).unwrap();
    }

    #[test]
    fn a_param_that_sets_both_value_and_secret_is_an_error() {
        let broken = CROSSREF_V1.replace(
            r#"value = "team@example.com""#,
            "value = \"x\"\nsecret = \"y\"",
        );
        let error = upgrade(&broken).unwrap_err().to_string();
        assert!(error.contains("exactly one"), "{error}");
    }

    #[test]
    fn a_manifest_without_a_version_is_an_error_not_a_guess() {
        let error = upgrade("id = 'x'").unwrap_err().to_string();
        assert!(error.contains("manifest_version"), "{error}");
    }
}
