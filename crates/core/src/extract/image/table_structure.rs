//! Reading a table the page typesets: the grid from a structure model, the
//! text from the page.
//!
//! **The invariant this module exists to hold: a typeset table's structure
//! comes from the structure model and its text from the page. No model
//! transcribes glyphs the page already holds.**
//!
//! Every `table` region PP-DocLayoutV2 marks out used to go to
//! granite-docling-258M, which reads the crop as if it were a page and answers
//! in DocTags. On one 168-page textbook that was 56 crops, 411.7 s, and 21
//! admitted tables: the other 35 calls came back as prose that admission threw
//! away against the page's own glyphs. 7.4 s a crop to learn, most of the time,
//! that the crop was not a table after all.
//!
//! SLANet-plus answers a narrower question — the *grid*: `<tr>`, `<td`,
//! `colspan`, `rowspan`, and a box per cell — and the cells' text is then taken
//! from the page's own text layer by geometry rather than transcribed. For a
//! typeset PDF that is the better source anyway: the glyphs are already there
//! and correct, and what was missing was only which cell they sit in. On the
//! same 56 crops: 23 ms a crop against 7.4 s, 51 rectangular grids against 21
//! admitted, and better column structure in 10 cases against 1.
//!
//! # Where each half runs
//!
//! The model runs in the recognition worker, like every other model in this
//! path — see [`super::dispatch`]'s invariant. The fill runs in the host,
//! because the host is what holds the page's words and the rectangle the crop
//! covers, and neither is inference. So **what crosses the pipe is geometry,
//! not text**: a [`TableGrid`] of cells with `row`/`col`/`rowspan`/`colspan`
//! and a box in fractions of the crop. Nothing on the worker side of that pipe
//! has ever seen a character of the document.
//!
//! One module rather than three files because this is one rule with two halves,
//! and a grid whose fill lived elsewhere would be half a rule in each place.
//! What is *not* here is the loader: `load_table_structure_local` sits in
//! [`super::dispatch`] beside the other `_local` loaders, so the boundary
//! between "loads weights" and "does not" stays where a reader already looks
//! for it.

use std::path::{Path, PathBuf};

use crate::types::BoundingBox;

use super::NativeTextOnPage;

// ── The pinned recipe ────────────────────────────────────────────────────────

pub const MODEL_ID: &str = "slanet-plus";

/// The graph, as installed.
pub const GRAPH: &str = "slanet-plus.onnx";

pub const ARTIFACTS: &[&str] = &[GRAPH];

/// Where the file comes from.
///
/// ModelScope and not the HuggingFace hub, because that is where RapidAI
/// publishes it — there is no `hf-hub` repo and revision to pin, so the pin is
/// the URL and the digest below. Both are checked: the digest is identical to
/// the one RapidTable's own `default_models.yaml` declares for this file, which
/// is what makes this a pin of *their* artifact rather than of whatever that
/// path serves today.
pub const SOURCE_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidTable/resolve/master/slanet-plus.onnx";

/// Size and SHA-256 of each artifact, in [`ARTIFACTS`] order.
const DIGESTS: &[(u64, &str)] = &[(
    7_758_305,
    "d57a942af6a2f57d6a4a0372573c696a2379bf5857c45e2ac69993f3b334514b",
)];

/// The square the graph is fed. `TablePreprocess.max_len`.
pub const SIDE: u32 = 488;

/// ImageNet normalization, in the channel order the graph was trained on.
///
/// That order is **BGR**, not RGB: PaddleOCR's table configs decode with
/// `img_mode: BGR` and RapidTable's `LoadImage` hands OpenCV-ordered channels
/// straight to the preprocessor, so the 0.485 mean lands on blue. Written the
/// wrong way round on purpose — matching the training is the point, and
/// "fixing" it here would be feeding the graph a different picture.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// The tokens `TableLabelDecode` attaches a cell box to. `<td>` is not in this
/// model's dictionary at all — the export is the `merge_no_span_structure` one,
/// which trades `<td>` for `<td></td>` — but it is listed because RapidTable
/// lists it, and a dictionary that grew it back would still decode.
const TD_TOKENS: [&str; 3] = ["<td>", "<td", "<td></td>"];

/// The metadata key the structure-token dictionary is stored under, inside the
/// ONNX file itself. There is no separate dictionary file to install.
const DICT_KEY: &str = "character";

/// Declared because the catalogue asks every recognizer for one, and it is
/// zero for the same reason the formula reader's is: a table is admitted on
/// whether its grid holds the page's glyphs, never on a score. See
/// [`super::ocr::admit`], which never consults it for a `Table`.
pub const ADMISSION_THRESHOLD: f32 = 0.0;

/// What the recipe records about a table this model's grid produced.
///
/// Every knob that changes the bytes: the graph and where it came from, the
/// square and the channel order it is fed in, and the fact that spans are
/// expanded across the grid positions they cover rather than written once. The
/// last is not the model's — it is [`to_markdown`]'s, and it decides what a
/// merged header cell reads as in every column but the first.
pub fn identity() -> String {
    format!("ort-2.0.0-rc.13+{MODEL_ID}+RapidAI/RapidTable+{SIDE}px-bgr-imagenet+spans-expanded")
}

pub fn footprint_bytes() -> u64 {
    DIGESTS.iter().map(|(size, _)| size).sum()
}

/// Under `recognizers/`, beside Texify, because that is what this is: a row of
/// the recognizer catalogue, installed and offered like the others. It is not
/// under `layout/` — the detector decides *which* areas are read and this reads
/// one of them.
pub fn install_dir(model_dir: &Path) -> PathBuf {
    model_dir.join("recognizers").join(MODEL_ID)
}

pub fn is_installed(model_dir: &Path) -> bool {
    let dir = install_dir(model_dir);
    ARTIFACTS.iter().all(|name| dir.join(name).is_file())
}

