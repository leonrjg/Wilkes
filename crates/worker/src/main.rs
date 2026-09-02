use std::io::BufRead;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use wilkes_core::embed::dispatch;
use wilkes_core::extract::image::dispatch as recognize_dispatch;
use wilkes_core::extract::image::ocr::OcrEngine;
use wilkes_core::extract::image::{LayoutModel, WireLayoutRegion};
use wilkes_core::extract::image::worker_ocr;
use wilkes_core::generate::engines::dispatch as generate_dispatch;
use wilkes_core::generate::{Generated, Generator};
use wilkes_core::models::progress::EmbedProgress;
use wilkes_core::types::{EmbedderModel, GeneratorModel};
use wilkes_core::worker::ipc::{CancelSignal, WorkerEvent, WorkerRequest, WorkerRole};

/// How many models one worker process keeps resident at once.
///
/// Three, because reading a document needs three at the same time: the layout
/// detector that marks out what a page holds, the page reader, and the formula
/// reader for the areas the detector called formulas. They alternate
/// continuously within a single document, so a process that held one would
/// spend the run loading and unloading rather than reading.
///
/// Bounded rather than open, because a map with no bound is a memory leak that
/// only shows up on the machine of whoever switches models most. Least
/// recently used is evicted, which for the reading pass above never evicts
/// anything and for someone trying recognizers in the settings panel evicts
/// the one they moved away from.
const MAX_RESIDENT_MODELS: usize = 3;

/// Identity of one resident model.
///
/// A worker holds up to [`MAX_RESIDENT_MODELS`] of these, all of one
/// [`WorkerKind`](wilkes_core::worker::ipc::WorkerKind) — the host runs a
/// separate `WorkerManager` per kind, and a process never changes kind. Within
/// a kind the role, the model and the device all vary, and each combination is
/// its own entry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LoadedModelKey {
    role: WorkerRole,
    model: String,
    /// The model cache root, not the index directory: a reload is warranted
    /// when the artefacts change, and the index a build happens to write to
    /// says nothing about them.
    model_dir: std::path::PathBuf,
    device: String,
}

impl LoadedModelKey {
    fn from_request(req: &WorkerRequest) -> Self {
        Self {
            role: req.role,
            model: req.model.clone(),
            model_dir: req.model_dir.clone(),
            device: req.device.clone(),
        }
    }
}

enum LoadedPayload {
    Embedder(Arc<dyn wilkes_core::embed::Embedder>),
    Generator(Arc<dyn Generator>),
    Recognizer(Arc<dyn OcrEngine>),
    LayoutDetector(Arc<dyn LayoutModel>),
}

struct LoadedModel {
    key: LoadedModelKey,
    payload: LoadedPayload,
    background_task: Option<tokio::task::JoinHandle<()>>,
}

impl LoadedModel {
    fn embedder(&self) -> anyhow::Result<Arc<dyn wilkes_core::embed::Embedder>> {
        match &self.payload {
            LoadedPayload::Embedder(embedder) => Ok(Arc::clone(embedder)),
            other => anyhow::bail!(
                "worker holds {} but received an embedding request",
                other.what()
            ),
        }
    }

    fn generator(&self) -> anyhow::Result<Arc<dyn Generator>> {
        match &self.payload {
            LoadedPayload::Generator(generator) => Ok(Arc::clone(generator)),
            other => anyhow::bail!(
                "worker holds {} but received a generation request",
                other.what()
            ),
        }
    }

    fn recognizer(&self) -> anyhow::Result<Arc<dyn OcrEngine>> {
        match &self.payload {
            LoadedPayload::Recognizer(recognizer) => Ok(Arc::clone(recognizer)),
            other => anyhow::bail!(
                "worker holds {} but received a recognition request",
                other.what()
            ),
        }
    }

    fn layout_detector(&self) -> anyhow::Result<Arc<dyn LayoutModel>> {
        match &self.payload {
            LoadedPayload::LayoutDetector(detector) => Ok(Arc::clone(detector)),
            other => anyhow::bail!(
                "worker holds {} but received a layout request",
                other.what()
            ),
        }
    }
}

