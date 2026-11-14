//! Editing a PDF in place: the fonts edited text is set in, and writing the
//! result out.
//!
//! All of it is MuPDF and font lookup, which block, so each call is moved off
//! the async workers. None of it is inference: this runs in the host by the
//! same rule that puts rendering there.

use std::path::PathBuf;

use wilkes_core::pdf_edit::{self, fonts, EditExport};

/// The id of the installed font that stands in for one a document names.
pub async fn resolve_font(request: fonts::FontRequest) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || fonts::resolve(&request).map(|font| font.id)).await?
}

/// A resolved font's file, for the interface to set text in.
pub async fn font_bytes(id: String) -> anyhow::Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || fonts::load(&id).map(|font| font.data)).await?
}

/// Write `source` with the reader's edits applied to `destination`.
pub async fn save_edits(
    source: PathBuf,
    destination: PathBuf,
    export: EditExport,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || pdf_edit::write_edited_pdf(&source, &export, &destination))
        .await?
}

/// `source` with the reader's edits applied, as bytes.
pub async fn export_edits(source: PathBuf, export: EditExport) -> anyhow::Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || pdf_edit::edited_pdf_bytes(&source, &export)).await?
}
