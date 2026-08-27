use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

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

pub fn managed_err(msg: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    let error = msg.into();
    const CODES: &[&str] = &[
        "MANAGED_WORKSPACE_NOT_FOUND",
        "MANAGED_WORKSPACE_CONFIGURATION_MISMATCH",
        "MANAGED_WORKSPACE_PROTECTED",
        "EMBEDDING_SPACE_MISMATCH",
        "EMBEDDING_SPACE_STALE",
        "EXTRACTION_RECIPE_MISMATCH",
        "SOURCE_CHANGED_DURING_IMPORT",
        "DOCUMENT_INDEX_INCOMPLETE",
        "CHUNK_REF_NOT_FOUND",
        "IDEMPOTENCY_KEY_CONFLICT",
    ];
    let code = CODES
        .iter()
        .find(|code| error.contains(**code))
        .map(|code| (*code).to_string());
    let status = match code.as_deref() {
        Some("MANAGED_WORKSPACE_NOT_FOUND" | "CHUNK_REF_NOT_FOUND") => StatusCode::NOT_FOUND,
        Some(
            "MANAGED_WORKSPACE_CONFIGURATION_MISMATCH"
            | "EMBEDDING_SPACE_MISMATCH"
            | "EMBEDDING_SPACE_STALE"
            | "EXTRACTION_RECIPE_MISMATCH"
            | "IDEMPOTENCY_KEY_CONFLICT",
        ) => StatusCode::CONFLICT,
        _ => StatusCode::BAD_REQUEST,
    };
    (status, Json(ErrorBody { code, error }))
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
    fn managed_errors_carry_stable_machine_code() {
        let (status, Json(body)) =
            managed_err("EMBEDDING_SPACE_MISMATCH: corpus=space-a, request=space-b");
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body.code.as_deref(), Some("EMBEDDING_SPACE_MISMATCH"));
    }
}
