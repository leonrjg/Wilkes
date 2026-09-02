//! The closed vocabulary of transformations a manifest may apply.
//!
//! Every entry here is a normalization one of the built-in clients already
//! performs by hand: `normalize_doi` is `openalex/client.rs`'s first line in
//! three functions, `year_from_date` is Semantic Scholar's `publicationDate`
//! reduced to a year, `strip_html` is `catalogue/providers.rs::clip`'s tag
//! removal. Naming them as a list is what keeps a manifest a description —
//! the moment this becomes "and also, arbitrary expressions", the sandbox,
//! the parser and the audit problem all arrive with it.
//!
//! Adding a variant is a deliberate act with a real case behind it, not a
//! release valve for a manifest that does not quite fit.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use super::manifest::FieldType;
use crate::metadata::doi::normalize_doi;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Coercion {
    /// A number or a numeric string to an integer. For services that quote
    /// their numbers, which several do inconsistently across endpoints.
    Int,
    /// A bool, a `"true"`/`"false"` string, or a 0/1 number.
    Bool,
    /// A DOI in any of its spellings to the one this library stores.
    NormalizeDoi,
    /// The leading year of a date string (`"2021-04-17"` → `2021`).
    YearFromDate,
    /// Provider blurbs that arrive as HTML.
    StripHtml,
    /// An array of strings to one string. Services that return `title` as a
    /// one-element array — Crossref does — need this or `[0]`.
    Join,
    /// A possibly-relative URL resolved against the manifest's `base_url`.
    AbsoluteUrl,
}

/// A value that survived selection and coercion, in the shape the result
/// struct holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Projected {
    Text(String),
    Integer(i64),
    Boolean(bool),
}

/// Why a selected value could not become the field's type.
///
/// Carries both sides because the user is looking at their manifest and the
/// service's response and needs to know which one to change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeMismatch {
    pub expected: String,
    pub found: String,
}

impl std::fmt::Display for TypeMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "expected {}, found {}", self.expected, self.found)
    }
}

impl Coercion {
    pub fn name(self) -> &'static str {
        match self {
            Self::Int => "int",
            Self::Bool => "bool",
            Self::NormalizeDoi => "normalize_doi",
            Self::YearFromDate => "year_from_date",
            Self::StripHtml => "strip_html",
            Self::Join => "join",
            Self::AbsoluteUrl => "absolute_url",
        }
    }

    /// Whether this coercion can produce a field of this type. Checked when a
    /// manifest is saved, so `year = { coerce = "strip_html" }` is a refusal
    /// rather than a null at query time.
    pub fn produces(self, field_type: FieldType) -> bool {
        match self {
            Self::Int | Self::YearFromDate => field_type == FieldType::Integer,
            Self::Bool => field_type == FieldType::Boolean,
            Self::NormalizeDoi | Self::StripHtml | Self::Join => field_type == FieldType::Text,
            Self::AbsoluteUrl => field_type == FieldType::Url,
        }
    }
}

