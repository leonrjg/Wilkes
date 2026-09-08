//! PP-FormulaNet_plus-S, exported by OAR OCR, under the existing ONNX worker.
//!
//! The graph contains the autoregressive loop. One invocation reads one crop;
//! the document's crop loop stays in the worker and dies with that process.
//! The host can answer every catalogue question below without loading weights.
//! Artifacts are pinned by digest; the export supplies no source-commit metadata.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use image::RgbImage;
use ort::{session::Session, value::Tensor};
use tokenizers::Tokenizer;

use super::ocr::{ImageRecognition, OcrEngine, RegionKind, SpottedRegion};
use crate::types::Point;

pub const MODEL_ID: &str = "pp-formulanet-plus-s";
pub const GRAPH: &str = "pp-formulanet_plus-s.onnx";
pub const TOKENIZER: &str = "tokenizer.json";
pub const SIDE: u32 = 384;
pub const ADMISSION_THRESHOLD: f32 = 0.0;
const VOCAB_SIZE: usize = 50_000;
/// Filename, size, SHA-256 and where each is fetched from.
///
/// The vocabulary is **UniMERNet's**, taken from UniMERNet. OAR republishes it
/// as `pp-formulanet-tokenizer.json` on ModelScope, and that file is this file
/// with a newline appended: 2,140,014 bytes against 2,140,013, the same 50,000
/// merges, the same ids, the same `</s>` at 2, identical once parsed. So there
/// is one vocabulary here and two places it is spelled, and the reasons to
/// pin this one are that it is the original rather than a copy of it, that
/// UniMERNet's Apache-2.0 is the licence PP-FormulaNet is already disclosed
/// under, and that it is served from the host the other recognizers are
/// fetched from — ModelScope has answered 403 for this path for at least one
/// user, and while it serves the file when asked from here, a second host in
/// the install path is a second thing that has to be reachable.
///
/// Not Texify's copy of the same bytes, though it is byte-identical and
/// already on disk when both readers are installed: that copy is disclosed
/// under `vikp/texify`'s CC-BY-SA-4.0, and reaching for it would put those
/// terms on an Apache-2.0 row to save two megabytes.
const ARTIFACTS: &[(&str, u64, &str, &str)] = &[
    (GRAPH, 231_878_904, "449d205c8fb2fe0a9b134a5e4a0f2421c2e7812fd902ea67dfda4e9ef4588978",
     "https://github.com/GreatV/oar-ocr/releases/download/v0.3.0/pp-formulanet_plus-s.onnx"),
    // A commit and not a branch. The digest below is what actually holds the
    // vocabulary still — a moved branch fails verification rather than
    // changing what stored readings mean — but a commit fails by not being
    // found, which is the failure that says what happened.
    (TOKENIZER, 2_140_013, "02c318d9cfa95bf323371762b8f838a82709530274d36dba6eca880f0add6cc4",
     "https://huggingface.co/wanderkid/unimernet/resolve/4be874ffb637a90929de479109c8a377689b5a09/tokenizer.json"),
];

pub fn identity() -> String {
    format!(
        "ort-2.0.0-rc.13+{MODEL_ID}+{}+{}+crop200-fit384-triangle-blackpad-gray-v1+raw-bpe-eos2",
        ARTIFACTS[0].2, ARTIFACTS[1].2
    )
}

pub fn footprint_bytes() -> u64 {
    ARTIFACTS.iter().map(|a| a.1).sum()
}
pub fn install_dir(model_dir: &Path) -> PathBuf {
    model_dir.join("recognizers").join(MODEL_ID)
}
pub fn is_installed(model_dir: &Path) -> bool {
    let dir = install_dir(model_dir);
    ARTIFACTS.iter().all(|(name, size, _, _)| {
        dir.join(name)
            .metadata()
            .map(|m| m.is_file() && m.len() == *size)
            .unwrap_or(false)
    })
}

pub fn inventory() -> crate::types::RecognizerInventory {
    crate::types::RecognizerInventory {
        name: MODEL_ID.to_string(),
        repo: "https://github.com/GreatV/oar-ocr/releases/tag/v0.3.0".to_string(),
        revision: format!("sha256:{}", ARTIFACTS[0].2),
        license: "Apache-2.0".to_string(),
        license_url: "https://huggingface.co/PaddlePaddle/PP-FormulaNet_plus-S".to_string(),
        derived_from: vec![
            "PP-FormulaNet_plus-S by PaddlePaddle (Apache-2.0)".to_string(),
            "ONNX export published by OAR OCR; source checkpoint and exporter version unspecified"
                .to_string(),
            "Vocabulary from UniMERNet (Apache-2.0, wanderkid/unimernet), which is \
             what OAR republishes for this model and parses identically to it"
                .to_string(),
        ],
        artifacts: ARTIFACTS
            .iter()
            .map(|(name, size, hash, _)| crate::types::InventoriedArtifact {
                filename: (*name).to_string(),
                size_bytes: *size,
                sha256: (*hash).to_string(),
            })
            .collect(),
        footprint_bytes: footprint_bytes(),
    }
}

