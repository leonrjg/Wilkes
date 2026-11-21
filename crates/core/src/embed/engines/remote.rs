//! Embedding on another Wilkes instance.
//!
//! The host is a Wilkes reached over its HTTP API. It embeds with the model
//! this instance names, in its own worker, and answers with the vectors and the
//! identity of the space they are in. Nothing else crosses: the library, the
//! index and extraction stay here, and the host keeps no record of the request.
//!
//! The pieces map onto the local ones one for one, so nothing downstream can
//! tell the difference:
//!
//! - [`RemoteInstaller`] is the [`EmbedderInstaller`]: `install` asks the host
//!   whether it has the model, has it download the model with progress when it
//!   does not, and settles the space's identity with one probe.
//! - [`RemoteEmbedder`] is the [`Embedder`]: it posts texts to the host's
//!   passage and query routes, whose role the route decides, never a flag.
//! - [`RemoteSeal`] is what a cancellation does to it, in place of the kill a
//!   local worker gets: in-flight requests are dropped at once and new ones
//!   refused until the cancelled work has stopped.
//!
//! Every failure to get vectors from the host is a [`WorkerFault`], so a build
//! stops rather than marking each document failed one batch at a time —
//! exactly what a local worker that died or refused would do.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::embed::installer::EmbedderInstaller;
use crate::embed::{EmbeddedTexts, Embedder, EmbeddingSpaceIdentity};
use crate::models::progress::{EmbedProgress, ProgressTx};
use crate::types::{EmbedderCapabilityManifest, EmbedderModel, EmbeddingEngine};
use crate::worker::fault::WorkerFault;

/// Reaching the host, not the answer: a host that is off is known in this
/// long, which keeps a cancelled or doomed build inside its deadline.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// One batch on the host. Generous because the first request after an
/// eviction reloads the model; finite because a host that stopped answering
/// mid-batch would otherwise hold a build forever.
const EMBED_TIMEOUT: Duration = Duration::from_secs(300);
/// Texts per request. Indexing hands over a whole queue of chunks at once; a
/// request per slice keeps each round trip short and under the host's body
/// limit.
const MAX_TEXTS_PER_REQUEST: usize = 256;
/// Bytes of text per request, against the host's 2 MiB default body limit with
/// room for JSON escaping.
const MAX_TEXT_BYTES_PER_REQUEST: usize = 1024 * 1024;

/// What names a model on the host: an engine and that engine's model id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbedderAddress {
    pub engine: EmbeddingEngine,
    pub model: EmbedderModel,
}

/// The two roles an asymmetric model distinguishes, as the route that asks for
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteRole {
    Passage,
    Query,
}

impl RemoteRole {
    fn path(self) -> &'static str {
        match self {
            Self::Passage => "/api/embed/text",
            Self::Query => "/api/embed/query",
        }
    }
}

/// One event of the host's install stream.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum InstallEvent {
    Progress(EmbedProgress),
    Done,
    Error(String),
}

/// Cancellation for remote embedding, owned by the context that seals its
/// workers.
///
/// Sealed, every request in flight returns at once and every new one fails
/// before it is sent; unsealed, requests go out again. A request takes the
/// token current when it starts, so an unseal never revives a cancelled one.
#[derive(Debug, Default)]
pub struct RemoteSeal {
    current: Mutex<CancellationToken>,
}

impl RemoteSeal {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn seal(&self) {
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .cancel();
    }

    pub fn unseal(&self) {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.is_cancelled() {
            *current = CancellationToken::new();
        }
    }

