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

/// The first feed a web page advertises with `<link rel="alternate">`,
/// resolved against the page's address. `None` when it advertises none.
pub fn advertised_feed(page_source: &str, page_url: &str) -> Option<String> {
    let base = url::Url::parse(page_url).ok()?;
    let document = dom_query::Document::from(page_source);
    let selection = document.select("link[rel~=\"alternate\"][href]");
    selection.iter().find_map(|link| {
        let kind = link.attr("type")?.to_ascii_lowercase();
        let is_feed = kind.contains("rss+xml")
            || kind.contains("atom+xml")
            || kind.contains("feed+json")
            || kind == "application/json";
        if !is_feed {
            return None;
        }
        base.join(link.attr("href")?.trim())
            .ok()
            .map(|url| url.to_string())
    })
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

    #[test]
    fn a_page_advertises_its_feed() {
        let page = r#"<html><head>
            <link rel="stylesheet" href="/style.css">
            <link rel="alternate" type="application/atom+xml" title="Atom" href="/atom/everything/">
            <link rel="alternate" type="application/rss+xml" href="https://example.org/rss">
        </head><body></body></html>"#;
        assert_eq!(
            advertised_feed(page, "https://example.org/2026/post/").as_deref(),
            Some("https://example.org/atom/everything/")
        );
        assert_eq!(advertised_feed("<html></html>", "https://example.org/"), None);
    }
}
