//! The external door for image description.
//!
//! One describer prompt serves both the first-class local path and whatever
//! the user has pulled into Ollama — the same family scales from 2B to 235B
//! with identical prompting, so this is a second door onto one describer, not
//! a second describer. What is behind the door is the user's business: Wilkes
//! does not manage those models, and names whichever one it was told to use in
//! the extraction recipe so a reading produced by a 2B and one produced by a
//! 235B are never mixed.
//!
//! What an Ollama server *is* — a URL that may be trusted, a client, an
//! endpoint, an error body worth reading — is not decided here. That belongs
//! to [`crate::generate::engines::ollama`], which spoke this protocol first,
//! and this module borrows it rather than keeping a second opinion.

use std::time::Duration;

use anyhow::Context;
use base64::Engine as _;
use reqwest::blocking::Client;
use tracing::warn;
use url::Url;

use crate::generate::engines::ollama::{
    checked_response, endpoint, normalize_base_url, ollama_client,
};
use crate::types::{ImageDescription, ImageOcrRegion};

use super::describe::{
    accept_description, describer_prompt, FigureDescriber, DESCRIBER_PROMPT_VERSION,
    MAX_DESCRIPTION_CHARS,
};

/// A describer is one model call over one figure. Generous because a large
/// model on a loaded machine is slow, bounded because an indexing pass that
/// waits forever on one image has stopped being an indexing pass.
const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(180);

/// The generation stops at the bound rather than being cut back to it.
/// Truncation is the last resort; not generating the excess is better than
/// paying for it and throwing it away. Sized in tokens against the character
/// bound at a deliberately loose ratio, because a description in a script
/// that tokenizes densely must not be cut short by an English assumption.
const MAX_DESCRIBE_TOKENS: i32 = MAX_DESCRIPTION_CHARS as i32;

/// Long enough to cover another figure in the same pass. A batch extraction
/// that lets the model unload between images pays the load cost per figure.
const KEEP_ALIVE: &str = "5m";

/// Describes figures with a model served by Ollama.
pub struct OllamaDescriber {
    base_url: Url,
    model: String,
    client: Client,
    remote: bool,
}

impl OllamaDescriber {
    /// Configure the door. `base_url` is the Ollama server and `model` the tag
    /// to ask, both as the user gave them.
    pub fn new(base_url: &str, model: &str) -> anyhow::Result<Self> {
        let base_url = normalize_base_url(base_url)?;
        anyhow::ensure!(!model.trim().is_empty(), "no Ollama model was named");
        let remote = !is_loopback(&base_url);
        if remote {
            // Said once, loudly, where an operator will see it. The property
            // this reports on — that document imagery leaves the machine — is
            // also on the trait, so a caller can disclose it in the interface
            // rather than only in a log.
            warn!(
                "the figure describer is remote: document imagery will be sent to {}",
                base_url
            );
        }
        Ok(Self {
            client: ollama_client(DESCRIBE_TIMEOUT)?,
            base_url,
            model: model.trim().to_string(),
            remote,
        })
    }
}

#[derive(serde::Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    /// One PNG, base64. Ollama's own multimodal field.
    images: Vec<String>,
    /// The whole reply at once: this is a batch extraction, not a chat.
    stream: bool,
    /// No reasoning preamble. The reply asked for is the description itself,
    /// and a describer that thinks out loud spends the timeout on text that
    /// would then have to be stripped back off by something downstream
    /// guessing where the answer began.
    think: bool,
    keep_alive: &'a str,
    options: GenerateOptions,
}

#[derive(serde::Serialize)]
struct GenerateOptions {
    /// Greedy. A description that changes between runs would make the
    /// canonical reading — and every rendition hash built on it — depend on a
    /// sampler's seed.
    temperature: f32,
    seed: u32,
    num_predict: i32,
}

#[derive(serde::Deserialize)]
struct GenerateResponse {
    response: String,
}

