//! What an item is, when it was published, and whether its page is the whole
//! post: decided from the source's entry or the fetched page alone.

use chrono::{DateTime, TimeZone, Utc};

/// Query parameters that say how a reader arrived, not what they arrived at.
/// Substack links carry five different `utm_*` values in one feed.
const TRACKING_PARAMETERS: &[&str] = &["fbclid", "gclid", "mc_cid", "mc_eid", "ref"];

/// An item's identity: its link, normalised so that sources disagreeing on
/// scheme, `www.`, tracking parameters or a trailing slash name it once.
///
/// - Scheme set to `https`; host lowercased, a leading `www.` removed.
/// - Fragment dropped. `utm_*`, `fbclid`, `gclid`, `mc_cid`, `mc_eid` and
///   `ref` dropped; the remaining parameters sorted by name.
/// - A trailing `/` removed, except for the bare host.
///
/// `None` for a link that is not an absolute http(s) URL: an item has to be
/// fetched from its link, and that one cannot be.
pub fn link_key(link: &str) -> Option<String> {
    let url = url::Url::parse(link.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let port = url
        .port()
        .filter(|port| *port != 80 && *port != 443)
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let mut path = url.path().to_string();
    while path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    if path == "/" {
        path.clear();
    }
    let mut parameters: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(name, _)| {
            !name.starts_with("utm_") && !TRACKING_PARAMETERS.contains(&name.as_ref())
        })
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    parameters.sort_by(|a, b| a.0.cmp(&b.0));
    let query = if parameters.is_empty() {
        String::new()
    } else {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in &parameters {
            serializer.append_pair(name, value);
        }
        format!("?{}", serializer.finish())
    };
    Some(format!("https://{host}{port}{path}{query}"))
}

/// The key of a provider's result that names no web address at all: the
/// provider and its own id, which is what the provider will be asked about.
pub fn provider_key(provider: &str, id: &str) -> String {
    format!("provider:{provider}/{id}")
}

/// The earliest date taken to be a real publication date. Lilian Weng's FAQ
/// entry claims the year 0001; a date that early is a placeholder, not a date.
fn earliest_real_date() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(1990, 1, 1, 0, 0, 0)
        .single()
        .expect("1990-01-01 is a valid date")
}

/// The source's publication date if it gave a real one: `published`, else
/// Atom's `updated`. Missing, or before 1990, is `None` — shown as the date the
/// item was first seen, never as a guessed publication date.
pub fn publication_date(
    published: Option<DateTime<Utc>>,
    updated: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    published
        .or(updated)
        .filter(|date| *date >= earliest_real_date())
}

/// The identifiers an item's link carries: a DOI from a `doi.org` link, and an
/// arXiv id from an `arxiv.org` one. Only those hosts are read, so a blog's
/// `/2024/1234.5678` is never taken for a paper.
pub fn link_identifiers(link: &str) -> (Option<String>, Option<String>) {
    let Ok(url) = url::Url::parse(link) else {
        return (None, None);
    };
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let doi = matches!(host.as_str(), "doi.org" | "dx.doi.org")
        .then(|| crate::metadata::doi::normalize_doi(link))
        .flatten();
    let arxiv = (host == "arxiv.org" || host.ends_with(".arxiv.org"))
        .then(|| crate::metadata::arxiv::find_arxiv_id(link))
        .flatten();
    (doi, arxiv)
}

/// What Wilkes knows about one publishing platform's pages, kept as data.
pub struct Platform {
    pub name: &'static str,
    /// Text in a page's source that says it was published here: a platform's
    /// own CDN appears on its custom domains too.
    pub recognised_by: &'static str,
    /// Text in the page's source that says the page shows only the start of
    /// the post. Not the visible markup: the word "paywall" appears on free
    /// Substack pages too.
    pub paywall: Option<&'static str>,
    /// Selectors for what extraction keeps and a reader does not want: share
    /// and subscribe boxes inside the article.
    pub remove: &'static [&'static str],
}

/// The platforms the survey's pages came from that need anything said about
/// them. A page from anywhere else is extracted as it is.
pub const PLATFORMS: &[Platform] = &[
    Platform {
        // Measured: on a short link-blog post the extraction took the sidebar
        // too (tag counts, the monthly briefing, the sponsor box).
        name: "Simon Willison's Weblog",
        recognised_by: "https://simonwillison.net/atom/everything/",
        paywall: None,
        remove: &["#secondary", "#sponsored-banner", ".recent-articles", "#ft"],
    },
    Platform {
        name: "Substack",
        recognised_by: "substackcdn.com",
        // In the post data embedded in the page: `"everyone"` on a free post.
        paywall: Some(r#""post":{"audience":"only_subscribers""#),
        remove: &[
            ".subscription-widget-wrap",
            ".subscription-widget-wrap-editor",
            ".subscribe-widget",
            ".button-wrapper",
            ".share-dialog",
            ".post-ufi",
            ".paywall",
            ".captioned-button-wrap",
            "[data-component-name=\"SubscribeWidgetToDOM\"]",
            "[data-component-name=\"ButtonCreateButton\"]",
        ],
    },
];

/// The platform a page was published on, if it is one Wilkes knows.
pub fn platform(page_source: &str) -> Option<&'static Platform> {
    PLATFORMS
        .iter()
        .find(|platform| page_source.contains(platform.recognised_by))
}

