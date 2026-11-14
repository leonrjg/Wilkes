//! A font cut down to the glyphs a document uses.
//!
//! Edited text is set in whole system fonts, and a whole font is hundreds of
//! kilobytes -- several times the size of a typical paper, for a sentence of
//! changes. The saved file needs the outlines of the glyphs it shows and no
//! others.
//!
//! This is the plainest subset there is: every glyph keeps its number, and
//! the ones nobody uses keep their place with nothing in it. Nothing that
//! refers to a glyph by number -- the character map, the widths, the content
//! streams already written -- has to change. Tables a PDF viewer never
//! consults (kerning, substitution, variations) are left out.
//!
//! Only TrueType outlines are cut. A font with PostScript outlines is one
//! opaque table to this module, and is embedded whole.

use std::collections::BTreeSet;

use anyhow::{anyhow, bail};

/// The tables a PDF viewer draws a TrueType font from. Anything else in the
/// file is for laying text out, which was done before it got here.
const KEPT_TABLES: [&[u8; 4]; 15] = [
    b"OS/2", b"cmap", b"cvt ", b"fpgm", b"gasp", b"glyf", b"head", b"hhea", b"hmtx", b"loca",
    b"maxp", b"name", b"post", b"prep", b"vhea",
];

// Composite glyph flags.
const ARGS_ARE_WORDS: u16 = 0x0001;
const HAS_SCALE: u16 = 0x0008;
const MORE_COMPONENTS: u16 = 0x0020;
const HAS_XY_SCALE: u16 = 0x0040;
const HAS_TWO_BY_TWO: u16 = 0x0080;

struct Table<'a> {
    tag: [u8; 4],
    data: &'a [u8],
}

/// `font` with outlines only for `glyphs` and the glyphs those are built
/// from. A font this cannot cut is returned as it is.
pub(super) fn subset(font: &[u8], glyphs: &BTreeSet<u16>) -> anyhow::Result<Vec<u8>> {
    let version = bytes(font, 0, 4)?;
    if version != [0, 1, 0, 0] && version != b"true" {
        return Ok(font.to_vec());
    }
    let tables = read_tables(font)?;
    let find = |tag: &[u8; 4]| {
        tables
            .iter()
            .find(|table| &table.tag == tag)
            .map(|t| t.data)
    };
    let (Some(head), Some(maxp), Some(loca), Some(glyf)) =
        (find(b"head"), find(b"maxp"), find(b"loca"), find(b"glyf"))
    else {
        return Ok(font.to_vec());
    };

    let glyph_count = usize::from(u16_at(maxp, 4)?);
    let long_offsets = u16_at(head, 50)? != 0;
    let offset_of = |glyph: usize| -> anyhow::Result<usize> {
        Ok(if long_offsets {
            u32_at(loca, glyph * 4)? as usize
        } else {
            usize::from(u16_at(loca, glyph * 2)?) * 2
        })
    };
    let outline = |glyph: usize| -> anyhow::Result<&[u8]> {
        let (start, end) = (offset_of(glyph)?, offset_of(glyph + 1)?);
        if end < start {
            bail!("glyph {glyph} ends before it starts");
        }
        bytes(glyf, start, end - start)
    };

    // The glyphs asked for, the "missing" glyph, and whatever those are
    // composed of: an accented letter is its letter and its accent.
    let mut kept: BTreeSet<usize> = BTreeSet::from([0]);
    let mut pending: Vec<usize> = glyphs.iter().map(|glyph| usize::from(*glyph)).collect();
    while let Some(glyph) = pending.pop() {
        if glyph >= glyph_count || !kept.insert(glyph) && glyph != 0 {
            continue;
        }
        pending.extend(components(outline(glyph)?)?);
    }
    pending.extend(components(outline(0)?)?);
    while let Some(glyph) = pending.pop() {
        if glyph < glyph_count && kept.insert(glyph) {
            pending.extend(components(outline(glyph)?)?);
        }
    }

    let mut new_glyf = Vec::new();
    let mut new_loca = Vec::with_capacity((glyph_count + 1) * 4);
    for glyph in 0..glyph_count {
        new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
        if kept.contains(&glyph) {
            new_glyf.extend_from_slice(outline(glyph)?);
            while new_glyf.len() % 4 != 0 {
                new_glyf.push(0);
            }
        }
    }
    new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());

    // Offsets are written long whatever the font had: head says which.
    let mut new_head = head.to_vec();
    new_head
        .get_mut(50..52)
        .ok_or_else(|| anyhow!("font header is truncated"))?
        .copy_from_slice(&1u16.to_be_bytes());

    let mut out_tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for table in &tables {
        if !KEPT_TABLES.contains(&&table.tag) {
            continue;
        }
        let data = match &table.tag {
            b"glyf" => new_glyf.clone(),
            b"loca" => new_loca.clone(),
            b"head" => new_head.clone(),
            b"post" => without_glyph_names(table.data)?,
            _ => table.data.to_vec(),
        };
        out_tables.push((table.tag, data));
    }
    Ok(write_font(version, out_tables))
}

/// A `post` table that names no glyphs: its header, saying so. The names are
/// most of the table, and nothing drawing a page asks a glyph its name.
fn without_glyph_names(post: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut header = bytes(post, 0, 32)?.to_vec();
    header[0..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());
    Ok(header)
}

