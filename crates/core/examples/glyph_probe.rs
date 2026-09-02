//! What the text layer actually says, codepoint by codepoint.
//!
//! Extraction can only ever carry what MuPDF names. A glyph the font maps to
//! nothing comes back as U+FFFD or as nothing at all, and no amount of reading
//! the page's geometry recovers it — the character is not there to be read.
//! This prints a line as codepoints so the difference between "flattened" and
//! "absent" is visible.
//!
//!     cargo run --release --example glyph_probe -- <pdf> <page> [substring]

use mupdf::{Document, TextPageFlags};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let pdf = args.next().expect("usage: <pdf> <page> [substring]");
    let number: i32 = args.next().expect("a page").parse()?;
    let needle = args.next();

    let document = Document::open(std::path::Path::new(&pdf))?;
    let page = document.load_page(number - 1)?;
    let text_page = page.to_text_page(TextPageFlags::ACCURATE_BBOXES)?;
    for block in text_page.blocks() {
        for line in block.lines() {
            // Every char MuPDF reports, including the ones whose code is not a
            // Unicode scalar — which is exactly what a glyph the font maps to
            // nothing comes back as, and what extraction drops today.
            let raw = line.chars().count();
            let chars: Vec<(char, f32, f32)> = line
                .chars()
                .filter_map(|ch| Some((ch.char()?, ch.origin().x, ch.size())))
                .collect();
            let unnamed: Vec<f32> = line
                .chars()
                .filter(|ch| ch.char().is_none())
                .map(|ch| ch.origin().x)
                .collect();
            let text: String = chars.iter().map(|(c, ..)| *c).collect();
            if let Some(needle) = &needle {
                if !text.contains(needle.as_str()) {
                    continue;
                }
            }
            println!(
                "\n{text:?}\n  {raw} chars reported, {} named, {} unnamed{}",
                chars.len(),
                unnamed.len(),
                if unnamed.is_empty() {
                    String::new()
                } else {
                    format!(" at x {unnamed:.1?}")
                }
            );
            for (c, x, size) in &chars {
                let name = match c {
                    '\u{fffd}' => "  ← REPLACEMENT: the font maps this glyph to nothing",
                    c if c.is_whitespace() => "  (space)",
                    _ => "",
                };
                println!("  U+{:04X} {c:?} x{x:7.1} {size:5.1}{name}", *c as u32);
            }
        }
    }
    Ok(())
}