/// What the model is, where it came from, and under what terms.
pub fn inventory() -> crate::types::RecognizerInventory {
    crate::types::RecognizerInventory {
        name: MODEL_ID.to_string(),
        // Not a hub repo: the pin is [`SOURCE_URL`] plus the digest below, and
        // saying so here is what keeps the disclosure honest beside a download
        // button that shows a repo for every other model.
        repo: SOURCE_URL.to_string(),
        // The file is served off `master` and pinned by digest instead. Stated
        // rather than left blank: a revision field that read like a commit
        // would claim a guarantee this source does not give.
        revision: format!("sha256:{}", DIGESTS[0].1),
        license: "Apache-2.0".to_string(),
        license_url: "https://www.modelscope.cn/models/RapidAI/RapidTable".to_string(),
        derived_from: vec![
            "SLANet-plus (Apache-2.0, PaddlePaddle) — the table structure model".to_string(),
            "ONNX export published by RapidAI in RapidTable (Apache-2.0)".to_string(),
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

/// Fetch the graph into `model_dir`, checking it against the size and digest
/// declared above.
///
/// The same shape as [`super::texify::install`] and
/// [`super::doclayout::install`] — fetch, verify, remove what does not match —
/// with the fetch coming from [`crate::models::downloader`] rather than from
/// `hf-hub` because this artifact is not on that hub. The verification is the
/// same function all three use, which is the point: one answer to "is this the
/// file the recipe names".
pub fn install(
    model_dir: &Path,
    progress: Option<crate::models::progress::ProgressTx>,
) -> anyhow::Result<()> {
    let dir = install_dir(model_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|error| anyhow::anyhow!("could not create {}: {error}", dir.display()))?;

    for (filename, (size_bytes, sha256)) in ARTIFACTS.iter().zip(DIGESTS) {
        let target = dir.join(filename);
        if target.is_file() && super::verify_artifact(&target, *size_bytes, sha256).is_ok() {
            continue;
        }
        crate::models::downloader::LocalModelManager::download(
            SOURCE_URL,
            &target,
            *size_bytes,
            progress.clone(),
        )?;
        // A file that does not match is removed rather than left where
        // `is_installed` would count it: a table reader that reads as installed
        // and is not is worse than one that reads as absent.
        if let Err(error) = super::verify_artifact(&target, *size_bytes, sha256) {
            let _ = std::fs::remove_file(&target);
            return Err(error);
        }
    }
    Ok(())
}

// ── What crosses the pipe ────────────────────────────────────────────────────

/// The table crops of one document, staged for the structure model in the
/// worker.
///
/// A document's crops go in one request, for the same reason its images and its
/// pages do: **no worker is ever started inside a loop.** See
/// [`super::LayoutRequest`].
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TableStructureRequest {
    /// One PNG per crop, in the order the results must come back.
    pub image_paths: Vec<PathBuf>,
}

/// One cell of a grid, as it crosses the pipe.
///
/// Geometry and nothing else. The box is in **fractions of the crop** — not
/// pixels of whatever PNG the host happened to stage, and certainly not text —
/// so the host maps it onto the page with the rectangle it already holds and
/// the worker never learns what the page says.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TableCell {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub colspan: usize,
    pub rowspan: usize,
    /// Where the grid put it, once spans were laid out.
    pub row: usize,
    pub col: usize,
}

impl TableCell {
    pub fn spans(&self) -> bool {
        self.colspan > 1 || self.rowspan > 1
    }
}

/// What one call to the structure model produced for one crop.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TableGrid {
    pub cells: Vec<TableCell>,
    pub rows: usize,
    pub cols: usize,
    /// The mean of the per-step maxima, as `TableLabelDecode` reports it.
    /// Carried onto the region's confidence so it is answerable afterwards;
    /// nothing admits on it.
    pub score: f32,
    /// The decode ran to the end of the graph's step budget without ever
    /// emitting `eos`, so the token stream did not close and the tail of the
    /// table is missing by construction. Reported, never suppressed:
    /// [`super::ocr::admit`] refuses it ahead of every structural rule.
    pub truncated: bool,
}

/// Something that can say what grid a table crop holds.
///
/// One method and one answer, like [`super::LayoutModel`]. Everything about
/// which crops are read, what their cells then contain and whether the reading
/// is admitted belongs to the host.
pub trait TableStructure: Send + Sync {
    /// The recipe this model reads tables under, as it enters the extraction
    /// identity.
    fn identity(&self) -> String;

    /// Declared for the catalogue's sake; see [`ADMISSION_THRESHOLD`].
    fn admission_threshold(&self) -> f32 {
        ADMISSION_THRESHOLD
    }

    /// One grid per crop, in the order they were given.
    ///
    /// A batch and not a single crop, because this is the unit that crosses a
    /// process boundary and the boundary is what makes it a batch — see
    /// [`super::ocr::OcrEngine::spot_batch`], which is a batch for the same
    /// reason and not for throughput.
    fn read_batch(&self, images: &[image::RgbImage]) -> anyhow::Result<Vec<TableGrid>>;

    /// Let go of whatever this keeps resident, without disabling it. A later
    /// `read_batch` works exactly as before, at the cost of a reload.
    fn release(&self);
}

// ── The grid, decoded ────────────────────────────────────────────────────────

/// Walk a structure-token stream into a grid, honouring `colspan` and
/// `rowspan`.
///
/// The same walk as RapidTable's `decode_one_logic_points`: a cell takes the
/// first column of its row that nothing above it already occupies, and marks
/// out the rectangle its spans cover so the cells after it step around it.
///
/// `boxes` are already in fractions of the crop, in the order the `<td` tokens
/// appear. A cell whose token arrived with no box left is given a degenerate
/// one rather than dropped: the *grid* is what the token stream says, and
/// silently losing a cell would change the table's shape to hide a decode that
/// ran out of boxes. Such a cell contains no point, so the fill gives it
/// nothing and it counts as empty — which is the fact, visible where admission
/// can act on it.
///
/// `pub` so the decode is testable without the graph, which is the only way a
/// `colspan` case the corpus does not contain can be covered at all.
pub fn lay_out(tokens: &[&str], boxes: &[[f32; 4]]) -> TableGrid {
    let mut cells: Vec<TableCell> = Vec::new();
    let mut occupied: std::collections::BTreeSet<(usize, usize)> =
        std::collections::BTreeSet::new();
    let (mut row, mut col) = (0usize, 0usize);
    let (mut rows, mut cols) = (0usize, 0usize);
    let mut next_box = 0usize;

    let span_of = |attr: &str, token: &str| -> Option<usize> {
        // ` colspan="12"` — the value is between the quotes. Split rather than
        // sliced: nothing here indexes a string by byte offset.
        let trimmed = token.trim();
        let mut parts = trimmed.split('"');
        let name = parts.next()?;
        if name.trim_end_matches('=') != attr {
            return None;
        }
        parts.next()?.parse::<usize>().ok()
    };

    let mut index = 0usize;
    while index < tokens.len() {
        let token = tokens[index];
        match token {
            "<tr>" => col = 0,
            "</tr>" => {
                row += 1;
                rows = rows.max(row);
            }
            "<td" | "<td></td>" | "<td>" => {
                let (mut colspan, mut rowspan) = (1usize, 1usize);
                if token == "<td" {
                    let mut ahead = index + 1;
                    while ahead < tokens.len() && tokens[ahead] != ">" {
                        if let Some(n) = span_of("colspan", tokens[ahead]) {
                            colspan = n.max(1);
                        }
                        if let Some(n) = span_of("rowspan", tokens[ahead]) {
                            rowspan = n.max(1);
                        }
                        ahead += 1;
                    }
                    index = ahead;
                }
                while occupied.contains(&(row, col)) {
                    col += 1;
                }
                let bbox = boxes.get(next_box).copied().unwrap_or([0.0; 4]);
                next_box += 1;
                for r in row..row + rowspan {
                    for c in col..col + colspan {
                        occupied.insert((r, c));
                    }
                }
                cells.push(TableCell {
                    x0: bbox[0],
                    y0: bbox[1],
                    x1: bbox[2],
                    y1: bbox[3],
                    colspan,
                    rowspan,
                    row,
                    col,
                });
                cols = cols.max(col + colspan);
                rows = rows.max(row + rowspan);
                col += colspan;
            }
            _ => {}
        }
        index += 1;
    }
    TableGrid {
        cells,
        rows,
        cols,
        score: 0.0,
        truncated: false,
    }
}

// ── Filling the cells from the page's own glyphs ─────────────────────────────

/// What a filled grid is, beside its Markdown: the facts admission judges it
/// on.
///
/// Carried on [`super::ocr::SpottedRegion`] and therefore into the annotation
/// cache, because admission is re-decided when the rules move and a summary
/// that had to be recomputed from pixels would mean re-running the model to
/// change a threshold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TableFillSummary {
    /// Grid positions in total, `rows * cols`, spans expanded.
    pub cells: u32,
    /// Of those, the ones no word of the page fell into.
    pub empty_cells: u32,
    /// Words the page draws inside the crop's own rectangle that landed in no
    /// cell at all.
    pub unassigned_words: u32,
    /// Words the page draws inside the crop's own rectangle, the denominator
    /// for the one above.
    pub words_in_box: u32,
    /// Whether the first row of the grid holds no glyph at all.
    pub first_row_empty: bool,
}

