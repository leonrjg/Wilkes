//! The stable vocabulary a programmatic consumer refuses on.
//!
//! Every code here is part of the consumer API contract: a caller branches on
//! it, and the HTTP layer maps it to a status. It travels as a typed error
//! rather than as a prefix on a sentence, because the machine-readable half of
//! a contract must not depend on prose nobody is stopping from being reworded.
//!
//! The error is raised where the fact is established — in the index, in the
//! import, in the resolver — and carried up the `anyhow` chain, so a caller
//! that wraps it with context adds to the message without losing the code.

use serde::{Deserialize, Serialize};

/// One refusal a consumer route can express.
///
/// Closed on purpose: a code that no consumer has been told about is a code
/// nobody can branch on, so adding one is a change to the documented contract
/// and not an implementation detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsumerErrorCode {
    ManagedWorkspaceNotFound,
    ManagedWorkspaceConfigurationMismatch,
    ManagedWorkspaceProtected,
    EmbeddingSpaceMismatch,
    EmbeddingSpaceStale,
    ExtractionRecipeMismatch,
    SourceChangedDuringImport,
    DocumentIndexIncomplete,
    ChunkRefNotFound,
    IdempotencyKeyConflict,
    /// The index carries no exact identity, so its chunk refs are absent and
    /// no passage in it can be addressed. Its vectors remain usable by Wilkes
    /// locally; the remedy for a consumer is a rebuild.
    IndexIdentityUnverified,
}

impl ConsumerErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ManagedWorkspaceNotFound => "MANAGED_WORKSPACE_NOT_FOUND",
            Self::ManagedWorkspaceConfigurationMismatch => {
                "MANAGED_WORKSPACE_CONFIGURATION_MISMATCH"
            }
            Self::ManagedWorkspaceProtected => "MANAGED_WORKSPACE_PROTECTED",
            Self::EmbeddingSpaceMismatch => "EMBEDDING_SPACE_MISMATCH",
            Self::EmbeddingSpaceStale => "EMBEDDING_SPACE_STALE",
            Self::ExtractionRecipeMismatch => "EXTRACTION_RECIPE_MISMATCH",
            Self::SourceChangedDuringImport => "SOURCE_CHANGED_DURING_IMPORT",
            Self::DocumentIndexIncomplete => "DOCUMENT_INDEX_INCOMPLETE",
            Self::ChunkRefNotFound => "CHUNK_REF_NOT_FOUND",
            Self::IdempotencyKeyConflict => "IDEMPOTENCY_KEY_CONFLICT",
            Self::IndexIdentityUnverified => "INDEX_IDENTITY_UNVERIFIED",
        }
    }
}

impl std::fmt::Display for ConsumerErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a consumer route failed with: a message, and the code to branch on
/// where one of the documented refusals applies.
///
/// The code is optional because not every failure is one of them — a poisoned
/// lock or an unreadable database is a fault rather than a contract outcome,
/// and inventing a code for it would tell a caller to handle something it
/// cannot. Those answer with the message alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsumerError {
    code: Option<ConsumerErrorCode>,
    message: String,
}

impl ConsumerError {
    pub fn new(code: ConsumerErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: Some(code),
            message: message.into(),
        }
    }

    /// A failure with no place in the contract's vocabulary.
    pub fn untyped(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
        }
    }

    pub fn code(&self) -> Option<ConsumerErrorCode> {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Recover the code an `anyhow` chain carries, if any.
    ///
    /// A downcast rather than a search through the rendered message: context
    /// added on the way up is part of what the caller reads, and none of it is
    /// allowed to decide what the caller branches on.
    pub fn from_anyhow(error: &anyhow::Error) -> Self {
        let code = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ConsumerError>())
            .and_then(ConsumerError::code);
        Self {
            code,
            message: format!("{error:#}"),
        }
    }
}

impl std::fmt::Display for ConsumerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code {
            Some(code) => write!(f, "{code}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ConsumerError {}

/// A message with no code. The conversion exists so that an internal helper
/// which cannot fail contractually keeps returning a plain string.
impl From<String> for ConsumerError {
    fn from(message: String) -> Self {
        Self::untyped(message)
    }
}

impl From<&str> for ConsumerError {
    fn from(message: &str) -> Self {
        Self::untyped(message)
    }
}

impl From<anyhow::Error> for ConsumerError {
    fn from(error: anyhow::Error) -> Self {
        Self::from_anyhow(&error)
    }
}

/// One of the documented refusals, as an `anyhow::Error` — for the
/// `ok_or_else` and `map_err` positions where a value is wanted rather than an
/// early return.
pub fn consumer_error(code: ConsumerErrorCode, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(ConsumerError::new(code, message))
}

/// Fail with one of the documented codes.
#[macro_export]
macro_rules! consumer_bail {
    ($code:expr, $($arg:tt)*) => {
        return ::std::result::Result::Err(
            $crate::consumer::ConsumerError::new($code, format!($($arg)*)).into()
        )
    };
}

/// Require a condition, failing with one of the documented codes.
#[macro_export]
macro_rules! consumer_ensure {
    ($condition:expr, $code:expr, $($arg:tt)*) => {
        if !$condition {
            $crate::consumer_bail!($code, $($arg)*);
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_survives_the_context_added_above_it() {
        let raised: anyhow::Error =
            ConsumerError::new(ConsumerErrorCode::ChunkRefNotFound, "3 refs").into();
        let wrapped = raised.context("Resolve failed");
        let recovered = ConsumerError::from_anyhow(&wrapped);
        assert_eq!(recovered.code(), Some(ConsumerErrorCode::ChunkRefNotFound));
        assert!(recovered.message().contains("Resolve failed"));
        assert!(recovered.message().contains("3 refs"));
    }

    #[test]
    fn an_ordinary_failure_carries_no_code_to_branch_on() {
        let error = anyhow::anyhow!("the index lock was poisoned");
        assert_eq!(ConsumerError::from_anyhow(&error).code(), None);
    }
}
