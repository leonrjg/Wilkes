//! Transcription of the text inside a native image.
//!
//! Wilkes owns the contract; the engine behind it is replaceable. What crosses
//! this boundary is Wilkes' own types — normalized text, an admission signal,
//! and quadrilaterals in Wilkes' coordinate spaces — so no engine's token
//! format or geometry convention leaks into the reading, the source map or
//! the cache key.
//!
//! There is exactly one production engine. A recognition failure is a partial
//! result, never a second engine's turn: two engines producing the reading
//! would mean two answers to "what does this document say", and the whole
//! design rests on there being one.

use crate::types::{
    BoundingBox, ImageOcrRegion, ImageTransform, OcrAdmission, Point,
};

use super::AnalysisContext;

/// The largest location token. The recognizer emits coordinates as
/// `<|LOC_0|>` .. `<|LOC_1000|>`, so the grid has 1001 stops and a coordinate
/// is `n / 1000` of the way across the image it was given.
pub const LOC_MAX: u16 = 1000;

/// One region as the model emitted it: text, an admission signal, and a
/// quadrilateral in fractions of the image, before any of Wilkes' geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct SpottedRegion {
    pub text: String,
    /// Mean probability of the tokens that spell `text`, from the decode's own
    /// log-probabilities. Uncalibrated by construction — it says how sure the
    /// decoder was of its own next token, not how often such a decode is
    /// right.
    pub confidence: f32,
    /// Top-left, top-right, bottom-right, bottom-left, each in `0.0..=1.0` of
    /// the image's width and height.
    pub quad: [Point; 4],
}

/// One decoded token of a spotting response.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpottingToken {
    pub id: u32,
    /// Log-probability the decoder assigned the token it chose.
    pub logprob: f32,
    /// `Some(n)` when this token is `<|LOC_n|>`.
    pub loc: Option<u16>,
}

/// Turns token ids back into text. Supplied by the engine, which owns the
/// tokenizer; kept behind a trait so the parser below is testable without one.
pub trait SpottingDecoder {
    fn decode(&self, ids: &[u32]) -> anyhow::Result<String>;
}

/// The production recognizer.
pub trait OcrEngine: Send + Sync {
    /// Model, revision, task prompt, preprocessing and threshold, as one
    /// string. Enters the extraction recipe.
    fn identity(&self) -> String;

    /// The threshold this engine's admission signal is compared against.
    fn admission_threshold(&self) -> f32;

    /// Transcribe every text region of one image.
    fn spot(&self, image: &image::RgbImage) -> anyhow::Result<Vec<SpottedRegion>>;
}

/// Parse a spotting response into regions.
///
/// The response is a flat token stream in which each text instance is followed
/// by eight location tokens — `x`,`y` for the top-left, top-right,
/// bottom-right and bottom-left corners in that order. Emission order is the
/// model's reading order and is preserved: reordering the regions here would
/// be a layout decision, and this phase does not make those.
///
/// A trailing run of text with no coordinates, or a run of location tokens
/// that is not eight long, is dropped rather than guessed at. Autoregressive
/// transcription can truncate, and half a quadrilateral is not a location.
pub fn parse_spotting(
    tokens: &[SpottingToken],
    decoder: &dyn SpottingDecoder,
) -> anyhow::Result<Vec<SpottedRegion>> {
    let mut regions = Vec::new();
    let mut text_ids: Vec<u32> = Vec::new();
    let mut text_logprobs: Vec<f32> = Vec::new();
    let mut coordinates: Vec<u16> = Vec::new();

    for token in tokens {
        match token.loc {
            Some(value) => coordinates.push(value),
            None => {
                // A text token after a complete quadrilateral starts the next
                // region; one after a partial quadrilateral means the model
                // emitted something this parser cannot place, and the whole
                // pending instance goes.
                if !coordinates.is_empty() {
                    if coordinates.len() == 8 {
                        if let Some(region) =
                            finish(&text_ids, &text_logprobs, &coordinates, decoder)?
                        {
                            regions.push(region);
                        }
                    }
                    text_ids.clear();
                    text_logprobs.clear();
                    coordinates.clear();
                }
                text_ids.push(token.id);
                text_logprobs.push(token.logprob);
            }
        }
    }
    if coordinates.len() == 8 {
        if let Some(region) = finish(&text_ids, &text_logprobs, &coordinates, decoder)? {
            regions.push(region);
        }
    }
    Ok(regions)
}

