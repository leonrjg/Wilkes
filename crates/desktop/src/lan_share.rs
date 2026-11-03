//! Sending files to other Wilkes instances on the local network.
//!
//! While sharing is on, this instance does three things and stops all three
//! together when it is turned off:
//!
//! - serves `POST /v1/files` on an arbitrary port of every IPv4 interface,
//!   writing what it receives into the active workspace's current root;
//! - announces that port over mDNS as `_wilkes._tcp`;
//! - browses for the same service, so the file menu can offer each instance it
//!   finds as a destination.
//!
//! A received file goes through [`AppContext::import_files_into_current_root`],
//! the same import a drop onto the file list uses, so which names and types a
//! root admits — and what happens on a name collision — has one answer.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use wilkes_api::commands::files::{validate_new_file_name, FileImportMode};
use wilkes_api::context::EventEmitter;
use wilkes_api::workspace::WorkspaceManager;
use wilkes_core::types::LanSharingSettings;

const SERVICE_TYPE: &str = "_wilkes._tcp.local.";
/// Bumped when the transfer route changes shape, so an instance never offers a
/// peer it cannot send to.
const PROTOCOL_VERSION: &str = "1";
const STAGING_DIRNAME: &str = "lan-incoming";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Another Wilkes instance found on the network.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct LanPeer {
    pub id: String,
    pub name: String,
    #[serde(skip)]
    addresses: Vec<Ipv4Addr>,
    #[serde(skip)]
    port: u16,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LanSharingStatus {
    enabled: bool,
    running: bool,
    /// What other instances list this one as.
    device_name: String,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ReceiveQuery {
    name: String,
    #[serde(default)]
    from: String,
}

#[derive(Serialize, Deserialize)]
struct Received {
    path: PathBuf,
}

struct Running {
    daemon: ServiceDaemon,
    /// Cancels transfers in flight, so turning sharing off does not wait on a
    /// sender that is still streaming a large file.
    cancel: CancellationToken,
    server_shutdown: tokio::sync::oneshot::Sender<()>,
    server_task: JoinHandle<()>,
    browse_task: JoinHandle<()>,
}

#[derive(Clone)]
struct ReceiverState {
    workspaces: Arc<WorkspaceManager>,
    events: Arc<dyn EventEmitter>,
    staging_dir: PathBuf,
    cancel: CancellationToken,
}

pub(crate) struct LanShareManager {
    /// Per process, so an instance can tell its own announcement from another
    /// instance's — including a second Wilkes on the same machine.
    instance_id: String,
    device_name: String,
    staging_dir: PathBuf,
    workspaces: Arc<WorkspaceManager>,
    events: Arc<dyn EventEmitter>,
    /// By mDNS full name, which is what a removal reports.
    peers: Arc<Mutex<HashMap<String, LanPeer>>>,
    running: tokio::sync::Mutex<Option<Running>>,
    last_error: Mutex<Option<String>>,
    client: reqwest::Client,
}

impl LanShareManager {
    pub(crate) fn new(
        data_dir: &Path,
        workspaces: Arc<WorkspaceManager>,
        events: Arc<dyn EventEmitter>,
    ) -> Self {
        let client = reqwest::Client::builder()
            // Peers are on the local network; a system proxy cannot reach them.
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .expect("a client with no proxy and a timeout always builds");
        Self {
            instance_id: uuid::Uuid::new_v4().simple().to_string(),
            device_name: device_name(),
            staging_dir: data_dir.join(STAGING_DIRNAME),
            workspaces,
            events,
            peers: Arc::new(Mutex::new(HashMap::new())),
            running: tokio::sync::Mutex::new(None),
            last_error: Mutex::new(None),
            client,
        }
    }

    /// Brings sharing into the state `settings` describes.
    pub(crate) async fn apply(&self, settings: &LanSharingSettings) -> Result<(), String> {
        let mut running = self.running.lock().await;
        if !settings.enabled {
            if let Some(previous) = running.take() {
                self.stop_running(previous).await;
            }
            *self.last_error.lock().unwrap() = None;
            return Ok(());
        }
        if running.is_some() {
            return Ok(());
        }
        match self.start().await {
            Ok(started) => {
                *running = Some(started);
                *self.last_error.lock().unwrap() = None;
                Ok(())
            }
            Err(error) => {
                let message = format!("Could not start sharing on the local network: {error:#}");
                error!("{message}");
                *self.last_error.lock().unwrap() = Some(message.clone());
                Err(message)
            }
        }
    }

    async fn start(&self) -> anyhow::Result<Running> {
        // Whatever a transfer cut short by the last shutdown left behind.
        if let Err(error) = tokio::fs::remove_dir_all(&self.staging_dir).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!("could not clear {}: {error}", self.staging_dir.display());
            }
        }
        tokio::fs::create_dir_all(&self.staging_dir).await?;

        let listener =
            tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))
                .await?;
        let port = listener.local_addr()?.port();

        let daemon = ServiceDaemon::new()?;
        // The listener is IPv4 only, so announcing IPv6 addresses would offer
        // peers an address nothing answers on.
        daemon.disable_interface(IfKind::IPv6)?;
        let instance = format!("wilkes-{}", self.instance_id);
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &format!("{instance}.local."),
            (),
            port,
            [
                ("id", self.instance_id.as_str()),
                ("name", self.device_name.as_str()),
                ("v", PROTOCOL_VERSION),
            ]
            .as_slice(),
        )?
        .enable_addr_auto();
        daemon.register(service)?;
        let browse = daemon.browse(SERVICE_TYPE)?;

        let cancel = CancellationToken::new();
        let app = receiver_router(ReceiverState {
            workspaces: Arc::clone(&self.workspaces),
            events: Arc::clone(&self.events),
            staging_dir: self.staging_dir.clone(),
            cancel: cancel.clone(),
        });
        let (server_shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server_task = tokio::spawn(async move {
            let served = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
            if let Err(error) = served {
                error!("LAN sharing listener stopped: {error}");
            }
        });

        let peers = Arc::clone(&self.peers);
        let events = Arc::clone(&self.events);
        let own_id = self.instance_id.clone();
        let browse_task = tokio::spawn(async move {
            while let Ok(event) = browse.recv_async().await {
                let changed = match event {
                    ServiceEvent::ServiceResolved(service) => {
                        let id = service.get_property_val_str("id").unwrap_or_default();
                        let version = service.get_property_val_str("v").unwrap_or_default();
                        if id.is_empty() || id == own_id || version != PROTOCOL_VERSION {
                            continue;
                        }
                        let mut addresses: Vec<Ipv4Addr> =
                            service.get_addresses_v4().into_iter().collect();
                        addresses.sort();
                        let peer = LanPeer {
                            id: id.to_string(),
                            name: service
                                .get_property_val_str("name")
                                .filter(|name| !name.is_empty())
                                .unwrap_or(id)
                                .to_string(),
                            addresses,
                            port: service.get_port(),
                        };
                        peers.lock().unwrap().insert(service.fullname.clone(), peer);
                        true
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        peers.lock().unwrap().remove(&fullname).is_some()
                    }
                    _ => false,
                };
                if changed {
                    emit_peers(&events, &peers);
                }
            }
        });

        info!(
            "sharing on the local network as {:?} on port {port}",
            self.device_name
        );
        Ok(Running {
            daemon,
            cancel,
            server_shutdown,
            server_task,
            browse_task,
        })
    }

    async fn stop_running(&self, running: Running) {
        running.cancel.cancel();
        running.browse_task.abort();
        if let Err(error) = running.daemon.shutdown() {
            warn!("mDNS daemon did not accept shutdown: {error}");
        }
        let _ = running.server_shutdown.send(());
        if let Err(error) = running.server_task.await {
            warn!("LAN sharing listener did not stop cleanly: {error}");
        }
        self.peers.lock().unwrap().clear();
        emit_peers(&self.events, &self.peers);
    }

    pub(crate) async fn stop(&self) {
        if let Some(running) = self.running.lock().await.take() {
            self.stop_running(running).await;
        }
    }

    pub(crate) async fn status(&self, settings: &LanSharingSettings) -> LanSharingStatus {
        LanSharingStatus {
            enabled: settings.enabled,
            running: self.running.lock().await.is_some(),
            device_name: self.device_name.clone(),
            error: self.last_error.lock().unwrap().clone(),
        }
    }

    pub(crate) fn peers(&self) -> Vec<LanPeer> {
        sorted_peers(&self.peers)
    }

    /// Sends each file in turn, stopping at the first the peer refuses.
    pub(crate) async fn send(&self, peer_id: &str, paths: Vec<PathBuf>) -> Result<(), String> {
        let peer = self
            .peers()
            .into_iter()
            .find(|peer| peer.id == peer_id)
            .ok_or_else(|| "That Wilkes is no longer on the network".to_string())?;
        let total = paths.len();
        for (sent, path) in paths.into_iter().enumerate() {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            if let Err(error) = self.send_one(&peer, &path).await {
                let progress = if sent > 0 {
                    format!(" ({sent} of {total} sent)")
                } else {
                    String::new()
                };
                return Err(format!(
                    "Could not send {name} to {}: {error}{progress}",
                    peer.name
                ));
            }
        }
        Ok(())
    }

    async fn send_one(&self, peer: &LanPeer, path: &Path) -> Result<(), String> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "it has no usable file name".to_string())?;
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("only files can be sent".to_string());
        }

        // A peer can announce an address this machine has no route to — a
        // VPN or container interface — so each is tried until one connects.
        let mut last_connect_error = None;
        for address in &peer.addresses {
            let file = tokio::fs::File::open(path)
                .await
                .map_err(|error| error.to_string())?;
            let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
            let response = self
                .client
                .post(format!("http://{address}:{}/v1/files", peer.port))
                .query(&[("name", name), ("from", self.device_name.as_str())])
                .header(reqwest::header::CONTENT_LENGTH, metadata.len())
                .body(body)
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) if error.is_connect() => {
                    warn!("could not reach {} at {address}: {error}", peer.name);
                    last_connect_error = Some(error.to_string());
                    continue;
                }
                Err(error) => return Err(error.to_string()),
            };
            if response.status().is_success() {
                return Ok(());
            }
            let status = response.status();
            let message = response.text().await.unwrap_or_default();
            return Err(if message.is_empty() {
                status.to_string()
            } else {
                message
            });
        }
        Err(last_connect_error.unwrap_or_else(|| "it announced no address".to_string()))
    }
}

