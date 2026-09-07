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
//!
//! `input_side` is *not* the size of the square the graph is fed, and the host
//! must not assume it is. The detector asks for more page than its graph takes
//! and cuts that into passes itself, inside the worker: the tiling, the map
//! from a tile's pixels back to the page and the reconciling of what two tiles
//! both saw are all decisions about a model's input, and every one of them is
//! on the other side of this pipe. What the host does is render each page as
//! one square of the size it was told and stage it.
//!
//! A document's pages go out in one request, and the loop over them runs in
//! the worker. That is the killability rule, not a throughput choice: a
//! per-page request is a worker started inside the host's page loop, so a kill
//! ended one page and the next iteration spawned a replacement that reloaded
//! the graph. See [`super::LayoutRequest`].

use std::path::PathBuf;

use super::{LayoutModel, LayoutRegion, LayoutRequest};
use crate::worker::fault::WorkerFault;
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
    /// Render a document's pages one at a time and stage each as a PNG.
    ///
    /// Pulled and written page by page rather than taken as a slice of
    /// images: at 1600 square a page is nearly eight megabytes, so a book held
    /// in memory to be staged all at once would be gigabytes, and the staging
    /// exists precisely so they do not have to be.
    ///
    /// Returns the directory alongside the staged paths and the page each one
    /// came from. The directory is the caller's to hold for exactly as long as
    /// the request: dropping it removes the files, whether the request
    /// succeeded, failed or was killed underneath. A page that would not
    /// rasterize is absent from both vectors — its index is not staged, and
    /// the caller answers it with no regions.
    fn stage(
        &self,
        page_count: usize,
        render: &mut dyn FnMut(usize) -> anyhow::Result<image::RgbImage>,
    ) -> anyhow::Result<(tempfile::TempDir, Vec<PathBuf>, Vec<usize>)> {
        std::fs::create_dir_all(&self.scratch_root).map_err(|error| {
            anyhow::anyhow!(
                "could not create the layout scratch directory {}: {error}",
                self.scratch_root.display()
            )
        })?;
        let staged = tempfile::Builder::new()
            .prefix("layout-")
            .tempdir_in(&self.scratch_root)?;
        let mut paths = Vec::with_capacity(page_count);
        let mut of_page = Vec::with_capacity(page_count);
        for index in 0..page_count {
            let Ok(page) = render(index) else {
                continue;
            };
            let path = staged.path().join(format!("{index}.png"));
            page.save_with_format(&path, image::ImageFormat::Png)
                .map_err(|error| {
                    anyhow::anyhow!("could not stage page {index} for detection: {error}")
                })?;
            paths.push(path);
            of_page.push(index);
        }
        Ok((staged, paths, of_page))
    }
}

impl LayoutModel for WorkerLayout {
    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn input_side(&self) -> u32 {
        self.input_side
    }

