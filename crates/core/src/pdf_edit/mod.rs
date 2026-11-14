//! Writing a reader's edits into a copy of a PDF.
//!
//! **What arrives.** The reader edits a PDF on screen by moving areas of its
//! pages and setting new text among them, and it is the authority on where
//! everything went: it did the line breaking, and it laid the pages out. What
//! it hands over is that result and nothing else -- for every page that
//! changed, which areas of which source pages are drawn where, and which runs
//! of text sit at which baselines ([`EditExport`]). Nothing here lays anything
//! out, which is what keeps the saved file from disagreeing with the screen.
//!
//! **The invariant.** The saved file shows exactly what the editor showed, and
//! holds nothing the editor did not show: text the reader replaced is *gone*
//! from the file, not painted over or clipped out of sight, because a PDF's
//! text is read by search and by extraction whether or not it is visible. And
//! the source file is never written.
//!
//! **How.** A page's content stream is not understood, only reused. For each
//! group of areas that moved together, the source page is copied, everything
//! outside those areas is redacted from the copy -- which removes the text for
//! real -- and what is left becomes a Form XObject. The output page then draws
//! each such XObject once, shifted and clipped to its areas, and sets the new
//! text over the gaps. Figures, formulas, rules and shadings travel through
//! untouched, in the operators the file already had.
//!
//! Pages the edits did not reach are not touched at all.

mod content;
pub mod fonts;
mod geometry;

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context};
use mupdf::pdf::{PdfAnnotationType, PdfDocument, PdfObject, PdfPage, PdfWriteOptions};
use mupdf::{Buffer, Font, Matrix, Rect, Size};
use serde::Deserialize;
use tracing::{debug, warn};

pub use geometry::Area;

/// The edited document, as the pages that differ from the source.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditExport {
    pub pages: Vec<ExportPage>,
}

/// One page of the result. Everything is in the page's own space: points,
/// origin top-left, y downward -- the space a viewer draws the page in.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPage {
    /// 1-based. Past the source's last page, it is a page to append.
    pub page: u32,
    pub width: f32,
    pub height: f32,
    pub strips: Vec<Strip>,
    pub text: Vec<TextChunk>,
}

/// An area of a source page, drawn on this page.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Strip {
    /// 1-based source page.
    pub source: u32,
    /// Where the area is on the source page.
    pub rect: Area,
    /// Where its top-left corner goes on this page.
    pub x: f32,
    pub y: f32,
    /// Drawn without its text: what an edited paragraph stood on -- a tint, a
    /// border, a rule beside it -- with the words it used to hold taken out.
    #[serde(default)]
    pub graphics_only: bool,
}

/// A run of text on one baseline, in one font.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextChunk {
    pub x: f32,
    pub baseline: f32,
    pub text: String,
    /// A font id from [`fonts::resolve`].
    pub font: String,
    pub size: f32,
    pub color: [u8; 3],
}

/// How far a kept area is grown before what lies outside it is redacted.
///
/// MuPDF removes a glyph when its *font* box -- ascender to descender, taller
/// than the ink of any one line -- reaches into a redaction. An area is the
/// ink of its lines, so the font boxes of its own first and last lines stand
/// proud of it. Redacting right up to the area would take those lines with
/// it; grown by this much, it cannot. The lines beyond are still caught: their
/// boxes are a line tall, and this is less than a line.
const KEEP_MARGIN_X: f32 = 1.0;
const KEEP_MARGIN_Y: f32 = 5.0;
/// How far outside an area its clip is drawn, to keep the antialiased edge of
/// its ink. The same margins the reader draws with.
const CLIP_MARGIN_X: f32 = 1.0;
const CLIP_MARGIN_Y: f32 = 0.5;

