//! The recognizer as the host addresses it: a proxy over the worker protocol.
//!
//! Recognition is candle inference against 1.9 GB of weights on whatever
//! accelerator the device resolves to. Running it in the host means a fault in
//! it takes the application down, and it is the same class of runtime the
//! embedder was moved out of process for. So the images go out and the regions
//! come back, and the host keeps everything that is not inference: which
//! images exist, where each region lands on the page, whether it clears the
//! admission threshold, whether the document already draws it as glyphs, and
//! the recipe the whole reading is recorded under.
//!
//! A document's images go in one request. That is not a throughput
//! optimization — recognition is minutes an image and the hop is milliseconds
//! — it is what makes the work killable. Asking image by image left the host
//! in a loop that outlived the process serving it: kill the recognizer and the
//! loop simply asked again, spawning a replacement. With one request the host
//! waits in exactly one place, and killing the recognizer ends the wait.
//!
//! `identity` and `admission_threshold` answer from constants without asking
//! the worker. They enter the extraction recipe and are needed before a single
//! image is sent — and a recipe that had to round-trip to a subprocess to be
//! known is one that could differ depending on whether the subprocess was up.

use std::path::{Path, PathBuf};

use super::dispatch::{self, RecognitionEngine};
use super::ocr::{ImageRecognition, OcrEngine};
use super::RecognitionRequest;
use crate::worker::ipc::{WorkerEvent, WorkerRequest, WorkerRole};
use crate::worker::manager::{ManagerCommand, WorkerManager};

pub struct WorkerOcr {
    manager: WorkerManager,
    /// Captured at construction, always in an async context, so `spot_batch`
    /// can be called from the blocking extraction thread it actually runs on.
    tokio_handle: tokio::runtime::Handle,
    engine: RecognitionEngine,
    model_id: String,
    model_dir: PathBuf,
    device: String,
    /// Where a batch's PNGs are staged. Under the application's own data
    /// directory rather than the system temp: these are pages of the user's
    /// documents, and they should not be written somewhere world-readable.
    scratch_root: PathBuf,
    /// Resolved once at construction. Both are pure functions of the engine
    /// and model, and recomputing them per batch would be work to answer a
    /// question that cannot change.
    identity: String,
    admission_threshold: f32,
}

impl WorkerOcr {
    /// Address the recognizer `engine`/`model_id` names through `manager`.
    ///
    /// Fails when the pair names no recognizer this build ships. That is
    /// checked here, before any document is read, because the alternative is
    /// discovering it once per batch with a library half-extracted.
    pub fn new(
        manager: WorkerManager,
        engine: RecognitionEngine,
        model_id: &str,
        model_dir: PathBuf,
        scratch_root: PathBuf,
        device: &str,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            manager,
            tokio_handle: tokio::runtime::Handle::current(),
            engine,
            model_id: model_id.to_string(),
            model_dir,
            device: device.to_string(),
            scratch_root,
            identity: dispatch::identity(engine, model_id)?,
            admission_threshold: dispatch::admission_threshold(engine, model_id)?,
        })
    }

    /// Stage one batch as PNG files and return where they went.
    ///
    /// The directory is returned alongside the paths so the caller holds it
    /// for exactly as long as the request: dropping it removes the files,
    /// whether the request succeeded, failed or was killed underneath.
    fn stage(
        &self,
        images: &[image::RgbImage],
    ) -> anyhow::Result<(tempfile::TempDir, Vec<PathBuf>)> {
        std::fs::create_dir_all(&self.scratch_root).map_err(|error| {
            anyhow::anyhow!(
                "could not create the recognition scratch directory {}: {error}",
                self.scratch_root.display()
            )
        })?;
        let staged = tempfile::Builder::new()
            .prefix("recognize-")
            .tempdir_in(&self.scratch_root)?;
        let mut paths = Vec::with_capacity(images.len());
        for (index, image) in images.iter().enumerate() {
            let path = staged.path().join(format!("{index}.png"));
            image
                .save_with_format(&path, image::ImageFormat::Png)
                .map_err(|error| {
                    anyhow::anyhow!("could not stage image {index} for recognition: {error}")
                })?;
            paths.push(path);
        }
        Ok((staged, paths))
    }
}

