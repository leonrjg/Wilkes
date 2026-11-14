//! Searchable text mapped back to the bytes it was read from.
//!
//! One mechanism, two producers. A PDF's stored reading is projected for
//! literal search: whitespace collapsed, ligatures expanded, quotes folded
//! (`search::pdf_projection`). An HTML file is projected for everything that
//! reads it: only the text the viewer shows, with every character still
//! pointing at the bytes of the file it came from (`extract::html`). Both are
//! the same thing — a string, and for every scalar in it the raw range that
//! produced it — and both answer the same question: which raw bytes does this
//! piece of projected text stand for.

use crate::types::ByteRange;

#[derive(Clone, Debug)]
struct ProjectionSpan {
    projected_range: ByteRange,
    raw_range: ByteRange,
}

/// A string read out of some raw text, and for every scalar of it the raw
/// byte range it stands for.
#[derive(Clone, Debug, Default)]
pub(crate) struct TextProjection {
    text: String,
    spans: Vec<ProjectionSpan>,
}

impl TextProjection {
    pub(crate) fn with_capacity(bytes: usize) -> Self {
        Self {
            text: String::with_capacity(bytes),
            spans: Vec::with_capacity(bytes),
        }
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    pub(crate) fn into_text(self) -> String {
        self.text
    }

    /// The last scalar pushed, if any.
    pub(crate) fn last_char(&self) -> Option<char> {
        self.text.chars().next_back()
    }

    /// Map a non-empty range of projected bytes to the smallest raw range
    /// containing every scalar that contributed to it.
    pub(crate) fn raw_range(&self, projected: ByteRange) -> Option<ByteRange> {
        // Spans are in projected order, so the first overlapping one is found
        // by bisection rather than by walking a 3 MB file's worth of them.
        let first = self
            .spans
            .partition_point(|span| span.projected_range.end <= projected.start);
        let mut overlapping = self.spans[first..]
            .iter()
            .take_while(|span| span.projected_range.start < projected.end);
        let first = overlapping.next()?;
        let mut start = first.raw_range.start;
        let mut end = first.raw_range.end;
        for span in overlapping {
            start = start.min(span.raw_range.start);
            end = end.max(span.raw_range.end);
        }
        Some(ByteRange { start, end })
    }

    /// The projected offset of the first scalar read from at or after `raw`,
    /// or the projection's end when nothing after it was projected.
    pub(crate) fn projected_offset(&self, raw: usize) -> usize {
        self.spans
            .iter()
            .find(|span| span.raw_range.end > raw)
            .map_or(self.text.len(), |span| span.projected_range.start)
    }

    /// Every scalar's projected and raw range, in projected order.
    pub(crate) fn ranges(&self) -> impl Iterator<Item = (&ByteRange, &ByteRange)> {
        self.spans
            .iter()
            .map(|span| (&span.projected_range, &span.raw_range))
    }

    pub(crate) fn push_scalar(&mut self, c: char, raw_range: ByteRange) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_maps_to_the_raw_bytes_of_every_scalar_inside_it() {
        let mut projection = TextProjection::default();
        projection.push_scalar('a', ByteRange { start: 3, end: 4 });
        projection.push_scalar('é', ByteRange { start: 10, end: 18 });
        projection.push_scalar('b', ByteRange { start: 20, end: 21 });

        assert_eq!(projection.as_str(), "aéb");
        assert_eq!(
            projection.raw_range(ByteRange { start: 1, end: 3 }),
            Some(ByteRange { start: 10, end: 18 })
        );
        assert_eq!(
            projection.raw_range(ByteRange { start: 0, end: 4 }),
            Some(ByteRange { start: 3, end: 21 })
        );
        assert_eq!(projection.raw_range(ByteRange { start: 4, end: 9 }), None);
        assert_eq!(projection.projected_offset(5), 1);
        assert_eq!(projection.projected_offset(19), 3);
        assert_eq!(projection.projected_offset(30), 4);
    }
}
