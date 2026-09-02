//! The layout detector: what a page holds, and where.
//!
//! PP-DocLayoutV2 as an ONNX graph, run on the `ort` already in the tree.
//! It answers one question — for each area of a rendered page, which of
//! twenty-five document classes it is — and it answers it from the picture,
//! which is what makes it the right instrument for a question the file itself
//! does not declare.
//!
//! ## What it replaced
//!
//! Until now [`crate::extract::pdf`] marked out formulas by reading the font
//! each glyph was drawn in and tables by finding the rules the page stroked.
//! Both worked on the documents they were written against and neither
//! generalized: the font rule needed a list of face names and could not see
//! inline mathematics at all, because its unit was the line and an inline
//! formula shares its line with prose; the rule stack found booktabs tables
//! and missed every unruled one. Those were the heuristics. They are gone —
//! this is the one mechanism, and when it is not installed nothing is marked
//! out rather than something being guessed.
//!
//! ## What it costs
//!
//! One page render and one 800x800 forward pass per page. Measured at ~250 ms
//! a page on this project's reference machine while it was busy, so roughly
//! ten minutes for a 2,300-page library — against the hours the recognizer
//! itself takes on what the detector marks out. The graph is 204 MB of fp32.
//!
//! ## What it does not decide
//!
//! Whether a marked-out area's reading reaches the document. That is settled
//! afterwards by the same admission rules as before: a formula on whether its
//! LaTeX parses, a table on whether it is rectangular, and — for every typeset
//! region — on whether the kind is one worth displacing the page's own glyphs
//! for. A false positive here still costs time and changes no bytes.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use image::RgbImage;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use tracing::debug;

use super::LayoutRegion;
use crate::types::{BoundingBox, RegionKind};

/// The detector's id, as a reading records it.
pub const MODEL_ID: &str = "PP-DocLayoutV2";

const REPO: &str = "alex-dinh/PP-DocLayoutV2-ONNX";

/// Pinned to a commit rather than to `main`. The graph's class order *is* the
/// contract for [`LABELS`], and a branch that moved under us would silently
/// re-label every region in the library.
const REVISION: &str = "5e30a2650d087e23af3a8084d42bd30d135af771";

const GRAPH: &str = "PP-DocLayoutV2.onnx";
const CONFIG: &str = "config.json";

pub const ARTIFACTS: &[&str] = &[GRAPH, CONFIG];

/// Size and digest of each artifact, in `ARTIFACTS` order. Read from the
/// pinned revision; a file that does not match is deleted rather than used.
const DIGESTS: &[(u64, &str)] = &[
    (
        213_963_712,
        "2009fcb35e64085ab9f6f2b27aca550edc29a040a24f7d6a0f05b74a2f804860",
    ),
    (
        4_535,
        "e1a25a0cbdc59c3668bc96467ecca6b9747eca29ff26d90282f452adc89df21f",
    ),
];

/// The square the graph declares, and the size the page is rasterized to.
///
/// The model's own `Preprocess` block says `Resize` to 800x800 with
/// `keep_ratio: false`, so the page is stretched into the square rather than
/// letterboxed and the map back is two independent scales. Following the
/// model's preprocessing rather than improving on it: a detector fed pixels
/// laid out differently from its training is a detector answering a different
/// question.
const SIDE: u32 = 800;

/// The score a detection must reach to be believed.
///
/// The model's own `draw_threshold`. Not tuned here — a threshold this module
/// picked for itself would be exactly the kind of unexplained constant this
/// detector exists to remove.
pub const SCORE_THRESHOLD: f32 = 0.5;

/// PP-DocLayoutV2's classes, in the order its `config.json` lists them.
///
/// The index *is* the class id the graph returns, so this order is a contract
/// with the pinned revision and not a convenience. Kept in full, including the
/// classes nothing routes, because a detection this build cannot name is a
/// fact about the build and is reported as one.
pub const LABELS: [&str; 25] = [
    "abstract",
    "algorithm",
    "aside_text",
    "chart",
    "content",
    "display_formula",
    "doc_title",
    "figure_title",
    "footer",
    "footer_image",
    "footnote",
    "formula_number",
    "header",
    "header_image",
    "image",
    "inline_formula",
    "number",
    "paragraph_title",
    "reference",
    "reference_content",
    "seal",
    "table",
    "text",
    "vertical_text",
    "vision_footnote",
];