fn sorted_peers(peers: &Mutex<HashMap<String, LanPeer>>) -> Vec<LanPeer> {
    let mut peers: Vec<LanPeer> = peers.lock().unwrap().values().cloned().collect();
    peers.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    peers
}

fn emit_peers(events: &Arc<dyn EventEmitter>, peers: &Mutex<HashMap<String, LanPeer>>) {
    match serde_json::to_value(sorted_peers(peers)) {
        Ok(payload) => events.emit("lan-peers-changed", payload),
        Err(error) => error!("could not serialize LAN peers: {error}"),
    }
}

/// The machine's name as people know it, without the `.local` mDNS suffix
/// macOS reports it with.
fn device_name() -> String {
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    let name = host.strip_suffix(".local").unwrap_or(&host);
    if name.is_empty() {
        "Wilkes".to_string()
    } else {
        name.to_string()
    }
}

fn receiver_router(state: ReceiverState) -> Router {
    Router::new()
        .route("/v1/files", post(receive_file))
        .with_state(state)
}

type ReceiveError = (StatusCode, String);

async fn receive_file(
    State(state): State<ReceiverState>,
    Query(query): Query<ReceiveQuery>,
    body: Body,
) -> Result<Json<Received>, ReceiveError> {
    validate_new_file_name(&query.name)
        .map_err(|error| (StatusCode::BAD_REQUEST, format!("{error:#}")))?;

    let ctx = state.workspaces.active();
    let settings = ctx.get_settings().await;
    let root = settings.last_directory.clone().ok_or_else(|| {
        (
            StatusCode::CONFLICT,
            "The receiving Wilkes has no folder open".to_string(),
        )
    })?;
    let limit = settings.max_download_bytes;

    // Dropped on every way out of this function, taking a partial file with
    // it: a refused, failed or cancelled transfer leaves nothing behind.
    let staging = tempfile::Builder::new()
        .prefix("incoming-")
        .tempdir_in(&state.staging_dir)
        .map_err(internal)?;
    let staged = staging.path().join(&query.name);

    tokio::select! {
        written = write_limited(body, &staged, limit) => written?,
        _ = state.cancel.cancelled() => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "The receiving Wilkes stopped sharing".to_string(),
            ));
        }
    }

    // Copy, not move: staging is in app data, and the root may be on another
    // volume, where a rename cannot reach.
    let imported = ctx
        .import_files_into_current_root(vec![staged], root, None, FileImportMode::Copy)
        .await
        .map_err(|error| (StatusCode::CONFLICT, format!("{error:#}")))?;
    let path = imported
        .into_iter()
        .next()
        .ok_or_else(|| internal("the import reported no file"))?;

    let sender = if query.from.is_empty() {
        "another Wilkes".to_string()
    } else {
        query.from
    };
    info!("received {} from {sender}", path.display());
    state.events.emit(
        "lan-file-received",
        serde_json::json!({
            "name": query.name,
            "from": sender,
            "path": path,
        }),
    );
    Ok(Json(Received { path }))
}

