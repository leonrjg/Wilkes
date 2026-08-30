//! granite-docling under ONNX Runtime: the consumer-facing recognizer.
//!
//! 258M parameters against PaddleOCR-VL's 0.9B, and it reads a page in one
//! pass — prose, headings, LaTeX formulas, tables and figure regions all come
//! back as one DocTags stream with no layout model in front of it. That is the
//! reason it is the default: the routing problem that makes a prompt-switched
//! parser expensive does not arise when the model classifies its own output.
//!
//! Wilkes owns everything outside the weights, as it does for the other
//! recognizer: the tiling, the prompt, the decode loop's admission signal, and
//! the parse from DocTags into Wilkes' own regions. [`super::onnx_vlm`] owns
//! the graph mechanics and knows nothing about this model.
//!
//! ## Why fp32
//!
//! Measured on this repository's page fixture, all three published precisions
//! on the CPU execution provider:
//!
//! | set                    | on disk | result                                       |
//! | ---------------------- | ------- | -------------------------------------------- |
//! | fp32                   | 1263 MB | correct; full table; stops cleanly at 338 tok |
//! | int8 vision, fp32 rest |  958 MB | structure right, characters broken: "Exper t Sy s t e m s" |
//! | int8                   |  318 MB | drops words, never reaches the table, loops to the cap |
//! | fp16                   |  632 MB | degenerate `!!!!`, NaN log-probabilities      |
//!
//! The damage is attributable. The int8 **vision encoder** corrupts characters
//! — it inserts spaces inside words while keeping the layout — and the int8
//! **decoder** loops and omits whole elements. fp16 is broken outright: its
//! tensors are emulated on the CPU provider and the decode diverges.
//!
//! int8 is not a smaller version of this model, it is a worse one, and it did
//! not even buy time: it spent *more* wall-clock than fp32, because looping to
//! the token cap costs more than the faster steps save. So the small sets are
//! not shipped, and this recognizer's size argument is 1.26 GB against
//! PaddleOCR-VL's 1.9 GB — not the 318 MB the file listing advertises. What it
//! buys at that size is formulas, tables and figure regions in one pass.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::RgbImage;
use tokenizers::Tokenizer;

use super::ocr::{
    normalize_recognized_text, ImageRecognition, OcrEngine, RegionKind, SpottedRegion,
};
use super::onnx_vlm::{splice_image_features, OnnxVlm};
use crate::types::Point;

// ── The pinned recipe ────────────────────────────────────────────────────────

/// The side of one vision tile, and of the global thumbnail.
const TILE: usize = 512;
/// The longest edge an image is resized to before it is tiled.
const LONGEST_EDGE: usize = 2048;
/// Visual tokens per tile, after the pixel-shuffle projector.
const TOKENS_PER_TILE: usize = 64;
/// DocTags coordinates are `<loc_0>`..`<loc_500>` of the way across the image.
const LOC_MAX: f32 = 500.0;

/// The task prompt. Post-trained on this exact string.
const TASK_PROMPT: &str = "Convert this page to docling.";

/// A decode that has not stopped by here has stopped saying anything. Sized
/// past the longest good answer measured on the corpus, not past the longest
/// imaginable page: the point of a cap is to end a degenerate loop, and one
/// set high enough to never trip is not a cap.
const MAX_NEW_TOKENS: usize = 2048;

/// The confidence a region must reach to enter the reading.
///
/// Measured rather than inherited: PaddleOCR-VL's 0.70 is an operating point
/// for a different decoder over a different token distribution, and carrying
/// it across would be picking a number.
///
/// Two runs of the fixture page, clean and deliberately degraded:
///
/// | page     | region                        | score | correct |
/// | -------- | ----------------------------- | ----- | ------- |
/// | clean    | all seven, prose to table     | .85–.96 | yes   |
/// | degraded | "Expert Systems in Practice"  | 0.447 | yes     |
/// | degraded | "12. Knowledge representation"| 0.517 | **no** (reads 3.2) |
///
/// 0.65 sits in the empty band between 0.517 and 0.846. It rejects the
/// misread heading, and it also rejects the one correct low-confidence
/// reading — which is the trade this threshold is for. A wrong transcription
/// entering the canonical reading is worse than a right one staying out of
/// it, because the reading is what every consumer then quotes.
///
/// Two pages is a band, not a sweep. Widening this to the evaluation corpus
/// is the outstanding work; what is not outstanding is whether the signal
/// separates good readings from bad, which it plainly does.
pub const ADMISSION_THRESHOLD: f32 = 0.65;

/// Bumped when anything above changes for the same weights.
const EXTRACTION_SETTINGS_VERSION: &str = "doctags-v1";

pub const MODEL_ID: &str = "granite-docling-258M";
const REPO: &str = "onnx-community/granite-docling-258M-ONNX";
const REVISION: &str = "main";

const VISION_GRAPH: &str = "onnx/vision_encoder.onnx";
const EMBED_GRAPH: &str = "onnx/embed_tokens.onnx";
const DECODER_GRAPH: &str = "onnx/decoder_model_merged.onnx";

/// Every file the recognizer needs on disk, with the sidecars that carry the
/// weights. A graph without its `.onnx_data` loads and then fails at the first
/// run, so the sidecars are inventoried artifacts and not an implementation
/// detail of the download.
pub const ARTIFACTS: &[&str] = &[
    VISION_GRAPH,
    "onnx/vision_encoder.onnx_data",
    EMBED_GRAPH,
    "onnx/embed_tokens.onnx_data",
    DECODER_GRAPH,
    "onnx/decoder_model_merged.onnx_data",
    "tokenizer.json",
    "config.json",
];

