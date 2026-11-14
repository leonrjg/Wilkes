//! The fonts edited text is set in.
//!
//! A PDF embeds the fonts it uses, but almost always as subsets: the glyphs
//! the document happened to need and no others. Text a reader types needs
//! glyphs the subset may not have, so edited text is set in a whole font
//! instead -- the same one on screen and in the saved file, or the two would
//! break lines differently.
//!
//! The font is the system's: the document's own face if it is installed,
//! otherwise the platform's serif, sans-serif or monospace at the same weight
//! and slant. That is a substitution, and would be a visible one in a document
//! set in a face nobody has installed -- so the reader does not set text in it
//! where it can help it. Where the document's own subset has a character, the
//! reader sets it in that, made typeable, and sends the file with the edits
//! ([`super::EditExport::fonts`]); the font resolved here is what stands in
//! for the characters the subset never had.

use std::sync::Arc;

use anyhow::{anyhow, Context};
use font_kit::family_name::FamilyName;
use font_kit::handle::Handle;
use font_kit::properties::{Properties, Style, Weight};
use font_kit::source::SystemSource;
use serde::Deserialize;
use tracing::debug;

/// A font as a document names it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontRequest {
    /// The font's PostScript name, without a subset's prefix. May be empty.
    pub name: String,
    pub bold: bool,
    pub italic: bool,
    pub generic: GenericFamily,
    /// Set when the weight and slant asked for are not the named font's own
    /// but another face of its family: a reader made a word bold. The name
    /// then says which family, not which font.
    #[serde(default)]
    pub sibling: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GenericFamily {
    Serif,
    SansSerif,
    Monospace,
}

/// A whole font, ready for a browser and for MuPDF alike.
pub struct ResolvedFont {
    /// The font's PostScript name. [`load`] takes it back.
    pub id: String,
    /// A standalone font file: never a collection.
    pub data: Vec<u8>,
}

/// The installed font nearest to the one a document asks for.
pub fn resolve(request: &FontRequest) -> anyhow::Result<ResolvedFont> {
    let source = SystemSource::new();
    let named = if request.name.is_empty() {
        None
    } else {
        source.select_by_postscript_name(&request.name).ok()
    };
    // The document's own font, when that is what is asked for and it is here.
    if let (Some(handle), false) = (&named, request.sibling) {
        return from_handle(handle);
    }

    let mut properties = Properties::new();
    properties
        .weight(if request.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if request.italic {
            Style::Italic
        } else {
            Style::Normal
        });
    let generic = match request.generic {
        GenericFamily::Serif => FamilyName::Serif,
        GenericFamily::SansSerif => FamilyName::SansSerif,
        GenericFamily::Monospace => FamilyName::Monospace,
    };
    // The family the named font says it belongs to, when it is installed.
    // Otherwise guesses from its name, which cost nothing to try before the
    // generic family.
    let known: Vec<String> = match named.as_ref().and_then(|handle| handle.load().ok()) {
        Some(font) => vec![font.family_name()],
        None => family_guesses(&request.name),
    };
    let families: Vec<FamilyName> = known
        .into_iter()
        .map(FamilyName::Title)
        .chain([generic])
        .collect();
    let handle = source
        .select_best_match(&families, &properties)
        .map_err(|error| {
            anyhow!(
                "no installed font can stand in for {:?}: {error}",
                request.name
            )
        })?;
    let resolved = from_handle(&handle)?;
    if named.is_none() {
        debug!(
            "pdf_edit: {:?} is not installed; setting it in {}",
            request.name, resolved.id
        );
    }
    Ok(resolved)
}

/// The families a font's name may belong to, likeliest first. What precedes
/// the separator is the family as often as not -- `Times-Bold`,
/// `Arial,Italic` -- as written, or with the words it runs together set
/// apart: `GillSans-ExtraBold` is a face of "Gill Sans".
fn family_guesses(name: &str) -> Vec<String> {
    let Some(stem) = name
        .split(['-', ','])
        .next()
        .filter(|stem| !stem.is_empty())
    else {
        return Vec::new();
    };
    let mut spaced = String::with_capacity(stem.len() + 4);
    let mut previous: Option<char> = None;
    for character in stem.chars() {
        if character.is_uppercase() && previous.is_some_and(char::is_lowercase) {
            spaced.push(' ');
        }
        spaced.push(character);
        previous = Some(character);
    }
    if spaced == stem {
        vec![stem.to_string()]
    } else {
        vec![stem.to_string(), spaced]
    }
}

