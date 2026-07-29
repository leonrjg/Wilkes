//! Engine-agnostic model plumbing: HF hub queries, raw file download, and the
//! progress vocabulary shared by embedding and generation installs.
pub mod downloader;
pub mod hf_hub;
pub mod progress;
