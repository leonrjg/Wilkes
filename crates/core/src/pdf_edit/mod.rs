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
//! untouched, in the operators the file already had. What an edited paragraph
//! stood on is drawn from a second kind of copy: the page with the operators
//! that show text cut out of it ([`strip`]), so a photograph under the old
//! words is still there under the new ones.
//!
//! Pages the edits did not reach are not touched at all.

mod content;
mod destinations;
pub mod fonts;
mod geometry;
mod strip;
mod subset;

use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::{anyhow, bail, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
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
    /// Fonts the text is set in that are not installed fonts: the document's
    /// own, which the reader made typeable and sends with its file.
    #[serde(default)]
    pub fonts: Vec<ExportFont>,
}

/// A font that came with the edits.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFont {
    /// What the text's [`TextChunk::font`] calls it.
    pub id: String,
    /// The font file, base64.
    pub data: String,
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
    /// A font id from [`fonts::resolve`], or one of [`EditExport::fonts`].
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
        if let Entry::Vacant(unread) = originals.entry(strip.source) {
            unread.insert(Original::read(&mut doc, strip.source)?);
        }
    }

    for page in &export.pages {
        if page.page > source_pages {
            doc.new_page(Size::new(page.width, page.height))?;
        }
    }

    // Before the pages are rewritten: where an annotation goes is decided by
    // where the page it is on had it.
    let annotations = relocated_annotations(&doc, export, &originals, source_pages)?;

    let fonts = FontTable::embed(&mut doc, export)?;
    let mut papers: HashMap<u32, PdfObject> = HashMap::new();
    for page in &export.pages {
        write_page(&mut doc, page, &originals, &fonts, &mut papers)
            .with_context(|| format!("writing page {}", page.page))?;
    }
    attach_annotations(&mut doc, export, annotations)?;
    destinations::relocate(&mut doc, export)?;

    let mut options = PdfWriteOptions::default();
    // Collect what the rewritten pages no longer reference: the original
    // content streams, and the scratch pages the redactions were done on.
    // No further: the next level also looks for objects that are copies of
    // one another by comparing each with every other, which takes seconds on
    // a book and finds nothing this writer made.
    options
        .set_garbage_level(2)
        .set_compress(true)
        .set_compress_fonts(true);
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
            bail!(
                "export pages are not in ascending order at page {}",
                page.page
            );
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
    fonts: &FontTable,
    // Each source page without its text, made the first time one is wanted.
    papers: &mut HashMap<u32, PdfObject>,
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
        let form =
            if group.graphics_only {
                match papers.entry(group.source) {
                    Entry::Occupied(paper) => paper.get().try_clone()?,
                    Entry::Vacant(unmade) => unmade
                        .insert(paper_form(doc, original).with_context(|| {
                            format!("taking the text out of page {}", group.source)
                        })?)
                        .try_clone()?,
                }
            } else {
                cut_form(doc, original, &kept)
                    .with_context(|| format!("cutting areas out of page {}", group.source))?
            };
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
        let font = fonts.get(&chunk.font)?;
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

/// How deep forms inside forms are followed when text is taken out of a page.
/// Real documents nest two or three; a file that refers to itself would
/// otherwise never end.
const MAX_FORM_DEPTH: u8 = 12;

/// A source page with its text taken out and everything else as it was, as a
/// Form XObject in the page's own user space.
fn paper_form(doc: &mut PdfDocument, original: &Original) -> anyhow::Result<PdfObject> {
    let media_box = original
        .boxes
        .iter()
        .find(|(key, _)| *key == "MediaBox")
        .ok_or_else(|| anyhow!("a page has no MediaBox"))?
        .1
        .try_clone()?;
    let mut form = doc.new_dict()?;
    form.dict_put("Type", PdfObject::new_name("XObject")?)?;
    form.dict_put("Subtype", PdfObject::new_name("Form")?)?;
    form.dict_put("BBox", media_box)?;
    form.dict_put(
        "Resources",
        textless_resources(doc, &original.resources, &original.contents, 0)?,
    )?;
    let mut form = doc.add_object(&form)?;
    form.write_stream_buffer(&Buffer::from_bytes(&strip::without_text(
        &original.contents,
    )?)?)?;
    Ok(form)
}

