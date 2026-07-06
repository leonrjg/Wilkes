use std::fmt;
use std::time::Duration;

use reqwest::header::RETRY_AFTER;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

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
    provider: &'static str,
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
}

#[derive(Debug)]
pub struct ProviderHttpError {
    pub provider: &'static str,
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
    pub fn new(provider: &'static str) -> Self {
        Self {
            provider,
            http: reqwest::Client::new(),
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
        let response = self.send_with_retry(url, headers).await?;
        response.json::<T>().await.map_err(|e| ProviderHttpError {
            provider: self.provider,
            kind: ProviderHttpErrorKind::Decode,
            status: None,
            message: e.to_string(),
            retry_after: None,
        })
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
                            provider: self.provider,
                            kind: ProviderHttpErrorKind::Request,
                            status: None,
                            message: error.to_string(),
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
            provider = self.provider,
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
            provider: self.provider,
            kind,
            status: Some(status),
            message: body,
            retry_after,
        }
    }
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
