//! One post as one self-contained HTML file.
//!
//! Nothing lies beside the file: every picture is inside it as a `data:` URI,
//! and where it came from is written in it twice — as `meta` elements in the
//! head, for a program, and as a header at the top of the article, for a
//! reader, because the viewer does not show the head.

use std::collections::HashMap;

use base64::Engine as _;
use chrono::{DateTime, Utc};

/// Where an imported post came from.
#[derive(Clone, Debug)]
pub struct Provenance {
    pub title: Option<String>,
    pub author: Option<String>,
    pub link: String,
    pub published: Option<DateTime<Utc>>,
    pub feed: String,
    pub fetched: DateTime<Utc>,
    pub item_id: String,
}

/// A fetched picture, ready to be written inside the file.
#[derive(Clone, Debug)]
pub struct Picture {
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

/// The picture formats the viewer draws, recognised by their own bytes rather
/// than by what the server said they were.
pub fn picture_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
        let head = head.trim_start_matches('\u{feff}').trim_start();
        (head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg")))
            .then_some("image/svg+xml")
    }
}

/// Put each fetched picture inside the article as a `data:` URI. A picture
/// with no entry in `pictures` is removed from the article — the caller only
/// leaves one out when it was told to go ahead without it.
pub fn embed_pictures(article_html: &str, pictures: &HashMap<String, Picture>) -> String {
    let document = dom_query::Document::from(article_html);
    for image in document.select("img[src]").iter() {
        let Some(src) = image.attr("src") else {
            continue;
        };
        if src.starts_with("data:") {
            continue;
        }
        match pictures.get(src.as_ref()) {
            Some(picture) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(&picture.bytes);
                image.set_attr("src", &format!("data:{};base64,{encoded}", picture.mime));
            }
            None => image.remove(),
        }
    }
    document.select("body").inner_html().to_string()
}