impl FigureDescriber for OllamaDescriber {
    fn identity(&self) -> String {
        format!(
            "ollama+{}+{}+{}",
            self.model,
            DESCRIBER_PROMPT_VERSION,
            if self.remote { "remote" } else { "local" }
        )
    }

    fn is_remote(&self) -> bool {
        self.remote
    }

    fn describe(
        &self,
        image: &image::RgbImage,
        ocr: &[ImageOcrRegion],
    ) -> anyhow::Result<ImageDescription> {
        let request = GenerateRequest {
            model: &self.model,
            prompt: &describer_prompt(ocr),
            images: vec![base64::engine::general_purpose::STANDARD.encode(encode_png(image)?)],
            stream: false,
            think: false,
            keep_alive: KEEP_ALIVE,
            options: GenerateOptions {
                temperature: 0.0,
                seed: 0,
                num_predict: MAX_DESCRIBE_TOKENS,
            },
        };
        let response = self
            .client
            .post(endpoint(&self.base_url, "api/generate")?)
            .json(&request)
            .send()
            .context("could not reach the describer")?;
        let body: GenerateResponse = checked_response(response, "describe an image")?
            .json()
            .context("the describer's reply was not an Ollama response")?;
        accept_description(&body.response)
    }
}

/// The image as PNG bytes.
///
/// Lossless on purpose: a describer reading a re-compressed figure is reading
/// artefacts the document does not contain, and this is the one place the
/// pixels leave Wilkes.
fn encode_png(image: &image::RgbImage) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut bytes);
    image::ImageEncoder::write_image(
        encoder,
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgb8,
    )
    .context("could not encode the image for the describer")?;
    Ok(bytes)
}

