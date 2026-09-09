//! Which files the MuPDF backend is allowed to read, and on what terms.
//!
//! MuPDF is linked here with its whole document-handler set registered — PDF,
//! EPUB, MOBI, FB2, XPS, CBZ, images, SVG, HTML, plain text and the OOXML
//! office formats. Wilkes admits a closed list of those rather than asking
//! `fz_recognize_document`, and the difference is not a nicety.
//!
//! **Why an allowlist.** MuPDF's FB2 handler claims `.xml`, its CBZ handler
//! claims `.zip` and `.tar`, its text handler claims `.txt` and `.log`, and
//! its HTML handler claims `.html`. Wilkes already reads every one of those as
//! [`crate::types::FileType::PlainText`], with exact line and column origins —
//! which is a better reading of a text file than pagination, and is what every
//! stored bookmark and every indexed chunk in every existing library is
//! addressed by. A backend that answered "yes, I recognize this" for them
//! would move each one from `plain-text-v1` to a paginated recipe, which
//! forces a full re-extraction and re-embedding of the corpus and turns every
//! `TextFile { line, col }` locator into a page. Silently.
//!
//! So a format joins this list only where MuPDF is the *only* reader Wilkes
//! has for it, and only after it has been measured — see
//! `crates/core/examples/probe_epub.rs`, which is what admitted the four
//! below. XPS and the office formats are deliberately absent: MuPDF will open
//! them and nobody has looked at what comes out.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The page a reflowable document is laid out onto.
///
/// **These three numbers are part of the extraction recipe.** A reflowable
/// document has no pagination of its own — `Document::layout` invents it, and
/// every page number and bounding box the index stores descends from exactly
/// these values. MuPDF's own defaults are *not* these (they paginate a 198-page
/// book at 228 and a 442-page one at 516), so a reading that relied on them
/// would be silently repaginated by a MuPDF point release, with no recipe
/// change to force re-extraction and nothing in the log. Changing them here
/// changes [`reflowable_recipe_suffix`] and re-extracts, which is the point.
pub const LAYOUT_WIDTH: f32 = 450.0;
pub const LAYOUT_HEIGHT: f32 = 600.0;
pub const LAYOUT_EM: f32 = 11.0;

/// A format the MuPDF backend reads, and the recipe identity of its reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagedFormat {
    /// Paginated by the file itself. The only one that is.
    Pdf,
    Epub,
    /// MOBI 6 and the PalmDB containers that carry it. Also where a Kindle
    /// KF8 file arrives, and is refused — see [`guard_container`].
    Mobi,
    Fb2,
    /// A comic archive: a zip of page images, with no text of its own.
    Cbz,
    /// A Kindle KF8 book, read by rebuilding it — see [`super::kindle`].
    Kf8,
}