/// Write `source` with `export` applied to `destination`.
pub fn write_edited_pdf(
    source: &Path,
    export: &EditExport,
    destination: &Path,
) -> anyhow::Result<()> {
    if same_file(source, destination) {
        bail!(
            "refusing to save edits over the document they were made from ({})",
            source.display()
        );
    }
    let source_str = source.to_str().ok_or_else(|| anyhow!("non-UTF-8 path"))?;
    let destination_str = destination
        .to_str()
        .ok_or_else(|| anyhow!("non-UTF-8 path"))?;

    let mut doc =
        PdfDocument::open(source_str).with_context(|| format!("opening {}", source.display()))?;
    let source_pages = doc.page_count()? as u32;
    validate(export, source_pages)?;

    // Read every source page a strip draws from before any page is rewritten:
    // a page that is both a source and a destination must be read as it was.
    let mut originals: HashMap<u32, Original> = HashMap::new();
    for strip in export.pages.iter().flat_map(|page| &page.strips) {
        if !originals.contains_key(&strip.source) {
            originals.insert(strip.source, Original::read(&mut doc, strip.source)?);
        }
    }

    for page in &export.pages {
        if page.page > source_pages {
            doc.new_page(Size::new(page.width, page.height))?;
        }
    }

    let mut fonts = FontTable::default();
    for page in &export.pages {
        write_page(&mut doc, page, &originals, &mut fonts)
            .with_context(|| format!("writing page {}", page.page))?;
    }

    let mut options = PdfWriteOptions::default();
    // Collect what the rewritten pages no longer reference: the original
    // content streams, and the scratch pages the redactions were done on.
    options.set_garbage_level(3).set_compress(true);
    doc.save_with_options(destination_str, options)
        .with_context(|| format!("saving {}", destination.display()))?;
    debug!(
        "pdf_edit: wrote {} changed page(s) of {} to {}",
        export.pages.len(),
        source.display(),
        destination.display()
    );
    Ok(())
}

/// The edited document as bytes, for a caller with nowhere of its own to put a
/// file -- a browser that will be handed it as a download.
pub fn edited_pdf_bytes(source: &Path, export: &EditExport) -> anyhow::Result<Vec<u8>> {
    let directory = tempfile::tempdir().context("creating a scratch directory")?;
    let destination = directory.path().join("edited.pdf");
    write_edited_pdf(source, export, &destination)?;
    std::fs::read(&destination).context("reading the edited document back")
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        // A destination that does not exist yet cannot be the source.
        _ => a == b,
    }
}

fn validate(export: &EditExport, source_pages: u32) -> anyhow::Result<()> {
    let mut expected_append = source_pages + 1;
    let mut previous = 0;
    for page in &export.pages {
        if page.page <= previous {
            bail!("export pages are not in ascending order at page {}", page.page);
        }
        previous = page.page;
        if page.page > source_pages {
            if page.page != expected_append {
                bail!(
                    "export appends page {} but the document would end at {}",
                    page.page,
                    expected_append - 1
                );
            }
            expected_append += 1;
        }
        for strip in &page.strips {
            if strip.source == 0 || strip.source > source_pages {
                bail!(
                    "page {} draws from page {}, which the document does not have",
                    page.page,
                    strip.source
                );
            }
        }
    }
    Ok(())
}

/// A source page as it was before anything was rewritten.
struct Original {
    contents: Vec<u8>,
    resources: PdfObject,
    /// Copied onto the scratch page a redaction is done on, so that page is
    /// the same shape as this one.
    boxes: Vec<(&'static str, PdfObject)>,
    /// PDF user space to page space.
    ctm: Matrix,
    bounds: Rect,
}

impl Original {
    fn read(doc: &mut PdfDocument, number: u32) -> anyhow::Result<Self> {
        let index = number as i32 - 1;
        let object = doc.find_page(index)?;
        let bounds = doc.load_page(index)?.bounds()?;

        let contents = match object.get_dict("Contents")? {
            None => Vec::new(),
            Some(contents) if contents.is_array()? => {
                let mut joined = Vec::new();
                for part in 0..contents.len()? {
                    if let Some(stream) = contents.get_array(part as i32)? {
                        joined.extend(stream.read_stream()?);
                        joined.push(b'\n');
                    }
                }
                joined
            }
            Some(contents) => contents.read_stream()?,
        };

        // Held by reference from every XObject cut from this page, so it has
        // to be an object of its own rather than a dictionary inside the page.
        let resources = match object.get_dict_inheritable("Resources")? {
            Some(resources) if resources.is_indirect()? => resources,
            Some(resources) => doc.add_object(&resources)?,
            None => doc.add_object(&doc.new_dict()?)?,
        };

        let mut boxes = Vec::new();
        for key in ["MediaBox", "CropBox", "Rotate", "UserUnit"] {
            if let Some(value) = object.get_dict_inheritable(key)? {
                boxes.push((key, value));
            }
        }

        Ok(Self {
            contents,
            resources,
            boxes,
            ctm: object.page_ctm()?,
            bounds,
        })
    }

