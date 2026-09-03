//! Apple Vision text recognition: the recognizer that ships with the machine.
//!
//! Not a document parser. `VNRecognizeTextRequest` reads lines of text and
//! returns each one with a box and a confidence — it does not delimit tables,
//! it does not emit LaTeX, and it has no notion of a figure. That is the whole
//! of what this engine offers and [`RecognizerDescriptor::emits`] says so:
//! [`RegionKind::Text`], and nothing else.
//!
//! Which is why it is not the default. granite-docling reads a page's prose,
//! formulas and tables in one pass; this reads the prose faster than anything
//! else on the machine and drops the rest on the floor. Choosing it is a
//! decision to trade the structure away, and the picker should present it as
//! that rather than as a speed setting.
//!
//! ## What it costs
//!
//! Measured over 545 images extracted from a seven-book corpus, against
//! granite-docling's ~12.6 s/image on the same machine:
//!
//! | recognizer                 | per image | 545 images |
//! | -------------------------- | --------- | ---------- |
//! | granite-docling 258M, ONNX | ~12,600ms | ~1.9 hours |
//! | Apple Vision, accurate     |   115.6ms | 63 seconds |
//!
//! Zero failures over those 545, mean confidence 0.933, and 89% of the images
//! yielded more than twenty characters. No weights to download, no GPU memory,
//! and about 1.6 cores for the duration.
//!
//! ## Where the work happens
//!
//! In the recognition worker like the others, through
//! [`super::dispatch::load_recognizer_local`]. It needs no isolation of its own
//! — there are no weights to fault on — but a recognizer that ran somewhere
//! else would be one the annotation cache, the diagnostics and the kill path
//! all had to special-case.

use std::ffi::{c_char, CStr};

use anyhow::{Context, Result};
use image::RgbImage;

use super::ocr::{ImageRecognition, OcrEngine, RegionKind, SpottedRegion};
use crate::types::Point;

/// The model id this engine reads under.
///
/// A name rather than a checkpoint: the recognizer is the operating system's,
/// so there is no revision to pin and no file to find. It still needs an id
/// because a reading records what produced it, and "the OS" is not an answer a
/// stored annotation can be re-read against.
pub const MODEL_ID: &str = "apple-vision-text";

/// The confidence a line must reach to enter the reading.
///
/// Vision's confidence is a per-line score from its own recognizer and is not
/// the same quantity as a decoder's mean token probability — 0.30 here and
/// 0.65 in granite-docling are not two positions on one scale. Set low
/// deliberately: over the 545-image corpus the mean was 0.933 and the mass
/// below 0.3 was the lines that were genuinely unreadable, so this rejects
/// noise without trimming the distribution's tail.
pub const ADMISSION_THRESHOLD: f32 = 0.30;

extern "C" {
    fn wilkes_vision_recognize_rgb(
        rgb: *const u8,
        width: usize,
        height: usize,
        error_out: *mut *mut c_char,
    ) -> *mut c_char;
    fn wilkes_vision_string_free(s: *mut c_char);
}

/// A string the shim malloc'd, freed when it goes out of scope.
///
/// Every `char *` that crosses back — result or message — is owned here, so
/// there is one place that frees and no path that forgets.
struct ShimString(*mut c_char);

impl ShimString {
    fn to_str(&self) -> Result<&str> {
        // SAFETY: the shim returns NUL-terminated UTF-8 or NULL, and NULL is
        // rejected by the callers before a `ShimString` is built from it.
        unsafe { CStr::from_ptr(self.0) }
            .to_str()
            .context("the Vision shim returned text that is not UTF-8")
    }
}

impl Drop for ShimString {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from the shim's allocator and is freed once.
        unsafe { wilkes_vision_string_free(self.0) }
    }
}

/// One line, as the shim reports it: Vision's own normalised rect, origin
/// bottom-left.
#[derive(serde::Deserialize)]
struct ShimRegion {
    text: String,
    confidence: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(serde::Deserialize)]
struct ShimResponse {
    regions: Vec<ShimRegion>,
}

/// The recognizer's identity, as it enters the extraction recipe.
///
/// Carries the OS version because the weights are the OS's: a system update can
/// change what this reads without changing anything in this repository, and a
/// reading that did not record the version could not be told apart from one the
/// new recognizer would produce differently.
pub fn identity() -> String {
    format!(
        "apple-vision+{MODEL_ID}+accurate+langcorrect+os-{}+admit-{ADMISSION_THRESHOLD}",
        os_version()
    )
}