impl LoadedPayload {
    fn what(&self) -> &'static str {
        match self {
            LoadedPayload::Embedder(_) => "an embedder",
            LoadedPayload::Generator(_) => "a generator",
            LoadedPayload::Recognizer(_) => "a recognizer",
            LoadedPayload::LayoutDetector(_) => "a layout detector",
        }
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
enum WorkerLoopAction {
    Stop,
    ParseError(String),
    Dispatch(WorkerRequest),
    /// An out-of-band `{"cancel":true}` line, not a request.
    Cancel,
}

#[derive(Debug, PartialEq, Eq)]
enum WorkerRequestKind {
    Embed,
    Recognize,
    Layout,
    Info,
    Generate,
    Unknown(String),
}

trait WorkerEventSink {
    fn emit(&self, event: WorkerEvent);
}

#[derive(Clone, Copy)]
struct StdoutEventSink;

impl WorkerEventSink for StdoutEventSink {
    fn emit(&self, event: WorkerEvent) {
        emit(event);
    }
}

trait ModelLoader: Send + Sync {
    async fn load(
        &self,
        key: &LoadedModelKey,
        event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
    ) -> anyhow::Result<LoadedModel>;
}

struct RealModelLoader;

impl ModelLoader for RealModelLoader {
    async fn load(
        &self,
        key: &LoadedModelKey,
        event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
    ) -> anyhow::Result<LoadedModel> {
        match key.role {
            WorkerRole::Embed(engine) => {
                let model = EmbedderModel(key.model.clone());
                let prepared = dispatch::prepare_embedder(
                    engine,
                    &model,
                    &key.model_dir,
                    &key.device,
                    event_tx,
                )
                .await?;
                Ok(LoadedModel {
                    key: key.clone(),
                    payload: LoadedPayload::Embedder(prepared.embedder),
                    background_task: prepared.background_task,
                })
            }
            WorkerRole::Recognize(engine) => {
                // Loaded here and kept in the slot: a document is dozens of
                // images, and reloading 1.9 GB for each of them would cost
                // more than recognizing them.
                let recognizer: Arc<dyn OcrEngine> = recognize_dispatch::load_recognizer_local(
                    engine,
                    &key.model,
                    &key.model_dir,
                    &key.device,
                )?
                .into();
                Ok(LoadedModel {
                    key: key.clone(),
                    payload: LoadedPayload::Recognizer(recognizer),
                    background_task: None,
                })
            }
            // Loaded here and kept resident beside the recognizers: a
            // document is detected page by page, and reloading the graph per
            // page would cost more than detecting on it.
            WorkerRole::Layout(_) => {
                let detector: Arc<dyn LayoutModel> =
                    recognize_dispatch::load_layout_detector_local(&key.model, &key.model_dir)?
                        .into();
                Ok(LoadedModel {
                    key: key.clone(),
                    payload: LoadedPayload::LayoutDetector(detector),
                    background_task: None,
                })
            }
            WorkerRole::Generate(engine) => {
                let model = GeneratorModel(key.model.clone());
                let (progress, forwarder) = bridge_progress(event_tx);
                let generator = generate_dispatch::prepare_generator(
                    engine,
                    &model,
                    &key.model_dir,
                    &key.device,
                    progress,
                )
                .await;
                if let Some(forwarder) = forwarder {
                    let _ = forwarder.await;
                }
                Ok(LoadedModel {
                    key: key.clone(),
                    payload: LoadedPayload::Generator(generator?),
                    background_task: None,
                })
            }
        }
    }
}

/// Wrap download progress into worker events. The returned sender must be
/// dropped before awaiting the forwarder, or it will never finish.
fn bridge_progress(
    event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
) -> (
    Option<wilkes_core::models::progress::ProgressTx>,
    Option<tokio::task::JoinHandle<()>>,
) {
    let Some(event_tx) = event_tx.cloned() else {
        return (None, None);
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<EmbedProgress>(64);
    let handle = tokio::spawn(async move {
        while let Some(progress) = rx.recv().await {
            if event_tx
                .send(WorkerEvent::Progress(progress))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    (Some(tx), Some(handle))
}

fn classify_input_line(line: &str) -> WorkerLoopAction {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return WorkerLoopAction::Stop;
    }

    // The cancel signal is checked first: it is a distinct, tiny shape, and
    // treating it as a malformed request would emit a spurious error.
    if let Ok(CancelSignal { cancel: true }) = serde_json::from_str::<CancelSignal>(trimmed) {
        return WorkerLoopAction::Cancel;
    }

    match serde_json::from_str::<WorkerRequest>(trimmed) {
        Ok(req) => WorkerLoopAction::Dispatch(req),
        Err(e) => WorkerLoopAction::ParseError(format!("Failed to parse worker config: {e}")),
    }
}

fn classify_worker_request(req: &WorkerRequest) -> WorkerRequestKind {
    match req.mode.as_str() {
        "embed" => WorkerRequestKind::Embed,
        "recognize" => WorkerRequestKind::Recognize,
        "layout" => WorkerRequestKind::Layout,
        "info" => WorkerRequestKind::Info,
        "generate" => WorkerRequestKind::Generate,
        other => WorkerRequestKind::Unknown(other.to_string()),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    wilkes_core::logging::init_logging_stderr();

    tracing::info!("[worker] starting up...");

    let mut active_model = ResidentModels::default();
    let loader = RealModelLoader;
    let sink = StdoutEventSink;

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<WorkerEvent>(128);

    // Background task to print events to stdout
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            sink.emit(event);
        }
    });

    // stdin is read on its own thread so a cancel line can arrive *while* a
    // request is being served. Requests are still handled one at a time, in
    // order; only the cancel flag jumps the queue.
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<WorkerLoopAction>(32);
    let stdin_cancel = Arc::clone(&cancel_flag);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let action = classify_input_line(&line);
            if matches!(action, WorkerLoopAction::Cancel) {
                stdin_cancel.store(true, Ordering::Relaxed);
                continue;
            }
            let stop = matches!(action, WorkerLoopAction::Stop);
            if line_tx.blocking_send(action).is_err() || stop {
                break;
            }
        }
    });

    while let Some(action) = line_rx.recv().await {
        match action {
            WorkerLoopAction::Stop => break,
            WorkerLoopAction::Cancel => {}
            WorkerLoopAction::ParseError(message) => sink.emit(WorkerEvent::Error(message)),
            WorkerLoopAction::Dispatch(req) => {
                let log_req = req.redacted_for_log();
                tracing::info!(
                    "[worker] received request: {}",
                    serde_json::to_string(&log_req).unwrap_or_default()
                );

                // Any cancel that arrived before this request belongs to the
                // previous one; clear it so it cannot abort a fresh generation.
                cancel_flag.store(false, Ordering::Relaxed);

                if let Err(e) = handle_worker_request(
                    req,
                    &mut active_model,
                    event_tx.clone(),
                    &loader,
                    &cancel_flag,
                )
                .await
                {
                    sink.emit(WorkerEvent::Error(e.to_string()));
                }
            }
        }
    }

    Ok(())
}

