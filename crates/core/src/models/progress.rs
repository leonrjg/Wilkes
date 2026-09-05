use crate::embed::index::job::{DocumentOutcome, DocumentStage};

use tokio::sync::mpsc;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DownloadProgress {
    pub bytes_received: u64,
    pub total_bytes: u64,
    pub done: bool,
}

/// One document's worth of movement in an index build.
///
/// The counters are the progress bar's; the rest names the document the build
/// is on and where it has got to. That naming used to be a preformatted
/// English sentence, which meant the interface could show it and nothing else
/// could do anything with it — not filter by outcome, not offer a retry, not
/// survive the window closing.
///
/// This event is a *notification carrying a copy*. The durable answer to
/// "what happened to this document" is the row
/// [`crate::embed::index::IndexJobJournal`] wrote before this was sent; a
/// listener that missed the event reads the journal and loses nothing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexBuildProgress {
    pub files_processed: usize,
    pub total_files: usize,
    /// The job this belongs to, when the build is journalling one.
    pub job_id: Option<i64>,
    /// The document this event is about, if it is about one.
    pub document: Option<String>,
    pub stage: Option<DocumentStage>,
    /// Set once the document is settled; `None` while it is still moving.
    pub outcome: Option<DocumentOutcome>,
    pub done: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum EmbedProgress {
    Download(DownloadProgress),
    Build(IndexBuildProgress),
}

pub type ProgressTx = mpsc::Sender<EmbedProgress>;
