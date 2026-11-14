//! A post's page, reduced to its article.
//!
//! The viewer strips scripts, not menus or comment sections, and search would
//! take them in; so a page is reduced to its article before it is written.
//! `dom_smoothie` (Readability's algorithm) finds the article. On a short post
//! it can take the page's sidebar along with it, so what the site puts on
//! every page is then taken out: a [`SiteTemplate`], read from another of the
//! site's posts, says what that is — nothing about any site is written here.
//! What is left is cleaned so the file holds only what the viewer will show:
//! everything its sanitizer would drop is dropped here, links are absolute,
//! and every picture names the one address it will be fetched from.

use std::collections::HashSet;

/// The widest picture a `srcset` is read for. Wider is a retina rendition of
/// a column of text, at four times the bytes.
const MAX_PICTURE_WIDTH: u32 = 1600;

/// The fewest characters an article has (Readability's
/// `readable_min_content_length`).
const MIN_ARTICLE_CHARS: usize = 140;

/// Why a page gave no article.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ArticleError {
    #[error("the page has no article Wilkes can find")]
    NoArticle,
    #[error("the page could not be read: {0}")]
    Unreadable(String),
}

/// What a page's article is.
#[derive(Clone, Debug, PartialEq)]
pub struct Article {
    pub title: Option<String>,
    pub byline: Option<String>,
    /// As the page states it, unparsed.
    pub published: Option<String>,
    /// The article's markup, cleaned.
    pub html: String,
    /// Every picture's address, absolute, in order, each once.
    pub pictures: Vec<String>,
    /// This page's text: the template another post of the same site is read
    /// against.
    pub site_template: SiteTemplate,
}

/// The text of one of a site's pages, for telling what the site puts on every
/// page from what a post says.
///
/// A stretch of two or more runs in a row that the other page holds too is
/// the site's — the list of recent articles, the newsletter box, the footer —
/// and is removed. A single shared run between the post's own text stays: a
/// heading two posts both have ("Notes", "Conclusion"), a command both quote.
/// Nothing depends on how well the other page's own article was found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SiteTemplate {
    runs: HashSet<String>,
}