fn os_version() -> String {
    std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Always true: the recognizer is part of the operating system.
pub fn is_installed() -> bool {
    true
}

/// What the recognizer is and under what terms.
///
/// No repository, no revision and no artifacts, because there are none: the
/// weights ship with macOS and are covered by its licence rather than by one
/// Wilkes could quote. Stated as empty rather than invented, so a reader of the
/// inventory can tell "nothing to record" from "not recorded".
pub fn inventory() -> crate::types::RecognizerInventory {
    crate::types::RecognizerInventory {
        name: MODEL_ID.to_string(),
        repo: String::new(),
        revision: os_version(),
        license: "Apple macOS system software licence".to_string(),
        license_url: "https://www.apple.com/legal/sla/".to_string(),
        derived_from: vec!["Vision.framework VNRecognizeTextRequest (Apple)".to_string()],
        artifacts: Vec::new(),
        footprint_bytes: footprint_bytes(),
    }
}

/// Nothing on disk that Wilkes put there.
pub fn footprint_bytes() -> u64 {
    0
}

/// Apple Vision, as an [`OcrEngine`].
///
/// Holds nothing. There is no model to keep resident and no cache to warm, so
/// the type exists to carry the trait rather than to own anything.
pub struct AppleVision;

impl AppleVision {
    pub fn load() -> Result<Self> {
        Ok(Self)
    }

    /// Recognize one image, turning the shim's rects into Wilkes' quads.
    fn spot(&self, image: &RgbImage) -> Result<ImageRecognition> {
        let (width, height) = (image.width() as usize, image.height() as usize);
        anyhow::ensure!(
            width > 0 && height > 0,
            "an empty image cannot be recognized"
        );

        let mut error: *mut c_char = std::ptr::null_mut();
        // SAFETY: the buffer is `width * height * 3` packed RGB8 for the life
        // of the call, and the shim neither retains nor frees it.
        let raw = unsafe {
            wilkes_vision_recognize_rgb(image.as_raw().as_ptr(), width, height, &mut error)
        };

        if raw.is_null() {
            let message = if error.is_null() {
                "Vision recognition failed without a message".to_string()
            } else {
                ShimString(error)
                    .to_str()
                    .unwrap_or("Vision recognition failed with an unreadable message")
                    .to_string()
            };
            anyhow::bail!("{message}");
        }

        let json = ShimString(raw);
        let response: ShimResponse = serde_json::from_str(json.to_str()?)
            .context("could not parse the Vision shim's recognition")?;

        let regions = response
            .regions
            .into_iter()
            .map(|region| SpottedRegion {
                // Vision reads lines of text. It reports nothing else, so
                // nothing else may be claimed for what it reports.
                kind: RegionKind::Text,
                text: region.text,
                confidence: region.confidence,
                quad: quad_from_vision_rect(region.x, region.y, region.w, region.h),
                // Apple's framework, not an autoregressive decoder run to a
                // token cap this codebase set — there is no such cap here to
                // hit.
                truncated: false,
                // Lines of text, never a grid filled from the page.
                structure: None,
            })
            .collect();

        // No `unroutable` and no `not_text`: both count things a document
        // parser delimits and this recognizer never does. Reporting a figure it
        // cannot see as a figure it declined to read would be a fiction.
        Ok(ImageRecognition::from_regions(regions))
    }
}

/// Vision's normalised rect to Wilkes' quad.
///
/// Two conversions in one step. Vision's origin is bottom-left and Wilkes'
/// fractions run down from the top, so the vertical axis flips; and the corners
/// come out top-left, top-right, bottom-right, bottom-left, which is the order
/// the other recognizers emit.
fn quad_from_vision_rect(x: f32, y: f32, w: f32, h: f32) -> [Point; 4] {
    let clamp = |v: f32| v.clamp(0.0, 1.0);
    let left = clamp(x);
    let right = clamp(x + w);
    let top = clamp(1.0 - (y + h));
    let bottom = clamp(1.0 - y);
    let at = |x: f32, y: f32| Point { x, y };
    [
        at(left, top),
        at(right, top),
        at(right, bottom),
        at(left, bottom),
    ]
}

impl OcrEngine for AppleVision {
    fn identity(&self) -> String {
        identity()
    }

    fn admission_threshold(&self) -> f32 {
        ADMISSION_THRESHOLD
    }

    fn spot_batch(&self, images: &[RgbImage]) -> Result<Vec<ImageRecognition>> {
        // The loop lives here because the batch is what crosses the worker
        // boundary; Vision itself takes one image at a time.
        images
            .iter()
            .enumerate()
            .map(|(i, image)| {
                self.spot(image)
                    .with_context(|| format!("Vision failed on image {i} of {}", images.len()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rect_becomes_a_quad_with_the_vertical_axis_flipped() {
        // Vision's bottom-left quarter is Wilkes' top-left... upside down: a
        // box at the origin of Vision's space is at the *bottom* of Wilkes'.
        let quad = quad_from_vision_rect(0.0, 0.0, 0.5, 0.5);
        assert_eq!(quad[0], Point { x: 0.0, y: 0.5 });
        assert_eq!(quad[2], Point { x: 0.5, y: 1.0 });
    }

    #[test]
    fn a_full_frame_rect_covers_the_whole_quad() {
        let quad = quad_from_vision_rect(0.0, 0.0, 1.0, 1.0);
        assert_eq!(quad[0], Point { x: 0.0, y: 0.0 });
        assert_eq!(quad[2], Point { x: 1.0, y: 1.0 });
    }

    #[test]
    fn corners_are_ordered_clockwise_from_the_top_left() {
        let quad = quad_from_vision_rect(0.25, 0.25, 0.5, 0.25);
        assert!(quad[1].x > quad[0].x, "top edge runs left to right");
        assert!(quad[3].y > quad[0].y, "left edge runs top to bottom");
        assert_eq!(quad[0].y, quad[1].y, "top edge is level");
        assert_eq!(quad[0].x, quad[3].x, "left edge is vertical");
    }

    #[test]
    fn a_rect_running_past_the_frame_is_clamped() {
        let quad = quad_from_vision_rect(0.9, -0.1, 0.5, 0.4);
        assert_eq!(quad[1].x, 1.0);
        assert_eq!(quad[3].y, 1.0);
    }

    #[test]
    fn the_identity_names_the_recognizer_and_its_threshold() {
        let id = identity();
        assert!(id.contains(MODEL_ID), "{id}");
        assert!(id.contains(&ADMISSION_THRESHOLD.to_string()), "{id}");
        assert!(id.contains("os-"), "{id}");
    }
}
