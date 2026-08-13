//! Bridge a prose prefix into the declarative passage space used by the index.

use std::ops::ControlFlow;

use crate::generate::{truncate_chars, Constraint, GenerationRequest, Generator, Sampling};

use super::prose::generate_to_sentence_boundary;

const MAX_PREFIX_CHARS: usize = 2_400;
const MAX_TOKENS: usize = 120;

pub fn build_request(prefix: &str) -> GenerationRequest {
    let tail = if prefix.chars().count() <= MAX_PREFIX_CHARS {
        prefix.trim()
    } else {
        let skip = prefix.chars().count() - MAX_PREFIX_CHARS;
        prefix
            .char_indices()
            .nth(skip)
            .map_or(prefix, |(index, _)| &prefix[index..])
            .trim()
    };
    GenerationRequest {
        system: None,
        prompt: format!(
            "Write the next one or two factual prose sentences that would naturally continue the passage below. Output only the continuation. Do not repeat the passage.\n\nPassage:\n{}\n\nContinuation:",
            truncate_chars(tail, MAX_PREFIX_CHARS)
        ),
        max_tokens: Some(MAX_TOKENS),
        constraint: Constraint::Text {
            stop: vec!["\n\n".to_string()],
        },
        sampling: Sampling {
            temperature: 0.2,
            seed: 0,
            ..Sampling::default()
        },
    }
}

pub fn hypothetical_continuation(
    generator: &dyn Generator,
    prefix: &str,
) -> anyhow::Result<String> {
    hypothetical_continuation_stream(generator, prefix, &mut |_| ControlFlow::Continue(()))
}

pub fn hypothetical_continuation_stream(
    generator: &dyn Generator,
    prefix: &str,
    sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
) -> anyhow::Result<String> {
    anyhow::ensure!(!prefix.trim().is_empty(), "cannot bridge an empty prefix");
    let generated = generate_to_sentence_boundary(generator, build_request(prefix), sink)?;
    anyhow::ensure!(
        generated.is_complete(),
        "hypothetical continuation did not finish cleanly (stop={:?}, tokens={}, model={})",
        generated.stop,
        generated.tokens,
        generator.model_id(),
    );
    let text = generated.text.trim();
    anyhow::ensure!(
        !text.is_empty(),
        "generator returned an empty hypothetical continuation"
    );
    Ok(text.to_string())
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::generate::mock::MockGenerator;
    use crate::generate::{Generated, StopReason};

    #[test]
    fn request_is_short_low_temperature_prose() {
        let request = build_request("The cache is populated.");
        assert_eq!(request.max_tokens, Some(120));
        assert_eq!(request.sampling.temperature, 0.2);
        assert!(request.prompt.contains("The cache is populated."));
    }

    #[test]
    fn empty_and_empty_output_are_rejected() {
        assert!(hypothetical_continuation(&MockGenerator::default(), " ").is_err());
        assert!(hypothetical_continuation(&MockGenerator::scripted([" "]), "prefix").is_err());
    }

    #[test]
    fn accepts_the_first_complete_sentence_before_the_token_limit() {
        let generator = MockGenerator::default();
        generator.scripted.lock().unwrap().push_back(Generated {
            text: "The cache expires when its source changes. The model keeps talking".into(),
            tokens: MAX_TOKENS,
            stop: StopReason::MaxTokens,
        });

        assert_eq!(
            hypothetical_continuation(&generator, "The cache is populated.").unwrap(),
            "The cache expires when its source changes."
        );
    }

    #[test]
    fn token_exhaustion_reports_actionable_metadata() {
        let generator = MockGenerator::default();
        generator.scripted.lock().unwrap().push_back(Generated {
            text: "an unfinished continuation".into(),
            tokens: MAX_TOKENS,
            stop: StopReason::MaxTokens,
        });

        let error = hypothetical_continuation(&generator, "A prefix").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("stop=MaxTokens"), "{message}");
        assert!(message.contains("tokens=120"), "{message}");
        assert!(message.contains("model=mock-generator"), "{message}");
    }
}
