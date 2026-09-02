//! Where the ONNX recognizer's wall clock actually goes.
//!
//! Not a correctness harness. Every measurement here runs against synthetic
//! pixels and a synthetic prompt of the right *shape*, because neither the
//! vision encoder's cost nor the decoder's depends on what the page says —
//! only on how many tiles it became and how many tokens have accumulated in
//! the cache. Fixing the token budget instead of decoding to EOS turns page
//! content from a confound into a constant, which is what makes two runs at
//! different thread counts comparable at all.
//!
//!     cargo run --release --example bench_recognizer -- <model_dir> stages 2,4,6,8,9,10
//!     cargo run --release --example bench_recognizer -- <model_dir> streams 1x9,2x4,3x3,4x2
//!
//! Loads its models in this process. The application is forbidden from doing
//! that — see the "no inference in the host process" invariant in `AGENTS.md`
//! — but a probe *is* the model's process, and Ctrl-C is the kill. Not
//! precedent for anything under `src/`.

use std::path::Path;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use wilkes_core::extract::image::granite_docling::{install_dir, GraniteDocling};
use wilkes_core::extract::image::ocr::OcrEngine;
use wilkes_core::extract::image::onnx_vlm::OnnxVlm;

const VISION_GRAPH: &str = "onnx/vision_encoder.onnx";
const EMBED_GRAPH: &str = "onnx/embed_tokens.onnx";
const DECODER_GRAPH: &str = "onnx/decoder_model_merged.onnx";

/// A page-shaped image tiles 4x4 plus a thumbnail, so 17 x 64 = 1088 visual
/// tokens, which the real prompt wraps in ~30 text tokens.
const TILES: usize = 17;
const SIDE: usize = 512;
const PROMPT_LEN: usize = 1118;

/// Deterministic non-zero filler. Constant buffers are a bad benchmark input:
/// zeros are fine but denormals are not, and a fixed pattern keeps every run
/// feeding the arithmetic units the same work.
fn fill(len: usize, seed: u64) -> Vec<f32> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((state >> 33) as f32 / (1u64 << 31) as f32) - 0.5
        })
        .collect()
}

struct Stages {
    load: Duration,
    encode: Duration,
    /// Step 0: the whole prompt through the decoder, cache empty.
    prefill: Duration,
    /// Every step after the first, which is the part that runs thousands of
    /// times per page.
    steady: Duration,
    steps: usize,
}

impl Stages {
    fn per_token(&self) -> Duration {
        self.steady / self.steps.max(1) as u32
    }
}

fn measure(dir: &Path, threads: usize, steps: usize) -> anyhow::Result<Stages> {
    let started = Instant::now();
    let mut model = OnnxVlm::load(dir, VISION_GRAPH, EMBED_GRAPH, DECODER_GRAPH, threads)?;
    let load = started.elapsed();

    let pixels = fill(TILES * 3 * SIDE * SIDE, 1);
    let started = Instant::now();
    let features = model.encode_image(&pixels, TILES, SIDE)?;
    let encode = started.elapsed();
    std::hint::black_box(&features);

    let prompt = fill(PROMPT_LEN * model.hidden_size(), 2);
    let mut marks: Vec<Instant> = Vec::with_capacity(steps + 1);
    let started = Instant::now();
    model.decode(&prompt, steps, |_| false, |_| marks.push(Instant::now()))?;
    let total = started.elapsed();

    let prefill = marks
        .first()
        .map(|first| first.duration_since(started))
        .unwrap_or_default();
    Ok(Stages {
        load,
        encode,
        prefill,
        steady: total - prefill,
        steps: steps.saturating_sub(1),
    })
}