/// Turn one selected JSON value into one field value.
///
/// With no coercion the conversion is strict — a `Text` field takes a JSON
/// string and nothing else. Strictness is the point: a service that answers
/// `"citationCount": "12"` where the manifest promised an integer has changed
/// shape, and saying so is more useful than quietly accepting it until the day
/// it answers `"twelve"`.
pub fn project(
    coercion: Option<Coercion>,
    field_type: FieldType,
    raw: &Value,
    base_url: &Url,
) -> Result<Projected, TypeMismatch> {
    match coercion {
        None => strict(field_type, raw, base_url),
        Some(Coercion::Int) => as_int(raw).map(Projected::Integer),
        Some(Coercion::Bool) => as_bool(raw).map(Projected::Boolean),
        Some(Coercion::NormalizeDoi) => {
            let text = as_str(raw)?;
            normalize_doi(text)
                .map(Projected::Text)
                .ok_or(TypeMismatch {
                    expected: "a DOI".to_string(),
                    found: format!("\"{text}\""),
                })
        }
        Some(Coercion::YearFromDate) => {
            let text = as_str(raw)?;
            // Character-aware: a date string from an unfamiliar service may
            // carry a BOM or a non-ASCII era marker, and `&text[..4]` would
            // panic on the first one of those to arrive.
            let year: String = text.chars().take(4).collect();
            year.parse::<i64>()
                .map(Projected::Integer)
                .map_err(|_| TypeMismatch {
                    expected: "a date starting with a four-digit year".to_string(),
                    found: format!("\"{text}\""),
                })
        }
        Some(Coercion::StripHtml) => Ok(Projected::Text(strip_html(as_str(raw)?))),
        Some(Coercion::Join) => {
            let items = raw.as_array().ok_or_else(|| mismatch("an array", raw))?;
            let parts = items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>();
            match parts.is_empty() {
                true => Err(mismatch("an array of strings", raw)),
                false => Ok(Projected::Text(parts.join(", "))),
            }
        }
        Some(Coercion::AbsoluteUrl) => {
            let text = as_str(raw)?;
            base_url
                .join(text)
                .map(|url| Projected::Text(url.to_string()))
                .map_err(|_| mismatch("a URL", raw))
        }
    }
}

fn strict(field_type: FieldType, raw: &Value, base_url: &Url) -> Result<Projected, TypeMismatch> {
    match field_type {
        FieldType::Text => Ok(Projected::Text(as_str(raw)?.to_string())),
        FieldType::Integer => raw
            .as_i64()
            .map(Projected::Integer)
            .ok_or_else(|| mismatch("an integer", raw)),
        FieldType::Boolean => raw
            .as_bool()
            .map(Projected::Boolean)
            .ok_or_else(|| mismatch("a boolean", raw)),
        // A URL field is validated, not merely copied: a `pdf_url` that is not
        // an absolute http(s) URL reaches `acquire::download_to_root`, which
        // would refuse it later and further away from the manifest that
        // produced it.
        FieldType::Url => {
            let text = as_str(raw)?;
            match Url::parse(text) {
                Ok(url) if matches!(url.scheme(), "http" | "https") => {
                    Ok(Projected::Text(url.to_string()))
                }
                Ok(url) => Err(TypeMismatch {
                    expected: "an http or https URL".to_string(),
                    found: format!("a {} URL", url.scheme()),
                }),
                // Relative is not an error the user cannot fix: it is exactly
                // what `absolute_url` is for, so the message says so.
                Err(_) => Err(TypeMismatch {
                    expected: format!(
                        "an absolute URL (use coerce = \"absolute_url\" to resolve against {})",
                        base_url.as_str()
                    ),
                    found: format!("\"{text}\""),
                }),
            }
        }
    }
}

fn as_str(raw: &Value) -> Result<&str, TypeMismatch> {
    raw.as_str().ok_or_else(|| mismatch("a string", raw))
}

fn as_int(raw: &Value) -> Result<i64, TypeMismatch> {
    if let Some(value) = raw.as_i64() {
        return Ok(value);
    }
    raw.as_str()
        .and_then(|text| text.trim().parse::<i64>().ok())
        .ok_or_else(|| mismatch("an integer or a numeric string", raw))
}

fn as_bool(raw: &Value) -> Result<bool, TypeMismatch> {
    if let Some(value) = raw.as_bool() {
        return Ok(value);
    }
    match raw {
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(mismatch("a boolean", raw)),
        },
        Value::Number(number) => match number.as_i64() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(mismatch("a boolean, or 0 or 1", raw)),
        },
        _ => Err(mismatch("a boolean", raw)),
    }
}

fn mismatch(expected: &str, raw: &Value) -> TypeMismatch {
    TypeMismatch {
        expected: expected.to_string(),
        found: describe(raw),
    }
}