impl PagedFormat {
    /// The format this path is admitted as, or `None` for a file this backend
    /// must not touch.
    ///
    /// By extension rather than by content, because the question being asked
    /// is "may Wilkes hand this to MuPDF", and that is a decision about the
    /// corpus rather than a fact about the bytes. Content still gets the last
    /// word: [`guard_container`] refuses a file whose insides are not what its
    /// name claimed.
    pub fn for_path(path: &Path) -> Option<Self> {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())?
            .to_ascii_lowercase();
        Some(match extension.as_str() {
            "pdf" => Self::Pdf,
            "epub" => Self::Epub,
            // `azw3` and `azw` are admitted so they can be refused *loudly*.
            // MuPDF opens a KF8 file, reports success, and returns a document
            // of one empty page; a file left out of this list would instead be
            // omitted as an unsupported extension and say nothing at all. See
            // `guard_container`, which is what stops the empty reading.
            // Kindle's two formats share a container and are told apart by
            // the header, not the name — see `guard_container`. The extension
            // decides only which is expected, because a recipe has to be
            // derivable without opening the file.
            "mobi" | "prc" | "pdb" => Self::Mobi,
            "azw3" | "azw" => Self::Kf8,
            "fb2" => Self::Fb2,
            // `cbz` and `cbt` only. MuPDF's comic handler also claims `.zip`
            // and `.tar`, and a corpus's archives are not documents.
            "cbz" | "cbt" => Self::Cbz,
            _ => return None,
        })
    }

    /// Whether MuPDF must be told what page to lay this out onto.
    ///
    /// Asked of the format rather than of the open document because the recipe
    /// has to be derivable without opening anything — a caller computes it for
    /// a path that need not exist. The backend still asks the document itself
    /// (`Document::is_reflowable`) before laying it out; these two must agree,
    /// and the test below is what holds them together.
    pub fn is_reflowable(self) -> bool {
        match self {
            Self::Pdf => false,
            Self::Epub | Self::Mobi | Self::Fb2 | Self::Kf8 => true,
            // Each page is an image with its own size; there is nothing to
            // reflow and MuPDF reports it as such.
            Self::Cbz => false,
        }
    }

    /// This format's contribution to [`crate::embed::ExtractionRecipe`].
    ///
    /// `pdf-mupdf-v1` is unchanged and must stay so: every PDF in every
    /// existing index was extracted under that exact string, and a different
    /// one would re-extract and re-embed every library in existence for a
    /// change that did not touch how PDFs are read.
    pub fn recipe_identity(self) -> String {
        match self {
            Self::Pdf => "pdf-mupdf-v1".to_string(),
            Self::Epub => format!("epub-mupdf-v1+{}", reflowable_recipe_suffix()),
            Self::Mobi => format!("mobi-mupdf-v1+{}", reflowable_recipe_suffix()),
            Self::Fb2 => format!("fb2-mupdf-v1+{}", reflowable_recipe_suffix()),
            Self::Cbz => "cbz-mupdf-v1".to_string(),
            // Its own identity because it is its own pipeline: rebuilt by
            // `kindle`, then laid out by MuPDF as HTML.
            Self::Kf8 => format!("kf8-rebuilt-v1+{}", reflowable_recipe_suffix()),
        }
    }

    /// This format's name in the saved-filter language (`file_type == "epub"`).
    ///
    /// Per format rather than one word for all of them, because `file_type ==
    /// "pdf"` is an expression users have already saved and it meant PDFs.
    /// Widening it to every book the day EPUB support landed would quietly
    /// change what their collections contain.
    pub fn query_name(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Epub => "epub",
            Self::Mobi => "mobi",
            Self::Fb2 => "fb2",
            Self::Cbz => "cbz",
            Self::Kf8 => "azw3",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Epub => "application/epub+zip",
            Self::Mobi => "application/x-mobipocket-ebook",
            Self::Fb2 => "application/x-fictionbook",
            Self::Cbz => "application/vnd.comicbook+zip",
            Self::Kf8 => "application/vnd.amazon.ebook",
        }
    }
}

/// The layout geometry, as it appears in a recipe.
pub fn reflowable_recipe_suffix() -> String {
    format!("layout-{LAYOUT_WIDTH}x{LAYOUT_HEIGHT}x{LAYOUT_EM}")
}

/// Refuse a container MuPDF would open and misread.
///
/// **The case this exists for.** A Kindle KF8 file (`.azw3`, and `.mobi` files
/// that are secretly KF8) is a PalmDB container whose text lives in KF8
/// records. MuPDF's reader walks MOBI 6 records, finds none, and returns —
/// without error — a document of one page holding zero characters. Two such
/// books were measured: a Spanish trade paperback and a HarperCollins title,
/// both `file_version=8`, both silently empty, one reporting its title as
/// "Table of Contents".
///
/// An empty reading is the worst possible failure here, because Wilkes has a
/// legitimate category for it: a scanned PDF with no text layer yields nothing
/// too. Without this guard a shelf of Kindle books would index as content-free
/// and nothing anywhere would say why. Refusing produces a `Failed` verdict
/// naming KF8, which is visible in the activity view and retryable if real
/// support ever lands.
///
/// The compression word alone does *not* catch it — both the working MOBI 6
/// and the failing KF8 file declare PalmDOC. The MOBI header's file-version
/// field does: 6 for what MuPDF reads, 8 for what it cannot.
pub fn guard_container(path: &Path, format: PagedFormat) -> anyhow::Result<()> {
    if !matches!(format, PagedFormat::Mobi | PagedFormat::Kf8) {
        return Ok(());
    }
    let header = palmdb_header(path)?;
    let Some(header) = header else {
        // Not a Mobipocket container at all. MuPDF would refuse it too, but it
        // would refuse it after opening, which for these formats is where the
        // silent readings come from.
        anyhow::bail!(
            "{} is named as a Kindle book but is not a Mobipocket container",
            path.display()
        );
    };
    // MuPDF decodes only these two and sends everything else through the
    // PalmDOC decompressor regardless, which returns plausible bytes rather
    // than an error. The `mobi` crate is no better placed to guess.
    anyhow::ensure!(
        header.compression == 1 || header.compression == 2,
        "{} uses compression {}, which no reader here can decode \
         (only uncompressed and PalmDOC are supported; 17480 is HUFF/CDIC)",
        path.display(),
        header.compression
    );
    Ok(())
}