/// The whole file. An absent value omits its element; nothing is written
/// empty.
pub fn render(provenance: &Provenance, article_html: &str) -> String {
    let mut head = String::from("<meta charset=\"utf-8\">\n");
    if let Some(title) = &provenance.title {
        head.push_str(&format!("<title>{}</title>\n", escape(title)));
    }
    let mut meta = |name: &str, value: &str| {
        head.push_str(&format!(
            "<meta name=\"{name}\" content=\"{}\">\n",
            escape(value)
        ));
    };
    if let Some(author) = &provenance.author {
        meta("author", author);
    }
    meta("wilkes:link", &provenance.link);
    if let Some(published) = &provenance.published {
        meta(
            "wilkes:published",
            &published.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
    }
    meta("wilkes:feed", &provenance.feed);
    meta(
        "wilkes:fetched",
        &provenance
            .fetched
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    meta("wilkes:feed-item", &provenance.item_id);

    let mut byline: Vec<String> = Vec::new();
    if let Some(author) = &provenance.author {
        byline.push(escape(author));
    }
    if let Some(published) = &provenance.published {
        byline.push(published.format("%-d %b %Y").to_string());
    }
    let host = url::Url::parse(&provenance.link)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| provenance.link.clone());
    byline.push(format!(
        "<a href=\"{}\">{}</a>",
        escape(&provenance.link),
        escape(&host)
    ));

    let mut header = String::from("<header>");
    if let Some(title) = &provenance.title {
        header.push_str(&format!("<h1>{}</h1>\n", escape(title)));
    }
    header.push_str(&format!("<p>{}</p></header>\n", byline.join(" · ")));

    format!(
        "<!doctype html>\n<html><head>\n{head}</head><body><article>\n{header}{article_html}\n</article></body></html>\n"
    )
}

/// `<date> <title>.html`: the date the post was published, or first seen
/// when it gave none, and its title cut to 80 characters, with nothing in it
/// that a file name cannot hold.
pub fn file_name(date: DateTime<Utc>, title: Option<&str>, fallback: &str) -> String {
    let cleaned: String = title
        .unwrap_or(fallback)
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let mut name = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.chars().count() > 80 {
        name = name
            .chars()
            .take(80)
            .collect::<String>()
            .trim_end()
            .to_string();
    }
    let name = name.trim_matches('.').trim().to_string();
    let name = if name.is_empty() {
        fallback.to_string()
    } else {
        name
    };
    format!("{} {name}.html", date.format("%Y-%m-%d"))
}

/// The same name, told apart from one already taken by the item's id.
pub fn disambiguated(name: &str, item_id: &str) -> String {
    let stem = name.strip_suffix(".html").unwrap_or(name);
    let short: String = item_id.chars().take(6).collect();
    format!("{stem} ({short}).html")
}

fn escape(text: &str) -> String {
    html_escape::encode_double_quoted_attribute(text).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn provenance() -> Provenance {
        Provenance {
            title: Some("Report from <the> Vatican".into()),
            author: Some("Gary Marcus".into()),
            link: "https://garymarcus.substack.com/p/report-from-the-vatican".into(),
            published: Utc.with_ymd_and_hms(2026, 10, 31, 15, 17, 33).single(),
            feed: "Gary Marcus".into(),
            fetched: Utc
                .with_ymd_and_hms(2026, 11, 14, 15, 2, 11)
                .single()
                .unwrap(),
            item_id: "5f0c2d9e-8a41".into(),
        }
    }

    #[test]
    fn the_file_carries_its_provenance_in_the_head_and_the_header() {
        let html = render(&provenance(), "<p>Body.</p>");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<title>Report from &lt;the&gt; Vatican</title>"));
        assert!(html.contains("<meta name=\"wilkes:published\" content=\"2026-10-31T15:17:33Z\">"));
        assert!(html.contains("<meta name=\"wilkes:feed-item\" content=\"5f0c2d9e-8a41\">"));
        assert!(html.contains("<h1>Report from &lt;the&gt; Vatican</h1>"));
        assert!(html.contains("Gary Marcus · 31 Oct 2026 · <a href=\"https://garymarcus.substack.com/p/report-from-the-vatican\">garymarcus.substack.com</a>"));
        // The visible text a search reads starts with the header.
        let masked = crate::extract::html::mask(&html);
        let visible = crate::extract::html::visible_projection(&masked).into_text();
        assert!(
            visible.starts_with("Report from <the> Vatican\nGary Marcus"),
            "{visible}"
        );
    }

    #[test]
    fn an_absent_value_is_not_written_empty() {
        let mut bare = provenance();
        bare.title = None;
        bare.author = None;
        bare.published = None;
        let html = render(&bare, "<p>Body.</p>");
        assert!(!html.contains("<title>"));
        assert!(!html.contains("name=\"author\""));
        assert!(!html.contains("wilkes:published"));
        assert!(!html.contains("<h1>"));
        assert!(!html.contains("content=\"\""));
    }

    #[test]
    fn pictures_go_inside_and_a_missing_one_is_removed() {
        let png = b"\x89PNG\r\n\x1a\nrest".to_vec();
        assert_eq!(picture_mime(&png), Some("image/png"));
        assert_eq!(
            picture_mime(b"<?xml version=\"1.0\"?><svg></svg>"),
            Some("image/svg+xml")
        );
        assert_eq!(picture_mime(b"<html>not a picture</html>"), None);
        let mut pictures = HashMap::new();
        pictures.insert(
            "https://example.org/a.png".to_string(),
            Picture {
                mime: "image/png",
                bytes: png,
            },
        );
        let html = embed_pictures(
            "<p>x</p><img src=\"https://example.org/a.png\"><img src=\"https://example.org/gone.png\">",
            &pictures,
        );
        assert!(
            html.contains("src=\"data:image/png;base64,iVBORw0KGgpyZXN0\""),
            "{html}"
        );
        assert!(!html.contains("gone.png"));
    }

    #[test]
    fn names_are_dated_cut_and_cleaned() {
        let date = Utc
            .with_ymd_and_hms(2026, 10, 31, 0, 0, 0)
            .single()
            .unwrap();
        assert_eq!(
            file_name(date, Some("What/is: AI? <really>"), "item"),
            "2026-10-31 What is AI really.html"
        );
        let long = "word ".repeat(40);
        let name = file_name(date, Some(&long), "item");
        assert!(name.chars().count() <= "2026-10-31 ".len() + 80 + ".html".len());
        assert_eq!(file_name(date, Some("..."), "item"), "2026-10-31 item.html");
        assert_eq!(
            disambiguated("2026-10-31 Post.html", "5f0c2d9e-8a41"),
            "2026-10-31 Post (5f0c2d).html"
        );
    }
}