/// What the recipe records about a reading this recognizer produced.
pub fn identity() -> String {
    format!(
        "ort-2.0.0-rc.13+{MODEL_ID}+{REPO}@{REVISION}+{EXTRACTION_SETTINGS_VERSION}\
         +tile-{TILE}x{LONGEST_EDGE}+admit-{ADMISSION_THRESHOLD}"
    )
}

pub fn footprint_bytes() -> u64 {
    // fp32 graphs plus tokenizer, as published.
    1_263_300_000
}

pub fn is_installed(model_dir: &Path) -> bool {
    let dir = install_dir(model_dir);
    ARTIFACTS.iter().all(|name| dir.join(name).is_file())
}

pub fn install_dir(model_dir: &Path) -> PathBuf {
    model_dir.join("recognizers").join(MODEL_ID)
}

/// What this recognizer is, where it came from, and under what terms.
///
/// Static: it describes the recipe rather than the machine, so it answers
/// before anything is installed. That is the point — the terms and the size
/// are disclosed where the download is offered, not after 1.26 GB arrives.
///
/// Sizes come from the published listing and digests are filled in from disk
/// after an install, because this export is served from a branch rather than
/// an immutable revision: pinning a digest Wilkes has not seen would be
/// recording a recollection, which is exactly what the inventory exists to
/// avoid.
pub fn inventory() -> crate::types::RecognizerInventory {
    crate::types::RecognizerInventory {
        name: MODEL_ID.to_string(),
        repo: REPO.to_string(),
        revision: REVISION.to_string(),
        license: "Apache-2.0".to_string(),
        license_url: "https://huggingface.co/ibm-granite/granite-docling-258M".to_string(),
        derived_from: vec![
            "granite-docling-258M (Apache-2.0, IBM)".to_string(),
            "siglip2-base-patch16-512 vision encoder (Apache-2.0, Google)".to_string(),
            "Idefics3 pixel-shuffle projector (Apache-2.0, HuggingFace)".to_string(),
        ],
        artifacts: ARTIFACTS
            .iter()
            .map(|filename| crate::types::InventoriedArtifact {
                filename: (*filename).to_string(),
                size_bytes: 0,
                sha256: String::new(),
            })
            .collect(),
        footprint_bytes: footprint_bytes(),
    }
}

/// Fetch the recognizer's artifacts into `model_dir`.
pub fn install(
    model_dir: &Path,
    progress: Option<crate::models::progress::ProgressTx>,
) -> Result<()> {
    use hf_hub::api::sync::ApiBuilder;

    let dir = install_dir(model_dir);
    std::fs::create_dir_all(dir.join("onnx"))
        .context("could not create the recognizer directory")?;

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
    for filename in ARTIFACTS {
        let target = dir.join(filename);
        if target.is_file() {
            continue;
        }
        let fetched = match reporter.clone() {
            Some(reporter) => api.download_with_progress(filename, reporter),
            None => api.download(filename),
        }
        .with_context(|| format!("could not download {filename} from {REPO}"))?;
        // The hub cache and the recognizer directory are two places; copying
        // rather than linking is what makes the install self-contained, which
        // is what `is_installed` is checking.
        std::fs::copy(&fetched, &target)
            .with_context(|| format!("could not place {filename} under {}", dir.display()))?;
    }

    anyhow::ensure!(
        is_installed(model_dir),
        "the {MODEL_ID} install finished without writing every file the inventory names"
    );
    Ok(())
}

// ── Preprocessing ────────────────────────────────────────────────────────────

/// The tile grid an image of this size is read at.
///
/// Two resizes, in the order the model was trained under. The first fits the
/// longest edge into [`LONGEST_EDGE`]; the second rounds *both* edges up to
/// whole tiles, because the split is an exact partition and a remainder strip
/// would be silently dropped rather than padded.
///
/// **Rounding both edges independently stretches the image, and that is
/// correct.** It reproduces `Idefics3ImageProcessor::resize_for_vision_encoder`
/// — the processor this checkpoint's `preprocessor_config.json` names — which
/// does exactly this and then resizes onto the result:
///
/// ```text
/// if width >= height:
///     width  = ceil(width / 512) * 512
///     height = int(width / aspect_ratio)
///     height = ceil(height / 512) * 512
/// ```
///
/// So a 1559x499 figure arrives at 2048x1024 here and in the reference alike,
/// and a portrait page is stretched about 30% wide by the same rule. Every
/// image the weights were trained on carried that distortion. Letterboxing the
/// image onto the grid instead — which reads like the obvious fix, and was
/// proposed once — would take the model off the distribution it learned, so it
/// is deliberately not done. See FIGURE.md, "Withdrawn".
///
/// Returned rather than applied so the cost is inspectable: a page at the
/// default bound is 16 tiles plus a thumbnail, and 17 x 64 = 1088 visual
/// tokens is the prefill that dominates a laptop's wall-clock.
pub fn tile_grid(width: u32, height: u32) -> (usize, usize, usize, usize) {
    let (w0, h0) = (width.max(1) as f64, height.max(1) as f64);
    let aspect = w0 / h0;

    // Fit the longest edge, keeping both edges even.
    let (mut w, mut h) = if w0 >= h0 {
        let w = LONGEST_EDGE as f64;
        let mut h = (w / aspect) as usize;
        if h % 2 != 0 {
            h += 1;
        }
        (LONGEST_EDGE, h)
    } else {
        let h = LONGEST_EDGE as f64;
        let mut w = (h * aspect) as usize;
        if w % 2 != 0 {
            w += 1;
        }
        (w, LONGEST_EDGE)
    };

    // Round up to whole tiles, driven by the longer edge as the model does.
    if w >= h {
        w = w.div_ceil(TILE) * TILE;
        h = ((w as f64 / aspect) as usize).div_ceil(TILE) * TILE;
    } else {
        h = h.div_ceil(TILE) * TILE;
        w = ((h as f64 * aspect) as usize).div_ceil(TILE) * TILE;
    }
    let w = w.max(TILE);
    let h = h.max(TILE);
    (w, h, w / TILE, h / TILE)
}

