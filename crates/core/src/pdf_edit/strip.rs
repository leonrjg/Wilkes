//! A page's content with its text taken out.
//!
//! What an edited paragraph stood on -- a tint, a border, a photograph -- is
//! drawn again under the paragraph's new words, and must come without the old
//! ones. Redaction would remove them, but it removes by area: every image
//! under the area is blanked along with the text over it. Here the text is
//! removed by *kind*: the operators that show text are cut out of the content
//! stream, and everything else is left exactly as it was written.
//!
//! The stream is not rewritten, only shortened. It is read far enough to know
//! where each operation starts and ends, and the ones that show text are left
//! out of the copy.

use anyhow::bail;

/// The operators that put glyphs on the page. With these gone the rest of a
/// text object -- its font, its position -- sets up for nothing.
const SHOWS_TEXT: [&[u8]; 4] = [b"Tj", b"TJ", b"'", b"\""];

/// `content` without the operations that show text.
pub(super) fn without_text(content: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(content.len());
    let mut at = 0;
    // Where the operation being read started: after the last operator.
    let mut operation = 0;
    while at < content.len() {
        let byte = content[at];
        match byte {
            b'%' => at = line_end(content, at),
            b'(' => at = literal_string_end(content, at)?,
            b'<' if content.get(at + 1) == Some(&b'<') => at += 2,
            b'<' => at = hex_string_end(content, at)?,
            b'>' if content.get(at + 1) == Some(&b'>') => at += 2,
            b'[' | b']' | b'{' | b'}' | b'>' => at += 1,
            b'/' => at = token_end(content, at + 1),
            _ if is_space(byte) => at += 1,
            _ => {
                let end = token_end(content, at);
                let token = &content[at..end];
                at = end;
                if is_operand(token) {
                    continue;
                }
                if token == b"BI" {
                    // An inline image: its data is bytes, not tokens.
                    at = inline_image_end(content, at)?;
                }
                if !SHOWS_TEXT.contains(&token) {
                    out.extend_from_slice(&content[operation..at]);
                } else {
                    // Keep the separation the operation gave its neighbours.
                    out.push(b'\n');
                }
                operation = at;
            }
        }
    }
    out.extend_from_slice(&content[operation..]);
    Ok(out)
}

fn is_space(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Where a name, a number or an operator ends.
fn token_end(content: &[u8], from: usize) -> usize {
    let mut at = from;
    while at < content.len() && !is_space(content[at]) && !is_delimiter(content[at]) {
        at += 1;
    }
    // A lone delimiter the caller did not expect must still be stepped over.
    at.max(from + usize::from(from < content.len() && at == from))
}

/// Numbers and the three keywords that are values, not operators.
fn is_operand(token: &[u8]) -> bool {
    matches!(token, b"true" | b"false" | b"null")
        || token
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.'))
}

fn line_end(content: &[u8], from: usize) -> usize {
    content[from..]
        .iter()
        .position(|byte| matches!(byte, b'\n' | b'\r'))
        .map_or(content.len(), |offset| from + offset)
}

/// Past the `)` that closes the string opening at `from`. Parentheses nest,
/// and a backslash takes the next byte with it.
fn literal_string_end(content: &[u8], from: usize) -> anyhow::Result<usize> {
    let mut depth = 0usize;
    let mut at = from;
    while at < content.len() {
        match content[at] {
            b'\\' => at += 1,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(at + 1);
                }
            }
            _ => {}
        }
        at += 1;
    }
    bail!("a string in the page's content is never closed")
}

fn hex_string_end(content: &[u8], from: usize) -> anyhow::Result<usize> {
    match content[from..].iter().position(|byte| *byte == b'>') {
        Some(offset) => Ok(from + offset + 1),
        None => bail!("a hex string in the page's content is never closed"),
    }
}

/// Past the `EI` that ends the inline image whose `BI` was just read. The
/// image's bytes begin after `ID` and can hold anything, so its end is the
/// first `EI` standing as a word of its own.
fn inline_image_end(content: &[u8], from: usize) -> anyhow::Result<usize> {
    let word = |at: usize, text: &[u8; 2]| {
        content.get(at..at + 2) == Some(text.as_slice())
            && at
                .checked_sub(1)
                .is_none_or(|before| is_space(content[before]))
            && content.get(at + 2).is_none_or(|after| is_space(*after))
    };
    let Some(data) = (from..content.len()).find(|at| word(*at, b"ID")) else {
        bail!("an inline image in the page's content has no data");
    };
    match (data + 3..content.len()).find(|at| word(*at, b"EI")) {
        Some(end) => Ok(end + 2),
        None => bail!("an inline image in the page's content is never ended"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stripped(content: &str) -> String {
        String::from_utf8(without_text(content.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn text_is_taken_out_and_drawing_is_left() {
        let content =
            "q 1 0 0 rg 10 10 100 20 re f Q\nBT /F1 12 Tf 10 15 Td (Hello) Tj ET\n0 0 m 5 5 l S";
        let out = stripped(content);
        assert!(out.contains("10 10 100 20 re f"));
        assert!(out.contains("0 0 m 5 5 l S"));
        assert!(!out.contains("Hello"));
        // What sets the text up stays; it is harmless with nothing to show.
        assert!(out.contains("/F1 12 Tf"));
    }

    #[test]
    fn every_way_of_showing_text_is_taken_out() {
        let out = stripped("BT [(A) -20 <4142>] TJ (B) ' 1 2 (C) \" ET /Im1 Do");
        for gone in ["(A)", "4142", "(B)", "(C)", "TJ"] {
            assert!(!out.contains(gone), "{gone} is still in {out:?}");
        }
        assert!(out.contains("/Im1 Do"));
    }

    #[test]
    fn a_string_that_looks_like_an_operator_is_only_a_string() {
        let out = stripped("BT (a \\) Tj ( nested ) Tj) Tj ET 1 w");
        assert!(!out.contains("nested"));
        assert!(out.trim_end().ends_with("ET 1 w"));
    }

    #[test]
    fn an_inline_image_goes_through_whole() {
        let content = "BI /W 2 /H 1 /BPC 8 /CS /G ID \x01Tj EI\nBT (gone) Tj ET";
        let out = stripped(content);
        assert!(out.contains("ID \x01Tj EI"));
        assert!(!out.contains("gone"));
    }

    #[test]
    fn marked_content_properties_are_not_mistaken_for_strings() {
        let out = stripped("/Span <</ActualText (x)>> BDC BT (y) Tj ET EMC 2 w");
        assert!(out.contains("/Span <</ActualText (x)>> BDC"));
        assert!(out.contains("EMC 2 w"));
        assert!(!out.contains("(y)"));
    }
}
