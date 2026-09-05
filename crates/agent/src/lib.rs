//! Wilkes behind an ACP chat.
//!
//! The chat itself -- the subprocess, the handshake, the streamed turn, the
//! permission boundary, the conversations on disk -- is `wilkes-chat`, which
//! knows nothing about documents. This crate is the half that does: the
//! read-only MCP server (`mcp`), the readers and searches it answers with
//! (`reader`, `search`), the block of state pushed into every prompt
//! (`context`), and the `ChatHost` that ties the three to a session (`host`).
//!
//! Which CLI answers, and whether its adapter is on this machine, is the
//! chat's own business and is re-exported below rather than decided again
//! here.

pub mod context;
pub mod host;
pub mod mcp;
pub mod reader;
pub mod search;

pub use host::{HostActiveDoc, HostContext, HostContextFile, WilkesChatHost};
pub use wilkes_chat::backend::{
    auth_note, install_backend_adapter, label, package_spec, probe_backend_availability,
    resolve_launch_spec, BackendAvailability, NpmPackageSpec, ResolvedLaunchSpec,
};

pub mod library;
