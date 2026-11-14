//! The content stream of a rewritten page.
//!
//! Written in *page space* -- origin top-left, y downward -- because that is
//! the space the reader describes everything in. A PDF's own user space has
//! its origin wherever the page's boxes and rotation put it, so the stream
//! opens by undoing the page's transform; from there on a coordinate from the
//! reader can be written down as it is.

use std::fmt::Write as _;

use anyhow::bail;
use mupdf::Matrix;

use super::geometry::Area;
use super::TextChunk;

pub(super) struct Stream {
    out: String,
}

impl Stream {
    /// A stream whose coordinates are the page's own, for a page whose user
    /// space maps to page space by `page_ctm`.
    pub(super) fn in_page_space(page_ctm: &Matrix) -> anyhow::Result<Self> {
        let mut stream = Self { out: String::new() };
        stream.out.push_str("q\n");
        stream.concat(&invert(page_ctm)?);
        Ok(stream)
    }

    /// Draw a source page's form, moved by `(dx, dy)` and visible only inside
    /// `clip`. The form is in its source page's user space, which
    /// `source_ctm` takes to page space.
    pub(super) fn draw_form(&mut self, name: &str, clip: &[Area], dx: f32, dy: f32, source_ctm: &Matrix) {
        self.out.push_str("q\n");
        for area in clip {
            let _ = writeln!(
                self.out,
                "{} {} {} {} re",
                number(area.x),
                number(area.y),
                number(area.width),
                number(area.height)
            );
        }
        self.out.push_str("W n\n");
        self.concat(&Matrix::new(1.0, 0.0, 0.0, 1.0, dx, dy));
        self.concat(source_ctm);
        let _ = writeln!(self.out, "/{name} Do");
        self.out.push_str("Q\n");
    }

    /// Set a run of glyphs on a baseline. Page space runs downward and a
    /// font's glyphs stand upward, so the text matrix turns them back over.
    pub(super) fn show_text(&mut self, font: &str, chunk: &TextChunk, glyphs: &[u16]) {
        if glyphs.is_empty() {
            return;
        }
        let [red, green, blue] = chunk.color.map(|channel| number(f32::from(channel) / 255.0));
        let _ = writeln!(self.out, "BT\n/{font} 1 Tf\n{red} {green} {blue} rg");
        let _ = writeln!(
            self.out,
            "{size} 0 0 {flipped} {x} {y} Tm",
            size = number(chunk.size),
            flipped = number(-chunk.size),
            x = number(chunk.x),
            y = number(chunk.baseline),
        );
        self.out.push('<');
        for glyph in glyphs {
            let _ = write!(self.out, "{glyph:04X}");
        }
        self.out.push_str("> Tj\nET\n");
    }

    pub(super) fn finish(mut self) -> String {
        self.out.push_str("Q\n");
        self.out
    }

    fn concat(&mut self, matrix: &Matrix) {
        let _ = writeln!(
            self.out,
            "{} {} {} {} {} {} cm",
            number(matrix.a),
            number(matrix.b),
            number(matrix.c),
            number(matrix.d),
            number(matrix.e),
            number(matrix.f)
        );
    }
}

/// A number as a content stream takes it: no exponent, no trailing zeros.
fn number(value: f32) -> String {
    let rounded = format!("{value:.4}");
    let trimmed = rounded.trim_end_matches('0').trim_end_matches('.');
    if trimmed == "-0" || trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

fn invert(matrix: &Matrix) -> anyhow::Result<Matrix> {
    let determinant = matrix.a * matrix.d - matrix.b * matrix.c;
    if determinant.abs() < f32::EPSILON {
        bail!("a page's transform cannot be inverted");
    }
    let (a, b, c, d) = (
        matrix.d / determinant,
        -matrix.b / determinant,
        -matrix.c / determinant,
        matrix.a / determinant,
    );
    Ok(Matrix::new(
        a,
        b,
        c,
        d,
        -(matrix.e * a + matrix.f * c),
        -(matrix.e * b + matrix.f * d),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_without_noise() {
        assert_eq!(number(12.0), "12");
        assert_eq!(number(0.5), "0.5");
        assert_eq!(number(-0.00001), "0");
        assert_eq!(number(72.125), "72.125");
    }

    #[test]
    fn an_inverse_undoes_a_page_transform() {
        // An upright US Letter page: user space is y-up from the bottom-left.
        let page = Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 792.0);
        let inverse = invert(&page).unwrap();
        // The page-space point (72, 100) is the user-space point (72, 692).
        let x = 72.0 * inverse.a + 100.0 * inverse.c + inverse.e;
        let y = 72.0 * inverse.b + 100.0 * inverse.d + inverse.f;
        assert_eq!((x, y), (72.0, 692.0));
    }
}