/// Loopback means the pixels stay on this machine. Anything else is the
/// explicit external door, and says so.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OcrAdmission, Point};

    fn figure() -> image::RgbImage {
        image::RgbImage::from_fn(8, 4, |x, y| image::Rgb([(x * 30) as u8, (y * 60) as u8, 0]))
    }

    fn transcription() -> Vec<ImageOcrRegion> {
        vec![ImageOcrRegion {
            kind: Default::default(),
            text: "Knowledge base".to_string(),
            confidence: 0.9,
            polygon_within_image: vec![Point { x: 2.0, y: 1.0 }],
            page_polygon: Vec::new(),
            admission: OcrAdmission::Accepted,
        }]
    }

    #[test]
    fn a_prose_reply_becomes_the_description() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/generate")
            .with_status(200)
            .with_body(
                r#"{"response": "A user interface exchanges arrows in both directions with an inference engine."}"#,
            )
            .create();

        let describer = OllamaDescriber::new(&server.url(), "qwen3-vl:2b").expect("configures");
        let description = describer
            .describe(&figure(), &transcription())
            .expect("describes");
        assert_eq!(
            description.description,
            "A user interface exchanges arrows in both directions with an inference engine."
        );
        mock.assert();
    }

    /// The transcription and the fixed instruction both reach the model, and
    /// the image goes with them. One prompt, whichever door it goes through.
    #[test]
    fn the_request_carries_the_instruction_the_transcription_and_the_image() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/generate")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::Regex("Describe only what is visibly present".into()),
                mockito::Matcher::Regex("Knowledge base at".into()),
                mockito::Matcher::Regex("\"images\":\\[\"iVBOR".into()),
                mockito::Matcher::Regex("\"temperature\":0".into()),
            ]))
            .with_status(200)
            .with_body(r#"{"response": "Two boxes joined by an arrow."}"#)
            .create();

        let describer = OllamaDescriber::new(&server.url(), "qwen3-vl:2b").expect("configures");
        describer
            .describe(&figure(), &transcription())
            .expect("describes");
        mock.assert();
    }

    /// Nothing about the reply's *shape* is required any more, so the request
    /// must not ask for one: JSON mode would constrain the reply to a form the
    /// gate no longer reads, on models that may not support it at all.
    #[test]
    fn the_request_asks_for_prose_within_a_bound_and_not_for_json() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/generate")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::Regex("\"think\":false".into()),
                mockito::Matcher::Regex("\"num_predict\":1500".into()),
            ]))
            .with_status(200)
            .with_body(r#"{"response": "Two boxes."}"#)
            .create();

        let describer = OllamaDescriber::new(&server.url(), "qwen3-vl:2b").expect("configures");
        describer.describe(&figure(), &[]).expect("describes");
        mock.assert();

        let sent = OllamaDescriber::new("http://localhost:11434", "m").expect("configures");
        assert!(
            !serde_json::to_string(&GenerateRequest {
                model: &sent.model,
                prompt: "p",
                images: Vec::new(),
                stream: false,
                think: false,
                keep_alive: KEEP_ALIVE,
                options: GenerateOptions {
                    temperature: 0.0,
                    seed: 0,
                    num_predict: MAX_DESCRIBE_TOKENS,
                },
            })
            .unwrap()
            .contains("format"),
            "the request must not carry a response format"
        );
    }

    /// A model that answers with nothing has failed to describe the image; it
    /// has not described the image as empty.
    #[test]
    fn an_empty_reply_is_a_failure() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/api/generate")
            .with_status(200)
            .with_body(r#"{"response": "   "}"#)
            .create();

        let describer = OllamaDescriber::new(&server.url(), "qwen3-vl:2b").expect("configures");
        assert!(describer.describe(&figure(), &[]).is_err());
    }

    #[test]
    fn a_server_error_is_a_failure_and_not_an_empty_description() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/api/generate")
            .with_status(500)
            .with_body(r#"{"error": "model 'qwen3-vl:2b' not found"}"#)
            .create();

        let describer = OllamaDescriber::new(&server.url(), "qwen3-vl:2b").expect("configures");
        let error = describer
            .describe(&figure(), &[])
            .expect_err("a 500 is a failure");
        let reported = error.to_string();
        assert!(reported.contains("500"), "{reported}");
        assert!(
            reported.contains("not found"),
            "the server's own reason survives: {reported}"
        );
    }

    /// Local analysis is the default requirement, so a describer that sends
    /// imagery off the machine has to be able to say so.
    #[test]
    fn a_describer_that_leaves_the_machine_discloses_it() {
        let local = OllamaDescriber::new("http://localhost:11434", "qwen3-vl:2b").expect("local");
        assert!(!local.is_remote());
        assert!(local.identity().ends_with("local"));

        let loopback =
            OllamaDescriber::new("http://127.0.0.1:11434", "qwen3-vl:2b").expect("local");
        assert!(!loopback.is_remote());

        let elsewhere =
            OllamaDescriber::new("http://gpu-box.example:11434", "qwen3-vl:2b").expect("remote");
        assert!(elsewhere.is_remote());
        assert!(elsewhere.identity().ends_with("remote"));
    }

    /// The model is part of the identity: a 2B and a 235B describing the same
    /// figure are different readings, and must not share an index.
    #[test]
    fn the_model_named_is_part_of_the_recipe() {
        let small =
            OllamaDescriber::new("http://localhost:11434", "qwen3-vl:2b").expect("configures");
        let large =
            OllamaDescriber::new("http://localhost:11434", "qwen3-vl:32b").expect("configures");
        assert_ne!(small.identity(), large.identity());
    }

    #[test]
    fn a_url_that_is_not_a_plain_server_is_refused() {
        assert!(OllamaDescriber::new("ftp://localhost:11434", "m").is_err());
        assert!(OllamaDescriber::new("http://user:secret@localhost:11434", "m").is_err());
        assert!(OllamaDescriber::new("http://localhost:11434/custom", "m").is_err());
        assert!(OllamaDescriber::new("http://localhost:11434", "  ").is_err());
    }
}