/// Tiles for one image, as the vision graph wants them: `[tiles, 3, 512, 512]`
/// flattened, row-major over the grid, with the global thumbnail last.
fn prepare_tiles(image: &RgbImage) -> (Vec<f32>, usize, usize, usize) {
    let (w, h, cols, rows) = tile_grid(image.width(), image.height());
    // Lanczos3 for the same reason the config names LANCZOS: downsampling a
    // page of small type with a cheaper filter loses the strokes that
    // distinguish characters.
    let resized = image::imageops::resize(image, w as u32, h as u32, FilterType::Lanczos3);

    let mut out = Vec::with_capacity((rows * cols + 1) * 3 * TILE * TILE);
    let mut push = |img: &RgbImage| {
        // Channel-major, and normalized to [-1, 1] by the config's mean and
        // standard deviation of 0.5.
        for channel in 0..3 {
            for y in 0..TILE {
                for x in 0..TILE {
                    let value = img.get_pixel(x as u32, y as u32)[channel] as f32 / 255.0;
                    out.push((value - 0.5) / 0.5);
                }
            }
        }
    };

    for row in 0..rows {
        for col in 0..cols {
            let tile = image::imageops::crop_imm(
                &resized,
                (col * TILE) as u32,
                (row * TILE) as u32,
                TILE as u32,
                TILE as u32,
            )
            .to_image();
            push(&tile);
        }
    }
    // The global view is the tiled image itself, not the original: the model
    // was trained on a thumbnail of what the tiles cover.
    let global = image::imageops::resize(&resized, TILE as u32, TILE as u32, FilterType::Lanczos3);
    push(&global);

    (out, rows * cols + 1, rows, cols)
}

/// The prompt for a `rows` x `cols` tiling, as a string.
///
/// Built as text and then tokenized, rather than assembled from token ids,
/// because the `<row_R_col_C>` markers are not numbered contiguously in this
/// vocabulary — they run 100258, 100259, 100261, ... and an id computed from
/// row and column would address the wrong marker.
fn prompt_text(rows: usize, cols: usize) -> String {
    let mut out = String::from("<|start_of_role|>user<|end_of_role|>");
    let images = "<image>".repeat(TOKENS_PER_TILE);
    for row in 1..=rows {
        for col in 1..=cols {
            out.push_str("<fake_token_around_image>");
            out.push_str(&format!("<row_{row}_col_{col}>"));
            out.push_str(&images);
        }
        out.push('\n');
    }
    out.push_str("\n<fake_token_around_image><global-img>");
    out.push_str(&images);
    out.push_str("<fake_token_around_image>");
    out.push_str(TASK_PROMPT);
    out.push_str("<|end_of_text|>\n<|start_of_role|>assistant<|end_of_role|>");
    out
}

// ── DocTags ──────────────────────────────────────────────────────────────────

/// One element of a DocTags stream, before it becomes a region.
#[derive(Debug, Clone, PartialEq)]
struct Element {
    kind: RegionKind,
    text: String,
    /// `x0, y0, x1, y1` in `<loc_>` units, if the element carried four.
    location: Option<[f32; 4]>,
    /// Character span in the decoded stream, used to score the element from
    /// the tokens that produced it.
    span: (usize, usize),
}

/// What this build does with a DocTags tag.
///
/// Three answers, not two. The distinction that matters is between a tag this
/// build *knows* carries no text and a tag it has never heard of: the first is
/// the recognizer working correctly and the second is a gap in this build's
/// coverage, and reporting them as one number means neither can be read.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Routing {
    /// Text to transcribe, of this kind.
    Read(RegionKind),
    /// A region the recognizer named and that carries no text to read. There
    /// is no [`RegionKind`] for it because there would be nothing to put in
    /// the reading — the element is a box around a thing seen, not a
    /// transcription of anything.
    NotText,
    /// A tag this build has no answer for at all.
    Unknown,
}

