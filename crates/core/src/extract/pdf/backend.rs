use std::path::Path;

use crate::types::ExtractedContent;

/// Platform-specific PDF extraction backend.
///
/// Each platform provides one concrete implementation.  New backends (e.g.
/// MuPDF for Linux) should implement this trait and be wired up in `mod.rs`.
pub(super) trait PdfBackend: Send + Sync {
    fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent>;
}
