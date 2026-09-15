use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{CONTENT_LENGTH, RETRY_AFTER};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

/// How long a single provider request may take, end to end.
///
/// Explicit because `reqwest`'s default is no timeout at all: a provider that
/// accepts a connection and then never answers would otherwise hold the
/// request — and, for a search the user is waiting on, the UI — forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on a single provider response body.
///
/// Enforced twice, like `acquire::MAX_DOWNLOAD_BYTES`: against the advertised
/// `Content-Length` before reading, and against the bytes actually received,
/// because a server may under-report the header or omit it entirely. A
/// provider answer is metadata about at most a few hundred records; anything
/// past this is a misconfigured URL pointed at a bulk dump, and reading it
/// into memory is the failure, not the symptom of one.
const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct RetryPolicy {
    pub max_attempts: usize,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl RetryPolicy {
    pub fn conservative() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(750),
            max_delay: Duration::from_secs(8),
        }
    }
}

#[derive(Clone)]
pub struct ProviderHttpClient {
    /// Owned rather than `&'static str` because a custom integration's name
    /// comes from a manifest the user wrote at runtime. Built-in providers
    /// still pass a literal and pay one allocation per client.
    provider: Arc<str>,
    http: reqwest::Client,
    retry: RetryPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderHttpErrorKind {
    NotFound,
    RateLimited,
    Temporary,
    Http,
    Request,
    Decode,
    /// The body exceeded [`MAX_RESPONSE_BYTES`]. Not `Decode`: nothing was
    /// wrong with the bytes, there were simply too many of them, and retrying
    /// would fetch the same too-many again.
    TooLarge,
}

#[derive(Debug)]
pub struct ProviderHttpError {
    pub provider: Arc<str>,
    pub kind: ProviderHttpErrorKind,
    pub status: Option<StatusCode>,
    pub message: String,
    pub retry_after: Option<Duration>,
}

impl fmt::Display for ProviderHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(status) => write!(
                f,
                "{} request failed with HTTP {status}: {}",
                self.provider, self.message
            ),
            None => write!(f, "{} request failed: {}", self.provider, self.message),
        }
    }
}

impl std::error::Error for ProviderHttpError {}