fn describe(raw: &Value) -> String {
    match raw {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "a boolean".to_string(),
        Value::Number(_) => "a number".to_string(),
        Value::String(text) => {
            let clipped: String = text.chars().take(40).collect();
            format!("\"{clipped}\"")
        }
        Value::Array(_) => "an array".to_string(),
        Value::Object(_) => "an object".to_string(),
    }
}

/// Remove tags, decode the handful of entities that survive them, and collapse
/// whitespace. Character-aware throughout.
fn strip_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let decoded = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Url {
        Url::parse("https://api.example.test/v1/").unwrap()
    }

    #[test]
    fn strict_text_refuses_a_number() {
        let error = project(None, FieldType::Text, &json!(2021), &base()).unwrap_err();
        assert_eq!(error.expected, "a string");
        assert_eq!(error.found, "a number");
    }

    #[test]
    fn int_accepts_a_quoted_number() {
        let value = project(
            Some(Coercion::Int),
            FieldType::Integer,
            &json!("12"),
            &base(),
        );
        assert_eq!(value.unwrap(), Projected::Integer(12));
    }

    #[test]
    fn normalize_doi_rejects_a_non_doi_by_name() {
        let error = project(
            Some(Coercion::NormalizeDoi),
            FieldType::Text,
            &json!("not-a-doi"),
            &base(),
        )
        .unwrap_err();
        assert_eq!(error.expected, "a DOI");
    }

    #[test]
    fn year_from_date_is_character_aware() {
        // Two chars, four bytes: byte slicing would have panicked here.
        let error = project(
            Some(Coercion::YearFromDate),
            FieldType::Integer,
            &json!("二千"),
            &base(),
        )
        .unwrap_err();
        assert!(error.expected.contains("four-digit year"));

        let value = project(
            Some(Coercion::YearFromDate),
            FieldType::Integer,
            &json!("2021-04-17"),
            &base(),
        );
        assert_eq!(value.unwrap(), Projected::Integer(2021));
    }

    #[test]
    fn a_relative_url_says_which_coercion_fixes_it() {
        let error = project(None, FieldType::Url, &json!("/papers/1.pdf"), &base()).unwrap_err();
        assert!(error.expected.contains("absolute_url"), "{error}");

        let value = project(
            Some(Coercion::AbsoluteUrl),
            FieldType::Url,
            &json!("/papers/1.pdf"),
            &base(),
        );
        assert_eq!(
            value.unwrap(),
            Projected::Text("https://api.example.test/papers/1.pdf".to_string())
        );
    }

    #[test]
    fn a_non_http_url_is_refused() {
        let error =
            project(None, FieldType::Url, &json!("javascript:alert(1)"), &base()).unwrap_err();
        assert!(error.expected.contains("http or https"), "{error}");
    }

    #[test]
    fn join_and_strip_html() {
        assert_eq!(
            project(
                Some(Coercion::Join),
                FieldType::Text,
                &json!(["a", "b"]),
                &base()
            )
            .unwrap(),
            Projected::Text("a, b".to_string())
        );
        assert_eq!(
            project(
                Some(Coercion::StripHtml),
                FieldType::Text,
                &json!("<p>An <i>open</i>   book &amp; more</p>"),
                &base()
            )
            .unwrap(),
            Projected::Text("An open book & more".to_string())
        );
    }

    #[test]
    fn every_coercion_produces_exactly_the_types_it_claims() {
        for coercion in [
            Coercion::Int,
            Coercion::Bool,
            Coercion::NormalizeDoi,
            Coercion::YearFromDate,
            Coercion::StripHtml,
            Coercion::Join,
            Coercion::AbsoluteUrl,
        ] {
            let produced = [
                FieldType::Text,
                FieldType::Integer,
                FieldType::Boolean,
                FieldType::Url,
            ]
            .into_iter()
            .filter(|ty| coercion.produces(*ty))
            .count();
            assert_eq!(produced, 1, "{} claims {produced} types", coercion.name());
        }
    }
}