/// What a class means to Wilkes, or nothing.
///
/// Only the kinds that are *worth displacing a page's own glyphs for* map to
/// something — see [`RegionKind::supersedes_native_glyphs`]. A class that maps
/// to `None` is still counted; it is simply an area the page already tells us
/// about better than a recognizer would.
///
/// Both formula classes map to [`RegionKind::Formula`]. `inline_formula` mapped
/// to nothing for as long as supersession replaced whole *lines* — an inline
/// expression shares its line with prose, so reading one meant either losing
/// the sentence or reading it twice. A region owns words now, so an inline
/// formula is spliced into the sentence where the page drew it.
pub fn kind_of(label: &str) -> Option<RegionKind> {
    match label {
        // Both formulas, and both routed. An inline expression was reported
        // and not read for as long as supersession replaced whole lines: it
        // shares its line with prose, so reading it meant either losing the
        // sentence or reading it twice. Regions own words now, so it is
        // spliced into the sentence where the page drew it.
        "display_formula" | "inline_formula" => Some(RegionKind::Formula),
        "table" => Some(RegionKind::Table),
        "chart" => Some(RegionKind::Chart),
        _ => None,
    }
}

/// The static label equal to `label`, or `None` when this detector has no such
/// class.
///
/// The crossing back from a worker's reply into the detector's own vocabulary.
/// A [`LayoutRegion`](super::LayoutRegion)'s label is a `&'static str` out of
/// [`LABELS`] because the diagnostics count classes by identity; a string off a
/// pipe is not one of those until it has been found here.
pub fn label_of(label: &str) -> Option<&'static str> {
    LABELS.iter().copied().find(|known| *known == label)
}

/// The detector's recipe, as it enters the extraction identity.
pub fn identity() -> String {
    format!(
        "{MODEL_ID}+{REPO}@{REVISION}+{SIDE}px+score-{SCORE_THRESHOLD}+labels-{}",
        LABELS.len()
    )
}

pub fn install_dir(model_dir: &Path) -> PathBuf {
    model_dir.join("layout").join(MODEL_ID)
}

pub fn is_installed(model_dir: &Path) -> bool {
    let dir = install_dir(model_dir);
    ARTIFACTS.iter().all(|name| dir.join(name).is_file())
}

pub fn footprint_bytes() -> u64 {
    DIGESTS.iter().map(|(size, _)| size).sum()
}

/// What the detector is, where it came from, and under what terms.
pub fn inventory() -> crate::types::RecognizerInventory {
    crate::types::RecognizerInventory {
        name: MODEL_ID.to_string(),
        repo: REPO.to_string(),
        revision: REVISION.to_string(),
        license: "Apache-2.0".to_string(),
        license_url: "https://huggingface.co/PaddlePaddle/PP-DocLayoutV2".to_string(),
        derived_from: vec![
            "PP-DocLayoutV2 (Apache-2.0, PaddlePaddle)".to_string(),
            "RT-DETR detection head (Apache-2.0, PaddlePaddle)".to_string(),
            format!("ONNX export by {REPO}"),
        ],
        artifacts: ARTIFACTS
            .iter()
            .zip(DIGESTS)
            .map(
                |(filename, (size_bytes, sha256))| crate::types::InventoriedArtifact {
                    filename: (*filename).to_string(),
                    size_bytes: *size_bytes,
                    sha256: (*sha256).to_string(),
                },
            )
            .collect(),
        footprint_bytes: footprint_bytes(),
    }
}

