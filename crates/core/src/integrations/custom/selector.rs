//! Where in a JSON response a value is. Selection only.
//!
//! The grammar is dotted keys and integer indices — `message.items[0].title`,
//! `published.date-parts[0][0]` — with `['quoted keys']` for keys containing a
//! dot or a bracket, and a trailing `[*]` marking the array a capability
//! iterates.
//!
//! There is no filter, no predicate, no arithmetic and no function call, and
//! that absence is the design. Every transformation a manifest can apply comes
//! from the closed vocabulary in [`super::coerce`], so reading a manifest tells
//! you exactly what it can do to a response. A selector language that could
//! express `$.link[?(@.content-type=='application/pdf')].URL` would also be a
//! language, with a parser, a semantics and an evaluation cost to defend; the
//! same fact is reachable here with `first_of` over the two or three places a
//! service actually puts it.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Key(String),
    Index(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selector {
    steps: Vec<Step>,
    /// Set by a trailing `[*]`: the selected value is expected to be an array
    /// the caller will iterate. Only a capability's `items` may use it.
    pub iterates: bool,
}

impl Selector {
    pub fn parse(source: &str) -> Result<Self, String> {
        let mut steps = Vec::new();
        let mut iterates = false;
        let chars: Vec<char> = source.chars().collect();
        let mut i = 0usize;
        let mut expect_key = true;

        if chars.is_empty() {
            return Err("selector is empty".to_string());
        }

        while i < chars.len() {
            match chars[i] {
                '[' => {
                    if iterates {
                        return Err("'[*]' must be the last step".to_string());
                    }
                    let close = find(&chars, i + 1, ']')
                        .ok_or_else(|| "unclosed '[' in selector".to_string())?;
                    let inner: String = chars[i + 1..close].iter().collect();
                    if inner == "*" {
                        iterates = true;
                    } else if let Ok(index) = inner.parse::<usize>() {
                        steps.push(Step::Index(index));
                    } else if let Some(key) = unquote(&inner) {
                        steps.push(Step::Key(key));
                    } else {
                        return Err(format!("'[{inner}]' is not an index, '*', or a quoted key"));
                    }
                    i = close + 1;
                    expect_key = false;
                }
                '.' => {
                    if expect_key {
                        return Err("selector has an empty step".to_string());
                    }
                    expect_key = true;
                    i += 1;
                }
                _ => {
                    if iterates {
                        return Err("'[*]' must be the last step".to_string());
                    }
                    let end = chars[i..]
                        .iter()
                        .position(|c| *c == '.' || *c == '[')
                        .map(|offset| i + offset)
                        .unwrap_or(chars.len());
                    let key: String = chars[i..end].iter().collect();
                    if key.is_empty() {
                        return Err("selector has an empty step".to_string());
                    }
                    steps.push(Step::Key(key));
                    i = end;
                    expect_key = false;
                }
            }
        }

        if expect_key {
            return Err("selector ends with '.'".to_string());
        }
        if steps.is_empty() && !iterates {
            return Err("selector selects nothing".to_string());
        }

        Ok(Self { steps, iterates })
    }

    /// The value at this selector, or `None` if any step is absent.
    ///
    /// A JSON `null` resolves to `None`: a service that answers `"doi": null`
    /// is saying it has no DOI, which is the same absence as not sending the
    /// key at all, and a caller that had to distinguish them would be encoding
    /// one service's habits into the projection.
    pub fn resolve<'a>(&self, root: &'a Value) -> Option<&'a Value> {
        let mut current = root;
        for step in &self.steps {
            current = match step {
                Step::Key(key) => current.get(key)?,
                Step::Index(index) => current.get(index)?,
            };
        }
        (!current.is_null()).then_some(current)
    }
}

fn find(chars: &[char], from: usize, needle: char) -> Option<usize> {
    chars[from..]
        .iter()
        .position(|c| *c == needle)
        .map(|offset| from + offset)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_keys_and_indices() {
        let body = json!({"published": {"date-parts": [[2021, 4]]}});
        let selector = Selector::parse("published.date-parts[0][0]").unwrap();
        assert_eq!(selector.resolve(&body), Some(&json!(2021)));
    }

    #[test]
    fn resolves_a_quoted_key_containing_a_dot() {
        let body = json!({"query.bibliographic": {"n": 1}});
        let selector = Selector::parse("['query.bibliographic'].n").unwrap();
        assert_eq!(selector.resolve(&body), Some(&json!(1)));
    }

    #[test]
    fn a_missing_step_and_an_explicit_null_are_both_absent() {
        let body = json!({"doi": null, "ids": {}});
        assert!(Selector::parse("doi").unwrap().resolve(&body).is_none());
        assert!(Selector::parse("ids.doi").unwrap().resolve(&body).is_none());
        assert!(Selector::parse("nope.deep")
            .unwrap()
            .resolve(&body)
            .is_none());
    }

    #[test]
    fn marks_a_trailing_wildcard_as_iterating() {
        let selector = Selector::parse("message.items[*]").unwrap();
        assert!(selector.iterates);
        let body = json!({"message": {"items": [1, 2]}});
        assert_eq!(selector.resolve(&body), Some(&json!([1, 2])));
    }

    #[test]
    fn rejects_a_wildcard_that_is_not_last() {
        assert!(Selector::parse("items[*].title").is_err());
    }

    #[test]
    fn rejects_malformed_selectors() {
        for source in ["", ".", "a.", "a..b", "a[", "a[x]", "a[1"] {
            assert!(Selector::parse(source).is_err(), "accepted {source:?}");
        }
    }
}