impl OcrEngine for WorkerOcr {
    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn admission_threshold(&self) -> f32 {
        self.admission_threshold
    }

    fn spot_batch(&self, images: &[image::RgbImage]) -> anyhow::Result<Vec<ImageRecognition>> {
        if images.is_empty() {
            return Ok(Vec::new());
        }
        // Held until the request is over. Named, because dropping it is what
        // removes the files and an unnamed temporary would drop it here.
        let (_staged, image_paths) = self.stage(images)?;
        let expected = image_paths.len();

        let request = WorkerRequest {
            mode: "recognize".to_string(),
            role: WorkerRole::Recognize(self.engine),
            model: self.model_id.clone(),
            model_dir: self.model_dir.clone(),
            device: self.device.clone(),
            texts: None,
            generate: None,
            recognize: Some(RecognitionRequest { image_paths }),
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let cmd = ManagerCommand::Submit {
            req: Box::new(request),
            reply: tx,
        };

        let regions: Vec<ImageRecognition> = self.tokio_handle.block_on(async move {
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
        })?;

        // Results are positional — the caller pairs them back with its own
        // images by index — so a short reply is a wrong answer, not a partial
        // one, and must not be silently zipped against the wrong images.
        anyhow::ensure!(
            regions.len() == expected,
            "the recognizer answered for {} of {expected} image(s)",
            regions.len()
        );
        Ok(regions)
    }
}

/// A recognizer addressed through `manager`, as an [`OcrEngine`].
pub fn attach(
    manager: WorkerManager,
    engine: RecognitionEngine,
    model_id: &str,
    model_dir: PathBuf,
    scratch_root: PathBuf,
    device: &str,
) -> anyhow::Result<Box<dyn OcrEngine>> {
    Ok(Box::new(WorkerOcr::new(
        manager,
        engine,
        model_id,
        model_dir,
        scratch_root,
        device,
    )?))
}

/// Read back what the host staged. The worker's half of the hop.
pub fn read_staged_image(path: &Path) -> anyhow::Result<image::RgbImage> {
    Ok(image::ImageReader::open(path)
        .map_err(|error| anyhow::anyhow!("could not open {}: {error}", path.display()))?
        .decode()
        .map_err(|error| anyhow::anyhow!("{} is not a readable image: {error}", path.display()))?
        .to_rgb8())
}

#[cfg(test)]
mod tests {
    use super::super::ocr::{RegionKind, SpottedRegion};
    use super::*;