/// A grid with the page's own words in it.
#[derive(Clone, Debug)]
pub struct FilledTable {
    /// The grid as Markdown, which is what enters the reading.
    pub markdown: String,
    pub summary: TableFillSummary,
}

/// Put every word the page draws inside `covered` into the cell it falls in,
/// and build the table.
///
/// `covered` is the page rectangle the crop's canvas covers — the one
/// `typeset::render` hands back and `DiscoveredImage::bbox` carries — so the
/// map from a cell's fractions to a point of the page is exact rather than an
/// approximation of the render scale.
///
/// Words are taken in the survey's own order, which is the page's reading
/// order, so a cell holding three words holds them in the order the page sets
/// them. A word inside the rectangle that no cell claimed is counted: it is the
/// proxy for a structure the model missed, because the glyphs are certainly
/// there and the grid had nowhere to put them.
///
/// A word with no characters behind it is an **error**, not an empty cell. The
/// page's word list and the boxes it is addressed by are two walks over one
/// text page, and a word in one that is blank in the other means they disagree
/// — which would silently drop a cell's text and leave the table looking merely
/// sparse, exactly the shape the admission rules below are trying to catch.
pub fn fill_from_page(
    grid: &TableGrid,
    covered: &BoundingBox,
    words: &[NativeTextOnPage],
) -> anyhow::Result<FilledTable> {
    let boxes: Vec<(f32, f32, f32, f32)> = grid
        .cells
        .iter()
        .map(|cell| {
            let to_page = |fx: f32, fy: f32| {
                (
                    covered.x + fx * covered.width,
                    covered.y + fy * covered.height,
                )
            };
            let (x0, y0) = to_page(cell.x0, cell.y0);
            let (x1, y1) = to_page(cell.x1, cell.y1);
            (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
        })
        .collect();

    let mut text: Vec<String> = vec![String::new(); grid.cells.len()];
    let mut unassigned = 0u32;
    let mut words_in_box = 0u32;
    for word in words {
        let cx = word.bbox.x + word.bbox.width / 2.0;
        let cy = word.bbox.y + word.bbox.height / 2.0;
        let in_table = cx >= covered.x
            && cx <= covered.x + covered.width
            && cy >= covered.y
            && cy <= covered.y + covered.height;
        if !in_table {
            continue;
        }
        anyhow::ensure!(
            !word.text.trim().is_empty(),
            "the page's word list holds a word with no characters at ({:.1}, {:.1}) on page {}; \
             the two walks over this page's text disagree",
            word.bbox.x,
            word.bbox.y,
            word.page
        );
        words_in_box += 1;
        // Smallest containing cell, so a cell nested inside a spanning one
        // takes the word rather than the span swallowing it.
        let hit = boxes
            .iter()
            .enumerate()
            .filter(|(_, (x0, y0, x1, y1))| cx >= *x0 && cx <= *x1 && cy >= *y0 && cy <= *y1)
            .min_by(|a, b| {
                let area = |c: &(f32, f32, f32, f32)| (c.2 - c.0) * (c.3 - c.1);
                area(a.1).total_cmp(&area(b.1))
            })
            .map(|(index, _)| index);
        match hit {
            Some(index) => {
                if !text[index].is_empty() {
                    text[index].push(' ');
                }
                text[index].push_str(&word.text);
            }
            None => unassigned += 1,
        }
    }

    let expanded = expand(grid, &text);
    let filled = expanded
        .iter()
        .flatten()
        .filter(|cell| !cell.trim().is_empty())
        .count() as u32;
    let positions = (grid.rows * grid.cols) as u32;
    Ok(FilledTable {
        markdown: to_markdown(&expanded),
        summary: TableFillSummary {
            cells: positions,
            empty_cells: positions.saturating_sub(filled),
            unassigned_words: unassigned,
            words_in_box,
            first_row_empty: expanded
                .first()
                .is_none_or(|row| row.iter().all(|cell| cell.trim().is_empty())),
        },
    })
}

/// The grid as a rectangle of strings, spans **expanded**.
///
/// A cell with `colspan="3"` writes its text into all three grid positions it
/// covers, and likewise down a `rowspan`. That keeps the table rectangular,
/// which is what [`super::ocr::markdown_table_is_rectangular`] demands, and it
/// keeps every cell's text findable at every column it belongs to — which is
/// what the index is for. The alternative, writing the text once and leaving
/// the rest blank, loses a merged header from every column but the first.
fn expand(grid: &TableGrid, text: &[String]) -> Vec<Vec<String>> {
    if grid.rows == 0 || grid.cols == 0 {
        return Vec::new();
    }
    let mut out = vec![vec![String::new(); grid.cols]; grid.rows];
    for (index, cell) in grid.cells.iter().enumerate() {
        let Some(text) = text.get(index) else {
            continue;
        };
        let rows = out
            .iter_mut()
            .take((cell.row + cell.rowspan).min(grid.rows))
            .skip(cell.row);
        for row in rows {
            let positions = row
                .iter_mut()
                .take((cell.col + cell.colspan).min(grid.cols))
                .skip(cell.col);
            for position in positions {
                position.clone_from(text);
            }
        }
    }
    out
}

/// The expanded grid as Markdown, the way the pipeline's own admission wants
/// it: a header row, its delimiter, and a row per remaining row.
///
/// A `|` inside a cell is escaped, because an unescaped one would silently
/// widen the row and turn a correct read into a ragged table.
fn to_markdown(grid: &[Vec<String>]) -> String {
    let Some(first) = grid.first() else {
        return String::new();
    };
    let width = first.len();
    let mut out = String::new();
    for (index, row) in grid.iter().enumerate() {
        out.push_str("| ");
        let escaped: Vec<String> = row.iter().map(|cell| cell.replace('|', "\\|")).collect();
        out.push_str(&escaped.join(" | "));
        out.push_str(" |\n");
        if index == 0 {
            out.push_str("| ");
            out.push_str(&vec!["---"; width].join(" | "));
            out.push_str(" |\n");
        }
    }
    out
}

// ── The model, in the worker ─────────────────────────────────────────────────

#[cfg(feature = "recognize-onnx")]
mod graph {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use anyhow::Context as _;
    use image::RgbImage;
    use ort::session::{Session, SessionInputValue};
    use ort::value::Tensor;
    use tracing::debug;

    use super::{
        install_dir, is_installed, lay_out, TableGrid, TableStructure, DICT_KEY, MEAN, MODEL_ID,
        SIDE, STD, TD_TOKENS,
    };

    /// One loaded copy of the graph, and what it declared about itself.
    struct Reader {
        session: Session,
        /// `["sos"] + dictionary + ["eos"]`, which is what the argmax indexes.
        dict: Vec<String>,
        bbox_output: usize,
        probs_output: usize,
        /// How many numbers the graph gives per cell: 4 (x0,y0,x1,y1) or 8
        /// (four corners). Both are handled; which one this file is decides at
        /// load.
        bbox_width: usize,
    }

    /// SLANet-plus, addressed.
    ///
    /// One reader, not several. A crop is 23 ms — three orders of magnitude
    /// under a page reader's — and a document's whole table budget on the
    /// measured 168-page textbook is 1.3 s, so a second copy of the graph would
    /// buy nothing for 7.4 MB of resident set. `Mutex<Option<..>>` for the same
    /// reason [`super::super::doclayout::DocLayout`] uses one: `Session::run`
    /// needs `&mut`, and `release` is a promise that a later read still works.
    pub struct SlanetPlus {
        path: PathBuf,
        threads: usize,
        reader: Mutex<Option<Reader>>,
    }

    impl SlanetPlus {
        /// Check the graph is here and note how to load it. Loading is deferred
        /// to the first crop — a runtime that attaches a reader and then indexes
        /// nothing should not pay for it.
        pub fn load(model_dir: &Path, threads: usize) -> anyhow::Result<Self> {
            let path = install_dir(model_dir).join(super::GRAPH);
            anyhow::ensure!(
                is_installed(model_dir),
                "the {MODEL_ID} table reader is not installed: {} is missing",
                path.display()
            );
            anyhow::ensure!(threads > 0, "a table reader needs at least one thread");
            Ok(Self {
                path,
                threads,
                reader: Mutex::new(None),
            })
        }

        fn open(&self) -> anyhow::Result<Reader> {
            let session = Session::builder()
                .map_err(|e| anyhow::anyhow!("could not start an ONNX session builder: {e}"))?
                .with_intra_threads(self.threads)
                .map_err(|e| anyhow::anyhow!("could not set the session thread count: {e}"))?
                .commit_from_file(&self.path)
                .map_err(|e| anyhow::anyhow!("could not load {}: {e}", self.path.display()))?;

            let raw = session
                .metadata()
                .map_err(|e| anyhow::anyhow!("the graph has no readable metadata: {e}"))?
                .custom(DICT_KEY)
                .with_context(|| {
                    format!(
                        "the graph carries no `{DICT_KEY}` metadata; there is no token dictionary"
                    )
                })?;
            // `str::lines`, which is what `splitlines` is here. A trailing empty
            // line would be a token, so it is not tolerated silently.
            let mut dict: Vec<String> = raw.lines().map(str::to_string).collect();
            anyhow::ensure!(
                dict.iter().all(|token| !token.is_empty()),
                "the token dictionary holds an empty entry"
            );
            // `TableLabelDecode(merge_no_span_structure=True)`, verbatim.
            if !dict.iter().any(|token| token == "<td></td>") {
                dict.push("<td></td>".to_string());
            }
            dict.retain(|token| token != "<td>");
            let mut full = vec!["sos".to_string()];
            full.extend(dict);
            full.push("eos".to_string());

            // Which output is which is asked of the graph rather than assumed
            // from an ordering: the class count is the dictionary's, and the
            // other output is the boxes. A graph that answered neither shape is
            // a graph this decoder cannot read, and says so.
            let outputs = session.outputs();
            anyhow::ensure!(
                outputs.len() == 2,
                "the graph declares {} outputs, 2 expected",
                outputs.len()
            );
            let last_dim = |index: usize| -> Option<usize> {
                outputs[index]
                    .dtype()
                    .tensor_shape()
                    .and_then(|shape| shape.last().copied())
                    .and_then(|d| usize::try_from(d).ok())
            };
            let (probs_output, bbox_output) = match (last_dim(0), last_dim(1)) {
                (Some(a), _) if a == full.len() => (0, 1),
                (_, Some(b)) if b == full.len() => (1, 0),
                (a, b) => anyhow::bail!(
                    "neither output ends in {} classes (got {a:?} and {b:?}); this is not the \
                     dictionary this graph was exported with",
                    full.len()
                ),
            };
            let bbox_width = last_dim(bbox_output).with_context(|| {
                "the box output declares no static width; this decoder needs 4 or 8".to_string()
            })?;
            anyhow::ensure!(
                bbox_width == 4 || bbox_width == 8,
                "the box output is {bbox_width} wide; 4 or 8 expected"
            );

            debug!(
                classes = full.len(),
                bbox_width, "loaded {MODEL_ID}'s structure graph"
            );
            Ok(Reader {
                session,
                dict: full,
                bbox_output,
                probs_output,
                bbox_width,
            })
        }

        /// One forward pass and its decode, over one crop.
        ///
        /// The box coordinates come back in **fractions of the crop**. The
        /// graph answers in fractions of the padded square, and RapidTable's
        /// two-step undo of that — multiply by the original side, then rescale
        /// by `resized / (side * ratio)` — composes to a single multiplication
        /// by the crop's longest side, because that is what the square covers.
        /// Written as the composition rather than as the two steps, with the
        /// two steps stated so the next reader can check the algebra rather
        /// than trust it. Dividing by the crop's own width and height then
        /// gives the fractions the host maps onto the page.
        fn read(reader: &mut Reader, crop: &RgbImage) -> anyhow::Result<TableGrid> {
            let image = preprocess(crop)?;
            let names: Vec<String> = reader
                .session
                .inputs()
                .iter()
                .map(|input| input.name().to_string())
                .collect();
            anyhow::ensure!(
                names.len() == 1,
                "the graph declares {} inputs, 1 expected",
                names.len()
            );
            let feed: Vec<(String, SessionInputValue)> = vec![(names[0].clone(), image.into())];
            let outputs = reader
                .session
                .run(feed)
                .map_err(|e| anyhow::anyhow!("SLANet-plus failed: {e}"))?;

            let (probs_shape, probs) = outputs[reader.probs_output]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("the structure output is not f32: {e}"))?;
            let (bbox_shape, boxes) = outputs[reader.bbox_output]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("the box output is not f32: {e}"))?;
            anyhow::ensure!(
                probs_shape.len() == 3 && bbox_shape.len() == 3,
                "the graph answered {probs_shape:?} and {bbox_shape:?}, two rank-3 tensors expected"
            );
            let steps = probs_shape[1] as usize;
            let classes = probs_shape[2] as usize;
            anyhow::ensure!(
                classes == reader.dict.len(),
                "the graph scores {classes} classes, the dictionary holds {}",
                reader.dict.len()
            );
            anyhow::ensure!(
                bbox_shape[1] as usize == steps && bbox_shape[2] as usize == reader.bbox_width,
                "the box output is {bbox_shape:?}, {steps}x{} expected",
                reader.bbox_width
            );

            let end = reader
                .dict
                .iter()
                .position(|token| token == "eos")
                .expect("the dictionary was built with an eos");
            let beg = reader
                .dict
                .iter()
                .position(|token| token == "sos")
                .expect("the dictionary was built with a sos");
            // The square covers the crop's longest side; see the doc comment.
            let scale = crop.width().max(crop.height()) as f32;
            let (width, height) = (crop.width() as f32, crop.height() as f32);

            let mut tokens: Vec<&str> = Vec::new();
            let mut cell_boxes: Vec<[f32; 4]> = Vec::new();
            let mut scores: Vec<f32> = Vec::new();
            let mut truncated = true;
            for step in 0..steps {
                let row = &probs[step * classes..(step + 1) * classes];
                let (index, best) =
                    row.iter()
                        .enumerate()
                        .fold(
                            (0usize, f32::MIN),
                            |acc, (i, v)| {
                                if *v > acc.1 {
                                    (i, *v)
                                } else {
                                    acc
                                }
                            },
                        );
                if step > 0 && index == end {
                    truncated = false;
                    break;
                }
                if index == beg || index == end {
                    continue;
                }
                let token = reader.dict[index].as_str();
                if TD_TOKENS.contains(&token) {
                    let raw = &boxes[step * reader.bbox_width..(step + 1) * reader.bbox_width];
                    // Four numbers are a rectangle; eight are its corners, and
                    // the enclosing rectangle is what a cell is here either way.
                    let xs: Vec<f32> = raw.iter().step_by(2).copied().collect();
                    let ys: Vec<f32> = raw.iter().skip(1).step_by(2).copied().collect();
                    let fold = |v: &[f32], f: fn(f32, f32) -> f32| v.iter().copied().fold(v[0], f);
                    cell_boxes.push([
                        fold(&xs, f32::min) * scale / width,
                        fold(&ys, f32::min) * scale / height,
                        fold(&xs, f32::max) * scale / width,
                        fold(&ys, f32::max) * scale / height,
                    ]);
                }
                tokens.push(token);
                scores.push(best);
            }

            let mut grid = lay_out(&tokens, &cell_boxes);
            grid.score = if scores.is_empty() {
                0.0
            } else {
                scores.iter().sum::<f32>() / scores.len() as f32
            };
            grid.truncated = truncated;
            Ok(grid)
        }
    }

    /// Resize the longest side to [`SIDE`], normalize, and pad the rest of the
    /// square with zeros — in that order, so the pad is zero in *normalized*
    /// space, which is what `TablePreprocess.pad_img` produces.
    fn preprocess(crop: &RgbImage) -> anyhow::Result<Tensor<f32>> {
        let (w, h) = (crop.width(), crop.height());
        anyhow::ensure!(w > 0 && h > 0, "an empty crop cannot be read");
        let ratio = SIDE as f32 / w.max(h) as f32;
        // `int()` in Python truncates, and the pad below covers whatever the
        // truncation left over.
        let (rw, rh) = ((w as f32 * ratio) as u32, (h as f32 * ratio) as u32);
        let (rw, rh) = (rw.clamp(1, SIDE), rh.clamp(1, SIDE));
        // Bilinear: `cv2.resize`'s default interpolation.
        let resized = image::imageops::resize(crop, rw, rh, image::imageops::FilterType::Triangle);

        let side = SIDE as usize;
        let mut chw = vec![0f32; 3 * side * side];
        for (x, y, pixel) in resized.enumerate_pixels() {
            let (x, y) = (x as usize, y as usize);
            // BGR, for the reason given at `MEAN`.
            let bgr = [pixel.0[2], pixel.0[1], pixel.0[0]];
            for channel in 0..3 {
                chw[channel * side * side + y * side + x] =
                    (f32::from(bgr[channel]) / 255.0 - MEAN[channel]) / STD[channel];
            }
        }
        Tensor::from_array((vec![1i64, 3, side as i64, side as i64], chw))
            .context("could not build SLANet-plus' input tensor")
    }

    impl TableStructure for SlanetPlus {
        fn identity(&self) -> String {
            super::identity()
        }

        /// The crops of one document, one at a time on the one resident graph.
        ///
        /// The loop is here, next to the model, because that is where a kill
        /// ends it — see the "workers are never started inside a loop"
        /// invariant in `AGENTS.md`. It is not a parallel loop: a crop is 23 ms
        /// and the whole of a 168-page textbook's tables is 1.3 s, so there is
        /// nothing here for a second reader to save.
        fn read_batch(&self, images: &[RgbImage]) -> anyhow::Result<Vec<TableGrid>> {
            if images.is_empty() {
                return Ok(Vec::new());
            }
            let mut held = self
                .reader
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if held.is_none() {
                *held = Some(self.open()?);
            }
            let reader = held.as_mut().expect("just loaded");
            let mut out = Vec::with_capacity(images.len());
            for (index, image) in images.iter().enumerate() {
                let grid = Self::read(reader, image)?;
                debug!(
                    "read table {} of {} with {MODEL_ID}: {}x{}, {} cell(s)",
                    index + 1,
                    images.len(),
                    grid.rows,
                    grid.cols,
                    grid.cells.len()
                );
                out.push(grid);
            }
            Ok(out)
        }

        fn release(&self) {
            if self
                .reader
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .is_some()
            {
                debug!("{MODEL_ID} released its graph; the next crop reloads it");
            }
        }
    }
}