fn finish(
    text_ids: &[u32],
    logprobs: &[f32],
    coordinates: &[u16],
    decoder: &dyn SpottingDecoder,
) -> anyhow::Result<Option<SpottedRegion>> {
    let text = normalize_recognized_text(&decoder.decode(text_ids)?);
    if text.is_empty() {
        return Ok(None);
    }
    let scale = f32::from(LOC_MAX);
    let point = |index: usize| Point {
        x: f32::from(coordinates[index * 2]).min(scale) / scale,
        y: f32::from(coordinates[index * 2 + 1]).min(scale) / scale,
    };
    let confidence = if logprobs.is_empty() {
        0.0
    } else {
        (logprobs.iter().sum::<f32>() / logprobs.len() as f32).exp()
    };
    Ok(Some(SpottedRegion {
        text,
        confidence: confidence.clamp(0.0, 1.0),
        quad: [point(0), point(1), point(2), point(3)],
    }))
}

/// Collapse the whitespace a recognizer emits and nothing else.
///
/// Punctuation, percent signs, units, diacritics and scripts all survive
/// verbatim: they are the content. Character-aware throughout — a transcription
/// is arbitrary Unicode, and there is no byte offset here that is safe to
/// assume lands on a character boundary.
pub fn normalize_recognized_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out
}

/// Bumped when the geometry below changes: how a normalized quad becomes
/// pixels, how pixels become page coordinates, or what the deduplication
/// against native glyphs counts as the same claim.
///
/// This is Wilkes' own mapping, not the engine's, which is why it is versioned
/// here and not inside the engine's settings. It is in the analyzer identity
/// because a region that moves is a different reading: the bytes may be
/// identical and still resolve to a different part of the page, and a search
/// hit that lands somewhere else is exactly the failure this feature exists to
/// avoid.
pub const MAPPING_VERSION: &str = "image-mapping-v1";

/// Place each spotted region on the page, and decide whether it enters the
/// reading.
///
/// Three outcomes, all recorded: admitted, below the threshold, or already
/// present as native glyphs. Nothing is discarded silently — a label missing
/// from the reading is answerable from the image's own regions.
#[allow(clippy::too_many_arguments)]
pub fn place_and_admit(
    spotted: Vec<SpottedRegion>,
    transform: &ImageTransform,
    image_bbox: &BoundingBox,
    pixel_width: u32,
    pixel_height: u32,
    page: u32,
    context: &AnalysisContext,
    threshold: f32,
) -> Vec<ImageOcrRegion> {
    let native = context.native_text_within(page, image_bbox);
    spotted
        .into_iter()
        .map(|region| {
            let polygon_within_image: Vec<Point> = region
                .quad
                .iter()
                .map(|point| Point {
                    x: point.x * pixel_width as f32,
                    y: point.y * pixel_height as f32,
                })
                .collect();
            let page_polygon: Vec<Point> = polygon_within_image
                .iter()
                .map(|point| {
                    transform.pixel_to_page(point.x, point.y, pixel_width, pixel_height)
                })
                .collect();
            let comparable = normalize_for_comparison(&region.text);
            let admission = if !comparable.is_empty() && native.contains(comparable.as_str()) {
                OcrAdmission::DeduplicatedAgainstNativeText
            } else if region.confidence < threshold {
                OcrAdmission::RejectedLowConfidence
            } else {
                OcrAdmission::Accepted
            };
            ImageOcrRegion {
                text: region.text,
                confidence: region.confidence,
                polygon_within_image,
                page_polygon,
                admission,
            }
        })
        .collect()
}