pub fn install(
    model_dir: &Path,
    progress: Option<crate::models::progress::ProgressTx>,
) -> Result<()> {
    let dir = install_dir(model_dir);
    std::fs::create_dir_all(&dir)?;
    for (name, size, hash, url) in ARTIFACTS {
        let target = dir.join(name);
        if super::verify_artifact(&target, *size, hash).is_ok() {
            continue;
        }
        // Only publish a verified download, so a partial transfer is never installed.
        let staged = tempfile::NamedTempFile::new_in(&dir)?;
        crate::models::downloader::LocalModelManager::download(
            url,
            staged.path(),
            *size,
            progress.clone(),
        )?;
        super::verify_artifact(staged.path(), *size, hash)?;
        staged
            .persist(&target)
            .with_context(|| format!("could not install {name}"))?;
    }
    Ok(())
}

/// UniMERNet's crop/fit/black-pad recipe, at this export's square frame.
///
/// The transform itself is [`super::unimernet::fit_and_gray`] and is not
/// repeated here: PP-FormulaNet is PaddlePaddle's re-implementation of
/// UniMERNet and inherited its preprocessing whole — the same threshold at
/// 200, the same black padding, the same 0.7931/0.1738 grey statistics. What
/// is this export's own is the frame, which is square where UniMERNet's is
/// wide. Two copies of one recipe would be two places for it to drift, and
/// both readers name it in their identity, so a drift in one copy would
/// silently change what one of their stored readings means.
///
/// `pub` so a probe that measured the encoder against its own resize would be
/// measuring a model nobody runs. Pure, and holds no state.
pub fn preprocess(crop: &RgbImage) -> Result<Vec<f32>> {
    super::unimernet::fit_and_gray(crop, SIDE, SIDE)
}

/// End at the first EOS. Missing EOS means an unfinished decode even when the
/// text parses. Reject invalid ids rather than hiding a mismatched vocabulary.
fn decode(tokenizer: &Tokenizer, ids: &[i64]) -> Result<(String, bool)> {
    let end = ids.iter().position(|id| *id == 2);
    let mut tokens = Vec::new();
    for id in ids.iter().take(end.unwrap_or(ids.len())) {
        anyhow::ensure!(
            *id >= 0 && (*id as usize) < VOCAB_SIZE,
            "invalid formula token id {id}"
        );
        tokens.push(*id as u32);
    }
    let text = tokenizer
        .decode(&tokens, true)
        .map_err(|e| anyhow::anyhow!("formula token decode failed: {e}"))?;
    // Preserve LaTeX, including spaces in text commands, rather than applying
    // upstream's display-only whitespace/Unicode rewriting to indexed content.
    Ok((text.trim().to_string(), end.is_none()))
}

pub struct PpFormulaNet {
    dir: PathBuf,
    threads: usize,
    readers: Vec<Mutex<Option<Session>>>,
    tokenizer: Tokenizer,
}

impl PpFormulaNet {
    /// Called only by the worker dispatch (or a standalone model probe).
    pub fn load(model_dir: &Path, readers: usize, threads: usize) -> Result<Self> {
        anyhow::ensure!(
            readers > 0 && threads > 0,
            "formula reader needs positive concurrency"
        );
        let dir = install_dir(model_dir);
        for (name, size, hash, _) in ARTIFACTS {
            super::verify_artifact(&dir.join(name), *size, hash)?;
        }
        let tokenizer = Tokenizer::from_file(dir.join(TOKENIZER))
            .map_err(|e| anyhow::anyhow!("could not load formula tokenizer: {e}"))?;
        anyhow::ensure!(
            tokenizer.get_vocab_size(true) == VOCAB_SIZE
                && tokenizer.token_to_id("</s>") == Some(2),
            "unexpected formula vocabulary"
        );
        Ok(Self {
            dir,
            threads,
            readers: (0..readers).map(|_| Mutex::new(None)).collect(),
            tokenizer,
        })
    }

    fn open(&self) -> Result<Session> {
        Ok(Session::builder()?
            .with_intra_threads(self.threads)
            .map_err(|error| anyhow::anyhow!("could not configure formula session: {error}"))?
            .commit_from_file(self.dir.join(GRAPH))?)
    }
}

