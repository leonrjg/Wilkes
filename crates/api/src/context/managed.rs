//! The managed corpus: the index Wilkes keeps for documents it was handed
//! rather than found in a library root.

mod backup;
mod import;
mod runtime;

pub(crate) use self::backup::*;
