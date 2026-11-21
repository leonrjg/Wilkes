//! Who may call the HTTP API.
//!
//! Anyone who can reach it, unless a token is set: the API is unauthenticated
//! by default, which is why it binds loopback by default, and an operator who
//! exposes it further restricts it with firewall rules or by setting a token.
//! With a token set, every route but `/health` and [`login`] requires it — as
//! a bearer header, or, from a browser, as the cookie [`TOKEN_COOKIE`] it was
//! given in exchange for the token.

use std::sync::{Arc, RwLock};

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

pub use wilkes_agent::mcp::{require_bearer_token, TOKEN_COOKIE};

/// The token the API currently requires, `None` meaning none is. Shared with
/// the running listener, so a rotation takes effect without a restart.
pub type ApiToken = Arc<RwLock<Option<String>>>;

/// A year: the cookie is the token itself, so it lasts until the token is
/// rotated, which is what invalidates it.
const COOKIE_MAX_AGE_SECS: u64 = 365 * 24 * 60 * 60;

#[derive(Deserialize)]
pub struct LoginBody {
    token: String,
}

/// Exchange the token for the cookie a browser sends with every request after.
///
/// The one route that answers without a credential, because it is how a
/// browser gets one. With no token required it sets nothing: there is nothing
/// to prove.
pub async fn login(token: ApiToken, body: LoginBody) -> Response {
    let expected = token
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let Some(expected) = expected else {
        return StatusCode::NO_CONTENT.into_response();
    };
    if body.token.trim() != expected {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "That is not this server's token" })),
        )
            .into_response();
    }
    let cookie = format!(
        "{TOKEN_COOKIE}={expected}; Path=/; HttpOnly; SameSite=Strict; Max-Age={COOKIE_MAX_AGE_SECS}"
    );
    let mut response = StatusCode::NO_CONTENT.into_response();
    match HeaderValue::from_str(&cookie) {
        Ok(value) => {
            response.headers_mut().insert(header::SET_COOKIE, value);
            response
        }
        Err(error) => {
            tracing::error!("could not set the API session cookie: {error}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn login_sets_the_cookie_only_for_the_right_token() {
        let token: ApiToken = Arc::new(RwLock::new(Some("secret".to_string())));

        let wrong = login(
            Arc::clone(&token),
            LoginBody {
                token: "guess".into(),
            },
        )
        .await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        assert!(wrong.headers().get(header::SET_COOKIE).is_none());

        let right = login(
            Arc::clone(&token),
            LoginBody {
                token: "secret".into(),
            },
        )
        .await;
        assert_eq!(right.status(), StatusCode::NO_CONTENT);
        let cookie = right
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(cookie.starts_with(&format!("{TOKEN_COOKIE}=secret;")));
        assert!(cookie.contains("HttpOnly"));

        let open: ApiToken = Arc::new(RwLock::new(None));
        let none = login(open, LoginBody { token: "x".into() }).await;
        assert_eq!(none.status(), StatusCode::NO_CONTENT);
        assert!(none.headers().get(header::SET_COOKIE).is_none());
    }
}
