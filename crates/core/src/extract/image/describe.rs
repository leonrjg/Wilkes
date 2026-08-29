//! Description of what a native image shows.
//!
//! A separate fact from the transcription, produced by a separate model, and
//! labelled separately in the reading. The recognizer says what the picture
//! *says*; the describer says what it *shows* — the arrows, the direction of
//! flow, the relationships a transcription of the labels loses entirely. One
//! is quotable as the document's words and the other is not, which is why they
//! never merge into a single paragraph.
//!
//! No stage here doubles as a fallback for the other. An absent describer is
//! reported as an absent describer, and a recognizer failure is never answered
//! by asking the describer to read the labels instead.

use crate::types::{ImageDescription, ImageOcrRegion};

/// Bumped whenever the instruction changes. Part of the extraction recipe: the
/// same image and the same weights under a different prompt are a different
/// reading.
///
/// `v2` is the prose instruction. `v1` asked for a JSON object carrying
/// `{source, relation, target}` triples; the version moved so that no reading
/// produced under the schema is ever mistaken for one produced under this.
pub const DESCRIBER_PROMPT_VERSION: &str = "figure-describer-v2";

/// The instruction every describer is given, whatever runs it.
///
/// Fixed and shared so the first-class local path and the external Ollama door
/// ask for the same thing: two doors onto one prompt, not two prompts.
///
/// It asks for the visible only. A describer that infers what a diagram is
/// *about* writes claims the document does not make, and those claims would
/// enter the canonical reading with a page locator attached — which is the one
/// thing the labelling in the reading cannot repair.
///
/// It asks for detail, and says why the labels alone are not the answer: the
/// transcription is already in the reading beside this, so a description that
/// restates the labels adds nothing. The arrows are the addition.
pub const DESCRIBER_INSTRUCTION: &str = "\
Describe only what is visibly present in this image: the elements drawn in it \
and the relationships the drawing expresses between them — arrows and their \
direction, containment, adjacency, order. Be specific and detailed. Do not \
infer purpose, significance or context, and do not mention anything you \
cannot see. Do not merely list the labels; describe how the drawing connects \
them. Reply with the description itself, as plain prose, and nothing else.";

/// The longest description that reaches the canonical reading.
///
/// Wide enough for a dense figure described element by element — the worked
/// example in `docs/internal/FIGURE.md` is around a quarter of it — and still
/// a bound, because a describer that runs away should enter the reading as a
/// truncated claim rather than as pages of invention carrying a page locator.
pub const MAX_DESCRIPTION_CHARS: usize = 1500;

/// Generates a description of one image.
///
/// Sync for the reason given on [`super::ImageAnalyzer`]: extraction is a
/// synchronous contract with many callers, and a describer that needs a server
/// uses a blocking client rather than making every one of them async.
pub trait FigureDescriber: Send + Sync {
    /// Model, revision and prompt version, as one string. Enters the
    /// extraction recipe, so switching describer or prompt forces
    /// re-extraction.
    fn identity(&self) -> String;

    /// Whether this describer sends document imagery off the machine. Local
    /// analysis is the default requirement; a remote describer has to be
    /// configured deliberately and has to say so.
    fn is_remote(&self) -> bool {
        false
    }

    fn describe(
        &self,
        image: &image::RgbImage,
        ocr: &[ImageOcrRegion],
    ) -> anyhow::Result<ImageDescription>;
}

/// The prompt one describer call sends, built the same way for every backend.
pub fn describer_prompt(ocr: &[ImageOcrRegion]) -> String {
    if ocr.is_empty() {
        return DESCRIBER_INSTRUCTION.to_string();
    }
    let mut prompt = String::from(DESCRIBER_INSTRUCTION);
    prompt.push_str(
        "\n\nText already transcribed from this image, with its position as \
        fractions of the image (x, y of the top-left corner):\n",
    );
    for region in ocr {
        let corner = region.polygon_within_image.first();
        match corner {
            Some(point) => prompt.push_str(&format!(
                "- {} at ({:.2}, {:.2})\n",
                region.text, point.x, point.y
            )),
            None => prompt.push_str(&format!("- {}\n", region.text)),
        }
    }
    prompt
}

