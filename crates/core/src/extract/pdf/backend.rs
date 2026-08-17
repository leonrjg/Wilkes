use std::path::Path;

use crate::types::{ExtractedContent, OutlineEntry};

/// Platform-specific PDF extraction backend.
///
/// Each platform provides one concrete implementation.  New backends (e.g.
/// MuPDF for Linux) should implement this trait and be wired up in `mod.rs`.
pub(super) trait PdfBackend: Send + Sync {
    fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent>;

    /// The bookmark tree, flattened to depth-tagged entries in reading order.
    /// Separate from `extract` because it costs a document open and nothing
    /// else — no page rendering, no text, no bounding boxes.
    fn outline(&self, path: &Path) -> anyhow::Result<Vec<OutlineEntry>>;
}
