//! Where in a response a value is, and what is done to it on the way out.
//!
//! The expression language is JMESPath (<https://jmespath.org>): a specified,
//! side-effect-free query language for JSON with a conformance suite, no
//! assignment, no recursion and no loops beyond its own projections. It
//! replaces the dotted-path selector and the closed list of coercions this
//! crate used to carry: every new thing a manifest needed — a predicate to
//! pick the PDF out of a `link[]` array, a join, a prefix stripped from an id —
//! was another vocabulary entry to design, document and defend, and a language
//! that already says all of them stops that growth.
//!
//! What JMESPath lacks, this module registers as functions rather than as
//! syntax, so reading an expression still tells you everything it can do:
//!
//! | function | does |
//! | --- | --- |
//! | `normalize_doi(x)` | a DOI in any spelling, to the one this library stores |
//! | `year_from_date(x)` | the leading year of a date string, as an integer |
//! | `strip_html(x)` | tags and entities out of a blurb |
//! | `absolute_url(x)` | a possibly-relative URL, against the capability's base |
//! | `to_int(x)`, `to_bool(x)` | numeric strings and `0`/`1`/`"true"` made typed |
//! | `split(s, sep)`, `replace(s, from, to)` | the two string operations ids need |
//!
//! Every one of them maps a `null` to `null`: an absent value stays absent
//! rather than failing the expression, which is what lets
//! `normalize_doi(ids.doi)` describe an optional field. A value that is
//! *present* and of the wrong shape is an error naming both sides, never a
//! guess.

use jmespath::functions::{ArgumentType, CustomFunction, Signature};
use jmespath::{Context, ErrorReason, JmespathError, Rcvar, Runtime, RuntimeError, Variable};
use serde_json::Value;
use url::Url;

use super::manifest::FieldType;
use crate::metadata::doi::normalize_doi;

/// Names of the functions this module registers, beyond JMESPath's builtins.
const REGISTERED: &[&str] = &[
    "normalize_doi",
    "year_from_date",
    "strip_html",
    "absolute_url",
    "to_int",
    "to_bool",
    "split",
    "replace",
];

/// The JMESPath builtins every conforming implementation provides.
const BUILTINS: &[&str] = &[
    "abs",
    "avg",
    "ceil",
    "contains",
    "ends_with",
    "floor",
    "join",
    "keys",
    "length",
    "map",
    "max",
    "max_by",
    "merge",
    "min",
    "min_by",
    "not_null",
    "reverse",
    "sort",
    "sort_by",
    "starts_with",
    "sum",
    "to_array",
    "to_number",
    "to_string",
    "type",
    "values",
];

/// Evaluates expressions with the manifest's function set.
///
/// Built per base URL because `absolute_url` resolves against it, and a
/// resolver step on another origin resolves against *that* origin.
pub struct Evaluator {
    runtime: Runtime,
}

