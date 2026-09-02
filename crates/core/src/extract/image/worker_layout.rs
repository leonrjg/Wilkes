//! The layout detector as the host addresses it: a proxy over the worker
//! protocol.
//!
//! The detector used to run on the extraction thread, which was the last model
//! in this path that did. It was easy to justify one page at a time — a
//! quarter of a second, a graph of a few megabytes — and the justification was
//! wrong at document scale: a four-hundred-page book is a hundred seconds of
//! uninterruptible inference in the process that owns the user interface, and
//! nothing but quitting the application could stop it.
//!
//! So it goes where the recognizers already went. The rule this holds up is
//! the one [`super::dispatch`] states: **no model in the image-analysis path
//! executes in the host.** What the host keeps is everything that is not
//! inference — which pages are rendered, at what scale, what a region means,
//! and the recipe the reading is recorded under.
//!
//! `identity` and `input_side` answer from constants without asking the
//! worker. They are needed before a single page is rendered — `input_side`
//! decides the render itself — and a detector that had to round-trip to a
//! subprocess to say how big a page should be would be one that could not
//! answer while the subprocess was down.

use std::path::PathBuf;

use super::{LayoutModel, LayoutRegion, LayoutRequest};
use crate::worker::ipc::{WorkerEvent, WorkerRequest, WorkerRole};
use crate::worker::manager::{ManagerCommand, WorkerManager};

use super::dispatch::RecognitionEngine;

pub struct WorkerLayout {
    manager: WorkerManager,
    /// Captured at construction, always in an async context, so `detect` can
    /// be called from the blocking extraction thread it actually runs on.
    tokio_handle: tokio::runtime::Handle,
    engine: RecognitionEngine,
    model_id: String,
    model_dir: PathBuf,
    /// Where a page's PNG is staged. Under the application's own data
    /// directory rather than the system temp: these are pages of the user's
    /// documents, and they should not be written somewhere world-readable.
    scratch_root: PathBuf,
    /// Resolved once at construction. Both are pure functions of the model,
    /// and recomputing them per page would be work to answer a question that
    /// cannot change.
    identity: String,
    input_side: u32,
}

impl WorkerLayout {
    /// Stage one page as a PNG and return where it went.
    ///
    /// The directory is returned alongside the path so the caller holds it for
    /// exactly as long as the request: dropping it removes the file, whether
    /// the request succeeded, failed or was killed underneath.
    fn stage(&self, page: &image::RgbImage) -> anyhow::Result<(tempfile::TempDir, PathBuf)> {
        std::fs::create_dir_all(&self.scratch_root).map_err(|error| {
            anyhow::anyhow!(
                "could not create the layout scratch directory {}: {error}",
                self.scratch_root.display()
            )
        })?;
        let staged = tempfile::Builder::new()
            .prefix("layout-")
            .tempdir_in(&self.scratch_root)?;
        let path = staged.path().join("page.png");
        page.save_with_format(&path, image::ImageFormat::Png)
            .map_err(|error| anyhow::anyhow!("could not stage a page for detection: {error}"))?;
        Ok((staged, path))
    }
}

impl LayoutModel for WorkerLayout {
    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn input_side(&self) -> u32 {
        self.input_side
    }

    fn detect(&self, page: &image::RgbImage) -> anyhow::Result<Vec<LayoutRegion>> {
        // Held until the request is over. Named, because dropping it is what
        // removes the file and an unnamed temporary would drop it here.
        let (_staged, image_path) = self.stage(page)?;

        let request = WorkerRequest {
            mode: "layout".to_string(),
            role: WorkerRole::Layout(self.engine),
            model: self.model_id.clone(),
            model_dir: self.model_dir.clone(),
            // The detector runs on ONNX Runtime's CPU provider, which is the
            // only one it was measured on. Naming a device it will not honour
            // would be a setting that reads as a promise.
            device: "cpu".to_string(),
            texts: None,
            generate: None,
            recognize: None,
            layout: Some(LayoutRequest { image_path }),
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let cmd = ManagerCommand::Submit {
            req: Box::new(request),
            reply: tx,
        };

        let wire = self.tokio_handle.block_on(async move {
            self.manager
                .send(cmd)
                .await
                .map_err(|error| anyhow::anyhow!("could not reach the layout detector: {error}"))?;

            while let Some(event) = rx.recv().await {
                match event {
                    WorkerEvent::LayoutRegions(regions) => return Ok(regions),
                    WorkerEvent::Error(error) => {
                        return Err(anyhow::anyhow!("layout detector error: {error}"))
                    }
                    WorkerEvent::Done => break,
                    _ => {}
                }
            }
            Err(anyhow::anyhow!(
                "the layout detector finished without returning regions"
            ))
        })?;

        // A page with nothing on it is a real answer, so an empty list is not
        // checked for. What is checked is the vocabulary: a class this build
        // does not know is a host and worker that disagree about what the
        // detector is, and reporting it as an unread area would hide that.
        wire.into_iter()
            .map(super::WireLayoutRegion::into_region)
            .collect()
    }

    /// Knock the detector down, freeing the graph it holds.
    ///
    /// The manager keeps supervising: the next page spawns a replacement and
    /// loads again. Entered on the captured handle because this is called from
    /// the blocking extraction thread, and the shutdown it starts is a task.
    fn release(&self) {
        let _guard = self.tokio_handle.enter();
        self.manager.request_shutdown();
    }
}

/// The layout detector addressed through `manager`, as a [`LayoutModel`].
///
/// Cheap: it resolves constants and keeps a handle, and loads nothing. The
/// graph is loaded by the worker, on its first page.
pub fn attach(
    manager: WorkerManager,
    model_dir: PathBuf,
    scratch_root: PathBuf,
) -> anyhow::Result<Box<dyn LayoutModel>> {
    #[cfg(feature = "recognize-onnx")]
    {
        Ok(Box::new(WorkerLayout {
            manager,
            tokio_handle: tokio::runtime::Handle::current(),
            // The runtime the detector runs under, which is the same one the
            // ONNX recognizers run under. It travels on the request so the
            // worker knows what it is being asked to load.
            engine: RecognitionEngine::Onnx,
            model_id: super::doclayout::MODEL_ID.to_string(),
            model_dir,
            scratch_root,
            identity: super::doclayout::identity(),
            input_side: super::doclayout::DocLayout::input_side(),
        }))
    }
    #[cfg(not(feature = "recognize-onnx"))]
    {
        let (_, _, _) = (manager, model_dir, scratch_root);
        anyhow::bail!("this build has no layout detector compiled in")
    }
}
