//! Who may call the HTTP API.
//!
//! On loopback, anyone on this machine: that is what the API was for, and a
//! process here can read the library's files directly anyway. On any other
//! address, only a caller that presents the API's token — as a bearer header,
//! or, from a browser, as the cookie [`TOKEN_COOKIE`] it was given in exchange
//! for the token. Whether a token is required is decided by the address the
//! listener binds, never by where a request comes from: a reverse proxy on the
//! same machine would otherwise turn every outside request into a local one.

use std::net::IpAddr;
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

/// The token a listener bound to `bind` must require.
///
/// Loopback needs none, and any token given for it is not required. Any
/// other address refuses to serve without one: an API that can read and
/// rewrite the library, open on the network to whoever finds the port, is the
/// state this exists to rule out.
pub fn token_for_bind(bind: IpAddr, token: Option<String>) -> anyhow::Result<Option<String>> {
    token_for_reach(bind.is_loopback(), &bind.to_string(), token)
}

/// [`token_for_bind`] for a listener whose address is a name, decided by
/// whether every address the name resolves to is loopback.
pub fn token_for_reach(
    loopback_only: bool,
    bind: &str,
    token: Option<String>,
) -> anyhow::Result<Option<String>> {
    if loopback_only {
        return Ok(None);
    }
    match token.map(|token| token.trim().to_string()) {
        Some(token) if !token.is_empty() => Ok(Some(token)),
        _ => anyhow::bail!(
            "The Wilkes API is bound to {bind}, which is reachable from other machines, and so \
             requires a bearer token"
        ),
    }
}

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

    #[test]
    fn loopback_needs_no_token_and_anything_else_does() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let v6_loopback: IpAddr = "::1".parse().unwrap();
        let any: IpAddr = "0.0.0.0".parse().unwrap();

        assert_eq!(token_for_bind(loopback, None).unwrap(), None);
        assert_eq!(token_for_bind(v6_loopback, Some("t".into())).unwrap(), None);
        assert!(token_for_bind(any, None).is_err());
        assert!(token_for_bind(any, Some("  ".into())).is_err());
        assert_eq!(
            token_for_bind(any, Some(" secret ".into())).unwrap(),
            Some("secret".to_string())
        );
    }

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