impl Evaluator {
    pub fn new(base_url: &Url) -> Self {
        let mut runtime = Runtime::new();
        runtime.register_builtin_functions();
        register(
            &mut runtime,
            "normalize_doi",
            vec![ArgumentType::Any],
            |args, ctx| {
                text_in_text_out(args, ctx, "a DOI", |text| {
                    normalize_doi(text).ok_or_else(|| "not a DOI".to_string())
                })
            },
        );
        register(
            &mut runtime,
            "year_from_date",
            vec![ArgumentType::Any],
            |args, ctx| {
                let Some(text) = null_or_text(args, ctx, "a date")? else {
                    return Ok(null());
                };
                // Character-aware: a date string from an unfamiliar service
                // may carry a BOM or a non-ASCII era marker, and `&text[..4]`
                // would panic on the first one of those to arrive.
                let year: String = text.chars().take(4).collect();
                match year.parse::<i64>() {
                    Ok(year) => Ok(number(year)),
                    Err(_) => Err(invalid(
                        ctx,
                        "a date starting with a four-digit year",
                        &args[0],
                    )),
                }
            },
        );
        register(
            &mut runtime,
            "strip_html",
            vec![ArgumentType::Any],
            |args, ctx| text_in_text_out(args, ctx, "a string", |text| Ok(strip_html(text))),
        );
        let base = base_url.clone();
        register(
            &mut runtime,
            "absolute_url",
            vec![ArgumentType::Any],
            move |args, ctx| {
                text_in_text_out(args, ctx, "a URL", |text| {
                    base.join(text)
                        .map(|url| url.to_string())
                        .map_err(|_| "not a URL".to_string())
                })
            },
        );
        register(
            &mut runtime,
            "to_int",
            vec![ArgumentType::Any],
            |args, ctx| match &*args[0] {
                Variable::Null => Ok(null()),
                Variable::Number(value) => match value.as_i64() {
                    Some(value) => Ok(number(value)),
                    None => Err(invalid(ctx, "an integer or a numeric string", &args[0])),
                },
                Variable::String(text) => match text.trim().parse::<i64>() {
                    Ok(value) => Ok(number(value)),
                    Err(_) => Err(invalid(ctx, "an integer or a numeric string", &args[0])),
                },
                _ => Err(invalid(ctx, "an integer or a numeric string", &args[0])),
            },
        );
        register(
            &mut runtime,
            "to_bool",
            vec![ArgumentType::Any],
            |args, ctx| match &*args[0] {
                Variable::Null => Ok(null()),
                Variable::Bool(value) => Ok(boolean(*value)),
                Variable::String(text) => match text.trim().to_ascii_lowercase().as_str() {
                    "true" => Ok(boolean(true)),
                    "false" => Ok(boolean(false)),
                    _ => Err(invalid(ctx, "a boolean", &args[0])),
                },
                Variable::Number(value) => match value.as_i64() {
                    Some(0) => Ok(boolean(false)),
                    Some(1) => Ok(boolean(true)),
                    _ => Err(invalid(ctx, "a boolean, or 0 or 1", &args[0])),
                },
                _ => Err(invalid(ctx, "a boolean", &args[0])),
            },
        );
        register(
            &mut runtime,
            "split",
            vec![ArgumentType::Any, ArgumentType::String],
            |args, ctx| {
                let Some(text) = null_or_text(args, ctx, "a string")? else {
                    return Ok(null());
                };
                let separator = args[1].as_string().cloned().unwrap_or_default();
                if separator.is_empty() {
                    return Err(invalid(ctx, "a non-empty separator", &args[1]));
                }
                Ok(Rcvar::new(Variable::Array(
                    text.split(separator.as_str())
                        .map(|part| Rcvar::new(Variable::String(part.to_string())))
                        .collect(),
                )))
            },
        );
        register(
            &mut runtime,
            "replace",
            vec![
                ArgumentType::Any,
                ArgumentType::String,
                ArgumentType::String,
            ],
            |args, ctx| {
                let Some(text) = null_or_text(args, ctx, "a string")? else {
                    return Ok(null());
                };
                let from = args[1].as_string().cloned().unwrap_or_default();
                if from.is_empty() {
                    return Err(invalid(ctx, "a non-empty search string", &args[1]));
                }
                let to = args[2].as_string().cloned().unwrap_or_default();
                Ok(Rcvar::new(Variable::String(
                    text.replace(from.as_str(), &to),
                )))
            },
        );
        Self { runtime }
    }

    /// The value `expression` selects from `value`, or `None` when it selects
    /// nothing.
    ///
    /// A JSON `null` is the same absence as a missing key: a service that
    /// answers `"doi": null` is saying it has no DOI, and a caller that had to
    /// tell the two apart would be encoding one service's habits into the
    /// projection.
    pub fn eval(&self, expression: &str, value: &Value) -> Result<Option<Value>, String> {
        let compiled = self
            .runtime
            .compile(expression)
            .map_err(|error| describe_error(&error))?;
        let found = compiled
            .search(value)
            .map_err(|error| describe_error(&error))?;
        let value = serde_json::to_value(&*found)
            .map_err(|error| format!("expression result is not JSON: {error}"))?;
        Ok((!value.is_null()).then_some(value))
    }
}

/// Whether `expression` can be evaluated at all: it parses, and every function
/// it calls exists. Everything that can be said without a response.
pub fn check(expression: &str) -> Result<(), String> {
    if expression.trim().is_empty() {
        return Err("expression is empty".to_string());
    }
    let ast = jmespath::parse(expression).map_err(|error| describe_error(&error))?;
    let mut unknown = None;
    walk(&ast, &mut |node| {
        if let jmespath::ast::Ast::Function { name, .. } = node {
            if !BUILTINS.contains(&name.as_str()) && !REGISTERED.contains(&name.as_str()) {
                unknown.get_or_insert_with(|| name.clone());
            }
        }
    });
    match unknown {
        Some(name) => Err(format!(
            "'{name}' is not a function this build provides; available: {}",
            function_names().join(", ")
        )),
        None => Ok(()),
    }
}

