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

use crate::types::{ImageDescription, ImageOcrRegion, ImageRelationship};

/// Bumped whenever the instruction or the schema changes. Part of the
/// extraction recipe: the same image and the same weights under a different
/// prompt are a different reading.
pub const DESCRIBER_PROMPT_VERSION: &str = "figure-describer-v1";

/// The instruction every describer is given, whatever runs it.
///
/// Fixed and shared so the first-class local path and the external Ollama door
/// ask for the same thing: two doors onto one prompt, not two prompts.
///
/// It asks for the visible only. A describer that infers what a diagram is
/// *about* writes claims the document does not make, and those claims would
/// enter the canonical reading with a page locator attached — which is the one
/// thing the labelling in the reading cannot repair.
pub const DESCRIBER_INSTRUCTION: &str = "\
Describe only what is visibly present in this image: the elements drawn in it \
and the relationships the drawing expresses between them. Do not infer \
purpose, significance or context, and do not mention anything you cannot see. \
Reply with JSON only, in this exact shape:\n\
{\"description\": \"one or two sentences\", \"relationships\": \
[{\"source\": \"\", \"relation\": \"\", \"target\": \"\"}]}\n\
Use an empty relationships list when the image expresses none.";

/// The longest description that reaches the canonical reading. A describer
/// that runs away is a partial result with a truncated claim in it, not a
/// paragraph of invention inserted into a document.
pub const MAX_DESCRIPTION_CHARS: usize = 600;

/// Generates a description of one image.
///
/// Sync for the reason given on [`super::ImageAnalyzer`]: extraction is a
/// synchronous contract with many callers, and a describer that needs a server
/// uses a blocking client rather than making every one of them async.
pub trait FigureDescriber: Send + Sync {
    /// Model, revision, prompt and schema version, as one string. Enters the
    /// extraction recipe, so switching describer or recipe forces
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
    prompt.push_str("\n\nText already transcribed from this image, with its position as \
        fractions of the image (x, y of the top-left corner):\n");
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

/// The describer's fixed response schema, as it arrives on the wire.
#[derive(serde::Deserialize)]
struct DescriptionResponse {
    description: String,
    #[serde(default)]
    relationships: Vec<ImageRelationship>,
}

/// Validate one describer response against the fixed schema.
///
/// A response that is not the schema is a failed description, not a
/// best-effort paragraph: the whole point of the schema is that the bytes
/// entering the reading were shaped by something that understood the request.
/// Models framed by chat templates commonly wrap JSON in prose or a fence, so
/// the object is located within the reply — but it must be an object of this
/// shape, and a reply with none is an error.
pub fn parse_description(reply: &str) -> anyhow::Result<ImageDescription> {
    let json = extract_json_object(reply)
        .ok_or_else(|| anyhow::anyhow!("describer replied with no JSON object"))?;
    let parsed: DescriptionResponse = serde_json::from_str(json)
        .map_err(|error| anyhow::anyhow!("describer reply is not the response schema: {error}"))?;

    let description = super::ocr::normalize_recognized_text(&parsed.description);
    anyhow::ensure!(
        !description.is_empty(),
        "describer returned an empty description"
    );
    let description = truncate_chars(&description, MAX_DESCRIPTION_CHARS);

    let relationships = parsed
        .relationships
        .into_iter()
        .filter(|relationship| {
            !relationship.source.trim().is_empty()
                && !relationship.relation.trim().is_empty()
                && !relationship.target.trim().is_empty()
        })
        .collect();

    Ok(ImageDescription {
        description,
        relationships,
    })
}

/// The first balanced `{...}` run in the reply, ignoring braces inside JSON
/// strings.
fn extract_json_object(reply: &str) -> Option<&str> {
    let bytes = reply.as_bytes();
    let start = reply.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes.iter().enumerate().skip(start) {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&reply[start..=offset]);
                }
            }
            _ => {}
        }
    }
    None
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
            text: text.to_string(),
            confidence: 0.9,
            polygon_within_image: vec![Point { x, y }],
            page_polygon: Vec::new(),
            admission: OcrAdmission::Accepted,
        }
    }

    #[test]
    fn the_schema_is_read_out_of_a_reply_that_wraps_it() {
        let description = parse_description(
            "Here you go:\n```json\n{\"description\": \"A flow chart.\", \
             \"relationships\": [{\"source\": \"A\", \"relation\": \"feeds\", \
             \"target\": \"B\"}]}\n```",
        )
        .expect("the object is found and parsed");
        assert_eq!(description.description, "A flow chart.");
        assert_eq!(description.relationships.len(), 1);
        assert_eq!(description.relationships[0].target, "B");
    }

    #[test]
    fn a_reply_that_is_not_the_schema_is_a_failure() {
        assert!(parse_description("I see a diagram of an expert system.").is_err());
        assert!(parse_description("{\"caption\": \"wrong field\"}").is_err());
        assert!(parse_description("{\"description\": \"   \"}").is_err());
    }

    /// Braces inside the description must not close the object early.
    #[test]
    fn braces_inside_strings_do_not_end_the_object() {
        let description =
            parse_description("{\"description\": \"the set {a, b} is drawn\"}").expect("parses");
        assert_eq!(description.description, "the set {a, b} is drawn");
    }

    #[test]
    fn a_runaway_description_is_truncated_on_a_character_boundary() {
        let long = "é".repeat(MAX_DESCRIPTION_CHARS + 50);
        let reply = format!("{{\"description\": \"{long}\"}}");
        let description = parse_description(&reply).expect("parses");
        assert_eq!(
            description.description.chars().count(),
            MAX_DESCRIPTION_CHARS + 1
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
