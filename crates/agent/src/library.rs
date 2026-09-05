//! Typed research-library operations for MCP. Execution belongs to the API
//! layer; these types describe the capability and its path boundary only.
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LibraryKind {
    Bookmarks,
    Tags,
    SearchHistory,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum LibraryEdit {
    AddBookmark {
        path: PathBuf,
        location: Location,
        quote: String,
        note: Option<String>,
    },
    UpdateBookmarkNote {
        id: String,
        note: Option<String>,
    },
    RemoveBookmark {
        id: String,
    },
    CreateTag {
        name: String,
        color: Option<String>,
    },
    UpdateTag {
        id: String,
        name: String,
        color: Option<String>,
    },
    DeleteTag {
        id: String,
    },
    TagDocuments {
        paths: Vec<PathBuf>,
        #[serde(default)]
        add_tag_ids: Vec<String>,
        #[serde(default)]
        remove_tag_ids: Vec<String>,
    },
    CreateCollection {
        name: String,
        expression: String,
    },
    UpdateCollection {
        id: String,
        name: String,
        expression: String,
    },
    DeleteCollection {
        id: String,
    },
    RenameFile {
        path: PathBuf,
        new_name: String,
    },
    RefreshMetadata {
        path: PathBuf,
    },
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Location {
    Pdf {
        page: u32,
    },
    Text {
        line: u32,
        #[serde(default)]
        col: u32,
    },
}
impl Location {
    pub fn into_origin(self) -> Result<wilkes_core::types::SourceOrigin, String> {
        match self {
            Self::Pdf { page } if page > 0 => {
                Ok(wilkes_core::types::SourceOrigin::PdfPage { page, bbox: None })
            }
            Self::Text { line, col } if line > 0 => {
                Ok(wilkes_core::types::SourceOrigin::TextFile { line, col })
            }
            _ => Err("Page and line numbers are 1-based".into()),
        }
    }
}
impl LibraryEdit {
    pub fn paths_mut(&mut self) -> Vec<&mut PathBuf> {
        match self {
            Self::AddBookmark { path, .. }
            | Self::RenameFile { path, .. }
            | Self::RefreshMetadata { path } => vec![path],
            Self::TagDocuments { paths, .. } => paths.iter_mut().collect(),
            _ => vec![],
        }
    }
}