/// The glyphs a composite glyph is made of. None for a simple one.
fn components(outline: &[u8]) -> anyhow::Result<Vec<usize>> {
    // An empty outline is a glyph with nothing to draw: a space.
    if outline.is_empty() || (u16_at(outline, 0)? as i16) >= 0 {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    let mut at = 10;
    loop {
        let flags = u16_at(outline, at)?;
        found.push(usize::from(u16_at(outline, at + 2)?));
        at += 4 + if flags & ARGS_ARE_WORDS != 0 { 4 } else { 2 };
        at += if flags & HAS_SCALE != 0 {
            2
        } else if flags & HAS_XY_SCALE != 0 {
            4
        } else if flags & HAS_TWO_BY_TWO != 0 {
            8
        } else {
            0
        };
        if flags & MORE_COMPONENTS == 0 {
            return Ok(found);
        }
    }
}

fn read_tables(font: &[u8]) -> anyhow::Result<Vec<Table<'_>>> {
    let count = usize::from(u16_at(font, 4)?);
    (0..count)
        .map(|index| {
            let record = 12 + 16 * index;
            let tag = bytes(font, record, 4)?;
            let offset = u32_at(font, record + 8)? as usize;
            let length = u32_at(font, record + 12)? as usize;
            Ok(Table {
                tag: [tag[0], tag[1], tag[2], tag[3]],
                data: bytes(font, offset, length)?,
            })
        })
        .collect()
}

fn write_font(version: &[u8], mut tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    // The directory is searched by tag, so it is in tag order.
    tables.sort_by_key(|(tag, _)| *tag);
    let count = tables.len() as u16;
    let entry_selector = 15 - count.max(1).leading_zeros() as u16;
    let search_range: u16 = 16 * (1 << entry_selector);
    let mut out = Vec::new();
    out.extend_from_slice(version);
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&(count * 16 - search_range).to_be_bytes());

    let mut offset = 12 + 16 * tables.len();
    for (tag, data) in &tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(data).to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        offset += data.len().next_multiple_of(4);
    }
    for (_, data) in &tables {
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    out
}

fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, chunk| {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

fn bytes(data: &[u8], at: usize, length: usize) -> anyhow::Result<&[u8]> {
    at.checked_add(length)
        .and_then(|end| data.get(at..end))
        .ok_or_else(|| anyhow!("font file is truncated"))
}

fn u16_at(data: &[u8], at: usize) -> anyhow::Result<u16> {
    let two = bytes(data, at, 2)?;
    Ok(u16::from_be_bytes([two[0], two[1]]))
}

fn u32_at(data: &[u8], at: usize) -> anyhow::Result<u32> {
    let four = bytes(data, at, 4)?;
    Ok(u32::from_be_bytes([four[0], four[1], four[2], four[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TrueType font of four glyphs: 0 is the missing glyph, 1 and 2 are
    /// simple, 3 is composed of 2. With a table no viewer needs.
    fn font() -> Vec<u8> {
        let simple = |fill: u8| {
            let mut glyph = vec![0, 1]; // one contour
            glyph.extend_from_slice(&[fill; 10]);
            glyph
        };
        let mut composite = vec![0xFF, 0xFF]; // -1 contours
        composite.extend_from_slice(&[0; 8]); // bounding box
        composite.extend_from_slice(&0u16.to_be_bytes()); // flags: byte args, last
        composite.extend_from_slice(&2u16.to_be_bytes()); // glyph 2
        composite.extend_from_slice(&[0, 0]); // args

        let glyphs = [simple(0xA0), simple(0xA1), simple(0xA2), composite];
        let mut glyf = Vec::new();
        let mut loca = Vec::new();
        for glyph in &glyphs {
            loca.extend_from_slice(&((glyf.len() / 2) as u16).to_be_bytes());
            glyf.extend_from_slice(glyph);
        }
        loca.extend_from_slice(&((glyf.len() / 2) as u16).to_be_bytes());

        let mut head = vec![0; 54];
        head[50..52].copy_from_slice(&0u16.to_be_bytes()); // short offsets
        let mut maxp = vec![0; 6];
        maxp[4..6].copy_from_slice(&4u16.to_be_bytes());

        write_font(
            &[0, 1, 0, 0],
            vec![
                (*b"head", head),
                (*b"maxp", maxp),
                (*b"loca", loca),
                (*b"glyf", glyf),
                (*b"GSUB", vec![7; 400]),
            ],
        )
    }

    fn outlines(font: &[u8]) -> Vec<usize> {
        let tables = read_tables(font).unwrap();
        let loca = tables.iter().find(|t| &t.tag == b"loca").unwrap().data;
        (0..4)
            .map(|glyph| {
                (u32_at(loca, glyph * 4 + 4).unwrap() - u32_at(loca, glyph * 4).unwrap()) as usize
            })
            .collect()
    }

    #[test]
    fn unused_glyphs_keep_their_numbers_and_lose_their_outlines() {
        let cut = subset(&font(), &BTreeSet::from([1])).unwrap();
        assert_eq!(outlines(&cut), vec![12, 12, 0, 0]);
    }

    #[test]
    fn a_composite_glyph_keeps_what_it_is_made_of() {
        let cut = subset(&font(), &BTreeSet::from([3])).unwrap();
        assert_eq!(outlines(&cut), vec![12, 0, 12, 16]);
    }

    #[test]
    fn tables_a_viewer_does_not_read_are_left_out() {
        let cut = subset(&font(), &BTreeSet::from([1])).unwrap();
        let tags: Vec<[u8; 4]> = read_tables(&cut).unwrap().iter().map(|t| t.tag).collect();
        assert_eq!(tags, vec![*b"glyf", *b"head", *b"loca", *b"maxp"]);
        assert!(cut.len() < font().len());
    }

    #[test]
    fn a_font_with_other_outlines_is_left_whole() {
        let mut other = font();
        other[0..4].copy_from_slice(b"OTTO");
        assert_eq!(subset(&other, &BTreeSet::from([1])).unwrap(), other);
    }
}
