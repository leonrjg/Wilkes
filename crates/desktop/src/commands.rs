//! The Tauri commands the window invokes, one module per area. [`crate::run`]
//! registers them.

mod diagnostics;
mod feeds;
mod files;
mod generation;
mod image_analysis;
mod indexing;
mod literature;
mod metadata;
mod research;
mod search;
mod services;
mod settings;
mod viewer;
mod workers;
mod workspaces;

pub(crate) use self::diagnostics::*;
pub(crate) use self::feeds::*;
pub(crate) use self::files::*;
pub(crate) use self::generation::*;
pub(crate) use self::image_analysis::*;
pub(crate) use self::indexing::*;
pub(crate) use self::literature::*;
pub(crate) use self::metadata::*;
pub(crate) use self::research::*;
pub(crate) use self::search::*;
pub(crate) use self::services::*;
pub(crate) use self::settings::*;
pub(crate) use self::viewer::*;
pub(crate) use self::workers::*;
pub(crate) use self::workspaces::*;
