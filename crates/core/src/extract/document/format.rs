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
            "mobi" | "prc" | "pdb" | "azw3" | "azw" => Self::Mobi,
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
            Self::Epub | Self::Mobi | Self::Fb2 => true,
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
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Epub => "application/epub+zip",
            Self::Mobi => "application/x-mobipocket-ebook",
            Self::Fb2 => "application/x-fictionbook",
            Self::Cbz => "application/vnd.comicbook+zip",
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
    if format != PagedFormat::Mobi {
        return Ok(());
    }
    let mut file = std::fs::File::open(path)?;

    // PalmDB: 78-byte header, then 8 bytes per record entry. Record 0 holds
    // the PalmDOC header and, for a MOBI, the MOBI header behind it.
    let mut header = [0u8; 86];
    file.read_exact(&mut header).map_err(|error| {
        anyhow::anyhow!(
            "{} is too short to be a PalmDB container: {error}",
            path.display()
        )
    })?;
    let kind = &header[60..68];
    anyhow::ensure!(
        kind == b"BOOKMOBI" || kind == b"TEXtREAd",
        "{} is not a Mobipocket container (type {:?})",
        path.display(),
        String::from_utf8_lossy(kind)
    );
    let record0 = u32::from_be_bytes([header[78], header[79], header[80], header[81]]) as u64;

    let mut record = [0u8; 40];
    file.seek(SeekFrom::Start(record0))?;
    file.read_exact(&mut record)
        .map_err(|error| anyhow::anyhow!("{} has no readable record 0: {error}", path.display()))?;

    // PalmDOC header, first field. MuPDF decodes only these two and sends
    // everything else through the PalmDOC decompressor regardless, which
    // returns plausible bytes rather than an error.
    let compression = u16::from_be_bytes([record[0], record[1]]);
    anyhow::ensure!(
        compression == 1 || compression == 2,
        "{} uses compression {compression}, which this reader cannot decode \
         (only uncompressed and PalmDOC are supported; 17480 is HUFF/CDIC)",
        path.display()
    );

    // The MOBI header follows the 16-byte PalmDOC header. A container without
    // one is a plain PalmDOC book, which MuPDF reads.
    if &record[16..20] != b"MOBI" {
        return Ok(());
    }
    let version = u32::from_be_bytes([record[36], record[37], record[38], record[39]]);
    anyhow::ensure!(
        version < 8,
        "{} is a Kindle KF8 book (MOBI file version {version}). MuPDF reads \
         MOBI 6 records and this file has none, so it would be indexed as \
         empty rather than failing; refused instead. Convert it to EPUB to \
         read it here.",
        path.display()
    );
    Ok(())
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
            ("f.azw3", PagedFormat::Mobi),
            ("f.azw", PagedFormat::Mobi),
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
        for format in [PagedFormat::Epub, PagedFormat::Mobi, PagedFormat::Fb2] {
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
    }

    #[test]
    fn a_non_palmdb_file_is_not_guarded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.epub");
        std::fs::write(&path, b"not a palmdb file").unwrap();
        assert!(guard_container(&path, PagedFormat::Epub).is_ok());
    }

    /// The measured failure, reconstructed: a PalmDB container declaring
    /// PalmDOC compression (which is what the real KF8 files declare) and MOBI
    /// file version 8.
    #[test]
    fn a_kf8_container_is_refused_by_version_not_compression() {
        let dir = tempfile::tempdir().unwrap();
        let mobi6 = palmdb_fixture(2, Some(6));
        let kf8 = palmdb_fixture(2, Some(8));
        let huff = palmdb_fixture(17480, Some(6));

        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };

        let good = write("good.mobi", &mobi6);
        assert!(guard_container(&good, PagedFormat::Mobi).is_ok());

        let bad = write("bad.azw3", &kf8);
        let error = guard_container(&bad, PagedFormat::Mobi)
            .unwrap_err()
            .to_string();
        assert!(error.contains("KF8"), "{error}");
        assert!(error.contains("file version 8"), "{error}");

        let compressed = write("huff.mobi", &huff);
        let error = guard_container(&compressed, PagedFormat::Mobi)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("17480") || error.contains("compression"),
            "{error}"
        );
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