/// `resources` with every form that `content` draws replaced by one without
/// text, and so on down through the forms those draw. The same object when
/// nothing drawn shows text. A form the resources list and the content does
/// not draw is left alone: it is not part of this page.
fn textless_resources(
    doc: &mut PdfDocument,
    resources: &PdfObject,
    content: &[u8],
    depth: u8,
) -> anyhow::Result<PdfObject> {
    let Some(xobjects) = resources.get_dict("XObject")? else {
        return resources.try_clone().map_err(Into::into);
    };
    let drawn = strip::drawn(content)?;
    let mut replaced: Vec<(PdfObject, PdfObject)> = Vec::new();
    for index in 0..xobjects.dict_len()? as i32 {
        let (Some(key), Some(xobject)) =
            (xobjects.get_dict_key(index)?, xobjects.get_dict_val(index)?)
        else {
            continue;
        };
        let is_form = match xobject.get_dict("Subtype")? {
            Some(subtype) => subtype.as_name()? == b"Form",
            None => false,
        };
        if !is_form || !drawn.contains(key.as_name()?) {
            continue;
        }
        if depth >= MAX_FORM_DEPTH {
            bail!("the page's forms are nested more than {MAX_FORM_DEPTH} deep");
        }
        let content = xobject.read_stream()?;
        let stripped = strip::without_text(&content)?;
        let inner = xobject.get_dict("Resources")?;
        let inner_stripped = match &inner {
            Some(inner) => Some(textless_resources(doc, inner, &content, depth + 1)?),
            None => None,
        };
        let same_resources = match (&inner, &inner_stripped) {
            (Some(was), Some(now)) => same_object(was, now)?,
            _ => true,
        };
        if stripped == content && same_resources {
            continue;
        }
        let mut copy = shallow_copy(doc, &xobject, &["Length", "Filter", "DecodeParms"])?;
        if let Some(inner_stripped) = inner_stripped {
            copy.dict_put("Resources", inner_stripped)?;
        }
        let mut copy = doc.add_object(&copy)?;
        copy.write_stream_buffer(&Buffer::from_bytes(&stripped)?)?;
        replaced.push((key, copy));
    }
    if replaced.is_empty() {
        return resources.try_clone().map_err(Into::into);
    }
    let mut new_xobjects = shallow_copy(doc, &xobjects, &[])?;
    for (key, form) in replaced {
        new_xobjects.dict_put(key, form)?;
    }
    let mut new_resources = shallow_copy(doc, resources, &[])?;
    new_resources.dict_put("XObject", new_xobjects)?;
    doc.add_object(&new_resources).map_err(Into::into)
}

/// A new dictionary with the entries of another, less the keys named.
fn shallow_copy(
    doc: &PdfDocument,
    dict: &PdfObject,
    without: &[&str],
) -> anyhow::Result<PdfObject> {
    let mut copy = doc.new_dict()?;
    for index in 0..dict.dict_len()? as i32 {
        let (Some(key), Some(value)) = (dict.get_dict_key(index)?, dict.get_dict_val(index)?)
        else {
            continue;
        };
        if without
            .iter()
            .any(|name| key.as_name().ok() == Some(name.as_bytes()))
        {
            continue;
        }
        copy.dict_put(key, value)?;
    }
    Ok(copy)
}

/// Whether two handles are to the same indirect object.
fn same_object(a: &PdfObject, b: &PdfObject) -> anyhow::Result<bool> {
    Ok(a.is_indirect()? && b.is_indirect()? && a.as_indirect()? == b.as_indirect()?)
}

