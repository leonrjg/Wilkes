//! What recognizers exist, and the one place that says so.
//!
//! Recognition is dispatched the way embedding and generation are: a role
//! carries an engine, the engine plus a model id names a recognizer, and the
//! worker subprocess loads it without knowing which one it is going to get.
//! Nothing here mentions a checkpoint by name outside the arm that owns it, so
//! a second engine — a Python recognizer behind the sidecar, say — is a
//! variant and two match arms rather than a new path through the code.
//!
//! The split between what needs the weights and what does not is the point of
//! this module. `load_recognizer_local` needs 1.9 GB and a device and must run
//! in the worker. `identity` and `admission_threshold` are derived from
//! constants, are needed by the host to write the extraction recipe, and must
//! never require a model to answer — the recipe describes what the reading was
//! produced under, and the process that owns the recipe is not the process
//! holding the weights.

use std::path::Path;

use super::ocr::OcrEngine;

/// A way of recognizing the text drawn inside an image.
///
/// One variant today. It is an enum rather than an implied constant because
/// the worker protocol carries it, and a protocol that cannot name a second
/// engine is one that has to change shape to gain one.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize,
)]
pub enum RecognitionEngine {
    /// PaddleOCR-VL under candle, in this process.
    #[default]
    #[serde(alias = "candle")]
    Candle,
}

impl RecognitionEngine {
    pub fn as_str(&self) -> &'static str {
        match self {
            RecognitionEngine::Candle => "candle",
        }
    }
}

/// The model id the shipped recipe names for `engine`.
pub fn shipped_model_id(engine: RecognitionEngine) -> &'static str {
    match engine {
        #[cfg(feature = "candle")]
        RecognitionEngine::Candle => super::paddleocr_vl::SHIPPED_CHECKPOINT.name,
        #[cfg(not(feature = "candle"))]
        RecognitionEngine::Candle => "",
    }
}

/// The recipe string a recognizer of this engine and model reads documents
/// under, answered without loading it.
pub fn identity(engine: RecognitionEngine, model_id: &str) -> anyhow::Result<String> {
    match engine {
        #[cfg(feature = "candle")]
        RecognitionEngine::Candle => Ok(super::paddleocr_vl::identity_of(&checkpoint(model_id)?)),
        #[cfg(not(feature = "candle"))]
        RecognitionEngine::Candle => {
            let _ = model_id;
            anyhow::bail!("the candle recognizer is not compiled into this build")
        }
    }
}

/// The confidence a region of this engine's output must reach to enter the
/// reading. Part of the identity above, and needed by the host for the same
/// reason: admission happens where the geometry does.
pub fn admission_threshold(engine: RecognitionEngine, model_id: &str) -> anyhow::Result<f32> {
    match engine {
        #[cfg(feature = "candle")]
        RecognitionEngine::Candle => {
            checkpoint(model_id)?;
            Ok(super::paddleocr_vl::ADMISSION_THRESHOLD)
        }
        #[cfg(not(feature = "candle"))]
        RecognitionEngine::Candle => {
            let _ = model_id;
            anyhow::bail!("the candle recognizer is not compiled into this build")
        }
    }
}

/// Whether this engine's weights for `model_id` are on disk and intact.
///
/// Asked by the host before it attaches an analyzer, not by the worker before
/// it loads one. Attaching is now cheap and no longer discovers a missing
/// checkpoint by failing to load it, so without this a library would be read
/// under a recipe that claims enrichment while every image quietly failed —
/// which is the exact outcome the recipe exists to make impossible.
pub fn installed(
    engine: RecognitionEngine,
    model_id: &str,
    model_dir: &Path,
) -> anyhow::Result<bool> {
    match engine {
        #[cfg(feature = "candle")]
        RecognitionEngine::Candle => Ok(super::paddleocr_vl::is_installed(
            model_dir,
            &checkpoint(model_id)?,
        )),
        #[cfg(not(feature = "candle"))]
        RecognitionEngine::Candle => {
            let (_, _) = (model_id, model_dir);
            anyhow::bail!("the candle recognizer is not compiled into this build")
        }
    }
}

/// Load the recognizer in the calling process.
///
/// Must only be called from a worker subprocess. The recognizer is candle
/// inference against 1.9 GB of weights on whatever accelerator the device
/// string resolves to, and a fault in it takes down the process it is in —
/// which is the whole reason recognition is addressed over a worker protocol
/// rather than called directly.
pub fn load_recognizer_local(
    engine: RecognitionEngine,
    model_id: &str,
    model_dir: &Path,
    device: &str,
) -> anyhow::Result<Box<dyn OcrEngine>> {
    match engine {
        #[cfg(feature = "candle")]
        RecognitionEngine::Candle => Ok(Box::new(super::paddleocr_vl::PaddleOcrVl::load(
            model_dir,
            checkpoint(model_id)?,
            device,
        )?)),
        #[cfg(not(feature = "candle"))]
        RecognitionEngine::Candle => {
            let (_, _, _) = (model_id, model_dir, device);
            anyhow::bail!("the candle recognizer is not compiled into this build")
        }
    }
}

/// The checkpoint a model id names, or an error naming what is on offer.
///
/// An unknown id is an error rather than a fall back to the shipped
/// checkpoint: reading a library under a recognizer nobody asked for is
/// indistinguishable, afterwards, from reading it under the one they did.
#[cfg(feature = "candle")]
fn checkpoint(model_id: &str) -> anyhow::Result<super::paddleocr_vl::Checkpoint> {
    super::paddleocr_vl::CHECKPOINTS
        .iter()
        .find(|checkpoint| checkpoint.name == model_id)
        .copied()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown recognizer '{model_id}'; this build ships {}",
                super::paddleocr_vl::CHECKPOINTS
                    .iter()
                    .map(|checkpoint| checkpoint.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "candle")]
    #[test]
    fn the_recipe_strings_are_answerable_without_the_weights() {
        let shipped = shipped_model_id(RecognitionEngine::Candle);
        assert!(!shipped.is_empty());
        assert!(identity(RecognitionEngine::Candle, shipped)
            .unwrap()
            .contains(shipped));
        assert_eq!(
            admission_threshold(RecognitionEngine::Candle, shipped).unwrap(),
            super::super::paddleocr_vl::ADMISSION_THRESHOLD
        );
    }

    /// A model id nobody ships is refused rather than quietly answered with
    /// the shipped one, which would put a reading into the index under a
    /// recipe that never produced it.
    #[cfg(feature = "candle")]
    #[test]
    fn an_unknown_recognizer_is_an_error_not_a_fallback() {
        let error = identity(RecognitionEngine::Candle, "no-such-recognizer").unwrap_err();
        assert!(
            error.to_string().contains("no-such-recognizer"),
            "the error should name what was asked for: {error}"
        );
        assert!(load_recognizer_local(
            RecognitionEngine::Candle,
            "no-such-recognizer",
            Path::new("/nonexistent"),
            "cpu"
        )
        .is_err());
    }

    #[cfg(feature = "candle")]
    #[test]
    fn two_checkpoints_read_documents_under_two_recipes() {
        let names: Vec<&str> = super::super::paddleocr_vl::CHECKPOINTS
            .iter()
            .map(|checkpoint| checkpoint.name)
            .collect();
        assert!(names.len() >= 2, "the fixture assumes more than one");
        assert_ne!(
            identity(RecognitionEngine::Candle, names[0]).unwrap(),
            identity(RecognitionEngine::Candle, names[1]).unwrap()
        );
    }
}
