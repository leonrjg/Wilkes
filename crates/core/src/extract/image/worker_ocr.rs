//! The recognizer as the host addresses it: a proxy over the worker protocol.
//!
//! Recognition is candle inference against 1.9 GB of weights on whatever
//! accelerator the device resolves to. Running it in the host means a fault in
//! it takes the application down, and it is the same class of runtime the
//! embedder was moved out of process for. So the pixels go out and the regions
//! come back, and the host keeps everything that is not inference: which
//! images exist, where each region lands on the page, whether it clears the
//! admission threshold, whether the document already draws it as glyphs, and
//! the recipe the whole reading is recorded under.
//!
//! `identity` and `admission_threshold` answer from constants without asking
//! the worker. They enter the extraction recipe and are needed before a single
//! image is sent — and a recipe that had to round-trip to a subprocess to be
//! known is one that could differ depending on whether the subprocess was up.

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;

use super::dispatch::{self, RecognitionEngine};
use super::ocr::{OcrEngine, SpottedRegion};
use super::RecognitionRequest;
use crate::worker::ipc::{WorkerEvent, WorkerRequest, WorkerRole};
use crate::worker::manager::{ManagerCommand, WorkerManager};

pub struct WorkerOcr {
    manager: WorkerManager,
    /// Captured at construction, always in an async context, so `spot` can be
    /// called from the blocking extraction thread it actually runs on.
    tokio_handle: tokio::runtime::Handle,
    engine: RecognitionEngine,
    model_id: String,
    model_dir: PathBuf,
    device: String,
    /// Resolved once at construction. Both are pure functions of the engine
    /// and model, and recomputing them per image would be per-image work to
    /// answer a question that cannot change.
    identity: String,
    admission_threshold: f32,
}

impl WorkerOcr {
    /// Address the recognizer `engine`/`model_id` names through `manager`.
    ///
    /// Fails when the pair names no recognizer this build ships. That is
    /// checked here, before any document is read, because the alternative is
    /// discovering it once per image with a library half-extracted.
    pub fn new(
        manager: WorkerManager,
        engine: RecognitionEngine,
        model_id: &str,
        model_dir: PathBuf,
        device: &str,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            manager,
            tokio_handle: tokio::runtime::Handle::current(),
            engine,
            model_id: model_id.to_string(),
            model_dir,
            device: device.to_string(),
            identity: dispatch::identity(engine, model_id)?,
            admission_threshold: dispatch::admission_threshold(engine, model_id)?,
        })
    }
}

impl OcrEngine for WorkerOcr {
    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn admission_threshold(&self) -> f32 {
        self.admission_threshold
    }

    fn spot(&self, image: &image::RgbImage) -> anyhow::Result<Vec<SpottedRegion>> {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image.clone())
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(|error| anyhow::anyhow!("could not encode the image for recognition: {error}"))?;

        let request = WorkerRequest {
            mode: "recognize".to_string(),
            role: WorkerRole::Recognize(self.engine),
            model: self.model_id.clone(),
            model_dir: self.model_dir.clone(),
            device: self.device.clone(),
            texts: None,
            generate: None,
            recognize: Some(RecognitionRequest {
                image_png_base64: base64::engine::general_purpose::STANDARD
                    .encode(png.into_inner()),
            }),
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let cmd = ManagerCommand::Submit {
            req: Box::new(request),
            reply: tx,
        };

        self.tokio_handle.block_on(async move {
            self.manager
                .send(cmd)
                .await
                .map_err(|error| anyhow::anyhow!("could not reach the recognizer: {error}"))?;

            while let Some(event) = rx.recv().await {
                match event {
                    WorkerEvent::Regions(regions) => return Ok(regions),
                    WorkerEvent::Error(error) => {
                        return Err(anyhow::anyhow!("recognizer error: {error}"))
                    }
                    WorkerEvent::Done => break,
                    _ => {}
                }
            }
            Err(anyhow::anyhow!(
                "the recognizer finished without returning regions"
            ))
        })
    }
}

/// A recognizer addressed through `manager`, as an [`OcrEngine`].
pub fn attach(
    manager: WorkerManager,
    engine: RecognitionEngine,
    model_id: &str,
    model_dir: PathBuf,
    device: &str,
) -> anyhow::Result<Box<dyn OcrEngine>> {
    Ok(Box::new(WorkerOcr::new(
        manager, engine, model_id, model_dir, device,
    )?))
}

