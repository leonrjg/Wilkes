//! Reading a syndication feed for the items it lists, and finding the feed a
//! web page advertises.

use super::identity::publication_date;
use super::SourceEntry;

/// Why a feed document gave no items.
#[derive(Debug, thiserror::Error)]
pub enum SyndicationError {
    #[error("not an RSS, Atom or JSON feed: {0}")]
    NotAFeed(String),
}

/// What a feed document says.
#[derive(Clone, Debug)]
pub struct FeedDocument {
    /// The feed's own name for itself.
    pub title: Option<String>,
    pub entries: Vec<SourceEntry>,
    /// Entries left out because they name no link to fetch.
    pub without_link: usize,
}

/// The items a feed document lists: link, title, author, date and an inbox
/// picture. The feed's own text is never read; every item is fetched from its
/// link.
///
/// An entry without a link cannot be fetched, so it is not an item. Each one
/// left out is logged, and counted in [`FeedDocument::without_link`] so the
/// caller can say so.
pub fn read_feed(bytes: &[u8]) -> Result<FeedDocument, SyndicationError> {
    let feed = feed_rs::parser::parse(bytes)
        .map_err(|error| SyndicationError::NotAFeed(error.to_string()))?;
    let title = feed
        .title
        .as_ref()
        .map(|title| collapse(&title.content))
        .filter(|title| !title.is_empty());
    let mut entries = Vec::with_capacity(feed.entries.len());
    let mut without_link = 0usize;
    for entry in feed.entries {
        let link = entry
            .links
            .iter()
            .filter(|link| link.target.is_none())
            .find(|link| matches!(link.rel.as_deref(), None | Some("alternate")))
            .map(|link| link.href.trim().to_string())
            .filter(|href| !href.is_empty());
        let Some(link) = link else {
            tracing::warn!(
                entry = %entry.id,
                "feed entry has no link to fetch it from; left out"
            );
            without_link += 1;
            continue;
        };
        let image_url = entry
            .media
            .iter()
            .flat_map(|media| media.content.iter())
            .find(|content| {
                content
                    .content_type
                    .as_ref()
                    .is_some_and(|kind| kind.ty().as_str().eq_ignore_ascii_case("image"))
            })
            .and_then(|content| content.url.as_ref())
            .map(|url| url.to_string());
        entries.push(SourceEntry {
            link,
            title: entry
                .title
                .map(|title| collapse(&title.content))
                .filter(|title| !title.is_empty()),
            author: entry
                .authors
                .iter()
                .filter_map(|person| person.name.as_deref())
                .map(collapse)
                .find(|name| !name.is_empty()),
            published: publication_date(entry.published, entry.updated),
            image_url,
        });
    }
    Ok(FeedDocument {
        title,
        entries,
        without_link,
    })
}

/// The most feeds a page is taken to offer: each is fetched to be sure.
pub const MAX_OFFERED_FEEDS: usize = 8;

/// The feeds a web page may offer, resolved against its address, each once
/// and in order: first those its head advertises with `<link rel="alternate">`,
/// then the links in its body that look like feeds — a feed's media type, an
/// address whose path is a feed's (`/feed`, `/rss`, `/atom/entries/`,
/// `index.xml`), or a link that calls itself one ("Atom feed", "RSS"). A page
/// often advertises only its main feed in its head and links the others, so
/// both are read. They are only candidates: each is fetched and read before
/// it is offered to anyone.
pub fn offered_feeds(page_source: &str, page_url: &str) -> Vec<String> {
    let Ok(base) = url::Url::parse(page_url) else {
        return Vec::new();
    };
    let document = dom_query::Document::from(page_source);
    let mut found: Vec<String> = Vec::new();
    let mut take = |href: &str| {
        let Ok(url) = base.join(href.trim()) else {
            return;
        };
        if !matches!(url.scheme(), "http" | "https") || url == base {
            return;
        }
        let url = url.to_string();
        if !found.contains(&url) && found.len() < MAX_OFFERED_FEEDS {
            found.push(url);
        }
    };
    for link in document.select("link[rel~=\"alternate\"][href]").iter() {
        if link.attr("type").is_some_and(|kind| is_feed_type(&kind)) {
            if let Some(href) = link.attr("href") {
                take(&href);
            }
        }
    }
    for anchor in document.select("body a[href]").iter() {
        let Some(href) = anchor.attr("href") else {
            continue;
        };
        let typed = anchor.attr("type").is_some_and(|kind| is_feed_type(&kind));
        let named = [
            anchor.attr("title").map(|t| t.to_string()),
            Some(anchor.text().to_string()),
        ]
        .into_iter()
        .flatten()
        .any(|label| names_a_feed(&label));
        let addressed = base
            .join(href.trim())
            .is_ok_and(|url| path_is_a_feeds(url.path()));
        if typed || named || addressed {
            take(&href);
        }
    }
    found
}