    /// The pixels the worker reads must be the pixels the host saw. A lossy
    /// staging would move the transcription of small type without moving the
    /// recipe that claims to describe it.
    #[tokio::test]
    async fn images_survive_staging_unchanged() {
        let mut original = image::RgbImage::new(7, 3);
        for (x, y, pixel) in original.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x * 30) as u8, (y * 70) as u8, ((x + y) * 20) as u8]);
        }
        let second = image::RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3]));

        let root = tempfile::tempdir().unwrap();
        let ocr = WorkerOcr {
            manager: test_manager(),
            tokio_handle: tokio::runtime::Handle::current(),
            engine: RecognitionEngine::default(),
            model_id: "m".to_string(),
            model_dir: PathBuf::new(),
            device: "cpu".to_string(),
            scratch_root: root.path().join("scratch"),
            identity: "id".to_string(),
            admission_threshold: 0.5,
        };

        let (staged, paths) = ocr.stage(&[original.clone(), second.clone()]).unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(read_staged_image(&paths[0]).unwrap(), original);
        assert_eq!(read_staged_image(&paths[1]).unwrap(), second);

        // The staging directory owns the files: dropping it takes them with
        // it, whether the request succeeded, failed or was killed.
        let path = paths[0].clone();
        drop(staged);
        assert!(!path.exists(), "staged files outlived the batch");
    }

    /// The images are the user's documents. They are staged under the
    /// application's own directory, not somewhere world-readable.
    #[tokio::test]
    async fn staging_happens_under_the_root_it_was_given() {
        let root = tempfile::tempdir().unwrap();
        let scratch = root.path().join("scratch");
        let ocr = WorkerOcr {
            manager: test_manager(),
            tokio_handle: tokio::runtime::Handle::current(),
            engine: RecognitionEngine::default(),
            model_id: "m".to_string(),
            model_dir: PathBuf::new(),
            device: "cpu".to_string(),
            scratch_root: scratch.clone(),
            identity: "id".to_string(),
            admission_threshold: 0.5,
        };
        let (_staged, paths) = ocr.stage(&[image::RgbImage::new(2, 2)]).unwrap();
        assert!(paths[0].starts_with(&scratch), "{}", paths[0].display());
    }

    #[test]
    fn a_path_that_is_not_an_image_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-png");
        std::fs::write(&path, b"not a png").unwrap();
        assert!(read_staged_image(&path).is_err());
        assert!(read_staged_image(&dir.path().join("absent.png")).is_err());
    }

    fn test_manager() -> WorkerManager {
        crate::worker::manager::WorkerManager::new(
            crate::worker::manager::WorkerPaths {
                python_path: PathBuf::new(),
                python_package_dir: PathBuf::new(),
                requirements_path: PathBuf::new(),
                venv_dir: PathBuf::new(),
                worker_bin: PathBuf::new(),
                data_dir: PathBuf::new(),
            },
            crate::worker::ipc::WorkerKind::Recognize,
        )
        .0
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

        let (manager, _events, loop_fut) = crate::worker::manager::WorkerManager::new(
            crate::worker::manager::WorkerPaths {
                python_path: std::path::PathBuf::new(),
                python_package_dir: std::path::PathBuf::new(),
                requirements_path: std::path::PathBuf::new(),
                venv_dir: std::path::PathBuf::new(),
                worker_bin,
                data_dir: model_dir.clone(),
            },
            crate::worker::ipc::WorkerKind::Recognize,
        );
        tokio::spawn(loop_fut);

        let engine = RecognitionEngine::default();
        let model_id = dispatch::shipped_model_id(engine);
        assert!(
            dispatch::installed(engine, model_id, &model_dir).unwrap(),
            "the recognizer is not installed under {}",
            model_dir.display()
        );

        let scratch = tempfile::tempdir().unwrap();
        let recognizer = attach(
            manager,
            engine,
            model_id,
            model_dir,
            scratch.path().to_path_buf(),
            "auto",
        )
        .unwrap();
        let identity = recognizer.identity();
        assert!(identity.contains(model_id), "{identity}");

        // A page with a known label on it, rasterized — so an empty answer is
        // a failure of the chain rather than an honest reading of a blank
        // image. The corpus builder is reused rather than a fixture invented
        // here: it is what the accuracy harness draws with.
        let page = crate::extract::image::corpus::PageSpec::default().with_text(
            20.0,
            150.0,
            "Knowledge base",
        );
        let pdf = crate::extract::image::corpus::build_pdf(vec![page]);
        let image = crate::extract::image::corpus::render_page(&pdf, 0, 4.0);

        let started = std::time::Instant::now();
        let mut batch = tokio::task::spawn_blocking(move || recognizer.spot_batch(&[image]))
            .await
            .unwrap()
            .expect("the recognizer should answer");
        assert_eq!(batch.len(), 1, "one answer per image");
        let regions = batch.remove(0).regions;
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
            kind: RegionKind::Text,
            text: "Figure 1".to_string(),
            confidence: 0.87,
            quad: [
                crate::types::Point { x: 0.1, y: 0.2 },
                crate::types::Point { x: 0.9, y: 0.2 },
                crate::types::Point { x: 0.9, y: 0.4 },
                crate::types::Point { x: 0.1, y: 0.4 },
            ],
        }];
        let batch = vec![
            ImageRecognition {
                regions: regions.clone(),
                unroutable: 2,
                not_text: 1,
            },
            ImageRecognition::default(),
        ];
        let wire = serde_json::to_string(&WorkerEvent::Regions(batch.clone())).unwrap();
        assert!(wire.contains("Figure 1"), "{wire}");
        match serde_json::from_str::<WorkerEvent>(&wire).unwrap() {
            // One entry per image, including the image that had no text in
            // it: results are positional and a dropped empty would shift
            // every region after it onto the wrong figure.
            WorkerEvent::Regions(back) => assert_eq!(back, batch),
            other => panic!("expected Regions, got {other:?}"),
        }
    }
}