async fn write_limited(body: Body, target: &Path, limit: u64) -> Result<(), ReceiveError> {
    use futures_util::StreamExt;

    let too_large = || {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("The file is larger than the receiving Wilkes accepts ({limit} bytes)"),
        )
    };
    let mut file = tokio::fs::File::create(target).await.map_err(internal)?;
    let mut stream = body.into_data_stream();
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                format!("The transfer was interrupted: {error}"),
            )
        })?;
        written += chunk.len() as u64;
        if written > limit {
            return Err(too_large());
        }
        file.write_all(&chunk).await.map_err(internal)?;
    }
    file.flush().await.map_err(internal)?;
    Ok(())
}

fn internal(error: impl std::fmt::Display) -> ReceiveError {
    error!("LAN receive failed: {error}");
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    #[derive(Default)]
    struct RecordingEmitter(Mutex<Vec<(String, serde_json::Value)>>);

    impl EventEmitter for RecordingEmitter {
        fn emit(&self, name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((name.to_string(), payload));
        }
    }

    /// A receiver served on loopback without mDNS, and a manager that knows it
    /// as a peer: the transfer itself, end to end, with discovery left out.
    struct Harness {
        _data: TempDir,
        root: TempDir,
        workspaces: Arc<WorkspaceManager>,
        events: Arc<RecordingEmitter>,
        staging_dir: PathBuf,
        cancel: CancellationToken,
        sender: LanShareManager,
        peer: LanPeer,
    }

    impl Harness {
        async fn new() -> Self {
            let data = tempdir().unwrap();
            let root = tempdir().unwrap();
            let events = Arc::new(RecordingEmitter::default());
            let emitter: Arc<dyn EventEmitter> = events.clone();
            let (workspaces, _rx, _loop) = WorkspaceManager::new(
                data.path().to_path_buf(),
                data.path().join("settings.json"),
                Arc::clone(&emitter),
            )
            .unwrap();
            workspaces
                .active()
                .update_settings(serde_json::json!({ "last_directory": root.path() }))
                .await
                .unwrap();

            let staging_dir = data.path().join(STAGING_DIRNAME);
            std::fs::create_dir_all(&staging_dir).unwrap();
            let cancel = CancellationToken::new();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let app = receiver_router(ReceiverState {
                workspaces: Arc::clone(&workspaces),
                events: emitter,
                staging_dir: staging_dir.clone(),
                cancel: cancel.clone(),
            });
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });

            let sender = LanShareManager::new(
                data.path(),
                Arc::clone(&workspaces),
                Arc::new(RecordingEmitter::default()),
            );
            let peer = LanPeer {
                id: "peer".to_string(),
                name: "Peer".to_string(),
                addresses: vec![Ipv4Addr::LOCALHOST],
                port,
            };
            sender
                .peers
                .lock()
                .unwrap()
                .insert("peer._wilkes._tcp.local.".to_string(), peer.clone());
            Self {
                _data: data,
                root,
                workspaces,
                events,
                staging_dir,
                cancel,
                sender,
                peer,
            }
        }

        fn source(&self, name: &str, contents: &[u8]) -> (TempDir, PathBuf) {
            let dir = tempdir().unwrap();
            let path = dir.path().join(name);
            std::fs::write(&path, contents).unwrap();
            (dir, path)
        }

        fn staging_is_empty(&self) -> bool {
            std::fs::read_dir(&self.staging_dir)
                .unwrap()
                .next()
                .is_none()
        }
    }

    #[tokio::test]
    async fn a_sent_file_lands_in_the_receivers_current_root() {
        let harness = Harness::new().await;
        let (_src, path) = harness.source("notes.txt", b"hello from the other machine");

        harness.sender.send("peer", vec![path]).await.unwrap();

        let received = harness.root.path().join("notes.txt");
        assert_eq!(
            std::fs::read(&received).unwrap(),
            b"hello from the other machine"
        );
        assert!(harness.staging_is_empty());
        let events = harness.events.0.lock().unwrap();
        let (_, payload) = events
            .iter()
            .find(|(name, _)| name == "lan-file-received")
            .expect("the receiver says a file arrived");
        assert_eq!(payload["name"], "notes.txt");
        assert_eq!(payload["from"], harness.sender.device_name.as_str());
    }

    #[tokio::test]
    async fn a_name_already_in_the_root_is_refused_and_left_alone() {
        let harness = Harness::new().await;
        std::fs::write(harness.root.path().join("notes.txt"), b"mine").unwrap();
        let (_src, path) = harness.source("notes.txt", b"theirs");

        let error = harness.sender.send("peer", vec![path]).await.unwrap_err();

        assert!(error.contains("already exists"), "{error}");
        assert_eq!(
            std::fs::read(harness.root.path().join("notes.txt")).unwrap(),
            b"mine"
        );
        assert!(harness.staging_is_empty());
    }

    #[tokio::test]
    async fn a_type_the_library_does_not_hold_is_refused() {
        let harness = Harness::new().await;
        let (_src, path) = harness.source("tool.exe", &[0x4d, 0x5a, 0x90, 0x00, 0x03]);

        let error = harness.sender.send("peer", vec![path]).await.unwrap_err();

        assert!(error.contains("not supported"), "{error}");
        assert!(!harness.root.path().join("tool.exe").exists());
    }

    #[tokio::test]
    async fn a_file_over_the_download_limit_is_refused_and_nothing_is_kept() {
        let harness = Harness::new().await;
        harness
            .workspaces
            .active()
            .update_settings(serde_json::json!({ "max_download_bytes": 4 }))
            .await
            .unwrap();
        let (_src, path) = harness.source("notes.txt", b"more than four bytes");

        let error = harness.sender.send("peer", vec![path]).await.unwrap_err();

        assert!(error.contains("larger than"), "{error}");
        assert!(!harness.root.path().join("notes.txt").exists());
        assert!(harness.staging_is_empty());
    }

    #[tokio::test]
    async fn a_receiver_with_no_folder_open_says_so() {
        let harness = Harness::new().await;
        harness
            .workspaces
            .active()
            .update_settings(serde_json::json!({ "last_directory": null }))
            .await
            .unwrap();
        let (_src, path) = harness.source("notes.txt", b"hello");

        let error = harness.sender.send("peer", vec![path]).await.unwrap_err();

        assert!(error.contains("no folder open"), "{error}");
    }

    #[tokio::test]
    async fn a_name_that_walks_out_of_the_root_is_refused() {
        let harness = Harness::new().await;
        let response = harness
            .sender
            .client
            .post(format!("http://127.0.0.1:{}/v1/files", harness.peer.port))
            .query(&[("name", "../escape.txt")])
            .body("x")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!harness
            .root
            .path()
            .parent()
            .unwrap()
            .join("escape.txt")
            .exists());
    }

    #[tokio::test]
    async fn an_unknown_peer_is_reported_rather_than_guessed() {
        let harness = Harness::new().await;
        let (_src, path) = harness.source("notes.txt", b"hello");

        let error = harness.sender.send("gone", vec![path]).await.unwrap_err();

        assert!(error.contains("no longer on the network"), "{error}");
    }

    /// Turning sharing off must not wait on a sender that is still streaming.
    #[tokio::test]
    async fn stopping_cuts_a_transfer_in_flight_and_keeps_nothing() {
        use futures_util::StreamExt;

        let harness = Harness::new().await;
        let stalled = futures_util::stream::once(async {
            Ok::<_, std::io::Error>(bytes_of(b"the first part, then nothing"))
        })
        .chain(futures_util::stream::pending());
        let request = harness
            .sender
            .client
            .post(format!("http://127.0.0.1:{}/v1/files", harness.peer.port))
            .query(&[("name", "notes.txt")])
            .body(reqwest::Body::wrap_stream(stalled))
            .send();
        let request = tokio::spawn(request);
        // Until the first chunk is staged, there is nothing to cut short.
        while harness.staging_is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        harness.cancel.cancel();
        let response = tokio::time::timeout(Duration::from_secs(3), request)
            .await
            .expect("the receiver answers within the cancellation bound")
            .unwrap()
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!harness.root.path().join("notes.txt").exists());
        assert!(harness.staging_is_empty());
    }

    /// Two instances over real mDNS. Ignored by default: it needs multicast
    /// on a network interface, which CI runners and sandboxes often lack.
    #[tokio::test]
    #[ignore]
    async fn two_instances_find_each_other_and_transfer() {
        let first = Harness::new().await;
        let second = Harness::new().await;
        let a = LanShareManager::new(
            first._data.path(),
            Arc::clone(&first.workspaces),
            Arc::new(RecordingEmitter::default()),
        );
        let b = LanShareManager::new(
            second._data.path(),
            Arc::clone(&second.workspaces),
            Arc::new(RecordingEmitter::default()),
        );
        let on = LanSharingSettings { enabled: true };
        a.apply(&on).await.unwrap();
        b.apply(&on).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let peer = loop {
            if let Some(peer) = a.peers().into_iter().find(|peer| peer.id == b.instance_id) {
                break peer;
            }
            assert!(tokio::time::Instant::now() < deadline, "b was never found");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert!(a.peers().iter().all(|peer| peer.id != a.instance_id));

        let (_src, path) = first.source("notes.txt", b"over mDNS");
        a.send(&peer.id, vec![path]).await.unwrap();
        assert_eq!(
            std::fs::read(second.root.path().join("notes.txt")).unwrap(),
            b"over mDNS"
        );

        let started = std::time::Instant::now();
        a.apply(&LanSharingSettings { enabled: false })
            .await
            .unwrap();
        b.stop().await;
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(a.peers().is_empty());
    }

    fn bytes_of(data: &'static [u8]) -> axum::body::Bytes {
        axum::body::Bytes::from_static(data)
    }
}
