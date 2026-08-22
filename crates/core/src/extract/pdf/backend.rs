use std::path::Path;

use crate::types::{DeclaredOutline, ExtractedContent};

/// Platform-specific PDF extraction backend.
///
/// Each platform provides one concrete implementation.  New backends (e.g.
/// MuPDF for Linux) should implement this trait and be wired up in `mod.rs`.
pub(super) trait PdfBackend: Send + Sync {
    fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent>;

    /// The bookmark tree, flattened to depth-tagged entries in reading order,
    /// each anchored in the reading `extract` produces. Separate from
    /// `extract` because callers want it without the text, not because it is
    /// cheaper: anchoring a bookmark to a byte offset means reading the
    /// document, so this costs what extraction costs.
    fn outline(&self, path: &Path) -> anyhow::Result<DeclaredOutline>;
}