async fn handle_worker_request(
    req: WorkerRequest,
    active_model: &mut ResidentModels,
    event_tx: tokio::sync::mpsc::Sender<WorkerEvent>,
    loader: &impl ModelLoader,
    cancel_flag: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    match classify_worker_request(&req) {
        WorkerRequestKind::Embed => {
            handle_embed_plan(req, active_model, event_tx, loader).await?;
        }
        WorkerRequestKind::Recognize => {
            handle_recognize_plan(req, active_model, event_tx, loader).await?;
        }
        WorkerRequestKind::Layout => {
            handle_layout_plan(req, active_model, event_tx, loader).await?;
        }
        WorkerRequestKind::Info => {
            handle_info_plan(req, active_model, event_tx, loader).await?;
        }
        WorkerRequestKind::Generate => {
            handle_generate_plan(req, active_model, event_tx, loader, cancel_flag).await?;
        }
        WorkerRequestKind::Unknown(other) => {
            let _ = event_tx
                .send(WorkerEvent::Error(format!("Unknown mode: {other}")))
                .await;
        }
    }
    Ok(())
}

/// Recognize the text drawn in a document's images.
///
/// A whole document per request, so the host waits in one place instead of
/// looping over per-image requests it could not be interrupted between. The
/// images arrive as paths the host staged and owns; this process only reads
/// them.
async fn handle_recognize_plan(
    req: WorkerRequest,
    active_model: &mut ResidentModels,
    event_tx: tokio::sync::mpsc::Sender<WorkerEvent>,
    loader: &impl ModelLoader,
) -> anyhow::Result<()> {
    let Some(payload) = req.recognize.clone() else {
        let _ = event_tx
            .send(WorkerEvent::Error(
                "recognize request carries no images".to_string(),
            ))
            .await;
        return Ok(());
    };

    let images = payload
        .image_paths
        .iter()
        .map(|path| worker_ocr::read_staged_image(path))
        .collect::<anyhow::Result<Vec<_>>>();
    let images = match images {
        Ok(images) => images,
        Err(error) => {
            let _ = event_tx
                .send(WorkerEvent::Error(format!("{error:#}")))
                .await;
            return Ok(());
        }
    };

    let recognizer = get_or_load(active_model, &req, loader, Some(&event_tx))
        .await?
        .recognizer()?;

    // Off the async executor: this is minutes of inference per image, and
    // leaving it on the runtime would stall this process's stdin with it.
    let spotted = tokio::task::spawn_blocking(move || recognizer.spot_batch(&images)).await?;

    match spotted {
        Ok(regions) => {
            let _ = event_tx.send(WorkerEvent::Regions(regions)).await;
            let _ = event_tx.send(WorkerEvent::Done).await;
        }
        Err(error) => {
            let _ = event_tx
                .send(WorkerEvent::Error(format!("recognition failed: {error:#}")))
                .await;
        }
    }
    Ok(())
}

/// Mark out what one rendered page holds.
///
/// One page per request, unlike recognition's whole document. Detection is a
/// quarter of a second where recognition is minutes, and the host cannot
/// decide what to render next until it knows what this page held — so the unit
/// that crosses is the unit the host actually has.
async fn handle_layout_plan(
    req: WorkerRequest,
    active_model: &mut ResidentModels,
    event_tx: tokio::sync::mpsc::Sender<WorkerEvent>,
    loader: &impl ModelLoader,
) -> anyhow::Result<()> {
    let Some(payload) = req.layout.clone() else {
        let _ = event_tx
            .send(WorkerEvent::Error(
                "layout request carries no page".to_string(),
            ))
            .await;
        return Ok(());
    };

    let page = match worker_ocr::read_staged_image(&payload.image_path) {
        Ok(page) => page,
        Err(error) => {
            let _ = event_tx
                .send(WorkerEvent::Error(format!("{error:#}")))
                .await;
            return Ok(());
        }
    };

    let detector = get_or_load(active_model, &req, loader, Some(&event_tx))
        .await?
        .layout_detector()?;

    // Off the async executor for the same reason recognition is: this is a
    // graph execution that holds a core for as long as it takes, and leaving
    // it on the runtime would stall this process's stdin with it — which is
    // the pipe a cancel would arrive on.
    let detected = tokio::task::spawn_blocking(move || detector.detect(&page)).await?;

    match detected {
        Ok(regions) => {
            let wire = regions.iter().map(WireLayoutRegion::from_region).collect();
            let _ = event_tx.send(WorkerEvent::LayoutRegions(wire)).await;
            let _ = event_tx.send(WorkerEvent::Done).await;
        }
        Err(error) => {
            let _ = event_tx
                .send(WorkerEvent::Error(format!("detection failed: {error:#}")))
                .await;
        }
    }
    Ok(())
}

