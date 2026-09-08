//! PP-FormulaNet_plus-S, exported by OAR OCR, under the existing ONNX worker.
//!
//! The graph contains the autoregressive loop. One invocation reads one crop;
//! the document's crop loop stays in the worker and dies with that process.
//! The host can answer every catalogue question below without loading weights.
//! Artifacts are pinned by digest; the export supplies no source-commit metadata.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use image::{imageops, RgbImage};
use ort::{session::Session, value::Tensor};
use tokenizers::Tokenizer;

use super::ocr::{ImageRecognition, OcrEngine, RegionKind, SpottedRegion};
use crate::types::Point;

pub const MODEL_ID: &str = "pp-formulanet-plus-s";
pub const GRAPH: &str = "pp-formulanet_plus-s.onnx";
pub const TOKENIZER: &str = "pp-formulanet-tokenizer.json";
pub const SIDE: u32 = 384;
pub const ADMISSION_THRESHOLD: f32 = 0.0;
const VOCAB_SIZE: usize = 50_000;
const ARTIFACTS: &[(&str, u64, &str, &str)] = &[
    (GRAPH, 231_878_904, "449d205c8fb2fe0a9b134a5e4a0f2421c2e7812fd902ea67dfda4e9ef4588978",
     "https://github.com/GreatV/oar-ocr/releases/download/v0.3.0/pp-formulanet_plus-s.onnx"),
    (TOKENIZER, 2_140_014, "2811d82701ec97c192fa256aa2b4516929373870ae660326cc5b1dc879b95ff2",
     "https://www.modelscope.cn/models/greatv/oar-ocr/resolve/master/pp-formulanet-tokenizer.json"),
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
            "Tokenizer matches PaddlePaddle's published inference configuration".to_string(),
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

/// OAR's crop/fit/black-pad recipe. Use RGB luminance explicitly: `image`'s
/// generic luma conversion uses different coefficients from Pillow/OpenCV.
/// Resample directly to the fitted size, as OAR does, rather than allocating
/// Paddle's intermediate image with its short edge enlarged to 384 pixels.
/// This resampling choice is versioned in the extraction identity.
pub fn preprocess(crop: &RgbImage) -> Result<Vec<f32>> {
    anyhow::ensure!(crop.width() > 0 && crop.height() > 0, "empty formula image");
    let luma = |p: &image::Rgb<u8>| -> u8 {
        (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).round() as u8
    };
    let (mut low, mut high) = (u8::MAX, u8::MIN);
    for p in crop.pixels() {
        let v = luma(p);
        low = low.min(v);
        high = high.max(v);
    }
    let mut bounds = (crop.width(), crop.height(), 0, 0);
    if high > low {
        for (x, y, p) in crop.enumerate_pixels() {
            if (luma(p) - low) as f32 * 255.0 / ((high - low) as f32) < 200.0 {
                bounds.0 = bounds.0.min(x);
                bounds.1 = bounds.1.min(y);
                bounds.2 = bounds.2.max(x);
                bounds.3 = bounds.3.max(y);
            }
        }
    }
    let cropped = if bounds.0 <= bounds.2 && bounds.1 <= bounds.3 {
        imageops::crop_imm(
            crop,
            bounds.0,
            bounds.1,
            bounds.2 - bounds.0 + 1,
            bounds.3 - bounds.1 + 1,
        )
        .to_image()
    } else {
        crop.clone()
    };
    let scale = SIDE as f64 / cropped.width().max(cropped.height()) as f64;
    let w = ((cropped.width() as f64 * scale) as u32).clamp(1, SIDE);
    let h = ((cropped.height() as f64 * scale) as u32).clamp(1, SIDE);
    let resized = imageops::resize(&cropped, w, h, imageops::FilterType::Triangle);
    let mut padded = RgbImage::new(SIDE, SIDE);
    imageops::overlay(
        &mut padded,
        &resized,
        ((SIDE - w) / 2) as i64,
        ((SIDE - h) / 2) as i64,
    );
    Ok(padded
        .pixels()
        .map(|p| {
            let gray = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) / 255.0;
            (gray - 0.7931) / 0.1738
        })
        .collect())
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
