//! A post's page, reduced to its article.
//!
//! The viewer strips scripts, not menus or comment sections, and search would
//! take them in; so a page is reduced to its article before it is written.
//! `dom_smoothie` (Readability's algorithm) finds the article. What is left is
//! cleaned so the file holds only what the viewer will show: everything its
//! sanitizer would drop is dropped here, links are absolute, and every picture
//! names the one address it will be fetched from.

use super::identity::platform;

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
}

/// The article of a page fetched from `page_url`.
pub fn extract(page_source: &str, page_url: &str) -> Result<Article, ArticleError> {
    // A platform's boxes are found by their classes, which extraction does
    // not keep: they are removed from the page before it is read.
    let page = dom_query::Document::from(page_source);
    if let Some(platform) = platform(page_source) {
        for selector in platform.remove {
            page.select(selector).remove();
        }
    }
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
    let base = url::Url::parse(page_url).map_err(|error| ArticleError::Unreadable(error.to_string()))?;
    let (html, pictures) = clean(&article.content, &base);
    // Readability's own floor for a passage that counts as content: anything
    // shorter is a page's furniture that happened to score highest.
    if collapse(&article.text_content).chars().count() < MIN_ARTICLE_CHARS {
        return Err(ArticleError::NoArticle);
    }
    let text = |value: String| Some(collapse(&value)).filter(|value| !value.is_empty());
    Ok(Article {
        title: text(article.title),
        byline: article.byline.and_then(text),
        published: article.published_time.and_then(text),
        html,
        pictures,
    })
}

/// Drop what the viewer would drop, make every link absolute, and settle each picture on one address.
fn clean(content: &str, base: &url::Url) -> (String, Vec<String>) {
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
    (document.select("body").inner_html().to_string(), pictures)
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
        let article = extract(&page(&body), "https://example.org/2026/post/").unwrap();
        assert!(article.html.contains("Paragraph 7"));
        assert!(article.html.contains("A figure"));
        for absent in ["Archive", "Copyright footer", "First!", "evil()", "Subscribe", "srcset", "style="] {
            assert!(!article.html.contains(absent), "{absent} kept: {}", article.html);
        }
        assert!(article.html.contains("href=\"https://example.org/linked/3\""));
        assert_eq!(article.pictures, vec!["https://example.org/img/a-1456.png".to_string()]);
        assert!(article.html.contains("src=\"https://example.org/img/a-1456.png\""));
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
        let article = extract(&source, "https://paulgraham.com/essay.html").unwrap();
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

    #[test]
    fn a_platforms_boxes_inside_the_article_are_removed() {
        let body = format!(
            "{}<div class=\"subscription-widget-wrap\"><p>Subscribe to keep reading this newsletter every week</p></div>",
            paragraphs()
        );
        let source = page(&body).replace("<head>", "<head><link rel=\"preconnect\" href=\"https://substackcdn.com\">");
        let article = extract(&source, "https://example.substack.com/p/post").unwrap();
        assert!(!article.html.contains("keep reading"), "{}", article.html);
    }
}