async fn handle_embed_plan(
    req: WorkerRequest,
    active_model: &mut ResidentModels,
    event_tx: tokio::sync::mpsc::Sender<WorkerEvent>,
    loader: &impl ModelLoader,
) -> anyhow::Result<()> {
    let embedder = get_or_load(active_model, &req, loader, None)
        .await?
        .embedder()?;
    let texts = req.texts.unwrap_or_default();
    let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    match embedder.embed(&text_refs) {
        Ok(embeddings) => {
            let _ = event_tx.send(WorkerEvent::Embeddings(embeddings)).await;
            let _ = event_tx.send(WorkerEvent::Done).await;
        }
        Err(e) => {
            let _ = event_tx
                .send(WorkerEvent::Error(format!("Embed error: {e}")))
                .await;
        }
    }
    Ok(())
}

async fn handle_info_plan(
    req: WorkerRequest,
    active_model: &mut ResidentModels,
    event_tx: tokio::sync::mpsc::Sender<WorkerEvent>,
    loader: &impl ModelLoader,
) -> anyhow::Result<()> {
    let embedder = get_or_load(active_model, &req, loader, None)
        .await?
        .embedder()?;
    let _ = event_tx
        .send(WorkerEvent::Info {
            dimension: embedder.dimension(),
            max_seq_length: 512,
        })
        .await;
    let _ = event_tx.send(WorkerEvent::Done).await;
    Ok(())
}

async fn handle_generate_plan(
    req: WorkerRequest,
    active_model: &mut ResidentModels,
    event_tx: tokio::sync::mpsc::Sender<WorkerEvent>,
    loader: &impl ModelLoader,
    cancel_flag: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let generator = get_or_load(active_model, &req, loader, Some(&event_tx))
        .await?
        .generator()?;
    if let Some(runtime) = generator.runtime() {
        let _ = event_tx.send(WorkerEvent::GenerationRuntime(runtime)).await;
    }
    let Some(generation) = req.generate.clone() else {
        let _ = event_tx
            .send(WorkerEvent::Error(
                "generate request missing the generation payload".to_string(),
            ))
            .await;
        return Ok(());
    };

    let cancel_flag = Arc::clone(cancel_flag);
    let sink_tx = event_tx.clone();
    let decode_generator = Arc::clone(&generator);
    // The decode is CPU/GPU-bound and its sink is synchronous, so it runs on a
    // blocking thread and emits through `blocking_send`.
    let result: anyhow::Result<Generated> = tokio::task::spawn_blocking(move || {
        decode_generator.generate_stream(generation, &mut |token| {
            if cancel_flag.load(Ordering::Relaxed) {
                return ControlFlow::Break(());
            }
            if sink_tx
                .blocking_send(WorkerEvent::Token {
                    text: token.to_string(),
                })
                .is_err()
            {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        })
    })
    .await?;

    match result {
        Ok(generated) => {
            if let Some(timings) = generator.last_timings() {
                let _ = event_tx.send(WorkerEvent::GenerationMetrics(timings)).await;
            }
            // Always terminal, including on cancellation: a well-formed
            // terminal event is what keeps the pipe at a request boundary.
            let _ = event_tx
                .send(WorkerEvent::Completion {
                    tokens: generated.tokens,
                    stop: generated.stop,
                })
                .await;
        }
        Err(e) => {
            let _ = event_tx
                .send(WorkerEvent::Error(format!("Generate error: {e:#}")))
                .await;
        }
    }
    Ok(())
}

/// The models this process holds, most recently used last.
///
/// A `Vec` and not a map: it never exceeds [`MAX_RESIDENT_MODELS`], so a linear
/// scan is cheaper than hashing a key that carries two paths, and the order
/// *is* the eviction policy rather than something kept alongside it.
#[derive(Default)]
struct ResidentModels {
    loaded: Vec<LoadedModel>,
}

impl ResidentModels {
    fn position_of(&self, key: &LoadedModelKey) -> Option<usize> {
        self.loaded.iter().position(|model| &model.key == key)
    }

    /// Make room for one more, evicting least-recently-used.
    fn evict_to_fit(&mut self) {
        while self.loaded.len() >= MAX_RESIDENT_MODELS {
            let evicted = self.loaded.remove(0);
            tracing::info!(
                "[worker] evicting {} (model: {}, device: {}) to stay within {} resident",
                evicted.payload.what(),
                evicted.key.model,
                evicted.key.device,
                MAX_RESIDENT_MODELS
            );
            if let Some(task) = evicted.background_task {
                task.abort();
            }
        }
    }
}

async fn get_or_load<'a>(
    active: &'a mut ResidentModels,
    req: &WorkerRequest,
    loader: &impl ModelLoader,
    event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
) -> anyhow::Result<&'a LoadedModel> {
    let key = LoadedModelKey::from_request(req);

    if let Some(index) = active.position_of(&key) {
        tracing::info!("[worker] reusing cached model");
        // Moved to the back, so "least recently used" means what it says. The
        // reading pass keeps all three in play and evicts nothing; this only
        // decides who goes when somebody switches models.
        let model = active.loaded.remove(index);
        active.loaded.push(model);
        return Ok(active.loaded.last().expect("just pushed"));
    }

    tracing::info!(
        "[worker] loading model from scratch (role: {:?}, model: {}, device: {}, model_dir: {})",
        key.role,
        key.model,
        key.device,
        key.model_dir.display()
    );
    active.evict_to_fit();
    let loaded = loader.load(&key, event_tx).await?;
    tracing::info!("[worker] model load succeeded");
    active.loaded.push(loaded);
    Ok(active.loaded.last().expect("just pushed"))
}

