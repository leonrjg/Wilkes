//! Where the document's own pointers into its pages point now.
//!
//! A PDF refers to places in itself by page and coordinates: an outline entry,
//! the target of a cross-reference, a named destination. Those are kept beside
//! the pages rather than in them, so moving a page's content does not move
//! them -- a heading pushed onto the next page leaves its outline entry
//! pointing at the foot of the page it left. Here every such destination that
//! names a place on a rewritten page is pointed at wherever that place went.
//!
//! A place goes with the strip it is in. One that is in none -- in the gap
//! above a heading, where a destination usually is, or in text that was
//! replaced -- goes with the first strip under it, which is what a reader
//! following it would have seen at the top of the view.

use std::collections::{HashMap, HashSet};

use mupdf::pdf::{PdfDocument, PdfObject};

use super::content::{apply, invert};
use super::{EditExport, Strip};

/// How many outline entries or name-tree nodes are followed before a file is
/// taken to be referring to itself.
const MAX_NODES: usize = 1_000_000;

/// Point every destination into a rewritten page at where its target is now.
pub(super) fn relocate(doc: &mut PdfDocument, export: &EditExport) -> anyhow::Result<()> {
    let page_count = doc.page_count()?;
    let mut pages = HashMap::new();
    for index in 0..page_count {
        pages.insert(doc.find_page(index)?.as_indirect()?, index as u32 + 1);
    }
    let mut rewriter = Rewriter {
        doc,
        export,
        pages,
        seen: HashSet::new(),
    };

    let catalog = rewriter.doc.catalog()?;
    if let Some(outlines) = catalog.get_dict("Outlines")? {
        rewriter.outline_level(outlines.get_dict("First")?)?;
    }
    for index in 0..page_count {
        let page = rewriter.doc.find_page(index)?;
        let Some(annotations) = page.get_dict("Annots")? else {
            continue;
        };
        for at in 0..annotations.len()? as i32 {
            if let Some(annotation) = annotations.get_array(at)? {
                rewriter.pointer(&annotation)?;
            }
        }
    }
    // Named destinations: the old dictionary in the catalog, and the tree.
    if let Some(named) = catalog.get_dict("Dests")? {
        for index in 0..named.dict_len()? as i32 {
            if let Some(value) = named.get_dict_val(index)? {
                rewriter.named(value)?;
            }
        }
    }
    if let Some(tree) = catalog
        .get_dict("Names")?
        .map(|names| names.get_dict("Dests"))
    {
        rewriter.name_tree(tree?)?;
    }
    Ok(())
}

struct Rewriter<'a> {
    doc: &'a mut PdfDocument,
    export: &'a EditExport,
    /// Page number by the page object's number.
    pages: HashMap<i32, u32>,
    /// Objects already dealt with: a destination can be shared, and must be
    /// moved once.
    seen: HashSet<i32>,
}

