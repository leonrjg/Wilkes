//! Outline (table-of-contents) reading for documents that declare one in
//! their text rather than in a bookmark tree.
//!
//! Markdown ATX headings only — `# Title` through `######`. Setext underlines
//! and HTML headings are not read, because a heading Wilkes is unsure about is
//! worse than one it never claimed: consumers segment documents by these
//! offsets, so a false heading cuts a section in the wrong place and every
//! position downstream of it is wrong too.

use crate::types::OutlineEntry;

/// Markdown ATX headings, in document order, with their byte offsets.
///
/// The offset is the start of the heading line, so a section runs from its own
/// heading to the next one — which is what a reader means by "this section".
pub fn markdown_outline(text: &str) -> Vec<OutlineEntry> {
    let mut entries = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if let Some(entry) = heading(line, offset) {
            entries.push(entry);
        }
        offset += line.len();
    }
    entries
}

/// One line as a heading, or `None`.
///
/// Indentation of four or more spaces is a code block in every Markdown
/// dialect, and a `#` inside one is a comment in whatever language the block
/// holds — the single most common way a scanner invents headings that are not
/// there.
fn heading(line: &str, offset: usize) -> Option<OutlineEntry> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent >= 4 {
        return None;
    }
    let rest = line.trim_start_matches(' ');
    let hashes = rest.len() - rest.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    // `#text` is not a heading in CommonMark — the space is required, and
    // without this check every `#include` and `#!/bin/sh` becomes a section.
    let title = rest[hashes..].strip_prefix(' ')?;
    let title = title.trim().trim_end_matches('#').trim();
    if title.is_empty() {
        return None;
    }
    Some(OutlineEntry {
        title: title.to_string(),
        level: hashes as u32 - 1,
        page: None,
        byte_offset: Some(offset),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_atx_headings_with_their_offsets_and_depth() {
        let text = "intro line\n# Chapter one\nbody\n## Section 1.1 ##\nmore\n";
        let outline = markdown_outline(text);
        assert_eq!(outline.len(), 2);

        assert_eq!(outline[0].title, "Chapter one");
        assert_eq!(outline[0].level, 0);
        assert_eq!(outline[0].byte_offset, Some(11));
        assert_eq!(outline[0].page, None);

        // Closing hashes are decoration, not part of the title.
        assert_eq!(outline[1].title, "Section 1.1");
        assert_eq!(outline[1].level, 1);
        assert_eq!(outline[1].byte_offset, Some(text.find("## Section").unwrap()));
    }

    /// Each of these produced a section boundary in a document that has none.
    #[test]
    fn refuses_the_lines_that_only_look_like_headings() {
        for line in [
            "#include <stdio.h>",
            "#!/bin/sh",
            "    # indented four spaces is a code block",
            "####### seven hashes is not a heading",
            "#",
            "## ",
            "text # mid-line hash",
        ] {
            assert!(
                markdown_outline(&format!("{line}\n")).is_empty(),
                "{line:?} was read as a heading"
            );
        }
    }

    #[test]
    fn a_document_without_headings_has_an_empty_outline() {
        assert!(markdown_outline("just prose\nand more prose\n").is_empty());
        assert!(markdown_outline("").is_empty());
    }
}