/// Which DocTags tag maps to which routing.
///
/// [`Routing::Unknown`] covers two different things and is told apart later by
/// whether the element carries a location: structure — `<doctag>`,
/// `<page_break>`, the classification markers inside a picture, the containers
/// whose contents are handled by their own tag — carries none and is passed
/// over, and located content carries one and is counted.
fn routing_of(tag: &str) -> Routing {
    match tag {
        "text" | "paragraph" | "caption" | "footnote" | "page_header" | "page_footer" | "title"
        | "list_item" => Routing::Read(RegionKind::Text),
        t if t.starts_with("section_header") => Routing::Read(RegionKind::Text),
        "formula" => Routing::Read(RegionKind::Formula),
        "otsl" => Routing::Read(RegionKind::Table),
        "chart" => Routing::Read(RegionKind::Chart),
        "code" => Routing::Read(RegionKind::Code),
        // A figure. Wilkes' answer to "what is in this picture" is the
        // describer's, written under its own label from a separate model; the
        // recognizer saying *that there is a picture here* adds a box and no
        // bytes. Contents the model marked out inside it — a caption, the
        // labels on a diagram — are their own elements and are read, because
        // parsing continues inside this element's body rather than skipping
        // past it.
        "picture" => Routing::NotText,
        _ => Routing::Unknown,
    }
}

/// The located regions a decode marked out that produced no element.
///
/// Two counts and not one, because they are two different facts. `not_text` is
/// the recognizer working: it named a picture, and a picture has no
/// transcription. `unroutable` is this build's coverage falling short: the
/// model named something and Wilkes has no answer for the tag at all.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Unread {
    not_text: u32,
    unroutable: u32,
}

/// Parse a DocTags stream into elements.
///
/// `<loc_n>` is not a token in this vocabulary — it is ordinary text that the
/// tokenizer splits into pieces — so locations are lexed from the decoded
/// string rather than matched by token id, which is how the other recognizer
/// reads its coordinates.
///
/// An element whose location is absent or is not four values is **dropped**,
/// never placed at a guessed position. Half a rectangle is not a location, and
/// the reading would carry text pointing at the wrong part of the page.
fn parse_doctags(stream: &str) -> (Vec<Element>, Unread) {
    let mut elements = Vec::new();
    let mut unread = Unread::default();
    let bytes: Vec<char> = stream.chars().collect();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != '<' {
            i += 1;
            continue;
        }
        let Some(open_end) = find(&bytes, i, '>') else {
            break;
        };
        let tag: String = bytes[i + 1..open_end].iter().collect();
        let routing = routing_of(&tag);
        let Routing::Read(kind) = routing else {
            // A tag that yields no text. Counted when it delimits content — a
            // located, closed element — and passed over when it is structure,
            // which is what `<doctag>` and the cell markers inside a table
            // are. A location is what tells them apart: DocTags gives every
            // content element one and gives structure none.
            //
            // Either way parsing continues *inside* the body rather than
            // skipping past the element, so anything the model marked out
            // within it is read on its own terms.
            if let Some(close_at) = find_str(&bytes, open_end + 1, &format!("</{tag}>")) {
                let body: String = bytes[open_end + 1..close_at].iter().collect();
                if split_location(&body).0.is_some() {
                    if routing == Routing::NotText {
                        // Expected, and frequent: every figure crop given to
                        // this model comes back as one of these. Debug, not a
                        // warning — a warning that fires once per figure
                        // teaches a reader to ignore warnings.
                        tracing::debug!(
                            "{MODEL_ID} marked out a <{tag}> region, which carries no \
                             text to read"
                        );
                        unread.not_text += 1;
                    } else {
                        tracing::warn!(
                            "{MODEL_ID} marked out a <{tag}> region this build has no kind \
                             for; it is counted and left out of the reading"
                        );
                        unread.unroutable += 1;
                    }
                }
            }
            i = open_end + 1;
            continue;
        };
        // Find this tag's close; an unterminated element is a truncated decode
        // and is dropped with everything after it.
        let close = format!("</{tag}>");
        let Some(close_at) = find_str(&bytes, open_end + 1, &close) else {
            break;
        };
        let body: String = bytes[open_end + 1..close_at].iter().collect();
        let (location, text) = split_location(&body);
        // A chart's contents arrive as cells, the same as a table's, and the
        // canonical form for both is a Markdown table. Whether the result is
        // *rectangular enough to be one* is the admission rule's question,
        // not this parser's.
        let text = if matches!(kind, RegionKind::Table | RegionKind::Chart) {
            otsl_to_markdown(&text)
        } else {
            normalize_recognized_text(&strip_tags(&text))
        };
        if !text.is_empty() {
            elements.push(Element {
                kind,
                text,
                location,
                span: (open_end + 1, close_at),
            });
        }
        i = close_at + close.chars().count();
    }
    (elements, unread)
}

fn find(haystack: &[char], from: usize, needle: char) -> Option<usize> {
    (from..haystack.len()).find(|i| haystack[*i] == needle)
}

fn find_str(haystack: &[char], from: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|i| haystack[*i..*i + needle.len()] == needle[..])
}

/// Take a leading run of `<loc_n>` values off an element body.
fn split_location(body: &str) -> (Option<[f32; 4]>, String) {
    let mut values = Vec::new();
    let mut rest = body;
    while let Some(stripped) = rest.strip_prefix("<loc_") {
        let Some(end) = stripped.find('>') else { break };
        let Ok(value) = stripped[..end].parse::<f32>() else {
            break;
        };
        values.push(value);
        rest = &stripped[end + 1..];
    }
    let location = if values.len() >= 4 {
        Some([values[0], values[1], values[2], values[3]])
    } else {
        None
    };
    (location, rest.to_string())
}