impl OcrEngine for PpFormulaNet {
    fn identity(&self) -> String {
        identity()
    }
    fn admission_threshold(&self) -> f32 {
        ADMISSION_THRESHOLD
    }
    fn spot_batch(&self, images: &[RgbImage]) -> Result<Vec<ImageRecognition>> {
        super::granite_docling::in_parallel(images, self.readers.len(), |hand, image| {
            let pixels = preprocess(image)?;
            let mut held = self.readers[hand]
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if held.is_none() {
                *held = Some(self.open()?);
            }
            let session = held.as_mut().expect("just loaded");
            let outputs = session.run(ort::inputs!["x" => Tensor::from_array(([1usize, 1, SIDE as usize, SIDE as usize], pixels))?])?;
            let (shape, ids) = outputs["fetch_name_0"].try_extract_tensor::<i64>()?;
            anyhow::ensure!(
                shape.len() == 2 && shape[0] == 1,
                "unexpected formula output shape {shape:?}"
            );
            let (text, truncated) = decode(&self.tokenizer, ids)?;
            if text.is_empty() && !truncated {
                return Ok(ImageRecognition {
                    regions: Vec::new(),
                    unroutable: 0,
                    not_text: 1,
                });
            }
            Ok(ImageRecognition::from_regions(vec![SpottedRegion {
                kind: RegionKind::Formula,
                text,
                // The graph emits ids, not calibrated probabilities. Formula
                // admission uses LaTeX validity and completion, never this score.
                confidence: 0.0,
                truncated,
                structure: None,
                quad: [
                    Point { x: 0.0, y: 0.0 },
                    Point { x: 1.0, y: 0.0 },
                    Point { x: 1.0, y: 1.0 },
                    Point { x: 0.0, y: 1.0 },
                ],
            }]))
        })
    }
    fn release(&self) {
        for reader in &self.readers {
            reader
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::imageops;
    /// Each artifact is fetched from somewhere that cannot move under it: the
    /// graph from a release asset, the vocabulary from a commit. A branch
    /// would let a re-export change what every stored reading means while the
    /// digest here still described the file it replaced.
    #[test]
    fn both_artifacts_are_pinned_to_something_that_cannot_move() {
        let revision = ARTIFACTS[1]
            .3
            .split("/resolve/")
            .nth(1)
            .and_then(|tail| tail.split('/').next())
            .expect("the vocabulary is fetched through a /resolve/<revision>/ path");
        assert_eq!(revision.len(), 40, "a branch, not a commit: {revision}");
        assert!(revision.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(ARTIFACTS[0].3.contains("/releases/download/v0.3.0/"));
        for (name, size, hash, url) in ARTIFACTS {
            assert_eq!(hash.len(), 64, "{name} is not pinned by digest");
            assert!(*size > 0, "{name} has no size to verify against");
            assert!(url.starts_with("https://"), "{name} is fetched over {url}");
        }
        // Both digests are in the identity, so a copy-paste that made them
        // equal would make two different files describe one reading.
        assert_ne!(ARTIFACTS[0].2, ARTIFACTS[1].2);
        assert!(identity().contains(ARTIFACTS[0].2) && identity().contains(ARTIFACTS[1].2));
    }

    #[test]
    fn preprocessing_handles_uniform_thin_and_empty_images() {
        assert!(preprocess(&RgbImage::new(0, 1)).is_err());
        let white = RgbImage::from_pixel(20, 20, image::Rgb([255; 3]));
        let tensor = preprocess(&white).unwrap();
        assert_eq!(tensor.len(), (SIDE * SIDE) as usize);
        assert!(tensor
            .iter()
            .all(|x| (*x - (1.0 - 0.7931) / 0.1738).abs() < 1e-5));
        let thin = RgbImage::from_pixel(1, 2000, image::Rgb([255; 3]));
        assert!(preprocess(&thin).unwrap().iter().all(|x| x.is_finite()));
    }
    #[test]
    fn margins_do_not_change_the_fitted_expression() {
        let mut ink = RgbImage::from_pixel(12, 6, image::Rgb([255; 3]));
        for x in 0..12 {
            ink.put_pixel(x, 0, image::Rgb([0; 3]));
            ink.put_pixel(x, 5, image::Rgb([0; 3]));
        }
        let mut margins = RgbImage::from_pixel(40, 30, image::Rgb([255; 3]));
        imageops::overlay(&mut margins, &ink, 7, 9);
        assert_eq!(preprocess(&ink).unwrap(), preprocess(&margins).unwrap());
    }
    #[test]
    fn decoding_distinguishes_eos_truncation_and_invalid_ids() {
        let tokenizer = Tokenizer::new(tokenizers::models::bpe::BPE::default());
        assert!(!decode(&tokenizer, &[2, -1]).unwrap().1);
        assert!(decode(&tokenizer, &[]).unwrap().1);
        assert!(decode(&tokenizer, &[-1, 2]).is_err());
        assert!(decode(&tokenizer, &[VOCAB_SIZE as i64, 2]).is_err());
    }
}