    fn token(&self) -> CancellationToken {
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

fn cancelled() -> anyhow::Error {
    WorkerFault::gone("remote embedding was cancelled")
}

/// A Wilkes instance's HTTP API, as the embedding host.
#[derive(Clone, Debug)]
pub struct RemoteHost {
    base_url: String,
    token: Option<String>,
    client: reqwest::Client,
}

impl RemoteHost {
    /// `address` is the host's base URL; a bare `host:port` is read as HTTP.
    pub fn new(address: &str, token: Option<String>) -> anyhow::Result<Self> {
        let base_url = normalize_address(address)?;
        let client = reqwest::Client::builder()
            // A host on the LAN or a tailnet is not reachable through a
            // system proxy.
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        Ok(Self {
            base_url,
            token,
            client,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let builder = self
            .client
            .request(method, format!("{}{path}", self.base_url));
        match &self.token {
            Some(token) => builder.bearer_auth(token),
            None => builder,
        }
    }

    /// Send a request and turn every way it can fail into a fault: an
    /// unreachable host is gone, and any refusal it answers with is reported.
    async fn send(&self, request: reqwest::RequestBuilder) -> anyhow::Result<reqwest::Response> {
        let response = request.send().await.map_err(|error| {
            WorkerFault::gone(format!(
                "embedding host {} is unreachable: {error}",
                self.base_url
            ))
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(WorkerFault::reported(format!(
                "embedding host {} refused the token",
                self.base_url
            )));
        }
        let body = response.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|e| e.as_str())
                    .map(str::to_string)
            })
            .unwrap_or(body);
        Err(WorkerFault::reported(format!(
            "embedding host {} answered {status}: {detail}",
            self.base_url
        )))
    }

    /// What the host can embed with.
    pub async fn capabilities(&self) -> anyhow::Result<EmbedderCapabilityManifest> {
        let response = self
            .send(self.request(reqwest::Method::GET, "/api/embed/capabilities"))
            .await?;
        response.json().await.map_err(|error| {
            WorkerFault::reported(format!(
                "embedding host {} sent an unreadable model list: {error}",
                self.base_url
            ))
        })
    }

    /// Have the host download `address`, passing its progress on to `tx`.
    pub async fn install(&self, address: &EmbedderAddress, tx: &ProgressTx) -> anyhow::Result<()> {
        let mut response = self
            .send(
                self.request(reqwest::Method::POST, "/api/embed/install")
                    .json(address),
            )
            .await?;
        let mut stream = SseBuffer::default();
        loop {
            let chunk = response.chunk().await.map_err(|error| {
                WorkerFault::gone(format!(
                    "embedding host {} stopped answering during the download: {error}",
                    self.base_url
                ))
            })?;
            let Some(chunk) = chunk else {
                return Err(WorkerFault::gone(format!(
                    "embedding host {} ended the download without finishing it",
                    self.base_url
                )));
            };
            for event in stream.push(&chunk)? {
                match event {
                    InstallEvent::Progress(progress) => {
                        // The listener may have gone; the download goes on
                        // regardless, and the result is what decides.
                        let _ = tx.send(progress).await;
                    }
                    InstallEvent::Done => return Ok(()),
                    InstallEvent::Error(error) => {
                        return Err(WorkerFault::reported(format!(
                            "embedding host {} could not download {}: {error}",
                            self.base_url,
                            address.model.model_id()
                        )))
                    }
                }
            }
        }
    }

    /// Embed one request's worth of texts in `role`.
    pub async fn embed(
        &self,
        role: RemoteRole,
        address: &EmbedderAddress,
        texts: &[&str],
    ) -> anyhow::Result<EmbeddedTexts> {
        let body = serde_json::json!({ "texts": texts, "embedder": address });
        let response = self
            .send(
                self.request(reqwest::Method::POST, role.path())
                    .timeout(EMBED_TIMEOUT)
                    .json(&body),
            )
            .await?;
        response.json().await.map_err(|error| {
            WorkerFault::gone(format!(
                "embedding host {} sent an unreadable reply: {error}",
                self.base_url
            ))
        })
    }
}

/// `http://host:port` with no trailing slash. A bare `host:port` is HTTP; any
/// scheme but HTTP or HTTPS is refused.
pub fn normalize_address(address: &str) -> anyhow::Result<String> {
    let trimmed = address.trim().trim_end_matches('/');
    anyhow::ensure!(!trimmed.is_empty(), "The embedding host has no address");
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let url = reqwest::Url::parse(&with_scheme)
        .map_err(|error| anyhow::anyhow!("'{address}' is not an address: {error}"))?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https"),
        "The embedding host must be reached over HTTP or HTTPS, not {}",
        url.scheme()
    );
    anyhow::ensure!(url.host_str().is_some(), "'{address}' names no host");
    anyhow::ensure!(
        url.path() == "/" || url.path().is_empty(),
        "The embedding host's address is its base URL, with no path"
    );
    Ok(with_scheme)
}

/// Server-sent events, split out of a byte stream that may cut one anywhere.
#[derive(Default)]
struct SseBuffer {
    pending: Vec<u8>,
}

impl SseBuffer {
    fn push(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<InstallEvent>> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = find_event_end(&self.pending) {
            let raw: Vec<u8> = self.pending.drain(..end.0 + end.1).collect();
            let text = String::from_utf8_lossy(&raw[..end.0]).into_owned();
            if let Some(event) = parse_event(&text)? {
                events.push(event);
            }
        }
        Ok(events)
    }
}

/// Where the first complete event ends: its length and the separator's.
fn find_event_end(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    let crlf = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

fn parse_event(text: &str) -> anyhow::Result<Option<InstallEvent>> {
    let mut name = None;
    let mut data = String::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    // A comment-only event is a keep-alive.
    let Some(name) = name else {
        return Ok(None);
    };
    let event = match name.as_str() {
        "progress" => InstallEvent::Progress(serde_json::from_str(&data).map_err(|error| {
            WorkerFault::gone(format!(
                "unreadable download progress from the host: {error}"
            ))
        })?),
        "done" => InstallEvent::Done,
        "error" => InstallEvent::Error(data),
        other => {
            return Err(WorkerFault::gone(format!(
                "the host sent an unknown download event '{other}'"
            )))
        }
    };
    Ok(Some(event))
}

/// Split `texts` into request-sized slices.
fn request_slices<'a>(texts: &'a [&'a str]) -> Vec<&'a [&'a str]> {
    let mut slices = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (index, text) in texts.iter().enumerate() {
        let count = index - start;
        if count > 0
            && (count >= MAX_TEXTS_PER_REQUEST || bytes + text.len() > MAX_TEXT_BYTES_PER_REQUEST)
        {
            slices.push(&texts[start..index]);
            start = index;
            bytes = 0;
        }
        bytes += text.len();
    }
    if start < texts.len() {
        slices.push(&texts[start..]);
    }
    slices
}

/// The identity a host reply claims, required: a host that does not name the
/// space its vectors are in cannot be checked against an index.
fn reply_identity(
    reply: &EmbeddedTexts,
    host: &RemoteHost,
) -> anyhow::Result<EmbeddingSpaceIdentity> {
    reply.embedding_space_identity.clone().ok_or_else(|| {
        WorkerFault::reported(format!(
            "embedding host {} does not name the space its vectors are in; it needs a newer \
             Wilkes",
            host.base_url
        ))
    })
}

/// The installer for a model on a remote host.
pub struct RemoteInstaller {
    host: Arc<RemoteHost>,
    seal: Arc<RemoteSeal>,
    address: EmbedderAddress,
    /// Captured at construction, which is always in an async context, so the
    /// embedder can be driven from blocking threads.
    tokio_handle: tokio::runtime::Handle,
    /// Settled by `install`: whether the host has the model, and the identity
    /// of the space it embeds into. `build` hands out what `install` found;
    /// it cannot ask itself, because it is not async.
    available: AtomicBool,
    identity: Mutex<Option<EmbeddingSpaceIdentity>>,
}

impl RemoteInstaller {
    pub fn new(
        host: Arc<RemoteHost>,
        seal: Arc<RemoteSeal>,
        engine: EmbeddingEngine,
        model: EmbedderModel,
    ) -> Self {
        Self {
            host,
            seal,
            address: EmbedderAddress { engine, model },
            tokio_handle: tokio::runtime::Handle::current(),
            available: AtomicBool::new(false),
            identity: Mutex::new(None),
        }
    }

    async fn install_unsealed(&self, tx: &ProgressTx) -> anyhow::Result<()> {
        let manifest = self.host.capabilities().await?;
        let model = manifest
            .models
            .iter()
            .find(|model| {
                model.engine == self.address.engine
                    && model.model_id == self.address.model.model_id()
            })
            .ok_or_else(|| {
                WorkerFault::reported(format!(
                    "embedding host {} does not offer {} model {}",
                    self.host.base_url,
                    self.address.engine.as_str(),
                    self.address.model.model_id()
                ))
            })?;
        if !model.locally_available {
            // A download nobody is watching is not started. Restoring a
            // workspace or loading a managed corpus installs with no one to
            // report to; letting those fetch a model onto the host would be a
            // download with no progress and nothing to cancel it from. The
            // model list's download, and a build, both listen.
            if tx.is_closed() {
                return Err(WorkerFault::reported(format!(
                    "embedding host {} does not have {} model {}; download it from the model \
                     list first",
                    self.host.base_url,
                    self.address.engine.as_str(),
                    self.address.model.model_id()
                )));
            }
            self.host.install(&self.address, tx).await?;
        }
        self.available.store(true, Ordering::Release);

        // The probe that settles the space: the host loads the model, and its
        // reply names the identity every later batch must repeat.
        let reply = self
            .host
            .embed(RemoteRole::Passage, &self.address, &["wilkes"])
            .await?;
        let identity = reply_identity(&reply, &self.host)?;
        anyhow::ensure!(
            reply.vectors.len() == 1 && reply.vectors[0].len() == identity.dimension,
            "embedding host {} answered the probe with vectors of the wrong shape",
            self.host.base_url
        );
        *self
            .identity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(identity);
        Ok(())
    }
}

#[async_trait]
impl EmbedderInstaller for RemoteInstaller {
    fn is_available(&self, _data_dir: &std::path::Path) -> bool {
        self.available.load(Ordering::Acquire)
    }

    async fn install(&self, _data_dir: &std::path::Path, tx: ProgressTx) -> anyhow::Result<()> {
        let token = self.seal.token();
        tokio::select! {
            _ = token.cancelled() => Err(cancelled()),
            result = self.install_unsealed(&tx) => result,
        }
    }

    fn uninstall(&self, _data_dir: &std::path::Path) -> anyhow::Result<()> {
        anyhow::bail!(
            "{} is on the embedding host {}; remove it there",
            self.address.model.model_id(),
            self.host.base_url
        )
    }

    fn build(&self, _data_dir: &std::path::Path) -> anyhow::Result<Arc<dyn Embedder>> {
        let identity = self
            .identity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| {
                anyhow::anyhow!("a remote embedder is built after its install has probed the host")
            })?;
        Ok(Arc::new(RemoteEmbedder {
            host: Arc::clone(&self.host),
            seal: Arc::clone(&self.seal),
            address: self.address.clone(),
            tokio_handle: self.tokio_handle.clone(),
            identity,
        }))
    }
}

/// An [`Embedder`] whose model runs on another Wilkes.
pub struct RemoteEmbedder {
    host: Arc<RemoteHost>,
    seal: Arc<RemoteSeal>,
    address: EmbedderAddress,
    tokio_handle: tokio::runtime::Handle,
    identity: EmbeddingSpaceIdentity,
}

impl RemoteEmbedder {
    fn send(&self, role: RemoteRole, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let token = self.seal.token();
        if token.is_cancelled() {
            return Err(cancelled());
        }
        let expected = self.identity.id();
        self.tokio_handle.block_on(async {
            let mut vectors = Vec::with_capacity(texts.len());
            for slice in request_slices(texts) {
                let reply = tokio::select! {
                    _ = token.cancelled() => return Err(cancelled()),
                    reply = self.host.embed(role, &self.address, slice) => reply?,
                };
                let identity = reply_identity(&reply, &self.host)?;
                if identity.id() != expected {
                    return Err(WorkerFault::reported(format!(
                        "embedding host {} answered in space {} where {} was expected",
                        self.host.base_url,
                        identity.id().as_str(),
                        expected.as_str()
                    )));
                }
                if reply.vectors.len() != slice.len() {
                    return Err(WorkerFault::reported(format!(
                        "embedding host {} returned {} vectors for {} texts",
                        self.host.base_url,
                        reply.vectors.len(),
                        slice.len()
                    )));
                }
                vectors.extend(reply.vectors);
            }
            Ok(vectors)
        })
    }
}

impl Embedder for RemoteEmbedder {
    /// The passage route: the host has no route that embeds without a role,
    /// and every caller of `embed` in Wilkes stores what it gets.
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.send(RemoteRole::Passage, texts)
    }

    fn embed_query(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.send(RemoteRole::Query, texts)
    }

    fn embed_passages(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.send(RemoteRole::Passage, texts)
    }

    fn model_id(&self) -> &str {
        self.address.model.model_id()
    }

    fn dimension(&self) -> usize {
        self.identity.dimension
    }

    fn engine(&self) -> EmbeddingEngine {
        self.address.engine
    }

    fn embedding_space_identity(&self) -> EmbeddingSpaceIdentity {
        self.identity.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::fault::{fault_of, Fault};

    fn identity() -> EmbeddingSpaceIdentity {
        EmbeddingSpaceIdentity::for_test(EmbeddingEngine::Fastembed, "AllMiniLML6V2", 3)
    }

    fn reply(count: usize) -> String {
        serde_json::to_string(&EmbeddedTexts {
            embedding_space_id: Some(identity().id().0),
            engine: "fastembed".to_string(),
            model_id: "AllMiniLML6V2".to_string(),
            dimension: 3,
            vectors: vec![vec![0.1, 0.2, 0.3]; count],
            embedding_space_identity: Some(identity()),
        })
        .unwrap()
    }

    fn manifest(locally_available: bool) -> String {
        serde_json::json!({
            "engines": ["Fastembed"],
            "roles": ["query", "passage"],
            "models": [{
                "engine": "Fastembed",
                "model_id": "AllMiniLML6V2",
                "display_name": "all-MiniLM-L6-v2",
                "description": "",
                "repository_id": null,
                "dimension": 3,
                "supported_dimensions": [3],
                "query_prefix": null,
                "passage_prefix": null,
                "prefix_source": "not_documented",
                "max_input_tokens": null,
                "locally_available": locally_available,
                "size_bytes": null,
                "preferred_batch_size": null,
                "catalogued": true,
                "is_default": true,
                "is_recommended": true
            }]
        })
        .to_string()
    }

    fn installer(url: &str, token: Option<&str>, seal: Arc<RemoteSeal>) -> RemoteInstaller {
        RemoteInstaller::new(
            Arc::new(RemoteHost::new(url, token.map(str::to_string)).unwrap()),
            seal,
            EmbeddingEngine::Fastembed,
            EmbedderModel("AllMiniLML6V2".to_string()),
        )
    }

    #[test]
    fn addresses_are_base_urls() {
        assert_eq!(
            normalize_address("mini.local:2020").unwrap(),
            "http://mini.local:2020"
        );
        assert_eq!(
            normalize_address(" https://mini.example/ ").unwrap(),
            "https://mini.example"
        );
        assert!(normalize_address("").is_err());
        assert!(normalize_address("ftp://mini:21").is_err());
        assert!(normalize_address("http://mini:2020/api").is_err());
    }

    #[test]
    fn slices_respect_both_bounds() {
        let many = vec!["x"; MAX_TEXTS_PER_REQUEST * 2 + 1];
        let slices = request_slices(&many);
        assert_eq!(
            slices.iter().map(|s| s.len()).collect::<Vec<_>>(),
            [MAX_TEXTS_PER_REQUEST, MAX_TEXTS_PER_REQUEST, 1]
        );

        let big = "y".repeat(MAX_TEXT_BYTES_PER_REQUEST / 2 + 1);
        let large = vec![big.as_str(); 3];
        assert_eq!(request_slices(&large).len(), 3);

        // A single text larger than the bound still goes, alone.
        let huge = "z".repeat(MAX_TEXT_BYTES_PER_REQUEST + 1);
        assert_eq!(request_slices(&[huge.as_str()]).len(), 1);
        assert!(request_slices(&[]).is_empty());
    }

    #[test]
    fn install_events_survive_arbitrary_cuts() {
        let stream = "event: progress\ndata: {\"Download\":{\"bytes_received\":5,\"total_bytes\":10,\"done\":false}}\n\n:\n\nevent: done\ndata: \n\n";
        let mut buffer = SseBuffer::default();
        let mut events = Vec::new();
        for byte in stream.as_bytes() {
            events.extend(buffer.push(&[*byte]).unwrap());
        }
        assert!(matches!(
            events.as_slice(),
            [
                InstallEvent::Progress(EmbedProgress::Download(_)),
                InstallEvent::Done
            ]
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_model_on_the_host_is_probed_and_embeds_in_slices() {
        let mut server = mockito::Server::new_async().await;
        let caps = server
            .mock("GET", "/api/embed/capabilities")
            .match_header("authorization", "Bearer secret")
            .with_body(manifest(true))
            .create_async()
            .await;
        let probe = server
            .mock("POST", "/api/embed/text")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "texts": ["wilkes"],
                "embedder": {"engine": "Fastembed", "model": "AllMiniLML6V2"}
            })))
            .with_body(reply(1))
            .create_async()
            .await;
        let query = server
            .mock("POST", "/api/embed/query")
            .with_body(reply(2))
            .create_async()
            .await;

        let installer = installer(&server.url(), Some("secret"), RemoteSeal::new());
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        installer
            .install(std::path::Path::new("."), tx)
            .await
            .unwrap();
        assert!(installer.is_available(std::path::Path::new(".")));
        let embedder = installer.build(std::path::Path::new(".")).unwrap();
        assert_eq!(embedder.dimension(), 3);
        assert_eq!(embedder.embedding_space_identity(), identity());

        let vectors = tokio::task::spawn_blocking(move || embedder.embed_query(&["a", "b"]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(vectors.len(), 2);
        caps.assert_async().await;
        probe.assert_async().await;
        query.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_missing_model_is_downloaded_with_progress() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/api/embed/capabilities")
            .with_body(manifest(false))
            .create_async()
            .await;
        let install = server
            .mock("POST", "/api/embed/install")
            .with_header("content-type", "text/event-stream")
            .with_body(
                "event: progress\ndata: {\"Download\":{\"bytes_received\":1,\"total_bytes\":2,\"done\":false}}\n\nevent: done\ndata: \n\n",
            )
            .create_async()
            .await;
        server
            .mock("POST", "/api/embed/text")
            .with_body(reply(1))
            .create_async()
            .await;

        let installer = installer(&server.url(), None, RemoteSeal::new());
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        installer
            .install(std::path::Path::new("."), tx)
            .await
            .unwrap();
        install.assert_async().await;
        assert!(matches!(rx.recv().await, Some(EmbedProgress::Download(_))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_missing_model_is_not_downloaded_when_nobody_watches() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/api/embed/capabilities")
            .with_body(manifest(false))
            .create_async()
            .await;
        let install = server
            .mock("POST", "/api/embed/install")
            .expect(0)
            .create_async()
            .await;

        let installer = installer(&server.url(), None, RemoteSeal::new());
        let (tx, _) = tokio::sync::mpsc::channel(8);
        let error = installer
            .install(std::path::Path::new("."), tx)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("download it from the model list"));
        assert!(!installer.is_available(std::path::Path::new(".")));
        install.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_token_and_an_absent_host_are_faults() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/api/embed/capabilities")
            .with_status(401)
            .create_async()
            .await;
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let error = installer(&server.url(), Some("wrong"), RemoteSeal::new())
            .install(std::path::Path::new("."), tx.clone())
            .await
            .unwrap_err();
        assert_eq!(fault_of(&error), Some(Fault::Reported));

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = installer(&format!("http://{address}"), None, RemoteSeal::new())
            .install(std::path::Path::new("."), tx)
            .await
            .unwrap_err();
        assert_eq!(fault_of(&error), Some(Fault::Gone));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sealing_drops_a_stalled_request_inside_the_deadline() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/api/embed/capabilities")
            .with_body(manifest(true))
            .create_async()
            .await;
        server
            .mock("POST", "/api/embed/text")
            .with_body(reply(1))
            .create_async()
            .await;
        let seal = RemoteSeal::new();
        let installer = installer(&server.url(), None, Arc::clone(&seal));
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        installer
            .install(std::path::Path::new("."), tx)
            .await
            .unwrap();
        let embedder = installer.build(std::path::Path::new(".")).unwrap();

        // A host that accepts the request and never answers it.
        let stalled = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stalled_url = format!("http://{}", stalled.local_addr().unwrap());
        let accept = tokio::spawn(async move {
            let (socket, _) = stalled.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
            drop(socket);
        });
        let embedder = RemoteEmbedder {
            host: Arc::new(RemoteHost::new(&stalled_url, None).unwrap()),
            seal: Arc::clone(&seal),
            address: EmbedderAddress {
                engine: embedder.engine(),
                model: EmbedderModel(embedder.model_id().to_string()),
            },
            tokio_handle: tokio::runtime::Handle::current(),
            identity: embedder.embedding_space_identity(),
        };

        let started = std::time::Instant::now();
        let call = tokio::task::spawn_blocking(move || embedder.embed_passages(&["text"]));
        tokio::time::sleep(Duration::from_millis(200)).await;
        seal.seal();
        let error = call.await.unwrap().unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(fault_of(&error), Some(Fault::Gone));

        accept.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sealed_embedder_refuses_until_unsealed() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/api/embed/capabilities")
            .with_body(manifest(true))
            .create_async()
            .await;
        server
            .mock("POST", "/api/embed/text")
            .with_body(reply(1))
            .expect_at_least(2)
            .create_async()
            .await;
        let seal = RemoteSeal::new();
        let installer = installer(&server.url(), None, Arc::clone(&seal));
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        installer
            .install(std::path::Path::new("."), tx)
            .await
            .unwrap();
        let embedder = installer.build(std::path::Path::new(".")).unwrap();

        seal.seal();
        let sealed = Arc::clone(&embedder);
        let error = tokio::task::spawn_blocking(move || sealed.embed_passages(&["t"]))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(fault_of(&error), Some(Fault::Gone));

        seal.unseal();
        let vectors = tokio::task::spawn_blocking(move || embedder.embed_passages(&["t"]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(vectors.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reply_in_another_space_is_refused() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/api/embed/capabilities")
            .with_body(manifest(true))
            .create_async()
            .await;
        server
            .mock("POST", "/api/embed/text")
            .with_body(reply(1))
            .create_async()
            .await;
        let other = serde_json::to_string(&EmbeddedTexts {
            embedding_space_id: None,
            engine: "fastembed".to_string(),
            model_id: "Other".to_string(),
            dimension: 3,
            vectors: vec![vec![0.0; 3]],
            embedding_space_identity: Some(EmbeddingSpaceIdentity::for_test(
                EmbeddingEngine::Fastembed,
                "Other",
                3,
            )),
        })
        .unwrap();
        server
            .mock("POST", "/api/embed/query")
            .with_body(other)
            .create_async()
            .await;
        let installer = installer(&server.url(), None, RemoteSeal::new());
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        installer
            .install(std::path::Path::new("."), tx)
            .await
            .unwrap();
        let embedder = installer.build(std::path::Path::new(".")).unwrap();
        let error = tokio::task::spawn_blocking(move || embedder.embed_query(&["q"]))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(fault_of(&error), Some(Fault::Reported));
    }
}