/// Every function an expression may call, for the authoring prompt.
pub fn function_names() -> Vec<&'static str> {
    BUILTINS.iter().chain(REGISTERED).copied().collect()
}

/// The registered (non-builtin) functions and what each is for.
pub fn registered_function_hints() -> &'static [(&'static str, &'static str)] {
    &[
        (
            "normalize_doi(x)",
            "DOI in any spelling to normalized text; null stays null",
        ),
        (
            "year_from_date(x)",
            "leading four-digit year of a date string, as an integer",
        ),
        ("strip_html(x)", "HTML-bearing string to plain text"),
        (
            "absolute_url(x)",
            "relative or absolute URL to an absolute URL, against the base",
        ),
        (
            "to_int(x)",
            "integer or numeric string to an integer (to_number yields a float)",
        ),
        (
            "to_bool(x)",
            "boolean, \"true\"/\"false\" or 0/1 to a boolean",
        ),
        ("split(s, sep)", "string to an array of its parts"),
        ("replace(s, from, to)", "every `from` in s replaced by `to`"),
    ]
}

fn walk(ast: &jmespath::ast::Ast, visit: &mut impl FnMut(&jmespath::ast::Ast)) {
    use jmespath::ast::Ast;
    visit(ast);
    match ast {
        Ast::Comparison { lhs, rhs, .. }
        | Ast::And { lhs, rhs, .. }
        | Ast::Or { lhs, rhs, .. }
        | Ast::Projection { lhs, rhs, .. }
        | Ast::Subexpr { lhs, rhs, .. } => {
            walk(lhs, visit);
            walk(rhs, visit);
        }
        Ast::Condition {
            predicate, then, ..
        } => {
            walk(predicate, visit);
            walk(then, visit);
        }
        Ast::Expref { ast, .. } => walk(ast, visit),
        Ast::Flatten { node, .. } | Ast::Not { node, .. } | Ast::ObjectValues { node, .. } => {
            walk(node, visit)
        }
        Ast::Function { args, .. } => args.iter().for_each(|arg| walk(arg, visit)),
        Ast::MultiList { elements, .. } => elements.iter().for_each(|element| walk(element, visit)),
        Ast::MultiHash { elements, .. } => elements
            .iter()
            .for_each(|element| walk(&element.value, visit)),
        Ast::Identity { .. }
        | Ast::Field { .. }
        | Ast::Index { .. }
        | Ast::Literal { .. }
        | Ast::Slice { .. } => {}
    }
}

fn describe_error(error: &JmespathError) -> String {
    format!("{} in `{}`", error.reason, error.expression)
}

fn register(
    runtime: &mut Runtime,
    name: &str,
    inputs: Vec<ArgumentType>,
    f: impl Fn(&[Rcvar], &mut Context<'_>) -> Result<Rcvar, JmespathError> + Send + Sync + 'static,
) {
    runtime.register_function(
        name,
        Box::new(CustomFunction::new(
            Signature::new(inputs, None),
            Box::new(f),
        )),
    );
}

fn null() -> Rcvar {
    Rcvar::new(Variable::Null)
}

fn boolean(value: bool) -> Rcvar {
    Rcvar::new(Variable::Bool(value))
}

fn number(value: i64) -> Rcvar {
    Rcvar::new(Variable::Number(serde_json::Number::from(value)))
}

fn text(value: String) -> Rcvar {
    Rcvar::new(Variable::String(value))
}

fn invalid(ctx: &Context<'_>, expected: &str, found: &Rcvar) -> JmespathError {
    JmespathError::from_ctx(
        ctx,
        ErrorReason::Runtime(RuntimeError::InvalidType {
            expected: expected.to_string(),
            actual: describe_variable(found),
            position: 0,
        }),
    )
}

/// The first argument as text, `None` for a `null`, an error for anything else.
fn null_or_text<'a>(
    args: &'a [Rcvar],
    ctx: &Context<'_>,
    expected: &str,
) -> Result<Option<&'a str>, JmespathError> {
    match &*args[0] {
        Variable::Null => Ok(None),
        Variable::String(text) => Ok(Some(text.as_str())),
        _ => Err(invalid(ctx, expected, &args[0])),
    }
}

fn text_in_text_out(
    args: &[Rcvar],
    ctx: &Context<'_>,
    expected: &str,
    f: impl FnOnce(&str) -> Result<String, String>,
) -> Result<Rcvar, JmespathError> {
    match null_or_text(args, ctx, expected)? {
        None => Ok(null()),
        Some(input) => f(input)
            .map(text)
            .map_err(|_| invalid(ctx, expected, &args[0])),
    }
}

