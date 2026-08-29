use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;
use wilkes_core::consumer::{ConsumerError, ConsumerErrorCode};

#[derive(Serialize)]
pub struct ErrorBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub error: String,
}

pub fn err(msg: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorBody {
            code: None,
            error: msg.into(),
        }),
    )
}

pub fn server_err(msg: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody {
            code: None,
            error: msg.into(),
        }),
    )
}

/// The consumer surface's refusal, rendered.
///
/// The status comes from the code the error carries, and the code was decided
/// where the failure was established. Nothing here reads the message: a
/// machine-readable contract that depended on prose would break the first time
/// a sentence was reworded, which is a change nobody would think of as
/// breaking.
pub fn consumer_err(error: ConsumerError) -> (StatusCode, Json<ErrorBody>) {
    let status = match error.code() {
        Some(ConsumerErrorCode::ManagedWorkspaceNotFound | ConsumerErrorCode::ChunkRefNotFound) => {
            StatusCode::NOT_FOUND
        }
        Some(
            ConsumerErrorCode::ManagedWorkspaceConfigurationMismatch
            | ConsumerErrorCode::EmbeddingSpaceMismatch
            | ConsumerErrorCode::EmbeddingSpaceStale
            | ConsumerErrorCode::ExtractionRecipeMismatch
            | ConsumerErrorCode::IdempotencyKeyConflict
            | ConsumerErrorCode::IndexIdentityUnverified,
        ) => StatusCode::CONFLICT,
        _ => StatusCode::BAD_REQUEST,
    };
    (
        status,
        Json(ErrorBody {
            code: error.code().map(|code| code.as_str().to_string()),
            error: error.to_string(),
        }),
    )
}

/// The same rendering for a failure that arrived as an `anyhow` chain, whose
/// code — if it has one — is downcast rather than parsed out.
pub fn consumer_anyhow_err(error: anyhow::Error) -> (StatusCode, Json<ErrorBody>) {
    consumer_err(ConsumerError::from_anyhow(&error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_err() {
        let (status, Json(body)) = err("bad request");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.error, "bad request");
    }

    #[test]
    fn test_server_err() {
        let (status, Json(body)) = server_err("boom");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.error, "boom");
    }

    #[test]
    fn consumer_errors_carry_stable_machine_code() {
        let (status, Json(body)) = consumer_err(ConsumerError::new(
            ConsumerErrorCode::EmbeddingSpaceMismatch,
            "corpus=space-a, request=space-b",
        ));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body.code.as_deref(), Some("EMBEDDING_SPACE_MISMATCH"));
        assert_eq!(
            body.error,
            "EMBEDDING_SPACE_MISMATCH: corpus=space-a, request=space-b"
        );
    }

    /// An index whose chunk refs are absent names a rebuild, and does it in the
    /// same shape as every other refusal rather than as a sentence.
    #[test]
    fn an_unverified_index_is_a_conflict_a_caller_can_branch_on() {
        let (status, Json(body)) = consumer_err(ConsumerError::new(
            ConsumerErrorCode::IndexIdentityUnverified,
            "rebuild this index",
        ));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body.code.as_deref(), Some("INDEX_IDENTITY_UNVERIFIED"));
    }

    /// A message that happens to contain a code name is not a coded failure.
    /// Under the old substring search it was, which is exactly the coupling
    /// between prose and contract this replaces.
    #[test]
    fn a_message_naming_a_code_does_not_become_one() {
        let (status, Json(body)) = consumer_err(ConsumerError::untyped(
            "the caller asked about CHUNK_REF_NOT_FOUND",
        ));
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.code, None);
    }
}