fn is_feed_type(kind: &str) -> bool {
    let kind = kind.to_ascii_lowercase();
    kind.contains("rss+xml")
        || kind.contains("atom+xml")
        || kind.contains("feed+json")
        || kind == "application/json"
}

/// A path one of whose segments is a feed's name (`feed`, `rss`, `atom`), or
/// that ends in a feed file's extension.
fn path_is_a_feeds(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    segments
        .iter()
        .any(|segment| matches!(*segment, "feed" | "feeds" | "rss" | "atom"))
        || segments.last().is_some_and(|last| {
            [".xml", ".rss", ".atom", ".rdf"]
                .iter()
                .any(|extension| last.ends_with(extension))
        })
}

/// A link's words that call it a feed: "RSS", "Atom", "feed" as words.
fn names_a_feed(label: &str) -> bool {
    label
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| matches!(word.to_ascii_lowercase().as_str(), "rss" | "atom" | "feed"))
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_atom_feed_gives_links_titles_authors_and_dates() {
        let feed = br#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Weblog</title><id>https://example.org/</id><updated>2026-11-09T10:00:00+00:00</updated>
  <entry>
    <title>Locating a beep</title>
    <link href="https://example.org/2026/Nov/9/beep/" rel="alternate"/>
    <id>https://example.org/2026/Nov/9/beep/</id>
    <updated>2026-11-09T10:00:00+00:00</updated>
    <author><name>Writer</name></author>
    <content type="html">&lt;p&gt;Full text is never read.&lt;/p&gt;</content>
  </entry>
  <entry>
    <title>No link</title><id>tag:example.org,2026:2</id><updated>2026-11-08T10:00:00+00:00</updated>
  </entry>
</feed>"#;
        let document = read_feed(feed).unwrap();
        assert_eq!(document.title.as_deref(), Some("Weblog"));
        assert_eq!(document.without_link, 1);
        let entries = document.entries;
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.link, "https://example.org/2026/Nov/9/beep/");
        assert_eq!(entry.title.as_deref(), Some("Locating a beep"));
        assert_eq!(entry.author.as_deref(), Some("Writer"));
        assert_eq!(
            entry.published.map(|date| date.to_rfc3339()),
            Some("2026-11-09T10:00:00+00:00".to_string())
        );
    }

    #[test]
    fn an_rss_feed_gives_its_cover_enclosure_and_a_placeholder_date_is_none() {
        let feed = br#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Blog</title><link>https://example.org</link><description>d</description>
  <item>
    <title>Post</title><link>https://example.substack.com/p/post</link>
    <pubDate>Sat, 31 Oct 2026 15:17:33 GMT</pubDate>
    <enclosure url="https://substackcdn.com/image/cover.jpeg" length="0" type="image/jpeg"/>
  </item>
  <item>
    <title>FAQ</title><link>https://example.org/faq/</link>
    <pubDate>Mon, 01 Jan 0001 00:00:00 +0000</pubDate>
  </item>
  <item><title>Undated</title><link>https://example.org/undated/</link></item>
</channel></rss>"#;
        let entries = read_feed(feed).unwrap().entries;
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0].image_url.as_deref(),
            Some("https://substackcdn.com/image/cover.jpeg")
        );
        assert!(entries[0].published.is_some());
        assert_eq!(entries[1].published, None);
        assert_eq!(entries[2].published, None);
    }

    #[test]
    fn a_page_that_is_not_a_feed_is_an_error() {
        assert!(read_feed(b"<html><body>Not a feed</body></html>").is_err());
    }

    /// Simon Willison's entries page, as served: its head advertises only
    /// the main feed, and its body links the feed of entries alone.
    #[test]
    fn a_page_offers_the_feeds_its_head_advertises_and_its_body_links() {
        let page = r#"<html><head>
            <link rel="stylesheet" href="/style.css">
            <link rel="alternate" type="application/atom+xml" title="Atom" href="/atom/everything/">
        </head><body>
            <h2>Entries <a style="float: right" href="/atom/entries/" title="Atom feed"><svg></svg></a></h2>
            <a href="/2026/Nov/9/mysterious-beeps/">Locating a beep</a>
            <a href="/feedback/">Feedback</a>
            <a href="https://example.org/comments/feed/">Comments</a>
            <a href="/index.xml">Subscribe</a>
            <a href="/atom/everything/">My Atom feed</a>
            <a href="/about/">About this site's RSS</a>
        </body></html>"#;
        assert_eq!(
            offered_feeds(page, "https://simonwillison.net/entries/"),
            vec![
                "https://simonwillison.net/atom/everything/",
                "https://simonwillison.net/atom/entries/",
                "https://example.org/comments/feed/",
                "https://simonwillison.net/index.xml",
                // Said to be one; fetching it will say it is not.
                "https://simonwillison.net/about/",
            ]
        );
        assert!(offered_feeds("<html></html>", "https://example.org/").is_empty());
    }
}