fn describe_variable(value: &Rcvar) -> String {
    match &**value {
        Variable::Null => "null".to_string(),
        Variable::Bool(_) => "a boolean".to_string(),
        Variable::Number(_) => "a number".to_string(),
        Variable::String(text) => {
            let clipped: String = text.chars().take(40).collect();
            format!("\"{clipped}\"")
        }
        Variable::Array(_) => "an array".to_string(),
        Variable::Object(_) => "an object".to_string(),
        Variable::Expref(_) => "an expression reference".to_string(),
    }
}

/// A value that survived selection, in the shape the result struct holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Projected {
    Text(String),
    Integer(i64),
    Boolean(bool),
    Object(serde_json::Map<String, Value>),
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

/// Turn one selected value into one field value.
///
/// The conversion is strict — a `Text` field takes a JSON string and nothing
/// else. Strictness is the point: a service that answers `"citationCount":
/// "12"` where the manifest promised an integer has changed shape, and saying
/// so is more useful than quietly accepting it until the day it answers
/// `"twelve"`. The expression is where a conversion is asked for (`to_int`).
pub fn project(
    field_type: FieldType,
    raw: &Value,
    base_url: &Url,
) -> Result<Projected, TypeMismatch> {
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
        FieldType::Object => raw
            .as_object()
            .cloned()
            .map(Projected::Object)
            .ok_or_else(|| mismatch("an object", raw)),
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
                        "an absolute URL (wrap it in absolute_url(...) to resolve against {})",
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

    fn eval(expression: &str, value: Value) -> Result<Option<Value>, String> {
        Evaluator::new(&base()).eval(expression, &value)
    }

    #[test]
    fn selects_keys_indices_and_quoted_keys() {
        let body =
            json!({"published": {"date-parts": [[2021, 4]]}, "query.bibliographic": {"n": 1}});
        assert_eq!(
            eval(r#"published."date-parts"[0][0]"#, body.clone()).unwrap(),
            Some(json!(2021))
        );
        assert_eq!(
            eval(r#""query.bibliographic".n"#, body).unwrap(),
            Some(json!(1))
        );
    }

    #[test]
    fn a_missing_step_and_an_explicit_null_are_both_absent() {
        let body = json!({"doi": null, "ids": {}});
        for expression in ["doi", "ids.doi", "nope.deep"] {
            assert_eq!(
                eval(expression, body.clone()).unwrap(),
                None,
                "{expression}"
            );
        }
    }

    #[test]
    fn not_null_is_the_ordered_alternative_first_of_used_to_be() {
        let body = json!({"a": null, "b": {"c": "second"}});
        assert_eq!(
            eval("not_null(a, b.c)", body).unwrap(),
            Some(json!("second"))
        );
    }

    /// The one case the dotted-path selector could not say: `first_of` is
    /// *first present*, not *or*, and OpenAlex's `is_open_access` is an `or`.
    #[test]
    fn a_disjunction_is_expressible() {
        let neither =
            json!({"open_access": {"is_oa": false}, "best_oa_location": {"is_oa": false}});
        let second = json!({"open_access": {"is_oa": false}, "best_oa_location": {"is_oa": true}});
        let expression = "open_access.is_oa || best_oa_location.is_oa";
        assert_eq!(eval(expression, neither).unwrap(), Some(json!(false)));
        assert_eq!(eval(expression, second).unwrap(), Some(json!(true)));
    }

    #[test]
    fn a_predicate_picks_the_pdf_link_out_of_a_list() {
        let body = json!({"link": [
            {"URL": "https://x.test/a.html", "content-type": "text/html"},
            {"URL": "https://x.test/a.pdf", "content-type": "application/pdf"},
        ]});
        assert_eq!(
            eval(
                r#"link[?"content-type" == 'application/pdf'].URL | [0]"#,
                body
            )
            .unwrap(),
            Some(json!("https://x.test/a.pdf"))
        );
    }

    #[test]
    fn normalize_doi_maps_null_to_null_and_rejects_a_non_doi_by_name() {
        assert_eq!(eval("normalize_doi(doi)", json!({})).unwrap(), None);
        assert_eq!(
            eval(
                "normalize_doi(doi)",
                json!({"doi": "https://doi.org/10.1145/ABC"})
            )
            .unwrap(),
            Some(json!("10.1145/abc"))
        );
        let error = eval("normalize_doi(doi)", json!({"doi": "not-a-doi"})).unwrap_err();
        assert!(error.contains("a DOI"), "{error}");
        assert!(error.contains("not-a-doi"), "{error}");
    }

    #[test]
    fn year_from_date_is_character_aware() {
        // Two chars, four bytes: byte slicing would have panicked here.
        let error = eval("year_from_date(d)", json!({"d": "二千"})).unwrap_err();
        assert!(error.contains("four-digit year"), "{error}");
        assert_eq!(
            eval("year_from_date(d)", json!({"d": "2021-04-17"})).unwrap(),
            Some(json!(2021))
        );
    }

    #[test]
    fn to_int_accepts_a_quoted_number_and_refuses_a_word() {
        assert_eq!(
            eval("to_int(n)", json!({"n": "12"})).unwrap(),
            Some(json!(12))
        );
        assert_eq!(eval("to_int(n)", json!({"n": 7})).unwrap(), Some(json!(7)));
        assert!(eval("to_int(n)", json!({"n": "twelve"})).is_err());
    }

    #[test]
    fn to_bool_accepts_the_spellings_services_use() {
        for (raw, expected) in [
            (json!(true), true),
            (json!("false"), false),
            (json!(1), true),
            (json!(0), false),
        ] {
            assert_eq!(
                eval("to_bool(v)", json!({ "v": raw })).unwrap(),
                Some(json!(expected))
            );
        }
        assert!(eval("to_bool(v)", json!({"v": "maybe"})).is_err());
    }

    #[test]
    fn absolute_url_resolves_against_the_evaluators_base() {
        assert_eq!(
            eval("absolute_url(u)", json!({"u": "/papers/1.pdf"})).unwrap(),
            Some(json!("https://api.example.test/papers/1.pdf"))
        );
    }

    #[test]
    fn strip_html_and_join() {
        assert_eq!(
            eval(
                "strip_html(t)",
                json!({"t": "<p>An <i>open</i>   book &amp; more</p>"})
            )
            .unwrap(),
            Some(json!("An open book & more"))
        );
        assert_eq!(
            eval("join(', ', names)", json!({"names": ["a", "b"]})).unwrap(),
            Some(json!("a, b"))
        );
    }

    #[test]
    fn an_id_can_be_reduced_to_its_last_path_segment_and_joined() {
        let body =
            json!({"referenced_works": ["https://openalex.org/W10", "https://openalex.org/W11"]});
        assert_eq!(
            eval("map(&split(@, '/')[-1], referenced_works)", body.clone()).unwrap(),
            Some(json!(["W10", "W11"]))
        );
        assert_eq!(
            eval(
                "replace(referenced_works[0], 'https://openalex.org/', '')",
                body
            )
            .unwrap(),
            Some(json!("W10"))
        );
    }

    #[test]
    fn check_refuses_an_unknown_function_by_name_before_any_response_exists() {
        let error = check("shell_out(x)").unwrap_err();
        assert!(error.contains("shell_out"), "{error}");
        assert!(check("normalize_doi(not_null(a, b))").is_ok());
        assert!(check("a.").is_err());
        assert!(check("   ").is_err());
    }

    #[test]
    fn check_finds_a_function_nested_inside_a_projection() {
        let error = check("items[*].{id: id, bad: bogus(x)}").unwrap_err();
        assert!(error.contains("bogus"), "{error}");
    }

    #[test]
    fn project_is_strict_about_types() {
        let error = project(FieldType::Text, &json!(2021), &base()).unwrap_err();
        assert_eq!(error.expected, "a string");
        assert_eq!(error.found, "a number");
        assert_eq!(
            project(FieldType::Integer, &json!(3), &base()).unwrap(),
            Projected::Integer(3)
        );
        assert!(project(FieldType::Integer, &json!("3"), &base()).is_err());
        assert!(matches!(
            project(FieldType::Object, &json!({"a": 1}), &base()).unwrap(),
            Projected::Object(_)
        ));
    }

    #[test]
    fn a_relative_url_says_which_function_fixes_it() {
        let error = project(FieldType::Url, &json!("/papers/1.pdf"), &base()).unwrap_err();
        assert!(error.expected.contains("absolute_url"), "{error}");
        let error = project(FieldType::Url, &json!("javascript:alert(1)"), &base()).unwrap_err();
        assert!(error.expected.contains("http or https"), "{error}");
    }
}