#[cfg(feature = "recognize-onnx")]
pub use graph::SlanetPlus;

// ── The model as the host addresses it ───────────────────────────────────────

/// The table reader as the host addresses it: a proxy over the worker protocol.
///
/// The same shape as [`super::worker_ocr::WorkerOcr`] and
/// [`super::worker_layout::WorkerLayout`], and here rather than in a file of
/// its own because the grid it stages for and the fill it feeds are in this
/// module: what the host puts on the wire and what it does with the reply are
/// one rule.
///
/// `identity` and `admission_threshold` answer from constants without asking
/// the worker. They enter the extraction recipe and are needed before a single
/// crop is sent.
pub struct WorkerTableStructure {
    manager: crate::worker::manager::WorkerManager,
    /// Captured at construction, always in an async context, so `read_batch`
    /// can be called from the blocking extraction thread it actually runs on.
    tokio_handle: tokio::runtime::Handle,
    engine: super::dispatch::RecognitionEngine,
    model_id: String,
    model_dir: PathBuf,
    /// Where a batch's PNGs are staged. Under the application's own data
    /// directory rather than the system temp: these are pages of the user's
    /// documents, and they should not be written somewhere world-readable.
    scratch_root: PathBuf,
    identity: String,
}

impl WorkerTableStructure {
    /// Stage one batch as PNG files and return where they went.
    ///
    /// The directory is returned alongside the paths so the caller holds it for
    /// exactly as long as the request: dropping it removes the files, whether
    /// the request succeeded, failed or was killed underneath.
    fn stage(
        &self,
        images: &[image::RgbImage],
    ) -> anyhow::Result<(tempfile::TempDir, Vec<PathBuf>)> {
        std::fs::create_dir_all(&self.scratch_root).map_err(|error| {
            anyhow::anyhow!(
                "could not create the table scratch directory {}: {error}",
                self.scratch_root.display()
            )
        })?;
        let staged = tempfile::Builder::new()
            .prefix("tables-")
            .tempdir_in(&self.scratch_root)?;
        let mut paths = Vec::with_capacity(images.len());
        for (index, image) in images.iter().enumerate() {
            let path = staged.path().join(format!("{index}.png"));
            image
                .save_with_format(&path, image::ImageFormat::Png)
                .map_err(|error| anyhow::anyhow!("could not stage table crop {index}: {error}"))?;
            paths.push(path);
        }
        Ok((staged, paths))
    }
}

