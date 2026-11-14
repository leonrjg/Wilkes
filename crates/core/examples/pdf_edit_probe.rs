//! Drive the PDF edit writer from the command line.
//!
//! The reader hands a host two things: requests for whole fonts, and the
//! edited document as an export. This runs either through the same code the
//! application uses, without the application:
//!
//! ```text
//! pdf_edit_probe font '{"name":"Georgia","bold":false,"italic":false,"generic":"serif"}' out.ttf
//! pdf_edit_probe write source.pdf export.json edited.pdf
//! ```
//!
//! `font` prints the id the font resolved to and writes its file. `write`
//! applies an export saved as JSON.

use std::path::Path;

use anyhow::{bail, Context};
use wilkes_core::pdf_edit::{self, fonts, EditExport};

fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["font", request, output] => {
            let request: fonts::FontRequest =
                serde_json::from_str(request).context("parsing the font request")?;
            let font = fonts::resolve(&request)?;
            std::fs::write(output, &font.data).with_context(|| format!("writing {output}"))?;
            println!("{}", font.id);
            Ok(())
        }
        ["write", source, export, destination] => {
            let export: EditExport = serde_json::from_str(
                &std::fs::read_to_string(export).with_context(|| format!("reading {export}"))?,
            )
            .context("parsing the export")?;
            pdf_edit::write_edited_pdf(Path::new(source), &export, Path::new(destination))?;
            println!(
                "wrote {destination}: {} changed page(s)",
                export.pages.len()
            );
            Ok(())
        }
        _ => bail!(
            "usage: pdf_edit_probe font <request json> <out> | write <source> <export json> <dest>"
        ),
    }
}