impl ProviderHttpClient {
    pub fn new(provider: impl Into<Arc<str>>) -> Self {
        Self {
            provider: provider.into(),
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|error| {
                    // Only fails when the TLS backend cannot initialise, which
                    // is fatal for every provider call this client would make.
                    tracing::error!("provider HTTP client build failed: {error}");
                    reqwest::Client::new()
                }),
            retry: RetryPolicy::conservative(),
        }
    }

    #[cfg(test)]
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub async fn get_status(
        &self,
        url: String,
        headers: &[(&str, String)],
    ) -> Result<StatusCode, ProviderHttpError> {
        let response = self.send_with_retry(url, headers).await?;
        Ok(response.status())
    }

    pub async fn get_json<T: DeserializeOwned>(
        &self,
        url: String,
        headers: &[(&str, String)],
    ) -> Result<T, ProviderHttpError> {
        let body = self.get_bytes(url, headers).await?;
        serde_json::from_slice::<T>(&body).map_err(|e| ProviderHttpError {
            provider: self.provider.clone(),
            kind: ProviderHttpErrorKind::Decode,
            status: None,
            message: e.to_string(),
            retry_after: None,
        })
    }

    /// The raw response body, bounded by [`MAX_RESPONSE_BYTES`].
    ///
    /// Public because a custom integration's probe shows the user the bytes a
    /// service actually returned next to what Wilkes made of them; that is the
    /// whole point of the probe, and it cannot be done from a decoded `T`.
    pub async fn get_bytes(
        &self,
        url: String,
        headers: &[(&str, String)],
    ) -> Result<Vec<u8>, ProviderHttpError> {
        let response = self.send_with_retry(url, headers).await?;
        self.read_bounded(response).await
    }

    /// Read a body, refusing one that is — or claims to be — over the cap.
    ///
    /// Streamed rather than `bytes()` so an over-long body is abandoned at the
    /// chunk that crosses the line. Reading it whole and then measuring it
    /// would have already done the damage the cap exists to prevent.
    async fn read_bounded(
        &self,
        response: reqwest::Response,
    ) -> Result<Vec<u8>, ProviderHttpError> {
        if let Some(advertised) = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
        {
            if advertised > MAX_RESPONSE_BYTES {
                return Err(self.too_large(advertised));
            }
        }

        let mut response = response;
        let mut body: Vec<u8> = Vec::new();
        loop {
            let chunk = response.chunk().await.map_err(|e| ProviderHttpError {
                provider: self.provider.clone(),
                kind: ProviderHttpErrorKind::Request,
                status: None,
                message: request_error_message(&e),
                retry_after: None,
            })?;
            let Some(chunk) = chunk else { break };
            if body.len() as u64 + chunk.len() as u64 > MAX_RESPONSE_BYTES {
                return Err(self.too_large(body.len() as u64 + chunk.len() as u64));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    fn too_large(&self, bytes: u64) -> ProviderHttpError {
        tracing::warn!(
            provider = %self.provider,
            bytes,
            limit = MAX_RESPONSE_BYTES,
            "provider response exceeds the size cap"
        );
        ProviderHttpError {
            provider: self.provider.clone(),
            kind: ProviderHttpErrorKind::TooLarge,
            status: None,
            message: format!(
                "response of {bytes} bytes exceeds the {MAX_RESPONSE_BYTES} byte limit"
            ),
            retry_after: None,
        }
    }

    async fn send_with_retry(
        &self,
        url: String,
        headers: &[(&str, String)],
    ) -> Result<reqwest::Response, ProviderHttpError> {
        let attempts = self.retry.max_attempts.max(1);
        let mut attempt = 0usize;

        loop {
            attempt += 1;
            let response = self.send_once(&url, headers).await;
            match response {
                Ok(response) if should_retry_status(response.status()) => {
                    let status = response.status();
                    let retry_after = retry_after(response.headers());
                    let body = response.text().await.unwrap_or_default();
                    if attempt >= attempts {
                        return Err(self.status_error(status, body, retry_after));
                    }
                    self.sleep_before_retry(attempt, retry_after).await;
                }
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    let status = response.status();
                    let retry_after = retry_after(response.headers());
                    let body = response.text().await.unwrap_or_default();
                    return Err(self.status_error(status, body, retry_after));
                }
                Err(error) => {
                    if attempt >= attempts {
                        return Err(ProviderHttpError {
                            provider: self.provider.clone(),
                            kind: ProviderHttpErrorKind::Request,
                            status: None,
                            message: request_error_message(&error),
                            retry_after: None,
                        });
                    }
                    self.sleep_before_retry(attempt, None).await;
                }
            }
        }
    }

    async fn send_once(
        &self,
        url: &str,
        headers: &[(&str, String)],
    ) -> Result<reqwest::Response, reqwest::Error> {
        let mut request = self.http.get(url);
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        request.send().await
    }

    async fn sleep_before_retry(&self, attempt: usize, retry_after: Option<Duration>) {
        let delay = retry_after.unwrap_or_else(|| self.backoff_delay(attempt));
        tracing::debug!(
            provider = %self.provider,
            attempt,
            delay_ms = delay.as_millis(),
            "retrying provider request"
        );
        tokio::time::sleep(delay).await;
    }

    fn backoff_delay(&self, attempt: usize) -> Duration {
        let shift = attempt.saturating_sub(1).min(10) as u32;
        let multiplier = 1_u32 << shift;
        let delay = self.retry.base_delay.saturating_mul(multiplier);
        delay.min(self.retry.max_delay)
    }

    fn status_error(
        &self,
        status: StatusCode,
        body: String,
        retry_after: Option<Duration>,
    ) -> ProviderHttpError {
        let kind = match status {
            StatusCode::NOT_FOUND => ProviderHttpErrorKind::NotFound,
            StatusCode::TOO_MANY_REQUESTS => ProviderHttpErrorKind::RateLimited,
            s if s.is_server_error() => ProviderHttpErrorKind::Temporary,
            _ => ProviderHttpErrorKind::Http,
        };
        ProviderHttpError {
            provider: self.provider.clone(),
            kind,
            status: Some(status),
            message: body,
            retry_after,
        }
    }
}

/// Render every causal layer before `reqwest::Error` is reduced to the
/// provider-neutral error type. Reqwest deliberately keeps transport details
/// such as DNS and TLS failures in `source()`; its top-level Display is often
/// only "error sending request", which is not enough to repair a manifest or
/// diagnose the machine's network configuration.
///
/// The actual URL is omitted here. Callers already carry a separately
/// redacted URL, and a request URL may contain a custom integration's secret
/// query parameter.
fn request_error_message(error: &reqwest::Error) -> String {
    error_chain_message(error, error.url().map(reqwest::Url::as_str))
}

fn error_chain_message(error: &(dyn StdError + 'static), hidden_url: Option<&str>) -> String {
    let mut messages = Vec::new();
    let mut current = Some(error);
    while let Some(cause) = current {
        let mut message = cause.to_string();
        if let Some(url) = hidden_url {
            message = message.replace(url, "<request URL>");
        }
        if !message.is_empty() && messages.last() != Some(&message) {
            messages.push(message);
        }
        current = cause.source();
    }
    messages.join("\nCaused by: ")
}

fn should_retry_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct NestedError {
        message: &'static str,
        source: Option<Box<NestedError>>,
    }

    impl fmt::Display for NestedError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.message)
        }
    }

    impl StdError for NestedError {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            self.source
                .as_deref()
                .map(|source| source as &(dyn StdError + 'static))
        }
    }

    #[test]
    fn causal_request_details_are_preserved_and_the_actual_url_is_hidden() {
        let error = NestedError {
            message: "error sending request for url (https://secret.test/path)",
            source: Some(Box::new(NestedError {
                message: "dns error",
                source: Some(Box::new(NestedError {
                    message: "host not found",
                    source: None,
                })),
            })),
        };

        let rendered = error_chain_message(&error, Some("https://secret.test/path"));

        assert_eq!(
            rendered,
            "error sending request for url (<request URL>)\nCaused by: dns error\nCaused by: host not found"
        );
        assert!(!rendered.contains("secret.test"));
    }

    #[tokio::test]
    async fn a_real_transport_failure_keeps_its_reqwest_cause_chain() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("http://{address}/unreachable");
        let client = ProviderHttpClient::new("test").with_retry_policy(RetryPolicy {
            max_attempts: 1,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        });

        let error = client.get_status(url.clone(), &[]).await.unwrap_err();

        assert!(error.message.contains("<request URL>"), "{}", error.message);
        assert!(error.message.contains("Caused by:"), "{}", error.message);
        assert!(!error.message.contains(&url), "{}", error.message);
    }

    #[tokio::test]
    async fn retries_rate_limited_request_then_succeeds() {
        let mut server = mockito::Server::new_async().await;
        let first = server
            .mock("GET", "/resource")
            .with_status(429)
            .with_header("Retry-After", "0")
            .expect(1)
            .create_async()
            .await;
        let second = server
            .mock("GET", "/resource")
            .with_status(200)
            .with_body(r#"{"ok":true}"#)
            .expect(1)
            .create_async()
            .await;

        let client = ProviderHttpClient::new("test").with_retry_policy(RetryPolicy {
            max_attempts: 2,
            base_delay: Duration::from_millis(0),
            max_delay: Duration::from_millis(0),
        });
        let value = client
            .get_json::<serde_json::Value>(format!("{}/resource", server.url()), &[])
            .await
            .unwrap();

        assert_eq!(value["ok"], true);
        first.assert_async().await;
        second.assert_async().await;
    }

    #[tokio::test]
    async fn classifies_not_found_without_retry() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/missing")
            .with_status(404)
            .expect(1)
            .create_async()
            .await;

        let client = ProviderHttpClient::new("test").with_retry_policy(RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(0),
            max_delay: Duration::from_millis(0),
        });
        let error = client
            .get_status(format!("{}/missing", server.url()), &[])
            .await
            .unwrap_err();

        assert_eq!(error.kind, ProviderHttpErrorKind::NotFound);
        mock.assert_async().await;
    }
}