/// A source page with everything but `kept` taken out, as a Form XObject in
/// the page's own user space.
fn cut_form(
    doc: &mut PdfDocument,
    original: &Original,
    kept: &[Area],
) -> anyhow::Result<PdfObject> {
    let page_area = original.page_area();
    let safe: Vec<Area> = kept
        .iter()
        .map(|area| area.expanded(KEEP_MARGIN_X, KEEP_MARGIN_Y))
        .collect();
    let redactions = geometry::complement(page_area, &safe);

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

/// An annotation with somewhere to be on a rewritten page.
struct Relocated {
    destination: u32,
    annotation: PdfObject,
    /// Its `/Rect` and `/QuadPoints` there, in that page's user space.
    rect: [f32; 4],
    quads: Option<Vec<f32>>,
}

/// Where the annotations of every rewritten page belong now.
///
/// A link is an area of a page, kept beside the content rather than in it, so
/// moving the content does not move the link: without this, a citation pushed
/// down two lines leaves its link where it was, over whatever is there now. An
/// annotation goes with the strip its centre is in -- to wherever that strip
/// went, on whichever page. One whose text was replaced is in no strip, and is
/// dropped: it was a link on words that are gone.
fn relocated_annotations(
    doc: &PdfDocument,
    export: &EditExport,
    originals: &HashMap<u32, Original>,
    source_pages: u32,
) -> anyhow::Result<Vec<Relocated>> {
    let mut relocated = Vec::new();
    for page in export.pages.iter().filter(|page| page.page <= source_pages) {
        let object = doc.find_page(page.page as i32 - 1)?;
        let Some(annotations) = object.get_dict("Annots")? else {
            continue;
        };
        let source_ctm = object.page_ctm()?;
        let mut dropped = 0;
        for index in 0..annotations.len()? {
            let Some(annotation) = annotations.get_array(index as i32)? else {
                continue;
            };
            let Some(rect) = numbers(&annotation, "Rect")?.filter(|rect| rect.len() == 4) else {
                dropped += 1;
                continue;
            };
            let (x0, y0) = content::apply(&source_ctm, rect[0], rect[1]);
            let (x1, y1) = content::apply(&source_ctm, rect[2], rect[3]);
            let centre = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);

            let landing = export.pages.iter().find_map(|destination| {
                destination
                    .strips
                    .iter()
                    .find(|strip| {
                        strip.source == page.page
                            && !strip.graphics_only
                            && strip.rect.contains(centre.0, centre.1)
                    })
                    .map(|strip| {
                        (
                            destination.page,
                            strip.x - strip.rect.x,
                            strip.y - strip.rect.y,
                        )
                    })
            });
            let Some((destination, dx, dy)) = landing else {
                dropped += 1;
                continue;
            };
            if !originals.contains_key(&page.page) {
                bail!("page {} has a strip but was not read", page.page);
            }

            // Through this page's space, moved, and back out through the
            // destination's: the two pages need not share an origin.
            let to_destination =
                content::invert(&doc.find_page(destination as i32 - 1)?.page_ctm()?)?;
            let moved = |x: f32, y: f32| {
                let (px, py) = content::apply(&source_ctm, x, y);
                content::apply(&to_destination, px + dx, py + dy)
            };
            let (ax, ay) = moved(rect[0], rect[1]);
            let (bx, by) = moved(rect[2], rect[3]);
            let quads = numbers(&annotation, "QuadPoints")?.map(|points| {
                points
                    .chunks_exact(2)
                    .flat_map(|point| {
                        let (x, y) = moved(point[0], point[1]);
                        [x, y]
                    })
                    .collect()
            });
            relocated.push(Relocated {
                destination,
                annotation,
                rect: [ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)],
                quads,
            });
        }
        if dropped > 0 {
            debug!(
                "pdf_edit: page {}: {dropped} annotation(s) sat on text that was replaced and were dropped",
                page.page
            );
        }
    }
    Ok(relocated)
}

/// Give every rewritten page the annotations that belong on it now.
fn attach_annotations(
    doc: &mut PdfDocument,
    export: &EditExport,
    relocated: Vec<Relocated>,
) -> anyhow::Result<()> {
    let mut by_page: HashMap<u32, Vec<Relocated>> = HashMap::new();
    for entry in relocated {
        by_page.entry(entry.destination).or_default().push(entry);
    }
    for page in &export.pages {
        let mut object = doc.find_page(page.page as i32 - 1)?;
        let mut list = doc.new_array()?;
        for mut entry in by_page.remove(&page.page).unwrap_or_default() {
            entry
                .annotation
                .dict_put("Rect", number_array(doc, &entry.rect)?)?;
            if let Some(quads) = &entry.quads {
                entry
                    .annotation
                    .dict_put("QuadPoints", number_array(doc, quads)?)?;
            }
            // The page it is on now, which need not be the one it was on.
            entry.annotation.dict_put("P", object.try_clone()?)?;
            list.array_push(entry.annotation)?;
        }
        object.dict_put("Annots", list)?;
    }
    Ok(())
}

