//! Where the seconds of a page go, and what moves them.
//!
//! `formula_recall` answers what the detector finds. This answers what it
//! costs, stage by stage, and then measures the levers one at a time against
//! the same pages so a claim about a speed-up is a difference and not a
//! recollection.
//!
//!     cargo run --release --example perf_profile -- <labels.json> <mode> [flags]
//!
//! Modes:
//!   `stages`       one full extraction of the selected pages, timed per stage
//!   `threads`      the detector and one Texify crop at 2..10 intra-op threads
//!   `batch`        Texify at batch 1, 4, 8, 16 over the same crops
//!   `concurrency`  N page pipelines in flight at once, threads split N ways
//!   `coreml`       the same two graphs under the CoreML execution provider
//!   `graphs`       what the three graphs declare as inputs and outputs
//!   `equivalence`  the cached decoder against the uncached one, crop by crop
//!   `gate`         what the whole-page pass scores on every fixture page,
//!                  which is the number the tile gate is set from
//!   `readers`      N Texify readers x M threads over one document's crops
//!
//! Flags: `--threads N` `--batch N` `--lanes N` `--tiles on|off` `--pages N`
//!        `--crops N` `--cap N` `--texify-threads N` `--decoder cached|uncached`
//!        `--sweep 4,8`
//!
//! Loads its models in this process, exactly as `formula_recall` does, and for
//! the same reason: a probe *is* the model's process and Ctrl-C is the kill.
//! Not precedent for anything under `src/` — see the invariant in `AGENTS.md`.
//!
//! Every number it prints is a wall time beside the CPU time the process
//! actually burned over the same interval, from `getrusage(RUSAGE_SELF)`.
//! Their ratio is the effective cores: 1.0 means one core's worth of work
//! happened, 6.0 means six did. It is the process's own CPU, so another
//! program loading the machine shows up as a *worse* wall time at the same
//! effective cores, which is how contention is told apart from a stage that is
//! simply serial.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use image::RgbImage;
use mupdf::text_page::TextBlockType;
use mupdf::{Document, TextPageFlags};
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::{DynValue, Tensor};
use tokenizers::Tokenizer;

use wilkes_core::extract::image::doclayout::{self, DocLayout, Pass, Recipe};
use wilkes_core::extract::image::texify;
use wilkes_core::extract::pdf::typeset::{self, PageSurvey, WordBox};
use wilkes_core::types::{BoundingBox, RegionKind};

// ---------------------------------------------------------------------------
// The fixture, and which of its pages this probe runs on
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Clone)]
struct Label {
    #[allow(dead_code)]
    kind: String,
}

#[derive(serde::Deserialize, Clone)]
struct Entry {
    pdf: PathBuf,
    /// 0-based, as the fixture records it and as MuPDF's `load_page` wants it.
    page: i32,
    page_width: f32,
    page_height: f32,
    boxes: Vec<Label>,
}

fn tag(entry: &Entry) -> String {
    let stem = entry
        .pdf
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "?".to_string());
    let short: String = stem.split_whitespace().next().unwrap_or("?").to_string();
    format!("{short}#{}", entry.page)
}

/// The pages the whole report is about: the `math` most heavily labelled, and
/// the `prose` pages the fixture holds no label for at all.
///
/// Chosen from the fixture by its own label counts rather than named here, so
/// the selection cannot drift from the file it claims to come from. Sorted by
/// count and then by name, because a tie broken by hash order would make two
/// runs of this probe two different measurements.
fn select(entries: &[Entry], math: usize, prose: usize) -> (Vec<Entry>, Vec<Entry>) {
    let mut heavy: Vec<Entry> = entries
        .iter()
        .filter(|e| !e.boxes.is_empty())
        .cloned()
        .collect();
    heavy.sort_by(|a, b| b.boxes.len().cmp(&a.boxes.len()).then(tag(a).cmp(&tag(b))));
    heavy.truncate(math);
    let mut light: Vec<Entry> = entries
        .iter()
        .filter(|e| e.boxes.is_empty())
        .cloned()
        .collect();
    light.sort_by_key(tag);
    light.truncate(prose);
    (heavy, light)
}

// ---------------------------------------------------------------------------
// The clock
// ---------------------------------------------------------------------------

/// User + system CPU seconds this process has burned so far.
fn cpu_seconds() -> f64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // Safe: `usage` is a live, correctly sized `rusage` and the call only
    // writes into it.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let of = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 * 1e-6;
    of(usage.ru_utime) + of(usage.ru_stime)
}

/// Peak resident set, in bytes. macOS reports `ru_maxrss` in bytes; Linux
/// reports kilobytes. Only the former is ever run here, and the unit is named
/// rather than guessed at because a table in the wrong one is a wrong table.
fn peak_rss_bytes() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    if cfg!(target_os = "macos") {
        usage.ru_maxrss as u64
    } else {
        usage.ru_maxrss as u64 * 1024
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// One stage's total wall and CPU over however many times it ran.
#[derive(Default, Clone, Copy)]
struct Cost {
    wall: Duration,
    cpu: f64,
    runs: u32,
}

impl Cost {
    fn add(&mut self, wall: Duration, cpu: f64) {
        self.wall += wall;
        self.cpu += cpu;
        self.runs += 1;
    }

    fn merge(&mut self, other: &Cost) {
        self.wall += other.wall;
        self.cpu += other.cpu;
        self.runs += other.runs;
    }

    fn ms(&self) -> f64 {
        self.wall.as_secs_f64() * 1000.0
    }

    /// CPU seconds per wall second: how many cores this stage actually used.
    fn cores(&self) -> f64 {
        let wall = self.wall.as_secs_f64();
        if wall > 0.0 {
            self.cpu / wall
        } else {
            0.0
        }
    }
}

/// A named list of stages, in the order they were first seen.
#[derive(Default, Clone)]
struct Ledger {
    order: Vec<&'static str>,
    costs: Vec<Cost>,
}

impl Ledger {
    fn at(&mut self, name: &'static str) -> &mut Cost {
        if let Some(index) = self.order.iter().position(|n| *n == name) {
            return &mut self.costs[index];
        }
        self.order.push(name);
        self.costs.push(Cost::default());
        self.costs.last_mut().expect("just pushed")
    }

    /// Run `body`, charge what it cost to `name`, hand back what it returned.
    fn time<T>(&mut self, name: &'static str, body: impl FnOnce() -> T) -> T {
        let (cpu0, wall0) = (cpu_seconds(), Instant::now());
        let out = body();
        let (wall, cpu) = (wall0.elapsed(), cpu_seconds() - cpu0);
        self.at(name).add(wall, cpu);
        out
    }

    fn merge(&mut self, other: &Ledger) {
        for (name, cost) in other.order.iter().zip(&other.costs) {
            self.at(name).merge(cost);
        }
    }

    fn total(&self) -> Cost {
        let mut all = Cost::default();
        for cost in &self.costs {
            all.wall += cost.wall;
            all.cpu += cost.cpu;
        }
        all
    }

    fn print(&self, pages: usize, title: &str) {
        let total = self.total();
        println!("\n── {title} · {pages} page(s) ──");
        println!(
            "{:<28} {:>9} {:>10} {:>8} {:>7} {:>7}",
            "stage", "runs", "ms total", "ms/page", "cores", "share"
        );
        for (name, cost) in self.order.iter().zip(&self.costs) {
            println!(
                "{:<28} {:>9} {:>10.0} {:>8.1} {:>7.2} {:>6.1}%",
                name,
                cost.runs,
                cost.ms(),
                cost.ms() / pages as f64,
                cost.cores(),
                100.0 * cost.ms() / total.ms().max(1e-9),
            );
        }
        println!(
            "{:<28} {:>9} {:>10.0} {:>8.1} {:>7.2}",
            "TOTAL",
            "",
            total.ms(),
            total.ms() / pages as f64,
            total.cores(),
        );
    }
}

// ---------------------------------------------------------------------------
// The page's own words, as `typeset::regions` wants them
// ---------------------------------------------------------------------------
//
// Character for character the survey `formula_recall::page_words` builds, and
// for the same reason: the regions this probe times are the regions extraction
// would render, and a survey assembled differently would be a different set of
// crops.

fn flush_word(
    out: &mut Vec<WordBox>,
    block: usize,
    line: usize,
    word: &mut usize,
    chars: &mut usize,
    bbox: &mut Option<BoundingBox>,
) {
    if *chars == 0 {
        *bbox = None;
        return;
    }
    if let Some(bbox) = bbox.take() {
        out.push(WordBox {
            block,
            line,
            word: *word,
            bbox,
        });
    }
    *chars = 0;
    *word += 1;
}

fn page_words(text_page: &mupdf::TextPage) -> PageSurvey {
    let mut out = Vec::new();
    let mut drawn = Vec::new();
    for (block_index, block) in text_page.blocks().enumerate() {
        if block.r#type() == TextBlockType::Image {
            let bounds = block.bounds();
            drawn.push(BoundingBox {
                x: bounds.x0,
                y: bounds.y0,
                width: (bounds.x1 - bounds.x0).max(0.0),
                height: (bounds.y1 - bounds.y0).max(0.0),
            });
            continue;
        }
        for (line_index, line) in block.lines().enumerate() {
            let mut word = 0usize;
            let mut chars = 0usize;
            let mut bbox: Option<BoundingBox> = None;
            for ch in line.chars() {
                let Some(c) = ch.char() else { continue };
                if c.is_whitespace() {
                    flush_word(
                        &mut out,
                        block_index,
                        line_index,
                        &mut word,
                        &mut chars,
                        &mut bbox,
                    );
                    continue;
                }
                chars += 1;
                let q = ch.quad();
                let x0 = q.ul.x.min(q.ll.x);
                let y0 = q.ul.y.min(q.ur.y);
                let x1 = q.ur.x.max(q.lr.x);
                let y1 = q.ll.y.max(q.lr.y);
                if x1 > x0 && y1 > y0 {
                    let next = BoundingBox {
                        x: x0,
                        y: y0,
                        width: x1 - x0,
                        height: y1 - y0,
                    };
                    bbox = Some(match &bbox {
                        Some(existing) => existing.merge(&next),
                        None => next,
                    });
                }
            }
            flush_word(
                &mut out,
                block_index,
                line_index,
                &mut word,
                &mut chars,
                &mut bbox,
            );
        }
    }
    PageSurvey { words: out, drawn }
}