    fn detect_document(
        &self,
        page_count: usize,
        render: &mut dyn FnMut(usize) -> anyhow::Result<image::RgbImage>,
    ) -> anyhow::Result<Vec<Vec<LayoutRegion>>> {
        // Held until the request is over. Named, because dropping it is what
        // removes the files and an unnamed temporary would drop it here.
        let (_staged, image_paths, of_page) = self.stage(page_count, render)?;
        let expected = image_paths.len();
        // Nothing rasterized, so there is nothing to ask. Submitting would
        // start a worker and load a graph to detect on no pages at all.
        if image_paths.is_empty() {
            return Ok(vec![Vec::new(); page_count]);
        }

        let request = WorkerRequest {
            batch_size: crate::worker::ipc::default_embed_batch_size(),
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
            layout: Some(LayoutRequest { image_paths }),
            table: None,
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let cmd = ManagerCommand::Submit {
            req: Box::new(request),
            reply: tx,
        };

        let wire = self.tokio_handle.block_on(async move {
            self.manager.send(cmd).await.map_err(|error| {
                WorkerFault::gone(format!("could not reach the layout detector: {error}"))
            })?;

            while let Some(event) = rx.recv().await {
                match event {
                    WorkerEvent::LayoutRegions(regions) => return Ok(regions),
                    WorkerEvent::Error(error) => {
                        return Err(WorkerFault::reported(format!(
                            "layout detector error: {error}"
                        )))
                    }
                    WorkerEvent::Gone(detail) => return Err(WorkerFault::gone(detail)),
                    WorkerEvent::Done => break,
                    _ => {}
                }
            }
            Err(anyhow::anyhow!(
                "the layout detector finished without returning regions"
            ))
        })?;

        // Results are positional — the caller pairs them back with its own
        // pages by index — so a short reply is a wrong answer, not a partial
        // one, and must not be silently zipped against the wrong pages.
        anyhow::ensure!(
            wire.len() == expected,
            "the layout detector answered for {} of {expected} page(s)",
            wire.len()
        );

        // A page with nothing on it is a real answer, so an empty list is not
        // checked for. What is checked is the vocabulary: a class this build
        // does not know is a host and worker that disagree about what the
        // detector is, and reporting it as an unread area would hide that.
        //
        // Placed back against the document's own page numbering, so a page
        // that never rasterized reads as a page with no regions rather than
        // shifting every later page's regions up by one.
        let mut found = vec![Vec::new(); page_count];
        for (index, page) in of_page.into_iter().zip(wire) {
            found[index] = page
                .into_iter()
                .map(super::WireLayoutRegion::into_region)
                .collect::<anyhow::Result<Vec<_>>>()?;
        }
        Ok(found)
    }

    /// Knock the detector down, freeing the graph it holds.
    ///
    /// The manager keeps supervising: the next document spawns a replacement
    /// and loads again. Entered on the captured handle because this is called from
    /// the blocking extraction thread, and the shutdown it starts is a task.
    fn release(&self) {
        let _guard = self.tokio_handle.enter();
        self.manager.request_shutdown();
    }
}

/// The layout detector addressed through `manager`, as a [`LayoutModel`].
///
/// Cheap: it resolves constants and keeps a handle, and loads nothing. The
/// graph is loaded by the worker, on its first document.
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

#[cfg(test)]
mod tests {
    use super::*;

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

    fn test_layout(scratch_root: PathBuf) -> WorkerLayout {
        WorkerLayout {
            manager: test_manager(),
            tokio_handle: tokio::runtime::Handle::current(),
            engine: RecognitionEngine::default(),
            model_id: "m".to_string(),
            model_dir: PathBuf::new(),
            scratch_root,
            identity: "id".to_string(),
            input_side: 8,
        }
    }

    /// A document's pages are staged together, in page order, under one
    /// directory. One directory because one request: the whole point of the
    /// batch is that the host waits in a single place, and a per-page staging
    /// directory would be the shape of a per-page request.
    #[tokio::test]
    async fn a_document_stages_every_page_in_order() {
        let root = tempfile::tempdir().unwrap();
        let detector = test_layout(root.path().join("scratch"));

        let pages: Vec<image::RgbImage> = (0..4)
            .map(|n| image::RgbImage::from_pixel(3, 2, image::Rgb([n * 10, n, 0])))
            .collect();
        // Page 2 will not rasterize. It is skipped rather than staged, and
        // `of_page` is what lets the reply be placed back against the
        // document's own numbering instead of shifting every later page up.
        let (staged, paths, of_page) = detector
            .stage(4, &mut |index| {
                if index == 2 {
                    anyhow::bail!("this page will not rasterize");
                }
                Ok(pages[index].clone())
            })
            .unwrap();

        assert_eq!(paths.len(), 3);
        assert_eq!(of_page, vec![0, 1, 3]);
        for (path, index) in paths.iter().zip(&of_page) {
            assert_eq!(
                super::super::worker_ocr::read_staged_image(path).unwrap(),
                pages[*index]
            );
        }
        // Dropping the directory takes the pages with it, whether the request
        // succeeded, failed or was killed underneath.
        let first = paths[0].clone();
        drop(staged);
        assert!(!first.exists(), "staged pages outlived the request");
    }

    /// A document where nothing rasterized asks the worker nothing at all.
    /// Submitting would start a worker and load a graph to detect on no pages
    /// — and give a cancel one more process to have to kill.
    ///
    /// It still answers for every page, because the caller places the reply
    /// against its own page numbering and a short answer would silently
    /// shift it.
    #[tokio::test]
    async fn a_document_with_no_rendered_pages_asks_nothing() {
        let root = tempfile::tempdir().unwrap();
        let detector = test_layout(root.path().join("scratch"));

        let found = detector
            .detect_document(3, &mut |_| anyhow::bail!("no page rasterized"))
            .unwrap();

        assert_eq!(found.len(), 3);
        assert!(found.iter().all(Vec::is_empty));
    }
}