impl Rewriter<'_> {
    /// Whether this object is met for the first time. One that is not an
    /// object of its own belongs to whoever holds it, and is met once.
    fn first_visit(&mut self, object: &PdfObject) -> anyhow::Result<bool> {
        Ok(!object.is_indirect()? || self.seen.insert(object.as_indirect()?))
    }

    fn outline_level(&mut self, first: Option<PdfObject>) -> anyhow::Result<()> {
        let mut next = first;
        let mut followed = 0;
        while let Some(item) = next {
            followed += 1;
            if followed > MAX_NODES || !self.first_visit(&item)? {
                break;
            }
            self.pointer(&item)?;
            self.outline_level(item.get_dict("First")?)?;
            next = item.get_dict("Next")?;
        }
        Ok(())
    }

    fn name_tree(&mut self, node: Option<PdfObject>) -> anyhow::Result<()> {
        let Some(node) = node else { return Ok(()) };
        if self.seen.len() > MAX_NODES || !self.first_visit(&node)? {
            return Ok(());
        }
        if let Some(names) = node.get_dict("Names")? {
            // Key, value, key, value.
            for index in (1..names.len()? as i32).step_by(2) {
                if let Some(value) = names.get_array(index)? {
                    self.named(value)?;
                }
            }
        }
        if let Some(kids) = node.get_dict("Kids")? {
            for index in 0..kids.len()? as i32 {
                self.name_tree(kids.get_array(index)?)?;
            }
        }
        Ok(())
    }

    /// A named destination's value: the destination, or a dictionary with it.
    fn named(&mut self, value: PdfObject) -> anyhow::Result<()> {
        if value.is_array()? {
            self.destination(value)
        } else if value.is_dict()? {
            match value.get_dict("D")? {
                Some(destination) => self.destination(destination),
                None => Ok(()),
            }
        } else {
            Ok(())
        }
    }

    /// Something that points somewhere -- an outline entry, a link -- by a
    /// destination of its own or by an action that goes to one.
    fn pointer(&mut self, holder: &PdfObject) -> anyhow::Result<()> {
        if let Some(destination) = holder.get_dict("Dest")? {
            self.destination(destination)?;
        }
        if let Some(action) = holder.get_dict("A")? {
            let goes_to = match action.get_dict("S")? {
                Some(kind) => kind.as_name()? == b"GoTo",
                None => false,
            };
            if let (true, Some(destination)) = (goes_to, action.get_dict("D")?) {
                self.destination(destination)?;
            }
        }
        Ok(())
    }

    /// `[page /XYZ left top zoom]` and its kin. A destination by name is a
    /// reference to one of these, which is rewritten where it is defined.
    fn destination(&mut self, mut destination: PdfObject) -> anyhow::Result<()> {
        if !destination.is_array()? || !self.first_visit(&destination)? {
            return Ok(());
        }
        let Some(page_ref) = destination.get_array(0)? else {
            return Ok(());
        };
        if !page_ref.is_indirect()? {
            return Ok(());
        }
        let Some(&source) = self.pages.get(&page_ref.as_indirect()?) else {
            return Ok(());
        };
        let Some(kind) = destination.get_array(1)? else {
            return Ok(());
        };
        // Which of the numbers after the kind are a left edge and a top edge.
        let (left_at, top_at) = match kind.as_name()? {
            b"XYZ" => (Some(2), 3),
            b"FitH" | b"FitBH" => (None, 2),
            b"FitR" => (Some(2), 5),
            _ => return Ok(()),
        };
        let number = |at: i32| -> anyhow::Result<Option<f32>> {
            match destination.get_array(at)? {
                Some(value) if value.is_number()? => Ok(Some(value.as_float()?)),
                _ => Ok(None),
            }
        };
        let Some(top) = number(top_at)? else {
            return Ok(());
        };
        let left = match left_at {
            Some(at) => number(at)?,
            None => None,
        };

        let source_ctm = self.doc.find_page(source as i32 - 1)?.page_ctm()?;
        let (x, y) = apply(&source_ctm, left.unwrap_or(0.0), top);
        let Some((page, dx, dy)) = moved(self.export, source, left.map(|_| x), y) else {
            return Ok(());
        };
        let page_object = self.doc.find_page(page as i32 - 1)?;
        let to_user = invert(&page_object.page_ctm()?)?;
        let (new_left, new_top) = apply(&to_user, x + dx, y + dy);
        destination.array_put(top_at, PdfObject::new_real(new_top)?)?;
        if let (Some(at), Some(_)) = (left_at, left) {
            destination.array_put(at, PdfObject::new_real(new_left)?)?;
        }
        if page != source {
            destination.array_put(0, page_object)?;
        }
        Ok(())
    }
}

/// Where a point of a source page went: the page it is on now and how far it
/// moved, in page space. `None` when the page was not rewritten, or nothing
/// at or under the point is drawn anywhere.
fn moved(export: &EditExport, source: u32, x: Option<f32>, y: f32) -> Option<(u32, f32, f32)> {
    let strips = || {
        export.pages.iter().flat_map(move |page| {
            page.strips
                .iter()
                .filter(move |strip| strip.source == source && !strip.graphics_only)
                .map(move |strip| (page.page, strip))
        })
    };
    let beside = |strip: &Strip| match x {
        Some(x) if x < strip.rect.x => strip.rect.x - x,
        Some(x) if x > strip.rect.right() => x - strip.rect.right(),
        _ => 0.0,
    };
    let level = |strip: &Strip| y >= strip.rect.y && y <= strip.rect.bottom();
    // The strip the point is in, or the one beside it at the same height;
    // failing that, the first strip under it.
    let chosen = strips()
        .filter(|(_, strip)| level(strip))
        .min_by(|a, b| beside(a.1).total_cmp(&beside(b.1)))
        .or_else(|| {
            strips()
                .filter(|(_, strip)| strip.rect.y > y)
                .min_by(|a, b| {
                    (a.1.rect.y, beside(a.1))
                        .partial_cmp(&(b.1.rect.y, beside(b.1)))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        })?;
    let (page, strip) = chosen;
    Some((page, strip.x - strip.rect.x, strip.y - strip.rect.y))
}