/// Whether the page shows only the start of a paid post.
pub fn is_preview_only(page_source: &str) -> bool {
    platform(page_source)
        .and_then(|platform| platform.paywall)
        .is_some_and(|marker| page_source.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_differing_in_scheme_host_tracking_and_slash_are_one_item() {
        let key = link_key("https://garymarcus.substack.com/p/report-from-the-vatican").unwrap();
        for variant in [
            "http://garymarcus.substack.com/p/report-from-the-vatican",
            "https://www.garymarcus.substack.com/p/report-from-the-vatican/",
            "https://GaryMarcus.Substack.com/p/report-from-the-vatican?utm_source=substack&utm_medium=email&utm_campaign=post",
            "https://garymarcus.substack.com/p/report-from-the-vatican#comments",
            "https://garymarcus.substack.com/p/report-from-the-vatican?ref=feed&fbclid=x",
        ] {
            assert_eq!(link_key(variant).as_deref(), Some(key.as_str()), "{variant}");
        }
        assert_eq!(
            key,
            "https://garymarcus.substack.com/p/report-from-the-vatican"
        );
    }

    #[test]
    fn meaningful_parameters_stay_and_are_ordered() {
        assert_eq!(
            link_key("https://scottaaronson.blog/?p=9001&utm_source=rss").as_deref(),
            Some("https://scottaaronson.blog?p=9001")
        );
        assert_eq!(
            link_key("https://example.org/a?b=2&a=1").as_deref(),
            Some("https://example.org/a?a=1&b=2")
        );
        assert_ne!(
            link_key("https://scottaaronson.blog/?p=1"),
            link_key("https://scottaaronson.blog/?p=2")
        );
        assert_eq!(
            link_key("http://www.paulgraham.com/").as_deref(),
            Some("https://paulgraham.com")
        );
        assert_eq!(
            link_key("http://localhost:8080/post/").as_deref(),
            Some("https://localhost:8080/post")
        );
    }

    #[test]
    fn a_link_that_cannot_be_fetched_has_no_key() {
        assert_eq!(link_key("/relative/path"), None);
        assert_eq!(link_key("mailto:someone@example.org"), None);
        assert_eq!(link_key(""), None);
    }

    #[test]
    fn a_placeholder_or_missing_date_is_no_date() {
        let year_one = Utc.with_ymd_and_hms(1, 1, 1, 0, 0, 0).single();
        assert_eq!(publication_date(year_one, None), None);
        assert_eq!(publication_date(None, None), None);
        let real = Utc.with_ymd_and_hms(2026, 10, 31, 15, 17, 33).single();
        assert_eq!(publication_date(None, real), real);
        assert_eq!(publication_date(real, year_one), real);
    }

    #[test]
    fn identifiers_are_read_only_from_their_own_hosts() {
        assert_eq!(
            link_identifiers("https://doi.org/10.1000/XYZ.1"),
            (Some("10.1000/xyz.1".to_string()), None)
        );
        assert_eq!(
            link_identifiers("https://arxiv.org/abs/2401.01234v2"),
            (None, Some("2401.01234".to_string()))
        );
        assert_eq!(
            link_identifiers("https://example.org/2024/2401.01234"),
            (None, None)
        );
    }

    #[test]
    fn a_paid_substack_post_is_a_preview_and_a_free_one_is_not() {
        let paid = r#"<script src="https://substackcdn.com/x.js"></script><script>window._preloads = {"post":{"audience":"only_subscribers","title":"x"}}</script><p>There is a paywall here</p>"#;
        let free = r#"<script src="https://substackcdn.com/x.js"></script><script>window._preloads = {"post":{"audience":"everyone","title":"x"}}</script><p>Subscribe; no paywall</p>"#;
        assert!(is_preview_only(paid));
        assert!(!is_preview_only(free));
        // The same marker on a page from no known platform is not a paywall.
        assert!(!is_preview_only(
            r#"{"post":{"audience":"only_subscribers"}}"#
        ));
    }
}