/// Decode what [`WorkerOcr::spot`] encoded. The worker's half of the hop.
pub fn decode_request_image(request: &RecognitionRequest) -> anyhow::Result<image::RgbImage> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&request.image_png_base64)
        .map_err(|error| anyhow::anyhow!("the recognition payload is not base64: {error}"))?;
    Ok(image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|error| anyhow::anyhow!("the recognition payload is not a PNG: {error}"))?
        .to_rgb8())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pixels the worker recognizes must be the pixels the host saw. A
    /// lossy hop would move the transcription of small type without moving the
    /// recipe that claims to describe it.
    #[test]
    fn the_image_survives_the_hop_unchanged() {
        let mut original = image::RgbImage::new(7, 3);
        for (x, y, pixel) in original.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x * 30) as u8, (y * 70) as u8, ((x + y) * 20) as u8]);
        }

        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(original.clone())
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let request = RecognitionRequest {
            image_png_base64: base64::engine::general_purpose::STANDARD
                .encode(png.into_inner()),
        };

        // Through JSON, because that is what the hop actually is.
        let wire = serde_json::to_string(&request).unwrap();
        let received: RecognitionRequest = serde_json::from_str(&wire).unwrap();

        assert_eq!(decode_request_image(&received).unwrap(), original);
    }

    #[test]
    fn a_payload_that_is_not_a_png_is_an_error() {
        let request = RecognitionRequest {
            image_png_base64: base64::engine::general_purpose::STANDARD.encode("not a png"),
        };
        assert!(decode_request_image(&request).is_err());

        let request = RecognitionRequest {
            image_png_base64: "!!! not base64 !!!".to_string(),
        };
        assert!(decode_request_image(&request).is_err());
    }

    /// The whole chain against the real recognizer: spawn the worker binary,
    /// load 1.9 GB of weights in it, recognize an image, read the regions
    /// back. Everything else in this file is mocked on one side or the other,
    /// so this is the only test that would catch a role that routes to the
    /// wrong process, a mode the worker does not serve, or a payload it
    /// cannot decode.
    ///
    /// Ignored by default: it needs the weights on disk and takes minutes.
    ///
    /// ```text
    /// cargo build -p wilkes-rust-worker
    /// WILKES_MODEL_DIR="$HOME/Library/Application Support/app.wilkes/models"     ///   cargo test -p wilkes-core --lib worker_ocr -- --ignored --nocapture
    /// ```
    ///
    /// A blank image is deliberate. This asserts the hop, not the reading:
    /// "no text in it" is a correct answer and an empty region list is a
    /// success, while an error means the chain is broken.
    #[tokio::test]
    #[ignore = "needs the installed recognizer; minutes"]
    #[cfg(feature = "candle")]
    async fn the_real_recognizer_answers_over_the_worker_protocol() {
        let model_dir = std::path::PathBuf::from(
            std::env::var("WILKES_MODEL_DIR").expect("WILKES_MODEL_DIR must name the model cache"),
        );
        let worker_bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug/wilkes-rust-worker");
        assert!(
            worker_bin.exists(),
            "build it first: cargo build -p wilkes-rust-worker ({})",
            worker_bin.display()
        );

        let (manager, _events, loop_fut) =
            crate::worker::manager::WorkerManager::new(crate::worker::manager::WorkerPaths {
                python_path: std::path::PathBuf::new(),
                python_package_dir: std::path::PathBuf::new(),
                requirements_path: std::path::PathBuf::new(),
                venv_dir: std::path::PathBuf::new(),
                worker_bin,
                data_dir: model_dir.clone(),
            });
        tokio::spawn(loop_fut);

        let engine = RecognitionEngine::default();
        let model_id = dispatch::shipped_model_id(engine);
        assert!(
            dispatch::installed(engine, model_id, &model_dir).unwrap(),
            "the recognizer is not installed under {}",
            model_dir.display()
        );

        let recognizer = attach(manager, engine, model_id, model_dir, "auto").unwrap();
        let identity = recognizer.identity();
        assert!(identity.contains(model_id), "{identity}");

        // A page with a known label on it, rasterized — so an empty answer is
        // a failure of the chain rather than an honest reading of a blank
        // image. The corpus builder is reused rather than a fixture invented
        // here: it is what the accuracy harness draws with.
        let page = crate::extract::image::corpus::PageSpec::default()
            .with_text(20.0, 150.0, "Knowledge base");
        let pdf = crate::extract::image::corpus::build_pdf(vec![page]);
        let image = crate::extract::image::corpus::render_page(&pdf, 0, 4.0);

        let started = std::time::Instant::now();
        let regions = tokio::task::spawn_blocking(move || recognizer.spot(&image))
            .await
            .unwrap()
            .expect("the recognizer should answer");
        let read = regions
            .iter()
            .map(|region| region.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!(
            "recognized in {:.1}s: {} region(s): {read}",
            started.elapsed().as_secs_f32(),
            regions.len()
        );
        assert!(
            read.to_lowercase().contains("knowledge"),
            "the label did not survive the hop; read {read:?}"
        );
    }

    /// Regions are the whole reply, and they cross as ordinary JSON so a
    /// recognizer written in another language can produce them.
    #[test]
    fn regions_round_trip_as_plain_json() {
        let regions = vec![SpottedRegion {
            text: "Figure 1".to_string(),
            confidence: 0.87,
            quad: [
                crate::types::Point { x: 0.1, y: 0.2 },
                crate::types::Point { x: 0.9, y: 0.2 },
                crate::types::Point { x: 0.9, y: 0.4 },
                crate::types::Point { x: 0.1, y: 0.4 },
            ],
        }];
        let wire = serde_json::to_string(&WorkerEvent::Regions(regions.clone())).unwrap();
        assert!(wire.contains("Figure 1"), "{wire}");
        match serde_json::from_str::<WorkerEvent>(&wire).unwrap() {
            WorkerEvent::Regions(back) => assert_eq!(back, regions),
            other => panic!("expected Regions, got {other:?}"),
        }
    }
}
