//! One module per task. Each owns its prompt, its constraint, **and** its
//! verification: a task returns `Err` rather than an unvalidated string, so
//! callers never post-process model output.

pub mod cluster_label;
pub mod document_summary;
pub mod relation;
pub mod search_results_summary;