impl SiteTemplate {
    /// The template of a page of the site: all of its text, as runs.
    pub fn from_page(page_source: &str) -> Self {
        let whole = dom_query::Document::from(page_source);
        for name in crate::extract::html::STRIPPED {
            whole.select(name).remove();
        }
        Self {
            runs: runs(&whole).into_iter().map(|(_, text)| text).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// The article's markup without the text the site puts on every page.
    fn subtract(&self, article_html: &str) -> String {
        let document = dom_query::Document::from(article_html);
        let found = runs(&document);
        let shared: Vec<bool> = found
            .iter()
            .map(|(_, text)| self.runs.contains(text))
            .collect();
        for (index, (nodes, _)) in found.iter().enumerate() {
            let in_stretch = shared[index]
                && ((index > 0 && shared[index - 1])
                    || shared.get(index + 1).copied().unwrap_or(false));
            if in_stretch {
                for node in nodes {
                    node.remove_from_parent();
                }
            }
        }
        document.select("body").inner_html().to_string()
    }
}

/// A document's runs of text: within each block, the text between the
/// blocks and line breaks inside it, whitespace collapsed, with the nodes it
/// is made of. A paragraph is one run; so is a button-like link at the end of
/// a box, or one paragraph of an essay set as `<br>`-separated text.
fn runs<'a>(document: &'a dom_query::Document) -> Vec<(Vec<dom_query::NodeRef<'a>>, String)> {
    let mut found = Vec::new();
    if let Some(body) = document.select("body").nodes().first() {
        collect_runs(body, &mut found);
    }
    found
}

fn collect_runs<'a>(
    block: &dom_query::NodeRef<'a>,
    found: &mut Vec<(Vec<dom_query::NodeRef<'a>>, String)>,
) {
    let mut run: Vec<dom_query::NodeRef<'a>> = Vec::new();
    let close = |run: &mut Vec<dom_query::NodeRef<'a>>,
                 found: &mut Vec<(Vec<dom_query::NodeRef<'a>>, String)>| {
        let text = collapse(
            &run.iter()
                .map(|node| node.text().to_string())
                .collect::<String>(),
        );
        let nodes = std::mem::take(run);
        if !text.is_empty() {
            found.push((nodes, text));
        }
    };
    for child in block.children() {
        let name = child
            .is_element()
            .then(|| child.node_name())
            .flatten()
            .map(|name| name.to_ascii_lowercase());
        match name.as_deref() {
            Some("br") => close(&mut run, found),
            Some(name) if crate::extract::html::BLOCK.contains(&name) => {
                close(&mut run, found);
                collect_runs(&child, found);
            }
            _ => run.push(child),
        }
    }
    close(&mut run, found);
}

/// The article of a page fetched from `page_url`, without what `template`
/// says the site puts on every page. No template — the site has no other post
/// to compare with — leaves the article as Readability found it.
pub fn extract(
    page_source: &str,
    page_url: &str,
    template: Option<&SiteTemplate>,
) -> Result<Article, ArticleError> {
    let page = dom_query::Document::from(page_source);
    // No `is_probably_readable` first: it counts `<p>` and `<pre>` alone, and
    // so turns away every page set in a table with `<br>` between paragraphs —
    // all of Paul Graham's essays, measured. The extraction itself is the
    // test: it finds the article on those pages, and says when there is none.
    let mut readability = dom_smoothie::Readability::with_document(page, Some(page_url), None)
        .map_err(|error| ArticleError::Unreadable(error.to_string()))?;
    let article = readability.parse().map_err(|error| match error {
        dom_smoothie::ReadabilityError::GrabFailed => ArticleError::NoArticle,
        other => ArticleError::Unreadable(other.to_string()),
    })?;
    let base =
        url::Url::parse(page_url).map_err(|error| ArticleError::Unreadable(error.to_string()))?;
    let site_template = SiteTemplate::from_page(page_source);
    let content = match template {
        Some(template) => template.subtract(&article.content),
        None => article.content.to_string(),
    };
    let (html, pictures, text) = clean(&content, &base);
    // Readability's own floor for a passage that counts as content: anything
    // shorter is a page's furniture that happened to score highest.
    if collapse(&text).chars().count() < MIN_ARTICLE_CHARS {
        return Err(ArticleError::NoArticle);
    }
    let text = |value: String| Some(collapse(&value)).filter(|value| !value.is_empty());
    Ok(Article {
        title: text(article.title),
        byline: article.byline.and_then(text),
        published: article.published_time.and_then(text),
        html,
        pictures,
        site_template,
    })
}

/// Drop what the viewer would drop, make every link absolute, and settle each picture on one address.
fn clean(content: &str, base: &url::Url) -> (String, Vec<String>, String) {
    let document = dom_query::Document::from(content);
    for name in crate::extract::html::STRIPPED {
        document.select(name).remove();
    }
    // A `<picture>`'s sources are alternatives to its `<img>`, which is kept.
    document.select("source").remove();

    for link in document.select("a[href]").iter() {
        if let Some(href) = link.attr("href") {
            match base.join(href.trim()) {
                Ok(absolute) => link.set_attr("href", absolute.as_str()),
                Err(_) => link.remove_attr("href"),
            }
        }
    }

    let mut pictures: Vec<String> = Vec::new();
    for image in document.select("img").iter() {
        let chosen = image
            .attr("srcset")
            .and_then(|srcset| widest_candidate(&srcset))
            .or_else(|| image.attr("src").map(|src| src.trim().to_string()))
            .filter(|src| !src.is_empty());
        image.remove_attrs(&["srcset", "sizes", "loading", "decoding", "style"]);
        let Some(chosen) = chosen else {
            image.remove();
            continue;
        };
        if chosen.starts_with("data:") {
            image.set_attr("src", &chosen);
            continue;
        }
        match base.join(&chosen) {
            Ok(absolute) if matches!(absolute.scheme(), "http" | "https") => {
                let absolute = absolute.to_string();
                image.set_attr("src", &absolute);
                if !pictures.contains(&absolute) {
                    pictures.push(absolute);
                }
            }
            _ => image.remove(),
        }
    }
    for element in document.select("[style]").iter() {
        element.remove_attr("style");
    }
    let body = document.select("body");
    (
        body.inner_html().to_string(),
        pictures,
        body.text().to_string(),
    )
}

/// The widest `srcset` candidate no wider than [`MAX_PICTURE_WIDTH`], or the
/// narrowest when every one is wider. Density descriptors (`2x`) say nothing
/// about width, so a `srcset` of them is left to `src`.
fn widest_candidate(srcset: &str) -> Option<String> {
    let mut candidates: Vec<(u32, String)> = Vec::new();
    for candidate in split_srcset(srcset) {
        let mut parts = candidate.split_whitespace();
        let (Some(url), Some(descriptor)) = (parts.next(), parts.next()) else {
            continue;
        };
        if let Some(width) = descriptor
            .strip_suffix('w')
            .and_then(|width| width.parse::<u32>().ok())
        {
            candidates.push((width, url.to_string()));
        }
    }
    let within = candidates
        .iter()
        .filter(|(width, _)| *width <= MAX_PICTURE_WIDTH)
        .max_by_key(|(width, _)| *width);
    within
        .or_else(|| candidates.iter().min_by_key(|(width, _)| *width))
        .map(|(_, url)| url.clone())
}

/// A `srcset` split into its candidates. A comma separates candidates only
/// where it follows a descriptor: addresses (Substack's among them) carry
/// commas of their own.
fn split_srcset(srcset: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut current = String::new();
    let mut seen_space = false;
    for c in srcset.chars() {
        if c == ',' && seen_space {
            candidates.push(std::mem::take(&mut current).trim().to_string());
            seen_space = false;
            continue;
        }
        if c.is_whitespace() && !current.trim().is_empty() {
            seen_space = true;
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        candidates.push(current.trim().to_string());
    }
    candidates
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(body: &str) -> String {
        format!(
            "<!doctype html><html><head><title>A post - Blog</title>\
             <meta property=\"article:published_time\" content=\"2026-10-31T15:17:33Z\">\
             <script>tracker()</script></head><body>\
             <nav><a href=\"/\">Home</a> <a href=\"/archive\">Archive</a> <a href=\"/about\">About</a></nav>\
             <main><article><h1>A post</h1>{body}</article></main>\
             <footer>Copyright footer. <a href=\"/privacy\">Privacy</a></footer>\
             <div id=\"comments\"><h2>Comments</h2><p>First!</p></div></body></html>"
        )
    }

    fn paragraphs() -> String {
        (0..8)
            .map(|i| {
                format!(
                    "<p>Paragraph {i} of the post says something worth reading, at the length \
                     a real paragraph has, with a <a href=\"/linked/{i}\">relative link</a> \
                     inside it and enough words that a reader would call it prose.</p>"
                )
            })
            .collect()
    }

    #[test]
    fn the_article_is_kept_and_the_page_around_it_is_not() {
        let body = format!(
            "{}<figure><img src=\"/img/a.png\" srcset=\"/img/a-400.png 400w, /img/a-1456.png 1456w, /img/a-2912.png 2912w\" style=\"width:100%\"><figcaption>A figure</figcaption></figure><script>evil()</script><form><button>Subscribe</button></form>",
            paragraphs()
        );
        let article = extract(&page(&body), "https://example.org/2026/post/", None).unwrap();
        assert!(article.html.contains("Paragraph 7"));
        assert!(article.html.contains("A figure"));
        for absent in [
            "Archive",
            "Copyright footer",
            "First!",
            "evil()",
            "Subscribe",
            "srcset",
            "style=",
        ] {
            assert!(
                !article.html.contains(absent),
                "{absent} kept: {}",
                article.html
            );
        }
        assert!(article
            .html
            .contains("href=\"https://example.org/linked/3\""));
        assert_eq!(
            article.pictures,
            vec!["https://example.org/img/a-1456.png".to_string()]
        );
        assert!(article
            .html
            .contains("src=\"https://example.org/img/a-1456.png\""));
        assert!(article.published.is_some());
    }

    /// Paul Graham's essays: a table layout, one `<font>` of text with `<br>`
    /// between paragraphs, and no `<p>` anywhere.
    #[test]
    fn an_essay_set_in_a_table_with_line_breaks_is_an_article() {
        let essay = (0..12)
            .map(|i| format!("Paragraph {i} of an essay, set the old way, runs on for long enough that a reader would take it for prose and keep reading it.<br><br>"))
            .collect::<String>();
        let source = format!(
            r#"<html><head><title>An Essay</title></head><body><table><tr><td>
<map><area href="index.html"><area href="articles.html"></map>
<img src="https://s.example.com/nav.gif" usemap="nav"></td><td width="26"></td>
<td><font size="2" face="verdana">October 2024<br><br>{essay}</font></td></tr></table></body></html>"#
        );
        let article = extract(&source, "https://paulgraham.com/essay.html", None).unwrap();
        let masked = crate::extract::html::mask(&article.html);
        let text = crate::extract::html::visible_projection(&masked).into_text();
        assert!(text.contains("Paragraph 11"), "{text}");
        assert!(!text.contains("articles.html"));
        // Each paragraph still ends a line.
        assert!(text.contains("keep reading it.\nParagraph 1 "), "{text}");
    }

    #[test]
    fn a_page_with_no_article_says_so() {
        let error = extract(
            "<html><body><nav><a href=\"/\">Home</a></nav><p>Short.</p></body></html>",
            "https://example.org/",
            None,
        )
        .unwrap_err();
        assert_eq!(error, ArticleError::NoArticle);
    }

    #[test]
    fn srcset_candidates_split_at_descriptors_not_at_commas_in_addresses() {
        let srcset = "https://substackcdn.com/image/fetch/w_424,c_limit,f_webp/x.png 424w, https://substackcdn.com/image/fetch/w_1456,c_limit,f_webp/x.png 1456w, https://substackcdn.com/image/fetch/w_2000,c_limit/x.png 2000w";
        assert_eq!(
            widest_candidate(srcset).as_deref(),
            Some("https://substackcdn.com/image/fetch/w_1456,c_limit,f_webp/x.png")
        );
        assert_eq!(
            widest_candidate("big.png 2400w, bigger.png 3200w").as_deref(),
            Some("big.png")
        );
        assert_eq!(widest_candidate("a.png 1x, a@2x.png 2x"), None);
    }

    /// A short post laid out as a link blog lays one out: the post beside a
    /// sidebar of recent articles and a sponsor box. Readability takes the
    /// sidebar with the post; another post of the site says it is the site's.
    fn link_blog_page(slug: &str, post: &str) -> String {
        format!(
            r#"<html><head><title>{slug}</title></head><body>
<div id="top"><h1><a href="/">A Weblog</a></h1><a href="/newsletters/">Newsletter</a></div>
<div id="wrapper"><div id="primary"><div class="entry">
<p class="eyebrow">11th November 2026 - Link Blog</p>
<div>{post}</div>
<div class="entryFooter">Posted <a href="/2026/Nov/11/">11th November 2026</a> at 6:42 pm</div>
</div>
<div class="recent"><h2>Recent articles</h2><ul>
<li><a href="/a/">Locating the source of a mysterious beep</a> - 9th November 2026</li>
<li><a href="/b/">Hiding long descriptions behind details</a> - 9th November 2026</li></ul></div>
</div>
<div id="secondary"><div class="metabox">
<p>This is a <strong>link post</strong>, posted on <a href="/2026/Nov/11/">11th November 2026</a>.</p>
<a class="tag" href="/tags/{slug}/">{slug} <span>83</span></a>
<section><h3>Monthly briefing</h3><p>Sponsor me for <strong>$10/month</strong> and get a curated email digest of the month's most important developments.</p>
<p>Pay me to send you less!</p><a href="/sponsors/">Sponsor &amp; subscribe</a></section>
</div></div></div>
<div id="ft"><ul><li><a href="/about/">Colophon</a></li><li>&copy;</li></ul></div>
</body></html>"#
        )
    }

    #[test]
    fn what_the_site_puts_on_every_page_is_taken_out_of_a_short_post() {
        let post = "<p><strong><a href=\"https://tools.example.org/\">Browse the tools by tag</a></strong>. I wanted a better way to browse my collection of small tools for a while, so the code of each one is now read for the libraries and APIs it uses.</p><h3>Notes</h3><p>The homepage now features tag clouds for browsing by each of those categories of tag.</p>";
        let other = "<p>A video went out yesterday, with numbers in it that had not been public before, and this post says which of them matter and why the rest are noise.</p><h3>Notes</h3><p>Another paragraph of a different post, long enough to be read as its own article by anyone.</p>";
        let url = "https://example.org/2026/Nov/11/tools/";
        let page = link_blog_page("tools", post);

        // Without the site's template the sidebar comes along: the case this
        // is for.
        let plain = extract(&page, url, None).unwrap();
        assert!(plain.html.contains("Monthly briefing"), "{}", plain.html);

        let template = SiteTemplate::from_page(&link_blog_page("video", other));
        let article = extract(&page, url, Some(&template)).unwrap();
        for absent in [
            "Recent articles",
            "mysterious beep",
            "Monthly briefing",
            "Sponsor me",
            "Pay me",
            "Sponsor &amp; subscribe",
            "This is a",
        ] {
            assert!(
                !article.html.contains(absent),
                "{absent} kept: {}",
                article.html
            );
        }
        assert!(article.html.contains("Browse the tools by tag"));
        assert!(article.html.contains("tag clouds"));
        // A heading both posts have inside their articles is the post's.
        assert!(article.html.contains("<h3>Notes</h3>"), "{}", article.html);
        // The page's own template is ready for the next post.
        assert!(!article.site_template.is_empty());
    }

    /// Nothing a template holds touches a page it does not come from.
    #[test]
    fn another_sites_template_changes_nothing() {
        let template = SiteTemplate::from_page(&link_blog_page("video", "<p>A video went out yesterday, with numbers in it that had not been public before, and this post says which of them matter.</p>"));
        let page = page(&paragraphs());
        assert_eq!(
            extract(&page, "https://example.net/post/", Some(&template))
                .unwrap()
                .html,
            extract(&page, "https://example.net/post/", None)
                .unwrap()
                .html
        );
    }
}