impl TableStructure for WorkerTableStructure {
    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn read_batch(&self, images: &[image::RgbImage]) -> anyhow::Result<Vec<TableGrid>> {
        use crate::worker::fault::WorkerFault;
        use crate::worker::ipc::{WorkerEvent, WorkerRequest, WorkerRole};
        use crate::worker::manager::ManagerCommand;

        if images.is_empty() {
            return Ok(Vec::new());
        }
        // Held until the request is over. Named, because dropping it is what
        // removes the files and an unnamed temporary would drop it here.
        let (_staged, image_paths) = self.stage(images)?;
        let expected = image_paths.len();

        let request = WorkerRequest {
            mode: "table".to_string(),
            role: WorkerRole::Table(self.engine),
            model: self.model_id.clone(),
            model_dir: self.model_dir.clone(),
            // ONNX Runtime's CPU provider is the only one this graph was
            // measured on. Naming a device it will not honour would be a
            // setting that reads as a promise.
            device: "cpu".to_string(),
            texts: None,
            generate: None,
            recognize: None,
            layout: None,
            table: Some(TableStructureRequest { image_paths }),
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let cmd = ManagerCommand::Submit {
            req: Box::new(request),
            reply: tx,
        };

        let grids: Vec<TableGrid> = self.tokio_handle.block_on(async move {
            self.manager.send(cmd).await.map_err(|error| {
                WorkerFault::gone(format!("could not reach the table reader: {error}"))
            })?;

            while let Some(event) = rx.recv().await {
                match event {
                    WorkerEvent::TableStructures(grids) => return Ok(grids),
                    WorkerEvent::Error(error) => {
                        return Err(WorkerFault::reported(format!(
                            "table reader error: {error}"
                        )))
                    }
                    WorkerEvent::Gone(detail) => return Err(WorkerFault::gone(detail)),
                    WorkerEvent::Done => break,
                    _ => {}
                }
            }
            Err(anyhow::anyhow!(
                "the table reader finished without returning any grid"
            ))
        })?;

        // Results are positional — the caller pairs them back with its own
        // crops by index — so a short reply is a wrong answer, not a partial
        // one, and must not be silently zipped against the wrong crops.
        anyhow::ensure!(
            grids.len() == expected,
            "the table reader answered for {} of {expected} crop(s)",
            grids.len()
        );
        Ok(grids)
    }

    /// Knock the reader down, freeing the graph it holds.
    ///
    /// The manager keeps supervising: the next batch spawns a replacement and
    /// loads again.
    fn release(&self) {
        let _guard = self.tokio_handle.enter();
        self.manager.request_shutdown();
    }
}

/// The table reader addressed through `manager`, as a [`TableStructure`].
///
/// Cheap: it resolves constants and keeps a handle, and loads nothing. The
/// graph is loaded by the worker, on its first crop.
pub fn attach(
    manager: crate::worker::manager::WorkerManager,
    engine: super::dispatch::RecognitionEngine,
    model_id: &str,
    model_dir: PathBuf,
    scratch_root: PathBuf,
) -> anyhow::Result<Box<dyn TableStructure>> {
    anyhow::ensure!(
        model_id == MODEL_ID,
        "unknown table reader '{model_id}'; this build ships {MODEL_ID}"
    );
    Ok(Box::new(WorkerTableStructure {
        manager,
        tokio_handle: tokio::runtime::Handle::current(),
        engine,
        model_id: model_id.to_string(),
        model_dir,
        scratch_root,
        identity: identity(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(page: u32, text: &str, x: f32, y: f32, w: f32, h: f32) -> NativeTextOnPage {
        NativeTextOnPage {
            page,
            text: text.to_string(),
            bbox: BoundingBox {
                x,
                y,
                width: w,
                height: h,
            },
        }
    }

    /// A cell's box covering the whole of a grid position, in fractions.
    fn cell(row: usize, col: usize, rows: usize, cols: usize) -> TableCell {
        TableCell {
            x0: col as f32 / cols as f32,
            y0: row as f32 / rows as f32,
            x1: (col + 1) as f32 / cols as f32,
            y1: (row + 1) as f32 / rows as f32,
            colspan: 1,
            rowspan: 1,
            row,
            col,
        }
    }

    // ── The decode ───────────────────────────────────────────────────────────

    /// The plain case: two rows of two `<td></td>`, in reading order.
    #[test]
    fn a_token_stream_of_plain_cells_becomes_a_rectangular_grid() {
        let tokens = [
            "<tr>",
            "<td></td>",
            "<td></td>",
            "</tr>",
            "<tr>",
            "<td></td>",
            "<td></td>",
            "</tr>",
        ];
        let boxes = [
            [0.0, 0.0, 0.5, 0.5],
            [0.5, 0.0, 1.0, 0.5],
            [0.0, 0.5, 0.5, 1.0],
            [0.5, 0.5, 1.0, 1.0],
        ];
        let grid = lay_out(&tokens, &boxes);
        assert_eq!((grid.rows, grid.cols), (2, 2));
        assert_eq!(grid.cells.len(), 4);
        let at: Vec<(usize, usize)> = grid.cells.iter().map(|c| (c.row, c.col)).collect();
        assert_eq!(at, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
        assert!(grid.cells.iter().all(|cell| !cell.spans()));
        // The boxes are taken in token order, one per `<td` token.
        assert_eq!(grid.cells[1].x0, 0.5);
    }

    /// A `colspan` header over two columns. The corpus this was measured on
    /// held none, so this is the only place the attribute walk is exercised.
    #[test]
    fn a_colspan_cell_occupies_every_column_it_covers() {
        let tokens = [
            "<tr>",
            "<td",
            " colspan=\"2\"",
            ">",
            "</td>",
            "</tr>",
            "<tr>",
            "<td></td>",
            "<td></td>",
            "</tr>",
        ];
        let boxes = [
            [0.0, 0.0, 1.0, 0.5],
            [0.0, 0.5, 0.5, 1.0],
            [0.5, 0.5, 1.0, 1.0],
        ];
        let grid = lay_out(&tokens, &boxes);
        assert_eq!((grid.rows, grid.cols), (2, 2));
        assert_eq!(grid.cells.len(), 3);
        assert_eq!((grid.cells[0].colspan, grid.cells[0].rowspan), (2, 1));
        assert!(grid.cells[0].spans());
        // The cells after it start where it ends, not where it began.
        assert_eq!((grid.cells[1].row, grid.cells[1].col), (1, 0));
        assert_eq!((grid.cells[2].row, grid.cells[2].col), (1, 1));
    }

    /// A `rowspan` cell in the first column: the second row's first cell must
    /// step around it into column one, not sit under it.
    #[test]
    fn a_rowspan_cell_pushes_the_row_below_it_along() {
        let tokens = [
            "<tr>",
            "<td",
            " rowspan=\"2\"",
            ">",
            "</td>",
            "<td></td>",
            "</tr>",
            "<tr>",
            "<td></td>",
            "</tr>",
        ];
        let boxes = [
            [0.0, 0.0, 0.5, 1.0],
            [0.5, 0.0, 1.0, 0.5],
            [0.5, 0.5, 1.0, 1.0],
        ];
        let grid = lay_out(&tokens, &boxes);
        assert_eq!((grid.rows, grid.cols), (2, 2));
        assert_eq!((grid.cells[0].rowspan, grid.cells[0].colspan), (2, 1));
        assert_eq!((grid.cells[1].row, grid.cells[1].col), (0, 1));
        assert_eq!(
            (grid.cells[2].row, grid.cells[2].col),
            (1, 1),
            "the cell below a rowspan takes the next free column, not the occupied one"
        );
    }

    /// A `<td` token with no box left is still a cell of the grid. Dropping it
    /// would change the table's shape to hide a decode that ran out of boxes.
    #[test]
    fn a_cell_with_no_box_left_keeps_its_place_in_the_grid() {
        let tokens = ["<tr>", "<td></td>", "<td></td>", "</tr>"];
        let grid = lay_out(&tokens, &[[0.0, 0.0, 0.5, 1.0]]);
        assert_eq!(grid.cells.len(), 2);
        assert_eq!(
            (grid.cells[1].x0, grid.cells[1].x1),
            (0.0, 0.0),
            "a cell with no box is degenerate, so it contains nothing and reads as empty"
        );
    }

    // ── The map onto the page ────────────────────────────────────────────────

    /// A cell's fractions land on the page rectangle the crop covers, exactly.
    /// A word at the centre of the top-left quarter of the crop belongs to the
    /// cell at the top-left of the grid, wherever on the page that crop was cut
    /// from.
    #[test]
    fn cell_fractions_map_onto_the_page_rectangle_the_crop_covers() {
        let grid = TableGrid {
            cells: vec![
                cell(0, 0, 2, 2),
                cell(0, 1, 2, 2),
                cell(1, 0, 2, 2),
                cell(1, 1, 2, 2),
            ],
            rows: 2,
            cols: 2,
            score: 1.0,
            truncated: false,
        };
        // A crop cut from well inside a page: 100pt across at (300, 500).
        let covered = BoundingBox {
            x: 300.0,
            y: 500.0,
            width: 100.0,
            height: 40.0,
        };
        let words = vec![
            word(3, "a", 320.0, 505.0, 4.0, 4.0),
            word(3, "b", 380.0, 505.0, 4.0, 4.0),
            word(3, "c", 320.0, 528.0, 4.0, 4.0),
            word(3, "d", 380.0, 528.0, 4.0, 4.0),
        ];
        let filled = fill_from_page(&grid, &covered, &words).unwrap();
        assert_eq!(filled.markdown, "| a | b |\n| --- | --- |\n| c | d |\n");
        assert_eq!(filled.summary.words_in_box, 4);
        assert_eq!(filled.summary.unassigned_words, 0);
        assert_eq!(filled.summary.empty_cells, 0);
        assert!(!filled.summary.first_row_empty);
    }

    /// Only the words inside the crop's own rectangle are considered. A word
    /// elsewhere on the page is not this table's, and is not counted against it
    /// either.
    #[test]
    fn words_outside_the_crop_are_neither_placed_nor_counted() {
        let grid = TableGrid {
            cells: vec![cell(0, 0, 1, 2), cell(0, 1, 1, 2)],
            rows: 1,
            cols: 2,
            score: 1.0,
            truncated: false,
        };
        let covered = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let words = vec![
            word(1, "in", 2.0, 5.0, 1.0, 1.0),
            word(1, "out", 500.0, 500.0, 1.0, 1.0),
        ];
        let filled = fill_from_page(&grid, &covered, &words).unwrap();
        assert_eq!(filled.summary.words_in_box, 1);
        assert!(filled.markdown.contains("in"));
        assert!(!filled.markdown.contains("out"));
    }

    /// A word inside a cell nested within a spanning one goes to the smaller
    /// cell: the span must not swallow what a real cell holds.
    #[test]
    fn a_word_in_nested_cells_goes_to_the_smallest_one() {
        let grid = TableGrid {
            cells: vec![
                // A header spanning both columns of a 2x2 grid.
                TableCell {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 1.0,
                    y1: 1.0,
                    colspan: 2,
                    rowspan: 2,
                    row: 0,
                    col: 0,
                },
                // And a tight cell inside its right half.
                TableCell {
                    x0: 0.5,
                    y0: 0.0,
                    x1: 1.0,
                    y1: 0.5,
                    colspan: 1,
                    rowspan: 1,
                    row: 0,
                    col: 1,
                },
            ],
            rows: 2,
            cols: 2,
            score: 1.0,
            truncated: false,
        };
        let covered = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let filled =
            fill_from_page(&grid, &covered, &[word(1, "tight", 74.0, 24.0, 2.0, 2.0)]).unwrap();
        // The word went to the nested cell, which the expansion writes at
        // (0,1) — over the span, which claims that position too but is written
        // first.
        assert!(
            filled.markdown.lines().next().unwrap().contains("tight"),
            "{}",
            filled.markdown
        );
        assert_eq!(filled.summary.unassigned_words, 0);
    }

    /// A word the page draws inside the crop that no cell contains is counted,
    /// never dropped: it is the proxy for a column the grid does not have.
    #[test]
    fn a_word_no_cell_contains_is_counted_as_unassigned() {
        let grid = TableGrid {
            cells: vec![cell(0, 0, 1, 1)],
            rows: 1,
            cols: 1,
            score: 1.0,
            truncated: false,
        };
        // The one cell covers the left half only.
        let mut grid = grid;
        grid.cells[0].x1 = 0.5;
        let covered = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 10.0,
        };
        let filled = fill_from_page(
            &grid,
            &covered,
            &[
                word(1, "left", 10.0, 5.0, 2.0, 2.0),
                word(1, "right", 90.0, 5.0, 2.0, 2.0),
            ],
        )
        .unwrap();
        assert_eq!(filled.summary.unassigned_words, 1);
        assert_eq!(filled.summary.words_in_box, 2);
    }

    /// A word with no characters behind it is an error, not an empty cell. The
    /// two walks over a page's text have disagreed, and defaulting it would
    /// leave the table looking merely sparse.
    #[test]
    fn a_word_with_no_characters_is_an_error_rather_than_a_blank_cell() {
        let grid = TableGrid {
            cells: vec![cell(0, 0, 1, 1)],
            rows: 1,
            cols: 1,
            score: 1.0,
            truncated: false,
        };
        let covered = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let error = fill_from_page(&grid, &covered, &[word(7, "  ", 5.0, 5.0, 1.0, 1.0)])
            .unwrap_err()
            .to_string();
        assert!(error.contains("page 7"), "{error}");
    }

    // ── The Markdown ─────────────────────────────────────────────────────────

    /// A spanning cell writes its text into every grid position it covers, so
    /// the table stays rectangular and a merged header is findable under every
    /// column it heads.
    #[test]
    fn a_spanning_cell_is_written_into_every_position_it_covers() {
        let grid = TableGrid {
            cells: vec![
                TableCell {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 1.0,
                    y1: 0.5,
                    colspan: 2,
                    rowspan: 1,
                    row: 0,
                    col: 0,
                },
                cell(1, 0, 2, 2),
                cell(1, 1, 2, 2),
            ],
            rows: 2,
            cols: 2,
            score: 1.0,
            truncated: false,
        };
        let covered = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let filled = fill_from_page(
            &grid,
            &covered,
            &[
                word(1, "Head", 50.0, 25.0, 2.0, 2.0),
                word(1, "a", 25.0, 75.0, 2.0, 2.0),
                word(1, "b", 75.0, 75.0, 2.0, 2.0),
            ],
        )
        .unwrap();
        assert_eq!(
            filled.markdown,
            "| Head | Head |\n| --- | --- |\n| a | b |\n"
        );
        assert!(super::super::ocr::markdown_table_is_rectangular(
            &filled.markdown
        ));
    }

    /// A pipe inside a cell is escaped. An unescaped one would widen the row
    /// and turn a correct read into a ragged table.
    #[test]
    fn a_pipe_in_a_cell_is_escaped() {
        let grid = TableGrid {
            cells: vec![cell(0, 0, 2, 1), cell(1, 0, 2, 1)],
            rows: 2,
            cols: 1,
            score: 1.0,
            truncated: false,
        };
        let covered = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let filled = fill_from_page(
            &grid,
            &covered,
            &[
                word(1, "a|b", 5.0, 2.0, 1.0, 1.0),
                word(1, "c", 5.0, 7.0, 1.0, 1.0),
            ],
        )
        .unwrap();
        assert!(filled.markdown.contains("a\\|b"), "{}", filled.markdown);
    }

    /// A grid with no rows produces no Markdown at all, rather than a header
    /// delimiter with nothing above it.
    #[test]
    fn an_empty_grid_produces_no_table() {
        let filled = fill_from_page(
            &TableGrid::default(),
            &BoundingBox {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            &[],
        )
        .unwrap();
        assert!(filled.markdown.is_empty());
        assert!(filled.summary.first_row_empty);
    }

    // ── The recipe ───────────────────────────────────────────────────────────

    /// Identity, inventory and installedness are all answerable with no graph
    /// loaded and nothing on disk — the host needs them before a worker exists.
    #[test]
    fn the_recipe_is_answerable_without_the_graph() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_installed(dir.path()));
        let id = identity();
        assert!(id.contains(MODEL_ID), "{id}");
        assert!(id.contains(&SIDE.to_string()), "{id}");

        let inventory = inventory();
        assert_eq!(inventory.name, MODEL_ID);
        assert_eq!(inventory.footprint_bytes, footprint_bytes());
        assert_eq!(inventory.artifacts.len(), ARTIFACTS.len());
        assert_eq!(inventory.artifacts[0].sha256, DIGESTS[0].1);
        assert!(inventory.repo.contains("modelscope"), "{}", inventory.repo);

        // The file the recipe names, where the recipe names it.
        std::fs::create_dir_all(install_dir(dir.path())).unwrap();
        std::fs::write(install_dir(dir.path()).join(GRAPH), b"not the graph").unwrap();
        assert!(is_installed(dir.path()));
        assert!(install_dir(dir.path()).ends_with("recognizers/slanet-plus"));
    }

    /// A model id nobody ships is refused rather than attached as the one that
    /// is, which would read tables under a recipe that never produced them.
    #[tokio::test]
    async fn attaching_an_unknown_table_reader_is_an_error() {
        let (manager, _events, _loop_fut) = crate::worker::manager::WorkerManager::new(
            crate::worker::manager::WorkerPaths {
                python_path: PathBuf::new(),
                python_package_dir: PathBuf::new(),
                requirements_path: PathBuf::new(),
                venv_dir: PathBuf::new(),
                worker_bin: PathBuf::new(),
                data_dir: PathBuf::new(),
            },
            crate::worker::ipc::WorkerKind::Recognize,
        );
        let attached = attach(
            manager,
            super::super::dispatch::RecognitionEngine::Onnx,
            "no-such-table-reader",
            PathBuf::new(),
            PathBuf::new(),
        );
        let error = match attached {
            Ok(_) => panic!("an unknown table reader must not attach"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("no-such-table-reader"),
            "{error}"
        );
    }

    /// A grid is geometry, and it crosses as ordinary JSON so a structure model
    /// written in another language could produce one. Nothing of the document's
    /// text is in it.
    #[test]
    fn grids_round_trip_as_plain_json() {
        let grids = vec![
            TableGrid {
                cells: vec![
                    TableCell {
                        x0: 0.0,
                        y0: 0.0,
                        x1: 1.0,
                        y1: 0.5,
                        colspan: 2,
                        rowspan: 1,
                        row: 0,
                        col: 0,
                    },
                    cell(1, 0, 2, 2),
                    cell(1, 1, 2, 2),
                ],
                rows: 2,
                cols: 2,
                score: 0.987,
                truncated: true,
            },
            TableGrid::default(),
        ];
        let wire = serde_json::to_string(&crate::worker::ipc::WorkerEvent::TableStructures(
            grids.clone(),
        ))
        .unwrap();
        assert!(wire.contains("colspan"), "{wire}");
        match serde_json::from_str::<crate::worker::ipc::WorkerEvent>(&wire).unwrap() {
            // One entry per crop, including the crop that produced no cell:
            // results are positional and a dropped empty would shift every grid
            // after it onto the wrong table.
            crate::worker::ipc::WorkerEvent::TableStructures(back) => assert_eq!(back, grids),
            other => panic!("expected TableStructures, got {other:?}"),
        }
    }

    /// The pixels the worker reads must be the pixels the host cut. A lossy
    /// staging would move a cell boundary without moving the recipe that claims
    /// to describe it.
    #[tokio::test]
    async fn crops_survive_staging_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let (manager, _events, _loop_fut) = crate::worker::manager::WorkerManager::new(
            crate::worker::manager::WorkerPaths {
                python_path: PathBuf::new(),
                python_package_dir: PathBuf::new(),
                requirements_path: PathBuf::new(),
                venv_dir: PathBuf::new(),
                worker_bin: PathBuf::new(),
                data_dir: PathBuf::new(),
            },
            crate::worker::ipc::WorkerKind::Recognize,
        );
        let reader = WorkerTableStructure {
            manager,
            tokio_handle: tokio::runtime::Handle::current(),
            engine: super::super::dispatch::RecognitionEngine::Onnx,
            model_id: MODEL_ID.to_string(),
            model_dir: PathBuf::new(),
            scratch_root: root.path().join("scratch"),
            identity: "id".to_string(),
        };
        let mut crop = image::RgbImage::new(5, 3);
        for (x, y, pixel) in crop.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x * 40) as u8, (y * 60) as u8, 7]);
        }
        let (staged, paths) = reader.stage(&[crop.clone()]).unwrap();
        assert!(paths[0].starts_with(root.path().join("scratch")));
        assert_eq!(
            super::super::worker_ocr::read_staged_image(&paths[0]).unwrap(),
            crop
        );
        let path = paths[0].clone();
        drop(staged);
        assert!(!path.exists(), "staged crops outlived the batch");
    }
}
