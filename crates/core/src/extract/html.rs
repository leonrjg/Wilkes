//! Reading an HTML file: the text the viewer shows, and nothing else.
//!
//! An HTML file used to be indexed as plain text, markup and all — every tag,
//! attribute and, for a page with its pictures inside, megabytes of base64.
//! Search matched inside attributes, chunks were mostly markup, and the
//! embedding model was handed `class="..."` to understand.
//!
//! The reading has two layers.
//!
//! - **Masked text** ([`mask`]): the file with every byte the viewer does not
//!   show replaced by a space, newlines kept. Same length, same newlines, still
//!   valid UTF-8, so a position in it is a position in the file.
//! - **Visible-text projection** ([`visible_projection`]): the masked text with
//!   whitespace collapsed as a browser would, every projected character mapped
//!   back to the bytes of the file it came from. This is what is chunked,
//!   embedded, searched and summarised; a match in it resolves to the file's
//!   own bytes, which is what the viewer highlights.
//!
//! What counts as shown is decided by the reader's sanitizer
//! (`wilkes-reader`'s `htmlDocument.ts`), and the two must agree: its
//! `STRIPPED` list is repeated here, element for element.

use std::path::Path;

use crate::extract::ContentExtractor;
use crate::text_projection::TextProjection;
use crate::types::{
    ByteRange, DeclaredOutline, ExtractedContent, FileMetadata, OutlineAnchor, OutlineEntry,
    SourceMap, SourceOrigin, SourceSegment, TextProvenance,
};

/// The recipe every HTML file is extracted under. Bumping it re-extracts and
/// re-embeds every HTML file in every library.
pub const HTML_RECIPE: &str = "html-visible-v1";

/// Elements removed with their contents, as the reader's sanitizer removes
/// them (`STRIPPED` in `htmlDocument.ts`). Anything else is unwrapped there and
/// keeps its text, so it keeps it here.
pub(crate) const STRIPPED: &[&str] = &[
    "script", "style", "title", "head", "noscript", "template", "iframe", "frame", "frameset",
    "object", "embed", "applet", "form", "button", "select", "textarea", "canvas", "audio",
    "video", "svg", "math", "link", "meta", "base", "dialog", "portal",
];

/// Of [`STRIPPED`], the void elements: they have no closing tag, so removing
/// one removes the tag alone.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
    "source", "track", "wbr",
];

/// Elements whose content the parser reads as raw text: the first closing tag
/// of the same name ends them, whatever looks like markup in between.
const RAW_TEXT: &[&str] = &[
    "script", "style", "textarea", "title", "xmp", "iframe", "noembed", "noframes", "noscript",
];

/// Elements that sit inside a line of text. A tag of one of these separates
/// nothing: `un<em>believ</em>able` is one word on screen and one word here.
const INLINE: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "data", "del", "dfn", "em", "font", "i",
    "ins", "kbd", "mark", "q", "s", "samp", "small", "span", "strike", "strong", "sub", "sup",
    "time", "tt", "u", "var", "wbr",
];

/// Elements that end a line of text where they open or close.
const BLOCK: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "br",
    "caption",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "html",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "tbody",
    "tfoot",
    "thead",
    "tr",
    "ul",
];

/// What a blanked stretch of the file is to the text around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Blank {
    /// Nothing: an inline tag, or the padding after a decoded entity.
    Join,
    /// A word break: any other tag, a comment, a stripped element.
    Space,
    /// A line break: a block element's tag.
    Break,
}

/// The file with everything the viewer does not show blanked.
#[derive(Clone, Debug)]
pub struct Masked {
    /// Same length and newlines as the file; valid UTF-8.
    pub text: String,
    /// Each blanked stretch and what it stands for, in file order.
    blanks: Vec<(ByteRange, Blank)>,
    /// Each `h1`–`h6`: its level and the file range between its tags.
    headings: Vec<(u32, ByteRange)>,
    /// The text of the first `<title>`, as written.
    title: Option<ByteRange>,
}