/// Remove any remaining DocTags markup from an element's text.
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for ch in text.chars() {
        match ch {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Convert an OTSL cell stream into a Markdown table.
///
/// Markdown because the design fixed one canonical table format, chosen for
/// consumers rather than for the model: OTSL is compact and neither an
/// embedder nor a reader can do anything with it.
///
/// The cells are converted as they were found; whether what comes out is a
/// well-formed table is decided once, by the admission rule in
/// [`super::ocr::markdown_table_is_rectangular`], so a ragged table is a
/// counted rejection rather than a silent nothing. This function's only claim
/// is that the engine's own format does not cross the extraction boundary.
fn otsl_to_markdown(body: &str) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut in_cell = false;
    let mut chars = body.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '<' {
            if in_cell {
                cell.push(ch);
            }
            continue;
        }
        let mut tag = String::new();
        for c in chars.by_ref() {
            if c == '>' {
                break;
            }
            tag.push(c);
        }
        match tag.as_str() {
            // A new cell closes the previous one.
            "fcel" | "ched" | "rhed" | "srow" => {
                if in_cell {
                    row.push(cell.trim().to_string());
                }
                cell.clear();
                in_cell = true;
            }
            "ecel" => {
                if in_cell {
                    row.push(cell.trim().to_string());
                }
                cell.clear();
                row.push(String::new());
                in_cell = false;
            }
            "nl" => {
                if in_cell {
                    row.push(cell.trim().to_string());
                    cell.clear();
                    in_cell = false;
                }
                if !row.is_empty() {
                    rows.push(std::mem::take(&mut row));
                }
            }
            // A caption inside the table belongs to the table, not to a cell.
            "caption" => break,
            _ => {}
        }
    }
    if in_cell {
        row.push(cell.trim().to_string());
    }
    if !row.is_empty() {
        rows.push(row);
    }

    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if rows.is_empty() || width == 0 {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(&format!("| {} |\n", rows[0].join(" | ")));
    out.push_str(&format!("|{}\n", " --- |".repeat(width)));
    for row in &rows[1..] {
        out.push_str(&format!("| {} |\n", row.join(" | ")));
    }
    out.trim_end().to_string()
}

// ── The engine ───────────────────────────────────────────────────────────────

pub struct GraniteDocling {
    model: Mutex<OnnxVlm>,
    tokenizer: Tokenizer,
    image_token_id: u32,
    eos_token_id: u32,
}

impl GraniteDocling {
    pub fn load(model_dir: &Path, threads: usize) -> Result<Self> {
        let dir = install_dir(model_dir);
        anyhow::ensure!(
            is_installed(model_dir),
            "the {MODEL_ID} recognizer is not installed under {}",
            dir.display()
        );

        let model = OnnxVlm::load(&dir, VISION_GRAPH, EMBED_GRAPH, DECODER_GRAPH, threads)?;
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| anyhow::anyhow!("could not read the tokenizer: {e}"))?;

        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)
                .context("could not read the model config")?;
        let id = |key: &str| -> Result<u32> {
            config[key]
                .as_u64()
                .with_context(|| format!("the config does not name {key}"))
                .map(|v| v as u32)
        };

        Ok(Self {
            model: Mutex::new(model),
            tokenizer,
            image_token_id: id("image_token_id")?,
            eos_token_id: id("eos_token_id")?,
        })
    }

    /// Read one image into elements.
    fn read(&self, image: &RgbImage) -> Result<ImageRecognition> {
        let (pixels, tiles, rows, cols) = prepare_tiles(image);
        let prompt = prompt_text(rows, cols);
        let encoded = self
            .tokenizer
            .encode(prompt, false)
            .map_err(|e| anyhow::anyhow!("could not encode the prompt: {e}"))?;
        let ids = encoded.get_ids();

        let slots: Vec<usize> = ids
            .iter()
            .enumerate()
            .filter(|(_, id)| **id == self.image_token_id)
            .map(|(i, _)| i)
            .collect();
        anyhow::ensure!(
            slots.len() == tiles * TOKENS_PER_TILE,
            "the prompt opened {} image slots for {tiles} tiles; the tiling and the \
             prompt disagree",
            slots.len()
        );

        let mut model = self
            .model
            .lock()
            .map_err(|_| anyhow::anyhow!("the recognizer mutex was poisoned"))?;

        let features = model.encode_image(&pixels, tiles, TILE)?;
        let mut embeds = model.embed_tokens(ids)?;
        let hidden = model.hidden_size();
        splice_image_features(&mut embeds, hidden, &slots, &features)?;

        let eos = self.eos_token_id;
        let tokens = model.decode(&embeds, MAX_NEW_TOKENS, |id| id == eos, |_| {})?;
        drop(model);

        let ids: Vec<u32> = tokens
            .iter()
            .filter(|t| t.id != eos)
            .map(|t| t.id)
            .collect();
        let logprobs: Vec<f32> = tokens
            .iter()
            .filter(|t| t.id != eos)
            .map(|t| t.logprob)
            .collect();

        if tokens.len() >= MAX_NEW_TOKENS {
            // Not an error: a truncated decode is a partial reading, and the
            // elements that did parse are still what the page says. Saying so
            // is the difference between a partial result and a silent one.
            tracing::warn!(
                "the {MODEL_ID} decode hit its {MAX_NEW_TOKENS}-token cap; this page's \
                 reading is partial"
            );
        }

        let stream = self
            .tokenizer
            .decode(&ids, false)
            .map_err(|e| anyhow::anyhow!("could not decode the response: {e}"))?;
        let char_ends = token_char_ends(&self.tokenizer, &ids)?;

        let (elements, unread) = parse_doctags(&stream);
        Ok(ImageRecognition {
            regions: elements
                .into_iter()
                .filter_map(|element| to_region(element, &char_ends, &logprobs))
                .collect(),
            unroutable: unread.unroutable,
            not_text: unread.not_text,
        })
    }
}

