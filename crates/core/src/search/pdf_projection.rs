use crate::types::ByteRange;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};

/// Internal projection symbol for a discretionary hyphen at a visual line
/// boundary. Literal occurrences are escaped while constructing the projection,
/// so this symbol cannot collide with document content.
const WRAP_HYPHEN: char = '\u{e000}';
const LITERAL_ESCAPE: char = '\u{e001}';

#[derive(Clone, Debug)]
enum NormalizedUnitKind {
    Char(char),
    WrapHyphen,
}

#[derive(Clone, Debug)]
struct NormalizedUnit {
    kind: NormalizedUnitKind,
    raw_range: ByteRange,
}

#[derive(Clone, Debug)]
struct ProjectionSpan {
    projected_range: ByteRange,
    raw_range: ByteRange,
}

/// Search-only view of extracted PDF text. `text` is normalized enough to
/// ignore layout artifacts, while `spans` maps every emitted scalar back to the
/// original extraction bytes used by SourceMap and preview highlighting.
#[derive(Clone, Debug)]
pub(crate) struct PdfSearchProjection {
    text: String,
    spans: Vec<ProjectionSpan>,
}

impl PdfSearchProjection {
    pub(crate) fn new(raw: &str) -> Self {
        let units = normalize(raw, false);
        let mut projection = Self {
            text: String::with_capacity(raw.len()),
            spans: Vec::with_capacity(units.len()),
        };
        for unit in units {
            match unit.kind {
                NormalizedUnitKind::WrapHyphen => {
                    projection.push_scalar(WRAP_HYPHEN, unit.raw_range);
                }
                NormalizedUnitKind::Char(c) => {
                    if matches!(c, WRAP_HYPHEN | LITERAL_ESCAPE) {
                        projection.push_scalar(LITERAL_ESCAPE, unit.raw_range.clone());
                    }
                    projection.push_scalar(c, unit.raw_range);
                }
            }
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

/// Compile a non-regex PDF query against [`PdfSearchProjection`]. A visual
/// line-wrap hyphen may appear between adjacent letters without being present
/// in the query; an explicit query hyphen matches either a real hyphen or a
/// visual wrap hyphen, but a real document hyphen is never made optional.
pub(crate) fn literal_matcher(query: &str, case_sensitive: bool) -> anyhow::Result<RegexMatcher> {
    let units = normalize(query, true);
    anyhow::ensure!(
        !units.is_empty(),
        "PDF search query is empty after removing extraction artifacts"
    );

    let wrap = regex::escape(&WRAP_HYPHEN.to_string());
    let hyphen = regex::escape("-");
    let wrap_or_hyphen = format!("(?:{wrap}|{hyphen})");
    let optional_wrap = format!("(?:{wrap})?");
    let mut pattern = String::with_capacity(query.len().saturating_mul(2));

    for (index, unit) in units.iter().enumerate() {
        let between_letters = previous_is_letter(&units, index) && next_is_letter(&units, index);
        match unit.kind {
            NormalizedUnitKind::WrapHyphen if between_letters => {
                pattern.push_str(&wrap_or_hyphen);
            }
            NormalizedUnitKind::WrapHyphen => pattern.push_str(&wrap),
            NormalizedUnitKind::Char('-') if between_letters => {
                pattern.push_str(&wrap_or_hyphen);
            }
            NormalizedUnitKind::Char(c) => push_literal_pattern(&mut pattern, c),
        }

        if unit_is_letter(unit) && units.get(index + 1).is_some_and(unit_is_letter) {
            pattern.push_str(&optional_wrap);
        }
    }

    Ok(RegexMatcherBuilder::new()
        .case_insensitive(!case_sensitive)
        .build(&pattern)?)
}

fn push_literal_pattern(pattern: &mut String, c: char) {
    if matches!(c, WRAP_HYPHEN | LITERAL_ESCAPE) {
        pattern.push_str(&regex::escape(&LITERAL_ESCAPE.to_string()));
    }
    pattern.push_str(&regex::escape(&c.to_string()));
}

fn previous_is_letter(units: &[NormalizedUnit], index: usize) -> bool {
    index
        .checked_sub(1)
        .and_then(|previous| units.get(previous))
        .is_some_and(unit_is_letter)
}

fn next_is_letter(units: &[NormalizedUnit], index: usize) -> bool {
    units.get(index + 1).is_some_and(unit_is_letter)
}

fn unit_is_letter(unit: &NormalizedUnit) -> bool {
    matches!(unit.kind, NormalizedUnitKind::Char(c) if c.is_alphabetic())
}

fn normalize(input: &str, trim_outer_whitespace: bool) -> Vec<NormalizedUnit> {
    let chars = input.char_indices().collect::<Vec<_>>();
    let mut units = Vec::with_capacity(chars.len());
    let mut index = 0;

    while index < chars.len() {
        let (start, c) = chars[index];
        let end = char_end(input, &chars, index);

        if is_discretionary_hyphen(c) && index > 0 && chars[index - 1].1.is_alphabetic() {
            if let Some(continuation) = line_wrap_continuation(&chars, index) {
                push_unit(
                    &mut units,
                    NormalizedUnitKind::WrapHyphen,
                    ByteRange {
                        start,
                        end: chars[continuation].0,
                    },
                );
                index = continuation;
                continue;
            }

            if c == '\u{00ad}'
                && chars
                    .get(index + 1)
                    .is_some_and(|(_, next)| next.is_alphabetic())
            {
                push_unit(
                    &mut units,
                    NormalizedUnitKind::WrapHyphen,
                    ByteRange { start, end },
                );
                index += 1;
                continue;
            }
        }

        if c.is_whitespace() {
            let mut next = index + 1;
            while next < chars.len() && chars[next].1.is_whitespace() {
                next += 1;
            }
            let whitespace_end = if next < chars.len() {
                chars[next].0
            } else {
                input.len()
            };
            if !trim_outer_whitespace || !units.is_empty() {
                push_unit(
                    &mut units,
                    NormalizedUnitKind::Char(' '),
                    ByteRange {
                        start,
                        end: whitespace_end,
                    },
                );
            }
            index = next;
            continue;
        }

        if is_ignored_format_character(c) || c == '\u{00ad}' {
            index += 1;
            continue;
        }

        let raw_range = ByteRange { start, end };
        match c {
            '\u{fb00}' => push_chars(&mut units, "ff", raw_range),
            '\u{fb01}' => push_chars(&mut units, "fi", raw_range),
            '\u{fb02}' => push_chars(&mut units, "fl", raw_range),
            '\u{fb03}' => push_chars(&mut units, "ffi", raw_range),
            '\u{fb04}' => push_chars(&mut units, "ffl", raw_range),
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' | '\u{02bc}' => {
                push_unit(&mut units, NormalizedUnitKind::Char('\''), raw_range);
            }
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => {
                push_unit(&mut units, NormalizedUnitKind::Char('"'), raw_range);
            }
            '\u{2010}' | '\u{2011}' => {
                push_unit(&mut units, NormalizedUnitKind::Char('-'), raw_range);
            }
            _ => push_unit(&mut units, NormalizedUnitKind::Char(c), raw_range),
        }
        index += 1;
    }

    if trim_outer_whitespace
        && matches!(
            units.last(),
            Some(NormalizedUnit {
                kind: NormalizedUnitKind::Char(' '),
                ..
            })
        )
    {
        units.pop();
    }
    units
}

fn push_chars(units: &mut Vec<NormalizedUnit>, chars: &str, raw_range: ByteRange) {
    for c in chars.chars() {
        push_unit(units, NormalizedUnitKind::Char(c), raw_range.clone());
    }
}

fn push_unit(units: &mut Vec<NormalizedUnit>, kind: NormalizedUnitKind, raw_range: ByteRange) {
    if matches!(kind, NormalizedUnitKind::Char(' ')) {
        if let Some(NormalizedUnit {
            kind: NormalizedUnitKind::Char(' '),
            raw_range: previous,
        }) = units.last_mut()
        {
            previous.end = raw_range.end;
            return;
        }
    }
    units.push(NormalizedUnit { kind, raw_range });
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

fn is_ignored_format_character(c: char) -> bool {
    matches!(c, '\u{200b}' | '\u{2060}' | '\u{feff}')
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

    #[test]
    fn literal_passage_ignores_pdf_line_wrap_hyphenation_and_whitespace() {
        let raw = "The topic should be specific to your degree pro-\n  gramme. It should also be some-\r\nthing that interests you.";
        let query = "The topic should be specific to your degree programme.\nIt should also be something that interests you.";
        let ranges = matches(raw, query);

        assert_eq!(ranges.len(), 1);
        assert_eq!(&raw[ranges[0].start..ranges[0].end], raw);
    }

    #[test]
    fn genuine_inline_hyphen_is_not_optional() {
        assert!(matches("some-thing", "something").is_empty());
        assert_eq!(matches("some-thing", "some-thing").len(), 1);
        assert_eq!(matches("state-\nof-the-art", "state-of-the-art").len(), 1);
    }

    #[test]
    fn pasted_wrap_hyphen_query_matches_inline_or_wrapped_hyphen() {
        assert_eq!(matches("state-of-the-art", "state-\nof-the-art").len(), 1);
        assert_eq!(matches("state-\nof-the-art", "state-\nof-the-art").len(), 1);
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
        let raw = "Préface: \u{fb01}eld some-\nthing fin.";
        let ranges = matches(raw, "field something");
        assert_eq!(ranges.len(), 1);
        let range = &ranges[0];
        assert!(raw.is_char_boundary(range.start));
        assert!(raw.is_char_boundary(range.end));
        assert_eq!(&raw[range.start..range.end], "\u{fb01}eld some-\nthing");
    }

    #[test]
    fn escapes_literal_internal_projection_symbols_without_collisions() {
        let raw = format!("a{WRAP_HYPHEN}b a{LITERAL_ESCAPE}b");
        assert_eq!(matches(&raw, &format!("a{WRAP_HYPHEN}b")).len(), 1);
        assert_eq!(matches(&raw, &format!("a{LITERAL_ESCAPE}b")).len(), 1);
        assert!(matches(&raw, "ab").is_empty());
    }

    #[test]
    fn query_outer_whitespace_is_ignored() {
        assert_eq!(matches("before target after", " \n target\t").len(), 1);
    }

    #[test]
    fn case_sensitivity_is_applied_after_projection() {
        let projection = PdfSearchProjection::new("some-\nthing");
        let insensitive = literal_matcher("SOMETHING", false).unwrap();
        let sensitive = literal_matcher("SOMETHING", true).unwrap();

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