fn numbers(object: &PdfObject, key: &str) -> anyhow::Result<Option<Vec<f32>>> {
    let Some(array) = object.get_dict(key)? else {
        return Ok(None);
    };
    if !array.is_array()? {
        return Ok(None);
    }
    let mut values = Vec::with_capacity(array.len()?);
    for index in 0..array.len()? {
        match array.get_array(index as i32)? {
            Some(value) => values.push(value.as_float()?),
            None => return Ok(None),
        }
    }
    Ok(Some(values))
}

fn number_array(doc: &PdfDocument, values: &[f32]) -> anyhow::Result<PdfObject> {
    let mut array = doc.new_array()?;
    for value in values {
        array.array_push(PdfObject::new_real(*value)?)?;
    }
    Ok(array)
}

/// The fonts the new text is set in, as embedded in the document being
/// written.
struct FontTable {
    fonts: HashMap<String, EmbeddedFont>,
}

struct EmbeddedFont {
    /// Its name in a page's `/Font` resources.
    name: String,
    object: PdfObject,
    /// The whole font, which is what knows the glyph for a character.
    font: Font,
}

impl FontTable {
    /// Embed every font the export's text uses, each cut down to the glyphs
    /// that text shows. All of them at once and before any page is written,
    /// because what a font has to hold is everything every page asks of it.
    fn embed(doc: &mut PdfDocument, export: &EditExport) -> anyhow::Result<Self> {
        let mut text_in: Vec<(&str, String)> = Vec::new();
        for chunk in export.pages.iter().flat_map(|page| &page.text) {
            match text_in.iter_mut().find(|(id, _)| *id == chunk.font) {
                Some((_, text)) => text.push_str(&chunk.text),
                None => text_in.push((&chunk.font, chunk.text.clone())),
            }
        }

        let mut fonts = HashMap::new();
        for (number, (id, text)) in text_in.into_iter().enumerate() {
            let data = match export.fonts.iter().find(|font| font.id == id) {
                Some(sent) => STANDARD
                    .decode(&sent.data)
                    .with_context(|| format!("font {id} came with the edits unreadable"))?,
                None => fonts::load(id)?.data,
            };
            // The name MuPDF gives the embedded font: a sent font's id says
            // where it came from in characters a PDF name would escape.
            let name: String = id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        c
                    } else {
                        '-'
                    }
                })
                .collect();
            let font = Font::from_bytes(&name, &data)
                .with_context(|| format!("MuPDF could not read font {id}"))?;
            let glyphs: BTreeSet<u16> = glyph_ids(&font, &text).into_iter().collect();
            let cut = subset::subset(&data, &glyphs)
                .with_context(|| format!("cutting font {id} down to what is used"))?;
            debug!(
                "pdf_edit: font {id}: {} glyph(s) used, {} of {} bytes embedded",
                glyphs.len(),
                cut.len(),
                data.len()
            );
            let embedded = Font::from_bytes(&name, &cut)
                .with_context(|| format!("MuPDF could not read the subset of font {id}"))?;
            let object = doc.add_font(&embedded)?;
            let name = format!("WkF{number}");
            fonts.insert(id.to_string(), EmbeddedFont { name, object, font });
        }
        Ok(Self { fonts })
    }

    fn get(&self, id: &str) -> anyhow::Result<&EmbeddedFont> {
        self.fonts
            .get(id)
            .ok_or_else(|| anyhow!("font {id} was not embedded"))
    }
}

/// Text as a font's glyph ids, which is how a font added this way is
/// addressed.
fn glyph_ids(font: &Font, text: &str) -> Vec<u16> {
    text.chars()
        .map(|character| {
            let glyph = match font.encode_character(character as i32) {
                Ok(glyph) => glyph,
                Err(error) => {
                    warn!(
                        "pdf_edit: font {} could not be asked for {character:?}: {error}",
                        font.name()
                    );
                    0
                }
            };
            if glyph == 0 && !character.is_whitespace() {
                // Glyph 0 is the font's own "missing" box, which is at
                // least an honest account of what happened.
                warn!(
                    "pdf_edit: font {} has no glyph for {character:?}",
                    font.name()
                );
            }
            glyph as u16
        })
        .collect()
}

impl EmbeddedFont {
    fn glyphs(&self, text: &str) -> Vec<u16> {
        glyph_ids(&self.font, text)
    }
}

#[cfg(test)]
mod tests;