/// Aggregate decode throughput with `streams` models running at once, each
/// given `threads`. The barrier matters: staggered starts would let an early
/// stream finish into an empty machine and report a contention-free rate.
fn streams(dir: &Path, streams: usize, threads: usize, steps: usize) -> anyhow::Result<f64> {
    let models: Vec<OnnxVlm> = (0..streams)
        .map(|_| OnnxVlm::load(dir, VISION_GRAPH, EMBED_GRAPH, DECODER_GRAPH, threads))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let gate = Arc::new(Barrier::new(streams));
    let started = Instant::now();
    let elapsed: Vec<Duration> = std::thread::scope(|scope| {
        let handles: Vec<_> = models
            .into_iter()
            .map(|mut model| {
                let gate = Arc::clone(&gate);
                scope.spawn(move || {
                    let prompt = fill(PROMPT_LEN * model.hidden_size(), 2);
                    gate.wait();
                    let at = Instant::now();
                    // Prefill is excluded: it is one GEMM-shaped step per page
                    // and it scales with threads, so leaving it in would let a
                    // layout's thread count flatter or punish its decode rate.
                    let mut first = None;
                    model
                        .decode(
                            &prompt,
                            steps,
                            |_| false,
                            |_| {
                                first.get_or_insert_with(Instant::now);
                            },
                        )
                        .unwrap();
                    at.elapsed() - first.unwrap().duration_since(at)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let wall = started.elapsed();
    let slowest = elapsed.iter().max().copied().unwrap_or_default();
    eprintln!(
        "      slowest stream decodes in {:?}, wall {:?}, rss {}",
        slowest,
        wall,
        rss()
    );
    Ok((streams * (steps - 1)) as f64 / slowest.as_secs_f64())
}

/// Resident size of this process, as the kernel reports it.
fn rss() -> String {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output();
    match out {
        Ok(out) => {
            let kb: u64 = String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .unwrap_or(0);
            format!("{:.1} GB", kb as f64 / 1024.0 / 1024.0)
        }
        Err(_) => "?".into(),
    }
}

/// One whole page per stream — encode, prefill, then `steps` decode steps —
/// with every stream running at once. The component timings come back under
/// contention, which is the only way they compose into a real page rate.
fn pages(
    dir: &Path,
    n: usize,
    threads: usize,
    steps: usize,
    chunk: usize,
) -> anyhow::Result<(Duration, Duration, Duration, f64)> {
    let models: Vec<OnnxVlm> = (0..n)
        .map(|_| OnnxVlm::load(dir, VISION_GRAPH, EMBED_GRAPH, DECODER_GRAPH, threads))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let gate = Arc::new(Barrier::new(n));
    let (parts, peak) = peak_rss(|| {
        std::thread::scope(|scope| {
            let handles: Vec<_> = models
                .into_iter()
                .map(|mut model| {
                    let gate = Arc::clone(&gate);
                    scope.spawn(move || {
                        let pixels = fill(TILES * 3 * SIDE * SIDE, 1);
                        let prompt = fill(PROMPT_LEN * model.hidden_size(), 2);
                        gate.wait();
                        let at = Instant::now();
                        std::hint::black_box(encode_chunked(&mut model, &pixels, chunk).unwrap());
                        let encode = at.elapsed();
                        let at = Instant::now();
                        let mut first = None;
                        model
                            .decode(
                                &prompt,
                                steps,
                                |_| false,
                                |_| {
                                    first.get_or_insert_with(Instant::now);
                                },
                            )
                            .unwrap();
                        let after = at.elapsed();
                        let prefill = first.unwrap().duration_since(at);
                        (encode, prefill, after - prefill)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        })
    });
    let worst = |pick: fn(&(Duration, Duration, Duration)) -> Duration| {
        parts.iter().map(pick).max().unwrap_or_default()
    };
    Ok((worst(|p| p.0), worst(|p| p.1), worst(|p| p.2), peak))
}

/// Peak resident size while `body` runs, sampled from a watcher thread.
fn peak_rss<T>(body: impl FnOnce() -> T) -> (T, f64) {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let peak = Arc::new(std::sync::Mutex::new(0f64));
    let (s, p) = (Arc::clone(&stop), Arc::clone(&peak));
    let watcher = std::thread::spawn(move || {
        while !s.load(std::sync::atomic::Ordering::Relaxed) {
            let now = rss_gb();
            let mut peak = p.lock().unwrap();
            if now > *peak {
                *peak = now;
            }
            drop(peak);
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let out = body();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    watcher.join().unwrap();
    let peak = *peak.lock().unwrap();
    (out, peak)
}

fn rss_gb() -> f64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u64>()
                .ok()
        })
        .map(|kb| kb as f64 / 1024.0 / 1024.0)
        .unwrap_or(0.0)
}

/// Encode a page's tiles in groups of `chunk` rather than all at once.
///
/// Lossless by construction *if* the encoder treats tiles independently,
/// which is the thing this mode exists to check rather than assume: the
/// features from the chunked calls are compared against the whole-page call
/// before any timing is reported.
fn encode_chunked(model: &mut OnnxVlm, pixels: &[f32], chunk: usize) -> anyhow::Result<Vec<f32>> {
    let per_tile = 3 * SIDE * SIDE;
    let mut out = Vec::new();
    let mut done = 0usize;
    while done < TILES {
        let take = chunk.min(TILES - done);
        let slice = &pixels[done * per_tile..(done + take) * per_tile];
        out.extend_from_slice(&model.encode_image(slice, take, SIDE)?);
        done += take;
    }
    Ok(out)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let model_dir = args
        .first()
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: bench_recognizer <model_dir> <mode> <spec>"))?;
    let dir = install_dir(&model_dir);
    anyhow::ensure!(dir.is_dir(), "no recognizer under {}", dir.display());
    let mode = args.get(1).map(String::as_str).unwrap_or("stages");
    let spec = args.get(2).map(String::as_str).unwrap_or("");
    let steps: usize = std::env::var("BENCH_STEPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);

    println!("cores: {:?}", std::thread::available_parallelism());
    println!("model: {}", dir.display());
    println!("prompt {PROMPT_LEN} tokens, {TILES} tiles, {steps} decode steps\n");

    match mode {
        "stages" => {
            println!(
                "{:>7}  {:>8}  {:>9}  {:>9}  {:>10}  {:>9}",
                "threads", "load", "encode", "prefill", "per-token", "1k tok"
            );
            for threads in spec.split(',').filter(|s| !s.is_empty()) {
                let threads: usize = threads.parse()?;
                let s = measure(&dir, threads, steps)?;
                println!(
                    "{:>7}  {:>8.2?}  {:>9.2?}  {:>9.2?}  {:>10.2?}  {:>8.1}s",
                    threads,
                    s.load,
                    s.encode,
                    s.prefill,
                    s.per_token(),
                    s.per_token().as_secs_f64() * 1000.0
                );
            }
        }
        "streams" => {
            println!("{:>10}  {:>14}  {:>12}", "layout", "tokens/s total", "rss");
            for item in spec.split(',').filter(|s| !s.is_empty()) {
                let (n, t) = item
                    .split_once('x')
                    .ok_or_else(|| anyhow::anyhow!("spec is NxT, got {item}"))?;
                let (n, t): (usize, usize) = (n.parse()?, t.parse()?);
                let rate = streams(&dir, n, t, steps)?;
                println!("{:>10}  {:>14.2}", format!("{n}x{t}"), rate);
            }
        }
        "pages" => {
            // A page's real token count is a property of the document, not of
            // the machine; this is the mid-range the components are composed
            // at, and the per-token column lets any other budget be read off.
            let budget: usize = std::env::var("BENCH_PAGE_TOKENS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(800);
            let chunk: usize = std::env::var("BENCH_TILE_CHUNK")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(TILES);
            println!("tile chunk: {chunk}");
            println!(
                "{:>8}  {:>9}  {:>9}  {:>10}  {:>10}  {:>9}  {:>10}",
                "layout", "encode", "prefill", "per-token", "page", "peak rss", "pages/h"
            );
            for item in spec.split(',').filter(|s| !s.is_empty()) {
                let (n, t) = item
                    .split_once('x')
                    .ok_or_else(|| anyhow::anyhow!("spec is NxT, got {item}"))?;
                let (n, t): (usize, usize) = (n.parse()?, t.parse()?);
                let (encode, prefill, steady, peak) = pages(&dir, n, t, steps, chunk)?;
                let per_token = steady / (steps - 1) as u32;
                let page = encode + prefill + per_token * budget as u32;
                println!(
                    "{:>8}  {:>9.2?}  {:>9.2?}  {:>10.2?}  {:>10.2?}  {:>6.2} GB  {:>10.1}",
                    format!("{n}x{t}"),
                    encode,
                    prefill,
                    per_token,
                    page,
                    peak,
                    n as f64 * 3600.0 / page.as_secs_f64()
                );
            }
        }
        "tiles" => {
            let mut model = OnnxVlm::load(&dir, VISION_GRAPH, EMBED_GRAPH, DECODER_GRAPH, 9)?;
            let pixels = fill(TILES * 3 * SIDE * SIDE, 1);
            // ORT's arena never gives memory back, so a whole-page reference
            // call sets a high-water mark every later chunk size then inherits.
            // Measuring peak RSS honestly means one fresh process per size.
            let want_ref = std::env::var("BENCH_NO_REF").is_err();
            let reference = if want_ref {
                model.encode_image(&pixels, TILES, SIDE)?
            } else {
                Vec::new()
            };
            println!(
                "{:>6}  {:>9}  {:>10}  {:>12}  {:>10}  {:>11}",
                "chunk", "encode", "peak rss", "max |diff|", "max |ref|", "rel RMS"
            );
            for item in spec.split(',').filter(|s| !s.is_empty()) {
                let chunk: usize = item.parse()?;
                let ((features, elapsed), peak) = peak_rss(|| {
                    let at = Instant::now();
                    let f = encode_chunked(&mut model, &pixels, chunk).unwrap();
                    (f, at.elapsed())
                });
                let drift = if want_ref {
                    anyhow::ensure!(
                        features.len() == reference.len(),
                        "chunking changed the feature count"
                    );
                    features
                        .iter()
                        .zip(&reference)
                        .map(|(a, b)| (a - b).abs())
                        .fold(0f32, f32::max)
                } else {
                    f32::NAN
                };
                let scale = reference.iter().map(|v| v.abs()).fold(0f32, f32::max);
                let rms = (reference
                    .iter()
                    .zip(&features)
                    .map(|(a, b)| ((a - b) as f64).powi(2))
                    .sum::<f64>()
                    / reference.len().max(1) as f64)
                    .sqrt();
                let rms_ref = (reference.iter().map(|a| (*a as f64).powi(2)).sum::<f64>()
                    / reference.len().max(1) as f64)
                    .sqrt();
                println!(
                    "{:>6}  {:>9.2?}  {:>8.2} GB  {:>12.2e}  {:>10.2e}  {:>10.3}%",
                    chunk,
                    elapsed,
                    peak,
                    drift,
                    scale,
                    100.0 * rms / rms_ref.max(f64::MIN_POSITIVE)
                );
            }
        }
        "dump" => {
            // Write the library's own encode_image output for a full page, so
            // the same page can be encoded under two ENCODE_TILE_GROUP widths
            // and the raw bytes compared. Nothing here re-implements the
            // chunking: that is the thing under test.
            let mut model = OnnxVlm::load(&dir, VISION_GRAPH, EMBED_GRAPH, DECODER_GRAPH, 9)?;
            let pixels = fill(TILES * 3 * SIDE * SIDE, 1);
            let features = model.encode_image(&pixels, TILES, SIDE)?;
            let bytes: Vec<u8> = features.iter().flat_map(|f| f.to_le_bytes()).collect();
            std::fs::write(spec, &bytes)?;
            println!("wrote {} features to {spec}", features.len());
        }
        "batch" => {
            // A whole document's images through the real engine, so the pool
            // is exercised as production exercises it. The digest is over
            // every region's kind, box and text: two layouts that agree on it
            // read the page the same way.
            let count: usize = std::env::var("BENCH_IMAGES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(4);
            // A real page, so the decode has something to read and the
            // digest is a reading rather than an empty result.
            let page = std::env::var("BENCH_PAGE")
                .map_err(|_| anyhow::anyhow!("set BENCH_PAGE to a page image"))?;
            let page = image::open(&page)?.to_rgb8();
            let images: Vec<image::RgbImage> = (0..count).map(|_| page.clone()).collect();
            println!(
                "{:>9}  {:>10}  {:>9}  {:>20}",
                "layout", "elapsed", "peak rss", "digest"
            );
            for item in spec.split(',').filter(|s| !s.is_empty()) {
                let (r, t) = item
                    .split_once('x')
                    .ok_or_else(|| anyhow::anyhow!("spec is RxT, got {item}"))?;
                let (r, t): (usize, usize) = (r.parse()?, t.parse()?);
                let engine = GraniteDocling::load(&model_dir, r, t)?;
                let ((out, elapsed), peak) = peak_rss(|| {
                    let at = Instant::now();
                    let out = engine.spot_batch(&images);
                    (out, at.elapsed())
                });
                let out = out?;
                let mut digest: u64 = 0xcbf29ce484222325;
                for recognition in &out {
                    for region in &recognition.regions {
                        let mut eat = |bytes: &[u8]| {
                            for b in bytes {
                                digest ^= *b as u64;
                                digest = digest.wrapping_mul(0x100000001b3);
                            }
                        };
                        eat(format!("{:?}", region.kind).as_bytes());
                        eat(region.text.as_bytes());
                        eat(&region.confidence.to_le_bytes());
                        for point in &region.quad {
                            eat(&point.x.to_le_bytes());
                            eat(&point.y.to_le_bytes());
                        }
                    }
                }
                println!(
                    "{:>9}  {:>10.2?}  {:>6.2} GB  {:>20x}",
                    format!("{r}x{t}"),
                    elapsed,
                    peak,
                    digest
                );
            }
        }
        other => anyhow::bail!("unknown mode {other}"),
    }
    Ok(())
}
