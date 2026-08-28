//! The external door for image description.
//!
//! One describer prompt and one response schema serve both the first-class
//! local path and whatever the user has pulled into Ollama — the same family
//! scales from 2B to 235B with identical prompting, so this is a second door
//! onto one describer, not a second describer. What is behind the door is the
//! user's business: Wilkes does not manage those models, and names whichever
//! one it was told to use in the extraction recipe so a reading produced by a
//! 2B and one produced by a 235B are never mixed.

use std::time::Duration;

use anyhow::Context;
use base64::Engine as _;
use reqwest::blocking::Client;
use tracing::warn;
use url::Url;

use crate::types::{ImageDescription, ImageOcrRegion};

use super::describe::{
    describer_prompt, parse_description, FigureDescriber, DESCRIBER_PROMPT_VERSION,
};

/// A describer is one model call over one figure. Generous because a large
/// model on a loaded machine is slow, bounded because an indexing pass that
/// waits forever on one image has stopped being an indexing pass.
const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(180);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

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
            client: Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(DESCRIBE_TIMEOUT)
                .build()
                .context("could not create the describer's HTTP client")?,
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
    /// Ollama's JSON mode. The schema is still validated on the way in —
    /// well-formed JSON is not the same claim as the response schema.
    format: &'a str,
    options: GenerateOptions,
}

#[derive(serde::Serialize)]
struct GenerateOptions {
    /// Greedy. A description that changes between runs would make the
    /// canonical reading — and every rendition hash built on it — depend on a
    /// sampler's seed.
    temperature: f32,
    seed: u32,
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
            format: "json",
            options: GenerateOptions {
                temperature: 0.0,
                seed: 0,
            },
        };
        let url = self
            .base_url
            .join("api/generate")
            .context("could not build the describer's endpoint")?;
        let response = self
            .client
            .post(url)
            .json(&request)
            .send()
            .context("could not reach the describer")?;
        anyhow::ensure!(
            response.status().is_success(),
            "the describer returned {}",
            response.status()
        );
        let body: GenerateResponse = response
            .json()
            .context("the describer's reply was not an Ollama response")?;
        parse_description(&body.response)
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

fn normalize_base_url(input: &str) -> anyhow::Result<Url> {
    let mut url = Url::parse(input.trim()).context("the describer's URL is invalid")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https"),
        "the describer's URL must use http or https"
    );
    anyhow::ensure!(url.host_str().is_some(), "the describer's URL has no host");
    anyhow::ensure!(
        url.username().is_empty() && url.password().is_none(),
        "the describer's URL must not contain credentials"
    );
    anyhow::ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "the describer's URL must not contain a query or fragment"
    );
    anyhow::ensure!(
        matches!(url.path(), "" | "/"),
        "the describer's URL must not contain a path"
    );
    url.set_path("/");
    Ok(url)
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
            text: "Knowledge base".to_string(),
            confidence: 0.9,
            polygon_within_image: vec![Point { x: 2.0, y: 1.0 }],
            page_polygon: Vec::new(),
            admission: OcrAdmission::Accepted,
        }]
    }

    #[test]
    fn a_schema_reply_becomes_a_description() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/generate")
            .with_status(200)
            .with_body(
                r#"{"response": "{\"description\": \"A user interface reaches an inference engine.\", \"relationships\": [{\"source\": \"User interface\", \"relation\": \"reaches\", \"target\": \"Inference engine\"}]}"}"#,
            )
            .create();

        let describer = OllamaDescriber::new(&server.url(), "qwen3-vl:2b").expect("configures");
        let description = describer
            .describe(&figure(), &transcription())
            .expect("describes");
        assert_eq!(
            description.description,
            "A user interface reaches an inference engine."
        );
        assert_eq!(description.relationships[0].target, "Inference engine");
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
            .with_body(r#"{"response": "{\"description\": \"Two boxes.\"}"}"#)
            .create();

        let describer = OllamaDescriber::new(&server.url(), "qwen3-vl:2b").expect("configures");
        describer
            .describe(&figure(), &transcription())
            .expect("describes");
        mock.assert();
    }

    /// A reply that is not the schema is a failed description, reported as a
    /// partial analysis rather than smuggled into the reading as prose.
    #[test]
    fn a_reply_outside_the_schema_is_a_failure() {
        let mut server = mockito::Server::new();
        let _mock = server
            .mock("POST", "/api/generate")
            .with_status(200)
            .with_body(r#"{"response": "I think it shows an expert system."}"#)
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
            .with_body("model not found")
            .create();

        let describer = OllamaDescriber::new(&server.url(), "qwen3-vl:2b").expect("configures");
        let error = describer
            .describe(&figure(), &[])
            .expect_err("a 500 is a failure");
        assert!(error.to_string().contains("500"), "{error}");
    }

    /// Local analysis is the default requirement, so a describer that sends
    /// imagery off the machine has to be able to say so.
    #[test]
    fn a_describer_that_leaves_the_machine_discloses_it() {
        let local = OllamaDescriber::new("http://localhost:11434", "qwen3-vl:2b").expect("local");
        assert!(!local.is_remote());
        assert!(local.identity().ends_with("local"));

        let loopback = OllamaDescriber::new("http://127.0.0.1:11434", "qwen3-vl:2b").expect("local");
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
        let small = OllamaDescriber::new("http://localhost:11434", "qwen3-vl:2b").expect("configures");
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
