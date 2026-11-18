//! The agent's browser: a webview of the desktop app's own, signed in to
//! whatever the user signed in to in the browser pane, that MCP tools fetch
//! and read pages through.
//!
//! Some sites refuse a plain HTTP client whatever cookies it carries -- the ACM
//! Digital Library answers 403 -- but serve the same address to a browser the
//! user is signed in with. Wilkes owns no such browser outside the desktop
//! app, so this is a boundary the desktop app fills in and every other host
//! leaves empty; a tool that needs it says so rather than falling back to a
//! fetch the site would refuse.
//!
//! The browser only fetches. What becomes of a download -- its name, the size
//! limit, whether the library already has it -- is decided by the same core
//! rules every other download keeps, on the MCP side.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Serialize;
use tracing::error;

/// What a tool that needs the agent's browser answers where there is none.
pub const BROWSER_UNAVAILABLE: &str =
    "The agent browser is only available while the Wilkes desktop app is running";

/// A file the agent's browser downloaded into a staging directory of its
/// own, which goes with it.
#[derive(Debug)]
pub struct StagedDownload {
    dir: PathBuf,
    file: PathBuf,
    /// The name the site gave the file.
    pub suggested_name: String,
}

impl StagedDownload {
    /// `file` inside `dir`, a directory that holds nothing else worth keeping:
    /// it is removed when this is dropped.
    pub fn new(dir: PathBuf, file: PathBuf, suggested_name: String) -> Self {
        Self {
            dir,
            file,
            suggested_name,
        }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }
}

impl Drop for StagedDownload {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                error!(
                    "agent browser: could not clear {}: {e}",
                    self.dir.display()
                );
            }
        }
    }
}

/// A page as the agent's browser shows it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct BrowserPage {
    /// Where the browser ended up, after any redirect.
    pub url: String,
    pub title: String,
    /// The page's text as the browser renders it.
    pub text: String,
}

#[async_trait]
pub trait AgentBrowser: Send + Sync {
    /// Download `url` through the browser's own session. A page in place of a
    /// file -- a sign-in, a check, a paywall -- is an error, and the browser
    /// is left showing it for the user to deal with.
    async fn download(&self, url: &str) -> Result<StagedDownload, String>;

    /// Open `url` and return the page it lands on.
    async fn read(&self, url: &str) -> Result<BrowserPage, String>;
}