fn emit(event: WorkerEvent) {
    let line = serde_json::to_string(&event).expect("WorkerEvent serialization failed");
    println!("{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;
    use wilkes_core::embed::MockEmbedder;
    use wilkes_core::generate::mock::MockGenerator;
    use wilkes_core::generate::{Constraint, GenerationEngine, GenerationRequest, Sampling};
    use wilkes_core::types::EmbeddingEngine;
    use wilkes_core::extract::image::dispatch::RecognitionEngine;
    use wilkes_core::worker::ipc::WorkerRequest;

    struct SuccessLoader;

    impl ModelLoader for SuccessLoader {
        async fn load(
            &self,
            key: &LoadedModelKey,
            _event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
        ) -> anyhow::Result<LoadedModel> {
            let payload = match key.role {
                WorkerRole::Embed(_) => LoadedPayload::Embedder(Arc::new(MockEmbedder::default())),
                WorkerRole::Generate(_) => {
                    LoadedPayload::Generator(Arc::new(MockGenerator::scripted([
                        "Cache invalidation strategy",
                    ])))
                }
                WorkerRole::Recognize(_) => LoadedPayload::Recognizer(Arc::new(MockRecognizer)),
                WorkerRole::Layout(_) => {
                    LoadedPayload::LayoutDetector(Arc::new(MockLayoutDetector))
                }
            };
            Ok(LoadedModel {
                key: key.clone(),
                payload,
                background_task: None,
            })
        }
    }

    /// Returns one region covering the page, so a test can tell a real round
    /// trip from a canned reply.
    struct MockLayoutDetector;

    impl LayoutModel for MockLayoutDetector {
        fn identity(&self) -> String {
            "mock-layout".to_string()
        }
        fn input_side(&self) -> u32 {
            64
        }
        fn detect(
            &self,
            page: &image::RgbImage,
        ) -> anyhow::Result<Vec<wilkes_core::extract::image::LayoutRegion>> {
            Ok(vec![wilkes_core::extract::image::LayoutRegion {
                label: "table",
                kind: Some(wilkes_core::extract::image::ocr::RegionKind::Table),
                score: 0.9,
                bbox: wilkes_core::types::BoundingBox {
                    x: 0.0,
                    y: 0.0,
                    width: page.width() as f32 / page.width() as f32,
                    height: 1.0,
                },
            }])
        }
        fn release(&self) {}
    }

    /// Returns, per image, one region whose text names that image's size — so
    /// a test can tell a real round trip from a canned reply, and can tell the
    /// images apart within a batch.
    struct MockRecognizer;

    impl OcrEngine for MockRecognizer {
        fn identity(&self) -> String {
            "mock-recognizer".to_string()
        }
        fn admission_threshold(&self) -> f32 {
            0.5
        }
        fn spot_batch(
            &self,
            images: &[image::RgbImage],
        ) -> anyhow::Result<Vec<wilkes_core::extract::image::ocr::ImageRecognition>> {
            let corner = wilkes_core::types::Point { x: 0.0, y: 0.0 };
            Ok(images
                .iter()
                .map(|image| {
                    wilkes_core::extract::image::ocr::ImageRecognition::from_regions(vec![
                        wilkes_core::extract::image::ocr::SpottedRegion {
                            kind: wilkes_core::extract::image::ocr::RegionKind::Text,
                            text: format!("{}x{}", image.width(), image.height()),
                            confidence: 0.9,
                            quad: [corner; 4],
                        },
                    ])
                })
                .collect())
        }
    }

    struct FailLoader;

    impl ModelLoader for FailLoader {
        async fn load(
            &self,
            _key: &LoadedModelKey,
            _event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
        ) -> anyhow::Result<LoadedModel> {
            Err(anyhow::anyhow!("load failed"))
        }
    }

    fn request(mode: &str, role: WorkerRole, data_dir: PathBuf) -> WorkerRequest {
        WorkerRequest {
            mode: mode.to_string(),
            role,
            model: "model-a".to_string(),
            model_dir: data_dir.clone(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
            layout: None,
        }
    }

    fn embed_request(mode: &str) -> WorkerRequest {
        let dir = tempdir().unwrap();
        request(
            mode,
            WorkerRole::Embed(EmbeddingEngine::Candle),
            dir.path().to_path_buf(),
        )
    }

    fn generate_request(dir: &std::path::Path) -> WorkerRequest {
        let mut req = request(
            "generate",
            WorkerRole::Generate(GenerationEngine::Candle),
            dir.to_path_buf(),
        );
        req.generate = Some(GenerationRequest {
            system: None,
            prompt: "label these".to_string(),
            max_tokens: Some(16),
            constraint: Constraint::Text { stop: Vec::new() },
            sampling: Sampling::default(),
        });
        req
    }

    fn no_cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn test_classify_input_line_variants() {
        match classify_input_line("") {
            WorkerLoopAction::Stop => {}
            other => panic!("expected Stop, got {other:?}"),
        }

        match classify_input_line("not-json") {
            WorkerLoopAction::ParseError(message) => {
                assert!(message.contains("Failed to parse worker config"));
            }
            other => panic!("expected ParseError, got {other:?}"),
        }

        match classify_input_line(&serde_json::to_string(&embed_request("embed")).unwrap()) {
            WorkerLoopAction::Dispatch(req) => {
                assert_eq!(req.mode, "embed");
                assert_eq!(req.model, "model-a");
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn cancel_lines_are_recognised_and_never_reported_as_parse_errors() {
        match classify_input_line(r#"{"cancel":true}"#) {
            WorkerLoopAction::Cancel => {}
            other => panic!("expected Cancel, got {other:?}"),
        }
        // A false flag is not a cancellation, and is not a request either.
        assert!(matches!(
            classify_input_line(r#"{"cancel":false}"#),
            WorkerLoopAction::ParseError(_)
        ));
    }

    #[test]
    fn test_classify_worker_request_variants() {
        assert_eq!(
            classify_worker_request(&embed_request("embed")),
            WorkerRequestKind::Embed
        );
        assert_eq!(
            classify_worker_request(&embed_request("info")),
            WorkerRequestKind::Info
        );
        assert_eq!(
            classify_worker_request(&embed_request("generate")),
            WorkerRequestKind::Generate
        );

        match classify_worker_request(&embed_request("unknown")) {
            WorkerRequestKind::Unknown(value) => assert_eq!(value, "unknown"),
            other => panic!("expected Unknown, got {other:?}"),
        }

        // "build" is not a mode a worker has: the host builds, and a request
        // that says otherwise is an unknown one.
        match classify_worker_request(&embed_request("build")) {
            WorkerRequestKind::Unknown(value) => assert_eq!(value, "build"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_get_or_load_caching() {
        let dir = tempdir().unwrap();
        let req = request(
            "embed",
            WorkerRole::Embed(EmbeddingEngine::Candle),
            dir.path().to_path_buf(),
        );
        let mut active = ResidentModels {
            loaded: vec![LoadedModel {
                key: LoadedModelKey::from_request(&req),
                payload: LoadedPayload::Embedder(Arc::new(MockEmbedder::default())),
                background_task: None,
            }],
        };

        let loaded = get_or_load(&mut active, &req, &SuccessLoader, None)
            .await
            .unwrap();
        assert_eq!(loaded.embedder().unwrap().model_id(), "mock-model");
    }

    /// A model the request does not name is not served from the one that is
    /// resident. It stays resident — the map has room — but the load for the
    /// new key is what answers, and a load that fails is an error rather than
    /// a fallback onto the wrong model.
    #[tokio::test]
    async fn test_get_or_load_does_not_serve_a_different_model() {
        let dir = tempdir().unwrap();
        let mut active = ResidentModels {
            loaded: vec![LoadedModel {
                key: LoadedModelKey {
                    role: WorkerRole::Embed(EmbeddingEngine::Candle),
                    model: "old-model".to_string(),
                    model_dir: dir.path().to_path_buf(),
                    device: "cpu".to_string(),
                },
                payload: LoadedPayload::Embedder(Arc::new(MockEmbedder::default())),
                background_task: None,
            }],
        };
        let req = request(
            "embed",
            WorkerRole::Embed(EmbeddingEngine::Candle),
            dir.path().to_path_buf(),
        );

        assert!(get_or_load(&mut active, &req, &FailLoader, None)
            .await
            .is_err());
    }

    /// Reading a document needs the detector, the page reader and the formula
    /// reader at once, and alternates between them page by page. All three
    /// stay resident, and each request is answered by its own model rather
    /// than by whichever was used last.
    #[tokio::test]
    async fn the_three_reading_models_stay_resident_together() {
        let dir = tempdir().unwrap();
        let page = request(
            "recognize",
            WorkerRole::Recognize(RecognitionEngine::Onnx),
            dir.path().to_path_buf(),
        );
        let mut formula = page.clone();
        formula.model = "texify".to_string();
        let mut layout = request(
            "layout",
            WorkerRole::Layout(RecognitionEngine::Onnx),
            dir.path().to_path_buf(),
        );
        layout.model = "PP-DocLayoutV2".to_string();

        let mut active = ResidentModels::default();
        for _ in 0..3 {
            for req in [&layout, &page, &formula] {
                get_or_load(&mut active, req, &SuccessLoader, None)
                    .await
                    .unwrap();
            }
        }

        assert_eq!(active.loaded.len(), 3, "a reading pass must not evict");
        assert!(
            get_or_load(&mut active, &layout, &FailLoader, None)
                .await
                .is_ok(),
            "the detector was evicted by the readers it alternates with"
        );
    }

    /// The map is bounded, so a fourth model displaces the one used longest
    /// ago rather than growing the process.
    #[tokio::test]
    async fn a_fourth_model_evicts_the_least_recently_used() {
        let dir = tempdir().unwrap();
        let mut active = ResidentModels::default();
        let mut requests = Vec::new();
        for index in 0..MAX_RESIDENT_MODELS + 1 {
            let mut req = request(
                "embed",
                WorkerRole::Embed(EmbeddingEngine::Candle),
                dir.path().to_path_buf(),
            );
            req.model = format!("model-{index}");
            get_or_load(&mut active, &req, &SuccessLoader, None)
                .await
                .unwrap();
            requests.push(req);
        }

        assert_eq!(active.loaded.len(), MAX_RESIDENT_MODELS);
        assert!(active.position_of(&LoadedModelKey::from_request(&requests[0])).is_none());
        assert!(active
            .position_of(&LoadedModelKey::from_request(requests.last().unwrap()))
            .is_some());
    }

    /// A role change within a kind is still a different model: the accessor
    /// for the wrong payload names the mismatch rather than panicking.
    #[tokio::test]
    async fn the_wrong_accessor_names_the_mismatch() {
        let dir = tempdir().unwrap();
        let embed = request(
            "embed",
            WorkerRole::Embed(EmbeddingEngine::Candle),
            dir.path().to_path_buf(),
        );
        let mut active = ResidentModels::default();
        get_or_load(&mut active, &embed, &SuccessLoader, None)
            .await
            .unwrap();

        let generate = generate_request(dir.path());
        let loaded = get_or_load(&mut active, &generate, &SuccessLoader, None)
            .await
            .unwrap();
        assert!(loaded.generator().is_ok());
        assert!(loaded.embedder().is_err());
    }

    #[tokio::test]
    async fn test_get_or_load_failure() {
        let dir = tempdir().unwrap();
        let mut active = ResidentModels::default();
        let req = request(
            "embed",
            WorkerRole::Embed(EmbeddingEngine::Candle),
            dir.path().to_path_buf(),
        );
        assert!(get_or_load(&mut active, &req, &FailLoader, None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_handle_worker_request_info() {
        let dir = tempdir().unwrap();
        let mut active = ResidentModels::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let req = request(
            "info",
            WorkerRole::Embed(EmbeddingEngine::Candle),
            dir.path().to_path_buf(),
        );

        handle_worker_request(req, &mut active, tx, &SuccessLoader, &no_cancel())
            .await
            .unwrap();

        match rx.recv().await.unwrap() {
            WorkerEvent::Info { dimension, .. } => assert_eq!(dimension, 384),
            other => panic!("Expected Info event, got {other:?}"),
        }
        assert!(matches!(rx.recv().await.unwrap(), WorkerEvent::Done));
    }

    #[tokio::test]
    async fn test_handle_worker_request_embed() {
        let dir = tempdir().unwrap();
        let mut active = ResidentModels::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let req = request(
            "embed",
            WorkerRole::Embed(EmbeddingEngine::Candle),
            dir.path().to_path_buf(),
        );

        handle_worker_request(req, &mut active, tx, &SuccessLoader, &no_cancel())
            .await
            .unwrap();

        assert!(matches!(
            rx.recv().await.unwrap(),
            WorkerEvent::Embeddings(_)
        ));
        assert!(matches!(rx.recv().await.unwrap(), WorkerEvent::Done));
    }

    #[tokio::test]
    async fn generate_streams_tokens_then_a_completion() {
        let dir = tempdir().unwrap();
        let mut active = ResidentModels::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);

        handle_worker_request(
            generate_request(dir.path()),
            &mut active,
            tx,
            &SuccessLoader,
            &no_cancel(),
        )
        .await
        .unwrap();

        let mut streamed = String::new();
        let mut completion = None;
        while let Some(event) = rx.recv().await {
            match event {
                WorkerEvent::Token { text } => streamed.push_str(&text),
                WorkerEvent::Completion { tokens, stop } => {
                    completion = Some((tokens, stop));
                    break;
                }
                other => panic!("unexpected event {other:?}"),
            }
        }

        assert_eq!(streamed, "Cache invalidation strategy");
        let (_, stop) = completion.expect("a generation must end with Completion");
        assert_eq!(stop, wilkes_core::generate::StopReason::Eos);
    }

    #[tokio::test]
    async fn a_raised_cancel_flag_still_ends_with_a_terminal_event() {
        let dir = tempdir().unwrap();
        let mut active = ResidentModels::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let cancel = Arc::new(AtomicBool::new(true));

        handle_worker_request(
            generate_request(dir.path()),
            &mut active,
            tx,
            &SuccessLoader,
            &cancel,
        )
        .await
        .unwrap();

        let mut last = None;
        while let Some(event) = rx.recv().await {
            let terminal = event.is_terminal();
            last = Some(event);
            if terminal {
                break;
            }
        }
        match last {
            Some(WorkerEvent::Completion { stop, .. }) => {
                assert_eq!(stop, wilkes_core::generate::StopReason::Cancelled);
            }
            other => panic!("expected a Completion, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn generate_without_a_payload_reports_an_error() {
        let dir = tempdir().unwrap();
        let mut active = ResidentModels::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let mut req = generate_request(dir.path());
        req.generate = None;

        handle_worker_request(req, &mut active, tx, &SuccessLoader, &no_cancel())
            .await
            .unwrap();

        match rx.recv().await.unwrap() {
            WorkerEvent::Error(message) => assert!(message.contains("missing the generation")),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_handle_worker_request_unknown() {
        let dir = tempdir().unwrap();
        let mut active = ResidentModels::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let req = request(
            "unknown",
            WorkerRole::Embed(EmbeddingEngine::Candle),
            dir.path().to_path_buf(),
        );

        handle_worker_request(req, &mut active, tx, &FailLoader, &no_cancel())
            .await
            .unwrap();

        assert!(matches!(rx.recv().await.unwrap(), WorkerEvent::Error(_)));
    }

    /// The whole hop, in one test: the host's encoding, the worker's decode,
    /// the recognizer, and the regions coming back over the event channel.
    #[tokio::test]
    async fn a_recognize_request_returns_the_regions_of_each_image_it_carried() {
        let dir = tempdir().unwrap();
        let mut active = ResidentModels::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        // Two images of different sizes, staged the way the host stages them.
        let mut image_paths = Vec::new();
        for (index, (width, height)) in [(11u32, 5u32), (6, 9)].into_iter().enumerate() {
            let path = dir.path().join(format!("{index}.png"));
            image::RgbImage::new(width, height)
                .save_with_format(&path, image::ImageFormat::Png)
                .unwrap();
            image_paths.push(path);
        }
        let mut req = request(
            "recognize",
            WorkerRole::Recognize(wilkes_core::extract::image::dispatch::RecognitionEngine::Candle),
            dir.path().to_path_buf(),
        );
        req.recognize = Some(wilkes_core::extract::image::RecognitionRequest { image_paths });

        handle_worker_request(req, &mut active, tx, &SuccessLoader, &no_cancel())
            .await
            .unwrap();

        match rx.recv().await.unwrap() {
            WorkerEvent::Regions(batch) => {
                // One answer per image, in order. The recognizer reports the
                // dimensions it was handed, so these are the host's images
                // and not a canned reply, and they are not transposed.
                assert_eq!(batch.len(), 2);
                assert_eq!(batch[0].regions[0].text, "11x5");
                assert_eq!(batch[1].regions[0].text, "6x9");
            }
            other => panic!("expected Regions, got {other:?}"),
        }
        assert!(matches!(rx.recv().await.unwrap(), WorkerEvent::Done));
    }

    #[tokio::test]
    async fn a_recognize_request_without_images_reports_an_error() {
        let dir = tempdir().unwrap();
        let mut active = ResidentModels::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let req = request(
            "recognize",
            WorkerRole::Recognize(wilkes_core::extract::image::dispatch::RecognitionEngine::Candle),
            dir.path().to_path_buf(),
        );

        handle_worker_request(req, &mut active, tx, &SuccessLoader, &no_cancel())
            .await
            .unwrap();

        assert!(matches!(rx.recv().await.unwrap(), WorkerEvent::Error(_)));
    }

    /// A worker does not build. Extraction decides the recipe a document is
    /// read under and belongs to the process holding the settings, so a build
    /// request arriving here is a mistake to report, not a mode to serve.
    #[tokio::test]
    async fn a_build_request_is_refused_rather_than_served() {
        let dir = tempdir().unwrap();
        let mut active = ResidentModels::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let req = request(
            "build",
            WorkerRole::Embed(EmbeddingEngine::Candle),
            dir.path().to_path_buf(),
        );

        handle_worker_request(req, &mut active, tx, &SuccessLoader, &no_cancel())
            .await
            .unwrap();

        match rx.recv().await.unwrap() {
            WorkerEvent::Error(message) => assert!(
                message.contains("build"),
                "expected the mode to be named, got {message}"
            ),
            other => panic!("expected an error event, got {other:?}"),
        }
    }

    #[test]
    fn test_loaded_model_key_equality() {
        let k1 = LoadedModelKey {
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "m".to_string(),
            model_dir: PathBuf::from("d"),
            device: "cpu".to_string(),
        };
        let k2 = k1.clone();
        let k3 = LoadedModelKey {
            role: WorkerRole::Generate(GenerationEngine::Candle),
            ..k1.clone()
        };
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }

    #[tokio::test]
    async fn test_real_loader_fails_on_missing_model() {
        let loader = RealModelLoader;
        let key = LoadedModelKey {
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "non-existent".to_string(),
            model_dir: PathBuf::from("/tmp/non-existent-model-dir"),
            device: "cpu".to_string(),
        };
        assert!(loader.load(&key, None).await.is_err());
    }

    #[tokio::test]
    async fn test_handle_embed_plan_failure() {
        struct FailEmbedder;
        impl wilkes_core::embed::Embedder for FailEmbedder {
            fn embedding_space_identity(&self) -> wilkes_core::embed::EmbeddingSpaceIdentity {
                wilkes_core::embed::EmbeddingSpaceIdentity::for_test(
                    self.engine(),
                    self.model_id(),
                    self.dimension(),
                )
            }

            fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
                Err(anyhow::anyhow!("embed failed"))
            }
            fn model_id(&self) -> &str {
                "fail"
            }
            fn dimension(&self) -> usize {
                384
            }
            fn engine(&self) -> EmbeddingEngine {
                EmbeddingEngine::Candle
            }
        }

        struct FailEmbedderLoader;
        impl ModelLoader for FailEmbedderLoader {
            async fn load(
                &self,
                key: &LoadedModelKey,
                _event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
            ) -> anyhow::Result<LoadedModel> {
                Ok(LoadedModel {
                    key: key.clone(),
                    payload: LoadedPayload::Embedder(Arc::new(FailEmbedder)),
                    background_task: None,
                })
            }
        }

        let mut active = ResidentModels::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);

        handle_worker_request(
            embed_request("embed"),
            &mut active,
            tx,
            &FailEmbedderLoader,
            &no_cancel(),
        )
        .await
        .unwrap();

        match rx.recv().await.unwrap() {
            WorkerEvent::Error(e) => assert!(e.contains("Embed error")),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn test_stdout_event_sink() {
        StdoutEventSink.emit(WorkerEvent::Done);
    }
}