/// The comparison form for "does the page already draw this". Case-folded,
/// whitespace-collapsed, and stripped of the punctuation a label carries at
/// either end — so `Knowledge base` and `knowledge base.` are the same claim.
pub fn normalize_for_comparison(text: &str) -> String {
    normalize_recognized_text(text)
        .to_lowercase()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

/// Whether a point lies inside a rectangle.
pub(super) fn contains(bbox: &BoundingBox, point: &Point) -> bool {
    point.x >= bbox.x
        && point.x <= bbox.x + bbox.width
        && point.y >= bbox.y
        && point.y <= bbox.y + bbox.height
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids below 1000 stand for the character at that code point offset from
    /// `a`, which is enough to spell words in a test without a tokenizer.
    struct Letters;
    impl SpottingDecoder for Letters {
        fn decode(&self, ids: &[u32]) -> anyhow::Result<String> {
            Ok(ids
                .iter()
                .map(|id| char::from_u32(*id).unwrap_or('?'))
                .collect())
        }
    }

    fn text_token(c: char) -> SpottingToken {
        SpottingToken {
            id: c as u32,
            logprob: 0.0,
            loc: None,
        }
    }

    fn loc(value: u16) -> SpottingToken {
        SpottingToken {
            id: 0,
            logprob: 0.0,
            loc: Some(value),
        }
    }

    fn spell(word: &str) -> Vec<SpottingToken> {
        word.chars().map(text_token).collect()
    }

    fn quad(values: [u16; 8]) -> Vec<SpottingToken> {
        values.iter().map(|value| loc(*value)).collect()
    }

    #[test]
    fn a_region_is_its_text_followed_by_four_corners() {
        let mut tokens = spell("DREAM");
        tokens.extend(quad([253, 286, 346, 298, 345, 339, 252, 330]));

        let regions = parse_spotting(&tokens, &Letters).expect("parses");
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].text, "DREAM");
        assert_eq!(regions[0].quad[0], Point { x: 0.253, y: 0.286 });
        assert_eq!(regions[0].quad[2], Point { x: 0.345, y: 0.339 });
    }

    #[test]
    fn regions_keep_the_order_the_model_emitted_them_in() {
        let mut tokens = spell("first");
        tokens.extend(quad([0, 0, 10, 0, 10, 10, 0, 10]));
        tokens.extend(spell("second"));
        tokens.extend(quad([0, 20, 10, 20, 10, 30, 0, 30]));

        let regions = parse_spotting(&tokens, &Letters).expect("parses");
        let texts: Vec<&str> = regions.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, vec!["first", "second"]);
    }

    /// Autoregressive transcription can stop mid-instance. Half a
    /// quadrilateral is not a location, so the instance goes rather than
    /// being placed at a guessed corner.
    #[test]
    fn a_truncated_quadrilateral_drops_its_region() {
        let mut tokens = spell("cut");
        tokens.extend(quad([1, 2, 3, 4, 5, 6, 7, 8]));
        tokens.extend(spell("short"));
        tokens.extend(vec![loc(1), loc(2), loc(3)]);

        let regions = parse_spotting(&tokens, &Letters).expect("parses");
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].text, "cut");
    }

    #[test]
    fn text_with_no_coordinates_at_all_yields_nothing() {
        let regions = parse_spotting(&spell("unplaced"), &Letters).expect("parses");
        assert!(regions.is_empty());
    }

    /// The signal is the mean probability of the tokens that spell the text,
    /// so a decode that hesitated over its characters scores below one that
    /// did not.
    #[test]
    fn confidence_comes_from_the_decodes_own_log_probabilities() {
        let mut confident = spell("sure");
        for token in &mut confident {
            token.logprob = -0.01;
        }
        confident.extend(quad([0, 0, 1, 0, 1, 1, 0, 1]));

        let mut hesitant = spell("maybe");
        for token in &mut hesitant {
            token.logprob = -2.0;
        }
        hesitant.extend(quad([0, 0, 1, 0, 1, 1, 0, 1]));

        let confident = parse_spotting(&confident, &Letters).expect("parses");
        let hesitant = parse_spotting(&hesitant, &Letters).expect("parses");
        assert!(confident[0].confidence > 0.98);
        assert!(hesitant[0].confidence < 0.2);
    }

    #[test]
    fn normalization_collapses_whitespace_and_keeps_everything_else() {
        assert_eq!(
            normalize_recognized_text("  50 %\tof\n Größe — Ω  "),
            "50 % of Größe — Ω"
        );
    }

    /// The one thing byte indexing would break. A transcription is arbitrary
    /// Unicode and is never sliced by byte offset.
    #[test]
    fn normalization_is_character_safe() {
        assert_eq!(normalize_recognized_text("日本語  テキスト"), "日本語 テキスト");
        assert_eq!(normalize_for_comparison("«Größe»"), "größe");
    }
}