/// Fetch the detector's artifacts into `model_dir`, checking each against the
/// size and digest declared above.
pub fn install(
    model_dir: &Path,
    progress: Option<crate::models::progress::ProgressTx>,
) -> Result<()> {
    use hf_hub::api::sync::ApiBuilder;

    let dir = install_dir(model_dir);
    std::fs::create_dir_all(&dir).context("could not create the detector directory")?;

    let api = ApiBuilder::new()
        .with_progress(false)
        .build()
        .context("could not reach the model hub")?
        .repo(hf_hub::Repo::with_revision(
            REPO.to_string(),
            hf_hub::RepoType::Model,
            REVISION.to_string(),
        ));

    let reporter = progress.map(crate::models::hf_hub::HfProgressReporter::new);
    for (filename, (size_bytes, sha256)) in ARTIFACTS.iter().zip(DIGESTS) {
        let target = dir.join(filename);
        if target.is_file() && super::verify_artifact(&target, *size_bytes, sha256).is_ok() {
            continue;
        }
        let fetched = match reporter.clone() {
            Some(reporter) => api.download_with_progress(filename, reporter),
            None => api.download(filename),
        }
        .with_context(|| format!("could not download {filename} from {REPO}"))?;
        std::fs::copy(&fetched, &target)
            .with_context(|| format!("could not place {filename} under {}", dir.display()))?;
        // A file that does not match is removed rather than left where
        // `is_installed` would count it: a half-installed detector that reads
        // as installed is worse than one that reads as absent.
        if let Err(error) = super::verify_artifact(&target, *size_bytes, sha256) {
            let _ = std::fs::remove_file(&target);
            return Err(error);
        }
    }
    Ok(())
}

/// The graph's declared inputs. `scale_factor` is fed as identity so the boxes
/// come back in the square's own coordinates and the map back to the page
/// stays here, where a test can check it.
const INPUT_IMAGE: &str = "image";
const INPUT_SCALE: &str = "scale_factor";
const INPUT_SHAPE: &str = "im_shape";

/// Columns of the detection tensor. Six are the detection; the last two are
/// the reading-order outputs V2 adds, which nothing here consumes.
const DETECTION_COLUMNS: usize = 8;

/// PP-DocLayoutV2, addressed.
pub struct DocLayout {
    /// Where the graph is and what to load it with, kept so the session can be
    /// dropped and rebuilt: [`super::LayoutModel::release`] is a promise that
    /// a later `detect` still works.
    path: PathBuf,
    threads: usize,
    /// `Session::run` needs `&mut`, and the detector is shared across the
    /// extraction of one document; one page at a time is also what the cost
    /// model assumes. `None` after a release, and until the first page.
    session: Mutex<Option<Session>>,
}

impl DocLayout {
    /// Check the graph is here and note how to load it. Loading is deferred to
    /// the first page — a runtime that attaches an analyzer and then indexes
    /// nothing should not pay 204 MB for it.
    pub fn load(model_dir: &Path, threads: usize) -> Result<Self> {
        let path = install_dir(model_dir).join(GRAPH);
        anyhow::ensure!(
            path.is_file(),
            "the layout detector is not installed: {} is missing",
            path.display()
        );
        Ok(Self {
            path,
            threads,
            session: Mutex::new(None),
        })
    }

    fn open(&self) -> Result<Session> {
        Session::builder()
            .map_err(|e| anyhow::anyhow!("could not start an ONNX session builder: {e}"))?
            .with_intra_threads(self.threads)
            .map_err(|e| anyhow::anyhow!("could not set the session thread count: {e}"))?
            .commit_from_file(&self.path)
            .map_err(|e| anyhow::anyhow!("could not load {}: {e}", self.path.display()))
    }

    /// The size a page must be rendered at to be detected on.
    pub const fn input_side() -> u32 {
        SIDE
    }
}

/// Turn one rendered page into the model's input tensor: RGB, CHW, scaled to
/// 0..1.
///
/// The model's `NormalizeImage` step declares mean 0, std 1 and `norm_type:
/// none`, which in PaddleDetection's preprocessing is the identity after the
/// 1/255 scaling. Written out rather than folded away so the next reader can
/// check it against the config.
fn to_tensor(page: &RgbImage) -> Result<Tensor<f32>> {
    let (w, h) = (page.width() as usize, page.height() as usize);
    anyhow::ensure!(
        w == SIDE as usize && h == SIDE as usize,
        "the detector wants a {SIDE}x{SIDE} render, got {w}x{h}"
    );
    let mut chw = vec![0f32; 3 * w * h];
    for (x, y, pixel) in page.enumerate_pixels() {
        let (x, y) = (x as usize, y as usize);
        for channel in 0..3 {
            chw[channel * w * h + y * w + x] = f32::from(pixel.0[channel]) / 255.0;
        }
    }
    Tensor::from_array((vec![1i64, 3, h as i64, w as i64], chw))
        .context("could not build the detector's input tensor")
}