/// The gate on description bytes entering the canonical reading.
///
/// There is no schema to conform to, so this is what is left, and it is all
/// that was ever load-bearing: the reply is normalized to one passage, must
/// say something after normalization, and is bounded. A reply that is empty or
/// a refusal is a *failed* description — the caller records a partial analysis
/// — never an image that was described as nothing.
pub fn accept_description(reply: &str) -> anyhow::Result<ImageDescription> {
    let description = super::ocr::normalize_recognized_text(reply);
    anyhow::ensure!(
        !description.is_empty(),
        "describer returned an empty description"
    );
    Ok(ImageDescription {
        description: truncate_chars(&description, MAX_DESCRIPTION_CHARS),
    })
}

/// Character-aware truncation: a description is arbitrary Unicode, and there
/// is no byte offset in it that may be assumed to be a character boundary.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OcrAdmission, Point};

    fn region(text: &str, x: f32, y: f32) -> ImageOcrRegion {
        ImageOcrRegion {
            kind: Default::default(),
            text: text.to_string(),
            confidence: 0.9,
            polygon_within_image: vec![Point { x, y }],
            page_polygon: Vec::new(),
            admission: OcrAdmission::Accepted,
        }
    }

    /// The whole reply is the description. Nothing is extracted out of it,
    /// because there is no longer anything to extract.
    #[test]
    fn a_prose_reply_is_the_description() {
        let description = accept_description(
            "A non-expert communicates through a user interface, which\n\
             exchanges arrows with an inference engine.",
        )
        .expect("prose is accepted");
        assert_eq!(
            description.description,
            "A non-expert communicates through a user interface, which exchanges \
             arrows with an inference engine."
        );
    }

    /// JSON is no longer privileged, and no longer required: whatever a model
    /// sends is prose to this gate. The point of the amendment is that the
    /// shape of the reply was never the claim that mattered.
    #[test]
    fn a_reply_is_not_required_to_be_json() {
        let description = accept_description("{\"description\": \"two boxes\"}").expect("accepted");
        assert_eq!(description.description, "{\"description\": \"two boxes\"}");
    }

    #[test]
    fn an_empty_reply_is_a_failure_and_not_a_description_of_nothing() {
        assert!(accept_description("").is_err());
        assert!(accept_description("   \n\t ").is_err());
    }

    /// Nothing is stripped, unwrapped or rescued here. A model that frames
    /// its answer is prevented from doing so where the framing is chosen —
    /// the backend that speaks the protocol — not patched up afterwards by a
    /// gate that would have to guess which of the bytes were the answer.
    #[test]
    fn the_gate_rescues_nothing_and_only_normalizes() {
        let description =
            accept_description("  A flow chart\n\n  with three stages. ").expect("accepted");
        assert_eq!(description.description, "A flow chart with three stages.");
    }

    #[test]
    fn a_runaway_description_is_truncated_on_a_character_boundary() {
        let description = accept_description(&"é".repeat(MAX_DESCRIPTION_CHARS + 50))
            .expect("accepted, and bounded");
        assert_eq!(
            description.description.chars().count(),
            MAX_DESCRIPTION_CHARS + 1
        );
    }

    /// The instruction asks for the arrows, not the labels: the transcription
    /// is already in the reading beside the description.
    #[test]
    fn the_instruction_asks_for_detail_beyond_the_labels() {
        assert!(DESCRIBER_INSTRUCTION.contains("detailed"));
        assert!(DESCRIBER_INSTRUCTION.contains("Do not merely list the labels"));
        assert!(
            !DESCRIBER_INSTRUCTION.contains("JSON"),
            "the schema is withdrawn"
        );
    }

    #[test]
    fn the_prompt_carries_the_transcription_and_its_positions() {
        let prompt = describer_prompt(&[region("Knowledge base", 120.0, 40.0)]);
        assert!(prompt.starts_with(DESCRIBER_INSTRUCTION));
        assert!(prompt.contains("Knowledge base at (120.00, 40.00)"));
    }

    #[test]
    fn with_no_transcription_the_prompt_is_the_instruction_alone() {
        assert_eq!(describer_prompt(&[]), DESCRIBER_INSTRUCTION);
    }
}
