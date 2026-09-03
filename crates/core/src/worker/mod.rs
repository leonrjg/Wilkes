pub mod embedder;
pub mod fault;
pub mod ipc;
pub mod manager;
mod process;
mod python_env;
mod runtime;

/// Default time an idle model worker remains resident before it is unloaded.
/// Embedding and generation use the same worker lifecycle, so their defaults
/// must come from the same source even though each process can be tuned
/// independently at runtime.
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 5 * 60;
