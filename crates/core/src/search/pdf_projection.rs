use crate::types::ByteRange;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};

#[derive(Clone, Debug)]
struct NormalizedChar {
    c: char,
    raw_range: ByteRange,
}

#[derive(Clone, Debug)]
struct ProjectionSpan {
    projected_range: ByteRange,
    raw_range: ByteRange,
}

/// Search-only view of extracted PDF text: whitespace collapsed, ligatures
/// expanded, typographic quotes and dashes folded, while `spans` maps every
/// emitted scalar back to the extraction bytes SourceMap and preview
/// highlighting are expressed in.
///
/// It does not repair line-wrap hyphenation. It used to, and that was
/// compensation for a defect in the stored reading: a word the typesetter
/// broke across a line was stored broken, and only literal search knew better.
/// The reading is now sanitized where it is produced
/// (`extract::pdf::sanitize`), so what remains here is a view over *how a page
/// set* the text — never a second opinion about what the text says.
#[derive(Clone, Debug)]
pub(crate) struct PdfSearchProjection {
    text: String,
    spans: Vec<ProjectionSpan>,
}

impl PdfSearchProjection {
    pub(crate) fn new(raw: &str) -> Self {
        let chars = normalize_text(raw);
        let mut projection = Self {
            text: String::with_capacity(raw.len()),
            spans: Vec::with_capacity(chars.len()),
        };
        for NormalizedChar { c, raw_range } in chars {
            projection.push_scalar(c, raw_range);
        }
        projection
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    /// Map a non-empty matcher range in projected bytes to the smallest raw
    /// extraction range containing every contributing normalized scalar.
    pub(crate) fn raw_range(&self, projected: ByteRange) -> Option<ByteRange> {
        let mut overlapping = self.spans.iter().filter(|span| {
            span.projected_range.start < projected.end && span.projected_range.end > projected.start
        });
        let first = overlapping.next()?;
        let mut start = first.raw_range.start;
        let mut end = first.raw_range.end;
        for span in overlapping {
            start = start.min(span.raw_range.start);
            end = end.max(span.raw_range.end);
        }
        Some(ByteRange { start, end })
    }

    fn push_scalar(&mut self, c: char, raw_range: ByteRange) {
        let start = self.text.len();
        self.text.push(c);
        self.spans.push(ProjectionSpan {
            projected_range: ByteRange {
                start,
                end: self.text.len(),
            },
            raw_range,
        });
    }
}

/// One unit of a query. A query is the one side that can still carry a visual
/// line wrap: it may have been pasted out of a PDF viewer, which shows the
/// page's line breaks and not the document's words.
#[derive(Clone, Debug)]
enum QueryUnit {
    Char(char),
    /// A hyphen the paste's own line break marks as discretionary. The reading
    /// either joined that word or kept the hyphen — which one is the
    /// document's business — so the query accepts both.
    WrapHyphen,
}

/// Compile a non-regex PDF query against [`PdfSearchProjection`].
///
/// A hyphen a paste shows at a line end is optional, because the reading
/// resolved that break one of two ways and the person pasting cannot know
/// which. A hyphen the query states inline is required: it is the one thing
/// the person typing can be taken at their word about.
pub(crate) fn literal_matcher(query: &str, case_sensitive: bool) -> anyhow::Result<RegexMatcher> {
    let units = normalize_query(query);
    anyhow::ensure!(
        !units.is_empty(),
        "PDF search query is empty after removing extraction artifacts"
    );

    let optional_hyphen = format!("(?:{})?", regex::escape("-"));
    let mut pattern = String::with_capacity(query.len().saturating_mul(2));
    for unit in &units {
        match unit {
            QueryUnit::WrapHyphen => pattern.push_str(&optional_hyphen),
            QueryUnit::Char(c) => pattern.push_str(&regex::escape(&c.to_string())),
        }
    }

    Ok(RegexMatcherBuilder::new()
        .case_insensitive(!case_sensitive)
        .build(&pattern)?)
}

/// How one scalar folds for matching. Shared by both sides so a query and the
/// text it is matched against fold identically — the only difference between
/// them is the line-wrap question, which only a query can ask.
enum Folded {
    /// Invisible: a zero-width mark, or a soft hyphen the renderer only shows
    /// when it breaks a line.
    Skip,
    One(char),
    Many(&'static str),
}

fn fold(c: char) -> Folded {
    match c {
        '\u{fb00}' => Folded::Many("ff"),
        '\u{fb01}' => Folded::Many("fi"),
        '\u{fb02}' => Folded::Many("fl"),
        '\u{fb03}' => Folded::Many("ffi"),
        '\u{fb04}' => Folded::Many("ffl"),
        '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' | '\u{02bc}' => Folded::One('\''),
        '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => Folded::One('"'),
        '\u{2010}' | '\u{2011}' => Folded::One('-'),
        '\u{200b}' | '\u{2060}' | '\u{feff}' | '\u{00ad}' => Folded::Skip,
        c => Folded::One(c),
    }
}

/// Fold extracted text, collapsing each whitespace run to one space and
/// keeping every emitted scalar's raw byte range.
fn normalize_text(input: &str) -> Vec<NormalizedChar> {
    let chars = input.char_indices().collect::<Vec<_>>();
    let mut out: Vec<NormalizedChar> = Vec::with_capacity(chars.len());
    let mut index = 0;

    while index < chars.len() {
        let (start, c) = chars[index];

        if c.is_whitespace() {
            let mut next = index + 1;
            while next < chars.len() && chars[next].1.is_whitespace() {
                next += 1;
            }
            let whitespace_end = chars.get(next).map_or(input.len(), |(start, _)| *start);
            push_space(
                &mut out,
                ByteRange {
                    start,
                    end: whitespace_end,
                },
            );
            index = next;
            continue;
        }

        let raw_range = ByteRange {
            start,
            end: char_end(input, &chars, index),
        };
        match fold(c) {
            Folded::Skip => {}
            Folded::One(c) => out.push(NormalizedChar { c, raw_range }),
            Folded::Many(expansion) => out.extend(expansion.chars().map(|c| NormalizedChar {
                c,
                raw_range: raw_range.clone(),
            })),
        }
        index += 1;
    }

    out
}

/// A whitespace run is one space, and two runs in a row are still one space —
/// with the raw range extended so a match still resolves to every byte it
/// covered.
fn push_space(out: &mut Vec<NormalizedChar>, raw_range: ByteRange) {
    if let Some(previous) = out.last_mut() {
        if previous.c == ' ' {
            previous.raw_range.end = raw_range.end;
            return;
        }
    }
    out.push(NormalizedChar { c: ' ', raw_range });
}

/// Fold a query the same way, additionally recognising a hyphen the paste
/// broke a line at, and dropping the whitespace at either end.
fn normalize_query(input: &str) -> Vec<QueryUnit> {
    let chars = input.char_indices().collect::<Vec<_>>();
    let mut units: Vec<QueryUnit> = Vec::with_capacity(chars.len());
    let mut index = 0;

    while index < chars.len() {
        let (_, c) = chars[index];

        if is_discretionary_hyphen(c) && index > 0 && chars[index - 1].1.is_alphabetic() {
            if let Some(continuation) = line_wrap_continuation(&chars, index) {
                units.push(QueryUnit::WrapHyphen);
                index = continuation;
                continue;
            }
            if c == '\u{00ad}'
                && chars
                    .get(index + 1)
                    .is_some_and(|(_, next)| next.is_alphabetic())
            {
                units.push(QueryUnit::WrapHyphen);
                index += 1;
                continue;
            }
        }

        if c.is_whitespace() {
            let mut next = index + 1;
            while next < chars.len() && chars[next].1.is_whitespace() {
                next += 1;
            }
            if !units.is_empty() && !matches!(units.last(), Some(QueryUnit::Char(' '))) {
                units.push(QueryUnit::Char(' '));
            }
            index = next;
            continue;
        }

        match fold(c) {
            Folded::Skip => {}
            Folded::One(c) => units.push(QueryUnit::Char(c)),
            Folded::Many(expansion) => units.extend(expansion.chars().map(QueryUnit::Char)),
        }
        index += 1;
    }

    if matches!(units.last(), Some(QueryUnit::Char(' '))) {
        units.pop();
    }
    units
}

fn char_end(input: &str, chars: &[(usize, char)], index: usize) -> usize {
    chars
        .get(index + 1)
        .map_or(input.len(), |(next_start, _)| *next_start)
}

fn line_wrap_continuation(chars: &[(usize, char)], hyphen_index: usize) -> Option<usize> {
    let mut index = hyphen_index + 1;
    while chars
        .get(index)
        .is_some_and(|(_, c)| c.is_whitespace() && !is_line_break(*c))
    {
        index += 1;
    }

    let (_, line_break) = chars.get(index)?;
    if !is_line_break(*line_break) {
        return None;
    }
    if *line_break == '\r' && chars.get(index + 1).is_some_and(|(_, c)| *c == '\n') {
        index += 1;
    }
    index += 1;

    while chars
        .get(index)
        .is_some_and(|(_, c)| c.is_whitespace() && !is_line_break(*c))
    {
        index += 1;
    }
    chars
        .get(index)
        .and_then(|(_, c)| c.is_alphabetic().then_some(index))
}

fn is_discretionary_hyphen(c: char) -> bool {
    matches!(c, '-' | '\u{00ad}' | '\u{2010}' | '\u{2011}')
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use grep_matcher::Matcher;

    fn matches(raw: &str, query: &str) -> Vec<ByteRange> {
        let projection = PdfSearchProjection::new(raw);
        let matcher = literal_matcher(query, true).unwrap();
        let mut ranges = Vec::new();
        matcher
            .find_iter(projection.as_bytes(), |found| {
                ranges.push(
                    projection
                        .raw_range(ByteRange {
                            start: found.start(),
                            end: found.end(),
                        })
                        .unwrap(),
                );
                true
            })
            .unwrap();
        ranges
    }

    /// The reading a PDF now stores: words whole, lines still where the page
    /// put them. A passage query spanning those lines still finds it.
    #[test]
    fn literal_passage_ignores_the_lines_a_page_broke_the_passage_into() {
        let raw = "The topic should be specific to your degree programme.\n  It should also be something\nthat interests you.";
        let query = "The topic should be specific to your degree programme.\nIt should also be something that interests you.";
        let ranges = matches(raw, query);

        assert_eq!(ranges.len(), 1);
        assert_eq!(&raw[ranges[0].start..ranges[0].end], raw);
    }

    #[test]
    fn genuine_inline_hyphen_is_not_optional() {
        assert!(matches("some-thing", "something").is_empty());
        assert_eq!(matches("some-thing", "some-thing").len(), 1);
        assert_eq!(matches("state-of-the-art", "state-of-the-art").len(), 1);
    }

    /// A user pasting a phrase out of a PDF viewer pastes the page's line
    /// break with it. The reading resolved that break one way or the other,
    /// and the query has to match whichever it chose.
    #[test]
    fn pasted_wrap_hyphen_query_matches_the_joined_or_the_hyphenated_reading() {
        assert_eq!(matches("state-of-the-art", "state-\nof-the-art").len(), 1);
        assert_eq!(matches("stateof-the-art", "state-\nof-the-art").len(), 1);
    }

    /// A soft hyphen is invisible until it breaks a line, so it is invisible
    /// to search on both sides.
    #[test]
    fn a_soft_hyphen_is_not_part_of_the_word_on_either_side() {
        assert_eq!(matches("pre\u{00ad}shared", "preshared").len(), 1);
        assert_eq!(matches("preshared", "pre\u{00ad}shared").len(), 1);
    }

    #[test]
    fn normalizes_ligatures_typographic_quotes_spaces_and_format_marks() {
        let raw = "The\u{00a0}\u{fb01}eld said \u{201c}don\u{2019}t\u{201d}\u{200b}.";
        assert_eq!(matches(raw, "The field said \"don't\".").len(), 1);
    }

    #[test]
    fn does_not_conflate_em_dash_with_hyphen() {
        assert!(matches("A—B", "A-B").is_empty());
    }

    #[test]
    fn maps_multibyte_and_expanded_characters_to_valid_raw_boundaries() {
        let raw = "Préface: \u{fb01}eld something fin.";
        let ranges = matches(raw, "field something");
        assert_eq!(ranges.len(), 1);
        let range = &ranges[0];
        assert!(raw.is_char_boundary(range.start));
        assert!(raw.is_char_boundary(range.end));
        assert_eq!(&raw[range.start..range.end], "\u{fb01}eld something");
    }

    /// The projection no longer reserves any character for itself, so a
    /// document that happens to contain one is ordinary text.
    #[test]
    fn private_use_characters_are_ordinary_document_text() {
        let raw = "a\u{e000}b ab";
        assert_eq!(matches(raw, "a\u{e000}b").len(), 1);
        assert_eq!(matches(raw, "ab").len(), 1);
    }

    #[test]
    fn query_outer_whitespace_is_ignored() {
        assert_eq!(matches("before target after", " \n target\t").len(), 1);
    }

    #[test]
    fn case_sensitivity_is_applied_after_projection() {
        let projection = PdfSearchProjection::new("some thing");
        let insensitive = literal_matcher("SOME THING", false).unwrap();
        let sensitive = literal_matcher("SOME THING", true).unwrap();

        assert!(insensitive.is_match(projection.as_bytes()).unwrap());
        assert!(!sensitive.is_match(projection.as_bytes()).unwrap());
    }

    #[test]
    fn long_passage_pattern_compiles_and_matches() {
        let query = (0..400)
            .map(|index| format!("substantive{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let projection = PdfSearchProjection::new(&query);
        let matcher = literal_matcher(&query, true).unwrap();

        assert!(matcher.is_match(projection.as_bytes()).unwrap());
    }
}