/// The MOBI header's file-version field, or `None` for a container that has no
/// MOBI header (a plain PalmDOC book) or is not one at all.
///
/// This is what tells KF8 from MOBI 6, and it is the only thing that does: both
/// declare PalmDOC compression, so the field one would reach for first is the
/// wrong one.
pub fn mobi_file_version(path: &Path) -> anyhow::Result<Option<u32>> {
    Ok(palmdb_header(path)?.and_then(|header| header.version))
}

struct PalmDbHeader {
    compression: u16,
    version: Option<u32>,
}

fn palmdb_header(path: &Path) -> anyhow::Result<Option<PalmDbHeader>> {
    let mut file = std::fs::File::open(path)?;
    let mut header = [0u8; 86];
    if file.read_exact(&mut header).is_err() {
        return Ok(None);
    }
    let kind = &header[60..68];
    if kind != b"BOOKMOBI" && kind != b"TEXtREAd" {
        return Ok(None);
    }
    let record0 = u32::from_be_bytes([header[78], header[79], header[80], header[81]]) as u64;

    let mut record = [0u8; 40];
    file.seek(SeekFrom::Start(record0))?;
    file.read_exact(&mut record)
        .map_err(|error| anyhow::anyhow!("{} has no readable record 0: {error}", path.display()))?;

    Ok(Some(PalmDbHeader {
        compression: u16::from_be_bytes([record[0], record[1]]),
        // The MOBI header follows the 16-byte PalmDOC header.
        version: (&record[16..20] == b"MOBI")
            .then(|| u32::from_be_bytes([record[36], record[37], record[38], record[39]])),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The extensions Wilkes already reads as plain text must never reach this
    /// backend. Admitting one would reclassify every such file in every
    /// library out of `plain-text-v1` — a full re-extract and re-embed, and
    /// every line/column locator turned into a page.
    #[test]
    fn plain_text_extensions_are_refused() {
        for name in [
            "notes.txt",
            "readme.md",
            "page.html",
            "page.htm",
            "data.xml",
            "run.log",
            "a.csv",
            "b.json",
            "c.jsonl",
            "d.zip",
            "e.tar",
            "f.docx",
            "g.xlsx",
            "h.svg",
            "i.png",
        ] {
            assert_eq!(
                PagedFormat::for_path(Path::new(name)),
                None,
                "{name} must not be admitted to the MuPDF backend"
            );
        }
    }

    #[test]
    fn the_admitted_formats_are_the_measured_ones() {
        for (name, expected) in [
            ("a.pdf", PagedFormat::Pdf),
            ("A.PDF", PagedFormat::Pdf),
            ("b.epub", PagedFormat::Epub),
            ("c.mobi", PagedFormat::Mobi),
            ("d.prc", PagedFormat::Mobi),
            ("e.pdb", PagedFormat::Mobi),
            ("f.azw3", PagedFormat::Kf8),
            ("f.azw", PagedFormat::Kf8),
            ("g.fb2", PagedFormat::Fb2),
            ("h.cbz", PagedFormat::Cbz),
            ("i.cbt", PagedFormat::Cbz),
        ] {
            assert_eq!(
                PagedFormat::for_path(Path::new(name)),
                Some(expected),
                "{name}"
            );
        }
    }

    /// A PDF's recipe string is load-bearing: it is what every already-indexed
    /// PDF was extracted under, and changing it re-extracts every library.
    #[test]
    fn the_pdf_recipe_identity_is_unchanged() {
        assert_eq!(PagedFormat::Pdf.recipe_identity(), "pdf-mupdf-v1");
    }

    /// A reflowable format's recipe carries the geometry, because the geometry
    /// is what produced its page numbers.
    #[test]
    fn reflowable_recipes_carry_the_layout() {
        for format in [
            PagedFormat::Epub,
            PagedFormat::Mobi,
            PagedFormat::Fb2,
            PagedFormat::Kf8,
        ] {
            assert!(format.is_reflowable(), "{format:?}");
            let identity = format.recipe_identity();
            assert!(identity.contains("layout-450x600x11"), "{identity}");
        }
        for fixed in [PagedFormat::Pdf, PagedFormat::Cbz] {
            assert!(!fixed.is_reflowable(), "{fixed:?}");
            assert!(!fixed.recipe_identity().contains("layout"), "{fixed:?}");
        }
    }

    /// A saved `file_type == "pdf"` meant PDFs when it was written. Books get
    /// their own names rather than joining that one.
    #[test]
    fn each_format_keeps_its_own_query_name() {
        assert_eq!(PagedFormat::Pdf.query_name(), "pdf");
        assert_eq!(PagedFormat::Epub.query_name(), "epub");
        assert_eq!(PagedFormat::Mobi.query_name(), "mobi");
        assert_eq!(PagedFormat::Fb2.query_name(), "fb2");
        assert_eq!(PagedFormat::Cbz.query_name(), "cbz");
        assert_eq!(PagedFormat::Kf8.query_name(), "azw3");
    }

    #[test]
    fn a_non_palmdb_file_is_not_guarded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.epub");
        std::fs::write(&path, b"not a palmdb file").unwrap();
        assert!(guard_container(&path, PagedFormat::Epub).is_ok());
    }

    /// KF8 is told from MOBI 6 by the file-version field and by nothing else:
    /// both declare PalmDOC compression, so the field one would reach for
    /// first cannot distinguish them. Getting this wrong once meant two
    /// measured books indexing as empty without any error.
    #[test]
    fn the_file_version_is_what_tells_kf8_from_mobi_six() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };

        let six = write("six.mobi", &palmdb_fixture(2, Some(6)));
        let eight = write("eight.azw3", &palmdb_fixture(2, Some(8)));
        assert_eq!(mobi_file_version(&six).unwrap(), Some(6));
        assert_eq!(mobi_file_version(&eight).unwrap(), Some(8));
        // Both are readable now — one by MuPDF, one by rebuilding it — so
        // neither is refused.
        assert!(guard_container(&six, PagedFormat::Mobi).is_ok());
        assert!(guard_container(&eight, PagedFormat::Kf8).is_ok());
    }

    /// Compression MuPDF would decode as PalmDOC anyway, returning plausible
    /// bytes rather than an error. Refused before anything reads it.
    #[test]
    fn undecodable_compression_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huff.mobi");
        std::fs::write(&path, palmdb_fixture(17480, Some(6))).unwrap();
        let error = guard_container(&path, PagedFormat::Mobi)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("17480") || error.contains("compression"),
            "{error}"
        );
    }

    /// A file named as a Kindle book that is not one is refused here rather
    /// than opened and found wanting.
    #[test]
    fn a_container_that_is_not_mobipocket_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.azw3");
        std::fs::write(
            &path,
            b"this is not a palmdb container at all, not even close",
        )
        .unwrap();
        assert!(guard_container(&path, PagedFormat::Kf8).is_err());
    }

    /// A PalmDB container with the given PalmDOC compression and, optionally,
    /// a MOBI header declaring the given file version. Record 0 is placed at a
    /// fixed offset past the record list, as a real container does.
    fn palmdb_fixture(compression: u16, mobi_version: Option<u32>) -> Vec<u8> {
        let record0: u32 = 512;
        let mut bytes = vec![0u8; record0 as usize + 64];
        bytes[60..68].copy_from_slice(b"BOOKMOBI");
        bytes[76..78].copy_from_slice(&1u16.to_be_bytes());
        bytes[78..82].copy_from_slice(&record0.to_be_bytes());
        let base = record0 as usize;
        bytes[base..base + 2].copy_from_slice(&compression.to_be_bytes());
        if let Some(version) = mobi_version {
            bytes[base + 16..base + 20].copy_from_slice(b"MOBI");
            bytes[base + 36..base + 40].copy_from_slice(&version.to_be_bytes());
        }
        bytes
    }
}