/// The end offset, in characters of the decoded stream, of each token.
///
/// Byte-level BPE does not decode a token in isolation the way it decodes it
/// in context, so the spans come from decoding growing prefixes rather than
/// from decoding each token alone.
///
/// Growing prefixes are quadratic in the length of the reading, and the
/// tokenizer's own `DecodeStream` is the linear answer to exactly this — but
/// the pinned tokenizers 0.20 `step_decode_stream` underflows its prefix index
/// and panics part-way through a page's worth of DocTags. Until that
/// dependency moves, the slower decode is the one that is correct; the cost is
/// bounded by the 2048-token cap and is small beside the decode that produced
/// the tokens.
fn token_char_ends(tokenizer: &Tokenizer, ids: &[u32]) -> Result<Vec<usize>> {
    let mut ends = Vec::with_capacity(ids.len());
    for upto in 1..=ids.len() {
        let text = tokenizer
            .decode(&ids[..upto], false)
            .map_err(|e| anyhow::anyhow!("could not decode a prefix: {e}"))?;
        ends.push(text.chars().count());
    }
    Ok(ends)
}

/// Turn a parsed element into a region, scored by the tokens that spell it.
fn to_region(element: Element, char_ends: &[usize], logprobs: &[f32]) -> Option<SpottedRegion> {
    let location = element.location?;
    let (from, to) = element.span;

    // Every token whose decoded text falls inside the element's span.
    let mut scored = Vec::new();
    let mut start = 0usize;
    for (index, end) in char_ends.iter().enumerate() {
        if start < to && *end > from {
            if let Some(logprob) = logprobs.get(index) {
                scored.push(*logprob);
            }
        }
        start = *end;
    }
    let confidence = if scored.is_empty() {
        0.0
    } else {
        (scored.iter().sum::<f32>() / scored.len() as f32).exp()
    };

    let at = |x: f32, y: f32| Point {
        x: (x / LOC_MAX).clamp(0.0, 1.0),
        y: (y / LOC_MAX).clamp(0.0, 1.0),
    };
    let [x0, y0, x1, y1] = location;
    Some(SpottedRegion {
        kind: element.kind,
        text: element.text,
        confidence: confidence.clamp(0.0, 1.0),
        // DocTags gives an axis-aligned box; Wilkes' geometry is a
        // quadrilateral, and the corners are that box's, in the same order
        // the other recognizer emits them.
        quad: [at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1)],
    })
}

impl OcrEngine for GraniteDocling {
    fn identity(&self) -> String {
        identity()
    }

    fn admission_threshold(&self) -> f32 {
        ADMISSION_THRESHOLD
    }