/// Blank every byte the viewer does not show: tags with their attributes,
/// comments, the doctype, and the elements the reader removes with their
/// contents. Entities are decoded in place and padded with spaces, which
/// always fits: no entity is shorter than the character it names, in UTF-8.
pub fn mask(source: &str) -> Masked {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut blanks: Vec<(ByteRange, Blank)> = Vec::new();
    let mut headings = Vec::new();
    let mut open_headings: Vec<(u32, usize)> = Vec::new();
    let mut title = None;
    let n = bytes.len();
    let mut i = 0usize;

    while i < n {
        match bytes[i] {
            b'<' => {
                let next = bytes.get(i + 1).copied();
                if source[i..].starts_with("<!--") {
                    let end = find(bytes, b"-->", i + 4).map_or(n, |at| at + 3);
                    push_blank(&mut out, &mut blanks, i, end, Blank::Space);
                    i = end;
                } else if matches!(next, Some(b'!') | Some(b'?')) {
                    let end = find(bytes, b">", i + 2).map_or(n, |at| at + 1);
                    push_blank(&mut out, &mut blanks, i, end, Blank::Space);
                    i = end;
                } else if next == Some(b'/')
                    && bytes.get(i + 2).is_some_and(u8::is_ascii_alphabetic)
                {
                    let name = tag_name(bytes, i + 2);
                    let end = tag_end(bytes, i + 2);
                    if let Some(level) = heading_level(&name) {
                        if let Some(position) =
                            open_headings.iter().rposition(|(open, _)| *open == level)
                        {
                            let (level, start) = open_headings.remove(position);
                            headings.push((level, ByteRange { start, end: i }));
                        }
                    }
                    push_blank(&mut out, &mut blanks, i, end, classify(&name));
                    i = end;
                } else if next.is_some_and(|c| c.is_ascii_alphabetic()) {
                    let name = tag_name(bytes, i + 1);
                    let end = tag_end(bytes, i + 1);
                    let self_closing = end >= 2 && bytes[end - 2] == b'/';
                    let stripped = STRIPPED.contains(&name.as_str())
                        && !VOID.contains(&name.as_str())
                        && !self_closing;
                    if stripped {
                        let close = closing_tag(source, &name, end);
                        if name == "title" && title.is_none() {
                            title = Some(ByteRange {
                                start: end,
                                end: close.map_or(n, |(start, _)| start),
                            });
                        }
                        let stop = close.map_or(n, |(_, after)| after);
                        push_blank(&mut out, &mut blanks, i, stop, Blank::Space);
                        i = stop;
                    } else {
                        if let Some(level) = heading_level(&name) {
                            open_headings.push((level, end));
                        }
                        push_blank(&mut out, &mut blanks, i, end, classify(&name));
                        i = end;
                    }
                } else {
                    // A `<` that opens nothing is text.
                    i += 1;
                }
            }
            b'&' => match entity(source, i) {
                Some((end, decoded)) => {
                    let decoded_len = decoded.len();
                    out[i..i + decoded_len].copy_from_slice(decoded.as_bytes());
                    if i + decoded_len < end {
                        push_blank(&mut out, &mut blanks, i + decoded_len, end, Blank::Join);
                    }
                    i = end;
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }

    let text = String::from_utf8(out).expect(
        "masking replaces whole tags and whole entities with ASCII or a decoded character, \
         so it cannot split a UTF-8 sequence",
    );
    headings.sort_by_key(|(_, range)| range.start);
    Masked {
        text,
        blanks,
        headings,
        title,
    }
}

/// The visible text of an HTML file, each character mapped to the file bytes
/// it was read from.
///
/// Whitespace collapses as a browser collapses it: a run of it is one space,
/// a run that crosses a block element's tag is one line break, and a run made
/// only of inline tags and entity padding is nothing at all. Nothing leads or
/// trails.
pub(crate) fn visible_projection(masked: &Masked) -> TextProjection {
    let text = masked.text.as_str();
    let mut projection = TextProjection::with_capacity(text.len() / 8);
    let mut blanks = masked.blanks.iter().peekable();
    // The pending run: where it started, where it ends, and the strongest
    // break inside it.
    let mut run: Option<(usize, usize, Blank)> = None;

    let flush = |projection: &mut TextProjection, run: &mut Option<(usize, usize, Blank)>| {
        let Some((start, end, kind)) = run.take() else {
            return;
        };
        let separator = match kind {
            Blank::Join => return,
            Blank::Space => ' ',
            Blank::Break => '\n',
        };
        if projection.last_char().is_none() {
            return;
        }
        projection.push_scalar(separator, ByteRange { start, end });
    };

    for (offset, c) in text.char_indices() {
        let end = offset + c.len_utf8();
        while blanks.peek().is_some_and(|(range, _)| range.end <= offset) {
            blanks.next();
        }
        let blank = blanks
            .peek()
            .filter(|(range, _)| range.start <= offset)
            .map(|(_, kind)| *kind);
        let kind = match blank {
            Some(kind) => Some(kind),
            None if c.is_whitespace() => Some(Blank::Space),
            None => None,
        };
        match kind {
            Some(kind) => {
                run = Some(match run {
                    Some((start, _, strongest)) => (start, end, strongest.max(kind)),
                    None => (offset, end, kind),
                });
            }
            None => {
                flush(&mut projection, &mut run);
                projection.push_scalar(c, ByteRange { start: offset, end });
            }
        }
    }
    projection
}

/// Whether Wilkes reads `path` as HTML.
pub fn is_html(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["html", "htm", "xhtml"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// An HTML file's reading and its projection, from its bytes as they are now.
pub(crate) fn read(source: &str) -> (Masked, TextProjection) {
    let masked = mask(source);
    let projection = visible_projection(&masked);
    (masked, projection)
}

/// Map a range of an HTML file's reading — the text its chunks and stored
/// full text are cut from — to the bytes of the file it was read from, which
/// is what the viewer highlights.
///
/// Reads the file again rather than keeping a map beside the index: masking a
/// file is a linear pass, and a map kept beside the index would be a second
/// copy of what the file already says.
pub fn file_range(path: &Path, reading: ByteRange) -> anyhow::Result<Option<ByteRange>> {
    let source = std::fs::read_to_string(path)?;
    let (_, projection) = read(&source);
    Ok(projection.raw_range(reading))
}

/// Reads `.html`, `.htm` and `.xhtml` files as the text the viewer shows.
pub struct HtmlExtractor;

impl ContentExtractor for HtmlExtractor {
    fn can_handle(&self, path: &Path, _mime: Option<&str>) -> bool {
        is_html(path)
    }

    fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent> {
        let source = std::fs::read_to_string(path)?;
        let (masked, projection) = read(&source);
        let source_map = line_source_map(&source, &projection);
        let title = masked
            .title
            .as_ref()
            .map(|range| collapse(&masked.text[range.start..range.end]))
            .filter(|title| !title.is_empty());
        Ok(ExtractedContent {
            text: projection.into_text(),
            source_map,
            metadata: FileMetadata {
                path: path.to_path_buf(),
                size_bytes: source.len() as u64,
                mime: Some("text/html".to_string()),
                title,
                page_count: None,
            },
            images: Vec::new(),
        })
    }

    fn recipe_identity(&self, path: &Path) -> Option<String> {
        is_html(path).then(|| HTML_RECIPE.to_string())
    }

    /// `h1`–`h6`, in document order, each at its offset in the reading.
    fn outline(&self, path: &Path) -> anyhow::Result<DeclaredOutline> {
        let source = std::fs::read_to_string(path)?;
        let (masked, projection) = read(&source);
        Ok(DeclaredOutline {
            entries: heading_entries(&masked, &projection),
            diagnostics: Default::default(),
        })
    }
}

fn heading_entries(masked: &Masked, projection: &TextProjection) -> Vec<OutlineEntry> {
    let text = projection.as_str();
    masked
        .headings
        .iter()
        .filter_map(|(level, range)| {
            let start = projection.projected_offset(range.start);
            let end = projection.projected_offset(range.end).max(start);
            let title = collapse(text.get(start..end)?);
            (!title.is_empty()).then(|| OutlineEntry {
                title,
                level: level - 1,
                page: None,
                byte_offset: Some(start),
                anchor: OutlineAnchor::TextOffset,
            })
        })
        .collect()
}

/// One segment per line of the file the reading draws from, so a chunk or a
/// match resolves to the line of the file it came from.
fn line_source_map(source: &str, projection: &TextProjection) -> SourceMap {
    let mut segments: Vec<SourceSegment> = Vec::new();
    let mut line = 1u32;
    let mut counted_to = 0usize;
    for (projected, raw) in projection.ranges() {
        line += source.as_bytes()[counted_to..raw.start.max(counted_to)]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count() as u32;
        counted_to = counted_to.max(raw.start);
        match segments.last_mut() {
            Some(last)
                if matches!(last.origin, SourceOrigin::TextFile { line: at, .. } if at == line)
                    && last.text_range.end == projected.start =>
            {
                last.text_range.end = projected.end;
            }
            _ => segments.push(SourceSegment {
                text_range: projected.clone(),
                origin: SourceOrigin::TextFile { line, col: 0 },
                provenance: TextProvenance::Native,
            }),
        }
    }
    SourceMap { segments }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_blank(
    out: &mut [u8],
    blanks: &mut Vec<(ByteRange, Blank)>,
    start: usize,
    end: usize,
    kind: Blank,
) {
    for byte in &mut out[start..end] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
    blanks.push((ByteRange { start, end }, kind));
}

fn classify(name: &str) -> Blank {
    if INLINE.contains(&name) {
        Blank::Join
    } else if BLOCK.contains(&name) {
        Blank::Break
    } else {
        Blank::Space
    }
}

fn heading_level(name: &str) -> Option<u32> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn find(bytes: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= bytes.len() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at)
}

/// The lowercased tag name starting at `from`.
fn tag_name(bytes: &[u8], from: usize) -> String {
    bytes[from..]
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == b'-' || **c == b':')
        .map(|c| c.to_ascii_lowercase() as char)
        .collect()
}

/// One past the `>` that ends the tag whose name starts at `from`, or the end
/// of the file. A quote opens a quoted value only right after `=`, as the
/// tokenizer reads it, so an apostrophe in an unquoted value ends nothing.
fn tag_end(bytes: &[u8], from: usize) -> usize {
    let n = bytes.len();
    let mut i = from;
    while i < n {
        match bytes[i] {
            b'>' => return i + 1,
            b'=' => {
                i += 1;
                while i < n && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i < n && (bytes[i] == b'"' || bytes[i] == b'\'') {
                    let quote = bytes[i];
                    i += 1;
                    while i < n && bytes[i] != quote {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    n
}

/// Where the element `name`, opened before `from`, closes: the closing tag's
/// start and one past its end. Raw-text elements close at the first closing
/// tag of their name; others count nested elements of the same name.
fn closing_tag(source: &str, name: &str, from: usize) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let raw_text = RAW_TEXT.contains(&name);
    let mut depth = 0usize;
    let mut i = from;
    while let Some(at) = find(bytes, b"<", i) {
        let closing = bytes.get(at + 1) == Some(&b'/');
        let name_at = if closing { at + 2 } else { at + 1 };
        if bytes.get(name_at).is_some_and(u8::is_ascii_alphabetic) && tag_name(bytes, name_at) == name
        {
            let end = tag_end(bytes, name_at);
            if closing {
                if depth == 0 {
                    return Some((at, end));
                }
                depth -= 1;
            } else if !raw_text && !(end >= 2 && bytes[end - 2] == b'/') {
                depth += 1;
            }
            i = end;
        } else {
            i = at + 1;
        }
    }
    None
}

/// The entity starting at `at`: one past its `;`, and what it decodes to, if
/// it is one and its decoding fits in its own bytes.
fn entity(source: &str, at: usize) -> Option<(usize, String)> {
    let bytes = source.as_bytes();
    let body_start = at + 1;
    let mut end = body_start;
    if bytes.get(end) == Some(&b'#') {
        end += 1;
        if matches!(bytes.get(end), Some(b'x') | Some(b'X')) {
            end += 1;
            while bytes.get(end).is_some_and(u8::is_ascii_hexdigit) {
                end += 1;
            }
        } else {
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
        }
    } else if bytes.get(end).is_some_and(u8::is_ascii_alphabetic) {
        while bytes.get(end).is_some_and(u8::is_ascii_alphanumeric) {
            end += 1;
        }
    } else {
        return None;
    }
    if bytes.get(end) != Some(&b';') || end == body_start {
        return None;
    }
    let end = end + 1;
    let written = &source[at..end];
    let decoded = html_escape::decode_html_entities(written);
    if decoded == written || decoded.len() > written.len() {
        // Not an entity, or one whose characters would not fit where it was
        // written: kept as written, which is what it then is.
        return None;
    }
    Some((end, decoded.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible(source: &str) -> String {
        visible_projection(&mask(source)).as_str().to_string()
    }

    #[test]
    fn masking_keeps_length_newlines_and_utf8() {
        let source = "<!doctype html>\n<html><head><title>T</title>\n<style>p{}</style></head>\n<body><p class=\"x\">Caf&eacute; &amp; cr\u{e8}me</p>\n<img src=\"data:image/png;base64,AAAA\">\n<!-- note -->\n</body></html>";
        let masked = mask(source);
        assert_eq!(masked.text.len(), source.len());
        let newlines = |text: &str| {
            text.bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        };
        assert_eq!(newlines(&masked.text), newlines(source));
        assert!(!masked.text.contains('<'));
        assert!(!masked.text.contains("base64"));
        assert!(!masked.text.contains("p{}"));
        assert_eq!(collapse(&masked.text), "Café & crème");
    }

    #[test]
    fn inline_tags_and_entities_join_their_neighbours() {
        assert_eq!(visible("<p>un<em>believ</em>able</p>"), "unbelievable");
        assert_eq!(visible("<p>don&rsquo;t</p>"), "don\u{2019}t");
        assert_eq!(visible("<p>a <a href=\"x\">link</a>ed word</p>"), "a linked word");
        assert_eq!(visible("<p>one</p><p>two</p>"), "one\ntwo");
        assert_eq!(visible("<td>a</td><td>b</td>"), "a b");
        assert_eq!(visible("  <p>\n  lead  </p>  "), "lead");
    }

    #[test]
    fn stripped_elements_lose_their_contents_and_nest() {
        assert_eq!(
            visible("<p>a</p><svg><svg><text>x</text></svg><text>y</text></svg><p>b</p>"),
            "a\nb"
        );
        assert_eq!(visible("<script>if (a < b) {}</script>text"), "text");
        assert_eq!(visible("<p>keep</p><form><p>gone</p></form>"), "keep");
        // An unclosed raw-text element runs to the end, as the parser reads it.
        assert_eq!(visible("before<script>after"), "before");
        assert_eq!(visible("<meta charset=utf-8><p>after meta</p>"), "after meta");
    }

    #[test]
    fn a_projected_match_maps_back_to_the_file_bytes_and_line() {
        let source = "<html>\n<body>\n<p>The un<em>believ</em>able story</p>\n</body></html>";
        let (_, projection) = read(source);
        let text = projection.as_str();
        let start = text.find("unbelievable").unwrap();
        let raw = projection
            .raw_range(ByteRange {
                start,
                end: start + "unbelievable".len(),
            })
            .unwrap();
        assert_eq!(&source[raw.start..raw.end], "un<em>believ</em>able");
        let map = line_source_map(source, &projection);
        assert!(matches!(
            map.resolve_range(ByteRange {
                start,
                end: start + 3
            }),
            Some(SourceOrigin::TextFile { line: 3, .. })
        ));
    }

    #[test]
    fn headings_are_the_outline_at_their_reading_offsets() {
        let source = "<h1>Title</h1><p>x</p><h2>Part <em>one</em></h2><p>y</p><h2> </h2>";
        let (masked, projection) = read(source);
        let entries = heading_entries(&masked, &projection);
        let titles: Vec<_> = entries
            .iter()
            .map(|entry| (entry.title.as_str(), entry.level))
            .collect();
        assert_eq!(titles, vec![("Title", 0), ("Part one", 1)]);
        let offset = entries[1].byte_offset.unwrap();
        assert!(projection.as_str()[offset..].starts_with("Part one"));
    }

    /// A post written with 32 pictures inside, the largest the survey found:
    /// megabytes of base64 between paragraphs. The reading holds the
    /// paragraphs alone, and the chunker's window counts their characters,
    /// not the pictures' blanks.
    #[test]
    fn a_post_with_its_pictures_inside_reads_and_chunks_as_its_text() {
        use base64::Engine as _;
        let picture = base64::engine::general_purpose::STANDARD.encode(vec![7u8; 100_000]);
        let mut source = String::from(
            "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>Post</title></head>\n<body><article>\n",
        );
        for figure in 0..32 {
            for paragraph in 0..3 {
                source.push_str(&format!(
                    "<p>Paragraph {paragraph} before figure {figure}: {}</p>\n",
                    "words that a reader reads ".repeat(20)
                ));
            }
            source.push_str(&format!(
                "<figure><img src=\"data:image/png;base64,{picture}\" alt=\"figure {figure}\"><figcaption>Caption {figure}</figcaption></figure>\n"
            ));
        }
        source.push_str("</article></body></html>\n");
        assert!(source.len() > 3_000_000);

        let masked = mask(&source);
        assert_eq!(masked.text.len(), source.len());
        let reading = visible_projection(&masked).into_text();
        assert!(!reading.contains("base64") && !reading.contains('<') && !reading.contains("  "));
        assert!(reading.len() < source.len() / 50, "{}", reading.len());

        let content = ExtractedContent {
            text: reading,
            source_map: SourceMap { segments: Vec::new() },
            metadata: FileMetadata {
                path: "post.html".into(),
                size_bytes: source.len() as u64,
                mime: None,
                title: None,
                page_count: None,
            },
            images: Vec::new(),
        };
        let chunks =
            crate::embed::index::chunk::chunk_content(&content, "post.html".into(), 2000, 200);
        let (last, rest) = chunks.split_last().unwrap();
        assert!(last.text.chars().count() <= 2000);
        for chunk in rest {
            let length = chunk.text.chars().count();
            assert!((1500..=2000).contains(&length), "a chunk of {length} characters");
        }
    }

    /// The fixture the reader's own rendering is checked against
    /// (`ui/src/components/HtmlReading.test.tsx`): what the viewer shows of it
    /// and what search reads of it are the same text.
    #[test]
    fn the_fixture_reads_as_its_expected_visible_text() {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/html");
        let source = std::fs::read_to_string(fixtures.join("visible-text.html")).unwrap();
        let expected = std::fs::read_to_string(fixtures.join("visible-text.txt")).unwrap();
        let (_, projection) = read(&source);
        assert_eq!(projection.as_str(), expected.trim_end_matches('\n'));
    }

    #[test]
    fn an_unknown_entity_is_text_as_written() {
        assert_eq!(visible("a &unknownentity; b"), "a &unknownentity; b");
        assert_eq!(visible("AT&T and a & b"), "AT&T and a & b");
        assert_eq!(visible("&#x1F600;"), "\u{1F600}");
    }
}

