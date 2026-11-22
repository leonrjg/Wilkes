//! The API's handlers, one module per area. [`crate::api_router`] mounts them.

mod chunks;
mod diagnostics;
mod embedding;
mod feeds;
mod files;
mod generation;
mod image_analysis;
mod indexing;
mod literature;
mod managed;
mod research;
mod search;
mod viewer;
mod workers;
mod workspaces;

pub(crate) use self::chunks::*;
pub(crate) use self::diagnostics::*;
pub(crate) use self::embedding::*;
pub(crate) use self::feeds::*;
pub(crate) use self::files::*;
pub(crate) use self::generation::*;
pub(crate) use self::image_analysis::*;
pub(crate) use self::indexing::*;
pub(crate) use self::literature::*;
pub(crate) use self::managed::*;
pub(crate) use self::research::*;
pub(crate) use self::search::*;
pub(crate) use self::viewer::*;
pub(crate) use self::workers::*;
pub(crate) use self::workspaces::*;