    fn page_area(&self) -> Area {
        Area {
            x: self.bounds.x0,
            y: self.bounds.y0,
            width: self.bounds.x1 - self.bounds.x0,
            height: self.bounds.y1 - self.bounds.y0,
        }
    }
}

/// Strips of one source page that moved by the same amount: one XObject.
struct Group<'a> {
    source: u32,
    dx: f32,
    dy: f32,
    graphics_only: bool,
    strips: Vec<&'a Strip>,
}

fn groups(page: &ExportPage) -> Vec<Group<'_>> {
    let mut groups: Vec<Group> = Vec::new();
    for strip in &page.strips {
        let (dx, dy) = (strip.x - strip.rect.x, strip.y - strip.rect.y);
        // Offsets are sums of measured heights; two that differ by less than a
        // hundredth of a point are one move.
        let existing = groups.iter_mut().find(|group| {
            group.source == strip.source
                && group.graphics_only == strip.graphics_only
                && (group.dx - dx).abs() < 0.01
                && (group.dy - dy).abs() < 0.01
        });
        match existing {
            Some(group) => group.strips.push(strip),
            None => groups.push(Group {
                source: strip.source,
                dx,
                dy,
                graphics_only: strip.graphics_only,
                strips: vec![strip],
            }),
        }
    }
    // What a paragraph stood on goes down first, under everything drawn whole.
    groups.sort_by_key(|group| !group.graphics_only);
    groups
}

fn write_page(
    doc: &mut PdfDocument,
    page: &ExportPage,
    originals: &HashMap<u32, Original>,
    fonts: &mut FontTable,
) -> anyhow::Result<()> {
    let index = page.page as i32 - 1;
    let mut object = doc.find_page(index)?;
    let page_ctm = object.page_ctm()?;

    let mut stream = content::Stream::in_page_space(&page_ctm)?;
    let mut xobjects = doc.new_dict()?;
    for (number, group) in groups(page).iter().enumerate() {
        let original = originals
            .get(&group.source)
            .ok_or_else(|| anyhow!("source page {} was not read", group.source))?;
        let kept: Vec<Area> = group.strips.iter().map(|strip| strip.rect).collect();
        let form = cut_form(doc, original, &kept, group.graphics_only)
            .with_context(|| format!("cutting areas out of page {}", group.source))?;
        let name = format!("Wk{number}");
        xobjects.dict_put(name.as_str(), form)?;

        let clip: Vec<Area> = kept
            .iter()
            .map(|area| {
                area.expanded(CLIP_MARGIN_X, CLIP_MARGIN_Y)
                    .translated(group.dx, group.dy)
            })
            .collect();
        stream.draw_form(&name, &clip, group.dx, group.dy, &original.ctm);
    }

    let mut font_resources = doc.new_dict()?;
    for chunk in &page.text {
        let font = fonts.get(doc, &chunk.font)?;
        if font_resources.get_dict(font.name.as_str())?.is_none() {
            font_resources.dict_put(font.name.as_str(), font.object.try_clone()?)?;
        }
        let glyphs = font.glyphs(&chunk.text);
        stream.show_text(&font.name, chunk, &glyphs);
    }

    let mut resources = doc.new_dict()?;
    resources.dict_put("XObject", xobjects)?;
    resources.dict_put("Font", font_resources)?;
    let mut contents = doc.add_object(&doc.new_dict()?)?;
    contents.write_stream_buffer(&Buffer::from_bytes(stream.finish().as_bytes())?)?;
    object.dict_put("Contents", contents)?;
    object.dict_put("Resources", resources)?;
    Ok(())
}