/// A font [`resolve`] returned earlier, by its id.
pub fn load(id: &str) -> anyhow::Result<ResolvedFont> {
    let handle = SystemSource::new()
        .select_by_postscript_name(id)
        .map_err(|error| anyhow!("font {id} is no longer installed: {error}"))?;
    from_handle(&handle)
}

fn from_handle(handle: &Handle) -> anyhow::Result<ResolvedFont> {
    let font = handle
        .load()
        .map_err(|error| anyhow!("could not load a system font: {error:?}"))?;
    let id = font
        .postscript_name()
        .ok_or_else(|| anyhow!("a system font has no PostScript name to know it by"))?;
    let (bytes, index): (Arc<Vec<u8>>, u32) = match handle {
        Handle::Path { path, font_index } => (
            Arc::new(
                std::fs::read(path)
                    .with_context(|| format!("reading font file {}", path.display()))?,
            ),
            *font_index,
        ),
        Handle::Memory { bytes, font_index } => (bytes.clone(), *font_index),
    };
    let data = standalone(&bytes, index).with_context(|| format!("unpacking font {id}"))?;
    Ok(ResolvedFont { id, data })
}

/// One font as a file of its own.
///
/// Many system fonts ship as collections -- several faces in one file, sharing
/// tables -- and a browser's `FontFace` cannot be pointed at one face of a
/// collection. A face is a table directory plus the tables it names, so it is
/// copied out: the directory rewritten with new offsets, the tables as they
/// were. Their checksums are over their own bytes and stay valid.
fn standalone(data: &[u8], index: u32) -> anyhow::Result<Vec<u8>> {
    if data.get(0..4) != Some(b"ttcf") {
        return Ok(data.to_vec());
    }
    let count = read_u32(data, 8)?;
    if index >= count {
        return Err(anyhow!(
            "collection has {count} fonts, not one numbered {index}"
        ));
    }
    let directory = read_u32(data, 12 + 4 * index as usize)? as usize;
    let table_count = read_u16(data, directory + 4)? as usize;
    let header_len = 12 + 16 * table_count;
    let header = data
        .get(directory..directory + header_len)
        .ok_or_else(|| anyhow!("font directory runs past the end of the file"))?;

    let mut out = header.to_vec();
    for table in 0..table_count {
        let record = 12 + 16 * table;
        let offset = read_u32(header, record + 8)? as usize;
        let length = read_u32(header, record + 12)? as usize;
        let bytes = data
            .get(offset..offset + length)
            .ok_or_else(|| anyhow!("font table runs past the end of the file"))?;
        let new_offset = out.len() as u32;
        out[record + 8..record + 12].copy_from_slice(&new_offset.to_be_bytes());
        out.extend_from_slice(bytes);
        // Tables start on four-byte boundaries.
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    Ok(out)
}

fn read_u32(data: &[u8], at: usize) -> anyhow::Result<u32> {
    let bytes = data
        .get(at..at + 4)
        .ok_or_else(|| anyhow!("font file is truncated"))?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u16(data: &[u8], at: usize) -> anyhow::Result<u16> {
    let bytes = data
        .get(at..at + 2)
        .ok_or_else(|| anyhow!("font file is truncated"))?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A collection of two fonts that share one table and differ in another.
    fn collection() -> Vec<u8> {
        let table_a = b"AAAAAAA"; // 7 bytes: exercises padding
        let table_b = b"BBBB";
        let table_c = b"CCCCCCCC";
        let header_len = 12 + 2 * 4;
        let directory_len = 12 + 16 * 2;
        let tables_at = header_len + 2 * directory_len;
        let (a_at, b_at, c_at) = (tables_at, tables_at + 8, tables_at + 12);

        let mut data = Vec::new();
        data.extend_from_slice(b"ttcf");
        data.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        data.extend_from_slice(&2u32.to_be_bytes());
        data.extend_from_slice(&(header_len as u32).to_be_bytes());
        data.extend_from_slice(&((header_len + directory_len) as u32).to_be_bytes());
        for second in [
            (b"bbbb", b_at, table_b.len()),
            (b"cccc", c_at, table_c.len()),
        ] {
            data.extend_from_slice(&0x0001_0000u32.to_be_bytes());
            data.extend_from_slice(&2u16.to_be_bytes());
            data.extend_from_slice(&[0; 6]);
            for (tag, at, len) in [(b"aaaa", a_at, table_a.len()), second] {
                data.extend_from_slice(tag);
                data.extend_from_slice(&0u32.to_be_bytes());
                data.extend_from_slice(&(at as u32).to_be_bytes());
                data.extend_from_slice(&(len as u32).to_be_bytes());
            }
        }
        data.extend_from_slice(table_a);
        data.push(0);
        data.extend_from_slice(table_b);
        data.extend_from_slice(table_c);
        data
    }

    fn table(font: &[u8], tag: &[u8; 4]) -> Vec<u8> {
        let count = read_u16(font, 4).unwrap() as usize;
        for index in 0..count {
            let record = 12 + 16 * index;
            if &font[record..record + 4] == tag {
                let offset = read_u32(font, record + 8).unwrap() as usize;
                let length = read_u32(font, record + 12).unwrap() as usize;
                return font[offset..offset + length].to_vec();
            }
        }
        panic!("no table {tag:?}");
    }

    #[test]
    fn a_plain_font_file_is_already_standalone() {
        let font = b"\x00\x01\x00\x00rest of a font".to_vec();
        assert_eq!(standalone(&font, 0).unwrap(), font);
    }

    #[test]
    fn a_face_is_copied_out_of_a_collection_with_its_own_tables() {
        let data = collection();
        let second = standalone(&data, 1).unwrap();
        assert_eq!(&second[0..4], &0x0001_0000u32.to_be_bytes());
        assert_eq!(table(&second, b"aaaa"), b"AAAAAAA");
        assert_eq!(table(&second, b"cccc"), b"CCCCCCCC");
        // Every table starts on a four-byte boundary.
        assert_eq!(read_u32(&second, 12 + 16 + 8).unwrap() % 4, 0);
    }

    #[test]
    fn a_face_the_collection_does_not_have_is_an_error() {
        assert!(standalone(&collection(), 2).is_err());
    }

    #[test]
    fn a_family_is_guessed_from_what_precedes_the_style() {
        assert_eq!(
            family_guesses("GillSans-ExtraBold"),
            ["GillSans", "Gill Sans"]
        );
        assert_eq!(family_guesses("Arial,Italic"), ["Arial"]);
        assert_eq!(
            family_guesses("SourceSansPro-Regular"),
            ["SourceSansPro", "Source Sans Pro"]
        );
        assert!(family_guesses("").is_empty());
    }

    #[test]
    fn a_generic_request_resolves_to_an_installed_font_that_loads_again() {
        let request = FontRequest {
            name: "NoSuchFontAnywhere-Regular".to_string(),
            bold: false,
            italic: false,
            generic: GenericFamily::Serif,
            sibling: false,
        };
        let resolved = resolve(&request).expect("a system has a serif font");
        assert!(resolved.data.get(0..4) != Some(b"ttcf"));
        let again = load(&resolved.id).expect("its id names it");
        assert_eq!(again.data, resolved.data);
    }

    #[test]
    fn a_sibling_request_resolves_to_another_face_of_the_same_family() {
        let regular = FontRequest {
            name: String::new(),
            bold: false,
            italic: false,
            generic: GenericFamily::Serif,
            sibling: false,
        };
        let own = resolve(&regular).expect("a system has a serif font");
        // The same font by name, asked for in bold.
        let bold = resolve(&FontRequest {
            name: own.id.clone(),
            bold: true,
            sibling: true,
            ..regular.clone()
        })
        .expect("a face of the same family");
        let family_of = |id: &str| {
            SystemSource::new()
                .select_by_postscript_name(id)
                .unwrap()
                .load()
                .unwrap()
                .family_name()
        };
        assert_eq!(family_of(&bold.id), family_of(&own.id));
        // Without `sibling` the name wins, whatever weight is asked for.
        let named = resolve(&FontRequest {
            name: own.id.clone(),
            bold: true,
            ..regular
        })
        .unwrap();
        assert_eq!(named.id, own.id);
    }
}