    fn spot_batch(&self, images: &[RgbImage]) -> Result<Vec<ImageRecognition>> {
        let mut out = Vec::with_capacity(images.len());
        for (nth, image) in images.iter().enumerate() {
            tracing::info!(
                "reading image {} of {} with {MODEL_ID}",
                nth + 1,
                images.len()
            );
            out.push(self.read(image)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture page is 1000x1300; the reference run tiled it 4x4 with a
    /// thumbnail, for 1088 visual tokens. If this changes, every cached
    /// reading was produced under a different prefill than the next one will
    /// be.
    #[test]
    fn a_portrait_page_tiles_the_way_the_reference_run_did() {
        let (w, h, cols, rows) = tile_grid(1000, 1300);
        assert_eq!((w, h), (2048, 2048));
        assert_eq!((cols, rows), (4, 4));
        assert_eq!((rows * cols + 1) * TOKENS_PER_TILE, 1088);
    }

    /// A small image is upscaled to the longest edge rather than tiled once:
    /// the model's own preprocessing always rescales to the bound, and reading
    /// a 40x30 crop at native size would give the encoder almost nothing to
    /// look at. The other recognizer upscales here too.
    #[test]
    fn a_small_image_is_upscaled_to_whole_tiles_rather_than_read_at_its_own_size() {
        let (w, h, cols, rows) = tile_grid(40, 30);
        assert_eq!((w, h), (2048, 1536));
        assert_eq!((cols, rows), (4, 3));
        assert_eq!(w % TILE, 0);
        assert_eq!(h % TILE, 0);
    }

    /// The prompt must open exactly one image slot per visual token, or the
    /// splice puts the picture in the wrong place and the decode runs to
    /// completion on a corrupted prefix.
    #[test]
    fn the_prompt_opens_one_image_slot_per_visual_token() {
        let text = prompt_text(4, 4);
        let slots = text.matches("<image>").count();
        assert_eq!(slots, 17 * TOKENS_PER_TILE);
        assert!(text.contains("<row_1_col_1>") && text.contains("<row_4_col_4>"));
        assert!(text.contains("<global-img>"));
        assert!(text.ends_with("<|start_of_role|>assistant<|end_of_role|>"));
    }

    #[test]
    fn locations_are_lexed_off_the_front_of_an_element() {
        let (location, rest) = split_location("<loc_39><loc_35><loc_270><loc_48>Expert Systems");
        assert_eq!(location, Some([39.0, 35.0, 270.0, 48.0]));
        assert_eq!(rest, "Expert Systems");
    }

    #[test]
    fn an_element_with_fewer_than_four_locations_has_none() {
        let (location, _) = split_location("<loc_39><loc_35>truncated");
        assert_eq!(location, None);
    }

    /// The real stream from the reference run, trimmed. Parsing this is the
    /// thing the Rust side has to agree with Python about.
    #[test]
    fn the_reference_stream_parses_into_the_elements_it_shows() {
        let stream = "<doctag><section_header_level_1><loc_40><loc_28><loc_270><loc_44>\
Expert Systems in Practice</section_header_level_1>\
<text><loc_39><loc_76><loc_368><loc_123>A non-expert communicates.</text>\
<formula><loc_95><loc_158><loc_268><loc_183>S ( q , d ) = \\Sigma w</formula>\
<otsl><loc_39><loc_218><loc_440><loc_287><ched>Corpus<ched>Recall<nl>\
<fcel>Reports<fcel>0.91<nl><fcel>Manuals<fcel>0.87<nl></otsl></doctag>";
        let (elements, unread) = parse_doctags(stream);
        assert_eq!(elements.len(), 4, "{elements:#?}");
        assert_eq!(unread, Unread::default());

        assert_eq!(elements[0].kind, RegionKind::Text);
        assert_eq!(elements[0].text, "Expert Systems in Practice");
        assert_eq!(elements[1].kind, RegionKind::Text);

        assert_eq!(elements[2].kind, RegionKind::Formula);
        assert_eq!(elements[2].text, "S ( q , d ) = \\Sigma w");
        assert_eq!(elements[2].location, Some([95.0, 158.0, 268.0, 183.0]));

        assert_eq!(elements[3].kind, RegionKind::Table);
        assert_eq!(
            elements[3].text,
            "| Corpus | Recall |\n| --- | --- |\n| Reports | 0.91 |\n| Manuals | 0.87 |"
        );
    }

    /// The conversion squares nothing off, and the rule that a ragged table
    /// is not a table lives in admission, where the rejection is counted.
    /// Converting and admitting are two jobs and one of them is not this
    /// module's.
    #[test]
    fn a_ragged_table_converts_and_is_refused_by_admission() {
        let ragged = otsl_to_markdown("<ched>A<ched>B<nl><fcel>1<nl>");
        assert!(!ragged.is_empty(), "the cells are still converted");
        assert!(!crate::extract::image::ocr::markdown_table_is_rectangular(
            &ragged
        ));
    }

    #[test]
    fn a_table_with_one_column_is_not_a_table() {
        let single = otsl_to_markdown("<ched>A<nl><fcel>1<nl>");
        assert!(!crate::extract::image::ocr::markdown_table_is_rectangular(
            &single
        ));
    }

    /// A chart's cells are the same cells, and reach the reading as the same
    /// Markdown. The engine's own format never crosses the boundary.
    #[test]
    fn a_chart_is_converted_to_a_markdown_table() {
        let stream = "<chart><loc_10><loc_10><loc_90><loc_90><ched>Year<ched>Share<nl>\
<fcel>2024<fcel>0.4<nl><fcel>2025<fcel>0.6<nl></chart>";
        let (elements, _) = parse_doctags(stream);
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].kind, RegionKind::Chart);
        assert_eq!(
            elements[0].text,
            "| Year | Share |\n| --- | --- |\n| 2024 | 0.4 |\n| 2025 | 0.6 |"
        );
        assert!(crate::extract::image::ocr::markdown_table_is_rectangular(
            &elements[0].text
        ));
    }

    /// A located, closed element whose tag this build has no kind for is
    /// counted rather than passed over, and structure — which carries no
    /// location — is not counted as content.
    #[test]
    fn a_region_of_an_unknown_kind_is_counted_rather_than_dropped() {
        let stream = "<doctag><text><loc_1><loc_2><loc_3><loc_4>read</text>\
<checkbox_selected><loc_5><loc_6><loc_7><loc_8>x</checkbox_selected></doctag>";
        let (elements, unread) = parse_doctags(stream);
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].text, "read");
        assert_eq!(unread.unroutable, 1);
        assert_eq!(unread.not_text, 0, "a checkbox is not a picture");
    }

    /// A picture is not an unroutable region. This model is a document parser
    /// and every figure crop it is handed comes back as one, so counting it
    /// with the tags this build genuinely cannot route would make that count
    /// say nothing: a hundred figures correctly named and a hundred regions
    /// of content lost would read the same.
    #[test]
    fn a_picture_is_counted_as_carrying_no_text_and_not_as_an_unknown_kind() {
        let stream = "<doctag><picture><loc_0><loc_0><loc_500><loc_500></picture></doctag>";
        let (elements, unread) = parse_doctags(stream);
        assert!(elements.is_empty(), "{elements:#?}");
        assert_eq!(unread.not_text, 1);
        assert_eq!(
            unread.unroutable, 0,
            "the recognizer is not falling short here"
        );
    }

    /// What the model marks out *inside* a picture is read on its own terms:
    /// the parser continues into the body rather than skipping past the
    /// element, so a diagram's caption and its labels still reach the reading.
    /// The picture itself contributes the box and no bytes.
    #[test]
    fn the_contents_of_a_picture_are_still_read() {
        let stream = "<doctag><picture><loc_0><loc_0><loc_500><loc_500>\
<caption><loc_10><loc_10><loc_90><loc_20>Figure 3: components</caption>\
<text><loc_20><loc_30><loc_80><loc_40>Knowledge base</text>\
</picture></doctag>";
        let (elements, unread) = parse_doctags(stream);
        let read: Vec<&str> = elements.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(read, vec!["Figure 3: components", "Knowledge base"]);
        assert_eq!(unread.not_text, 1);
        assert_eq!(unread.unroutable, 0);
    }

    /// A picture with no location is structure, not a region, and is passed
    /// over exactly as an unknown structural tag is.
    #[test]
    fn a_picture_without_a_location_is_not_counted() {
        let (elements, unread) = parse_doctags("<doctag><picture></picture></doctag>");
        assert!(elements.is_empty());
        assert_eq!(unread, Unread::default());
    }

    #[test]
    fn an_unterminated_element_is_dropped_with_what_follows_it() {
        let truncated = "<text><loc_1><loc_2><loc_3><loc_4>kept</text><text><loc_5>cut off";
        let (elements, _) = parse_doctags(truncated);
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].text, "kept");
    }

    /// A region with no location is not placed at a guessed one.
    #[test]
    fn an_element_without_a_location_produces_no_region() {
        let element = Element {
            kind: RegionKind::Text,
            text: "somewhere".to_string(),
            location: None,
            span: (0, 9),
        };
        assert!(to_region(element, &[9], &[-0.1]).is_none());
    }

    #[test]
    fn a_regions_quad_is_its_box_in_fractions_of_the_image() {
        let element = Element {
            kind: RegionKind::Formula,
            text: "x^2".to_string(),
            location: Some([0.0, 0.0, 250.0, 500.0]),
            span: (0, 3),
        };
        let region = to_region(element, &[3], &[0.0]).expect("a located element is a region");
        assert_eq!(region.quad[0], Point { x: 0.0, y: 0.0 });
        assert_eq!(region.quad[2], Point { x: 0.5, y: 1.0 });
        assert_eq!(region.kind, RegionKind::Formula);
        assert!((region.confidence - 1.0).abs() < 1e-6);
    }

    /// Read a real page with the real weights.
    ///
    /// Ignored because it needs 1.26 GB installed and takes half a minute.
    /// `WILKES_GRANITE_MODEL_DIR` is the installation root and
    /// `WILKES_GRANITE_PAGE` a page image; both must be given.
    ///
    /// What it pins is agreement with the reference implementation this port
    /// was written against — same tiling, same prompt, same elements. A port
    /// that merely runs is not a port that reads the same document.
    #[test]
    #[ignore = "needs the installed recognizer and a page image; ~30s"]
    fn a_real_page_reads_into_the_elements_the_reference_run_produced() {
        let Ok(model_dir) = std::env::var("WILKES_GRANITE_MODEL_DIR") else {
            panic!("set WILKES_GRANITE_MODEL_DIR");
        };
        let Ok(page) = std::env::var("WILKES_GRANITE_PAGE") else {
            panic!("set WILKES_GRANITE_PAGE");
        };
        let image = image::open(&page).expect("the page image opens").to_rgb8();
        let engine = GraniteDocling::load(Path::new(&model_dir), 4).expect("the recognizer loads");

        let started = std::time::Instant::now();
        let regions = engine
            .spot_batch(std::slice::from_ref(&image))
            .expect("the page is read")
            .remove(0)
            .regions;
        eprintln!("read {} regions in {:?}", regions.len(), started.elapsed());
        for region in &regions {
            eprintln!(
                "  {:?} {:.3} {:?} {}",
                region.kind,
                region.confidence,
                region.quad[0],
                region.text.replace('\n', " / ")
            );
        }

        assert!(regions.len() >= 5, "expected a page's worth of elements");
        assert!(
            regions
                .iter()
                .any(|r| r.kind == RegionKind::Formula && r.text.contains("\\Sigma")),
            "the reference run read the equation as LaTeX"
        );
        let table = regions
            .iter()
            .find(|r| r.kind == RegionKind::Table)
            .expect("the reference run read the table");
        assert!(table.text.starts_with("| Corpus |"), "{}", table.text);
        assert!(table.text.contains("| --- |"), "{}", table.text);
        assert!(table.text.contains("Letters"), "{}", table.text);
        assert!(
            regions
                .iter()
                .any(|r| r.text.contains("Expert Systems in Practice")),
            "the heading is read verbatim"
        );
        // Every region carries a usable admission signal and a real box.
        for region in &regions {
            assert!(
                region.confidence > 0.0 && region.confidence <= 1.0,
                "{:?} scored {}",
                region.kind,
                region.confidence
            );
            assert!(region.quad[2].x > region.quad[0].x, "empty box");
            assert!(region.quad[2].y > region.quad[0].y, "empty box");
        }
    }

    /// The identity names everything that changes the bytes. A reading made
    /// under a different tiling, prompt or threshold is a different reading.
    #[test]
    fn the_identity_names_the_model_the_settings_and_the_threshold() {
        let id = identity();
        assert!(id.contains(MODEL_ID), "{id}");
        assert!(id.contains(EXTRACTION_SETTINGS_VERSION), "{id}");
        assert!(id.contains(&ADMISSION_THRESHOLD.to_string()), "{id}");
        assert!(id.contains("rc.13"), "{id}");
    }
}