// ---------------------------------------------------------------------------
// The detector, as this probe drives it
// ---------------------------------------------------------------------------

fn model_dir() -> PathBuf {
    std::env::var("PROBE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").expect("a home directory");
            PathBuf::from(home).join("Library/Application Support/app.wilkes/models")
        })
}

fn layout_graph() -> PathBuf {
    doclayout::install_dir(&model_dir()).join("PP-DocLayoutV2.onnx")
}

/// A session over one graph, with the threads and provider named.
///
/// `coreml` puts the CoreML provider ahead of the CPU one, which is what
/// `embed::engines::fastembed` does for the embedder. Nothing is forced: ONNX
/// Runtime assigns whatever nodes CoreML claims and leaves the rest on CPU, and
/// this probe reports which by reading the log.
fn session(path: &Path, threads: usize, coreml: bool) -> anyhow::Result<Session> {
    let mut builder = Session::builder()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_intra_threads(threads)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if coreml {
        #[cfg(feature = "fastembed-coreml")]
        {
            builder = builder
                .with_execution_providers([ort::ep::CoreML::default().build()])
                .map_err(|e| anyhow::anyhow!("the CoreML provider was refused: {e}"))?;
        }
        #[cfg(not(feature = "fastembed-coreml"))]
        anyhow::bail!(
            "this build has no CoreML provider: rebuild with --features fastembed-coreml, \
             which is what the desktop sidecar is built with"
        );
    }
    builder
        .commit_from_file(path)
        .map_err(|e| anyhow::anyhow!("could not load {}: {e}", path.display()))
}

/// One forward pass of the detector over one 800 px square, through a session
/// this probe owns.
///
/// The tensor is [`doclayout::to_tensor`] and the feed is keyed by the graph's
/// own input names, which is what `DocLayout::run` does; only the session is
/// this probe's, because a session built with a provider is the whole point of
/// the CoreML lever.
fn detect_window(session: &mut Session, input: &RgbImage) -> anyhow::Result<Vec<f32>> {
    let side = DocLayout::graph_side();
    let image = doclayout::to_tensor(input)?;
    let unit = Tensor::from_array((vec![1i64, 2], vec![1.0f32, 1.0]))?;
    let shape = Tensor::from_array((vec![1i64, 2], vec![side as f32, side as f32]))?;
    let feed: Vec<(String, SessionInputValue)> = session
        .inputs()
        .iter()
        .map(|input| input.name().to_string())
        .map(|name| -> anyhow::Result<(String, SessionInputValue)> {
            let value: SessionInputValue = match name.as_str() {
                doclayout::INPUT_IMAGE => image.clone().into(),
                doclayout::INPUT_SCALE => unit.clone().into(),
                doclayout::INPUT_SHAPE => shape.clone().into(),
                other => anyhow::bail!("the detector declares an unknown input {other}"),
            };
            Ok((name, value))
        })
        .collect::<anyhow::Result<_>>()?;
    let outputs = session
        .run(feed)
        .map_err(|e| anyhow::anyhow!("the layout detector failed: {e}"))?;
    let (_, rows) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(rows.to_vec())
}

// ---------------------------------------------------------------------------
// Texify, split into its two halves
// ---------------------------------------------------------------------------

/// What one decode cost, and how long the answer was.
#[derive(Default, Clone, Copy)]
struct Read {
    preprocess: Duration,
    encoder: Duration,
    decoder: Duration,
    tokens: usize,
    steps: usize,
}

struct Recognizer {
    encoder: Session,
    decoder: Session,
    /// The cached decoder, and what the two graphs declared between them.
    /// `None` is the *old* loop, kept in this probe and nowhere else: it is
    /// the baseline every table below is a difference against, and a baseline
    /// recalled rather than run is not one.
    with_past: Option<(Session, texify::CacheShape)>,
    tokenizer: Tokenizer,
}

impl Recognizer {
    fn load(threads: usize, coreml: bool, cached: bool) -> anyhow::Result<Self> {
        let dir = texify::install_dir(&model_dir());
        let decoder = session(&dir.join(texify::DECODER_GRAPH), threads, coreml)?;
        let with_past = if cached {
            let graph = session(&dir.join(texify::DECODER_WITH_PAST_GRAPH), threads, coreml)?;
            let shape = texify::CacheShape::discover(&decoder, &graph)?;
            Some((graph, shape))
        } else {
            None
        };
        Ok(Self {
            encoder: session(&dir.join(texify::ENCODER_GRAPH), threads, coreml)?,
            decoder,
            with_past,
            tokenizer: Tokenizer::from_file(dir.join("tokenizer.json"))
                .map_err(|e| anyhow::anyhow!("could not read the tokenizer: {e}"))?,
        })
    }

