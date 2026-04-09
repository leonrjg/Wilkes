use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct ErrorBody {
    pub error: String,
}

pub fn err(msg: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorBody { error: msg.into() }),
    )
}

pub fn server_err(msg: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody { error: msg.into() }),
    )
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
}