impl super::LayoutModel for DocLayout {
    fn identity(&self) -> String {
        identity()
    }

    fn input_side(&self) -> u32 {
        SIDE
    }

    fn release(&self) {
        let dropped = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .is_some();
        if dropped {
            debug!("layout detector released; the next page reloads it");
        }
    }

    fn detect(&self, page: &RgbImage) -> Result<Vec<LayoutRegion>> {
        let image = to_tensor(page)?;
        let unit = Tensor::from_array((vec![1i64, 2], vec![1.0f32, 1.0]))
            .context("could not build the scale tensor")?;
        let shape = Tensor::from_array((vec![1i64, 2], vec![SIDE as f32, SIDE as f32]))
            .context("could not build the shape tensor")?;

        let mut held = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.is_none() {
            *held = Some(self.open()?);
        }
        let session = held.as_mut().expect("the session was just opened");
        let feed: Vec<(String, SessionInputValue)> = session
            .inputs()
            .iter()
            .map(|input| input.name().to_string())
            .map(|name| -> Result<(String, SessionInputValue)> {
                let value: SessionInputValue = match name.as_str() {
                    INPUT_IMAGE => image.clone().into(),
                    INPUT_SCALE => unit.clone().into(),
                    INPUT_SHAPE => shape.clone().into(),
                    other => anyhow::bail!(
                        "the detector declares an input this build does not know: {other}"
                    ),
                };
                Ok((name, value))
            })
            .collect::<Result<_>>()?;

        let outputs = session
            .run(feed)
            .map_err(|e| anyhow::anyhow!("the layout detector failed: {e}"))?;
        let (shape, rows) = outputs[0].try_extract_tensor::<f32>().map_err(|e| {
            anyhow::anyhow!("the detector returned a tensor this build cannot read: {e}")
        })?;
        anyhow::ensure!(
            shape.len() == 2 && shape[1] as usize == DETECTION_COLUMNS,
            "the detector returned {shape:?}, {DETECTION_COLUMNS} columns expected"
        );

        Ok(decode_detections(rows))
    }
}

