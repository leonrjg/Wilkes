//! Preview payloads and the surrogate outline and links served with them.

use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PreviewData {
    /// Random-access image pages; no whole-document surrogate is produced.
    Comic {
        page: u32,
        highlight_bbox: Option<BoundingBox>,
        page_count: u32,
    },
    Text {
        content: String,
        language: Option<String>,
        highlight_line: u32,
        highlight_range: ByteRange,
    },
    /// A book: paginated by Wilkes, not by the file.
    ///
    /// Carries everything the reader needs *except* the pages themselves,
    /// which are fetched separately because they are megabytes and this is not
    /// the channel for them. The outline and the links travel here because a
    /// surrogate rendering cannot hold either — see
    /// [`crate::extract::document::surrogate`] — and because they are already known
    /// by the time the preview is answered.
    Book {
        page: u32,
        highlight_bbox: Option<BoundingBox>,
        /// The book's own table of contents, empty when it declares none.
        #[serde(default)]
        outline: Vec<SurrogateOutline>,
        /// Only pages that have links appear.
        #[serde(default)]
        links: Vec<SurrogatePageLinks>,
    },
    Pdf {
        page: u32,
        highlight_bbox: Option<BoundingBox>,
        /// The page areas whose text this document's reading owns rather than
        /// its glyphs. Empty for a document the index has never read, which is
        /// the same answer as "this document has none": in both cases the
        /// reader offers what the page draws, which is all there is to offer.
        #[serde(default)]
        superseded: Vec<SupersededArea>,
    },
}

/// One entry of a book's table of contents, anchored to a page of the
/// surrogate rendering.
///
/// Deliberately not a [`PdfDestination`-shaped thing](crate::types): a pdf.js
/// destination names its page by an internal reference that only pdf.js can
/// resolve, and this destination was resolved here, by MuPDF, against a
/// document the reader never sees. Saying "page 34" is the honest form of what
/// is known.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SurrogateOutline {
    pub title: String,
    /// 1-based page of the surrogate, absent for an entry that resolves to no
    /// page at all.
    pub page: Option<u32>,
    /// Where on that page, in unscaled page units from the top, when the
    /// destination pins a position. Absent for a plain "fit the page"
    /// destination, which pins none.
    #[serde(default)]
    pub offset_y: Option<f32>,
    /// An external target, for the entries that are links out rather than
    /// places in the book.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub items: Vec<SurrogateOutline>,
}

/// The links on one page of a surrogate rendering.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SurrogatePageLinks {
    /// 1-based.
    pub page: u32,
    pub links: Vec<SurrogateLink>,
}

/// One link: where it is on the page, and where it goes. Exactly one of `page`
/// and `url` is set — MuPDF resolves an internal destination itself and leaves
/// none for an external one, so which kind a link is comes from the document.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SurrogateLink {
    /// In unscaled page units, top-left origin, as every other box here is.
    pub bbox: BoundingBox,
    /// 1-based target page inside the book.
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub offset_y: Option<f32>,
    #[serde(default)]
    pub url: Option<String>,
}
