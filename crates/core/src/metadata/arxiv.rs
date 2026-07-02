use std::sync::OnceLock;

use regex::Regex;

fn explicit_arxiv_regex() -> Regex {
    Regex::new(
        r"(?ix)(?:\barxiv\s*:\s*|https?://(?:www\.)?arxiv\.org/(?:abs|pdf)/)(?P<id>(?:\d{4}\.\d{4,5})|(?:[a-z][a-z-]*(?:\.[a-z-]+)?/\d{7}))(?:v\d+)?(?:\.pdf)?",
    )
    .expect("valid arXiv explicit regex")
}

fn plain_modern_arxiv_regex() -> Regex {
    Regex::new(r"(?ix)(?:^|[^a-z0-9./])(?P<id>\d{4}\.\d{4,5})(?:v\d+)?(?:$|[^a-z0-9])")
        .expect("valid arXiv modern regex")
}

fn plain_legacy_arxiv_regex() -> Regex {
    Regex::new(
        r"(?ix)(?:^|[^a-z0-9./-])(?P<id>[a-z][a-z-]*(?:\.[a-z-]+)?/\d{7})(?:v\d+)?(?:$|[^a-z0-9])",
    )
    .expect("valid arXiv legacy regex")
}

fn version_suffix_regex() -> Regex {
    Regex::new(r"(?i)v\d+$").expect("valid arXiv version suffix regex")
}

fn explicit_arxiv_regex_ref() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(explicit_arxiv_regex)
}

fn plain_modern_arxiv_regex_ref() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(plain_modern_arxiv_regex)
}

fn plain_legacy_arxiv_regex_ref() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(plain_legacy_arxiv_regex)
}

fn version_suffix_regex_ref() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(version_suffix_regex)
}

pub fn find_arxiv_doi(text: &str) -> Option<String> {
    find_arxiv_id(text).map(|id| format!("10.48550/arXiv.{id}"))
}

pub fn find_arxiv_id(text: &str) -> Option<String> {
    find_with(explicit_arxiv_regex_ref(), text)
        .or_else(|| find_with(plain_legacy_arxiv_regex_ref(), text))
        .or_else(|| find_with(plain_modern_arxiv_regex_ref(), text))
}

fn find_with(regex: &Regex, text: &str) -> Option<String> {
    regex
        .captures_iter(text)
        .filter_map(|captures| captures.name("id").map(|m| m.as_str()))
        .find_map(normalize_arxiv_id)
}

fn normalize_arxiv_id(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches(|c: char| {
        matches!(
            c,
            '.' | ',' | ';' | ':' | '"' | '\'' | ')' | ']' | '}' | '>'
        )
    });
    let without_version = version_suffix_regex_ref().replace(trimmed, "");
    normalize_modern_id(&without_version).or_else(|| normalize_legacy_id(&without_version))
}

fn normalize_modern_id(value: &str) -> Option<String> {
    let (year_month, sequence) = value.split_once('.')?;
    if year_month.chars().count() != 4
        || !year_month.chars().all(|c| c.is_ascii_digit())
        || !matches!(sequence.chars().count(), 4 | 5)
        || !sequence.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }

    let digits: Vec<u32> = year_month
        .chars()
        .map(|c| c.to_digit(10))
        .collect::<Option<Vec<_>>>()?;
    let year = digits[0] * 10 + digits[1];
    let month = digits[2] * 10 + digits[3];

    if !(1..=12).contains(&month) || year < 7 || (year == 7 && month < 4) {
        return None;
    }

    Some(format!("{year_month}.{sequence}"))
}

fn normalize_legacy_id(value: &str) -> Option<String> {
    let (archive, number) = value.split_once('/')?;
    if archive.is_empty()
        || archive.split('.').any(|part| {
            part.is_empty() || !part.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
        })
        || number.chars().count() != 7
        || !number.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }

    let digits: Vec<u32> = number
        .chars()
        .map(|c| c.to_digit(10))
        .collect::<Option<Vec<_>>>()?;
    let month = digits[2] * 10 + digits[3];
    if !(1..=12).contains(&month) {
        return None;
    }

    Some(format!("{archive}/{number}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_modern_prefixed_arxiv_id_as_doi() {
        assert_eq!(
            find_arxiv_doi("arXiv:2506.12014"),
            Some("10.48550/arXiv.2506.12014".into())
        );
        assert_eq!(
            find_arxiv_doi("arXiv:0704.0001"),
            Some("10.48550/arXiv.0704.0001".into())
        );
    }

    #[test]
    fn strips_version_suffix_from_modern_ids() {
        assert_eq!(
            find_arxiv_doi("https://arxiv.org/abs/2401.04514v2"),
            Some("10.48550/arXiv.2401.04514".into())
        );
    }

    #[test]
    fn finds_pdf_url_form() {
        assert_eq!(
            find_arxiv_doi("https://arxiv.org/pdf/2506.12014.pdf"),
            Some("10.48550/arXiv.2506.12014".into())
        );
    }

    #[test]
    fn finds_plain_modern_id() {
        assert_eq!(
            find_arxiv_doi("Preprint 2506.12014v3 is relevant."),
            Some("10.48550/arXiv.2506.12014".into())
        );
    }

    #[test]
    fn finds_legacy_ids() {
        assert_eq!(
            find_arxiv_doi("arXiv:hep-th/9901001v3"),
            Some("10.48550/arXiv.hep-th/9901001".into())
        );
        assert_eq!(
            find_arxiv_doi("See math.GT/0309136 for details."),
            Some("10.48550/arXiv.math.GT/0309136".into())
        );
    }

    #[test]
    fn rejects_invalid_modern_ids() {
        assert_eq!(find_arxiv_doi("arXiv:0703.0001"), None);
        assert_eq!(find_arxiv_doi("arXiv:2500.12345"), None);
        assert_eq!(find_arxiv_doi("arXiv:9913.1234"), None);
        assert_eq!(find_arxiv_doi("1234.123"), None);
    }
}
