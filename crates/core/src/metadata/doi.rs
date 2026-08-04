use std::sync::OnceLock;

use regex::Regex;

fn doi_regex() -> Regex {
    Regex::new(r"(?i)\b(?:https?://(?:dx\.)?doi\.org/|doi:\s*)?(10\.\d{4,9}/[-._;()/:A-Z0-9]+)")
        .expect("valid DOI regex")
}

fn doi_regex_ref() -> &'static Regex {
    static DOI_REGEX: OnceLock<Regex> = OnceLock::new();
    DOI_REGEX.get_or_init(doi_regex)
}

pub fn find_doi(text: &str) -> Option<String> {
    find_dois(text).into_iter().next()
}

pub fn find_dois(text: &str) -> Vec<String> {
    doi_regex_ref()
        .captures_iter(text)
        .filter_map(|captures| captures.get(1).map(|m| m.as_str()))
        .filter_map(normalize_doi)
        .collect()
}

pub fn normalize_doi(value: &str) -> Option<String> {
    // DOI names are case-insensitive. Lowercase at the shared boundary so
    // every provider and extractor writes the same canonical representation.
    let lowercase = value.trim().to_ascii_lowercase();
    let trimmed = lowercase.as_str();
    let without_prefix = trimmed
        .strip_prefix("https://doi.org/")
        .or_else(|| trimmed.strip_prefix("http://doi.org/"))
        .or_else(|| trimmed.strip_prefix("https://dx.doi.org/"))
        .or_else(|| trimmed.strip_prefix("http://dx.doi.org/"))
        .unwrap_or(trimmed);
    let without_doi = if without_prefix
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("doi:"))
    {
        without_prefix.get(4..).unwrap_or("").trim()
    } else {
        without_prefix
    };
    let canonical = strip_doi_wrappers(without_doi.trim());

    if canonical.is_empty()
        || !canonical.starts_with("10.")
        || !canonical.contains('/')
        || has_repeated_alnum_suffix(canonical)
    {
        return None;
    }

    Some(canonical.to_string())
}

fn has_repeated_alnum_suffix(canonical: &str) -> bool {
    let Some((_, suffix)) = canonical.split_once('/') else {
        return false;
    };

    let mut chars = suffix
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase());

    let Some(first) = chars.next() else {
        return false;
    };

    let mut count = 1;
    for ch in chars {
        count += 1;
        if ch != first {
            return false;
        }
    }

    count >= 6
}

fn strip_doi_wrappers(value: &str) -> &str {
    let mut trimmed = value.trim();

    loop {
        trimmed = trimmed.trim_end_matches(|c: char| {
            matches!(
                c,
                '.' | ',' | ';' | ':' | '"' | '\'' | ')' | ']' | '}' | '>'
            )
        });

        let updated = trimmed
            .strip_prefix('(')
            .and_then(|v| v.strip_suffix(')'))
            .or_else(|| trimmed.strip_prefix('[').and_then(|v| v.strip_suffix(']')))
            .or_else(|| trimmed.strip_prefix('{').and_then(|v| v.strip_suffix('}')))
            .or_else(|| trimmed.strip_prefix('<').and_then(|v| v.strip_suffix('>')))
            .or_else(|| trimmed.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
            .or_else(|| {
                trimmed
                    .strip_prefix('\'')
                    .and_then(|v| v.strip_suffix('\''))
            });

        match updated {
            Some(next) => trimmed = next.trim(),
            None => break,
        }
    }

    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_doi_matches_standard_form() {
        assert_eq!(
            find_doi("See 10.1000/xyz123 for details."),
            Some("10.1000/xyz123".into())
        );
    }

    #[test]
    fn test_find_doi_normalizes_prefixed_forms() {
        assert_eq!(
            find_doi("doi:10.1000/xyz123"),
            Some("10.1000/xyz123".into())
        );
        assert_eq!(
            find_doi("https://doi.org/10.1000/xyz123"),
            Some("10.1000/xyz123".into())
        );
        assert_eq!(
            normalize_doi("HTTPS://DOI.ORG/10.1109/ICSME.2016.33"),
            Some("10.1109/icsme.2016.33".into())
        );
    }

    #[test]
    fn test_find_doi_ignores_surrounding_punctuation() {
        assert_eq!(find_doi("(10.1000/xyz123)."), Some("10.1000/xyz123".into()));
    }

    #[test]
    fn test_find_doi_prefers_first_valid_candidate() {
        assert_eq!(
            find_doi("bad 10.1 nope and then 10.1000/xyz123"),
            Some("10.1000/xyz123".into())
        );
    }

    #[test]
    fn test_find_dois_returns_all_valid_candidates() {
        assert_eq!(
            find_dois("See 10.1000/xyz123 and 10.1145/3544548.3581349"),
            vec![
                "10.1000/xyz123".to_string(),
                "10.1145/3544548.3581349".to_string()
            ]
        );
    }

    #[test]
    fn test_find_doi_rejects_repeated_alnum_suffix_placeholders() {
        assert_eq!(find_doi("https://doi.org/10.1145/nnnnnnn.nnnnnnn"), None);
        assert_eq!(find_doi("doi:10.1145/0000000.0000000"), None);
    }

    #[test]
    fn test_find_doi_allows_mixed_alnum_suffixes() {
        assert_eq!(
            find_doi("https://doi.org/10.1000/xyz123"),
            Some("10.1000/xyz123".into())
        );
        assert_eq!(
            find_doi("https://doi.org/10.1145/3544548.3581349"),
            Some("10.1145/3544548.3581349".into())
        );
    }
}
