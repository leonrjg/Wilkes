use serde::{de, Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Which CLI answers, and the shapes a chat's configuration is persisted in.
///
/// Defined by `wilkes-chat` and re-exported here rather than declared again.
/// They are the chat's own vocabulary -- the enum names the adapters that
/// crate has been checked against, and `ChatConfigValue` is what its store
/// writes -- so a copy here would be a second definition of the same thing,
/// agreeing only for as long as someone kept editing both.
pub use wilkes_chat::{AgentBackend, ChatBackendConfig, ChatConfigValue};

mod citations;
mod extraction;
mod files;
mod geometry;
mod integrations;
mod literature;
mod models;
mod preview;
mod research;
mod retained;
mod search;
mod semantic;
mod settings;
mod topics;

pub use self::citations::*;
pub use self::extraction::*;
pub use self::files::*;
pub use self::geometry::*;
pub use self::integrations::*;
pub use self::literature::*;
pub use self::models::*;
pub use self::preview::*;
pub use self::research::*;
pub use self::retained::*;
pub use self::search::*;
pub use self::semantic::*;
pub use self::settings::*;
pub use self::topics::*;

#[cfg(test)]
mod tests;