/// Turn the detection tensor into regions, in page fractions.
///
/// Separated from the session so it can be tested against rows written by
/// hand: the geometry and the thresholding are where a mistake would be
/// invisible, and neither needs a 204 MB graph to exercise.
fn decode_detections(rows: &[f32]) -> Vec<LayoutRegion> {
    let side = SIDE as f32;
    let mut found = Vec::new();
    for row in rows.chunks_exact(DETECTION_COLUMNS) {
        let (class, score) = (row[0], row[1]);
        if score < SCORE_THRESHOLD || score.is_nan() {
            continue;
        }
        // The graph returns every query, thresholded here rather than inside;
        // a negative or out-of-range class id is a graph this build does not
        // understand, and is dropped loudly rather than indexed with.
        let Some(label) = usize::try_from(class as i64)
            .ok()
            .and_then(|index| LABELS.get(index))
        else {
            debug!("layout detector returned class {class}, which this build has no name for");
            continue;
        };
        let clamp = |v: f32| (v / side).clamp(0.0, 1.0);
        let (x0, y0) = (clamp(row[2].min(row[4])), clamp(row[3].min(row[5])));
        let (x1, y1) = (clamp(row[2].max(row[4])), clamp(row[3].max(row[5])));
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        found.push(LayoutRegion {
            label,
            kind: kind_of(label),
            score,
            bbox: BoundingBox {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            },
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(class: f32, score: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> [f32; 8] {
        [class, score, x0, y0, x1, y1, 0.0, 0.0]
    }

    #[test]
    fn a_detection_becomes_a_fraction_of_the_page() {
        // 800 is the square, so half of it is half the page.
        let found = decode_detections(&row(5.0, 0.9, 200.0, 400.0, 600.0, 600.0));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "display_formula");
        assert_eq!(found[0].kind, Some(RegionKind::Formula));
        assert_eq!(found[0].bbox.x, 0.25);
        assert_eq!(found[0].bbox.y, 0.5);
        assert_eq!(found[0].bbox.width, 0.5);
        assert_eq!(found[0].bbox.height, 0.25);
    }

    #[test]
    fn a_detection_below_the_threshold_is_not_believed() {
        let mut rows = Vec::new();
        rows.extend(row(5.0, SCORE_THRESHOLD - 0.01, 0.0, 0.0, 100.0, 100.0));
        rows.extend(row(5.0, SCORE_THRESHOLD, 0.0, 0.0, 100.0, 100.0));
        let found = decode_detections(&rows);
        assert_eq!(found.len(), 1, "the threshold is inclusive and only just");
    }

    /// The graph returns every one of its queries, and the ones it does not
    /// believe carry boxes that run off the square or invert.
    #[test]
    fn a_box_running_past_the_page_is_clamped_and_an_empty_one_is_dropped() {
        let mut rows = Vec::new();
        rows.extend(row(21.0, 0.9, -40.0, -10.0, 900.0, 810.0));
        rows.extend(row(21.0, 0.9, 400.0, 400.0, 400.0, 500.0));
        let found = decode_detections(&rows);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bbox.x, 0.0);
        assert_eq!(found[0].bbox.y, 0.0);
        assert_eq!(found[0].bbox.width, 1.0);
        assert_eq!(found[0].bbox.height, 1.0);
    }

    /// Corners in either order describe the same rectangle.
    #[test]
    fn an_inverted_box_is_read_as_the_rectangle_it_describes() {
        let found = decode_detections(&row(21.0, 0.9, 600.0, 600.0, 200.0, 400.0));
        assert_eq!(found[0].bbox.x, 0.25);
        assert_eq!(found[0].bbox.y, 0.5);
    }

    #[test]
    fn a_class_this_build_has_no_name_for_is_dropped_rather_than_indexed_with() {
        assert!(decode_detections(&row(99.0, 0.9, 0.0, 0.0, 100.0, 100.0)).is_empty());
        assert!(decode_detections(&row(-1.0, 0.9, 0.0, 0.0, 100.0, 100.0)).is_empty());
    }

    /// Only the kinds worth displacing a page's own glyphs are routed. Every
    /// other class is named and counted and reaches no recognizer.
    #[test]
    fn only_the_kinds_that_supersede_glyphs_are_routed() {
        for label in LABELS {
            if let Some(kind) = kind_of(label) {
                assert!(
                    kind.supersedes_native_glyphs(),
                    "{label} routes to {kind:?}, which would be refused on arrival"
                );
            }
        }
        assert_eq!(kind_of("display_formula"), Some(RegionKind::Formula));
        assert_eq!(kind_of("table"), Some(RegionKind::Table));
        assert_eq!(kind_of("chart"), Some(RegionKind::Chart));
        // Both formula classes route. An inline expression is a formula that
        // happens to share its line with prose, and a region owns words.
        assert_eq!(kind_of("inline_formula"), Some(RegionKind::Formula));
        // Prose is not routed: the page's own glyphs are the better evidence
        // for it, and admission would refuse the transcription anyway.
        assert_eq!(kind_of("text"), None);
        assert_eq!(kind_of("paragraph_title"), None);
        assert_eq!(kind_of("formula_number"), None);
    }

    #[test]
    fn the_identity_names_the_graph_its_revision_and_its_threshold() {
        let id = identity();
        assert!(id.contains(MODEL_ID), "{id}");
        assert!(id.contains(REVISION), "{id}");
        assert!(id.contains(&SCORE_THRESHOLD.to_string()), "{id}");
        assert!(id.contains(&SIDE.to_string()), "{id}");
    }

    /// The label order is the graph's class order. A test rather than a
    /// comment, because the one thing that would silently re-label a library
    /// is this list drifting from the pinned revision's `config.json`.
    #[test]
    fn the_label_order_is_the_pinned_revisions_own() {
        assert_eq!(LABELS.len(), 25);
        assert_eq!(LABELS[0], "abstract");
        assert_eq!(LABELS[3], "chart");
        assert_eq!(LABELS[5], "display_formula");
        assert_eq!(LABELS[15], "inline_formula");
        assert_eq!(LABELS[21], "table");
        assert_eq!(LABELS[22], "text");
        assert_eq!(LABELS[24], "vision_footnote");
    }
}
