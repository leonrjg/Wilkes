//! Session-only history of HTTP calls initiated by Wilkes. Library-internal
//! traffic is deliberately outside this boundary. Timing ends at response
//! headers, so streaming responses are never buffered or delayed for logging.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use std::time::Instant;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

const CAPACITY: usize = 500;
static HISTORY: LazyLock<Mutex<History>> = LazyLock::new(|| Mutex::new(History::default()));
static ENABLED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpCall {
    pub id: String,
    pub started_at: String,
    pub source: String,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Default)]
struct History {
    calls: VecDeque<HttpCall>,
}

impl History {
    fn update(&mut self, call: HttpCall, started: bool) {
        if let Some(held) = self.calls.iter_mut().find(|held| held.id == call.id) {
            *held = call;
        } else if started {
            self.calls.push_back(call);
            if self.calls.len() > CAPACITY {
                self.calls.pop_front();
            }
        }
        // A late completion must never restore a cleared or evicted call.
    }
}

// Enabled only by the host's logging initialization. Worker processes use
// init_logging_stderr and never collect or forward HTTP history.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

fn publish(call: HttpCall, started: bool) {
    HISTORY.lock().update(call, started);
}

pub fn get_calls() -> Vec<HttpCall> {
    HISTORY.lock().calls.iter().rev().cloned().collect()
}

pub fn clear_calls() {
    HISTORY.lock().calls.clear();
}

pub struct CallGuard {
    call: Option<HttpCall>,
    started: Instant,
}

impl CallGuard {
    pub fn start(source: &str, method: &str, url: &str) -> Self {
        if !ENABLED.load(Ordering::Relaxed) {
            return Self {
                call: None,
                started: Instant::now(),
            };
        }
        let call = HttpCall {
            id: uuid::Uuid::new_v4().to_string(),
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            source: source.into(),
            method: method.into(),
            url: url.into(),
            status: None,
            duration_ms: None,
            error: None,
        };
        publish(call.clone(), true);
        Self {
            call: Some(call),
            started: Instant::now(),
        }
    }

    pub fn finish(mut self, status: Option<u16>, error: Option<String>) {
        self.complete(status, error);
    }

    fn complete(&mut self, status: Option<u16>, error: Option<String>) {
        if let Some(mut call) = self.call.take() {
            call.status = status;
            call.error = error;
            call.duration_ms =
                Some(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64);
            publish(call, false);
        }
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        self.complete(None, Some("Cancelled before response headers".into()));
    }
}

#[async_trait::async_trait]
pub trait LoggedRequest {
    async fn logged_send(self, source: &str) -> Result<reqwest::Response, reqwest::Error>;
}

#[async_trait::async_trait]
impl LoggedRequest for reqwest::RequestBuilder {
    async fn logged_send(self, source: &str) -> Result<reqwest::Response, reqwest::Error> {
        let (client, request) = self.build_split();
        let request = request?;
        let guard = CallGuard::start(source, request.method().as_str(), request.url().as_str());
        let response = client.execute(request).await;
        guard.finish(
            response.as_ref().ok().map(|r| r.status().as_u16()),
            response
                .as_ref()
                .err()
                .map(|error| crate::network::error_chain_message(error, None)),
        );
        response
    }
}

pub trait LoggedBlockingRequest {
    fn logged_send(self, source: &str) -> Result<reqwest::blocking::Response, reqwest::Error>;
}

impl LoggedBlockingRequest for reqwest::blocking::RequestBuilder {
    fn logged_send(self, source: &str) -> Result<reqwest::blocking::Response, reqwest::Error> {
        let (client, request) = self.build_split();
        let request = request?;
        let guard = CallGuard::start(source, request.method().as_str(), request.url().as_str());
        let response = client.execute(request);
        guard.finish(
            response.as_ref().ok().map(|r| r.status().as_u16()),
            response
                .as_ref()
                .err()
                .map(|error| crate::network::error_chain_message(error, None)),
        );
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(index: usize) -> HttpCall {
        HttpCall {
            id: index.to_string(),
            started_at: String::new(),
            source: "test".into(),
            method: "GET".into(),
            url: "https://test/".into(),
            status: None,
            duration_ms: None,
            error: None,
        }
    }

    #[test]
    fn retains_latest_500_starts_and_never_restores_evicted_or_cleared_calls() {
        let mut history = History::default();
        for index in 0..501 {
            history.update(call(index), true);
        }
        assert_eq!(history.calls.len(), 500);
        assert_eq!(history.calls.front().unwrap().id, "1");
        let mut completed = call(0);
        completed.status = Some(200);
        completed.duration_ms = Some(10);
        history.update(completed.clone(), false);
        assert_eq!(history.calls.len(), 500);
        completed.id = "1".into();
        history.update(completed.clone(), false);
        assert_eq!(history.calls.front().unwrap().status, Some(200));
        history.calls.clear();
        history.update(completed, false);
        assert!(history.calls.is_empty());
    }

    #[tokio::test]
    async fn captures_complete_urls_and_transport_errors() {
        enable();
        let source = uuid::Uuid::new_v4().to_string();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/request")
            .match_query(mockito::Matcher::UrlEncoded(
                "arbitrary_credential".into(),
                "secret".into(),
            ))
            .with_status(503)
            .with_body("response remains readable")
            .expect(2)
            .create_async()
            .await;
        let url = format!("{}/request?arbitrary_credential=secret", server.url());
        let response = reqwest::Client::new()
            .post(&url)
            .header("Authorization", "Bearer hidden-token")
            .body("private payload")
            .logged_send(&source)
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 503);
        assert_eq!(response.text().await.unwrap(), "response remains readable");
        let blocking_source = source.clone();
        tokio::task::spawn_blocking(move || {
            let response = reqwest::blocking::Client::new()
                .post(&url)
                .logged_send(&blocking_source)
                .unwrap();
            assert_eq!(response.status().as_u16(), 503);
        })
        .await
        .unwrap();
        drop(CallGuard::start(
            &source,
            "GET",
            "https://user:password@example.test/cancelled?key=value#fragment",
        ));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let failed_url = format!("http://{address}/failure?token=private");
        assert!(reqwest::Client::new()
            .get(&failed_url)
            .logged_send(&source)
            .await
            .is_err());
        let held: Vec<_> = get_calls()
            .into_iter()
            .filter(|call| call.source == source)
            .collect();
        assert_eq!(held.len(), 4);
        assert!(held.iter().all(|call| call.duration_ms.is_some()));
        assert_eq!(
            held.iter().filter(|call| call.status == Some(503)).count(),
            2
        );
        assert!(held
            .iter()
            .any(|call| call.error.as_deref() == Some("Cancelled before response headers")));
        assert_eq!(
            held.iter()
                .filter(|call| call.url.ends_with("/request?arbitrary_credential=secret"))
                .count(),
            2
        );
        assert!(held
            .iter()
            .any(|call| call.url
                == "https://user:password@example.test/cancelled?key=value#fragment"));
        let failed = held.iter().find(|call| call.url == failed_url).unwrap();
        assert!(failed.error.as_ref().unwrap().contains(&failed_url));
        mock.assert_async().await;
    }
}