/// A source page with everything but `kept` taken out, as a Form XObject in
/// the page's own user space. With `graphics_only`, the text inside `kept`
/// is taken out as well: redaction removes text and leaves what is drawn.
fn cut_form(
    doc: &mut PdfDocument,
    original: &Original,
    kept: &[Area],
    graphics_only: bool,
) -> anyhow::Result<PdfObject> {
    let page_area = original.page_area();
    let redactions = if graphics_only {
        vec![page_area]
    } else {
        let safe: Vec<Area> = kept
            .iter()
            .map(|area| area.expanded(KEEP_MARGIN_X, KEEP_MARGIN_Y))
            .collect();
        geometry::complement(page_area, &safe)
    };

    // A scratch page with its own copy of the content: redaction rewrites a
    // page's stream in place, and the source's must stay as it is for every
    // other group cut from it.
    let mut scratch: PdfPage = doc.new_page(Size::new(page_area.width, page_area.height))?;
    let mut scratch_object = scratch.object();
    for (key, value) in &original.boxes {
        scratch_object.dict_put(*key, value.try_clone()?)?;
    }
    let mut contents = doc.add_object(&doc.new_dict()?)?;
    contents.write_stream_buffer(&Buffer::from_bytes(&original.contents)?)?;
    scratch_object.dict_put("Contents", contents)?;
    scratch_object.dict_put("Resources", original.resources.try_clone()?)?;

    for area in &redactions {
        let mut annotation = scratch.create_annotation(PdfAnnotationType::Redact)?;
        annotation.set_rect(Rect::new(area.x, area.y, area.right(), area.bottom()))?;
    }
    if !redactions.is_empty() {
        scratch.redact()?;
    }

    let redacted = scratch_object
        .get_dict("Contents")?
        .ok_or_else(|| anyhow!("a redacted page has no content"))?
        .read_stream()?;
    let resources = scratch_object
        .get_dict("Resources")?
        .ok_or_else(|| anyhow!("a redacted page has no resources"))?;
    let media_box = scratch_object
        .get_dict_inheritable("MediaBox")?
        .ok_or_else(|| anyhow!("a page has no MediaBox"))?;
    drop(scratch);
    // The scratch page is the last one, and has done its work. What the form
    // still needs of it is held by reference; the rest is collected on save.
    let last = doc.page_count()? - 1;
    doc.delete_page(last)?;

    let mut form = doc.new_dict()?;
    form.dict_put("Type", PdfObject::new_name("XObject")?)?;
    form.dict_put("Subtype", PdfObject::new_name("Form")?)?;
    form.dict_put("BBox", media_box)?;
    form.dict_put("Resources", resources)?;
    let mut form = doc.add_object(&form)?;
    form.write_stream_buffer(&Buffer::from_bytes(&redacted)?)?;
    Ok(form)
}

/// The fonts already added to the document being written.
#[derive(Default)]
struct FontTable {
    fonts: HashMap<String, EmbeddedFont>,
}

struct EmbeddedFont {
    /// Its name in a page's `/Font` resources.
    name: String,
    object: PdfObject,
    font: Font,
}

impl FontTable {
    fn get(&mut self, doc: &mut PdfDocument, id: &str) -> anyhow::Result<&EmbeddedFont> {
        if !self.fonts.contains_key(id) {
            let resolved = fonts::load(id)?;
            let font = Font::from_bytes(id, &resolved.data)
                .with_context(|| format!("MuPDF could not read font {id}"))?;
            let object = doc.add_font(&font)?;
            let name = format!("WkF{}", self.fonts.len());
            self.fonts
                .insert(id.to_string(), EmbeddedFont { name, object, font });
        }
        Ok(&self.fonts[id])
    }
}

impl EmbeddedFont {
    /// The text as this font's glyph ids, which is how a font added this way
    /// is addressed.
    fn glyphs(&self, text: &str) -> Vec<u16> {
        text.chars()
            .map(|character| {
                let glyph = match self.font.encode_character(character as i32) {
                    Ok(glyph) => glyph,
                    Err(error) => {
                        warn!(
                            "pdf_edit: font {} could not be asked for {character:?}: {error}",
                            self.font.name()
                        );
                        0
                    }
                };
                if glyph == 0 && !character.is_whitespace() {
                    // Glyph 0 is the font's own "missing" box, which is at
                    // least an honest account of what happened.
                    warn!(
                        "pdf_edit: font {} has no glyph for {character:?}",
                        self.font.name()
                    );
                }
                glyph as u16
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