    /// Read one crop through the cached decoder pair, exactly as
    /// `texify::Texify::read` does: step 0 through the graph that takes no
    /// past, every later step through the one that takes it, the
    /// cross-attention cache handed back unchanged the whole way.
    ///
    /// Batch 1 only. The cached graph declares an `input_ids` of exactly one
    /// column, and a batch whose sequences end at different steps would need
    /// its finished rows kept running — which is a different measurement, not
    /// this one.
    fn read_cached(&mut self, crop: &RgbImage, cap: usize) -> anyhow::Result<(String, f32, Read)> {
        let (with_past, shape) = self
            .with_past
            .as_mut()
            .context("this recognizer was loaded without the cached decoder")?;
        let mut cost = Read::default();

        let started = Instant::now();
        let side = texify::SIDE;
        let planes = texify::preprocess(crop);
        cost.preprocess = started.elapsed();

        let started = Instant::now();
        let pixels = Tensor::from_array((vec![1i64, 3, side as i64, side as i64], planes))?;
        let encoded = self
            .encoder
            .run(vec![(
                "pixel_values".to_string(),
                SessionInputValue::from(pixels),
            )])
            .map_err(|e| anyhow::anyhow!("the vision encoder failed: {e}"))?;
        let (hidden_shape, hidden) = encoded[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let hidden_shape: Vec<i64> = hidden_shape.to_vec();
        let hidden: Vec<f32> = hidden.to_vec();
        drop(encoded);
        cost.encoder = started.elapsed();

        let started = Instant::now();
        let mut ids: Vec<i64> = vec![texify::START_TOKEN];
        let mut confidence = 0f64;
        let mut self_cache: Vec<DynValue> = Vec::new();
        let mut cross_cache: Vec<DynValue> = Vec::new();
        let mut feed_token = texify::START_TOKEN;
        let mut steps = 0usize;
        for step in 0..cap {
            steps += 1;
            let input = Tensor::from_array((vec![1i64, 1], vec![feed_token]))?;
            let (next, probability) = if step == 0 {
                let states = Tensor::from_array((hidden_shape.clone(), hidden.clone()))?;
                let mut out = self
                    .decoder
                    .run(vec![
                        ("input_ids".to_string(), SessionInputValue::from(input)),
                        (
                            "encoder_hidden_states".to_string(),
                            SessionInputValue::from(states),
                        ),
                    ])
                    .map_err(|e| anyhow::anyhow!("the decoder failed: {e}"))?;
                let chosen = last_row(&out)?;
                for (_, present) in &shape.self_attention {
                    self_cache.push(out.remove(present.as_str()).expect("declared present"));
                }
                for (_, present) in &shape.cross_attention {
                    cross_cache.push(out.remove(present.as_str()).expect("declared present"));
                }
                chosen
            } else {
                let mut feed: Vec<(String, SessionInputValue)> = Vec::new();
                feed.push(("input_ids".to_string(), SessionInputValue::from(input)));
                for ((past, _), value) in shape.self_attention.iter().zip(self_cache.drain(..)) {
                    feed.push((past.clone(), SessionInputValue::from(value)));
                }
                for ((past, _), value) in shape.cross_attention.iter().zip(&cross_cache) {
                    feed.push((past.clone(), SessionInputValue::from(value)));
                }
                let mut out = with_past
                    .run(feed)
                    .map_err(|e| anyhow::anyhow!("the cached decoder failed: {e}"))?;
                let chosen = last_row(&out)?;
                for (_, present) in &shape.self_attention {
                    self_cache.push(out.remove(present.as_str()).expect("declared present"));
                }
                chosen
            };
            confidence += f64::from(probability);
            if next == texify::EOS_TOKEN {
                break;
            }
            ids.push(next);
            feed_token = next;
        }
        cost.decoder = started.elapsed();
        cost.steps = steps;
        cost.tokens = ids.len() - 1;

        let text = self
            .tokenizer
            .decode(
                &ids[1..].iter().map(|id| *id as u32).collect::<Vec<u32>>(),
                true,
            )
            .map_err(|e| anyhow::anyhow!("could not detokenize: {e}"))?;
        Ok((
            texify::unwrap_delimiters(&text).to_string(),
            (confidence / ids.len().max(1) as f64) as f32,
            cost,
        ))
    }

    /// Read `crops` as one batch: one encoder call over an `N`-deep tensor,
    /// then a greedy decode that steps every sequence together.
    ///
    /// The decoder graph takes no attention mask, and does not need one to be
    /// batched. It is causal: the logits at a sequence's own last real position
    /// attend only to positions before it, so a shorter sequence right-padded
    /// out to the batch's length reads back exactly the logits it would have
    /// read alone. What batching costs is the padding — a batch runs for as
    /// many steps as its longest answer — and that is the number this measures.
    ///
    /// `batch == 1` is the *old* production path: one encoder call and one
    /// decode per crop, and the loop below degenerates to it exactly.
    fn read_uncached(
        &mut self,
        crops: &[RgbImage],
        cap: usize,
    ) -> anyhow::Result<(Vec<String>, Vec<f32>, Read)> {
        let mut cost = Read::default();
        let n = crops.len();
        anyhow::ensure!(n > 0, "a batch of no crops");

        let started = Instant::now();
        let side = texify::SIDE;
        let mut planes = Vec::with_capacity(n * 3 * side * side);
        for crop in crops {
            planes.extend_from_slice(&texify::preprocess(crop));
        }
        cost.preprocess = started.elapsed();

        let started = Instant::now();
        let pixels = Tensor::from_array((vec![n as i64, 3, side as i64, side as i64], planes))?;
        let encoded = self
            .encoder
            .run(vec![(
                "pixel_values".to_string(),
                SessionInputValue::from(pixels),
            )])
            .map_err(|e| anyhow::anyhow!("the vision encoder failed: {e}"))?;
        let (shape, hidden) = encoded[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let shape: Vec<i64> = shape.to_vec();
        let hidden: Vec<f32> = hidden.to_vec();
        drop(encoded);
        cost.encoder = started.elapsed();
        anyhow::ensure!(
            shape[0] == n as i64,
            "the encoder answered for {} of {n} crop(s) — its batch dimension is fixed",
            shape[0]
        );

        let started = Instant::now();
        let mut ids: Vec<Vec<i64>> = vec![vec![texify::START_TOKEN]; n];
        let mut live: Vec<bool> = vec![true; n];
        let mut confidence = vec![0f64; n];
        let mut steps = 0usize;
        while live.iter().any(|l| *l) && steps < cap {
            steps += 1;
            let width = ids.iter().map(Vec::len).max().expect("n > 0");
            // Right-padded with EOS. A pad only ever sits *after* the position
            // whose logits are read, and a causal decoder cannot see it.
            let mut flat = Vec::with_capacity(n * width);
            for row in &ids {
                flat.extend_from_slice(row);
                flat.extend(std::iter::repeat(texify::EOS_TOKEN).take(width - row.len()));
            }
            let input = Tensor::from_array((vec![n as i64, width as i64], flat))?;
            let states = Tensor::from_array((shape.clone(), hidden.clone()))?;
            let out = self
                .decoder
                .run(vec![
                    ("input_ids".to_string(), SessionInputValue::from(input)),
                    (
                        "encoder_hidden_states".to_string(),
                        SessionInputValue::from(states),
                    ),
                ])
                .map_err(|e| anyhow::anyhow!("the decoder failed: {e}"))?;
            let (logit_shape, logits) = out[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let vocabulary = *logit_shape.last().unwrap_or(&0) as usize;
            anyhow::ensure!(vocabulary > 0, "the decoder returned an empty vocabulary");
            for index in 0..n {
                if !live[index] {
                    continue;
                }
                let at = ids[index].len() - 1;
                let base = (index * width + at) * vocabulary;
                let (next, probability) = argmax(&logits[base..base + vocabulary]);
                confidence[index] += f64::from(probability);
                if next == texify::EOS_TOKEN {
                    live[index] = false;
                    continue;
                }
                ids[index].push(next);
            }
            drop(out);
        }
        cost.decoder = started.elapsed();
        cost.steps = steps;
        cost.tokens = ids.iter().map(|row| row.len() - 1).sum();

        let mut answers = Vec::with_capacity(n);
        for row in &ids {
            let text = self
                .tokenizer
                .decode(
                    &row[1..].iter().map(|id| *id as u32).collect::<Vec<u32>>(),
                    true,
                )
                .map_err(|e| anyhow::anyhow!("could not detokenize: {e}"))?;
            answers.push(texify::unwrap_delimiters(&text).to_string());
        }
        let confidences = ids
            .iter()
            .zip(&confidence)
            .map(|(row, total)| (total / row.len().max(1) as f64) as f32)
            .collect();
        Ok((answers, confidences, cost))
    }

    /// Read a chunk through whichever decoder this recognizer was loaded with.
    ///
    /// One entry point, so every mode below measures the path it was told to
    /// and not a third one assembled here.
    fn read(
        &mut self,
        crops: &[RgbImage],
        cap: usize,
    ) -> anyhow::Result<(Vec<String>, Vec<f32>, Read)> {
        if self.with_past.is_some() {
            anyhow::ensure!(
                crops.len() == 1,
                "the cached decoder takes one token of one sequence a step; batch it with \
                 --decoder uncached"
            );
            let (text, confidence, cost) = self.read_cached(&crops[0], cap)?;
            return Ok((vec![text], vec![confidence], cost));
        }
        self.read_uncached(crops, cap)
    }
}

/// The last position's logits off a decoder's `logits` output, greedily.
fn last_row(outputs: &SessionOutputs<'_>) -> anyhow::Result<(i64, f32)> {
    let (logit_shape, logits) = outputs["logits"]
        .try_extract_tensor::<f32>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let vocabulary = *logit_shape.last().unwrap_or(&0) as usize;
    anyhow::ensure!(vocabulary > 0, "the decoder returned an empty vocabulary");
    Ok(argmax(&logits[logits.len() - vocabulary..]))
}

fn argmax(logits: &[f32]) -> (i64, f32) {
    let mut best = 0usize;
    for (index, value) in logits.iter().enumerate() {
        if value > &logits[best] {
            best = index;
        }
    }
    let peak = logits[best];
    let total: f32 = logits.iter().map(|value| (value - peak).exp()).sum();
    (best as i64, if total > 0.0 { 1.0 / total } else { 0.0 })
}

// ---------------------------------------------------------------------------
// One page, end to end
// ---------------------------------------------------------------------------

/// What one page produced, beside what it cost.
#[derive(Default)]
struct PageResult {
    crops: usize,
    tokens: usize,
    regions_all: usize,
}

/// Render, stage, detect, crop and read one page, charging every stage.
///
/// The stages are the ones the application runs and in the order it runs them,
/// with two exceptions, both named: the PNG staging is done to a real file
/// because that is what `worker_layout::stage` does, and the whole-page pass is
/// separated from the tiles by running the page-only recipe first — the tiles'
/// cost is the difference. Nothing else is simulated.
#[allow(clippy::too_many_arguments)]
fn run_page(
    document: &Document,
    entry: &Entry,
    detector_session: &mut Session,
    recognizer: Option<&mut Recognizer>,
    scratch: &Path,
    tiles: bool,
    batch: usize,
    ledger: &mut Ledger,
) -> anyhow::Result<PageResult> {
    let mut result = PageResult::default();
    let page = ledger.time("page load", || document.load_page(entry.page))?;
    let bounds = page.bounds()?;
    let page_bbox = BoundingBox {
        x: bounds.x0,
        y: bounds.y0,
        width: bounds.x1 - bounds.x0,
        height: bounds.y1 - bounds.y0,
    };

    let survey = ledger.time(
        "text layer + word survey",
        || -> anyhow::Result<PageSurvey> {
            let text_page =
                page.to_text_page(TextPageFlags::ACCURATE_BBOXES | TextPageFlags::PRESERVE_IMAGES)?;
            Ok(page_words(&text_page))
        },
    )?;

    let render_side = Recipe::PRODUCTION.render_side;
    let square = ledger.time("page render @1600", || {
        typeset::render_page(&page, render_side)
    })?;

    let staged = scratch.join("page.png");
    ledger.time("stage: PNG encode", || {
        square.save_with_format(&staged, image::ImageFormat::Png)
    })?;
    let reread = ledger.time("stage: PNG decode", || -> anyhow::Result<RgbImage> {
        Ok(image::ImageReader::open(&staged)?.decode()?.to_rgb8())
    })?;

    let recipe = if tiles {
        Recipe::PRODUCTION
    } else {
        Recipe {
            tile_stride: None,
            ..Recipe::PRODUCTION
        }
    };
    let mut passes: Vec<Pass> = Vec::new();
    for (index, window) in recipe.windows().into_iter().enumerate() {
        // The tile gate, as `DocLayout::passes` applies it: the whole-page
        // pass first, and the four tiles only where it already saw something.
        // Here rather than only in the detector because this probe owns its
        // own session — a stages table that tiled a page production would not
        // would be a table of a pipeline nobody runs.
        if index == 1 {
            if let Some(floor) = recipe.tile_gate {
                if doclayout::best_formula_score(&passes[0].rows) < floor {
                    break;
                }
            }
        }
        let name = if index == 0 {
            "detector: whole page"
        } else {
            "detector: 4 tiles"
        };
        let rows = ledger.time(name, || -> anyhow::Result<Vec<f32>> {
            let input = doclayout::cut(&reread, &window);
            detect_window(detector_session, &input)
        })?;
        passes.push(Pass { window, rows });
    }

    let regions = ledger.time("decode + claim", || {
        let found = doclayout::decode(&passes, &recipe);
        let all = found.len();
        (
            typeset::regions((entry.page + 1) as u32, &page_bbox, &found, &survey),
            all,
        )
    });
    result.regions_all = regions.1;
    let formulas: Vec<&typeset::TypesetRegion> = regions
        .0
        .iter()
        .filter(|region| region.kind == RegionKind::Formula)
        .collect();
    result.crops = formulas.len();

    let mut crops: Vec<RgbImage> = Vec::with_capacity(formulas.len());
    for region in &formulas {
        let drawn = ledger.time("crop render", || typeset::render(&page, &region.bbox))?;
        crops.push(drawn.0.pixels);
    }

    // What `worker_ocr::stage` costs: one PNG per crop, written and read back.
    for (index, crop) in crops.iter().enumerate() {
        let path = scratch.join(format!("crop-{index}.png"));
        ledger.time("crop stage: PNG round trip", || -> anyhow::Result<()> {
            crop.save_with_format(&path, image::ImageFormat::Png)?;
            let _ = image::ImageReader::open(&path)?.decode()?.to_rgb8();
            Ok(())
        })?;
    }

    let Some(recognizer) = recognizer else {
        return Ok(result);
    };
    for chunk in crops.chunks(batch.max(1)) {
        let (cpu0, wall0) = (cpu_seconds(), Instant::now());
        let (_, _, cost) = recognizer.read(chunk, 512)?;
        let (wall, cpu) = (wall0.elapsed(), cpu_seconds() - cpu0);
        // The three halves are timed inside `read` with no CPU clock of their
        // own; the CPU is charged in proportion to the wall each took, so the
        // effective-cores column of the whole call is right and the split is
        // stated for what it is.
        let share = |part: Duration| {
            if wall.as_secs_f64() > 0.0 {
                cpu * part.as_secs_f64() / wall.as_secs_f64()
            } else {
                0.0
            }
        };
        ledger
            .at("texify: preprocess")
            .add(cost.preprocess, share(cost.preprocess));
        ledger
            .at("texify: encoder")
            .add(cost.encoder, share(cost.encoder));
        ledger
            .at("texify: decoder")
            .add(cost.decoder, share(cost.decoder));
        result.tokens += cost.tokens;
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// The modes
// ---------------------------------------------------------------------------

struct Options {
    fixture: Vec<Entry>,
    threads: usize,
    batch: usize,
    lanes: usize,
    tiles: bool,
    math: usize,
    prose: usize,
    recognize: bool,
    /// What the recognizer's sessions get, when it differs from the
    /// detector's. Production splits them — `dispatch::recognizer_layout`
    /// gives Texify 4 and `load_layout_detector_local` gives the detector one
    /// thread short of the machine — so a row claiming to be production has to
    /// be able to say so.
    texify_threads: Option<usize>,
    /// The decode cap. Production's is 512; a sweep can lower it to keep one
    /// looping crop from charging a whole batch for 512 steps, and says so.
    cap: usize,
    /// How many of the selected pages' crops the recognizer levers run over.
    /// Every `k`-th crop of the whole list, so a sweep that cannot afford all
    /// of them is still spread across every page rather than being the first
    /// page twice.
    crops: usize,
    /// Which decoder the Texify stage runs: the cached pair, which is what
    /// `texify::Texify` ships, or the single graph that re-runs the whole
    /// prefix every step, which is what it shipped before. `--decoder
    /// uncached` is the only way to get the second, and it exists so a
    /// before-and-after is two rows of one run rather than two runs of two
    /// builds.
    cached: bool,
    /// The intra-op thread counts the `threads` mode sweeps.
    sweep: Vec<usize>,
}

/// A staging directory nothing else in this process writes to.
///
/// Counted rather than named after the group, because the concurrency mode
/// runs several groups at once under one name and two lanes staging over one
/// another's `page.png` is a decode error, not a measurement.
fn scratch_dir(name: &str) -> anyhow::Result<PathBuf> {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "wilkes-perf-{name}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// One group of pages, run once, with every stage charged.
#[allow(clippy::too_many_arguments)]
fn run_group(
    label: &str,
    pages: &[Entry],
    threads: usize,
    texify_threads: usize,
    tiles: bool,
    batch: usize,
    recognize: bool,
    cached: bool,
    print: bool,
) -> anyhow::Result<(Ledger, usize, usize)> {
    if pages.is_empty() {
        return Ok((Ledger::default(), 0, 0));
    }
    let scratch = scratch_dir(label)?;
    let mut detector = session(&layout_graph(), threads, false)?;
    let mut recognizer = if recognize {
        Some(Recognizer::load(texify_threads, false, cached)?)
    } else {
        None
    };
    let mut ledger = Ledger::default();
    let (mut crops, mut tokens) = (0usize, 0usize);
    let mut opened: Option<(PathBuf, Document)> = None;
    for entry in pages {
        if opened
            .as_ref()
            .map(|(p, _)| *p != entry.pdf)
            .unwrap_or(true)
        {
            let document = Document::open(entry.pdf.as_path())
                .with_context(|| format!("could not open {}", entry.pdf.display()))?;
            opened = Some((entry.pdf.clone(), document));
        }
        let document = &opened.as_ref().expect("just opened").1;
        let out = run_page(
            document,
            entry,
            &mut detector,
            recognizer.as_mut(),
            &scratch,
            tiles,
            batch,
            &mut ledger,
        )
        .with_context(|| format!("{}", tag(entry)))?;
        crops += out.crops;
        tokens += out.tokens;
    }
    let _ = std::fs::remove_dir_all(&scratch);
    if print {
        ledger.print(pages.len(), label);
        println!(
            "  {crops} formula crop(s) over {} page(s) — {:.1} a page, {tokens} token(s) decoded",
            pages.len(),
            crops as f64 / pages.len() as f64,
        );
    }
    Ok((ledger, crops, tokens))
}

/// What the whole-page pass says about a page, before the tiles are paid for.
///
/// The tile gate reads exactly one number off that pass — the best score any
/// `display_formula` or `inline_formula` row carries, at *no* threshold — and
/// runs the four tiles only when it clears a floor. This mode prints that
/// number for every page of the fixture beside what the page actually holds,
/// so the floor is chosen from the distribution rather than guessed: a floor
/// above any labelled page's number would take that page off the tiled path.
///
/// The whole-page pass is the production one — 1600 px render, downsampled to
/// the graph's square — because the gate runs on that pass and not on a
/// separate cheaper look at the page.
fn mode_gate(options: &Options) -> anyhow::Result<()> {
    let mut detector = session(&layout_graph(), options.threads, false)?;
    let whole = Recipe {
        tile_stride: None,
        ..Recipe::PRODUCTION
    };
    let window = whole.windows()[0];

    println!(
        "\n{:<28} {:>7} {:>12} {:>10} {:>10} {:>12}",
        "page", "labels", "best score", "rows>=.05", "crops tiled", "crops whole"
    );
    let mut worst_math = f32::INFINITY;
    let mut worst_math_page = String::new();
    let mut best_prose = 0f32;
    let mut best_prose_page = String::new();
    let mut opened: Option<(PathBuf, Document)> = None;
    for entry in &options.fixture {
        if opened
            .as_ref()
            .map(|(p, _)| *p != entry.pdf)
            .unwrap_or(true)
        {
            opened = Some((entry.pdf.clone(), Document::open(entry.pdf.as_path())?));
        }
        let document = &opened.as_ref().expect("just opened").1;
        let page = document.load_page(entry.page)?;
        let bounds = page.bounds()?;
        let page_bbox = BoundingBox {
            x: bounds.x0,
            y: bounds.y0,
            width: bounds.x1 - bounds.x0,
            height: bounds.y1 - bounds.y0,
        };
        let text_page =
            page.to_text_page(TextPageFlags::ACCURATE_BBOXES | TextPageFlags::PRESERVE_IMAGES)?;
        let survey = page_words(&text_page);
        let square = typeset::render_page(&page, Recipe::PRODUCTION.render_side)?;

        let mut passes = Vec::new();
        for window in Recipe::PRODUCTION.windows() {
            let rows = detect_window(&mut detector, &doclayout::cut(&square, &window))?;
            passes.push(Pass { window, rows });
        }
        // The gate's own number, read off the whole-page pass alone.
        let best = doclayout::best_formula_score(&passes[0].rows);
        let above = passes[0]
            .rows
            .chunks_exact(8)
            .filter(|row| {
                doclayout::LABELS
                    .get(row[0] as usize)
                    .is_some_and(|label| doclayout::is_formula(label))
                    && row[1] >= 0.05
            })
            .count();

        let crops = |passes: &[Pass], recipe: &Recipe| {
            let found = doclayout::decode(passes, recipe);
            typeset::regions((entry.page + 1) as u32, &page_bbox, &found, &survey)
                .into_iter()
                .filter(|region| region.kind == RegionKind::Formula)
                .count()
        };
        let tiled = crops(&passes, &Recipe::PRODUCTION);
        let alone = crops(&passes[..1], &whole);
        let _ = window;

        println!(
            "{:<28} {:>7} {:>12.4} {:>10} {:>10} {:>12}",
            tag(entry),
            entry.boxes.len(),
            best,
            above,
            tiled,
            alone
        );
        if entry.boxes.is_empty() {
            if best > best_prose {
                best_prose = best;
                best_prose_page = tag(entry);
            }
        } else if best < worst_math {
            worst_math = best;
            worst_math_page = tag(entry);
        }
    }
    println!(
        "\nlowest best-score over labelled pages : {worst_math:.4}  ({worst_math_page})\n\
         highest best-score over unlabelled ones: {best_prose:.4}  ({best_prose_page})\n\
         the gate's floor is {}",
        doclayout::TILE_GATE_FLOOR
    );
    Ok(())
}

/// Recognition laid out the way production lays it out: N readers of M threads
/// each, handed one document's crops in one call.
///
/// Through [`texify::Texify`] itself and not through this probe's own decode
/// loop, because what is under measurement here is the *distribution* — which
/// reader takes which crop, and what several of them cost at once — and that
/// belongs to the engine. A probe that spread the crops itself would be
/// measuring a layout nobody runs.
///
/// One layout per invocation, named by `--readers` and `--texify-threads`, so
/// the peak RSS printed is this layout's own: `ru_maxrss` is a high-water mark
/// that never falls, and four layouts in one process would report the largest
/// of them four times.
fn mode_readers(options: &Options) -> anyhow::Result<()> {
    use wilkes_core::extract::image::ocr::OcrEngine as _;
    use wilkes_core::extract::image::texify::Texify;

    let threads = options.texify_threads.unwrap_or(4);
    let readers = options.lanes.max(1);
    let (math, _) = select(&options.fixture, options.math, options.prose);
    let crops = thinned(crops_of(&math, options.threads)?, options.crops);
    let engine = Texify::load(&model_dir(), readers, threads)?;
    println!(
        "\n{} crop(s) over {} page(s) in one call · {readers} reader(s) x {threads} thread(s)",
        crops.len(),
        math.len()
    );

    // One crop first, untimed: a reader's first call pays for loading 543 MB
    // of graphs, and charging that to a page would make this a table of load
    // times. Every reader is warmed, because a layout whose second reader
    // loads inside the measured call is not the layout being measured.
    let warm: Vec<RgbImage> = crops.iter().take(readers).cloned().collect();
    let _ = engine.spot_batch(&warm)?;

    let (cpu0, wall0) = (cpu_seconds(), Instant::now());
    let read = engine.spot_batch(&crops)?;
    let (wall, cpu) = (wall0.elapsed(), cpu_seconds() - cpu0);
    anyhow::ensure!(read.len() == crops.len(), "one answer per crop");
    let transcribed = read.iter().filter(|one| !one.regions.is_empty()).count();

    println!(
        "\n{:<9} {:>8} {:>10} {:>11} {:>10} {:>8} {:>10} {:>12}",
        "readers", "threads", "wall s", "ms/crop", "ms/page", "cores", "peak RSS", "transcribed"
    );
    println!(
        "{readers:<9} {threads:>8} {:>10.1} {:>11.0} {:>10.0} {:>8.2} {:>9.0}M {:>12}",
        wall.as_secs_f64(),
        wall.as_secs_f64() * 1000.0 / crops.len() as f64,
        wall.as_secs_f64() * 1000.0 / math.len() as f64,
        cpu / wall.as_secs_f64(),
        mib(peak_rss_bytes()),
        format!("{transcribed}/{}", crops.len()),
    );
    Ok(())
}

fn mode_stages(options: &Options) -> anyhow::Result<()> {
    let (math, prose) = select(&options.fixture, options.math, options.prose);
    println!(
        "math-heavy: {}",
        math.iter().map(tag).collect::<Vec<_>>().join(" ")
    );
    println!(
        "prose     : {}",
        prose.iter().map(tag).collect::<Vec<_>>().join(" ")
    );
    println!(
        "detector threads {} · texify threads {} · tiles {} · batch {}",
        options.threads,
        options.texify_threads.unwrap_or(options.threads),
        if options.tiles { "on" } else { "off" },
        options.batch
    );

    // Warm: the first session's first run pays for arena growth and page
    // faults on 204 MB of weights, and charging that to a page would make the
    // first page of every table a different measurement from the rest.
    let warm = math.first().cloned().into_iter().collect::<Vec<_>>();
    run_group(
        "warm-up",
        &warm,
        options.threads,
        options.texify_threads.unwrap_or(options.threads),
        options.tiles,
        options.batch,
        options.recognize,
        options.cached,
        false,
    )?;

    for (label, pages) in [("math-heavy", &math), ("prose", &prose)] {
        let (_, _, _) = run_group(
            label,
            pages,
            options.threads,
            options.texify_threads.unwrap_or(options.threads),
            options.tiles,
            options.batch,
            options.recognize,
            options.cached,
            true,
        )?;
    }
    println!("\npeak RSS {:.0} MiB", mib(peak_rss_bytes()));
    Ok(())
}

fn mode_threads(options: &Options) -> anyhow::Result<()> {
    let (math, _) = select(&options.fixture, options.math, options.prose);
    let scratch = scratch_dir("threads")?;
    let squares = render_all(&math)?;
    let crops = thinned(crops_of(&math, options.threads)?, options.crops);
    println!(
        "\n{} page(s) rendered once, {} crop(s) cut once; only the graphs are re-run",
        squares.len(),
        crops.len()
    );

    println!(
        "\n{:<9} {:>12} {:>8} {:>12} {:>10} {:>8} {:>10} {:>8} {:>7} {:>7} {:>7} {:>8} {:>8}",
        "threads",
        "det ms/page",
        "cores",
        "det whole ms",
        "det tiles",
        "cores",
        "texify/crop",
        "cores",
        "pre ms",
        "enc ms",
        "dec ms",
        "tokens",
        "ms/step",
    );
    for threads in options.sweep.clone() {
        let mut detector = session(&layout_graph(), threads, false)?;
        let mut whole = Cost::default();
        let mut tiled = Cost::default();
        // One untimed page first: a fresh session's first run is arena growth.
        if let Some(square) = squares.first() {
            for window in Recipe::PRODUCTION.windows() {
                let _ = detect_window(&mut detector, &doclayout::cut(square, &window))?;
            }
        }
        for square in &squares {
            for (index, window) in Recipe::PRODUCTION.windows().into_iter().enumerate() {
                let input = doclayout::cut(square, &window);
                let (cpu0, wall0) = (cpu_seconds(), Instant::now());
                let _ = detect_window(&mut detector, &input)?;
                let (wall, cpu) = (wall0.elapsed(), cpu_seconds() - cpu0);
                if index == 0 {
                    whole.add(wall, cpu);
                } else {
                    tiled.add(wall, cpu);
                }
            }
        }
        drop(detector);

        let mut recognizer = Recognizer::load(threads, false, options.cached)?;
        let _ = recognizer.read(&crops[..1], options.cap)?;
        let mut texify = Cost::default();
        let (mut pre, mut enc, mut dec) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
        let (mut tokens, mut steps) = (0usize, 0usize);
        for crop in &crops {
            let (cpu0, wall0) = (cpu_seconds(), Instant::now());
            let (_, _, cost) = recognizer.read(std::slice::from_ref(crop), options.cap)?;
            texify.add(wall0.elapsed(), cpu_seconds() - cpu0);
            pre += cost.preprocess;
            enc += cost.encoder;
            dec += cost.decoder;
            tokens += cost.tokens;
            steps += cost.steps;
        }
        drop(recognizer);

        let pages = squares.len() as f64;
        let n = crops.len() as f64;
        println!(
            "{threads:<9} {:>12.0} {:>8.2} {:>12.0} {:>10.0} {:>8.2} {:>10.0} {:>8.2} \
             {:>7.1} {:>7.1} {:>7.1} {:>8} {:>8.2}",
            (whole.ms() + tiled.ms()) / pages,
            (whole.cpu + tiled.cpu) / (whole.wall + tiled.wall).as_secs_f64(),
            whole.ms() / pages,
            tiled.ms() / pages,
            tiled.cores(),
            texify.ms() / texify.runs.max(1) as f64,
            texify.cores(),
            pre.as_secs_f64() * 1000.0 / n,
            enc.as_secs_f64() * 1000.0 / n,
            dec.as_secs_f64() * 1000.0 / n,
            tokens,
            dec.as_secs_f64() * 1000.0 / steps.max(1) as f64,
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}

fn mode_batch(options: &Options) -> anyhow::Result<()> {
    let (math, _) = select(&options.fixture, options.math, options.prose);
    let crops = thinned(crops_of(&math, options.threads)?, options.crops);
    // Uncached by construction: batching is a lever over the graph that takes
    // a whole prefix, and the cached graph decodes one token of one sequence.
    let mut recognizer = Recognizer::load(options.threads, false, false)?;
    println!("\nencoder input : {:?}", graph_io(&recognizer.encoder));
    println!("decoder input : {:?}", graph_io(&recognizer.decoder));
    let _ = recognizer.read(&crops[..1], options.cap)?;

    println!(
        "\n{:<7} {:>8} {:>11} {:>11} {:>11} {:>11} {:>9} {:>8} {:>8}",
        "batch", "calls", "ms/crop", "pre ms", "enc ms", "dec ms", "steps", "tokens", "cores"
    );
    // What batch 1 — the production path — reads. Every larger batch is
    // compared against it, because the graphs are *dynamically* quantized:
    // the activation scale of an int8 matmul is taken from the range of the
    // whole tensor, and in a batch that tensor spans every sequence in it. So
    // batching is not a pure rearrangement of the same arithmetic, and whether
    // it changes the answer is a question that has to be asked rather than
    // assumed away.
    let mut reference: Vec<String> = Vec::new();
    for batch in [1usize, 4, 8, 16] {
        let mut answers: Vec<String> = Vec::new();
        let mut total = Cost::default();
        let (mut pre, mut enc, mut dec) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
        let (mut steps, mut tokens, mut calls) = (0usize, 0usize, 0usize);
        for chunk in crops.chunks(batch) {
            let (cpu0, wall0) = (cpu_seconds(), Instant::now());
            let (said, _, cost) = recognizer.read(chunk, options.cap)?;
            total.add(wall0.elapsed(), cpu_seconds() - cpu0);
            answers.extend(said);
            pre += cost.preprocess;
            enc += cost.encoder;
            dec += cost.decoder;
            steps += cost.steps;
            tokens += cost.tokens;
            calls += 1;
            // A batch runs as many steps as its longest answer, so one crop
            // that decodes to the cap charges every crop beside it for the
            // whole of it. Printed per call rather than only in the total,
            // because that is the shape of the answer and an average hides it.
            eprintln!(
                "    batch {batch} call {calls}/{}: {} step(s), {} token(s), {:.1}s",
                crops.len().div_ceil(batch),
                cost.steps,
                cost.tokens,
                wall0.elapsed().as_secs_f64()
            );
        }
        let n = crops.len() as f64;
        println!(
            "{batch:<7} {calls:>8} {:>11.0} {:>11.0} {:>11.0} {:>11.0} {steps:>9} {tokens:>8} {:>8.2}",
            total.ms() / n,
            pre.as_secs_f64() * 1000.0 / n,
            enc.as_secs_f64() * 1000.0 / n,
            dec.as_secs_f64() * 1000.0 / n,
            total.cores(),
        );
        if batch == 1 {
            reference = answers;
        } else {
            let differ = reference
                .iter()
                .zip(&answers)
                .filter(|(a, b)| a != b)
                .count();
            println!(
                "        {differ} of {} reading(s) differ from batch 1",
                reference.len()
            );
        }
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
    }
    Ok(())
}

fn graph_io(session: &Session) -> Vec<String> {
    session
        .inputs()
        .iter()
        .map(|input| format!("{}{:?}", input.name(), input.dtype()))
        .collect()
}

fn mode_concurrency(options: &Options) -> anyhow::Result<()> {
    let (math, _) = select(&options.fixture, options.math, options.prose);
    println!(
        "\n{:<7} {:>9} {:>12} {:>12} {:>10} {:>10}",
        "lanes", "threads", "wall s", "pages/min", "cores", "peak RSS"
    );
    for lanes in [1usize, 2, 3] {
        if options.lanes != 0 && lanes != options.lanes {
            continue;
        }
        let threads = (10 / lanes).max(1);
        let (cpu0, wall0) = (cpu_seconds(), Instant::now());
        let mut handles = Vec::new();
        for lane in 0..lanes {
            // Each lane gets its own slice of the pages, its own sessions and
            // its own document handles: two lanes sharing a session would be
            // one lane with a lock in it.
            let pages: Vec<Entry> = math
                .iter()
                .enumerate()
                .filter(|(index, _)| index % lanes == lane)
                .map(|(_, entry)| entry.clone())
                .collect();
            let batch = options.batch;
            let recognizer_threads = options.texify_threads.unwrap_or(threads);
            let cached = options.cached;
            let tiles = options.tiles;
            let recognize = options.recognize;
            handles.push(std::thread::spawn(move || {
                run_group(
                    "lane",
                    &pages,
                    threads,
                    recognizer_threads,
                    tiles,
                    batch,
                    recognize,
                    cached,
                    false,
                )
            }));
        }
        let mut crops = 0usize;
        for handle in handles {
            let (_, lane_crops, _) = handle.join().expect("a lane panicked")?;
            crops += lane_crops;
        }
        let (wall, cpu) = (wall0.elapsed(), cpu_seconds() - cpu0);
        println!(
            "{lanes:<7} {threads:>9} {:>12.1} {:>12.1} {:>10.2} {:>9.0}M   ({crops} crops)",
            wall.as_secs_f64(),
            math.len() as f64 * 60.0 / wall.as_secs_f64(),
            cpu / wall.as_secs_f64(),
            mib(peak_rss_bytes()),
        );
    }
    Ok(())
}

fn mode_coreml(options: &Options) -> anyhow::Result<()> {
    let (math, _) = select(&options.fixture, options.math, options.prose);
    let squares = render_all(&math)?;
    let crops = thinned(crops_of(&math, options.threads)?, options.crops);

    println!("\n── detector under CoreML ──");
    match session(&layout_graph(), options.threads, true) {
        Err(error) => println!("  the graph did not load: {error:#}"),
        Ok(mut coreml) => {
            let mut cpu = session(&layout_graph(), options.threads, false)?;
            let mut on = Cost::default();
            let mut off = Cost::default();
            let mut worst = 0f32;
            let mut regions = (0usize, 0usize);
            // One untimed pass each.
            for square in squares.iter().take(1) {
                for window in Recipe::PRODUCTION.windows() {
                    let input = doclayout::cut(square, &window);
                    let _ = detect_window(&mut coreml, &input)?;
                    let _ = detect_window(&mut cpu, &input)?;
                }
            }
            for square in &squares {
                let mut a: Vec<Pass> = Vec::new();
                let mut b: Vec<Pass> = Vec::new();
                for window in Recipe::PRODUCTION.windows() {
                    let input = doclayout::cut(square, &window);
                    let (c0, w0) = (cpu_seconds(), Instant::now());
                    let rows = detect_window(&mut coreml, &input)?;
                    on.add(w0.elapsed(), cpu_seconds() - c0);
                    let (c0, w0) = (cpu_seconds(), Instant::now());
                    let base = detect_window(&mut cpu, &input)?;
                    off.add(w0.elapsed(), cpu_seconds() - c0);
                    a.push(Pass { window, rows });
                    b.push(Pass { window, rows: base });
                }
                let (delta, on_count, off_count) = box_delta(&a, &b);
                worst = worst.max(delta);
                regions = (regions.0 + on_count, regions.1 + off_count);
            }
            let pages = squares.len() as f64;
            println!(
                "  CoreML {:.0} ms/page ({:.2} cores) · CPU {:.0} ms/page ({:.2} cores)",
                on.ms() / pages,
                on.cores(),
                off.ms() / pages,
                off.cores()
            );
            println!(
                "  {} region(s) decoded under CoreML, {} under CPU; worst corner delta over the \n                   matched ones, in page fractions: {worst:.6}",
                regions.0, regions.1
            );
        }
    }

    println!("\n── texify under CoreML ──");
    match Recognizer::load(options.threads, true, options.cached) {
        Err(error) => println!("  the graphs did not load: {error:#}"),
        Ok(mut coreml) => {
            let mut cpu = Recognizer::load(options.threads, false, options.cached)?;
            let _ = coreml.read(&crops[..1], options.cap)?;
            let _ = cpu.read(&crops[..1], options.cap)?;
            let (mut on, mut off) = (Cost::default(), Cost::default());
            let (mut enc_on, mut enc_off) = (Duration::ZERO, Duration::ZERO);
            let mut same = 0usize;
            for crop in &crops {
                let (c0, w0) = (cpu_seconds(), Instant::now());
                let (a, _, ca) = coreml.read(std::slice::from_ref(crop), options.cap)?;
                on.add(w0.elapsed(), cpu_seconds() - c0);
                let (c0, w0) = (cpu_seconds(), Instant::now());
                let (b, _, cb) = cpu.read(std::slice::from_ref(crop), options.cap)?;
                off.add(w0.elapsed(), cpu_seconds() - c0);
                enc_on += ca.encoder;
                enc_off += cb.encoder;
                same += usize::from(a == b);
            }
            let n = crops.len() as f64;
            println!(
                "  CoreML {:.0} ms/crop ({:.2} cores, encoder {:.0} ms) · CPU {:.0} ms/crop \
                 ({:.2} cores, encoder {:.0} ms)",
                on.ms() / n,
                on.cores(),
                enc_on.as_secs_f64() * 1000.0 / n,
                off.ms() / n,
                off.cores(),
                enc_off.as_secs_f64() * 1000.0 / n,
            );
            println!(
                "  {same} of {} readings identical to the CPU's",
                crops.len()
            );
        }
    }
    Ok(())
}

/// How far apart two runs' *decoded* detections are, in page fractions.
///
/// Not row for row off the raw tensor: the graph returns a variable number of
/// rows and two providers that both found the same page can return them in a
/// different order and a different count, so a row-wise difference measures
/// nothing. What is compared is what `decode` believed — each region of the
/// first run matched to the nearest region of the second of the same label, and
/// the worst corner displacement over those matches. The counts are returned
/// beside it, because a provider that dropped a region entirely would otherwise
/// show as a small delta.
fn box_delta(a: &[Pass], b: &[Pass]) -> (f32, usize, usize) {
    let recipe = Recipe::PRODUCTION;
    let one = doclayout::decode(a, &recipe);
    let two = doclayout::decode(b, &recipe);
    let mut worst = 0f32;
    for region in &one {
        let nearest = two
            .iter()
            .filter(|other| other.label == region.label)
            .map(|other| {
                (region.bbox.x - other.bbox.x)
                    .abs()
                    .max((region.bbox.y - other.bbox.y).abs())
                    .max((region.bbox.width - other.bbox.width).abs())
                    .max((region.bbox.height - other.bbox.height).abs())
            })
            .min_by(f32::total_cmp);
        // No region of that label at all in the other run is a whole region of
        // disagreement, and is reported as the largest delta there can be.
        worst = worst.max(nearest.unwrap_or(1.0));
    }
    (worst, one.len(), two.len())
}

// ---------------------------------------------------------------------------
// Shared inputs for the levers
// ---------------------------------------------------------------------------

/// Every selected page rendered once at the production side.
fn render_all(pages: &[Entry]) -> anyhow::Result<Vec<RgbImage>> {
    let mut out = Vec::with_capacity(pages.len());
    let mut opened: Option<(PathBuf, Document)> = None;
    for entry in pages {
        if opened
            .as_ref()
            .map(|(p, _)| *p != entry.pdf)
            .unwrap_or(true)
        {
            opened = Some((entry.pdf.clone(), Document::open(entry.pdf.as_path())?));
        }
        let page = opened
            .as_ref()
            .expect("just opened")
            .1
            .load_page(entry.page)?;
        out.push(typeset::render_page(&page, Recipe::PRODUCTION.render_side)?);
    }
    Ok(out)
}

/// Every Formula crop the production recipe would hand the recognizer, cut
/// once, so a lever that only touches the recognizer is not also re-detecting.
fn crops_of(pages: &[Entry], threads: usize) -> anyhow::Result<Vec<RgbImage>> {
    let mut detector = session(&layout_graph(), threads, false)?;
    let mut out = Vec::new();
    let mut opened: Option<(PathBuf, Document)> = None;
    for entry in pages {
        if opened
            .as_ref()
            .map(|(p, _)| *p != entry.pdf)
            .unwrap_or(true)
        {
            opened = Some((entry.pdf.clone(), Document::open(entry.pdf.as_path())?));
        }
        let page = opened
            .as_ref()
            .expect("just opened")
            .1
            .load_page(entry.page)?;
        let bounds = page.bounds()?;
        let text_page =
            page.to_text_page(TextPageFlags::ACCURATE_BBOXES | TextPageFlags::PRESERVE_IMAGES)?;
        let survey = page_words(&text_page);
        let square = typeset::render_page(&page, Recipe::PRODUCTION.render_side)?;
        let mut passes: Vec<Pass> = Vec::new();
        for (index, window) in Recipe::PRODUCTION.windows().into_iter().enumerate() {
            // The gate, as production applies it: these are the crops the
            // recognizer would actually be handed, not the crops an ungated
            // detector would find.
            if index == 1 {
                if let Some(floor) = Recipe::PRODUCTION.tile_gate {
                    if doclayout::best_formula_score(&passes[0].rows) < floor {
                        break;
                    }
                }
            }
            let rows = detect_window(&mut detector, &doclayout::cut(&square, &window))?;
            passes.push(Pass { window, rows });
        }
        let found = doclayout::decode(&passes, &Recipe::PRODUCTION);
        let bbox = BoundingBox {
            x: bounds.x0,
            y: bounds.y0,
            width: bounds.x1 - bounds.x0,
            height: bounds.y1 - bounds.y0,
        };
        for region in typeset::regions((entry.page + 1) as u32, &bbox, &found, &survey) {
            if region.kind != RegionKind::Formula {
                continue;
            }
            out.push(typeset::render(&page, &region.bbox)?.0.pixels);
        }
    }
    anyhow::ensure!(!out.is_empty(), "no formula crop on the selected pages");
    Ok(out)
}

/// Every `k`-th crop, so a sweep too expensive to run over all of them is
/// still spread over every page.
fn thinned(crops: Vec<RgbImage>, keep: usize) -> Vec<RgbImage> {
    if keep == 0 || crops.len() <= keep {
        return crops;
    }
    let stride = crops.len().div_ceil(keep);
    crops.into_iter().step_by(stride).collect()
}

/// The cached decoder against the uncached one, crop by crop.
///
/// Both graphs are dynamically quantized: an int8 matmul takes its activation
/// scale from the range of the whole tensor it runs over, and the two graphs
/// do not run over the same tensors — the cached one multiplies one row where
/// the other multiplies the whole prefix. So a reading that is the same is a
/// finding and not an identity, and it has to be measured rather than argued.
///
/// The production engine is read beside them, over the same crops, because a
/// probe that agreed with itself and disagreed with `texify::Texify` would
/// have measured a decode nobody runs.
fn mode_equivalence(options: &Options) -> anyhow::Result<()> {
    use wilkes_core::extract::image::ocr::OcrEngine;
    use wilkes_core::extract::image::texify::Texify;

    let threads = options.texify_threads.unwrap_or(options.threads);
    let (math, _) = select(&options.fixture, options.math, options.prose);
    let crops = thinned(crops_of(&math, options.threads)?, options.crops);
    println!(
        "\n{} crop(s) over {} page(s), texify threads {threads}, cap {}",
        crops.len(),
        math.len(),
        options.cap
    );

    let mut old = Recognizer::load(threads, false, false)?;
    let mut new = Recognizer::load(threads, false, true)?;
    let engine = Texify::load(&model_dir(), 1, threads)?;

    let (mut differ, mut engine_differs) = (0usize, 0usize);
    let (mut worst, mut total_delta) = (0f64, 0f64);
    for (index, crop) in crops.iter().enumerate() {
        let (before, before_confidence, _) = old.read(std::slice::from_ref(crop), options.cap)?;
        let (after, after_confidence, _) = new.read(std::slice::from_ref(crop), options.cap)?;
        // The engine's own answer, through `spot_batch`: an empty region list
        // is its answer for "nothing to transcribe", which is the empty string
        // here.
        let read = engine.spot_batch(std::slice::from_ref(crop))?;
        let said = read
            .first()
            .and_then(|recognition| recognition.regions.first())
            .map(|region| (region.text.clone(), region.confidence))
            .unwrap_or_else(|| (String::new(), 0.0));

        let delta = f64::from(after_confidence[0]) - f64::from(before_confidence[0]);
        total_delta += delta;
        if delta.abs() > worst {
            worst = delta.abs();
        }
        if before[0] != after[0] {
            differ += 1;
            println!(
                "  crop {index} differs\n    uncached: {:?}\n    cached  : {:?}",
                before[0], after[0]
            );
        }
        if said.0 != after[0] {
            engine_differs += 1;
            println!(
                "  crop {index}: the engine and this probe's cached loop disagree\n    \
                 engine: {:?}\n    probe : {:?}",
                said.0, after[0]
            );
        }
    }

    let n = crops.len();
    println!("\n{differ} of {n} reading(s) differ between the uncached and the cached decoder");
    println!(
        "confidence: mean delta {:+.6}, worst |delta| {:.6}",
        total_delta / n.max(1) as f64,
        worst
    );
    println!("{engine_differs} of {n} reading(s) differ between texify::Texify and this probe");
    Ok(())
}

fn mode_graphs(options: &Options) -> anyhow::Result<()> {
    let recognizer = Recognizer::load(options.threads, false, options.cached)?;
    let detector = session(&layout_graph(), options.threads, false)?;
    for (name, session) in [
        ("layout", &detector),
        ("encoder", &recognizer.encoder),
        ("decoder", &recognizer.decoder),
    ] {
        println!("\n{name}:");
        for input in session.inputs() {
            println!("  in  {:<24} {:?}", input.name(), input.dtype());
        }
        for output in session.outputs() {
            println!("  out {:<24} {:?}", output.name(), output.dtype());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let fixture = args
        .next()
        .context("usage: <labels.json> <stages|threads|batch|concurrency|coreml|graphs> [flags]")?;
    let mode = args.next().unwrap_or_else(|| "stages".to_string());
    let mut options = Options {
        fixture: serde_json::from_str(
            &std::fs::read_to_string(&fixture)
                .with_context(|| format!("could not read {fixture}"))?,
        )
        .with_context(|| format!("{fixture} is not the fixture shape this probe reads"))?,
        threads: std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).max(1))
            .unwrap_or(1),
        batch: 1,
        lanes: 0,
        tiles: true,
        math: 5,
        prose: 3,
        recognize: true,
        crops: 32,
        cap: 512,
        texify_threads: None,
        cached: true,
        sweep: vec![2, 4, 6, 8, 10],
    };
    while let Some(flag) = args.next() {
        let mut value = || args.next().context("that flag wants a value");
        match flag.as_str() {
            "--threads" => options.threads = value()?.parse()?,
            "--batch" => options.batch = value()?.parse()?,
            "--lanes" => options.lanes = value()?.parse()?,
            "--math" => options.math = value()?.parse()?,
            "--prose" => options.prose = value()?.parse()?,
            "--tiles" => options.tiles = value()? != "off",
            "--crops" => options.crops = value()?.parse()?,
            "--cap" => options.cap = value()?.parse()?,
            "--texify-threads" => options.texify_threads = Some(value()?.parse()?),
            "--decoder" => {
                let named = value()?;
                options.cached = match named.as_str() {
                    "cached" => true,
                    "uncached" => false,
                    other => anyhow::bail!("--decoder takes cached or uncached, not {other}"),
                };
            }
            "--sweep" => {
                options.sweep = value()?
                    .split(',')
                    .map(|n| n.trim().parse::<usize>())
                    .collect::<Result<Vec<usize>, _>>()?;
            }
            "--no-recognize" => options.recognize = false,
            other => anyhow::bail!("unknown flag {other}"),
        }
    }

    println!(
        "decoder : {}",
        if options.cached { "cached" } else { "uncached" }
    );
    println!("detector: {}", doclayout::identity());
    println!("texify  : {}", texify::identity());
    println!(
        "machine : {} logical core(s)",
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0)
    );

    match mode.as_str() {
        "stages" => mode_stages(&options),
        "threads" => mode_threads(&options),
        "batch" => mode_batch(&options),
        "concurrency" => mode_concurrency(&options),
        "coreml" => mode_coreml(&options),
        "graphs" => mode_graphs(&options),
        "equivalence" => mode_equivalence(&options),
        "gate" => mode_gate(&options),
        "readers" => mode_readers(&options),
        other => anyhow::bail!("unknown mode {other}"),
    }
}
